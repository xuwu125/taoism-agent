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
    /// `fs.read` 的唯一根目录；None 表示文件能力关闭。
    pub root: Option<PathBuf>,
    pub allowed_domains: Vec<String>,
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
        let relative = Path::new(&raw);
        if relative.is_absolute() {
            return Err(ToolError::PathForbidden(format!("不允许绝对路径：{raw}")));
        }
        if relative
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::RootDir))
        {
            return Err(ToolError::PathForbidden(format!("不允许目录穿越：{raw}")));
        }
        let full = root.join(relative);
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
        Self { tools, policy }
    }

    pub fn policy(&self) -> &ToolPolicy {
        &self.policy
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.id().to_owned(), tool);
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
}
