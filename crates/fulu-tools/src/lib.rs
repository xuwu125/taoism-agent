//! 风火轮（Tools）：工具 trait 与最小权限执行（docs/02 第 3.4、5 节）。
//!
//! 工具**不是**模型想调就能调：每次执行前都要过「箓」给出的 `Authorization`
//! 白名单与配额，再由工具自身做路径/域名边界检查。模型返回的 `ToolCall`
//! 只是「请求」，不是「授权」。
//!
//! M2 只放行只读工具；写文件、子进程、网络访问等能力在名册显式登记后才开放。

use fulu_register::{Authorization, RegisterError};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub const MAX_READ_BYTES: u64 = 64 * 1024;
/// 单次写入上限（与 8.10 预算同源，防模型一次性廊作大文件）。
pub const MAX_WRITE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolArguments {
    pub json: String,
}

impl ToolArguments {
    pub fn empty() -> Self {
        Self { json: "{}".into() }
    }

    pub fn field(&self, key: &str) -> Option<String> {
        let value: Value = serde_json::from_str(&self.json).ok()?;
        match value.get(key)? {
            Value::String(text) => Some(text.clone()),
            other => Some(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub name: String,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    NotAllowed(String),
    QuotaExceeded(String),
    InvalidArguments(String),
    PathForbidden(String),
    RootNotConfigured,
    Io(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAllowed(msg) => write!(f, "风火轮越权：{msg}"),
            Self::QuotaExceeded(msg) => write!(f, "风火轮配额耗尽：{msg}"),
            Self::InvalidArguments(msg) => write!(f, "工具参数不合法：{msg}"),
            Self::PathForbidden(msg) => write!(f, "路径越界：{msg}"),
            Self::RootNotConfigured => {
                write!(f, "文件工具未配置工作区根目录，拒绝读取任意路径")
            }
            Self::Io(msg) => write!(f, "工具 I/O 失败：{msg}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// 工具可见的运行时边界。由法坛构造，工具本身不能放宽它。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolPolicy {
    /// 文件工具（fs.read / fs.write）的唯一根目录；None 表示文件能力关闭。
    pub root: Option<PathBuf>,
    pub allowed_domains: Vec<String>,
}

/// 工作区内相对路径的统一校验：拒绝绝对路径与 `..`/根分量穿越。
/// fs.read 与 fs.write 共用，避免两处口径漂移。
fn resolve_workspace_path(root: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let relative = Path::new(raw);
    if relative.is_absolute() {
        return Err(ToolError::PathForbidden(format!("不允许绝对路径：{raw}")));
    }
    if relative
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::RootDir))
    {
        return Err(ToolError::PathForbidden(format!("不允许目录穿越：{raw}")));
    }
    Ok(root.join(relative))
}

pub trait Tool: Send + Sync {
    fn id(&self) -> &str;
    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError>;
}

/// 无副作用工具：回显参数，用于验证工具循环确实跑通。
pub struct EchoTool;

impl Tool for EchoTool {
    fn id(&self) -> &str {
        "echo"
    }

    fn invoke(&self, args: &ToolArguments, _policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        let text = args.field("text").unwrap_or_else(|| args.json.clone());
        Ok(ToolOutput {
            name: self.id().into(),
            content: text,
            truncated: false,
        })
    }
}

/// 只读文件工具：拒绝绝对路径、`..` 穿越，且限制单次读取字节数。
pub struct FsReadTool;

impl Tool for FsReadTool {
    fn id(&self) -> &str {
        "fs.read"
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        let root = policy.root.as_ref().ok_or(ToolError::RootNotConfigured)?;
        let raw = args
            .field("path")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 path".into()))?;
        let full = resolve_workspace_path(root, &raw)?;
        let file = std::fs::File::open(&full)
            .map_err(|e| ToolError::Io(format!("{}：{e}", full.display())))?;
        let mut buffer = Vec::new();
        let read = file
            .take(MAX_READ_BYTES + 1)
            .read_to_end(&mut buffer)
            .map_err(|e| ToolError::Io(e.to_string()))?;
        let truncated = read as u64 > MAX_READ_BYTES;
        if truncated {
            buffer.truncate(MAX_READ_BYTES as usize);
        }
        Ok(ToolOutput {
            name: self.id().into(),
            content: String::from_utf8_lossy(&buffer).into_owned(),
            truncated,
        })
    }
}

/// 写文件工具（T17）：把敕行从「只读 + 命令行」扩到「受控写入」。
///
/// 风险定级为 `Mutate`（每符确认一次）而非 `Destructive`：能力严格弱于
/// 敕行（不能执行、不能删改工作区外、单次 256 KiB 上限），且覆盖已有
/// 文件前**强制自动备份**（`<path>.fulu-bak`，重名依次 `.fulu-bak.2`…），
/// 可恢复、可审计。权限档位为受限级：只读符申请它会被双向核验驳回。
///
/// 边界：与 fs.read 共用路径校验（拒绝对/穿越）；不允许写绝对路径；
/// 缺工作区根目录一律拒绝；`append=true` 时追加而非覆盖。
pub struct FsWriteTool;

impl Tool for FsWriteTool {
    fn id(&self) -> &str {
        "fs.write"
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        use std::io::Write;
        let root = policy.root.as_ref().ok_or(ToolError::RootNotConfigured)?;
        let raw = args
            .field("path")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 path".into()))?;
        let full = resolve_workspace_path(root, &raw)?;
        let content = args
            .field("content")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 content".into()))?;
        let bytes = content.as_bytes();
        if bytes.len() > MAX_WRITE_BYTES {
            return Err(ToolError::InvalidArguments(format!(
                "写入内容 {} 字节，超过单次 {} 字节上限",
                bytes.len(),
                MAX_WRITE_BYTES
            )));
        }
        let append = args.field("append").map(|v| v == "true").unwrap_or(false);

        // 覆盖已有文件前先备份（备份失败即拒绝写入，不静默丢历史）；
        // 追加不破坏原文件，无需备份。
        let mut backup_note = String::new();
        if !append && full.is_file() {
            let backup = backup_path_for(&full);
            std::fs::copy(&full, &backup)
                .map_err(|e| ToolError::Io(format!("备份 {} 失败：{e}", backup.display())))?;
            backup_note = format!("\n[已备份原文件至 {}]", backup.display());
        }
        if let Some(parent) = full.parent()
            && parent != root
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::Io(format!("建父目录失败：{e}")))?;
        }
        if append {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&full)
                .map_err(|e| ToolError::Io(format!("{}：{e}", full.display())))?;
            file.write_all(bytes)
                .map_err(|e| ToolError::Io(e.to_string()))?;
        } else {
            std::fs::write(&full, bytes)
                .map_err(|e| ToolError::Io(format!("{}：{e}", full.display())))?;
        }
        Ok(ToolOutput {
            name: self.id().into(),
            content: format!(
                "已{} {}（{} 字节）{}",
                if append { "追加" } else { "写入" },
                full.display(),
                bytes.len(),
                backup_note
            ),
            truncated: false,
        })
    }
}

/// 备份路径：`<target>.fulu-bak`，重名依次 `.fulu-bak.2`、`.fulu-bak.3`…
/// 暴露为纯函数便于测试；只在工作区内打转，绝不指向外部。
fn backup_path_for(target: &Path) -> PathBuf {
    let mut candidate = target.as_os_str().to_owned();
    candidate.push(".fulu-bak");
    let mut n = 1;
    let mut path = PathBuf::from(candidate);
    while path.exists() {
        n += 1;
        path = PathBuf::from(format!("{}.fulu-bak.{n}", target.display()));
    }
    path
}

