//! M3 端到端：**外宗（MCP）工具真正走进焚符循环**。
//!
//! 这条链路此前只到「能连上、能列工具」，法坛并不知道外宗工具的存在。
//! 本测试同时起两个真实端点：
//! 1. 假的 `llama-server`（本机 TCP/HTTP，OpenAI 兼容）——第一步返回工具调用，
//!    第二步看到工具回执后收敛；
//! 2. 假的 MCP server（Python 子进程，stdio JSON-RPC）——真实执行那次调用。
//!
//! 验证的是装配、箓核验、参数 schema 下发、子进程调用、回执入链这一整条路径，
//! 不是模型效果。找不到 python 时跳过（不静默失败）。

use fulu_app::AltarSession;
use fulu_core::{Sandbox, Talisman};
use fulu_mcp::McpServerInfo;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const FAKE_MCP_SERVER: &str = r#"
import json, sys
if hasattr(sys.stdin, "reconfigure"):
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        reply = {"jsonrpc": "2.0", "id": req["id"], "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-echo", "version": "0.0.1"}}}
    elif method == "tools/list":
        reply = {"jsonrpc": "2.0", "id": req["id"], "result": {"tools": [
            {"name": "shout",
             "description": "把文本转成大写后回显",
             "inputSchema": {"type": "object",
                             "properties": {"text": {"type": "string"}},
                             "required": ["text"]},
             "annotations": {"readOnlyHint": True, "destructiveHint": False}},
            {"name": "purge",
             "description": "清空远端缓存（破坏性，不可撤销）",
             "inputSchema": {"type": "object", "properties": {}},
             "annotations": {"destructiveHint": True}}]}}
    elif method == "tools/call":
        if req["params"]["name"] == "purge":
            text = "已清空远端缓存"
        else:
            text = req["params"]["arguments"]["text"].upper()
        reply = {"jsonrpc": "2.0", "id": req["id"], "result": {
            "isError": False,
            "content": [{"type": "text", "text": text}]}}
    else:
        continue
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
"#;

fn python() -> Option<String> {
    ["python", "python3"]
        .iter()
        .find(|candidate| {
            Command::new(candidate)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
        .map(|p| (*p).to_string())
}

/// 把请求读完整：客户端可能分两次写，只读一次就响应会偶发 ECONNRESET。
fn read_request(stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
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

/// 按脚本顺序为每个连接返回一段 JSON；用完即退出。
fn spawn_mock_llama(responses: Vec<String>) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            let raw = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(raw.as_bytes());
            let _ = stream.flush();
        }
    });
    (port, handle)
}

fn fake_server_info(python: &str, script: &str) -> McpServerInfo {
    McpServerInfo {
        name: "fake".into(),
        transport: fulu_mcp::Transport::Stdio {
            command: python.into(),
            args: vec![script.into()],
        },
        env_keys: Vec::new(),
        config_path: PathBuf::from("<test>"),
        notes: Vec::new(),
    }
}

