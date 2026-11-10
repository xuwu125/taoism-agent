//! 箓层（Register）：神将名册、授箓与执行前核验（docs/02 第 3.2 节）。
//!
//! 「箓」是**授权名册**，不是提示词。符里只写工具引用与 `credential_ref`，
//! 真正的能力上限、路径/域名边界、调用配额都登记在这里；执行前核验不过即「驳回」。
//!
//! 红线：本模块**不保存任何密钥明文**，只保存「哪个引用 ID 已受箓」。
//! 系统凭据库（Windows Credential Manager / Keychain / Secret Service）的接入属 M3。

use fulu_core::{Sandbox, Talisman};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// 权限等级，与符的 `Sandbox` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadOnly,
    Limited,
    Full,
}

impl Permission {
    pub fn from_sandbox(sandbox: Sandbox) -> Self {
        match sandbox {
            Sandbox::Strict => Self::ReadOnly,
            Sandbox::Limited => Self::Limited,
            Sandbox::Full => Self::Full,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "只读",
            Self::Limited => "受限执行",
            Self::Full => "完全执行",
        }
    }
}

/// 一条箓牒：工具的授予上限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolGrant {
    pub id: String,
    pub description: String,
    pub permission: Permission,
    /// 允许的目录前缀标识（具体路径解析由 `fulu-tools` 完成）
    pub allowed_paths: Vec<String>,
    pub allowed_domains: Vec<String>,
    pub needs_network: bool,
    pub max_calls: u32,
}

impl ToolGrant {
    pub fn read_only(id: &str, description: &str) -> Self {
        Self {
            id: id.to_owned(),
            description: description.to_owned(),
            permission: Permission::ReadOnly,
            allowed_paths: Vec::new(),
            allowed_domains: Vec::new(),
            needs_network: false,
            max_calls: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    InvalidTalisman(String),
    ToolNotInRegister(String),
    /// 符申请的沙箱等级高于名册授予的能力上限
    SandboxEscalation {
        tool: String,
        requested: Permission,
        granted: Permission,
    },
    CredentialNotGranted(String),
    NetworkNotGranted(String),
    QuotaNotPositive(String),
    QuotaExceeded {
        tool: String,
        max_calls: u32,
    },
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTalisman(msg) => write!(f, "符文本身不合法：{msg}"),
            Self::ToolNotInRegister(id) => write!(f, "工具 {id} 未受箓，禁止召将"),
            Self::SandboxEscalation {
                tool,
                requested,
                granted,
            } => write!(
                f,
                "工具 {tool} 申请 {} 权限，但名册只授予 {}，越权驳回",
                requested.label(),
                granted.label()
            ),
            Self::CredentialNotGranted(id) => write!(f, "敕令 {id} 未受箓，拒绝访问"),
            Self::NetworkNotGranted(id) => {
                write!(f, "工具 {id} 需要网络，但符的捆仙绳未开放 network")
            }
            Self::QuotaNotPositive(id) => write!(f, "工具 {id} 的配额必须 > 0"),
            Self::QuotaExceeded { tool, max_calls } => {
                write!(f, "工具 {tool} 调用次数已达上限 {max_calls}")
            }
        }
    }
}

impl std::error::Error for RegisterError {}

/// 一次核验的产物：交给风火轮执行层的最小权限集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    pub credential_ref: String,
    pub sandbox: Sandbox,
    pub allowed_tools: Vec<String>,
    pub max_calls: BTreeMap<String, u32>,
    pub network: bool,
}

