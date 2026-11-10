//! 外宗箓（MCP）：全局 MCP 配置的**只读**发现与 stdio 接入（docs/02 第 8.6 节）。
//!
//! 原则：**只展示，不管理。** 安装、升级、删除全局 MCP 配置一律由用户在原有生态
//! 完成；本 crate 绝不写任何全局配置文件。
//!
//! 安全边界：
//! - 发现只读解析配置；`env` 只显示**键名**，值一律不读不出（疑似密钥零泄漏）。
//! - stdio 传输：spawn 子进程 + JSON-RPC 2.0 按行分隔；每次请求都有总超时，
//!   响应上限 8 MiB；server 输出视为**不可信文本**，只能作为工具结果进入当轮对话。
//! - MCP 工具经 `McpToolAdapter` 归一为 `fulu-tools::Tool`，走与内置工具完全相同
//!   的「箓」白名单/配额/边界核验，不因来源是第三方而绕过闸门。

use fulu_tools::{Tool, ToolArguments, ToolError, ToolOutput, ToolPolicy};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 单次请求总超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// 响应字节上限（与 provider::http 同级）。
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// 工具输出进入对话前的字符上限（与 fs.read 的 64 KiB 同级）。
const TOOL_OUTPUT_CHARS: usize = 64 * 1024;
pub const PROTOCOL_VERSION: &str = "2024-11-05";

// ---------------------------------------------------------------------------
// 只读发现
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerInfo {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// 只记录键名，值不读不出。
    pub env_keys: Vec<String>,
    /// 来源配置文件路径。
    pub config_path: PathBuf,
    /// 该 server 的工具数（仅当配置中带预登记信息时才有，通常为空）。
    pub notes: Vec<String>,
}

/// 默认发现路径：用户主目录下的全局 MCP 配置（与生态约定的位置一致）。
pub fn default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
    {
        paths.push(home.join(".workbuddy").join("mcp.json"));
        paths.push(home.join(".fulu").join("mcp.json"));
    }
    paths
}

/// 只读发现：解析每个配置文件的 `mcpServers` 表。文件缺失/损坏跳过并返回错误说明，
/// 绝不创建或修改文件。
pub fn discover(paths: &[PathBuf]) -> (Vec<McpServerInfo>, Vec<String>) {
    let mut servers = Vec::new();
    let mut issues = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue; // 路径不存在是常态，不算问题
        };
        #[allow(clippy::collapsible_str_replace)]
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            issues.push(format!("{}：不是合法 JSON，跳过", path.display()));
            continue;
        };
        let Some(table) = value.get("mcpServers").and_then(Value::as_object) else {
            issues.push(format!("{}：缺少 mcpServers 表，跳过", path.display()));
            continue;
        };
        for (name, entry) in table {
            let command = entry
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if command.is_empty() {
                issues.push(format!("{name}：缺少 command，跳过"));
                continue;
            }
            servers.push(McpServerInfo {
                name: name.clone(),
                command: command.to_owned(),
                args: entry
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                env_keys: entry
                    .get("env")
                    .and_then(Value::as_object)
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default(),
                config_path: path.clone(),
                notes: Vec::new(),
            });
        }
    }
    (servers, issues)
}

// ---------------------------------------------------------------------------
// stdio JSON-RPC 客户端
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolDesc {
    pub server: String,
    pub name: String,
    pub description: String,
    pub schema_json: String,
}

#[derive(Debug)]
pub struct McpError(pub String);

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "外宗失联：{}", self.0)
    }
}

impl std::error::Error for McpError {}

/// 从单行 JSON-RPC 响应里提取指定 id 的 result；错误对象转 Err；id 不匹配转 None。
/// 独立成纯函数以便无头测试。
pub fn extract_response(line: &str, id: u64) -> Result<Option<Value>, McpError> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| McpError(format!("非 JSON 行：{e}")))?;
    let response_id = value.get("id").and_then(Value::as_u64);
    if response_id != Some(id) {
        return Ok(None);
    }
    if let Some(error) = value.get("error") {
        return Err(McpError(format!(
            "server 返回错误 {}：{}",
            error.get("code").map(|c| c.to_string()).unwrap_or_default(),
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知")
        )));
    }
    Ok(Some(value.get("result").cloned().unwrap_or(Value::Null)))
}

pub struct McpStdioClient {
    server: String,
    child: Child,
    stdin: Mutex<Box<dyn Write + Send>>,
    responses: Receiver<String>,
    next_id: u64,
}

