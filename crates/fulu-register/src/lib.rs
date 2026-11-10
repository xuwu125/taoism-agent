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

/// 工具的副作用等级。
///
/// 这一维与 [`Permission`] **正交**：`Permission` 决定「这张符最多能碰到什么」，
/// `RiskClass` 决定「碰到之前要不要人点头」。两者都校验，缺一不可——
/// 一个已受箓的 `stop_process` 在权限上完全合法，但仍然不该在无人复核时静默执行。
///
/// 外宗（MCP）工具的这一维来自 server 自述的 annotations，因此只用于**收紧**
/// 确认要求，绝不用来放宽权限：外宗工具永远停在 `Permission::Limited`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// 只读观察，无副作用
    Observe,
    /// 会改变状态，但可重放/可回滚（起进程、写文件、调接口）
    Mutate,
    /// 破坏性且不可逆（删数据、杀进程、改系统配置）
    Destructive,
}

impl RiskClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Observe => "只读观察",
            Self::Mutate => "可变更",
            Self::Destructive => "破坏性",
        }
    }

    /// 该等级要求的确认方式。
    pub fn confirmation(self) -> Confirmation {
        match self {
            Self::Observe => Confirmation::None,
            Self::Mutate => Confirmation::OncePerTalisman,
            Self::Destructive => Confirmation::EveryCall,
        }
    }
}

/// 一次召将需要多少次人的点头。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    /// 无需确认
    None,
    /// 同一张符里该工具只需确认一次
    OncePerTalisman,
    /// 每次调用都要确认
    EveryCall,
}

impl Confirmation {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "免确认",
            Self::OncePerTalisman => "每符确认一次",
            Self::EveryCall => "每次确认",
        }
    }
}

/// 无参工具的参数描述：合法 JSON Schema，模型据此知道「不需要参数」。
pub const EMPTY_PARAMETERS_JSON: &str = r#"{"type":"object","properties":{}}"#;

fn empty_parameters_json() -> String {
    EMPTY_PARAMETERS_JSON.to_owned()
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
    /// 交给模型的参数 JSON Schema。外宗（MCP）工具用它携带 server 声明的
    /// 真实 `inputSchema`，否则模型只能瞎猜参数名。
    #[serde(default = "empty_parameters_json")]
    pub parameters_json: String,
    /// 副作用等级：决定召将前是否需要人确认。缺省取最严的 `Destructive`
    /// ——没写清楚的工具按「高危」处理，不按「无害」处理。
    #[serde(default = "destructive_risk")]
    pub risk: RiskClass,
}

fn destructive_risk() -> RiskClass {
    RiskClass::Destructive
}

impl ToolGrant {
    /// 只读工具：权限 `ReadOnly`、风险 `Observe`。
    pub fn read_only(id: &str, description: &str) -> Self {
        Self {
            id: id.to_owned(),
            description: description.to_owned(),
            permission: Permission::ReadOnly,
            allowed_paths: Vec::new(),
            allowed_domains: Vec::new(),
            needs_network: false,
            max_calls: 8,
            parameters_json: empty_parameters_json(),
            risk: RiskClass::Observe,
        }
    }

    /// 受限级工具的基础模板：权限 `Limited`，风险留待调用方声明。
    /// 用途是外宗（MCP）工具登记——它们一律受限，风险由 annotations 决定。
    pub fn limited(id: &str, description: &str) -> Self {
        let mut grant = Self::read_only(id, description);
        grant.permission = Permission::Limited;
        grant
    }

    /// 带上副作用等级（builder 风格）。
    #[must_use]
    pub fn with_risk(mut self, risk: RiskClass) -> Self {
        self.risk = risk;
        self
    }

