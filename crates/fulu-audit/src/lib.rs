//! 缴符（Audit）：append-only 事件日志、脱敏与可校验链（docs/02 第 3.4 节）。
//!
//! 仪轨每一步都落一条事件；`digest = sha256(prev_digest || canonical(事件除 digest 外))`，
//! 因此任何事后篡改都会让 `verify_chain()` 失败。日志里**从不写完整 prompt 明文
//! 与任何密钥字面量**：写入前统一经 `redact()` 脱敏。
//!
//! M2 使用同步追加写；M3 再接系统级日志与导出格式。

use fulu_core::TalismanState;
use fulu_script::canonical_bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const GENESIS: &str = "genesis";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Formed,
    Sealed,
    /// 会话头（每会话一次）：只记粗粒度的会话上下文摘要，不写符文正文
    Context,
    DryRun,
    Ignited,
    Invoked,
    ToolCall,
    ToolResult,
    /// 高危召将获得人确认（「敕可」）
    Confirmed,
    /// 高危召将被拒（「敕否」）——不是符被驳回，是这一次召将没被放行
    Denied,
    Answered,
    Archived,
    Broken,
    Rejected,
    Exploded,
    TimedOut,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Formed => "成符",
            Self::Sealed => "盖印",
            Self::Context => "会话头",
            Self::DryRun => "待焚",
            Self::Ignited => "焚化",
            Self::Invoked => "请神",
            Self::ToolCall => "召将",
            Self::ToolResult => "将回",
            Self::Confirmed => "敕可",
            Self::Denied => "敕否",
            Self::Answered => "回执",
            Self::Archived => "缴符归档",
            Self::Broken => "破符",
            Self::Rejected => "驳回",
            Self::Exploded => "炸炉",
            Self::TimedOut => "过坛",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub seq: u64,
    pub seal: String,
    pub state: TalismanState,
    pub kind: EventKind,
    pub detail: String,
    pub timestamp_ms: u64,
    pub prev_digest: String,
    pub digest: String,
}

#[derive(Serialize)]
struct UnsignedEvent<'a> {
    seq: u64,
    seal: &'a str,
    state: TalismanState,
    kind: EventKind,
    detail: &'a str,
    timestamp_ms: u64,
    prev_digest: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditError {
    Serialize(String),
    Io(String),
    ChainBroken { seq: u64, reason: String },
}

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(msg) => write!(f, "事件序列化失败：{msg}"),
            Self::Io(msg) => write!(f, "缴符日志写入失败：{msg}"),
            Self::ChainBroken { seq, reason } => {
                write!(f, "符印事件链在 #{seq} 处断裂：{reason}")
            }
        }
    }
}

impl std::error::Error for AuditError {}

/// 关键字脱敏。命中后把「关键字 + 后续值」整体替换为 `[REDACTED]`，
/// 宁可多遮也不漏；字节索引只落在 ASCII 关键字/分隔符上，UTF-8 安全。
pub fn redact(text: &str) -> String {
    const NEEDLES: [&str; 8] = [
        "sk-",
        "api_key=",
        "apikey=",
        "bearer ",
        "token=",
        "secret=",
        "password=",
        "authorization:",
    ];
    let mut output = text.to_owned();
    let lowered = output.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for needle in NEEDLES {
        let mut offset = 0_usize;
        while offset <= bytes.len() {
            let Some(relative) = lowered[offset..].find(needle) else {
                break;
            };
            let start = offset + relative;
            let mut end = start + needle.len();
            while end < bytes.len() {
                let byte = bytes[end];
                if byte.is_ascii_whitespace()
                    || matches!(byte, b'"' | b',' | b')' | b']' | b'}' | b';')
                {
                    break;
                }
                end += 1;
            }
            ranges.push((start, end));
            offset = end.max(start + needle.len());
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.0 <= last.1 => last.1 = last.1.max(range.1),
            _ => merged.push(range),
        }
    }
    for (start, end) in merged.into_iter().rev() {
        if output.is_char_boundary(start) && output.is_char_boundary(end) {
            output.replace_range(start..end, "[REDACTED]");
        }
    }
    output
}

