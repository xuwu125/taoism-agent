//! 外宗箓（MCP）：全局 MCP 配置的**只读**发现与 stdio 接入（docs/02 第 8.6 节）。
//!
//! 原则：**只展示，不管理。** 安装、升级、删除全局 MCP 配置一律由用户在原有生态
//! 完成；本 crate 绝不写任何全局配置文件。唯一会写的是 **fulu 自有**的
//! `~/.fulu/outer-sect-risk.json` 风险覆盖表（T05），与 MCP 生态配置无关。
//!
//! 安全边界：
//! - 发现只读解析配置；`env` 只显示**键名**，值一律不读不出（疑似密钥零泄漏）。
//! - stdio 传输：spawn 子进程 + JSON-RPC 2.0 按行分隔；每次请求都有总超时，
//!   响应上限 8 MiB；server 输出视为**不可信文本**，只能作为工具结果进入当轮对话。
//! - MCP 工具经 `McpToolAdapter` 归一为 `fulu-tools::Tool`，走与内置工具完全相同
//!   的「箓」白名单/配额/边界核验，不因来源是第三方而绕过闸门。

use fulu_register::RiskClass;
use fulu_tools::{Tool, ToolArguments, ToolError, ToolOutput, ToolPolicy};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
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

/// 一台 server 的接入方式。
///
/// 只支持两种：本机 stdio 子进程、本机明文回环 HTTP。
/// 远程 MCP（HTTPS）需要 TLS 边界，属独立增量，不在这里偷偷打开。
#[derive(Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
    },
    StreamableHttp {
        url: String,
        /// 认证头等。**值只在内存里用**：Debug 打码、不进日志、不进缴符。
        headers: Vec<(String, String)>,
    },
}

impl Transport {
    /// 给人看的一行描述（永不包含 header 值）。
    pub fn label(&self) -> String {
        match self {
            Self::Stdio { command, args } => {
                if args.is_empty() {
                    format!("stdio: {command}")
                } else {
                    format!("stdio: {command} {}", args.join(" "))
                }
            }
            Self::StreamableHttp { url, headers } => {
                if headers.is_empty() {
                    format!("http: {url}")
                } else {
                    format!("http: {url}（{} 个请求头）", headers.len())
                }
            }
        }
    }
}

/// 手写 Debug：`env` 与 `headers` 的值一律打码。
/// 这条不是洁癖——`format!("{:?}")` 会顺手把配置里的 Bearer token 写进日志。
impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdio { command, args } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .finish(),
            Self::StreamableHttp { url, headers } => f
                .debug_struct("StreamableHttp")
                .field("url", url)
                .field(
                    "headers",
                    &headers
                        .iter()
                        .map(|(name, _)| format!("{name}: <masked>"))
                        .collect::<Vec<_>>(),
                )
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct McpServerInfo {
    pub name: String,
    pub transport: Transport,
    /// 只记录键名，值不读不出。
    pub env_keys: Vec<String>,
    /// 来源配置文件路径。
    pub config_path: PathBuf,
    /// 该 server 的工具数（仅当配置中带预登记信息时才有，通常为空）。
    pub notes: Vec<String>,
}

impl std::fmt::Debug for McpServerInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerInfo")
            .field("name", &self.name)
            .field("transport", &self.transport)
            .field("env_keys", &self.env_keys)
            .field("config_path", &self.config_path)
            .field("notes", &self.notes)
            .finish()
    }
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

/// fulu 自己的外宗风险覆盖表文件名。
///
/// 这是 **fulu 的**配置，不是 MCP 生态的配置：它不修改 server 的任何声明，
/// 只是让用户对「server 压根没声明」的工具表态。系统里没有这个文件时
/// 一切照缺省走，不存在也不影响运行。
pub const RISK_OVERRIDE_FILE: &str = "outer-sect-risk.json";

/// 覆盖表的位置。与全局 MCP 配置的 `~/.fulu/mcp.json` 同目录，便于一起备份。
pub fn risk_override_paths() -> Vec<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| vec![PathBuf::from(home).join(".fulu").join(RISK_OVERRIDE_FILE)])
        .unwrap_or_default()
}

/// 工具风险的来源，用于在界面上交代「这个等级是谁说的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskSource {
    /// server 在 annotations 里声明的
    Declared,
    /// 用户在 fulu 覆盖表里显式写的
    Override,
    /// server 没说，按缺省（破坏性）处理
    Undeclared,
    /// 被策略下限抬上来的（`--strict-outer-sect` 等）
    Floor,
}

impl RiskSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Declared => "server 自述",
            Self::Override => "fulu 覆盖表",
            Self::Undeclared => "未声明（按破坏性）",
            Self::Floor => "策略下限",
        }
    }
}

/// 用户自定的工具风险表。
///
/// 只能按**工具全名**逐条声明，不做任何模糊匹配：猜错的代价是放行一个
/// 破坏性操作，比多按一次确认贵得多。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RiskOverrides {
    map: BTreeMap<String, RiskClass>,
}