/// 敕行（ShellTool）：分 OS 命令行执行（docs/02 第 8.7 节，M2.5 基础版）。
/// 边界（防御纵深，真正的闸门始终是箓 + 确认）：
/// - **必须配置工作区根目录**：命令的 cwd 被锁定在根目录内；
/// - 分 OS 调用：Windows = PowerShell（-NoProfile -NonInteractive），macOS = zsh，
///   Linux = sh；
/// - 危险命令模式黑名单（非穷举，防呆不防恶）；
/// - 单次超时上限 60 s，超时即 kill；输出单流上限 64 KiB，超出截断并标注。
pub struct ShellTool;

/// 超时上下限（毫秒）。
pub const SHELL_TIMEOUT_MIN_MS: u64 = 100;
pub const SHELL_TIMEOUT_MAX_MS: u64 = 60_000;
/// 单流输出上限。
pub const SHELL_OUTPUT_BYTES: usize = 64 * 1024;

/// 危险命令模式（小写、非穷举）。
pub const SHELL_BLOCKLIST: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "del /f /s /q",
    "rd /s /q",
    "format ",
    "mkfs",
    "diskpart",
    "cipher /w",
    "dd if=",
    "shutdown",
    "reg delete",
    ":(){",
];

/// 平台对应的解释器；暴露为纯函数便于测试。
pub fn shell_command_for(command: &str) -> (&'static str, Vec<String>) {
    match std::env::consts::OS {
        "windows" => (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                command.to_owned(),
            ],
        ),
        "macos" => ("zsh", vec!["-c".into(), command.to_owned()]),
        _ => ("sh", vec!["-c".into(), command.to_owned()]),
    }
}

impl ShellTool {
    fn blocklisted(&self, command: &str) -> Option<&'static str> {
        let lowered = command.to_lowercase();
        SHELL_BLOCKLIST
            .iter()
            .find(|pattern| lowered.contains(*pattern))
            .copied()
    }
}

impl Tool for ShellTool {
    fn id(&self) -> &str {
        "shell.exec"
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let root = policy.root.as_ref().ok_or(ToolError::RootNotConfigured)?;
        let command = args
            .field("command")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 command".into()))?;
        if let Some(pattern) = self.blocklisted(&command) {
            return Err(ToolError::NotAllowed(format!(
                "命中危险命令模式「{pattern}」，敕行拒绝"
            )));
        }
        let timeout_ms = args
            .field("timeout_ms")
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(10_000)
            .clamp(SHELL_TIMEOUT_MIN_MS, SHELL_TIMEOUT_MAX_MS);

        let (program, interpreter_args) = shell_command_for(&command);
        let mut cmd = Command::new(program);
        cmd.args(&interpreter_args)
            .current_dir(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Io(format!("敕行启动失败：{e}")))?;

        // 单流各 64 KiB：take(cap+1) 读取后截断标注。
        let stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::Io("敕行缺少 stdout".into()))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| ToolError::Io("敕行缺少 stderr".into()))?;
        // 单流各 64 KiB：take(cap+1) 读取后截断标注。stdout/stderr 是不同类型，
        // 用泛型辅助函数统一处理。
        fn spawn_reader<R: std::io::Read + Send + 'static>(
            pipe: R,
        ) -> std::thread::JoinHandle<(usize, Vec<u8>)> {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                pipe.take(SHELL_OUTPUT_BYTES as u64 + 1)
                    .read_to_end(&mut buffer)
                    .map(|n| (n, buffer))
                    .unwrap_or((0, Vec::new()))
            })
        }
        let stdout_handle = spawn_reader(stdout_pipe);
        let stderr_handle = spawn_reader(stderr_pipe);

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut timed_out = false;
        loop {
            match child.try_wait().map_err(|e| ToolError::Io(e.to_string()))? {
                Some(_) => break,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }

        let (stdout_read, stdout_bytes) = stdout_handle
            .join()
            .map_err(|_| ToolError::Io("stdout 线程异常".into()))?;
        let (stderr_read, stderr_bytes) = stderr_handle
            .join()
            .map_err(|_| ToolError::Io("stderr 线程异常".into()))?;
        let mut content = String::from_utf8_lossy(&stdout_bytes).into_owned();
        let stderr_text = String::from_utf8_lossy(&stderr_bytes).into_owned();
        if !stderr_text.trim().is_empty() {
            content.push_str("\n[stderr] ");
            content.push_str(stderr_text.trim());
        }
        let truncated = stdout_read > SHELL_OUTPUT_BYTES || stderr_read > SHELL_OUTPUT_BYTES;
        if timed_out {
            content.push_str(&format!("\n[敕行超时：超过 {timeout_ms}ms，已终止]"));
        }
        Ok(ToolOutput {
            name: self.id().into(),
            content,
            truncated,
        })
    }
}

/// 问天（T15）：搜索能力统一入口，后端可插拔。
///
/// 设计：本工具**不自带搜索引擎**（那会引入 TLS/网络依赖），只做
/// 「归一化契约 → 后端路由」：优先把已发现/已接入的搜索类外宗
/// MCP 工具作为后端（不写任何全局配置），没有后端时**明确报错**，
/// 绝不伪装成功。后端顺序即优先级；某个后端失败自动尝试下一个，
/// 全部失败时把各自原因一并报出。
pub struct AskSearchTool {
    backends: Vec<Arc<dyn SearchBackend>>,
}

/// 问天后端抽象：任何能「query → 文本结果」的来源都实现它。
pub trait SearchBackend: Send + Sync {
    /// 后端标识（如 `mcp.<server>.<tool>`），随结果一起返回以便审计。
    fn id(&self) -> &str;
    fn search(&self, query: &str, limit: usize) -> Result<String, ToolError>;
}

/// 问天单次查询条数上下限。
pub const ASK_SEARCH_LIMIT_MIN: usize = 1;
pub const ASK_SEARCH_LIMIT_MAX: usize = 20;
pub const ASK_SEARCH_LIMIT_DEFAULT: usize = 5;

impl AskSearchTool {
    pub fn new(backends: Vec<Arc<dyn SearchBackend>>) -> Self {
        Self { backends }
    }
}

impl Tool for AskSearchTool {
    fn id(&self) -> &str {
        "ask.search"
    }

    fn invoke(&self, args: &ToolArguments, _policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        let query = args
            .field("query")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 query".into()))?;
        if query.trim().is_empty() {
            return Err(ToolError::InvalidArguments("query 不能为空".into()));
        }
        let limit = args
            .field("limit")
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(ASK_SEARCH_LIMIT_DEFAULT)
            .clamp(ASK_SEARCH_LIMIT_MIN, ASK_SEARCH_LIMIT_MAX);
        if self.backends.is_empty() {
            return Err(ToolError::NotAllowed(
                "问天未配置任何搜索后端：请先接入搜索类外宗 MCP server（fulu mcp list）或显式绑定后端，本端绝不静默联网".into(),
            ));
        }
        let mut failures = Vec::new();
        for backend in &self.backends {
            match backend.search(&query, limit) {
                Ok(content) => {
                    return Ok(ToolOutput {
                        name: self.id().into(),
                        content: format!("[问天 via {}]\n{content}", backend.id()),
                        truncated: false,
                    });
                }
                Err(error) => failures.push(format!("{}：{error}", backend.id())),
            }
        }
        Err(ToolError::Io(format!(
            "问天全部 {} 个后端失败：{}",
            self.backends.len(),
            failures.join("；")
        )))
    }
}