fn digest_hex(event: &AuditEvent) -> Result<String, AuditError> {
    let unsigned = UnsignedEvent {
        seq: event.seq,
        seal: &event.seal,
        state: event.state,
        kind: event.kind,
        detail: &event.detail,
        timestamp_ms: event.timestamp_ms,
        prev_digest: &event.prev_digest,
    };
    let canonical = canonical_bytes(&unsigned).map_err(|e| AuditError::Serialize(e.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(event.prev_digest.as_bytes());
    hasher.update(&canonical);
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct AuditLog {
    events: Vec<AuditEvent>,
    path: Option<PathBuf>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            path: None,
        }
    }

    /// 追加写文件；**不 truncate**，重复运行会继续接在已有日志后面。
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            events: Vec::new(),
            path: Some(path.into()),
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn events(&self) -> &[AuditEvent] {
        &self.events
    }

    pub fn append(
        &mut self,
        seal: &str,
        state: TalismanState,
        kind: EventKind,
        detail: &str,
    ) -> Result<&AuditEvent, AuditError> {
        let previous = self
            .events
            .last()
            .map(|event| event.digest.as_str())
            .unwrap_or(GENESIS)
            .to_owned();
        let mut event = AuditEvent {
            seq: self.events.len() as u64,
            seal: seal.to_owned(),
            state,
            kind,
            detail: redact(detail),
            timestamp_ms: now_ms(),
            prev_digest: previous,
            digest: String::new(),
        };
        event.digest = digest_hex(&event)?;
        if let Some(path) = &self.path {
            append_jsonl(path, &event)?;
        }
        self.events.push(event);
        Ok(self.events.last().expect("刚 push 过"))
    }

    /// 逐条重算摘要，并核对前后链接。
    pub fn verify_chain(&self) -> Result<(), AuditError> {
        let mut previous = GENESIS.to_owned();
        for event in &self.events {
            if event.prev_digest != previous {
                return Err(AuditError::ChainBroken {
                    seq: event.seq,
                    reason: "prev_digest 与前一条摘要不符".into(),
                });
            }
            if digest_hex(event)? != event.digest {
                return Err(AuditError::ChainBroken {
                    seq: event.seq,
                    reason: "事件内容被改动".into(),
                });
            }
            previous = event.digest.clone();
        }
        Ok(())
    }

    /// 按符印回放：一次仪轨的全部事件。
    pub fn replay(&self, seal: &str) -> Vec<&AuditEvent> {
        self.events
            .iter()
            .filter(|event| event.seal == seal)
            .collect()
    }

    /// 导出为 JSONL（导出文件可覆盖；运行时日志本身始终 append-only）。
    pub fn export_jsonl(&self, path: &Path) -> Result<usize, AuditError> {
        let file = std::fs::File::create(path).map_err(|e| AuditError::Io(e.to_string()))?;
        let mut writer = BufWriter::new(file);
        for event in &self.events {
            let line =
                serde_json::to_string(event).map_err(|e| AuditError::Serialize(e.to_string()))?;
            writeln!(writer, "{line}").map_err(|e| AuditError::Io(e.to_string()))?;
        }
        writer.flush().map_err(|e| AuditError::Io(e.to_string()))?;
        Ok(self.events.len())
    }
}

fn append_jsonl(path: &Path, event: &AuditEvent) -> Result<(), AuditError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AuditError::Io(e.to_string()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| AuditError::Io(e.to_string()))?;
    let mut writer = BufWriter::new(file);
    let line = serde_json::to_string(event).map_err(|e| AuditError::Serialize(e.to_string()))?;
    writeln!(writer, "{line}").map_err(|e| AuditError::Io(e.to_string()))?;
    writer.flush().map_err(|e| AuditError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> AuditLog {
        let mut log = AuditLog::new();
        log.append("seal-1", TalismanState::Formed, EventKind::Formed, "成符")
            .unwrap();
        log.append("seal-1", TalismanState::Sealed, EventKind::Sealed, "盖印")
            .unwrap();
        log.append("seal-1", TalismanState::Pending, EventKind::DryRun, "待焚")
            .unwrap();
        log
    }

    #[test]
    fn chain_is_linked_and_verifiable() {
        let log = log();
        assert_eq!(log.len(), 3);
        log.verify_chain().unwrap();
        assert_eq!(log.events()[0].prev_digest, GENESIS);
        assert_eq!(log.events()[1].prev_digest, log.events()[0].digest);
    }

    #[test]
    fn tampering_with_detail_breaks_the_chain() {
        let mut log = log();
        log.events[1].detail = "被事后改写的盖印记录".into();
        assert!(matches!(
            log.verify_chain(),
            Err(AuditError::ChainBroken { seq: 1, .. })
        ));
    }

    #[test]
    fn replay_filters_by_seal() {
        let mut log = log();
        log.append(
            "seal-2",
            TalismanState::Formed,
            EventKind::Formed,
            "另一张符",
        )
        .unwrap();
        assert_eq!(log.replay("seal-1").len(), 3);
        assert_eq!(log.replay("seal-2").len(), 1);
    }

    #[test]
    fn sensitive_literals_never_reach_the_log() {
        let mut log = AuditLog::new();
        let event = log
            .append(
                "seal",
                TalismanState::Draft,
                EventKind::Invoked,
                "Authorization: Bearer sk-proj-abcdefghijklmnop api_key=12345 password=hunter2",
            )
            .unwrap()
            .clone();
        assert!(!event.detail.contains("sk-proj"));
        assert!(!event.detail.contains("12345"));
        assert!(!event.detail.contains("hunter2"));
        assert!(event.detail.contains("[REDACTED]"));
    }

    #[test]
    fn redact_keeps_utf8_intact() {
        let text = "口令：符箓台 token=abc123 完成";
        let redacted = redact(text);
        assert!(redacted.contains("符箓台"));
        assert!(redacted.contains("完成"));
        assert!(!redacted.contains("abc123"));
    }

    #[test]
    fn append_only_file_keeps_previous_lines() {
        let path = std::env::temp_dir().join(format!(
            "fulu-audit-m2-{}-{}.jsonl",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        {
            let mut first = AuditLog::with_path(&path);
            first
                .append("s", TalismanState::Formed, EventKind::Formed, "第一次")
                .unwrap();
        }
        {
            let mut second = AuditLog::with_path(&path);
            second
                .append("s", TalismanState::Sealed, EventKind::Sealed, "第二次")
                .unwrap();
        }
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 2, "第二次打开不得清空已有日志");
        let _ = std::fs::remove_file(&path);
    }
}
