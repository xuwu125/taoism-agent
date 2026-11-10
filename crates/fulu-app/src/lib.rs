//! 用例门面：GUI 与 CLI **必须**共用这里，避免两端行为漂移（docs/02 第 9.2 节）。
//!
//! M0 只有只读体检；M2 起提供完整仪轨会话 `AltarSession`：
//! 环境体检、符文加载、dry-run、显式确认后焚化、缴符导出与本地服务探活。
//! UI 层不允许直接碰 `fulu-runtime`，全部经此门面。

use fulu_audit::{AuditLog, EventKind};
use fulu_context::{SessionContext, collect as collect_context};
use fulu_core::{Talisman, TalismanState};
use fulu_mcp::{McpServerInfo, RiskOverrides, RiskSource};
use fulu_model::{ModelManifest, VerifyStatus, model_path, status, verify_file};
use fulu_prompt::{CORE_TEXT, LayerCaps, LayerId, PromptDraft, assemble};
use fulu_provider::{Health, LocalLfmProvider};
use fulu_register::{Register, ToolGrant};
use fulu_skills::{default_roots, digest_matches, discover_global};

/// 提示词档位对外暴露：CLI `--prompt-mode` 与未来的子符分派都要用它。
pub use fulu_prompt::{AssembledPrompt, PromptMode};
/// 风险等级与确认档位对外暴露：CLI/GUI 需要它们解析 `--allow-risk` 与渲染敕门卡片。
pub use fulu_register::{Confirmation, RiskClass};
use fulu_runtime::{AllowUpTo, Altar, Confirmer, RitualConfig, RitualError, SkillVerifier};
/// 内容摘要对外暴露：CLI 的装配预览要展示与 dry-run 计划一致的 sha256。
pub use fulu_script::digest_hex;
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

/// 符胆校验器：扫描用户全局技能目录（**只读**），按符文里的 `sha256:` 引用重校验。
///
/// T03：符只引用不内嵌；两次焚符之间技能文件被改动 / 被删除 → 破符。
/// 不写任何技能包，符合「全局技能包只展示不管理」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalSkillVerifier {
    pub roots: Vec<PathBuf>,
}

impl GlobalSkillVerifier {
    pub fn from_defaults() -> Self {
        Self {
            roots: default_roots(),
        }
    }
}

impl SkillVerifier for GlobalSkillVerifier {
    fn verify(&self, refs: &[String]) -> Result<(), String> {
        if refs.is_empty() {
            return Ok(());
        }
        let (skills, issues) = discover_global(&self.roots);
        for reference in refs {
            let Some(digest) = reference.strip_prefix("sha256:") else {
                return Err(format!("「{reference}」不是 sha256: 内容寻址引用"));
            };
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("「{reference}」摘要格式不合法"));
            }
            let Some(skill) = skills
                .iter()
                .find(|skill| skill.sha256.eq_ignore_ascii_case(digest))
            else {
                let tail = if issues.is_empty() {
                    "（扫描无异常）".to_owned()
                } else {
                    format!("（扫描异常：{}）", issues.join("；"))
                };
                return Err(format!(
                    "找不到内容匹配的技能包：{reference}{tail}。技能包被改动或删除时必须重新成符，不能沿用旧引用"
                ));
            };
            if !digest_matches(skill) {
                return Err(format!("技能包 {} 内容已变化：{reference}", skill.name));
            }
        }
        Ok(())
    }
}

/// fulu 自有配置目录（与风险覆盖表同处），不存在时返回 None。
fn fulu_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".fulu"))
}