/// moli 浏览器（T14）：无头网页浏览，**不嵌 WebView、进程内零 TLS**。
///
/// 实现：调外部 Edge/Chrome 的无头命令行（`--headless=new --dump-dom`），
/// 网络与渲染、TLS 全部发生在外部浏览器进程里，本 crate 只解析它的
/// stdout 文本。这把「首个 TLS 依赖例外」的范围压到最小：fulu 自身
/// 仍然不链接任何 TLS 库。
///
/// 边界（真闸门始终是箓 + 确认：limited + Destructive，逐次确认）：
/// - URL 只接受 http/https；拒绝 `file://` / `data:` / `javascript:`；
/// - 拒绝 userinfo 技巧（`http://evil@example.com` 的 host 与可见域名不符）；
/// - `ToolPolicy::allowed_domains` 非空时，host 必须命中白名单后缀；
/// - 浏览器缺失 / 启动失败 / 页面无文本 → 如实报错，绝不伪装成功。
pub struct MoliTool;

/// 正文抽取后的单次上限（与 fs.read 同源）。
pub const MOLI_MAX_BYTES: usize = 64 * 1024;
/// JS 渲染等待（虚拟时间预算）上下限与默认值。
pub const MOLI_WAIT_MIN_MS: u64 = 0;
pub const MOLI_WAIT_MAX_MS: u64 = 15_000;
pub const MOLI_WAIT_DEFAULT_MS: u64 = 3_000;
/// 浏览器原始 DOM 读取上限（防异常大页拖爆内存）。
const MOLI_DOM_CAPTURE_BYTES: u64 = 8 * 1024 * 1024;

/// 按平台列出候选浏览器；暴露为纯函数便于测试。
pub fn moli_browser_candidates() -> Vec<PathBuf> {
    match std::env::consts::OS {
        "windows" => vec![
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".into(),
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe".into(),
            r"C:\Program Files\Google\Chrome\Application\chrome.exe".into(),
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe".into(),
        ],
        "macos" => vec![
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into(),
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into(),
            "/Applications/Chromium.app/Contents/MacOS/Chromium".into(),
        ],
        _ => vec![
            "/usr/bin/google-chrome".into(),
            "/usr/bin/chromium".into(),
            "/usr/bin/chromium-browser".into(),
            "/snap/bin/chromium".into(),
        ],
    }
}

/// 解析可用浏览器：`FULU_MOLI_BROWSER` 环境变量优先，其次平台候选里
/// 第一个真实存在的可执行文件。找不到返回 None（调用方如实报错）。
fn resolve_moli_browser() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("FULU_MOLI_BROWSER") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    moli_browser_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

/// 校验 URL：返回 (scheme 后的规范化 URL, host)。不做网络请求。
fn validate_moli_url(raw: &str, allowed_domains: &[String]) -> Result<(String, String), ToolError> {
    let trimmed = raw.trim();
    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or_else(|| ToolError::InvalidArguments(format!("URL 缺少 http(s):// 前缀：{raw}")))?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(ToolError::NotAllowed(format!(
            "moli 只接受 http/https，拒绝 {scheme}://（file/data/javascript 等一律禁止）"
        )));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(ToolError::InvalidArguments(format!(
            "URL 缺少主机名：{raw}"
        )));
    }
    if authority.contains('@') {
        return Err(ToolError::NotAllowed(
            "URL 含 userinfo（@）：可见域名可能与真实主机不符，moli 拒绝".into(),
        ));
    }
    if authority
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ToolError::InvalidArguments(format!(
            "URL 含空白/控制字符：{raw}"
        )));
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if !allowed_domains.is_empty()
        && !allowed_domains.iter().any(|domain| {
            host == domain.to_lowercase() || host.ends_with(&format!(".{}", domain.to_lowercase()))
        })
    {
        return Err(ToolError::NotAllowed(format!(
            "主机 {host} 不在域名白名单内：{}",
            allowed_domains.join("、")
        )));
    }
    Ok((trimmed.to_owned(), host))
}

/// HTML → 正文文本：去掉 script/style 块与全部标签，解码常见实体，
/// 折叠空白。不追求完美排版，只保证「模型看到的是人能读的文本」。
pub fn html_to_text(html: &str) -> String {
    // 1. 去 script/style 块（大小写不敏感，容忍属性；未闭合则吞到串尾）。
    let mut stripped = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let lower_rest = rest.to_lowercase();
        // 取最先出现的块标记（or_else 恒偏好其一会让更早的块漏删）。
        let pos = match (lower_rest.find("<script"), lower_rest.find("<style")) {
            (Some(s), Some(t)) => Some(s.min(t)),
            (Some(s), None) => Some(s),
            (None, Some(t)) => Some(t),
            (None, None) => None,
        };
        let Some(pos) = pos else {
            break;
        };
        let close_tag = if lower_rest[pos..].starts_with("<script") {
            "</script>"
        } else {
            "</style>"
        };
        let close = lower_rest[pos..]
            .find(close_tag)
            .map(|rel| pos + rel + close_tag.len())
            .unwrap_or(lower_rest.len());
        stripped.push_str(&rest[..pos]);
        rest = &rest[close..];
    }
    stripped.push_str(rest);

    // 2. 去标签。
    let mut text = String::with_capacity(stripped.len());
    let mut in_tag = false;
    for ch in stripped.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    // 3. 实体解码（常见几个；数字实体由浏览器渲染侧处理，不在此展开）。
    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    // 4. 折叠空白：每行 trim，压缩连续空格，去掉连续空行。
    let mut out = String::new();
    let mut blank = true;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !blank {
                out.push('\n');
                blank = true;
            }
        } else {
            out.push_str(&line);
            out.push('\n');
            blank = false;
        }
    }
    out.trim().to_owned()
}

impl Tool for MoliTool {
    fn id(&self) -> &str {
        "moli.open"
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        let url = args
            .field("url")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 url".into()))?;
        let (url, _host) = validate_moli_url(&url, &policy.allowed_domains)?;
        let wait_ms = args
            .field("wait_ms")
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(MOLI_WAIT_DEFAULT_MS)
            .clamp(MOLI_WAIT_MIN_MS, MOLI_WAIT_MAX_MS);
        let browser = resolve_moli_browser().ok_or_else(|| {
            ToolError::NotAllowed(
                "本机未找到可用的无头浏览器（Edge/Chrome）；可设 FULU_MOLI_BROWSER 指向浏览器可执行文件".into(),
            )
        })?;
        let dom = open_with_browser(&browser, &url, wait_ms)?;
        let mut text = html_to_text(&dom);
        if text.is_empty() {
            return Err(ToolError::Io(
                "页面无可见文本（可能是纯 JS 渲染未完成、反爬或空页）；如实回报，不伪装成功".into(),
            ));
        }
        let truncated = text.chars().count() > MOLI_MAX_BYTES;
        if truncated {
            let cut: String = text.chars().take(MOLI_MAX_BYTES).collect();
            text = format!("{cut}\n[TRUNCATED：moli 正文超过 64 KiB 上限]");
        }
        Ok(ToolOutput {
            name: self.id().into(),
            content: text,
            truncated,
        })
    }
}

/// 无头浏览器参数表（纯函数，便于测试锁住「必须用独立 profile」）。
fn moli_headless_args(wait_ms: u64, profile: &std::path::Path) -> Vec<String> {
    vec![
        "--headless=new".into(),
        "--disable-gpu".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--dump-dom".into(),
        format!("--virtual-time-budget={wait_ms}"),
        format!("--user-data-dir={}", profile.display()),
    ]
}