    /// 带上参数 schema（builder 风格，便于外宗工具登记时链式书写）。
    ///
    /// 这里只做形状检查（必须是一对大括号包住的 JSON 对象），不引入 JSON 解析器：
    /// 箓层保持「只存授权事实、不解释内容」的定位，真正的解析在装配时完成，
    /// 解析失败一律退回空参数，绝不把脏串塞给模型。
    #[must_use]
    pub fn with_parameters_json(mut self, schema: &str) -> Self {
        let trimmed = schema.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            self.parameters_json = trimmed.to_owned();
        }
        self
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
    /// 符的沙箱等级低于工具要求的最低权限（如只读符申请敕行）
    PermissionTooLow {
        tool: String,
        requested: Permission,
        required: Permission,
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
            Self::PermissionTooLow {
                tool,
                requested,
                required,
            } => write!(
                f,
                "工具 {tool} 需要 {} 及以上权限，符只申请了 {}，权限不足驳回",
                required.label(),
                requested.label()
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
    /// 每个已受箓工具的副作用等级，供法坛在召将前决定是否请人确认。
    /// 随授权一起下发，避免执行层「忘记查名册」而静默执行高危工具。
    pub risks: BTreeMap<String, RiskClass>,
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

    /// 该工具需要哪种确认。查不到就按最严处理（每次确认），不按免确认处理。
    pub fn confirmation_for(&self, tool: &str) -> Confirmation {
        self.risks
            .get(tool)
            .copied()
            .unwrap_or(RiskClass::Destructive)
            .confirmation()
    }

    /// 本次授权里需要人确认的工具（按危险程度降序），供 dry-run 展示。
    pub fn tools_needing_confirmation(&self) -> Vec<(String, RiskClass)> {
        let mut list: Vec<(String, RiskClass)> = self
            .allowed_tools
            .iter()
            .filter_map(|id| {
                let risk = self
                    .risks
                    .get(id)
                    .copied()
                    .unwrap_or(RiskClass::Destructive);
                (risk.confirmation() != Confirmation::None).then(|| (id.clone(), risk))
            })
            .collect();
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        list
    }
}

#[derive(Debug, Clone, Default)]
pub struct Register {
    grants: BTreeMap<String, ToolGrant>,
    granted_credentials: BTreeSet<String>,
}

impl Register {
    /// 出厂名册：本地模型无需密钥；登记两个只读工具与「受限执行」级敕行，
    /// 默认不开放任何网络能力。敕行虽已登记，只读符（Strict）申请它仍会被
    /// 越权驳回——登记 ≠ 可用，可用上限由符申请的沙箱等级决定。
    pub fn baseline() -> Self {
        let mut register = Self::new();
        register
            .granted_credentials
            .insert("lu://default/local".into());
        register.grant_tool(
            ToolGrant::read_only("fs.read", "读取工作区内的文本文件").with_parameters_json(
                r#"{"type":"object","properties":{"path":{"type":"string","description":"相对工作区根目录的路径"}},"required":["path"]}"#,
            ),
        );
        register.grant_tool(
            ToolGrant::read_only("echo", "回显参数，无副作用").with_parameters_json(
                r#"{"type":"object","properties":{"text":{"type":"string"}}}"#,
            ),
        );
        let shell = ToolGrant::limited("shell.exec", "分 OS 命令行执行（受限级，工作区内）")
            .with_risk(RiskClass::Destructive)
            .with_parameters_json(
                r#"{"type":"object","properties":{"command":{"type":"string","description":"单条命令，工作区内执行"}},"required":["command"]}"#,
            );
        register.grant_tool(shell);
        // T17：受控写入。能力弱于敕行（单次 256 KiB、覆盖前自动备份），
        // 定级 Mutate（每符确认一次）；Strict 符申请仍会被双向核验驳回。
        register.grant_tool(
            ToolGrant::limited("fs.write", "写工作区内文件（覆盖前自动备份 .fulu-bak）")
                .with_risk(RiskClass::Mutate)
                .with_parameters_json(
                    r#"{"type":"object","properties":{"path":{"type":"string","description":"相对工作区根目录的路径"},"content":{"type":"string","description":"要写入的文本（≤256KiB）"},"append":{"type":"boolean","description":"true 时追加而非覆盖"}},"required":["path","content"]}"#,
                ),
        );
        // T15：问天。工具本体不联网（无后端时明确报错）；后端是搜索类外宗
        // MCP 工具，代调网络能力 → 风险定级 Destructive（每次确认）：
        // 出网 + 外宗内容不可信，比敕行更严。
        let mut ask = ToolGrant::limited("ask.search", "问天搜索（可插拔后端，默认外宗 MCP）")
            .with_risk(RiskClass::Destructive)
            .with_parameters_json(
                r#"{"type":"object","properties":{"query":{"type":"string","description":"搜索关键词"},"limit":{"type":"integer","description":"返回条数（1-20，默认 5）"}},"required":["query"]}"#,
            );
        ask.max_calls = 8;
        register.grant_tool(ask);
        // T14：moli。网络发生在外部无头浏览器进程里，fulu 自身不链接 TLS；
        // 出网 + 模型选 URL（可携带数据外泄）→ Destructive 逐次确认。
        let mut moli = ToolGrant::limited(
            "moli.open",
            "moli 浏览器：无头打开网页并抽取正文文本（外部 Edge/Chrome，不嵌 WebView）",
        )
        .with_risk(RiskClass::Destructive)
        .with_parameters_json(
            r#"{"type":"object","properties":{"url":{"type":"string","description":"要打开的 http(s) 网址"},"wait_ms":{"type":"integer","description":"JS 渲染等待毫秒（0-15000，默认 3000）"}},"required":["url"]}"#,
        );
        moli.max_calls = 8;
        register.grant_tool(moli);
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

    /// 已登记的工具名（字典序），供展示与测试。
    pub fn tool_ids(&self) -> Vec<String> {
        self.grants.keys().cloned().collect()
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
        let mut risks = BTreeMap::new();
        for tool_id in &fu.tool_refs {
            let grant = self
                .grants
                .get(tool_id)
                .ok_or_else(|| RegisterError::ToolNotInRegister(tool_id.clone()))?;
            // 双向核验：符的沙箱等级必须与工具授予等级匹配——
            // 高于授予 = 越权（SandboxEscalation）；低于要求 = 权限不足
            // （PermissionTooLow，例如只读符申请敕行）。
            if requested > grant.permission {
                return Err(RegisterError::SandboxEscalation {
                    tool: tool_id.clone(),
                    requested,
                    granted: grant.permission,
                });
            }
            if requested < grant.permission {
                return Err(RegisterError::PermissionTooLow {
                    tool: tool_id.clone(),
                    requested,
                    required: grant.permission,
                });
            }
            if grant.needs_network && !fu.harness.network {
                return Err(RegisterError::NetworkNotGranted(tool_id.clone()));
            }
            if grant.max_calls == 0 {
                return Err(RegisterError::QuotaNotPositive(tool_id.clone()));
            }
            max_calls.insert(tool_id.clone(), grant.max_calls);
            risks.insert(tool_id.clone(), grant.risk);
        }

        Ok(Authorization {
            credential_ref: fu.credential_ref.clone(),
            sandbox: fu.harness.sandbox,
            allowed_tools: fu.tool_refs.clone(),
            max_calls,
            risks,
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
        fu.tool_refs = vec!["time.travel".into()];
        fu.harness.allow_tools = vec!["time.travel".into()];
        assert_eq!(
            Register::baseline().authorize(&fu),
            Err(RegisterError::ToolNotInRegister("time.travel".into()))
        );
    }

    #[test]
    fn shell_is_registered_but_read_only_talisman_lacks_permission() {
        // 敕行已登记（受限级），但只读符权限不足，双向核验都要拦。
        let mut fu = Talisman::sample();
        fu.tool_refs = vec!["shell.exec".into()];
        fu.harness.allow_tools = vec!["shell.exec".into()];
        assert!(matches!(
            Register::baseline().authorize(&fu),
            Err(RegisterError::PermissionTooLow { .. })
        ));
    }

    #[test]
    fn shell_grants_to_limited_sandbox_talisman() {
        let mut fu = Talisman::sample();
        fu.harness.sandbox = Sandbox::Limited;
        fu.tool_refs = vec!["shell.exec".into()];
        fu.harness.allow_tools = vec!["shell.exec".into()];
        let auth = Register::baseline().authorize(&fu).unwrap();
        assert_eq!(auth.max_calls.get("shell.exec"), Some(&8));
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

    #[test]
    fn grants_carry_parameter_schema_and_reject_garbage() {
        let register = Register::baseline();
        let fs_read = register.tool("fs.read").unwrap();
        assert!(fs_read.parameters_json.contains("\"required\":[\"path\"]"));

        // 未声明参数的工具仍必须给出合法空 schema，而不是空串。
        let plain = ToolGrant::read_only("mcp.x.y", "外宗工具");
        assert_eq!(plain.parameters_json, EMPTY_PARAMETERS_JSON);

        // 脏串被拒，保持原值不变。
        let kept = plain.clone().with_parameters_json("not json at all");
        assert_eq!(kept.parameters_json, EMPTY_PARAMETERS_JSON);
        let adopted = plain.with_parameters_json(r#"{"type":"object"}"#);
        assert_eq!(adopted.parameters_json, r#"{"type":"object"}"#);
    }

    #[test]
    fn baseline_declares_risk_per_tool() {
        let register = Register::baseline();
        // 只读工具免确认；敕行能删文件，必须按破坏性处理。
        assert_eq!(register.tool("fs.read").unwrap().risk, RiskClass::Observe);
        assert_eq!(register.tool("echo").unwrap().risk, RiskClass::Observe);
        assert_eq!(
            register.tool("shell.exec").unwrap().risk,
            RiskClass::Destructive
        );
    }

    #[test]
    fn grants_without_declared_risk_fall_back_to_destructive() {
        // 老配置 / 外部写入的箓牒没有 risk 字段，必须按最严处理，
        // 而不是「没写就当无害」。
        let grant: ToolGrant = serde_json::from_str(
            r#"{"id":"legacy","description":"老箓牒","permission":"limited",
                "allowed_paths":[],"allowed_domains":[],"needs_network":false,"max_calls":4}"#,
        )
        .unwrap();
        assert_eq!(grant.risk, RiskClass::Destructive);
        assert_eq!(grant.parameters_json, EMPTY_PARAMETERS_JSON);
    }

    #[test]
    fn authorization_carries_confirmation_requirements() {
        let mut limited = Talisman::sample();
        limited.harness.sandbox = Sandbox::Limited;
        limited.tool_refs = vec!["shell.exec".into()];
        limited.harness.allow_tools = limited.tool_refs.clone();
        let auth = Register::baseline().authorize(&limited).unwrap();
        assert_eq!(auth.confirmation_for("shell.exec"), Confirmation::EveryCall);
        assert_eq!(
            auth.tools_needing_confirmation(),
            vec![("shell.exec".to_owned(), RiskClass::Destructive)]
        );

        // 只读符：fs.read 免确认，待焚计划里不该出现「需确认」噪音。
        let strict = Register::baseline().authorize(&Talisman::sample()).unwrap();
        assert_eq!(strict.confirmation_for("fs.read"), Confirmation::None);
        assert!(strict.tools_needing_confirmation().is_empty());

        // 查不到的工具按最严处理，不给「漏登记 = 免确认」留口子。
        assert_eq!(
            strict.confirmation_for("no.such.tool"),
            Confirmation::EveryCall
        );
    }
}