/// 人类锁定层：只读文件，缺失/不可读 = 空层（装配器会跳过空小节）。
/// 上层权限阶梯（AI 提案需盖印）由流程约束，本函数绝不写回。
fn read_locked_layer(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…[TRUNCATED]")
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
    /// 工作区根目录（T02 的「工作区约定」层从这里的 `fulu.agents.md` 读取）。
    workspace: Option<PathBuf>,
    /// 提示词档位；主会话 full，子符 minimal，机械调用 none。
    prompt_mode: PromptMode,
    /// 会话头开关（可整体关闭）。
    context_enabled: bool,
    /// IP 粗粒度查询开关：**默认 false**，开启即显式允许一次外部查询，绝不静默联网。
    context_ip: bool,
    /// 每会话只采集一次；改变工作区或开关后重新采集。
    session_context: Option<SessionContext>,
    /// 静态层（0/1/3/4/5）快照，每会话装配一次；动态层（外宗、符文正文）每次装配。
    base_layers: Option<PromptDraft>,
    /// 会话头已落缴符事件的标记（每会话一条）。
    context_logged: bool,
    /// 符胆校验器（只读扫描全局技能目录）。
    skills: GlobalSkillVerifier,
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
            workspace: None,
            prompt_mode: PromptMode::Full,
            context_enabled: true,
            context_ip: false,
            session_context: None,
            base_layers: None,
            context_logged: false,
            skills: GlobalSkillVerifier::from_defaults(),
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
            root: Some(root.clone()),
            allowed_domains: Vec::new(),
        });
        // 工作区变了，约定层与磁盘信息都要重取，避免把旧工作区的提示词带进新符。
        self.workspace = Some(root);
        self.base_layers = None;
        self.session_context = None;
    }

    /// 提示词档位：主会话默认 full；召将子符用 minimal；纯机械调用用 none。
    pub fn set_prompt_mode(&mut self, mode: PromptMode) {
        self.prompt_mode = mode;
    }

    pub fn prompt_mode(&self) -> PromptMode {
        self.prompt_mode
    }

    /// 会话头总开关（关掉后系统提示词不含硬件/地域信息）。
    pub fn set_context_enabled(&mut self, enabled: bool) {
        self.context_enabled = enabled;
        self.base_layers = None;
    }

    /// IP 粗粒度查询：**必须显式开启**，默认关闭（不静默联网）。
    pub fn set_context_ip(&mut self, enabled: bool) {
        self.context_ip = enabled;
        self.session_context = None;
        self.base_layers = None;
    }

    pub fn context_ip(&self) -> bool {
        self.context_ip
    }

    /// 自定义符胆扫描根目录（测试与自定义部署用；只读）。
    pub fn set_skill_roots(&mut self, roots: Vec<PathBuf>) {
        self.skills = GlobalSkillVerifier { roots };
    }

    /// 会话头摘要（供 CLI/GUI 展示；未采集时返回 None）。
    pub fn context_summary(&mut self) -> Option<String> {
        if !self.context_enabled {
            return None;
        }
        self.ensure_context();
        self.session_context
            .as_ref()
            .map(|context| context.summary())
    }

    /// 七层装配预览：与真正送进法坛的 system 文本一致（同一装配函数）。
    pub fn prompt_preview(&mut self, fu: &Talisman) -> AssembledPrompt {
        self.ensure_prompt(fu)
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

    /// 每会话采集一次；IP 查询默认关闭，开启才发起一次外部请求。
    fn ensure_context(&mut self) {
        if self.session_context.is_none() && self.context_enabled {
            self.session_context =
                Some(collect_context(self.workspace.as_deref(), self.context_ip));
        }
    }

    /// 七层装配（T01/T02）：静态层每会话一次，动态层（外宗 / 符文正文）按符装配。
    ///
    /// 符文正文原样放进第 6 层，`fu.system` 一字不改；会话头只进系统消息与
    /// 缴符事件，绝不写回符文。人类锁定层只读，AI 提案需盖印（本轮不写入）。
    fn ensure_prompt(&mut self, fu: &Talisman) -> AssembledPrompt {
        if self.base_layers.is_none() {
            let mut draft = PromptDraft::new().set(LayerId::Core, CORE_TEXT);
            if self.context_enabled {
                self.ensure_context();
                if let Some(context) = &self.session_context {
                    draft = draft.set(LayerId::SessionHeader, context.summary());
                }
            }
            if let Some(home) = fulu_home() {
                if let Some(text) = read_locked_layer(&home.join("user.md")) {
                    draft = draft.set(LayerId::UserProfile, text);
                }
                if let Some(text) = read_locked_layer(&home.join("soul.md")) {
                    draft = draft.set(LayerId::Soul, text);
                }
            }
            if let Some(root) = &self.workspace
                && let Some(text) = read_locked_layer(&root.join("fulu.agents.md"))
            {
                draft = draft.set(LayerId::WorkspacePact, text);
            }
            self.base_layers = Some(draft);
        }
        let mut draft = self.base_layers.clone().unwrap_or_default();
        draft = draft.set(LayerId::OuterEnv, self.outer_env_text());
        draft = draft.set(LayerId::TalismanBody, fu.system.clone());
        assemble(self.prompt_mode, &draft, &LayerCaps::default())
    }

    /// 环境外宗层：外宗工具摘要 + 符胆发现计数（都只读，不写任何全局配置）。
    fn outer_env_text(&self) -> String {
        let (skills, issues) = discover_global(&self.skills.roots);
        let note = if issues.is_empty() {
            "扫描无异常".to_owned()
        } else {
            issues.join("；")
        };
        format!(
            "{}\n符胆：{} 个技能包（只读发现，{}）",
            self.outer_sect.summary(),
            skills.len(),
            note
        )
    }

    /// 写进 dry-run 计划的装配说明：用了哪几层、多少字符、有没有截断。
    fn prompt_note(&self, assembled: &AssembledPrompt) -> String {
        let mode = match assembled.mode {
            PromptMode::Full => "full",
            PromptMode::Minimal => "minimal",
            PromptMode::None => "none",
        };
        let layers = assembled
            .sections
            .iter()
            .map(|section| section.layer.label())
            .collect::<Vec<_>>()
            .join("、");
        format!(
            "提示词装配：{mode} 档 {} 层（{layers}）/ {} 字符{}；IP 查询{}；会话头只进系统消息与缴符，不进符文正文",
            assembled.sections.len(),
            assembled.total_chars,
            if assembled.total_truncated {
                "（有截断）"
            } else {
                ""
            },
            if self.context_ip {
                "已显式开启"
            } else {
                "关闭（默认不联网）"
            }
        )
    }

    /// T04：会话头落一条缴符事件（每会话一条，写入前由缴符层脱敏）。
    fn log_session_context(&mut self, seal: &str) -> Result<(), AppError> {
        if self.context_logged {
            return Ok(());
        }
        let detail = match &self.session_context {
            Some(context) => format!(
                "会话头（每会话一次，IP {}）：{}",
                if self.context_ip {
                    "显式开启"
                } else {
                    "关闭"
                },
                truncate_chars(&context.summary(), 512)
            ),
            None => "会话头：已按配置关闭（不采集）".to_owned(),
        };
        self.audit
            .append(seal, TalismanState::Pending, EventKind::Context, &detail)
            .map_err(|error| AppError::Io(format!("缴符会话头事件写入失败：{error}")))?;
        self.context_logged = true;
        Ok(())
    }

    /// 成符 → 盖印 → 待焚。零网络请求。
    pub fn dry_run(&mut self, fu: &Talisman) -> Result<DryRunPlan, AppError> {
        let assembled = self.ensure_prompt(fu);
        let mut altar = self.altar(fu, &assembled);
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
        self.log_session_context(&plan.seal)?;
        Ok(plan)
    }

    /// 焚化。`confirmed` 由 UI 的按钮 / CLI 的 `--confirm` 提供，门面不替用户决定。
    pub fn ignite(
        &mut self,
        fu: &Talisman,
        plan: &DryRunPlan,
        confirmed: bool,
    ) -> Result<Receipt, AppError> {
        let assembled = self.ensure_prompt(fu);
        let mut altar = self.altar(fu, &assembled);
        Ok(altar.ignite(fu, plan, confirmed)?)
    }

    /// 缴符导出为 JSONL。
    pub fn export_audit(&self, path: &Path) -> Result<usize, AppError> {
        self.audit
            .export_jsonl(path)
            .map_err(|e| AppError::Io(e.to_string()))
    }

    fn altar(&mut self, fu: &Talisman, assembled: &AssembledPrompt) -> Altar<'_> {
        let note = self.prompt_note(assembled);
        // 逐字段借用：确认器要借给法坛，缴符日志同时要可变借用。
        let Self {
            provider,
            tools,
            register,
            audit,
            confirmer,
            skills,
            ..
        } = self;
        let mut altar = Altar::new(
            provider,
            tools,
            register,
            audit,
            RitualConfig::from_talisman(fu),
        );
        // T02：装配结果交给法坛送进 system 消息，法坛不关心层的来源。
        altar.set_system_prompt(assembled.text.clone(), note);
        // T03：符胆校验器由门面注入，法坛不碰文件系统。
        altar.set_skill_verifier(&*skills);
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
    use fulu_audit::EventKind;

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

    /// T01/T02：七层真正装配出来，会话头与符文正文都在 system 里，符文本身不变。
    #[test]
    fn session_assembles_layers_and_logs_context_event() {
        let mut session = AltarSession::new();
        let fu = Talisman::sample();
        let original_system = fu.system.clone();
        let assembled = session.prompt_preview(&fu);
        assert_eq!(assembled.mode, PromptMode::Full);
        let layers: Vec<LayerId> = assembled.sections.iter().map(|s| s.layer).collect();
        for expected in [LayerId::Core, LayerId::SessionHeader, LayerId::TalismanBody] {
            assert!(layers.contains(&expected), "缺层：{expected:?}");
        }
        assert!(assembled.text.contains("## 符文正文"));
        assert!(assembled.text.contains(&original_system));
        assert!(
            assembled.text.contains("系统："),
            "会话头应出现在系统消息里"
        );
        assert_eq!(fu.system, original_system, "装配不得改写符文正文");
        assert!(!session.context_ip(), "IP 查询默认必须关闭（不静默联网）");

        let plan = session.dry_run(&fu).unwrap();
        assert!(
            plan.system_digest.is_some(),
            "提示词摘要必须进 dry-run 计划"
        );
        assert!(plan.notes.iter().any(|note| note.contains("提示词装配")));
        assert!(plan.notes.iter().any(|note| note.contains("IP 查询关闭")));
        assert!(
            session
                .audit()
                .events()
                .iter()
                .any(|event| event.kind == EventKind::Context),
            "会话头必须落缴符事件"
        );
        session.audit().verify_chain().unwrap();
    }

    /// T02：minimal 档与关闭会话头后，相应层必须真的消失。
    #[test]
    fn minimal_mode_and_disabled_context_drop_layers() {
        let mut session = AltarSession::new();
        session.set_context_enabled(false);
        session.set_prompt_mode(PromptMode::Minimal);
        let fu = Talisman::sample();
        let assembled = session.prompt_preview(&fu);
        assert_eq!(assembled.mode, PromptMode::Minimal);
        let layers: Vec<LayerId> = assembled.sections.iter().map(|s| s.layer).collect();
        assert!(layers.contains(&LayerId::Core));
        assert!(layers.contains(&LayerId::TalismanBody));
        assert!(!layers.contains(&LayerId::SessionHeader));
        assert!(!layers.contains(&LayerId::OuterEnv));
        assert!(!assembled.text.contains("系统："));
        assert!(
            session.context_summary().is_none(),
            "关闭采集后不返回会话头摘要"
        );
    }

    /// T03：技能包内容被改动后，引用旧摘要的符必须破符。
    #[test]
    fn changed_skill_digest_breaks_the_talisman() {
        let root = std::env::temp_dir().join(format!("fulu_app_skill_{}", std::process::id()));
        let skill_dir = root.join("demo");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        let text = "name: demo\nversion: 1.0\n\n原始技能内容\n";
        std::fs::write(&path, text).unwrap();
        let digest = fulu_script::digest_hex(text.as_bytes());

        let mut session = AltarSession::new();
        session.set_skill_roots(vec![root.clone()]);
        let mut fu = Talisman::sample();
        fu.skill_refs = vec![format!("sha256:{digest}")];
        assert!(session.dry_run(&fu).is_ok(), "摘要匹配时应正常成符");

        std::fs::write(&path, "name: demo\nversion: 2.0\n\n被偷偷改过的技能\n").unwrap();
        let error = session.dry_run(&fu).unwrap_err().to_string();
        assert!(
            error.contains("符胆") || error.contains("技能包"),
            "{error}"
        );
        assert!(
            session
                .audit()
                .events()
                .iter()
                .any(|event| event.kind == EventKind::Broken),
            "失配必须留下破符事件"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_skill_refs_are_rejected() {
        let mut session = AltarSession::new();
        session.set_skill_roots(vec![std::env::temp_dir().join("fulu_no_such_skills")]);
        let mut fu = Talisman::sample();
        fu.skill_refs = vec!["skill:demo".into()];
        assert!(session.dry_run(&fu).is_err(), "非内容寻址引用必须破符");
        fu.skill_refs = vec!["sha256:not-a-digest".into()];
        assert!(session.dry_run(&fu).is_err(), "假摘要必须破符");
    }
}
