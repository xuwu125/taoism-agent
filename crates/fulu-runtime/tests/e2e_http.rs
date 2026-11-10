//! M2 端到端：本机起一个假的 `llama-server`，走**真实 TCP/HTTP** 完成整场仪轨。
//!
//! 这里刻意不用 `StubProvider`：它验证的是「法坛 + provider 适配 + HTTP 客户端」
//! 三段确实能连起来，包括 Content-Length 解析、OpenAI 兼容响应解析、
//! 工具调用归一化与回执链。权重与推理引擎仍在进程外，本测试不下载任何模型。

use fulu_audit::AuditLog;
use fulu_core::Talisman;
use fulu_provider::{Health, LocalLfmProvider, Provider};
use fulu_register::Register;
use fulu_runtime::{Altar, RitualConfig};
use fulu_tools::ToolRegistry;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// 按脚本顺序为每个连接返回一段 JSON；连接数用尽后线程自行退出。
fn spawn_mock_server(responses: Vec<String>) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 8192];
            let _ = stream.read(&mut buffer);
            let raw = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(raw.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    });
    (port, handle)
}

fn provider(port: u16) -> LocalLfmProvider {
    let mut provider = LocalLfmProvider::from_model("lfm3-1.7b-q4km", 8192, "llama.cpp >= b11960");
    provider
        .set_endpoint(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    provider
}

#[test]
fn health_models_and_full_ritual_over_real_http() {
    let chat = r#"{"choices":[{"message":{"content":"摘要完成"}}],"usage":{"prompt_tokens":11,"completion_tokens":9}}"#;
    let (port, handle) = spawn_mock_server(vec![
        r#"{"status":"ok"}"#.into(),
        r#"{"data":[{"id":"lfm3-1.7b-q4km"}]}"#.into(),
        chat.into(),
    ]);
    let provider = provider(port);

    assert_eq!(
        provider.health().unwrap(),
        Health {
            ready: true,
            detail: r#"{"status":"ok"}"#.into(),
        }
    );
    assert_eq!(
        provider.loaded_models().unwrap(),
        vec!["lfm3-1.7b-q4km".to_owned()]
    );

    let fu = Talisman::sample();
    let register = Register::baseline();
    let tools = ToolRegistry::default();
    let mut audit = AuditLog::new();
    let mut altar = Altar::new(
        &provider,
        &tools,
        &register,
        &mut audit,
        RitualConfig::from_talisman(&fu),
    );
    let plan = altar.prepare(&fu).unwrap();
    assert_eq!(provider.capabilities().max_context_tokens, 8192);
    let receipt = altar.ignite(&fu, &plan, true).unwrap();

    assert_eq!(receipt.text, "摘要完成");
    assert!(receipt.converged);
    assert_eq!(receipt.usage_total, 20);
    assert!(!receipt.estimated_usage);
    assert_eq!(receipt.steps, 1);
    audit.verify_chain().unwrap();
    handle.join().unwrap();
}

#[test]
fn tool_calls_from_http_are_normalized_and_executed() {
    let first = r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"c1","function":{"name":"echo","arguments":"{\"text\":\"HTTP 工具回执\"}"}}]}}],"usage":{"prompt_tokens":6,"completion_tokens":4}}"#;
    let second = r#"{"choices":[{"message":{"content":"完成"}}],"usage":{"prompt_tokens":8,"completion_tokens":3}}"#;
    let (port, handle) = spawn_mock_server(vec![first.into(), second.into()]);
    let provider = provider(port);

    let mut fu = Talisman::sample();
    fu.tool_refs = vec!["echo".into()];
    fu.harness.allow_tools = vec!["echo".into()];
    let register = Register::baseline();
    let tools = ToolRegistry::default();
    let mut audit = AuditLog::new();
    let mut altar = Altar::new(
        &provider,
        &tools,
        &register,
        &mut audit,
        RitualConfig::from_talisman(&fu),
    );
    let plan = altar.prepare(&fu).unwrap();
    let receipt = altar.ignite(&fu, &plan, true).unwrap();

    assert_eq!(receipt.steps, 2, "第一次请神召将，第二次请神收敛");
    assert_eq!(receipt.text, "完成");
    assert!(receipt.converged);
    assert!(
        receipt
            .events
            .iter()
            .any(|event| event.detail.contains("将回 name=echo"))
    );
    handle.join().unwrap();
}

#[test]
fn pythonic_tool_call_over_http_is_also_normalized() {
    let first = r#"{"choices":[{"message":{"content":"请调用 echo(text=\"云篆归一\")"}}],"usage":{"prompt_tokens":5,"completion_tokens":5}}"#;
    let second = r#"{"choices":[{"message":{"content":"完成"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#;
    let (port, handle) = spawn_mock_server(vec![first.into(), second.into()]);
    let provider = provider(port);

    let mut fu = Talisman::sample();
    fu.tool_refs = vec!["echo".into()];
    fu.harness.allow_tools = vec!["echo".into()];
    let register = Register::baseline();
    let tools = ToolRegistry::default();
    let mut audit = AuditLog::new();
    let mut altar = Altar::new(
        &provider,
        &tools,
        &register,
        &mut audit,
        RitualConfig::from_talisman(&fu),
    );
    let plan = altar.prepare(&fu).unwrap();
    let receipt = altar.ignite(&fu, &plan, true).unwrap();
    assert_eq!(receipt.steps, 2);
    assert!(
        receipt
            .events
            .iter()
            .any(|event| event.detail.contains("召将 name=echo"))
    );
    handle.join().unwrap();
}