impl RiskOverrides {
    /// 读取并合并多份覆盖表；坏条目逐条报 issue，不影响其它条目。
    pub fn load(paths: &[PathBuf]) -> (Self, Vec<String>) {
        let mut overrides = Self::default();
        let mut issues = Vec::new();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue; // 没有覆盖表是常态
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                issues.push(format!(
                    "{}：不是合法 JSON，整份覆盖表已忽略",
                    path.display()
                ));
                continue;
            };
            let Some(table) = value.get("tools").and_then(Value::as_object) else {
                issues.push(format!(
                    "{}：缺少 tools 表，整份覆盖表已忽略",
                    path.display()
                ));
                continue;
            };
            for (id, raw) in table {
                let level = raw.as_str().map(str::to_ascii_lowercase);
                let risk = match level.as_deref() {
                    Some("observe") => RiskClass::Observe,
                    Some("mutate") => RiskClass::Mutate,
                    Some("destructive") => RiskClass::Destructive,
                    _ => {
                        issues.push(format!(
                            "{id}：风险等级「{}」不合法，须为 observe / mutate / destructive，已忽略",
                            raw.as_str().unwrap_or("非字符串")
                        ));
                        continue;
                    }
                };
                overrides.map.insert(id.clone(), risk);
            }
        }
        (overrides, issues)
    }

    pub fn get(&self, tool_id: &str) -> Option<RiskClass> {
        self.map.get(tool_id).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 覆盖表条目（按工具全名字典序），供 `mcp risk list` 展示。
    pub fn entries(&self) -> impl Iterator<Item = (&String, &RiskClass)> {
        self.map.iter()
    }

    /// 覆盖表里指向不存在工具的条目（多半是 server 升级后改了工具名）。
    pub fn stale_entries(&self, tool_ids: &[String]) -> Vec<String> {
        self.map
            .keys()
            .filter(|id| !tool_ids.iter().any(|known| known == *id))
            .cloned()
            .collect()
    }

    /// 写入一条覆盖；`None` 表示撤销该工具的覆盖。
    pub fn set_entry(&mut self, tool_id: &str, risk: Option<RiskClass>) {
        match risk {
            Some(level) => {
                self.map.insert(tool_id.to_owned(), level);
            }
            None => {
                self.map.remove(tool_id);
            }
        }
    }

    /// 序列化成人类可读、可手工审阅的 JSON（坏条目已在 `load` 阶段剔除）。
    pub fn to_json_pretty(&self) -> Result<String, String> {
        let mut tools = serde_json::Map::new();
        for (id, risk) in &self.map {
            tools.insert(
                id.clone(),
                serde_json::Value::String(risk_key(*risk).into()),
            );
        }
        let mut root = serde_json::Map::new();
        root.insert("tools".into(), serde_json::Value::Object(tools));
        serde_json::to_string_pretty(&serde_json::Value::Object(root))
            .map_err(|error| format!("覆盖表序列化失败：{error}"))
    }

    /// 原子写：先写同目录临时文件，再替换目标；替换失败时尽力恢复旧文件。
    ///
    /// 只写 fulu 自己的配置路径（由调用方给定），绝不触碰 MCP 生态的配置。
    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        let text = format!("{}\n", self.to_json_pretty()?);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建 {}：{error}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let backup = path.with_extension("json.bak");
        std::fs::write(&tmp, text)
            .map_err(|error| format!("写入 {} 失败：{error}", tmp.display()))?;

        // Windows 上 rename 未必能覆盖已存在文件：先把旧文件挪开，失败则回滚。
        let had_old = path.exists();
        if had_old {
            let _ = std::fs::remove_file(&backup);
            std::fs::rename(path, &backup).map_err(|error| format!("备份旧覆盖表失败：{error}"))?;
        }
        if let Err(error) = std::fs::rename(&tmp, path) {
            if had_old {
                let _ = std::fs::rename(&backup, path); // 尽力恢复
            }
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("替换覆盖表失败：{error}"));
        }
        if had_old {
            let _ = std::fs::remove_file(&backup);
        }
        Ok(())
    }
}

/// 覆盖表里的等级字符串（与 `RiskClass` 的 serde snake_case 一致）。
pub fn risk_key(risk: RiskClass) -> &'static str {
    match risk {
        RiskClass::Observe => "observe",
        RiskClass::Mutate => "mutate",
        RiskClass::Destructive => "destructive",
    }
}

/// 解析 CLI 传入的等级；不认识的值必须报错，绝不猜一个等级放行。
pub fn parse_risk(raw: &str) -> Option<RiskClass> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "observe" => Some(RiskClass::Observe),
        "mutate" => Some(RiskClass::Mutate),
        "destructive" => Some(RiskClass::Destructive),
        _ => None,
    }
}