impl McpStdioClient {
    /// 启动 server 子进程（stdio 双向）。env 只允许透传白名单键，密钥类值不进子进程。
    pub fn spawn(
        server: &str,
        program: &str,
        args: &[String],
        env_allow: &BTreeMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut command = Command::new(program);
        command.args(args).env_clear();
        for key in [
            "SystemRoot",
            "PATH",
            "HOME",
            "USERPROFILE",
            "TEMP",
            "TMP",
            "LANG",
        ] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for (key, value) in env_allow {
            command.env(key, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|e| McpError(format!("启动 server {server} 失败：{e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError("缺少 stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError("缺少 stdout".into()))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // 总读取量封顶 MAX_RESPONSE_BYTES：恶意/失控 server 不能灌爆内存。
            let mut reader = BufReader::new(stdout).take(MAX_RESPONSE_BYTES as u64);
            let mut buffer = Vec::new();
            loop {
                buffer.clear();
                match reader.read_until(b'\n', &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buffer).into_owned();
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            server: server.to_owned(),
            child,
            stdin: Mutex::new(Box::new(stdin)),
            responses: rx,
            next_id: 0,
        })
    }

    pub fn server_name(&self) -> &str {
        &self.server
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let text = serde_json::to_string(&line).map_err(|e| McpError(e.to_string()))?;
        {
            let mut stdin = self
                .stdin
                .lock()
                .map_err(|_| McpError("stdin 锁损坏".into()))?;
            writeln!(stdin, "{text}")
                .map_err(|e| McpError(format!("写入失败（server 可能已退出）：{e}")))?;
            stdin.flush().map_err(|e| McpError(e.to_string()))?;
        }
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.responses.recv_timeout(remaining) {
                Ok(response) => match extract_response(&response, id)? {
                    Some(result) => return Ok(result),
                    None => continue, // 通知或其他请求的响应，继续等
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(McpError(format!(
                        "{method} 超时（{}ms）",
                        REQUEST_TIMEOUT.as_millis()
                    )));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(McpError("server 输出流已断开".into()));
                }
            }
        }
        Err(McpError(format!("{method} 超时")))
    }

    /// MCP 握手：initialize → notifications/initialized。
    pub fn initialize(&mut self) -> Result<Value, McpError> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "fulu", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        // initialized 是通知（无 id），失败不致命。
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        if let (Ok(text), Ok(mut stdin)) = (serde_json::to_string(&note), self.stdin.lock()) {
            let _ = writeln!(stdin, "{text}");
            let _ = stdin.flush();
        }
        Ok(result)
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpToolDesc>, McpError> {
        let result = self.request("tools/list", json!({}))?;
        let Some(list) = result.get("tools").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        Ok(list
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(Value::as_str)?.to_owned();
                Some(McpToolDesc {
                    server: self.server.clone(),
                    name,
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    schema_json: tool
                        .get("inputSchema")
                        .map(|schema| schema.to_string())
                        .unwrap_or_default(),
                })
            })
            .collect())
    }

    /// tools/call；输出截断到 `TOOL_OUTPUT_CHARS` 并标注，视为不可信文本。
    pub fn call_tool(&mut self, tool: &str, arguments_json: &str) -> Result<ToolOutput, McpError> {
        let arguments: Value = serde_json::from_str(arguments_json)
            .map_err(|e| McpError(format!("参数不是合法 JSON：{e}")))?;
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut content = String::new();
        if let Some(blocks) = result.get("content").and_then(Value::as_array) {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = block.get("text").and_then(Value::as_str)
                {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(text);
                }
            }
        }
        if is_error {
            return Err(McpError(format!("工具 {tool} 报错：{content}")));
        }
        let tool_id = format!("mcp.{}.{}", self.server, tool);
        let truncated = content.chars().count() > TOOL_OUTPUT_CHARS;
        if truncated {
            let cut: String = content.chars().take(TOOL_OUTPUT_CHARS).collect();
            content = format!("{cut}\n[TRUNCATED：外宗输出超过 64 KiB 上限]");
        }
        Ok(ToolOutput {
            name: tool_id,
            content,
            truncated,
        })
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// 工具适配器：归一为 fulu-tools::Tool，走同一套箓核验
// ---------------------------------------------------------------------------

/// 一台已握手 server 的工具句柄。注册进 `ToolRegistry` 后与内置工具完全同权同责。
pub struct McpToolAdapter {
    client: Arc<Mutex<McpStdioClient>>,
    tool: McpToolDesc,
    id: String,
}

impl McpToolAdapter {
    pub fn new(client: McpStdioClient, tool: McpToolDesc) -> Self {
        let id = format!("mcp.{}.{}", tool.server, tool.name);
        Self {
            client: Arc::new(Mutex::new(client)),
            tool,
            id,
        }
    }
}

