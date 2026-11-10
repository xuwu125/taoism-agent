//! 用例门面：GUI 与 CLI **必须**共用这里，避免两端行为漂移（docs/02 第 9.2 节）。
//!
//! M0 只有只读体检；M2 起提供完整仪轨会话 `AltarSession`：
//! 环境体检、符文加载、dry-run、显式确认后焚化、缴符导出与本地服务探活。
//! UI 层不允许直接碰 `fulu-runtime`，全部经此门面。

use fulu_audit::AuditLog;
use fulu_core::Talisman;
use fulu_mcp::{McpServerInfo, RiskOverrides, RiskSource};
use fulu_model::{ModelManifest, VerifyStatus, model_path, status, verify_file};
use fulu_provider::{Health, LocalLfmProvider};
use fulu_register::{Register, ToolGrant};

/// 风险等级对外暴露：CLI/GUI 需要它来解析 `--allow-risk` 与渲染勾选项。
pub use fulu_register::RiskClass;
use fulu_runtime::{AllowUpTo, Altar, Confirmer, RitualConfig, RitualError};
use fulu_script::{decode_fu, encode_fu, seal_hex};
use fulu_tools::{Tool, ToolPolicy, ToolRegistry};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 门面重导出：UI 只依赖 `fulu-app`，不直连 `fulu-core` / `fulu-runtime`。
/// 契约是「UI 不构造运行时对象」，持有门面返回的值是允许的。
pub use fulu_core::Talisman as Fu;
pub use fulu_runtime::{
    ConfirmRequest, ConfirmVerdict, Confirmer as Gatekeeper, DryRunPlan, Receipt,
};