/// 写入/撤销一条覆盖，返回更新后的覆盖表与读取时的坏条目说明。
///
/// 工具全名必须是精确名字：含通配符、空白或空串一律拒绝。猜错名字的代价
/// 是放行一个破坏性操作，比让用户多写几个字符贵得多。
pub fn set_risk_override(
    path: &Path,
    tool_id: &str,
    risk: Option<RiskClass>,
) -> Result<(RiskOverrides, Vec<String>), String> {
    let id = tool_id.trim();
    if id.is_empty()
        || id
            .chars()
            .any(|ch| ch.is_whitespace() || ch == '*' || ch == '?')
    {
        return Err(
            "工具名必须是精确全名（如 mcp.cucumber.read_file），不接受空白、* 或 ? 通配".into(),
        );
    }
    let (mut overrides, issues) = RiskOverrides::load(&[path.to_path_buf()]);
    overrides.set_entry(id, risk);
    overrides.write_to(path)?;
    Ok((overrides, issues))
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
            let entry_type = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("stdio")
                .to_ascii_lowercase();
            let transport = if entry_type.contains("http") || entry.get("url").is_some() {
                let url = entry.get("url").and_then(Value::as_str).unwrap_or_default();
                if url.is_empty() {
                    issues.push(format!("{name}：声明为 HTTP 传输但缺少 url，跳过"));
                    continue;
                }
                // 头部的值必须读（否则认证型 server 根本连不上），但只在内存里用。
                let headers = entry
                    .get("headers")
                    .and_then(Value::as_object)
                    .map(|map| {
                        map.iter()
                            .filter_map(|(key, value)| {
                                value.as_str().map(|v| (key.clone(), v.to_owned()))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Transport::StreamableHttp {
                    url: url.to_owned(),
                    headers,
                }
            } else {
                let command = entry
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if command.is_empty() {
                    issues.push(format!("{name}：缺少 command，跳过"));
                    continue;
                }
                Transport::Stdio {
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
                }
            };
            servers.push(McpServerInfo {
                name: name.clone(),
                transport,
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
    /// 由 `annotations` 折出的副作用等级；缺失或自相矛盾时取最严。
    /// 接入时会再经覆盖表与策略下限修正，来源见 `risk_source`。
    pub risk: RiskClass,
    /// 最终等级的来源（自述 / 覆盖表 / 缺省 / 策略下限）。
    pub risk_source: RiskSource,
    /// 原始 annotations，原样留存以便追溯「这个等级是谁说的」。
    pub annotations: Value,
}

impl McpToolDesc {
    /// 应用覆盖表与策略下限，并记下最终等级的来源。
    fn apply_policy(&mut self, override_risk: Option<RiskClass>, floor: RiskClass) {
        if !self.has_annotations() {
            self.risk = RiskClass::Destructive;
            self.risk_source = RiskSource::Undeclared;
        }
        if let Some(risk) = override_risk {
            self.risk = risk;
            self.risk_source = RiskSource::Override;
        }
        if floor > self.risk {
            self.risk = floor;
            self.risk_source = RiskSource::Floor;
        }
    }

    /// server 是否就这个工具说了点什么（说了才有资格被称作「自述」）。
    pub fn has_annotations(&self) -> bool {
        let Some(object) = self.annotations.as_object() else {
            return false;
        };
        [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
    }

    /// 箓登记名：`mcp.<server>.<tool>`。
    pub fn id(&self) -> String {
        format!("mcp.{}.{}", self.server, self.name)
    }

    /// server 是否显式声明了只读（用于展示，不用来放宽权限）。
    pub fn declared_read_only(&self) -> bool {
        self.annotations
            .get("readOnlyHint")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// 传输无关的 MCP 调用面：stdio 子进程与回环 HTTP 都实现它。
///
/// 存在的意义是「工具适配器只写一次」——`McpToolAdapter` 只认这个 trait，
/// 换传输不动工具层，也不会出现两套略有差异的核验逻辑。
pub trait McpCaller: Send {
    fn server_name(&self) -> &str;
    fn initialize(&mut self) -> Result<Value, McpError>;
    fn list_tools(&mut self) -> Result<Vec<McpToolDesc>, McpError>;
    fn call_tool(&mut self, tool: &str, arguments_json: &str) -> Result<ToolOutput, McpError>;
}

/// 把 `tools/call` 的 `content` 块拼成纯文本，并做上限截断。
/// 外宗输出一律视为**不可信文本**，只能作为工具结果进入当轮对话。
fn render_tool_result(
    server: &str,
    tool: &str,
    result: &Value,
) -> Result<(String, bool), McpError> {
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
        return Err(McpError(format!("工具 {tool}（{server}）报错：{content}")));
    }
    let truncated = content.chars().count() > TOOL_OUTPUT_CHARS;
    if truncated {
        let cut: String = content.chars().take(TOOL_OUTPUT_CHARS).collect();
        content = format!("{cut}\n[TRUNCATED：外宗输出超过 64 KiB 上限]");
    }
    Ok((content, truncated))
}

/// 把 `tools/list` 的结果转成工具描述。
///
/// MCP 的 `annotations` 是 **server 自述**，本客户端只在「收紧」方向上采信：
/// 自述只读的工具可以免确认，但权限永远停在 `Limited`；自述不清楚、或
/// 自相矛盾（既说只读又说破坏性）一律按 `Destructive` 处理。
fn collect_tool_descs(server: &str, result: &Value) -> Vec<McpToolDesc> {
    let Some(list) = result.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?.to_owned();
            let annotations = tool.get("annotations");
            let mut desc = McpToolDesc {
                server: server.to_owned(),
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
                risk: risk_from_annotations(annotations),
                risk_source: RiskSource::Declared,
                annotations: annotations.cloned().unwrap_or(Value::Null),
            };
            // 统一走一次策略计算，避免「构造处」与「接入处」对来源的判断不一致。
            desc.apply_policy(None, RiskClass::Observe);
            Some(desc)
        })
        .collect()
}

/// 把 MCP 的 `annotations` 折成风险等级。
///
/// 规范里 `destructiveHint` 的缺省值是 true（即「没声明就按破坏性算」），
/// 这里照此执行，并且额外加一条：`readOnlyHint: true` 与 `destructiveHint: true`
/// 同时出现属自相矛盾，取更严的一方。
pub fn risk_from_annotations(annotations: Option<&Value>) -> RiskClass {
    let flag = |key: &str| {
        annotations
            .and_then(|value| value.get(key))
            .and_then(Value::as_bool)
    };
    let read_only = flag("readOnlyHint").unwrap_or(false);
    let destructive = flag("destructiveHint");
    match (read_only, destructive) {
        // 显式说「破坏性」就按破坏性算，不管别的字段怎么写。
        (_, Some(true)) => RiskClass::Destructive,
        // 显式说「只读」且没说破坏性 → 免确认（权限仍是 Limited）。
        (true, _) => RiskClass::Observe,
        // 显式说「非破坏」→ 可变更，每符确认一次。
        (false, Some(false)) => RiskClass::Mutate,
        // 什么都没说：按规范缺省（破坏性）处理。
        (false, None) => RiskClass::Destructive,
    }
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
    extract_response_value(&value, id)
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
        Ok(collect_tool_descs(&self.server, &result))
    }

    /// tools/call；输出截断到 `TOOL_OUTPUT_CHARS` 并标注，视为不可信文本。
    pub fn call_tool(&mut self, tool: &str, arguments_json: &str) -> Result<ToolOutput, McpError> {
        let arguments: Value = serde_json::from_str(arguments_json)
            .map_err(|e| McpError(format!("参数不是合法 JSON：{e}")))?;
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        let (content, truncated) = render_tool_result(&self.server, tool, &result)?;
        Ok(ToolOutput {
            name: format!("mcp.{}.{}", self.server, tool),
            content,
            truncated,
        })
    }
}

impl McpCaller for McpStdioClient {
    fn server_name(&self) -> &str {
        McpStdioClient::server_name(self)
    }

    fn initialize(&mut self) -> Result<Value, McpError> {
        McpStdioClient::initialize(self)
    }

    fn list_tools(&mut self) -> Result<Vec<McpToolDesc>, McpError> {
        McpStdioClient::list_tools(self)
    }

    fn call_tool(&mut self, tool: &str, arguments_json: &str) -> Result<ToolOutput, McpError> {
        McpStdioClient::call_tool(self, tool, arguments_json)
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// 回环 HTTP 传输（streamable-http）
// ---------------------------------------------------------------------------

/// MCP 的 streamable-http 客户端：POST JSON-RPC，响应可能是 JSON，也可能是 SSE。
///
/// 只允许明文回环地址（`http://127.0.0.1` / `localhost`）——MCP server 通常就起在
/// 本机；远程 MCP 需要 TLS 边界，是独立增量，不在这里偷偷放开。
pub struct McpHttpClient {
    server: String,
    endpoint: fulu_provider::http::Endpoint,
    path: String,
    headers: Vec<(String, String)>,
    session_id: Option<String>,
    next_id: u64,
}

impl McpHttpClient {
    pub fn new(server: &str, url: &str, headers: Vec<(String, String)>) -> Result<Self, McpError> {
        let endpoint = fulu_provider::http::Endpoint::parse(url)
            .map_err(|error| McpError(format!("server {server} 的 URL 不被接受：{error}")))?;
        let rest = url.strip_prefix("http://").unwrap_or(url);
        let path = match rest.find('/') {
            Some(index) => rest[index..].to_owned(),
            None => "/".to_owned(),
        };
        Ok(Self {
            server: server.to_owned(),
            endpoint,
            path,
            headers,
            session_id: None,
            next_id: 0,
        })
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        self.next_id += 1;
        let id = self.next_id;
        let payload = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let body =
            serde_json::to_vec(&payload).map_err(|e| McpError(format!("请求序列化失败：{e}")))?;

        // 借用期受限：先把头部组装成拥有所有权的 String，再交给请求层。
        let mut headers: Vec<(String, String)> = vec![
            (
                "Accept".to_owned(),
                "application/json, text/event-stream".to_owned(),
            ),
            (
                "MCP-Protocol-Version".to_owned(),
                PROTOCOL_VERSION.to_owned(),
            ),
        ];
        headers.extend(self.headers.iter().cloned());
        if let Some(session) = &self.session_id {
            headers.push(("Mcp-Session-Id".to_owned(), session.clone()));
        }
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let response = fulu_provider::http::send(
            &self.endpoint,
            &fulu_provider::http::HttpRequest {
                method: "POST",
                path: &self.path,
                headers: &borrowed,
                body: Some(&body),
                timeout: REQUEST_TIMEOUT,
            },
        )
        .map_err(|error| McpError(format!("{method} 失败：{error}")))?;

        if let Some(session) = response.header("mcp-session-id") {
            self.session_id = Some(session.to_owned());
        }
        response
            .require_success()
            .map_err(|error| McpError(format!("{method} 失败：{error}")))?;
        let text = response
            .body_text()
            .map_err(|error| McpError(error.to_string()))?;
        extract_from_payload(&text, id)?
            .ok_or_else(|| McpError(format!("{method} 的响应里没有 id={id} 的结果")))
    }

    /// 通知类消息（无 id）发出去即可，失败不致命。
    fn notify(&mut self, method: &str) {
        let payload = json!({"jsonrpc": "2.0", "method": method});
        let Ok(body) = serde_json::to_vec(&payload) else {
            return;
        };
        let mut headers: Vec<(String, String)> = vec![(
            "Accept".to_owned(),
            "application/json, text/event-stream".to_owned(),
        )];
        headers.extend(self.headers.iter().cloned());
        if let Some(session) = &self.session_id {
            headers.push(("Mcp-Session-Id".to_owned(), session.clone()));
        }
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let _ = fulu_provider::http::send(
            &self.endpoint,
            &fulu_provider::http::HttpRequest {
                method: "POST",
                path: &self.path,
                headers: &borrowed,
                body: Some(&body),
                timeout: REQUEST_TIMEOUT,
            },
        );
    }
}

/// 兼容三种响应形态：单个 JSON、SSE 事件流、以及只有通知没有结果的空响应。
fn extract_from_payload(text: &str, id: u64) -> Result<Option<Value>, McpError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        // 整体是一个 JSON 文档（单条响应或批量响应）。
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return extract_response_value(&value, id);
        }
    }
    for line in trimmed.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(found) = extract_response_value(&value, id)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn extract_response_value(value: &Value, id: u64) -> Result<Option<Value>, McpError> {
    if let Some(batch) = value.as_array() {
        for item in batch {
            if item.get("id").and_then(Value::as_u64) == Some(id)
                && let Some(found) = extract_response_value(item, id)?
            {
                return Ok(Some(found));
            }
        }
        return Ok(None);
    }
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return Ok(None);
    }
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知");
        let code = error.get("code").map(|c| c.to_string()).unwrap_or_default();
        return Err(McpError(format!("server 返回错误 {code}：{message}")));
    }
    Ok(Some(value.get("result").cloned().unwrap_or(Value::Null)))
}

impl McpCaller for McpHttpClient {
    fn server_name(&self) -> &str {
        &self.server
    }

    fn initialize(&mut self) -> Result<Value, McpError> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "fulu", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        self.notify("notifications/initialized");
        Ok(result)
    }

    fn list_tools(&mut self) -> Result<Vec<McpToolDesc>, McpError> {
        let result = self.request("tools/list", json!({}))?;
        Ok(collect_tool_descs(&self.server, &result))
    }

    fn call_tool(&mut self, tool: &str, arguments_json: &str) -> Result<ToolOutput, McpError> {
        let arguments: Value = serde_json::from_str(arguments_json)
            .map_err(|e| McpError(format!("参数不是合法 JSON：{e}")))?;
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        let (content, truncated) = render_tool_result(&self.server, tool, &result)?;
        Ok(ToolOutput {
            name: format!("mcp.{}.{}", self.server, tool),
            content,
            truncated,
        })
    }
}

// ---------------------------------------------------------------------------
// 工具适配器：归一为 fulu-tools::Tool，走同一套箓核验
// ---------------------------------------------------------------------------

/// 一台已握手 server 的工具句柄。注册进 `ToolRegistry` 后与内置工具完全同权同责。
pub struct McpToolAdapter {
    client: Arc<Mutex<dyn McpCaller>>,
    tool: McpToolDesc,
    id: String,
}

impl McpToolAdapter {
    pub fn new(client: impl McpCaller + 'static, tool: McpToolDesc) -> Self {
        Self::shared(Arc::from(Mutex::from(client)), tool)
    }

    /// 多个工具共用一台 server 的同一个调用端（子进程或 HTTP 会话）。
    ///
    /// 一台 server 通常暴露十几个工具；若每个工具各起一份，进程数/会话数与
    /// 握手次数会随工具数线性膨胀，且同一 server 的状态会被切成多份。
    pub fn shared(client: Arc<Mutex<dyn McpCaller>>, tool: McpToolDesc) -> Self {
        let id = format!("mcp.{}.{}", tool.server, tool.name);
        Self { client, tool, id }
    }

    /// 外宗工具在箓层的登记名（`mcp.<server>.<tool>`）。
    /// 与 `Tool::id` 同值，但这里不需要引入 `fulu-tools::Tool` 就能取到。
    pub fn tool_id(&self) -> &str {
        &self.id
    }

    pub fn description(&self) -> &str {
        &self.tool.description
    }

    /// 最终生效的副作用等级（已并入覆盖表与策略下限）。
    pub fn risk(&self) -> RiskClass {
        self.tool.risk
    }

    /// 该等级的来源（自述 / 覆盖表 / 未声明 / 策略下限）。
    pub fn risk_source(&self) -> RiskSource {
        self.tool.risk_source
    }

    /// server 是否显式声明只读——展示用，绝不用于放宽权限。
    pub fn declared_read_only(&self) -> bool {
        self.tool.declared_read_only()
    }

    /// server 声明的参数 schema（可能为空对象，调用方需容错）。
    pub fn schema_json(&self) -> &str {
        &self.tool.schema_json
    }
}

/// 一台 server 的握手结果：一个子进程 + 它暴露的全部工具。
pub struct McpSession {
    pub server: String,
    /// `initialize` 的原始回执（含 protocolVersion / serverInfo / capabilities）
    pub handshake: Value,
    pub tools: Vec<Arc<McpToolAdapter>>,
    /// 不阻断连接、但用户应当知道的实情（例如「该 server 声明了环境变量」）
    pub warnings: Vec<String>,
}

/// 握手并收集工具。**唯一**的接入入口：stdio 一个 server 一个进程，HTTP 一个 server 一个会话。
pub fn connect(info: &McpServerInfo) -> Result<McpSession, McpError> {
    connect_with(info, &RiskOverrides::default(), RiskClass::Observe)
}

/// 带风险策略的接入：`overrides` 是用户自定的逐工具等级，`floor` 是策略下限。
///
/// 两者的作用方向相反且都只向上：覆盖表可以改等级（也可能下调，因为那是
/// 用户自己的判断），下限只能抬高。**权限不受二者影响**——外宗工具永远是
/// `Limited`，这两样只决定「召将前要不要人点头」。
pub fn connect_with(
    info: &McpServerInfo,
    overrides: &RiskOverrides,
    floor: RiskClass,
) -> Result<McpSession, McpError> {
    let mut warnings = Vec::new();
    let shared: Arc<Mutex<dyn McpCaller>> = match &info.transport {
        Transport::Stdio { command, args } => Arc::new(Mutex::new(McpStdioClient::spawn(
            &info.name,
            command,
            args,
            &BTreeMap::new(),
        )?)),
        Transport::StreamableHttp { url, headers } => {
            if headers.is_empty() {
                warnings.push(format!(
                    "server「{}」未声明请求头；若对端要求认证，握手会以 401 失败",
                    info.name
                ));
            }
            Arc::new(Mutex::new(McpHttpClient::new(
                &info.name,
                url,
                headers.clone(),
            )?))
        }
    };

    let (handshake, mut descs) = {
        let mut caller = shared
            .lock()
            .map_err(|_| McpError("外宗客户端锁损坏".into()))?;
        let handshake = caller.initialize()?;
        let descs = caller.list_tools()?;
        (handshake, descs)
    };
    descs.sort_by(|a, b| a.name.cmp(&b.name));
    for desc in &mut descs {
        desc.apply_policy(overrides.get(&desc.id()), floor);
    }

    if !info.env_keys.is_empty() {
        warnings.push(format!(
            "server「{}」声明了环境变量 {}；fulu 不读取配置文件中的密钥值，\
             需在启动 fulu 前自行 export，否则相关工具可能失败",
            info.name,
            info.env_keys.join(", ")
        ));
    }
    if descs.is_empty() {
        warnings.push(format!("server「{}」未暴露任何工具", info.name));
    }

    let tools = descs
        .into_iter()
        .map(|desc| Arc::new(McpToolAdapter::shared(shared.clone(), desc)))
        .collect();
    Ok(McpSession {
        server: info.name.clone(),
        handshake,
        tools,
        warnings,
    })
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
    use fulu_tools::{Tool, ToolArguments};

    /// 一台最小的 MCP stdio server：list 两个工具，call 时回显大写 / 故意报错。
    const FAKE_SERVER_SCRIPT: &str = r#"
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
            {"name": "shout", "description": "回显大写", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
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

    /// 落地 mock 脚本并找到可用的 python 解释器；没有解释器时返回 None（跳过而非失败）。
    fn fake_server_fixture(name: &str) -> Option<(String, String)> {
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
            .map(|p| (*p).to_string());
        let interpreter = interpreter?;
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("fake_mcp_server.py");
        std::fs::write(&script_path, FAKE_SERVER_SCRIPT).unwrap();
        Some((interpreter, script_path.to_string_lossy().into_owned()))
    }

    fn server_info(name: &str, command: &str) -> McpServerInfo {
        McpServerInfo {
            name: name.into(),
            transport: Transport::Stdio {
                command: command.into(),
                args: vec!["--stdio".into()],
            },
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
                transport: Transport::Stdio {
                    command: "node".into(),
                    args: vec!["weather.js".into()],
                },
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
        let Some((interpreter, script)) = fake_server_fixture("fulu_mcp_test_stdio") else {
            eprintln!("跳过：本机无 python 解释器");
            return;
        };
        let mut client =
            McpStdioClient::spawn("fake", &interpreter, &[script], &BTreeMap::new()).unwrap();
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

    /// connect：一台 server 只起**一个**子进程，全部工具共享同一个 client；
    /// 参数 schema 原样带出，供箓层下发给模型。
    #[test]
    fn connect_shares_one_process_and_carries_real_schema() {
        let Some((interpreter, script)) = fake_server_fixture("fulu_mcp_test_connect") else {
            eprintln!("跳过：本机无 python 解释器");
            return;
        };
        let info = McpServerInfo {
            name: "fake".into(),
            transport: Transport::Stdio {
                command: interpreter,
                args: vec![script],
            },
            env_keys: vec!["API_KEY".into()],
            config_path: PathBuf::from("config.json"),
            notes: Vec::new(),
        };
        let session = connect(&info).unwrap();
        assert_eq!(session.server, "fake");
        assert_eq!(session.tools.len(), 2);
        assert!(session.handshake.is_object());
        assert_eq!(session.tools[0].tool_id(), "mcp.fake.boom");
        assert_eq!(session.tools[1].tool_id(), "mcp.fake.shout");
        assert!(
            session.warnings.iter().any(|w| w.contains("API_KEY")),
            "声明了环境变量却没提示：{:?}",
            session.warnings
        );
        // schema 必须是 server 声明的原文，而不是空壳。
        let shout = &session.tools[1];
        assert!(shout.schema_json().contains(r#""required":["text"]"#));

        // 共享验证：先失败调用，再从另一工具成功调用，进程仍然可用。
        let policy = ToolPolicy::default();
        assert!(
            shout
                .invoke(
                    &ToolArguments {
                        json: r#"{"text":"云篆"}"#.into()
                    },
                    &policy
                )
                .unwrap()
                .content
                == "云篆"
        );
        assert!(
            session.tools[0]
                .invoke(&ToolArguments::empty(), &policy)
                .is_err()
        );
        assert_eq!(
            shout
                .invoke(
                    &ToolArguments {
                        json: r#"{"text":"归一"}"#.into()
                    },
                    &policy
                )
                .unwrap()
                .content,
            "归一"
        );
    }

    #[test]
    fn adapter_normalizes_into_fulu_tool_output() {
        // 用内存态验证 id 命名与错误映射：不 spawn，直接构造工具描述。
        let tool = McpToolDesc {
            server: "fake".into(),
            name: "shout".into(),
            description: String::new(),
            schema_json: "{}".into(),
            risk: RiskClass::Destructive,
            risk_source: RiskSource::Undeclared,
            annotations: Value::Null,
        };
        let adapter_id = format!("mcp.{}.{}", tool.server, tool.name);
        assert_eq!(adapter_id, "mcp.fake.shout");
        // 未声明注解的工具按最严处理，且 `id()` 与适配器命名保持一致。
        assert_eq!(tool.id(), adapter_id);
        assert_eq!(tool.risk, RiskClass::Destructive);
        assert!(!tool.declared_read_only());
    }

    #[test]
    fn risk_overrides_load_valid_entries_and_report_the_rest() {
        let dir = std::env::temp_dir().join("fulu_mcp_risk_override");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("outer-sect-risk.json");
        std::fs::write(
            &path,
            r#"{"tools": {
                "mcp.cucumber.get_dashboard": "observe",
                "mcp.cucumber.start_process": "mutate",
                "mcp.cucumber.bogus_level": "harmless",
                "mcp.cucumber.numeric": 3
            }}"#,
        )
        .unwrap();
        let (overrides, issues) = RiskOverrides::load(std::slice::from_ref(&path));
        assert_eq!(overrides.len(), 2, "坏条目必须被丢弃而不是猜测");
        assert_eq!(
            overrides.get("mcp.cucumber.get_dashboard"),
            Some(RiskClass::Observe)
        );
        assert_eq!(
            overrides.get("mcp.cucumber.start_process"),
            Some(RiskClass::Mutate)
        );
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert!(issues.iter().any(|issue| issue.contains("harmless")));

        // 指向已不存在工具的条目要单独点出来，避免用户以为覆盖生效了。
        let stale = overrides.stale_entries(&["mcp.cucumber.get_dashboard".to_owned()]);
        assert_eq!(stale, vec!["mcp.cucumber.start_process".to_owned()]);

        // 缺文件是常态，不该报错。
        let (empty, issues) = RiskOverrides::load(&[dir.join("nope.json")]);
        assert!(empty.is_empty());
        assert!(issues.is_empty());
    }

    /// T05：写入入口只改 fulu 自己的覆盖表，且写入后立刻可读回。
    #[test]
    fn risk_override_write_set_unset_roundtrip() {
        let dir = std::env::temp_dir().join("fulu_mcp_risk_write");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("outer-sect-risk.json");

        let (after_set, _) = set_risk_override(
            &path,
            "mcp.cucumber.get_dashboard",
            Some(RiskClass::Observe),
        )
        .unwrap();
        assert_eq!(after_set.len(), 1);
        let (reloaded, issues) = RiskOverrides::load(std::slice::from_ref(&path));
        assert!(
            issues.is_empty(),
            "写出的 JSON 必须能被自己读回：{issues:?}"
        );
        assert_eq!(
            reloaded.get("mcp.cucumber.get_dashboard"),
            Some(RiskClass::Observe)
        );

        // 第二条写入不得抹掉第一条。
        set_risk_override(&path, "mcp.cucumber.start_process", Some(RiskClass::Mutate)).unwrap();
        let (two, _) = RiskOverrides::load(std::slice::from_ref(&path));
        assert_eq!(two.len(), 2);

        let (after_unset, _) =
            set_risk_override(&path, "mcp.cucumber.get_dashboard", None).unwrap();
        assert_eq!(after_unset.len(), 1);
        assert_eq!(after_unset.get("mcp.cucumber.get_dashboard"), None);
        assert!(!path.with_extension("json.tmp").exists(), "临时文件要清掉");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 模糊匹配是安全红线：宁可拒绝，也不猜一个可能放行破坏性操作的工具名。
    #[test]
    fn risk_override_rejects_wildcards_and_blank_names() {
        let path = std::env::temp_dir().join("fulu_mcp_risk_reject.json");
        for bad in ["", "   ", "mcp.cucumber.*", "mcp.?", "has space"] {
            let error = set_risk_override(&path, bad, Some(RiskClass::Observe)).unwrap_err();
            assert!(error.contains("精确全名"), "{bad:?} -> {error}");
        }
        assert!(!path.exists(), "被拒的写入绝不能落盘");
    }

    #[test]
    fn parse_risk_is_strict() {
        assert_eq!(parse_risk("OBSERVE"), Some(RiskClass::Observe));
        assert_eq!(parse_risk("mutate"), Some(RiskClass::Mutate));
        assert_eq!(parse_risk("destructive"), Some(RiskClass::Destructive));
        assert_eq!(parse_risk("harmless"), None);
        assert_eq!(risk_key(RiskClass::Destructive), "destructive");
    }

    #[test]
    fn policy_applies_override_and_floor_without_touching_permission() {
        let annotated = json!({"tools": [{
            "name": "read",
            "description": "",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true, "destructiveHint": false}
        }, {
            "name": "silent",
            "description": "",
            "inputSchema": {"type": "object"}
        }]});
        let mut descs = collect_tool_descs("s", &annotated);
        let silent = descs.pop().unwrap();
        assert_eq!(silent.risk, RiskClass::Destructive);
        assert_eq!(silent.risk_source, RiskSource::Undeclared);

        // 覆盖表可以把「server 没说」的工具降为只读，并标明等级来源。
        let mut overrides = RiskOverrides::default();
        overrides
            .map
            .insert("mcp.s.silent".into(), RiskClass::Observe);
        let mut silent = silent;
        silent.apply_policy(overrides.get("mcp.s.silent"), RiskClass::Observe);
        assert_eq!(silent.risk, RiskClass::Observe);
        assert_eq!(silent.risk_source, RiskSource::Override);

        // 策略下限只能往上抬，不能把覆盖表里的破坏性降下来。
        let mut read = descs.pop().unwrap();
        read.apply_policy(None, RiskClass::Destructive);
        assert_eq!(read.risk, RiskClass::Destructive);
        assert_eq!(read.risk_source, RiskSource::Floor);
    }

    #[test]
    fn annotations_map_to_risk_without_ever_loosening_permission() {
        // 显式只读：免确认（权限层仍是 Limited，与这里无关）。
        assert_eq!(
            risk_from_annotations(Some(
                &json!({"readOnlyHint": true, "destructiveHint": false})
            )),
            RiskClass::Observe
        );
        // 只读但没写 destructiveHint：仍按只读算（缺省不是「矛盾」）。
        assert_eq!(
            risk_from_annotations(Some(&json!({"readOnlyHint": true}))),
            RiskClass::Observe
        );
        // 显式非破坏：可变更，每符确认一次。
        assert_eq!(
            risk_from_annotations(Some(&json!({"destructiveHint": false}))),
            RiskClass::Mutate
        );
        // 显式破坏：最严。
        assert_eq!(
            risk_from_annotations(Some(&json!({"destructiveHint": true}))),
            RiskClass::Destructive
        );
        // 自相矛盾：取更严的一方，不给「只读」当挡箭牌。
        assert_eq!(
            risk_from_annotations(Some(
                &json!({"readOnlyHint": true, "destructiveHint": true})
            )),
            RiskClass::Destructive
        );
        // 什么都没写：规范缺省是破坏性。
        assert_eq!(risk_from_annotations(None), RiskClass::Destructive);
        assert_eq!(
            risk_from_annotations(Some(&json!({}))),
            RiskClass::Destructive
        );
    }

    #[test]
    fn tool_list_annotations_reach_the_description() {
        let result = json!({"tools": [{
            "name": "check_process",
            "description": "查进程状态",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true, "destructiveHint": false}
        }]});
        let descs = collect_tool_descs("cucumber", &result);
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].id(), "mcp.cucumber.check_process");
        assert_eq!(descs[0].risk, RiskClass::Observe);
        assert!(descs[0].declared_read_only());
    }

    #[test]
    fn default_paths_never_write() {
        for path in default_config_paths() {
            assert!(!path.exists() || path.is_file(), "{path:?} 应只读不建");
        }
    }

    /// 一台最小的 streamable-http MCP server：POST /mcp，JSON-RPC 应答。
    ///
    /// 除 initialize 外，缺 `Authorization` 一律 401——这样「认证头是否真的带上」
    /// 由服务端强制验证，而不是靠客户端自证。
    fn spawn_http_mcp(requests: Arc<Mutex<Vec<String>>>) -> (u16, std::thread::JoinHandle<()>) {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let raw = read_http_request(&mut stream);
                let text = String::from_utf8_lossy(&raw).into_owned();
                let head = text.to_ascii_lowercase();
                let body = text
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body.to_owned())
                    .unwrap_or_default();
                let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                requests
                    .lock()
                    .unwrap()
                    .push(format!("{method} {}", head.lines().next().unwrap_or("")));

                if method == "notifications/initialized" {
                    let _ = stream.write_all(
                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                if !head.contains("authorization: bearer t-123") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                let result = match method.as_str() {
                    "initialize" => json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fake-http", "version": "0.0.1"}
                    }),
                    "tools/list" => json!({"tools": [{
                        "name": "shout",
                        "description": "回显大写",
                        "inputSchema": {"type": "object",
                                        "properties": {"text": {"type": "string"}},
                                        "required": ["text"]}
                    }]}),
                    "tools/call" => json!({
                        "isError": false,
                        "content": [{"type": "text", "text": "HTTP 外宗回执"}]
                    }),
                    _ => json!({}),
                };
                let payload = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
                let raw = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: s-42\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(raw.as_bytes());
            }
        });
        (port, handle)
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        use std::io::Read;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
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
        buffer
    }

    #[test]
    fn http_transport_handshakes_lists_and_calls_with_auth_header() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (port, handle) = spawn_http_mcp(seen.clone());
        let info = McpServerInfo {
            name: "fake-http".into(),
            transport: Transport::StreamableHttp {
                url: format!("http://127.0.0.1:{port}/mcp"),
                headers: vec![("Authorization".into(), "Bearer t-123".into())],
            },
            env_keys: Vec::new(),
            config_path: PathBuf::from("<test>"),
            notes: Vec::new(),
        };
        let session = connect(&info).unwrap();
        assert_eq!(session.tools.len(), 1);
        assert_eq!(session.tools[0].tool_id(), "mcp.fake-http.shout");
        assert!(session.warnings.is_empty(), "{:?}", session.warnings);

        let output = session.tools[0]
            .invoke(&ToolArguments::empty(), &ToolPolicy::default())
            .unwrap();
        assert_eq!(output.content, "HTTP 外宗回执");
        assert_eq!(output.name, "mcp.fake-http.shout");

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            4,
            "initialize / initialized / tools/list / tools/call：{seen:?}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn http_transport_refuses_non_loopback_urls() {
        let remote = McpHttpClient::new("remote", "http://example.com/mcp", Vec::new());
        let Err(McpError(message)) = remote else {
            panic!("非回环地址必须被拒绝");
        };
        assert!(
            message.contains("回环") || message.contains("主机"),
            "{message}"
        );

        let tls = McpHttpClient::new("tls", "https://127.0.0.1:8443/mcp", Vec::new());
        let Err(McpError(message)) = tls else {
            panic!("HTTPS 必须被拒绝（需要 TLS 边界，属独立增量）");
        };
        assert!(message.contains("HTTP"), "{message}");
    }

    #[test]
    fn discovery_reads_http_servers_and_masks_header_values() {
        let dir = std::env::temp_dir().join("fulu_mcp_test_http_discovery");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"remote":{"type":"streamable-http","url":"http://127.0.0.1:64829/mcp","headers":{"Authorization":"Bearer sk-should-never-leak"}}}}"#,
        )
        .unwrap();
        let (servers, issues) = discover(std::slice::from_ref(&path));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].transport.label(),
            "http: http://127.0.0.1:64829/mcp（1 个请求头）"
        );
        // 值可以被读进内存（否则连不上认证型 server），但绝不出现在 Debug 里。
        let debug = format!("{:?}", servers[0]);
        assert!(!debug.contains("sk-should-never-leak"), "{debug}");
        assert!(debug.contains("<masked>"));
    }

    #[test]
    fn unused_server_info_helper_stays_in_sync() {
        // 防止未来改动 discovery 字段后测试夹具失同步。
        let info = server_info("weather", "node");
        assert_eq!(info.name, "weather");
    }
}