/// 用外部浏览器无头拉取 DOM（独立函数便于真机集成测试）。
fn open_with_browser(browser: &Path, url: &str, wait_ms: u64) -> Result<String, ToolError> {
    use std::process::{Command, Stdio};
    // 独立临时 profile：用户开着 Edge/Chrome 时，同 profile 的新进程会把请求
    // **转交给已有实例后立刻退出**（exit 0、stdout 为空），拿到的就是空页面。
    // 只有另给一个 --user-data-dir 才真的起一个无头实例；用完尽力删掉。
    let profile = std::env::temp_dir().join(format!(
        "fulu-moli-profile-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let output = Command::new(browser)
        .args(moli_headless_args(wait_ms, &profile))
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output();
    // 先清理再判错：浏览器没起来时 profile 往往已经被创建了一半。
    let _ = std::fs::remove_dir_all(&profile);
    let output = output.map_err(|e| ToolError::Io(format!("moli 启动浏览器失败：{e}")))?;
    if !output.status.success() {
        return Err(ToolError::Io(format!(
            "moli 浏览器退出码 {:?}，页面拉取失败",
            output.status.code()
        )));
    }
    let mut dom_bytes = Vec::new();
    use std::io::Read;
    (&output.stdout[..])
        .take(MOLI_DOM_CAPTURE_BYTES + 1)
        .read_to_end(&mut dom_bytes)
        .map_err(|e| ToolError::Io(format!("moli 读取 DOM 失败：{e}")))?;
    Ok(String::from_utf8_lossy(&dom_bytes).into_owned())
}

// ---------------------------------------------------------------------------
// 摄行（T16）：电脑控制。默认**不存在**，开启后逐次确认。
// ---------------------------------------------------------------------------
//
// 这是全系统唯一能「以用户本人身份操作任意窗口」的能力：不受工作区路径约束，
// 也不受命令行白名单约束。风险评估见 docs/03 第 4 节，硬规则：
//
// * 默认不注册（不是「注册后拒绝」——模型连看到它的机会都没有）；
// * 环境变量 `FULU_ALLOW_SHEXING=1` + 会话显式开启，两把钥匙同时到位；
// * 风险等级 `Destructive` → 敕门 `EveryCall`，每一次动作都要人点头；
// * 平台：当前仅 Windows（SendKeys + user32）；其它平台如实报「不支持」，不假装。

/// 摄行四件套的工具名（供箓层登记与 CLI 展示）。
pub const SHEXING_TOOL_IDS: [&str; 4] = [
    "shexing.type",
    "shexing.key",
    "shexing.click",
    "shexing.screenshot",
];
/// 单次键入的字符上限（防模型把整篇文档「打」进别的程序）。
pub const SHEXING_MAX_TEXT_CHARS: usize = 2000;
/// 摄行单次动作超时。
const SHEXING_TIMEOUT_MS: u64 = 8_000;

const SENDKEYS_SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait($env:FULU_SHEXING_KEYS)
"#;

const CLICK_SCRIPT: &str = r#"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class FuluInput {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
}
"@
[FuluInput]::SetCursorPos([int]$env:FULU_SHEXING_X, [int]$env:FULU_SHEXING_Y) | Out-Null
if ($env:FULU_SHEXING_BUTTON -eq 'right') {
  [FuluInput]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero)
  [FuluInput]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero)
} else {
  [FuluInput]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
  [FuluInput]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
}
"#;

const SCREENSHOT_SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
$bmp.Save($env:FULU_SHEXING_PATH, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose()
$bmp.Dispose()
Write-Output "$($bounds.Width)x$($bounds.Height)"
"#;

fn ensure_shexing_supported() -> Result<(), ToolError> {
    if cfg!(windows) {
        Ok(())
    } else {
        Err(ToolError::NotAllowed(
            "摄行当前仅支持 Windows（SendKeys + user32）；macOS Accessibility / Linux xdotool 未实现，不做假装支持"
                .into(),
        ))
    }
}

/// 跑一段 PowerShell。文本一律经环境变量传入，绝不拼进脚本——
/// 避免引号/换行被当成代码执行。
fn run_shexing_script(
    script: &str,
    env: &[(&str, &str)],
    root: &Path,
) -> Result<String, ToolError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| ToolError::Io(format!("摄行无法启动 PowerShell：{error}")))?;
    let deadline = Instant::now() + Duration::from_millis(SHEXING_TIMEOUT_MS);
    let status = loop {
        match child
            .try_wait()
            .map_err(|error| ToolError::Io(error.to_string()))?
        {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::Io(format!(
                    "摄行动作超时（>{SHEXING_TIMEOUT_MS}ms），已终止"
                )));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|error| ToolError::Io(format!("摄行读取输出失败：{error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr_text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !status.success() {
        let detail = if stderr_text.is_empty() {
            stdout
        } else {
            stderr_text
        };
        return Err(ToolError::Io(format!("摄行脚本失败：{detail}")));
    }
    Ok(stdout)
}

/// 把普通文本转成 SendKeys 字面量：`+ ^ % ~ ( ) [ ] { }` 必须转义，
/// 换行/制表转成 `{ENTER}` / `{TAB}`，其余原样。
pub fn escape_sendkeys(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '+' | '^' | '%' | '~' | '(' | ')' | '[' | ']' => {
                out.push('{');
                out.push(ch);
                out.push('}');
            }
            '{' => out.push_str("{{}"),
            '}' => out.push_str("{}}"),
            '\n' => out.push_str("{ENTER}"),
            '\r' => {} // 换行已由 \n 处理，避免 CRLF 打两次回车
            '\t' => out.push_str("{TAB}"),
            other => out.push(other),
        }
    }
    out
}

/// 组合键白名单：只允许字母数字与 `^ % +` 修饰符，以及 `{NAME}` 形式的命名键。
/// 其它字符（尤其是引号、分号、换行）一律拒绝——它会被原样交给 SendKeys。
fn validate_sendkeys(raw: &str) -> Result<(), String> {
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '^' | '%' | '+' => {}
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(inner) if inner.is_ascii_alphanumeric() => name.push(inner),
                        Some(_) => return Err("命名键只允许字母数字，如 {ENTER}、{F5}".into()),
                        None => return Err("命名键缺少右花括号".into()),
                    }
                }
                if name.is_empty() {
                    return Err("命名键不能为空（花括号里要有键名）".into());
                }
            }
            other if other.is_ascii_alphanumeric() => {}
            other => return Err(format!("键位里有不认识的字符「{other}」")),
        }
    }
    Ok(())
}

/// 摄行 · 键入文本。
pub struct ShexingTypeTool;

impl Tool for ShexingTypeTool {
    fn id(&self) -> &str {
        SHEXING_TOOL_IDS[0]
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        ensure_shexing_supported()?;
        let text = args
            .field("text")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 text".into()))?;
        let chars = text.chars().count();
        if chars == 0 {
            return Err(ToolError::InvalidArguments("text 不能为空".into()));
        }
        if chars > SHEXING_MAX_TEXT_CHARS {
            return Err(ToolError::InvalidArguments(format!(
                "text 有 {chars} 个字符，超过单次 {SHEXING_MAX_TEXT_CHARS} 字符上限"
            )));
        }
        let root = policy.root.clone().unwrap_or_else(|| PathBuf::from("."));
        let keys = escape_sendkeys(&text);
        run_shexing_script(SENDKEYS_SCRIPT, &[("FULU_SHEXING_KEYS", &keys)], &root)?;
        Ok(ToolOutput {
            name: self.id().into(),
            content: format!("已向当前前台窗口键入 {chars} 个字符"),
            truncated: false,
        })
    }
}

/// 摄行 · 发送组合键（SendKeys 语法子集：`^s`、`%{TAB}`、`{ENTER}`）。
pub struct ShexingKeyTool;

impl Tool for ShexingKeyTool {
    fn id(&self) -> &str {
        SHEXING_TOOL_IDS[1]
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        ensure_shexing_supported()?;
        let keys = args
            .field("keys")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 keys".into()))?;
        validate_sendkeys(&keys).map_err(ToolError::InvalidArguments)?;
        let root = policy.root.clone().unwrap_or_else(|| PathBuf::from("."));
        run_shexing_script(SENDKEYS_SCRIPT, &[("FULU_SHEXING_KEYS", &keys)], &root)?;
        Ok(ToolOutput {
            name: self.id().into(),
            content: format!("已发送组合键 {keys}"),
            truncated: false,
        })
    }
}