impl Authorization {
    /// 执行前逐次复核；调用计数由 `fulu-tools` 维护。
    pub fn check_call(&self, tool: &str, used: u32) -> Result<(), RegisterError> {
        if !self.allowed_tools.iter().any(|id| id == tool) {
            return Err(RegisterError::ToolNotInRegister(tool.to_owned()));
        }
        let limit = self
            .max_calls
            .get(tool)
            .copied()
            .ok_or_else(|| RegisterError::ToolNotInRegister(tool.to_owned()))?;
        if used >= limit {
            return Err(RegisterError::QuotaExceeded {
                tool: tool.to_owned(),
                max_calls: limit,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Register {
    grants: BTreeMap<String, ToolGrant>,
    granted_credentials: BTreeSet<String>,
}

impl Register {
    /// 出厂名册：本地模型无需密钥；只登记两个只读工具，默认不开放任何网络能力。
    pub fn baseline() -> Self {
        let mut register = Self::new();
        register
            .granted_credentials
            .insert("lu://default/local".into());
        register.grant_tool(ToolGrant::read_only("fs.read", "读取工作区内的文本文件"));
        register.grant_tool(ToolGrant::read_only("echo", "回显参数，无副作用"));
        register
    }

    pub fn new() -> Self {
        Self {
            grants: BTreeMap::new(),
            granted_credentials: BTreeSet::new(),
        }
    }

    pub fn grant_tool(&mut self, grant: ToolGrant) {
        self.grants.insert(grant.id.clone(), grant);
    }

    pub fn grant_credential(&mut self, credential_ref: &str) {
        self.granted_credentials.insert(credential_ref.to_owned());
    }

    pub fn tool(&self, id: &str) -> Option<&ToolGrant> {
        self.grants.get(id)
    }

    /// 盖印前的「授箓核验」。不通过即驳回，绝不带着越权进入法坛。
    pub fn authorize(&self, fu: &Talisman) -> Result<Authorization, RegisterError> {
        let problems = fu.validate();
        if !problems.is_empty() {
            return Err(RegisterError::InvalidTalisman(problems.to_string()));
        }
        if !self.granted_credentials.contains(&fu.credential_ref) {
            return Err(RegisterError::CredentialNotGranted(
                fu.credential_ref.clone(),
            ));
        }

        let requested = Permission::from_sandbox(fu.harness.sandbox);
        let mut max_calls = BTreeMap::new();
        for tool_id in &fu.tool_refs {
            let grant = self
                .grants
                .get(tool_id)
                .ok_or_else(|| RegisterError::ToolNotInRegister(tool_id.clone()))?;
            if requested > grant.permission {
                return Err(RegisterError::SandboxEscalation {
                    tool: tool_id.clone(),
                    requested,
                    granted: grant.permission,
                });
            }
            if grant.needs_network && !fu.harness.network {
                return Err(RegisterError::NetworkNotGranted(tool_id.clone()));
            }
            if grant.max_calls == 0 {
                return Err(RegisterError::QuotaNotPositive(tool_id.clone()));
            }
            max_calls.insert(tool_id.clone(), grant.max_calls);
        }

        Ok(Authorization {
            credential_ref: fu.credential_ref.clone(),
            sandbox: fu.harness.sandbox,
            allowed_tools: fu.tool_refs.clone(),
            max_calls,
            network: fu.harness.network,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_authorizes_the_sample_talisman() {
        let auth = Register::baseline().authorize(&Talisman::sample()).unwrap();
        assert_eq!(auth.credential_ref, "lu://default/local");
        assert_eq!(auth.allowed_tools, vec!["fs.read"]);
        assert!(!auth.network);
        assert_eq!(auth.sandbox, Sandbox::Strict);
    }

    #[test]
    fn unregistered_tool_is_rejected() {
        let mut fu = Talisman::sample();
        fu.tool_refs = vec!["shell.exec".into()];
        fu.harness.allow_tools = vec!["shell.exec".into()];
        assert_eq!(
            Register::baseline().authorize(&fu),
            Err(RegisterError::ToolNotInRegister("shell.exec".into()))
        );
    }

    #[test]
    fn sandbox_escalation_is_rejected() {
        let mut fu = Talisman::sample();
        fu.harness.sandbox = Sandbox::Full;
        assert!(matches!(
            Register::baseline().authorize(&fu),
            Err(RegisterError::SandboxEscalation { .. })
        ));
    }

    #[test]
    fn cloud_credential_must_be_granted_explicitly() {
        let mut fu = Talisman::sample();
        fu.head.provider_ref = "lu://cloud/openai".into();
        fu.credential_ref = "lu://default/openai-key".into();
        assert!(matches!(
            Register::baseline().authorize(&fu),
            Err(RegisterError::CredentialNotGranted(_))
        ));
    }

    #[test]
    fn network_tool_requires_network_flag() {
        let mut register = Register::baseline();
        let mut grant = ToolGrant::read_only("http.get", "发起 HTTP 请求");
        grant.needs_network = true;
        register.grant_tool(grant);
        let mut fu = Talisman::sample();
        fu.tool_refs = vec!["http.get".into()];
        fu.harness.allow_tools = vec!["http.get".into()];
        assert_eq!(
            register.authorize(&fu),
            Err(RegisterError::NetworkNotGranted("http.get".into()))
        );
        fu.harness.network = true;
        assert!(register.authorize(&fu).is_ok());
    }

    #[test]
    fn call_quota_is_enforced() {
        let auth = Register::baseline().authorize(&Talisman::sample()).unwrap();
        assert!(auth.check_call("fs.read", 7).is_ok());
        assert_eq!(
            auth.check_call("fs.read", 8),
            Err(RegisterError::QuotaExceeded {
                tool: "fs.read".into(),
                max_calls: 8
            })
        );
        assert_eq!(
            auth.check_call("shell.exec", 0),
            Err(RegisterError::ToolNotInRegister("shell.exec".into()))
        );
    }
}
