//! 重试策略端到端：本机起真 TCP 假服务，按轮次回 429 / 5xx / 断流，验证退避循环。
//!
//! 与 `e2e_stream.rs` 同一范式：不用桩，走真实 TCP + HTTP 客户端 + 退避循环全链路。
//! 所有测试把 `base_backoff_ms` 设为 1，总耗时毫秒级；不变量各条写在断言旁。

use fulu_core::Sampling;
use fulu_provider::{
    InvocationRequest, LocalLfmProvider, Provider, ProviderError, RetryPolicy, RetrySet,
    StreamControl, StreamEvent,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

const CHAT_BODY: &str = r#"{"choices":[{"message":{"content":"好"}}]}"#;

fn request() -> InvocationRequest {
    InvocationRequest {
        model_id: "lfm3-1.7b-q4km".into(),
        system: "你是符箓守则的执行者。".into(),
        user: "请给出摘要".into(),
        tools_json: "[]".into(),
        sampling: Sampling::default(),
        budget_tokens: 256,
        context_tokens: 8192,
    }
}

/// 毫秒级退避：基准 1ms，其余类别全开；`max_attempts` 按测试需要给。
fn fast_policy(max_attempts: u8, retry_on: RetrySet) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        base_backoff_ms: 1,
        retry_on,
    }
}

fn all_on() -> RetrySet {
    RetrySet::default()
}

fn provider(port: u16, policy: RetryPolicy) -> LocalLfmProvider {
    let mut provider = LocalLfmProvider::from_model("lfm3-1.7b-q4km", 8192, "llama.cpp >= b11960");
    provider
        .set_endpoint(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    provider.set_retry_policy(policy);
    provider
}

/// 读完整一个请求（头 + 按 Content-Length 的体），复用 e2e_stream 的写法。
fn read_request(stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if let Some(head_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..head_end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buffer.len() >= head_end + 4 + length {
                        break;
                    }
                }
            }
        }
    }
}

fn write_status(stream: &mut TcpStream, code: u16, body: &str) {
    let raw = format!(
        "HTTP/1.1 {code} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(raw.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn write_json(stream: &mut TcpStream, body: &str) {
    let raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(raw.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn sse_headers(stream: &mut TcpStream) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    stream.write_all(head.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn write_chunk(stream: &mut TcpStream, body: &str) {
    let raw = format!("{:x}\r\n{body}\r\n", body.len());
    stream.write_all(raw.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn text_event(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n")
}

#[test]
fn invoke_retries_on_server_error_then_succeeds() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut count = 0;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            read_request(&mut stream);
            // 不变量：前两次 500（ServerError），第三次 200 必须成功。
            if count < 2 {
                write_status(&mut stream, 500, "{\"error\":\"boom\"}");
            } else {
                write_json(&mut stream, CHAT_BODY);
            }
            count += 1;
            if count == 3 {
                return count;
            }
        }
        unreachable!("服务端不该在第三次之前退出");
    });

    let provider = provider(port, fast_policy(3, all_on()));
    let reply = provider.invoke(&request()).unwrap();
    assert_eq!(server.join().unwrap(), 3, "请求计数必须是 3");
    // 不变量：成功回执如实记下重试次数。
    assert_eq!(reply.retries, 2);
    assert_eq!(reply.text, "好");
}

#[test]
fn invoke_respects_max_attempts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut count = 0;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            read_request(&mut stream);
            write_status(&mut stream, 500, "{\"error\":\"boom\"}");
            count += 1;
            if count == 3 {
                return count;
            }
        }
        unreachable!("服务端不该在第三次之前退出");
    });

    let provider = provider(port, fast_policy(3, all_on()));
    let error = provider.invoke(&request()).unwrap_err();
    assert_eq!(server.join().unwrap(), 3, "请求计数必须正好是 max_attempts");
    // 不变量：最终失败只报最后一次的错误，且类别是 ServerError（状态码没被拍扁）。
    match error {
        ProviderError::ServerError { status, detail } => {
            assert_eq!(status, 500);
            assert!(detail.contains("boom"), "错误应带服务端正文：{detail}");
        }
        other => panic!("应报 ServerError，实际是 {other:?}"),
    }
}