fn write_script(name: &str) -> String {
    let dir = std::env::temp_dir().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake_mcp_server.py");
    std::fs::write(&path, FAKE_MCP_SERVER).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn outer_sect_tool_runs_inside_the_ritual_loop() {
    let Some(python) = python() else {
        eprintln!("跳过：本机无 python 解释器");
        return;
    };
    let script = write_script("fulu_app_test_mcp_loop");

    // 第一步：模型要求召外宗将；第二步：看到回执后收敛。
    let first = r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call-1","type":"function","function":{"name":"mcp.fake.shout","arguments":"{\"text\":\"云篆归一\"}"}}]}}],"usage":{"prompt_tokens":12,"completion_tokens":8}}"#;
    let second = r#"{"choices":[{"message":{"content":"完成：云篆归一（已由外宗回声）"}}],"usage":{"prompt_tokens":18,"completion_tokens":10}}"#;
    let (port, handle) = spawn_mock_llama(vec![first.into(), second.into()]);

    let mut session = AltarSession::new();
    session
        .set_endpoint(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    let report = session.attach_mcp(&[fake_server_info(&python, &script)]);
    assert!(report.servers[0].connected, "{:?}", report.servers[0]);
    assert!(report.tool_ids().contains(&"mcp.fake.shout".to_owned()));
    // 只读注解免确认，破坏性的 purge 必须计入「需人确认」。
    assert_eq!(
        report.summary(),
        "外宗：1/1 台已接入，共 2 个工具（其中 1 个需人确认）"
    );
    assert_eq!(report.gated_tools().len(), 1);
    assert_eq!(report.gated_tools()[0].id, "mcp.fake.purge");

    // 外宗工具固定「受限级」：符的沙箱必须是 limited 才配得上。
    let mut fu = Talisman::sample();
    fu.harness.sandbox = Sandbox::Limited;
    fu.harness.max_steps = 4;
    fu.tool_refs = vec!["mcp.fake.shout".into()];
    fu.harness.allow_tools = fu.tool_refs.clone();
    fu.goal.success_criteria = "contains:完成".into();
    fu.output.until = "success".into();

    let plan = session.dry_run(&fu).unwrap();
    assert!(
        plan.tools.contains(&"mcp.fake.shout".to_owned()),
        "外宗工具应出现在待焚计划里：{:?}",
        plan.tools
    );
    assert!(
        plan.notes.iter().any(|note| note.contains("外宗：1/1")),
        "待焚计划必须交代外宗影响面：{:?}",
        plan.notes
    );

    let receipt = session.ignite(&fu, &plan, true).unwrap();
    assert_eq!(receipt.steps, 2, "第一步召将，第二步收敛");
    assert!(receipt.converged);
    assert!(receipt.text.contains("完成"));
    assert!(
        receipt
            .events
            .iter()
            .any(|event| event.detail.contains("召将 name=mcp.fake.shout")),
        "事件链里必须有外宗召将记录"
    );
    assert!(
        receipt
            .events
            .iter()
            .any(|event| event.detail.contains("将回 name=mcp.fake.shout")),
        "外宗回执必须入链"
    );
    session.audit().verify_chain().unwrap();
    handle.join().unwrap();
}

#[test]
fn read_only_talisman_cannot_call_outer_sect_tools() {
    // 只有发现了外宗，才可能「误把只读符伸向 MCP」；这里不需要真跑子进程，
    // 直接验证箓层的双向核验在装配后依然拦得住。
    let Some(python) = python() else {
        eprintln!("跳过：本机无 python 解释器");
        return;
    };
    let script = write_script("fulu_app_test_mcp_strict");

    let mut session = AltarSession::new();
    session.attach_mcp(&[fake_server_info(&python, &script)]);

    let mut fu = Talisman::sample();
    fu.tool_refs = vec!["mcp.fake.shout".into()];
    fu.harness.allow_tools = fu.tool_refs.clone(); // 沙箱仍是 Strict

    let error = session
        .dry_run(&fu)
        .expect_err("只读符申请外宗工具必须被驳回");
    let message = error.to_string();
    assert!(
        message.contains("权限不足") && message.contains("mcp.fake.shout"),
        "驳回理由必须点名工具与权限差距：{message}"
    );
}

#[test]
fn unreachable_server_is_reported_not_guessed() {
    let info = McpServerInfo {
        name: "nowhere".into(),
        transport: fulu_mcp::Transport::Stdio {
            command: "fulu-no-such-binary-xyz".into(),
            args: Vec::new(),
        },
        env_keys: vec!["SOME_KEY".into()],
        config_path: PathBuf::from("<test>"),
        notes: Vec::new(),
    };
    let mut session = AltarSession::new();
    let report = session.attach_mcp(&[info]);
    assert_eq!(report.servers.len(), 1);
    assert!(!report.servers[0].connected);
    assert!(report.servers[0].tools.is_empty());
    let error = report.servers[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("nowhere"), "错误必须点名 server：{error}");

    // 连不上不影响内置工具：示例符照常可以 dry-run。
    let plan = session.dry_run(&Talisman::sample()).unwrap();
    assert_eq!(plan.tools, vec!["fs.read".to_owned()]);
}