/// 摄行 · 点击（先移动光标再按下/抬起）。
pub struct ShexingClickTool;

impl Tool for ShexingClickTool {
    fn id(&self) -> &str {
        SHEXING_TOOL_IDS[2]
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        ensure_shexing_supported()?;
        let x: u32 = args
            .field("x")
            .and_then(|raw| raw.parse().ok())
            .ok_or_else(|| ToolError::InvalidArguments("缺少合法整数 x".into()))?;
        let y: u32 = args
            .field("y")
            .and_then(|raw| raw.parse().ok())
            .ok_or_else(|| ToolError::InvalidArguments("缺少合法整数 y".into()))?;
        let button = args.field("button").unwrap_or_else(|| "left".into());
        if button != "left" && button != "right" {
            return Err(ToolError::InvalidArguments(format!(
                "button 只接受 left / right，收到「{button}」"
            )));
        }
        let root = policy.root.clone().unwrap_or_else(|| PathBuf::from("."));
        run_shexing_script(
            CLICK_SCRIPT,
            &[
                ("FULU_SHEXING_X", &x.to_string()),
                ("FULU_SHEXING_Y", &y.to_string()),
                ("FULU_SHEXING_BUTTON", &button),
            ],
            &root,
        )?;
        Ok(ToolOutput {
            name: self.id().into(),
            content: format!("已在 ({x}, {y}) 执行{button}键点击"),
            truncated: false,
        })
    }
}

/// 摄行 · 截屏（只写工作区，不联网、不自动进提示词）。
pub struct ShexingScreenshotTool;

impl Tool for ShexingScreenshotTool {
    fn id(&self) -> &str {
        SHEXING_TOOL_IDS[3]
    }

    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        ensure_shexing_supported()?;
        let root = policy.root.as_ref().ok_or(ToolError::RootNotConfigured)?;
        let raw = args
            .field("path")
            .ok_or_else(|| ToolError::InvalidArguments("缺少 path".into()))?;
        let full = resolve_workspace_path(root, &raw)?;
        if !full
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("png"))
            .unwrap_or(false)
        {
            return Err(ToolError::InvalidArguments(
                "截屏只写 .png 文件（路径需以 .png 结尾）".into(),
            ));
        }
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ToolError::Io(format!("建父目录失败：{error}")))?;
        }
        let screen = run_shexing_script(
            SCREENSHOT_SCRIPT,
            &[("FULU_SHEXING_PATH", &full.display().to_string())],
            root,
        )?;
        let bytes = std::fs::metadata(&full)
            .map_err(|error| ToolError::Io(format!("截屏文件未生成：{error}")))?
            .len();
        Ok(ToolOutput {
            name: self.id().into(),
            content: format!(
                "已截屏 {screen}（{}，{bytes} 字节）写入 {}",
                if bytes > 0 {
                    "非空"
                } else {
                    "空文件，可疑"
                },
                full.display()
            ),
            truncated: false,
        })
    }
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    policy: ToolPolicy,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new(ToolPolicy::default())
    }
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("policy", &self.policy)
            .finish()
    }
}

impl ToolRegistry {
    pub fn new(policy: ToolPolicy) -> Self {
        let mut tools: BTreeMap<String, Arc<dyn Tool>> = BTreeMap::new();
        tools.insert("echo".into(), Arc::new(EchoTool));
        tools.insert("fs.read".into(), Arc::new(FsReadTool));
        // T17：写入工具登记即存在，能否调用仍由箓决定（Strict 符被驳回）；
        // 真正的闸门是敕门：Mutate 级，每符确认一次。
        tools.insert("fs.write".into(), Arc::new(FsWriteTool));
        // T15：问天登记时后端为空（明确报错路径）；接入外宗后由门面注入后端。
        tools.insert(
            "ask.search".into(),
            Arc::new(AskSearchTool::new(Vec::new())),
        );
        // T14：moli 走外部无头浏览器；登记即存在，能否调用由箓 + 敕门决定。
        tools.insert("moli.open".into(), Arc::new(MoliTool));
        // 敕行：登记即存在，能否调用仍由箓的权限等级决定（Strict 符会被驳回）。
        tools.insert("shell.exec".into(), Arc::new(ShellTool));
        Self { tools, policy }
    }

    pub fn policy(&self) -> &ToolPolicy {
        &self.policy
    }