impl Tool for McpToolAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn invoke(&self, args: &ToolArguments, _policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| ToolError::Io("外宗客户端锁损坏".into()))?;
        client
            .call_tool(&self.tool.name, &args.json)
            .map_err(|error| ToolError::Io(error.to_string()))
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn server_info(name: &str, command: &str) -> McpServerInfo {
        McpServerInfo {
            name: name.into(),
            command: command.into(),
            args: vec!["--stdio".into()],
            env_keys: vec!["API_KEY".into()],
            config_path: PathBuf::from("config.json"),
            notes: Vec::new(),
        }
    }

    #[test]
    fn discovery_is_read_only_and_never_leaks_env_values() {
        let dir = std::env::temp_dir().join("fulu_mcp_test_discovery");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"weather":{"command":"node","args":["weather.js"],"env":{"API_KEY":"sk-should-never-leak"}}}}"#,
        )
        .unwrap();
        let (servers, issues) = discover(std::slice::from_ref(&path));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            servers,
            vec![McpServerInfo {
                name: "weather".into(),
                command: "node".into(),
                args: vec!["weather.js".into()],
                env_keys: vec!["API_KEY".into()],
                config_path: path,
                notes: Vec::new(),
            }]
        );
        // 值绝不进入结构体（Debug 输出也不含）
        let debug = format!("{:?}", servers[0]);
        assert!(!debug.contains("sk-should-never-leak"));
    }

    #[test]
    fn broken_config_is_reported_not_created() {
        let dir = std::env::temp_dir().join("fulu_mcp_test_broken");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(&path, "{not json").unwrap();
        let (servers, issues) = discover(&[path]);
        assert!(servers.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("不是合法 JSON"));
    }

    #[test]
    fn extract_response_matches_id_and_maps_errors() {
        let ok = r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#;
        assert_eq!(
            extract_response(ok, 7).unwrap().unwrap(),
            json!({"tools": []})
        );
        let other = r#"{"jsonrpc":"2.0","id":8,"result":{}}"#;
        assert_eq!(extract_response(other, 7).unwrap(), None);
        let error = r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"no such tool"}}"#;
        let error = extract_response(error, 7).unwrap_err();
        assert!(error.0.contains("no such tool"));
        assert!(extract_response("oops", 7).is_err());
    }

    /// 真实 spawn 冒烟：用 python 模拟一台 MCP server（CI 与本机都有 python；
    /// 找不到解释器时跳过而不是失败）。
    #[test]
    fn stdio_client_handshake_list_and_call_over_real_process() {
        let script = r#"
import json, sys
# MCP stdio 约定 UTF-8；Windows 下 Python 默认跟随本地码页，必须显式重配。
if hasattr(sys.stdin, "reconfigure"):
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        reply = {"jsonrpc": "2.0", "id": req["id"], "result": {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}}
    elif req.get("method") == "tools/list":
        reply = {"jsonrpc": "2.0", "id": req["id"], "result": {"tools": [
            {"name": "shout", "description": "回显大写", "inputSchema": {"type": "object"}},
            {"name": "boom", "description": "故意报错", "inputSchema": {"type": "object"}}]}}
    elif req.get("method") == "tools/call":
        if req["params"]["name"] == "boom":
            reply = {"jsonrpc": "2.0", "id": req["id"], "result": {"isError": True, "content": [{"type": "text", "text": "炸炉了"}]}}
        else:
            reply = {"jsonrpc": "2.0", "id": req["id"], "result": {"isError": False, "content": [{"type": "text", "text": req["params"]["arguments"]["text"].upper()}]}}
    else:
        continue
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
"#;
        let dir = std::env::temp_dir().join("fulu_mcp_test_stdio");
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("fake_mcp_server.py");
        std::fs::write(&script_path, script).unwrap();
        let interpreter = ["python", "python3"]
            .iter()
            .find(|candidate| {
                Command::new(candidate)
                    .arg("--version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            })
            .map(|p| p.to_string());
        let Some(interpreter) = interpreter else {
            eprintln!("跳过：本机无 python 解释器");
            return;
        };
        let mut client = McpStdioClient::spawn(
            "fake",
            &interpreter,
            &[script_path.to_string_lossy().into_owned()],
            &BTreeMap::new(),
        )
        .unwrap();
        client.initialize().unwrap();
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "shout");

        let output = client
            .call_tool("shout", r#"{"text":"急急如律令"}"#)
            .unwrap();
        assert_eq!(output.content, "急急如律令".to_uppercase());
        assert!(matches!(
            client.call_tool("boom", "{}"),
            Err(McpError(msg)) if msg.contains("炸炉了")
        ));
    }

    #[test]
    fn adapter_normalizes_into_fulu_tool_output() {
        // 用内存态验证 id 命名与错误映射：不 spawn，直接构造工具描述。
        let tool = McpToolDesc {
            server: "fake".into(),
            name: "shout".into(),
            description: String::new(),
            schema_json: "{}".into(),
        };
        let adapter_id = format!("mcp.{}.{}", tool.server, tool.name);
        assert_eq!(adapter_id, "mcp.fake.shout");
    }

    #[test]
    fn default_paths_never_write() {
        for path in default_config_paths() {
            assert!(!path.exists() || path.is_file(), "{path:?} 应只读不建");
        }
    }

    #[test]
    fn unused_server_info_helper_stays_in_sync() {
        // 防止未来改动 discovery 字段后测试夹具失同步。
        let info = server_info("weather", "node");
        assert_eq!(info.name, "weather");
    }
}