#[test]
fn invoke_does_not_retry_rate_limited_when_disabled() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("应收到唯一一次请求");
        read_request(&mut stream);
        write_status(&mut stream, 429, "{\"error\":\"rate limited\"}");
        1_usize
    });

    let mut retry_on = all_on();
    retry_on.rate_limited = false;
    let provider = provider(port, fast_policy(3, retry_on));
    let error = provider.invoke(&request()).unwrap_err();
    assert_eq!(server.join().unwrap(), 1, "关掉 429 重试后必须一发一收");
    // 不变量：429 被分类为 RateLimited（不再是 Transport），且立即返回。
    assert!(
        matches!(error, ProviderError::RateLimited(ref msg) if msg.contains("rate limited")),
        "应报 RateLimited，实际是 {error:?}"
    );
}

#[test]
fn stream_retries_before_first_event_only() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut count = 0;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            read_request(&mut stream);
            if count == 0 {
                // 第一个连接收了请求后一个字节都不回就断开：
                // 零增量，允许重试。
                drop(stream);
            } else {
                sse_headers(&mut stream);
                write_chunk(&mut stream, &text_event("先"));
                write_chunk(&mut stream, &text_event("盖"));
                let usage = "{\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}";
                write_chunk(&mut stream, &format!("data: {usage}\n\n"));
                write_chunk(&mut stream, "data: [DONE]\n\n");
                stream.write_all(b"0\r\n\r\n").unwrap();
                stream.flush().unwrap();
            }
            count += 1;
            if count == 2 {
                return count;
            }
        }
        unreachable!("服务端不该在第二次之前退出");
    });

    let provider = provider(port, fast_policy(3, all_on()));
    let mut texts = Vec::new();
    let mut sink = |event: StreamEvent| -> StreamControl {
        if let StreamEvent::TextDelta(text) = event {
            texts.push(text);
        }
        StreamControl::Continue
    };
    let reply = provider.invoke_stream(&request(), &mut sink).unwrap();
    assert_eq!(server.join().unwrap(), 2, "断流一次后必须重连成功");
    // 不变量：零增量重试成功，且回执记下重试次数。
    assert_eq!(reply.retries, 1);
    assert_eq!(texts.join(""), "先盖");
}

#[test]
fn stream_never_retries_after_first_event() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        sse_headers(&mut stream);
        write_chunk(&mut stream, &text_event("一半"));
        // 发了一个 TextDelta 就掐断：已交付增量，绝不允许重试。
        drop(stream);
        // 探测窗口：此后任何新连接都算违规。
        listener
            .set_nonblocking(true)
            .expect("设非阻塞以探测多余连接");
        let deadline = Instant::now() + Duration::from_millis(400);
        let mut extra = 0;
        while Instant::now() < deadline {
            if listener.accept().is_ok() {
                extra += 1;
            }
            thread::sleep(Duration::from_millis(20));
        }
        extra
    });

    let provider = provider(port, fast_policy(3, all_on()));
    let mut sink = |_event: StreamEvent| -> StreamControl { StreamControl::Continue };
    let error = provider.invoke_stream(&request(), &mut sink).unwrap_err();
    assert_eq!(server.join().unwrap(), 0, "首个增量之后绝不允许再有连接");
    // 不变量：错误必须是 Protocol 且指明缺 [DONE]，不是被重试吞掉的半份回执。
    match error {
        ProviderError::Protocol(message) => {
            assert!(message.contains("[DONE]"), "应指明缺结束标记：{message}");
        }
        other => panic!("应判协议错误，实际是 {other:?}"),
    }
}