    /// 换工作区/域名策略时只换策略，**保留已注册的工具**。
    /// 直接重建 registry 会把装配好的外宗（MCP）工具整批丢掉。
    pub fn set_policy(&mut self, policy: ToolPolicy) {
        self.policy = policy;
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.id().to_owned(), tool);
    }

    /// 取回已注册工具的句柄（问天后端绑定等组合场景用；只读）。
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// 执行入口：白名单 → 配额 → 工具自身边界，三道都要过。
    pub fn execute(
        &self,
        name: &str,
        args: &ToolArguments,
        auth: &Authorization,
        used: &mut BTreeMap<String, u32>,
    ) -> Result<ToolOutput, ToolError> {
        auth.check_call(name, used.get(name).copied().unwrap_or(0))
            .map_err(|error| match error {
                RegisterError::QuotaExceeded { tool, max_calls } => {
                    ToolError::QuotaExceeded(format!("{tool} 上限 {max_calls}"))
                }
                other => ToolError::NotAllowed(other.to_string()),
            })?;
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::NotAllowed(format!("工具 {name} 未实现")))?;
        let output = tool.invoke(args, &self.policy)?;
        *used.entry(name.to_owned()).or_insert(0) += 1;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_core::Talisman;
    use fulu_register::Register;

    fn auth_with(tools: &[&str]) -> Authorization {
        let mut fu = Talisman::sample();
        fu.tool_refs = tools.iter().map(|id| (*id).to_owned()).collect();
        fu.harness.allow_tools = fu.tool_refs.clone();
        Register::baseline().authorize(&fu).unwrap()
    }

    #[test]
    fn echo_roundtrip_records_one_call() {
        let registry = ToolRegistry::default();
        let auth = auth_with(&["echo"]);
        let mut used = BTreeMap::new();
        let output = registry
            .execute(
                "echo",
                &ToolArguments {
                    json: r#"{"text":"敕令急急如律令"}"#.into(),
                },
                &auth,
                &mut used,
            )
            .unwrap();
        assert_eq!(output.content, "敕令急急如律令");
        assert_eq!(used.get("echo"), Some(&1));
    }

    #[test]
    fn unauthorized_tool_is_blocked_even_if_implemented() {
        let registry = ToolRegistry::default();
        let auth = auth_with(&["echo"]);
        let mut used = BTreeMap::new();
        assert!(matches!(
            registry.execute("fs.read", &ToolArguments::empty(), &auth, &mut used),
            Err(ToolError::NotAllowed(_))
        ));
        assert!(used.is_empty());
    }

    #[test]
    fn quota_is_enforced_by_execution_layer() {
        let registry = ToolRegistry::default();
        let mut auth = auth_with(&["echo"]);
        auth.max_calls.insert("echo".into(), 1);
        let mut used = BTreeMap::new();
        registry
            .execute("echo", &ToolArguments::empty(), &auth, &mut used)
            .unwrap();
        assert!(matches!(
            registry.execute("echo", &ToolArguments::empty(), &auth, &mut used),
            Err(ToolError::QuotaExceeded(_))
        ));
    }

    #[test]
    fn fs_read_refuses_absolute_and_traversal_paths() {
        let root = std::env::temp_dir();
        let registry = ToolRegistry::new(ToolPolicy {
            root: Some(root.clone()),
            allowed_domains: Vec::new(),
        });
        let auth = auth_with(&["fs.read"]);
        let mut used = BTreeMap::new();
        for bad in ["../../etc/passwd", root.to_string_lossy().as_ref()] {
            let args = ToolArguments {
                json: format!(r#"{{"path":"{}"}}"#, bad.replace('\\', "/")),
            };
            let result = registry.execute("fs.read", &args, &auth, &mut used);
            assert!(matches!(result, Err(ToolError::PathForbidden(_))), "{bad}");
        }
    }

    #[test]
    fn fs_read_works_only_with_a_configured_root() {
        let args = ToolArguments {
            json: r#"{"path":"a.txt"}"#.into(),
        };
        let auth = auth_with(&["fs.read"]);
        let mut used = BTreeMap::new();
        assert_eq!(
            ToolRegistry::default().execute("fs.read", &args, &auth, &mut used),
            Err(ToolError::RootNotConfigured)
        );
    }

    fn auth_with_shell() -> Authorization {
        let mut fu = Talisman::sample();
        // 敕行是「受限执行」级：符必须申请 Limited 沙箱才可能调用。
        fu.harness.sandbox = fulu_core::Sandbox::Limited;
        fu.tool_refs = vec!["shell.exec".into()];
        fu.harness.allow_tools = vec!["shell.exec".into()];
        Register::baseline().authorize(&fu).unwrap()
    }

    fn shell_registry_with_temp_root() -> (ToolRegistry, std::path::PathBuf) {
        let root = unique_temp_root("fulu_shell_test");
        std::fs::create_dir_all(&root).unwrap();
        let registry = ToolRegistry::new(ToolPolicy {
            root: Some(root.clone()),
            allowed_domains: Vec::new(),
        });
        (registry, root)
    }

    #[test]
    fn shell_echo_runs_in_configured_workspace() {
        let (registry, _root) = shell_registry_with_temp_root();
        let auth = auth_with_shell();
        let mut used = BTreeMap::new();
        let output = registry
            .execute(
                "shell.exec",
                &ToolArguments {
                    json: r#"{"command":"echo fulu-ok"}"#.into(),
                },
                &auth,
                &mut used,
            )
            .unwrap();
        assert!(output.content.contains("fulu-ok"), "{}", output.content);
    }

    #[test]
    fn shell_requires_configured_root_even_when_authorized() {
        let registry = ToolRegistry::default();
        let auth = auth_with_shell();
        let mut used = BTreeMap::new();
        assert_eq!(
            registry.execute(
                "shell.exec",
                &ToolArguments {
                    json: r#"{"command":"echo x"}"#.into()
                },
                &auth,
                &mut used
            ),
            Err(ToolError::RootNotConfigured)
        );
    }

    #[test]
    fn shell_blocklist_refuses_destructive_patterns() {
        let (registry, _root) = shell_registry_with_temp_root();
        let auth = auth_with_shell();
        let mut used = BTreeMap::new();
        let result = registry.execute(
            "shell.exec",
            &ToolArguments {
                json: r#"{"command":"echo x && rm -rf /"}"#.into(),
            },
            &auth,
            &mut used,
        );
        assert!(matches!(result, Err(ToolError::NotAllowed(msg)) if msg.contains("rm -rf /")));
        assert!(used.is_empty(), "被拒绝的调用不计配额");
    }

    #[test]
    fn shell_strict_talisman_cannot_escalate_into_shell() {
        // 敕行是受限级工具：只读符权限不足，在箓层就被双向核验驳回（不进风火轮）。
        let mut fu = Talisman::sample();
        fu.tool_refs = vec!["shell.exec".into()];
        fu.harness.allow_tools = vec!["shell.exec".into()];
        assert!(matches!(
            Register::baseline().authorize(&fu),
            Err(fulu_register::RegisterError::PermissionTooLow { .. })
        ));
    }

    #[test]
    fn shell_timeout_is_capped_and_reported() {
        let (registry, _root) = shell_registry_with_temp_root();
        let auth = auth_with_shell();
        let mut used = BTreeMap::new();
        let command = if cfg!(windows) {
            r#"{"command":"Start-Sleep -Seconds 5","timeout_ms":300}"#
        } else {
            r#"{"command":"sleep 5","timeout_ms":300}"#
        };
        let output = registry
            .execute(
                "shell.exec",
                &ToolArguments {
                    json: command.into(),
                },
                &auth,
                &mut used,
            )
            .unwrap();
        assert!(output.content.contains("敕行超时"), "{}", output.content);
    }

    #[test]
    fn shell_timeout_argument_is_clamped() {
        // 超上限的 timeout_ms 被夹到 60s 上限：用一个立即返回的命令验证不报错。
        let (registry, _root) = shell_registry_with_temp_root();
        let auth = auth_with_shell();
        let mut used = BTreeMap::new();
        let output = registry
            .execute(
                "shell.exec",
                &ToolArguments {
                    json: r#"{"command":"echo ok","timeout_ms":999999999}"#.into(),
                },
                &auth,
                &mut used,
            )
            .unwrap();
        assert!(output.content.contains("ok"));
    }

    // ---- T17：fs.write 受控写入 ----

    fn auth_with_write() -> Authorization {
        let mut fu = Talisman::sample();
        // fs.write 是受限级工具：符必须申请 Limited 沙箱才可能调用。
        fu.harness.sandbox = fulu_core::Sandbox::Limited;
        fu.tool_refs = vec!["fs.write".into()];
        fu.harness.allow_tools = vec!["fs.write".into()];
        Register::baseline().authorize(&fu).unwrap()
    }

    // CI 修证：临时根目录必须每次调用唯一。此前固定 `fulu_write_test_{pid}`，
    // cargo test 并行跑多个 fs.write 测试时，后来者的 remove_dir_all 会把
    // 先行者的中间产物删掉（windows/mac 上实测出「备份文件 NotFound」）。
    static TEMP_ROOT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn unique_temp_root(prefix: &str) -> PathBuf {
        let n = TEMP_ROOT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("{prefix}_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_registry_with_temp_root() -> (ToolRegistry, PathBuf) {
        let root = unique_temp_root("fulu_write_test");
        let registry = ToolRegistry::new(ToolPolicy {
            root: Some(root.clone()),
            allowed_domains: Vec::new(),
        });
        (registry, root)
    }

    fn write_args(path: &str, content: &str) -> ToolArguments {
        let payload = serde_json::json!({ "path": path, "content": content });
        ToolArguments {
            json: payload.to_string(),
        }
    }

    #[test]
    fn fs_write_creates_file_and_nested_parents_within_workspace() {
        let (registry, root) = write_registry_with_temp_root();
        let auth = auth_with_write();
        let mut used = BTreeMap::new();
        registry
            .execute(
                "fs.write",
                &write_args("sub/dir/notes.txt", "急急如律令"),
                &auth,
                &mut used,
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("sub/dir/notes.txt")).unwrap(),
            "急急如律令"
        );
    }

    #[test]
    fn fs_write_refuses_absolute_and_traversal_paths() {
        let (registry, root) = write_registry_with_temp_root();
        let auth = auth_with_write();
        let mut used = BTreeMap::new();
        for bad in ["../../escape.txt", root.to_string_lossy().as_ref()] {
            let result = registry.execute("fs.write", &write_args(bad, "x"), &auth, &mut used);
            assert!(matches!(result, Err(ToolError::PathForbidden(_))), "{bad}");
        }
        assert!(used.is_empty(), "被拒绝的调用不计配额");
    }

    #[test]
    fn fs_write_backs_up_existing_file_before_overwrite() {
        let (registry, root) = write_registry_with_temp_root();
        let auth = auth_with_write();
        let mut used = BTreeMap::new();
        let target = root.join("doc.txt");
        std::fs::write(&target, "旧内容").unwrap();
        registry
            .execute(
                "fs.write",
                &write_args("doc.txt", "新内容"),
                &auth,
                &mut used,
            )
            .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "新内容");
        let backup = root.join("doc.txt.fulu-bak");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "旧内容");
    }

    #[test]
    fn fs_write_backup_names_never_clobber_previous_backups() {
        let first = backup_path_for(Path::new("/w/a.txt"));
        assert_eq!(first, PathBuf::from("/w/a.txt.fulu-bak"));
        // 已存在同名备份时退到编号备份（用真实文件验证 exists 分支）。
        let root = unique_temp_root("fulu_bak_test");
        let target = root.join("a.txt");
        std::fs::write(&target, "x").unwrap();
        std::fs::write(target.with_file_name("a.txt.fulu-bak"), "y").unwrap();
        let second = backup_path_for(&target);
        assert_eq!(second, target.with_file_name("a.txt.fulu-bak.2"));
    }

    #[test]
    fn fs_write_append_adds_without_overwrite_or_backup() {
        let (registry, root) = write_registry_with_temp_root();
        let auth = auth_with_write();
        let mut used = BTreeMap::new();
        std::fs::write(root.join("log.txt"), "一").unwrap();
        let args = ToolArguments {
            json: serde_json::json!({"path":"log.txt","content":"二","append":true}).to_string(),
        };
        registry
            .execute("fs.write", &args, &auth, &mut used)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("log.txt")).unwrap(),
            "一二"
        );
        assert!(
            !root.join("log.txt.fulu-bak").exists(),
            "追加不破坏原文件，不应产生备份"
        );
    }

    #[test]
    fn fs_write_rejects_oversized_content() {
        let (registry, _root) = write_registry_with_temp_root();
        let auth = auth_with_write();
        let mut used = BTreeMap::new();
        let big = "字".repeat(MAX_WRITE_BYTES + 1);
        let result = registry.execute("fs.write", &write_args("big.txt", &big), &auth, &mut used);
        assert!(matches!(result, Err(ToolError::InvalidArguments(msg)) if msg.contains("上限")));
    }

    #[test]
    fn fs_write_requires_configured_root() {
        let registry = ToolRegistry::default();
        let auth = auth_with_write();
        let mut used = BTreeMap::new();
        assert_eq!(
            registry.execute("fs.write", &write_args("a.txt", "x"), &auth, &mut used),
            Err(ToolError::RootNotConfigured)
        );
    }

    #[test]
    fn fs_write_strict_talisman_cannot_escalate_into_writing() {
        // fs.write 是受限级工具：只读符权限不足，在箓层就被双向核验驳回。
        let mut fu = Talisman::sample();
        fu.tool_refs = vec!["fs.write".into()];
        fu.harness.allow_tools = vec!["fs.write".into()];
        assert!(matches!(
            Register::baseline().authorize(&fu),
            Err(fulu_register::RegisterError::PermissionTooLow { .. })
        ));
    }

    // ---- T15：问天 ask.search（可插拔后端） ----

    struct FakeBackend {
        id: String,
        result: Result<String, ToolError>,
        seen: std::sync::Mutex<Vec<(String, usize)>>,
    }

    impl FakeBackend {
        fn ok(id: &str) -> Self {
            Self {
                id: id.into(),
                result: Ok(format!("{id} 的搜索结果")),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn failing(id: &str) -> Self {
            Self {
                id: id.into(),
                result: Err(ToolError::Io(format!("{id} 炸了"))),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl SearchBackend for FakeBackend {
        fn id(&self) -> &str {
            &self.id
        }
        fn search(&self, query: &str, limit: usize) -> Result<String, ToolError> {
            self.seen.lock().unwrap().push((query.to_owned(), limit));
            self.result.clone()
        }
    }

    fn ask_args(query: &str) -> ToolArguments {
        ToolArguments {
            json: serde_json::json!({ "query": query }).to_string(),
        }
    }

    fn ask_registry(backends: Vec<Arc<dyn SearchBackend>>) -> ToolRegistry {
        let mut registry = ToolRegistry::default();
        registry.register(Arc::new(AskSearchTool::new(backends)) as Arc<dyn Tool>);
        registry
    }

    fn auth_with_search() -> Authorization {
        let mut fu = Talisman::sample();
        fu.harness.sandbox = fulu_core::Sandbox::Limited;
        fu.tool_refs = vec!["ask.search".into()];
        fu.harness.allow_tools = vec!["ask.search".into()];
        Register::baseline().authorize(&fu).unwrap()
    }

    #[test]
    fn ask_search_without_backend_fails_loudly() {
        let registry = ToolRegistry::default(); // 出厂问天无后端
        let auth = auth_with_search();
        let mut used = BTreeMap::new();
        let result = registry.execute("ask.search", &ask_args("符箓"), &auth, &mut used);
        assert!(
            matches!(result, Err(ToolError::NotAllowed(ref msg)) if msg.contains("未配置任何搜索后端")),
            "{result:?}"
        );
    }

    #[test]
    fn ask_search_routes_to_first_backend_and_labels_the_source() {
        let registry = ask_registry(vec![
            Arc::new(FakeBackend::ok("mcp.a.web_search")),
            Arc::new(FakeBackend::ok("mcp.b.search")),
        ]);
        let auth = auth_with_search();
        let mut used = BTreeMap::new();
        let output = registry
            .execute("ask.search", &ask_args("符箓 架构"), &auth, &mut used)
            .unwrap();
        assert!(output.content.contains("[问天 via mcp.a.web_search]"));
        assert!(output.content.contains("mcp.a.web_search 的搜索结果"));
    }

    #[test]
    fn ask_search_falls_back_when_backend_fails() {
        let fallback = Arc::new(FakeBackend::ok("mcp.b.search"));
        let registry = ask_registry(vec![
            Arc::new(FakeBackend::failing("mcp.a.dead")),
            fallback.clone(),
        ]);
        let auth = auth_with_search();
        let mut used = BTreeMap::new();
        let output = registry
            .execute("ask.search", &ask_args("问天"), &auth, &mut used)
            .unwrap();
        assert!(output.content.contains("[问天 via mcp.b.search]"));
        assert_eq!(fallback.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn ask_search_all_backends_failed_reports_every_reason() {
        let registry = ask_registry(vec![
            Arc::new(FakeBackend::failing("mcp.a.dead")),
            Arc::new(FakeBackend::failing("mcp.b.dead")),
        ]);
        let auth = auth_with_search();
        let mut used = BTreeMap::new();
        let result = registry.execute("ask.search", &ask_args("问天"), &auth, &mut used);
        assert!(
            matches!(result, Err(ToolError::Io(ref msg)) if msg.contains("mcp.a.dead") && msg.contains("mcp.b.dead")),
            "{result:?}"
        );
    }

    #[test]
    fn ask_search_limit_is_clamped_and_query_validated() {
        let backend = Arc::new(FakeBackend::ok("mcp.a.search"));
        let registry = ask_registry(vec![backend.clone()]);
        let auth = auth_with_search();
        let mut used = BTreeMap::new();
        // 超上限被夹到 20。
        let args = ToolArguments {
            json: serde_json::json!({ "query": "x", "limit": 999 }).to_string(),
        };
        registry
            .execute("ask.search", &args, &auth, &mut used)
            .unwrap();
        assert_eq!(backend.seen.lock().unwrap()[0].1, ASK_SEARCH_LIMIT_MAX);
        // 空 query 拒绝。
        let result = registry.execute("ask.search", &ask_args("   "), &auth, &mut used);
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    // ---- T14：moli 浏览器 ----

    fn moli_policy(domains: &[&str]) -> ToolPolicy {
        ToolPolicy {
            root: None,
            allowed_domains: domains.iter().map(|d| d.to_string()).collect(),
        }
    }

    #[test]
    fn html_to_text_strips_script_style_tags_and_entities() {
        let html = r#"<html><head><style>.x{color:red}</style></head>
            <body><h1>符&nbsp;箓</h1>
            <script>var bomb = "<p>not text</p>";</script>
            <p>A &amp; B &lt;tag&gt; &quot;quoted&quot;</p>
            </body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("符 箓"), "{text}");
        assert!(text.contains("A & B <tag> \"quoted\""), "{text}");
        assert!(!text.contains("bomb"), "script 内容必须被剔除：{text}");
        assert!(!text.contains("color:red"), "style 内容必须被剔除：{text}");
        assert!(
            !text.contains("<h1>") && !text.contains("</"),
            "不允许残留原始标签（解码后的字面 <tag> 文本除外）：{text}"
        );
    }

    #[test]
    fn moli_rejects_non_http_schemes_and_userinfo() {
        let tool = MoliTool;
        let policy = moli_policy(&[]);
        for bad in [
            r#"{"url":"file:///C:/Windows/win.ini"}"#,
            r#"{"url":"data:text/html,<p>x</p>"}"#,
            r#"{"url":"javascript:alert(1)"}"#,
            r#"{"url":"http://evil@example.com/"}"#,
            r#"{"url":"不是网址"}"#,
        ] {
            let result = tool.invoke(&ToolArguments { json: bad.into() }, &policy);
            assert!(
                matches!(
                    result,
                    Err(ToolError::NotAllowed(_)) | Err(ToolError::InvalidArguments(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn moli_enforces_domain_allowlist_when_configured() {
        let tool = MoliTool;
        let policy = moli_policy(&["example.com"]);
        // 子域名放行判断在 validate 层：非白名单 host 必须在任何进程启动前被拒。
        let result = tool.invoke(
            &ToolArguments {
                json: r#"{"url":"https://attacker.net/x"}"#.into(),
            },
            &policy,
        );
        assert!(matches!(result, Err(ToolError::NotAllowed(msg)) if msg.contains("attacker.net")));
    }

    #[test]
    fn moli_validate_accepts_subdomain_and_keeps_host() {
        let (url, host) =
            validate_moli_url("https://Docs.Example.COM/path?q=1", &["example.com".into()])
                .unwrap();
        assert_eq!(host, "docs.example.com");
        assert_eq!(url, "https://Docs.Example.COM/path?q=1");
    }

    /// 无头参数必须带独立 profile：本机开着 Edge 时，共用默认 profile 的新进程
    /// 会把请求转交给已有实例并立刻退出（exit 0 + 空 stdout），moli 会**静默**
    /// 拉回空页。这条测试锁住那个修复，防止以后被人「简化」掉。
    #[test]
    fn moli_headless_args_always_isolate_the_profile() {
        let args = moli_headless_args(3_000, std::path::Path::new("fulu-moli-profile-x"));
        assert!(args.iter().any(|a| a == "--dump-dom"), "{args:?}");
        assert!(
            args.iter().any(|a| a == "--virtual-time-budget=3000"),
            "{args:?}"
        );
        assert!(
            args.iter()
                .any(|a| a.starts_with("--user-data-dir=") && a.len() > "--user-data-dir=".len()),
            "无头实例必须用独立 profile：{args:?}"
        );
    }

    /// 真机集成：本机有 Edge/Chrome 时，用本地 mock HTTP 服务走一次完整链路
    /// （无头浏览器 → DOM → 正文抽取）。没有浏览器则跳过（CI 容器常见）。
    #[test]
    fn moli_end_to_end_with_real_browser_when_available() {
        use std::io::{Read as _, Write as _};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let Some(browser) = moli_browser_candidates().into_iter().find(|p| p.is_file()) else {
            return; // 无浏览器环境：跳过而非失败
        };
        // 最小 mock HTTP 服务：一个 HTML 页面 + JS 改写 DOM。
        //
        // **必须循环 accept**：headless Chromium 打开一个页面会发起不止一个连接
        // （预连接 / favicon / 重试），只接一次会让真正的页面请求落空，DOM 里就没有正文。
        // 这正是本测试此前偶发失败（单跑过、全量偶发不过）的原因。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_in_thread = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let body = "<html><body><h1>急急如律令</h1>\
                <script>document.body.innerHTML += '<p id=\"js\">js-ok</p>';</script>\
                </body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            while !stop_in_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        let url = format!("http://127.0.0.1:{port}/");
        let dom = match open_with_browser(&browser, &url, MOLI_WAIT_DEFAULT_MS) {
            Ok(dom) => dom,
            Err(error) => {
                // 提前失败也要把服务停掉，别留下一个空转线程。
                stop.store(true, Ordering::Relaxed);
                let _ = handle.join();
                panic!("{error}");
            }
        };
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        let text = html_to_text(&dom);
        assert!(
            text.contains("急急如律令"),
            "DOM {} 字节 / 抽文 {} 字：{text}",
            dom.len(),
            text.chars().count()
        );
        assert!(text.contains("js-ok"), "JS 必须真实执行：{text}");
    }

    // ---- T16 摄行：默认不存在、转义与键位校验 ----

    #[test]
    fn shexing_tools_are_absent_from_the_builtin_registry() {
        // 「默认禁用」的正确形态是**工具不存在**，而不是「存在但拒绝」。
        let registry = ToolRegistry::default();
        for id in SHEXING_TOOL_IDS {
            assert!(
                registry.get(id).is_none(),
                "内置注册表不得含 {id}：摄行必须显式解锁"
            );
        }
    }

    #[test]
    fn sendkeys_escaping_is_exact() {
        assert_eq!(escape_sendkeys("a+b"), "a{+}b");
        assert_eq!(escape_sendkeys("100%"), "100{%}");
        assert_eq!(escape_sendkeys("{x}"), "{{}x{}}");
        assert_eq!(escape_sendkeys("f(x)"), "f{(}x{)}");
        assert_eq!(escape_sendkeys("a\nb"), "a{ENTER}b");
        // CRLF 只打一次回车，不能打成两次。
        assert_eq!(escape_sendkeys("a\r\nb"), "a{ENTER}b");
        assert_eq!(escape_sendkeys("a\tb"), "a{TAB}b");
    }

    #[test]
    fn sendkeys_validation_rejects_injection_shapes() {
        assert!(validate_sendkeys("^s").is_ok());
        assert!(validate_sendkeys("%{TAB}").is_ok());
        assert!(validate_sendkeys("{ENTER}").is_ok());
        assert!(validate_sendkeys("abc123").is_ok());
        // 引号、分号、换行都可能被当成脚本/命令的一部分。
        assert!(validate_sendkeys("a; Remove-Item").is_err());
        assert!(validate_sendkeys("\"").is_err());
        assert!(validate_sendkeys("a\nb").is_err());
        assert!(validate_sendkeys("{ENTER").is_err());
        assert!(validate_sendkeys("{}").is_err());
        assert!(validate_sendkeys("{EN TER}").is_err());
    }

    #[test]
    fn shexing_platform_gate_is_honest() {
        // 只测平台闸门本身：**绝不**在测试里真的敲键盘/点鼠标——那会操作
        // 开发者当前的前台窗口，属于不可接受的测试副作用。
        let gate = ensure_shexing_supported();
        if cfg!(windows) {
            assert!(gate.is_ok());
        } else {
            let message = format!("{}", gate.unwrap_err());
            assert!(message.contains("仅支持 Windows"), "{message}");
        }
    }
}