/// 破坏性外宗工具：无人值守时必须被敕否，并且模型能拿到拒绝理由继续走。
#[test]
fn destructive_outer_sect_tool_is_denied_without_a_confirmer() {
    let Some(python) = python() else {
        eprintln!("跳过：本机无 python 解释器");
        return;
    };
    let script = write_script("fulu_app_test_mcp_gate_deny");

    let first = r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call-1","type":"function","function":{"name":"mcp.fake.purge","arguments":"{}"}}]}}],"usage":{"prompt_tokens":12,"completion_tokens":8}}"#;
    let second = r#"{"choices":[{"message":{"content":"完成：未获许可，不做清理"}}],"usage":{"prompt_tokens":18,"completion_tokens":10}}"#;
    let (port, handle) = spawn_mock_llama(vec![first.into(), second.into()]);

    let mut session = AltarSession::new();
    session
        .set_endpoint(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    session.attach_mcp(&[fake_server_info(&python, &script)]);

    let mut fu = Talisman::sample();
    fu.harness.sandbox = Sandbox::Limited;
    fu.harness.max_steps = 4;
    fu.tool_refs = vec!["mcp.fake.purge".into()];
    fu.harness.allow_tools = fu.tool_refs.clone();
    fu.goal.success_criteria = "contains:完成".into();

    let plan = session.dry_run(&fu).unwrap();
    assert!(
        plan.notes
            .iter()
            .any(|note| note.contains("确认门") && note.contains("mcp.fake.purge")),
        "待焚计划必须先点名会被拦下的召将：{:?}",
        plan.notes
    );

    // 刻意不装确认器：法坛必须 fail closed。
    let receipt = session.ignite(&fu, &plan, true).unwrap();
    let labels: Vec<&str> = receipt
        .events
        .iter()
        .map(|event| event.kind.label())
        .collect();
    assert!(
        labels.contains(&"敕否"),
        "未确认的破坏性召将必须记敕否：{labels:?}"
    );
    assert!(
        !labels.contains(&"将回"),
        "被敕否的工具绝不能被真的执行：{labels:?}"
    );
    assert!(receipt.converged, "模型应当能绕开被拒的工具完成任务");
    session.audit().verify_chain().unwrap();
    handle.join().unwrap();
}

/// 同一张符，用户显式预授权后即可放行——授权痕迹必须落在缴符里。
#[test]
fn destructive_outer_sect_tool_runs_after_explicit_pre_authorization() {
    let Some(python) = python() else {
        eprintln!("跳过：本机无 python 解释器");
        return;
    };
    let script = write_script("fulu_app_test_mcp_gate_allow");

    let first = r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call-1","type":"function","function":{"name":"mcp.fake.purge","arguments":"{}"}}]}}],"usage":{"prompt_tokens":12,"completion_tokens":8}}"#;
    let second = r#"{"choices":[{"message":{"content":"完成：已清理"}}],"usage":{"prompt_tokens":18,"completion_tokens":10}}"#;
    let (port, handle) = spawn_mock_llama(vec![first.into(), second.into()]);

    let mut session = AltarSession::new();
    session
        .set_endpoint(&format!("http://127.0.0.1:{port}"))
        .unwrap();
    session.attach_mcp(&[fake_server_info(&python, &script)]);
    session.set_risk_ceiling(Some(fulu_app::RiskClass::Destructive));

    let mut fu = Talisman::sample();
    fu.harness.sandbox = Sandbox::Limited;
    fu.harness.max_steps = 4;
    fu.tool_refs = vec!["mcp.fake.purge".into()];
    fu.harness.allow_tools = fu.tool_refs.clone();
    fu.goal.success_criteria = "contains:完成".into();

    let plan = session.dry_run(&fu).unwrap();
    let receipt = session.ignite(&fu, &plan, true).unwrap();
    let labels: Vec<&str> = receipt
        .events
        .iter()
        .map(|event| event.kind.label())
        .collect();
    assert!(labels.contains(&"敕可"), "预授权放行必须留痕：{labels:?}");
    assert!(labels.contains(&"将回"), "放行后应当真的执行：{labels:?}");
    assert!(receipt.text.contains("已清理"));
    session.audit().verify_chain().unwrap();
    handle.join().unwrap();
}
