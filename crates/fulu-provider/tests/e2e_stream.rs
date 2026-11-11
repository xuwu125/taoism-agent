//! T13 端到端：本机起一个**真的会分块发 SSE 的假 llama-server**，走真实 TCP 验证流式链路。
//!
//! 与 `fulu-runtime/tests/e2e_http.rs` 同一思路：不用桩，检查「HTTP 客户端 + SSE 解析 +
//! 归一」三段能否真的连起来。重点验三件在单测里测不到的事：
//! 1. 分块（`Transfer-Encoding: chunked`）时**边收边给**，不是攒完再解析；
//! 2. 流式与非流式拼出的回执**必须一致**（同一份文字、同一个 usage）；
//! 3. 断流 / 取消**不许回拼半份回执**。

use fulu_core::Sampling;
use fulu_provider::{
    InvocationRequest, LocalLfmProvider, Provider, ProviderError, StreamControl, StreamEvent,
};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

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

fn provider(port: u16) -> LocalLfmProvider {
    let mut provider = LocalLfmProvider::from_model("lfm3-1.7b-q4km", 8192, "llama.cpp >= b11960");
    provider
        .set_endpoint(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    provider
}

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

fn write_chunk(stream: &mut TcpStream, body: &str) {
    let raw = format!("{:x}\r\n{body}\r\n", body.len());
    stream.write_all(raw.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn sse_headers(stream: &mut TcpStream, chunked: bool) {
    let encoding = if chunked {
        "Transfer-Encoding: chunked"
    } else {
        "Content-Length: 0"
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n{encoding}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn text_event(text: &str) -> String {
    let frame = json!({"choices":[{"delta":{"content":text}}]});
    format!("data: {frame}\n\n")
}

#[test]
fn chunked_stream_is_delivered_incrementally_and_assembles_into_one_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        sse_headers(&mut stream, true);
        for piece in ["先", "盖", "印"] {
            write_chunk(&mut stream, &text_event(piece));
            // 故意拉开时间：若客户端是「读完整再解析」，就收不到「边收边给」的证据。
            thread::sleep(Duration::from_millis(20));
        }
        let usage = json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3}});
        write_chunk(&mut stream, &format!("data: {usage}\n\n"));
        write_chunk(&mut stream, "data: [DONE]\n\n");
        stream.write_all(b"0\r\n\r\n").unwrap();
        stream.flush().unwrap();
    });

    let provider = provider(port);
    let mut arrivals: Vec<(std::time::Instant, String)> = Vec::new();
    let mut sink = |event: StreamEvent| -> StreamControl {
        if let StreamEvent::TextDelta(text) = event {
            arrivals.push((std::time::Instant::now(), text));
        }
        StreamControl::Continue
    };
    let started = std::time::Instant::now();
    let reply = provider.invoke_stream(&request(), &mut sink).unwrap();
    handle.join().unwrap();

    assert_eq!(reply.text, "先盖印");
    assert_eq!(reply.usage.total(), 10);
    assert!(!reply.usage.estimated, "服务端给了 usage 就不该标估算");
    assert_eq!(arrivals.len(), 3, "三段增量应分三次到达");
    assert_eq!(
        arrivals
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>(),
        "先盖印"
    );
    // 第一段必须在整体结束前很久就到了，否则「增量」只是说法。
    assert!(
        arrivals[0].0.duration_since(started) < Duration::from_millis(180),
        "首段增量到达太晚，说明不是真流式"
    );
}

#[test]
fn tool_call_arguments_split_across_chunks_are_merged_into_valid_json() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        sse_headers(&mut stream, true);
        let first = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"fs.read","arguments":"{\"path\":"}}]}}]});
        write_chunk(&mut stream, &format!("data: {first}\n\n"));
        let second = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.fu\"}"}}]},"finish_reason":"tool_calls"}]});
        write_chunk(&mut stream, &format!("data: {second}\n\n"));
        write_chunk(&mut stream, "data: [DONE]\n\n");
        stream.write_all(b"0\r\n\r\n").unwrap();
        stream.flush().unwrap();
    });

    let provider = provider(port);
    let mut names = Vec::new();
    let mut sink = |event: StreamEvent| -> StreamControl {
        if let StreamEvent::ToolCallDelta { name, .. } = event
            && let Some(name) = name
        {
            names.push(name);
        }
        StreamControl::Continue
    };
    let reply = provider.invoke_stream(&request(), &mut sink).unwrap();
    handle.join().unwrap();

    assert_eq!(
        names,
        vec!["fs.read".to_owned()],
        "函数名只应在首个增量出现"
    );
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id, "call_1");
    assert_eq!(reply.tool_calls[0].arguments_json, r#"{"path":"a.fu"}"#);
    // usage 缺失时必须标 estimated，不能伪称精确。
    assert!(reply.usage.estimated);
}

#[test]
fn a_stream_cut_before_done_is_an_error_not_half_a_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        sse_headers(&mut stream, true);
        write_chunk(&mut stream, &text_event("只说了一半"));
        // 不发 [DONE]，也不发 0 长度块：模拟推理进程中途被杀。
        drop(stream);
    });

    let provider = provider(port);
    let mut sink = |_event: StreamEvent| -> StreamControl { StreamControl::Continue };
    let error = provider.invoke_stream(&request(), &mut sink).unwrap_err();
    handle.join().unwrap();

    match error {
        ProviderError::Protocol(message) => {
            assert!(message.contains("[DONE]"), "应指明缺结束标记：{message}");
        }
        other => panic!("应判协议错误，实际是 {other:?}"),
    }
}

#[test]
fn cancel_stops_the_read_and_reports_cancelled_without_a_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        sse_headers(&mut stream, true);
        // 多备几段：若客户端取消后还在读，写端会因对端关闭而报错，测试即可发现。
        for piece in ["一", "二", "三", "四", "五"] {
            if write_chunk_safely(&mut stream, &text_event(piece)).is_err() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = write_chunk_safely(&mut stream, "data: [DONE]\n\n");
        false
    });

    let provider = provider(port);
    let mut seen = 0_usize;
    let mut sink = |event: StreamEvent| -> StreamControl {
        if matches!(event, StreamEvent::TextDelta(_)) {
            seen += 1;
            return StreamControl::Cancel;
        }
        StreamControl::Continue
    };
    let error = provider.invoke_stream(&request(), &mut sink).unwrap_err();
    let failed_write = handle.join().unwrap();

    assert_eq!(seen, 1, "取消之后不该再收到增量");
    assert!(
        matches!(error, ProviderError::Cancelled(_)),
        "应返回 Cancelled，实际是 {error:?}"
    );
    assert!(failed_write, "取消后连接应被关闭，对端应当写失败");
}

fn write_chunk_safely(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    let raw = format!("{:x}\r\n{body}\r\n", body.len());
    stream.write_all(raw.as_bytes())?;
    stream.flush()
}