/// 本地推理服务端点环境变量；未设置时 provider 处于「未就绪」，不会静默联网。
pub const ENDPOINT_ENV: &str = "FULU_LLAMA_ENDPOINT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    Manifest(String),
    Script(String),
    Ritual(String),
    Probe(String),
    Io(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(msg) => write!(f, "模型清单：{msg}"),
            Self::Script(msg) => write!(f, "符文：{msg}"),
            Self::Ritual(msg) => write!(f, "法坛：{msg}"),
            Self::Probe(msg) => write!(f, "本地推理服务：{msg}"),
            Self::Io(msg) => write!(f, "I/O：{msg}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<RitualError> for AppError {
    fn from(error: RitualError) -> Self {
        Self::Ritual(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub default_slot: String,
    pub model_id: String,
    pub repo: String,
    pub license: String,
    pub min_runtime: String,
    pub approx_size: String,
    pub context_tokens: u32,
    pub engine: String,
    pub bind: String,
    pub weights_path: String,
    pub model_status: String,
    pub manifest_issues: Vec<String>,
    pub compliance_warning: String,
    pub provider_stage: String,
}

impl DoctorReport {
    /// 单行摘要，供 GUI 状态栏使用。
    pub fn summary(&self) -> String {
        format!(
            "{} / {} / 权重 {} / 清单问题 {} 项",
            self.model_id,
            self.license,
            self.model_status,
            self.manifest_issues.len()
        )
    }
}

/// 环境体检：不联网、不加载权重、不启动推理进程。
pub fn doctor() -> Result<DoctorReport, AppError> {
    let manifest = ModelManifest::embedded().map_err(|e| AppError::Manifest(e.to_string()))?;
    let issues = manifest.validate();
    let default = manifest
        .default_entry()
        .map_err(|e| AppError::Manifest(e.to_string()))?;
    let path = model_path(default);
    let model_status = if path.exists() {
        match verify_file(&path, default) {
            Ok(verification) => match &verification.status {
                VerifyStatus::Verified => "VERIFIED".into(),
                VerifyStatus::Unverifiable(reason) => format!("UNVERIFIED（{reason}）"),
                VerifyStatus::Mismatch(reason) => format!("MISMATCH（{reason}）"),
            },
            Err(error) => format!("UNREADABLE（{error}）"),
        }
    } else {
        "MISSING（尚未导入权重；轻量包默认不含权重）".into()
    };

    Ok(DoctorReport {
        default_slot: default.slot.clone(),
        model_id: default.model_id.clone(),
        repo: default.repo.clone(),
        license: format!("{} ({})", default.license, default.license_spdx),
        min_runtime: default.min_runtime.clone(),
        approx_size: default.approx_size.clone(),
        context_tokens: default.default_context_tokens,
        engine: manifest.policy.engine.clone(),
        bind: manifest.policy.bind.clone(),
        weights_path: path.to_string_lossy().into_owned(),
        model_status,
        manifest_issues: issues,
        compliance_warning:
            "权重为 LFM Open License v1.0，不是 MIT；商业化前复核年营收 1000 万美元门槛与分发义务。"
                .into(),
        provider_stage:
            "M2 · 本地 llama-server（OpenAI 兼容 HTTP）已实现；端点由 FULU_LLAMA_ENDPOINT 指定"
                .into(),
    })
}

/// 使用编译期清单查看某槽位的状态（供 CLI `model list` 复用）。
pub fn model_status(slot: &str) -> Result<String, AppError> {
    let manifest = ModelManifest::embedded().map_err(|e| AppError::Manifest(e.to_string()))?;
    let entry = manifest
        .entry(slot)
        .ok_or_else(|| AppError::Manifest(format!("未知槽位：{slot}")))?;
    Ok(status(entry).label())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuReport {
    pub name: String,
    pub model_id: String,
    pub seal: String,
    pub valid: bool,
}

/// 解析并校验一张符；不合格的符不会返回报告，而是直接「破符」。
pub fn inspect_fu(text: &str) -> Result<FuReport, AppError> {
    let fu = decode_fu(text).map_err(|e| AppError::Script(e.to_string()))?;
    let seal = seal_hex(&fu).map_err(|e| AppError::Script(e.to_string()))?;
    Ok(FuReport {
        name: fu.name.clone(),
        model_id: fu.deity.model_id.clone(),
        seal,
        valid: fu.validate().is_empty(),
    })
}

pub fn sample_fu_text() -> Result<String, AppError> {
    encode_fu(&Talisman::sample()).map_err(|e| AppError::Script(e.to_string()))
}

/// 从文本解析一张符：GUI 编辑器里的内容走这条，不落盘也能校验。
pub fn parse_fu(text: &str) -> Result<Talisman, AppError> {
    decode_fu(text).map_err(|e| AppError::Script(e.to_string()))
}

/// 从 `.fu` 文件加载；解析阶段即做结构校验，破符绝不进入法坛。
pub fn load_fu(path: &Path) -> Result<Talisman, AppError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| AppError::Io(format!("{}：{e}", path.display())))?;
    decode_fu(&text).map_err(|e| AppError::Script(e.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub endpoint: String,
    pub ready: bool,
    pub detail: String,
    pub models: Vec<String>,
}

/// 外宗（MCP）一台 server 的接入结果。
///
/// 「只展示不管理」的落地形态：fulu 负责发现、握手、把工具挂进箓，
/// 但**不写**任何全局配置、不装不升级不删。连不上就如实记账，不猜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OuterSectServer {
    pub name: String,
    /// 传输描述（`stdio: node x.js` / `http: http://...`），不含认证头值。
    pub transport: String,
    pub connected: bool,
    pub tools: Vec<OuterSectTool>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

/// 一个外宗工具在箓里的登记结果：名字 + 风险等级。
///
/// 风险等级单独带出来，是因为它是**唯一**能让用户判断「这台 server 接进来
/// 会给我带来什么后果」的信息——只报工具数量等于什么都没说。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OuterSectTool {
    /// 箓登记名：`mcp.<server>.<tool>`
    pub id: String,
    pub risk: RiskClass,
    /// 这个等级是谁定的：server 自述 / fulu 覆盖表 / 未声明 / 策略下限
    pub risk_source: RiskSource,
    /// server 自述只读（仅供展示，不构成授权依据）
    pub declared_read_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OuterSectReport {
    pub servers: Vec<OuterSectServer>,
    /// 配置发现阶段的问题（非法 JSON、缺字段等），与连接失败分开记。
    pub issues: Vec<String>,
}

impl OuterSectReport {
    pub fn tool_ids(&self) -> Vec<String> {
        self.servers
            .iter()
            .flat_map(|server| server.tools.iter().map(|tool| tool.id.clone()))
            .collect()
    }

    /// 需要人确认的外宗工具（破坏性 / 可变更），按危险程度降序。
    pub fn gated_tools(&self) -> Vec<OuterSectTool> {
        let mut gated: Vec<OuterSectTool> = self
            .servers
            .iter()
            .flat_map(|server| server.tools.iter())
            .filter(|tool| tool.risk != RiskClass::Observe)
            .cloned()
            .collect();
        gated.sort_by(|a, b| b.risk.cmp(&a.risk).then_with(|| a.id.cmp(&b.id)));
        gated
    }

    pub fn connected_count(&self) -> usize {
        self.servers
            .iter()
            .filter(|server| server.connected)
            .count()
    }

    pub fn summary(&self) -> String {
        if self.servers.is_empty() {
            return "外宗：未发现可接入的 MCP server".into();
        }
        let gated = self.gated_tools().len();
        format!(
            "外宗：{}/{} 台已接入，共 {} 个工具（其中 {} 个需人确认）",
            self.connected_count(),
            self.servers.len(),
            self.tool_ids().len(),
            gated
        )
    }
}

/// 一场仪轨的完整会话。provider / 箓 / 风火轮 / 缴符都在这里组装，
/// GUI 与 CLI 各自只负责展示与收集「确认」。
pub struct AltarSession {
    provider: LocalLfmProvider,
    register: Register,
    tools: ToolRegistry,
    audit: AuditLog,
    outer_sect: OuterSectReport,
    /// 本次会话的确认器；`None` = 一律敕否（fail closed）
    confirmer: Option<Box<dyn Confirmer>>,
}

impl Default for AltarSession {
    fn default() -> Self {
        Self::new()
    }
}

impl AltarSession {
    /// 默认端点为空（未就绪），文件工具根目录也为空（关闭）。
    pub fn new() -> Self {
        let manifest = ModelManifest::embedded().expect("内嵌清单必须可解析");
        let entry = manifest
            .default_entry()
            .expect("内嵌清单必须有唯一默认槽位")
            .clone();
        Self {
            provider: LocalLfmProvider::from_model(
                &entry.model_id,
                entry.default_context_tokens,
                &entry.min_runtime,
            ),
            register: Register::baseline(),
            tools: ToolRegistry::default(),
            audit: AuditLog::new(),
            outer_sect: OuterSectReport::default(),
            confirmer: None,
        }
    }

    /// 从环境变量读取端点；仅接受回环地址。不发起任何请求。
    pub fn from_env() -> Result<Self, AppError> {
        let mut session = Self::new();
        if let Ok(endpoint) = std::env::var(ENDPOINT_ENV) {
            session
                .provider
                .set_endpoint(&endpoint)
                .map_err(|e| AppError::Probe(e.to_string()))?;
        }
        Ok(session)
    }

    pub fn set_endpoint(&mut self, endpoint: &str) -> Result<(), AppError> {
        self.provider
            .set_endpoint(endpoint)
            .map_err(|e| AppError::Probe(e.to_string()))
    }

    pub fn set_workspace(&mut self, root: PathBuf) {
        // 只换策略，保留已注册的外宗工具：重建 registry 会把它们静默丢掉。
        self.tools.set_policy(ToolPolicy {
            root: Some(root),
            allowed_domains: Vec::new(),
        });
    }

    pub fn outer_sect(&self) -> &OuterSectReport {
        &self.outer_sect
    }

    /// 接入外宗：逐台握手、把暴露的工具挂进箓与风火轮。
    ///
    /// 权限档位固定为「受限执行」：MCP 工具的副作用 fulu 无法判断，
    /// 因此只读符（`Strict`）申请它会被 `PermissionTooLow` 驳回——
    /// 用户必须显式把符的沙箱提到 `limited` 才能召这些将。
    ///
    /// 风险等级取 server 自述（`annotations`），并抬到 `risk_floor` 以上：
    /// `risk_floor = Destructive` 即「一概不信任注解」，用于强管控场景。
    pub fn attach_mcp(&mut self, servers: &[McpServerInfo]) -> OuterSectReport {
        self.attach_mcp_full(servers, RiskClass::Observe, &RiskOverrides::default())
    }

    /// 带风险下限的接入。下限只能**抬高**等级，不能压低。
    pub fn attach_mcp_with(
        &mut self,
        servers: &[McpServerInfo],
        risk_floor: RiskClass,
    ) -> OuterSectReport {
        self.attach_mcp_full(servers, risk_floor, &RiskOverrides::default())
    }

    /// 完整接入：策略下限 + 用户自定的逐工具风险覆盖表。
    ///
    /// 覆盖表是**用户显式声明**，可以下调 server 未声明工具的风险；
    /// 下限是策略，只能上调。二者都不影响权限（外宗固定 `Limited`）。
    pub fn attach_mcp_full(
        &mut self,
        servers: &[McpServerInfo],
        risk_floor: RiskClass,
        overrides: &RiskOverrides,
    ) -> OuterSectReport {
        for info in servers {
            match fulu_mcp::connect_with(info, overrides, risk_floor) {
                Ok(session) => {
                    let mut registered = Vec::new();
                    for tool in &session.tools {
                        let id = tool.tool_id().to_owned();
                        let risk = tool.risk().max(risk_floor);
                        let mut grant = ToolGrant::limited(
                            &id,
                            &format!("{}（外宗 {}）", tool.description(), session.server),
                        )
                        .with_risk(risk)
                        .with_parameters_json(tool.schema_json());
                        // 外宗一次对话里常要连查多次状态，配额比内置工具宽一些。
                        grant.max_calls = 16;
                        self.register.grant_tool(grant);
                        self.tools.register(tool.clone() as Arc<dyn Tool>);
                        registered.push(OuterSectTool {
                            id,
                            risk,
                            risk_source: tool.risk_source(),
                            declared_read_only: tool.declared_read_only(),
                        });
                    }
                    self.outer_sect.servers.push(OuterSectServer {
                        name: session.server.clone(),
                        transport: info.transport.label(),
                        connected: true,
                        tools: registered,
                        error: None,
                        warnings: session.warnings,
                    });
                }
                Err(error) => self.outer_sect.servers.push(OuterSectServer {
                    name: info.name.clone(),
                    transport: info.transport.label(),
                    connected: false,
                    tools: Vec::new(),
                    error: Some(error.to_string()),
                    warnings: Vec::new(),
                }),
            }
        }
        self.outer_sect.clone()
    }

    /// 按系统约定路径发现并接入（只读配置文件，绝不写回）。
    ///
    /// 同时读取 fulu 自己的风险覆盖表 `~/.fulu/outer-sect-risk.json`：
    /// 那份文件是**用户对 server 缺失声明的补充**，不是对 MCP 生态的修改。
    pub fn attach_mcp_from_defaults(&mut self) -> OuterSectReport {
        let (servers, mut issues) = fulu_mcp::discover(&fulu_mcp::default_config_paths());
        let (overrides, override_issues) = RiskOverrides::load(&fulu_mcp::risk_override_paths());
        issues.extend(override_issues);
        self.outer_sect.issues = issues;
        let report = self.attach_mcp_full(&servers, RiskClass::Observe, &overrides);

        // 覆盖表里指向已消失工具的条目：说明它没生效，必须说出来。
        let known = report.tool_ids();
        let stale = overrides.stale_entries(&known);
        if !stale.is_empty() {
            self.outer_sect.issues.push(format!(
                "风险覆盖表中 {} 条指向不存在的工具（已失效）：{}",
                stale.len(),
                stale.join("、")
            ));
        }
        self.outer_sect.clone()
    }

    /// 设定本次会话的风险放行上限。
    ///
    /// `None`（默认）= fail closed：需要确认的召将一律敕否。
    /// CLI 的 `--allow-risk` 与 GUI 的勾选框都落到这里，并且只影响**本次**会话。
    pub fn set_risk_ceiling(&mut self, ceiling: Option<RiskClass>) {
        self.confirmer = ceiling.map(|ceiling| Box::new(AllowUpTo(ceiling)) as Box<dyn Confirmer>);
    }

    /// 装上自定义确认器（CLI 的终端提问、GUI 的回主线程询问都走这里）。
    pub fn set_confirmer(&mut self, confirmer: Box<dyn Confirmer>) {
        self.confirmer = Some(confirmer);
    }

    pub fn has_confirmer(&self) -> bool {
        self.confirmer.is_some()
    }

    pub fn endpoint(&self) -> &str {
        &self.provider.endpoint
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// 只探活与列模型，**不启动进程、不下载权重**。
    pub fn probe(&self) -> Result<ProbeReport, AppError> {
        if self.provider.endpoint.is_empty() {
            return Err(AppError::Probe(format!(
                "未设置 {ENDPOINT_ENV}；请先在本机启动 llama-server 并把回环地址写入该变量"
            )));
        }
        let health: Health = self
            .provider
            .health()
            .map_err(|e| AppError::Probe(e.to_string()))?;
        let models = if health.ready {
            self.provider
                .loaded_models()
                .map_err(|e| AppError::Probe(e.to_string()))?
        } else {
            Vec::new()
        };
        Ok(ProbeReport {
            endpoint: self.provider.endpoint.clone(),
            ready: health.ready,
            detail: health.detail,
            models,
        })
    }

    /// 成符 → 盖印 → 待焚。零网络请求。
    pub fn dry_run(&mut self, fu: &Talisman) -> Result<DryRunPlan, AppError> {
        let mut altar = self.altar(fu);
        let mut plan = altar.prepare(fu)?;
        // 把外宗的影响面写进 dry-run 计划：用户确认前必须看见「这张符能召哪些外宗将」。
        if !self.outer_sect.servers.is_empty() {
            plan.notes.push(self.outer_sect.summary());
            for server in &self.outer_sect.servers {
                let state = if server.connected {
                    let gated = server
                        .tools
                        .iter()
                        .filter(|tool| tool.risk != RiskClass::Observe)
                        .count();
                    if gated == 0 {
                        format!("{} 个工具（自述全只读）", server.tools.len())
                    } else {
                        format!("{} 个工具，其中 {} 个需人确认", server.tools.len(), gated)
                    }
                } else {
                    format!(
                        "连接失败：{}",
                        server.error.as_deref().unwrap_or("未知原因")
                    )
                };
                plan.notes.push(format!(
                    "  外宗 {:<16} {}（{}）",
                    server.name, state, server.transport
                ));
            }
            // 只列这张符真的会召的外宗将，避免把整份名册刷到确认页上。
            for id in plan.tools.iter().filter(|id| id.starts_with("mcp.")) {
                if let Some(grant) = self.register.tool(id) {
                    plan.notes
                        .push(format!("  召将 {id}：{}", grant.risk.label()));
                }
            }
        }
        Ok(plan)
    }

    /// 焚化。`confirmed` 由 UI 的按钮 / CLI 的 `--confirm` 提供，门面不替用户决定。
    pub fn ignite(
        &mut self,
        fu: &Talisman,
        plan: &DryRunPlan,
        confirmed: bool,
    ) -> Result<Receipt, AppError> {
        let mut altar = self.altar(fu);
        Ok(altar.ignite(fu, plan, confirmed)?)
    }

    /// 缴符导出为 JSONL。
    pub fn export_audit(&self, path: &Path) -> Result<usize, AppError> {
        self.audit
            .export_jsonl(path)
            .map_err(|e| AppError::Io(e.to_string()))
    }

    fn altar(&mut self, fu: &Talisman) -> Altar<'_> {
        // 逐字段借用：确认器要借给法坛，缴符日志同时要可变借用。
        let Self {
            provider,
            tools,
            register,
            audit,
            confirmer,
            ..
        } = self;
        let mut altar = Altar::new(
            provider,
            tools,
            register,
            audit,
            RitualConfig::from_talisman(fu),
        );
        // 未装确认器时不装 —— 法坛自己会 fail closed，needless 不替它兜底。
        if let Some(confirmer) = confirmer.as_mut() {
            altar.set_confirmer(confirmer.as_mut());
        }
        altar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_reports_embedded_model_without_touching_disk() {
        let report = doctor().unwrap();
        assert_eq!(report.model_id, "lfm3-1.7b-q4km");
        assert!(report.license.contains("lfm1.0"));
        assert!(report.manifest_issues.is_empty());
        assert!(report.compliance_warning.contains("1000 万美元"));
        assert!(report.provider_stage.contains("M2"));
    }

    #[test]
    fn sample_fu_passes_inspection_and_has_a_seal() {
        let text = sample_fu_text().unwrap();
        let report = inspect_fu(&text).unwrap();
        assert!(report.valid);
        assert_eq!(report.seal.len(), 64);
    }

    #[test]
    fn broken_fu_is_rejected_by_facade() {
        let mut fu = Talisman::sample();
        fu.credential_ref = "sk-leaked-key".into();
        let text = encode_fu(&fu).unwrap();
        assert!(inspect_fu(&text).is_err());
    }

    #[test]
    fn session_dry_run_is_offline_and_probe_requires_endpoint() {
        let mut session = AltarSession::new();
        assert!(session.probe().is_err(), "未设端点时不得静默探活");
        let plan = session.dry_run(&Talisman::sample()).unwrap();
        assert_eq!(plan.model_id, "lfm3-1.7b-q4km");
        assert!(session.audit().verify_chain().is_ok());
    }

    #[test]
    fn burning_without_confirmation_is_refused_by_facade() {
        let mut session = AltarSession::new();
        let fu = Talisman::sample();
        let plan = session.dry_run(&fu).unwrap();
        assert!(session.ignite(&fu, &plan, false).is_err());
    }

    #[test]
    fn non_loopback_endpoint_is_refused() {
        let mut session = AltarSession::new();
        assert!(session.set_endpoint("http://0.0.0.0:8080").is_err());
        assert!(session.set_endpoint("http://127.0.0.1:8080").is_ok());
    }
}
