//! 用例门面：GUI 与 CLI **必须**共用这里，避免两端行为漂移（docs/02 第 9.2 节）。
//!
//! M0 只有只读体检；M2 起提供完整仪轨会话 `AltarSession`：
//! 环境体检、符文加载、dry-run、显式确认后焚化、缴符导出与本地服务探活。
//! UI 层不允许直接碰 `fulu-runtime`，全部经此门面。

use fulu_audit::{AuditLog, EventKind};
use fulu_context::{SessionContext, collect as collect_context};
use fulu_core::{Talisman, TalismanState};
use fulu_mcp::{McpServerInfo, RiskOverrides, RiskSource};
use fulu_model::{
    ModelManifest, VerifyStatus, digest_cache, model_path, status, verify_file_cached,
};
use fulu_orchestrator::NodeExecutor;
use fulu_prompt::{CORE_TEXT, LayerCaps, LayerId, PromptDraft, assemble};
use fulu_provider::{BearerToken, Health, LocalLfmProvider};
use fulu_register::{Register, ToolGrant};
use fulu_skills::{default_roots, digest_matches, discover_global};
use fulu_vault::{CredentialRef, Vault};

mod summon;
pub use summon::{
    DispatchRecord, MAX_DISPATCH_DEPTH, SUB_MAX_STEPS, SUB_MAX_TIMEOUT_SECS,
    SUMMON_PARAMETERS_JSON, SUMMON_TOOL_ID, SummonConfig, SummonTool,
};

/// 太极符图对外暴露：CLI 只依赖门面，不直接构造编排器对象。
pub use fulu_orchestrator::{
    GraphError, GraphNode, NodeReceipt, NodeResult, NodeStatus, OrchestrationReceipt, TaijiGraph,
    parse_graph,
};
/// 提示词档位对外暴露：CLI `--prompt-mode` 与未来的子符分派都要用它。
pub use fulu_prompt::{AssembledPrompt, PromptMode};
/// 风险等级与确认档位对外暴露：CLI/GUI 需要它们解析 `--allow-risk` 与渲染敕门卡片。
pub use fulu_register::{Confirmation, RiskClass};
use fulu_runtime::{AllowUpTo, Altar, Confirmer, RitualConfig, RitualError, SkillVerifier};
/// 内容摘要对外暴露：CLI 的装配预览要展示与 dry-run 计划一致的 sha256。
pub use fulu_script::digest_hex;
use fulu_script::{decode_fu, encode_fu, seal_hex};
use fulu_tools::{
    AskSearchTool, SearchBackend, Tool, ToolArguments, ToolError, ToolPolicy, ToolRegistry,
};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// 门面重导出：UI 只依赖 `fulu-app`，不直连 `fulu-core` / `fulu-runtime`。
/// 契约是「UI 不构造运行时对象」，持有门面返回的值是允许的。
pub use fulu_core::Sandbox;
pub use fulu_core::Talisman as Fu;
/// 流式事件类型由门面转出，CLI / GUI 不必直接依赖 `fulu-provider`。
pub use fulu_provider::{RetryPolicy, StreamControl, StreamEvent};
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
    /// 箓牒（凭据库）：取不到凭据就是拒绝执行，不退化成「用空密钥试一次」
    Vault(String),
    /// 调用方中止：独立于 `Ritual`，否则界面只能靠文案猜「这是炸炉还是人按了中止」
    Cancelled(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(msg) => write!(f, "模型清单：{msg}"),
            Self::Script(msg) => write!(f, "符文：{msg}"),
            Self::Ritual(msg) => write!(f, "法坛：{msg}"),
            Self::Probe(msg) => write!(f, "本地推理服务：{msg}"),
            Self::Io(msg) => write!(f, "I/O：{msg}"),
            Self::Vault(msg) => write!(f, "箓牒：{msg}"),
            Self::Cancelled(msg) => write!(f, "中止：{msg}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<RitualError> for AppError {
    fn from(error: RitualError) -> Self {
        match error {
            // 取消不降级成 Ritual 字符串：调用方需要能 match 出「这是人按了中止」。
            RitualError::Cancelled(msg) => Self::Cancelled(msg),
            other => Self::Ritual(other.to_string()),
        }
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
/// 用运行时清单（seal 写回后的文件优先），不用编译期快照。
pub fn doctor() -> Result<DoctorReport, AppError> {
    let manifest = ModelManifest::runtime().map_err(|e| AppError::Manifest(e.to_string()))?;
    doctor_with_path(&manifest, None)
}

/// 可注入权重路径（测试与嵌入方）：`None` 表示用清单推导的默认位置。
/// 测试据此避开本机真实 1.1 GB 权重，不必每次体检都全量 sha256。
fn doctor_with_path(
    manifest: &ModelManifest,
    weights_path: Option<&Path>,
) -> Result<DoctorReport, AppError> {
    let issues = manifest.validate();
    let default = manifest
        .default_entry()
        .map_err(|e| AppError::Manifest(e.to_string()))?;
    let path = weights_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| model_path(default));
    let model_status = if path.exists() {
        // doctor 与 list 一样走 path+size+mtime 缓存（显式校验命令 model verify
        // 仍全量哈希）；注入的不存在路径在上面的 exists 分支就被拦住了。
        match verify_file_cached(&path, default, &digest_cache::cache_file()) {
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

/// 使用运行时清单查看某槽位的状态（供 GUI 状态栏复用）。
pub fn model_status(slot: &str) -> Result<String, AppError> {
    let manifest = ModelManifest::runtime().map_err(|e| AppError::Manifest(e.to_string()))?;
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

/// 把符文按**规范格式**写回 `.fu` 文本（T33）。
///
/// 规范格式只认字段：注释、字段顺序、空行都会按 `fulu-script` 的规则重排。
/// GUI 必须把这句话显示给用户——「保存」不等于「原样保存」，否则用户会以为
/// 自己写的注释还在。
pub fn dump_fu(fu: &Fu) -> Result<String, AppError> {
    encode_fu(fu).map_err(|e| AppError::Script(e.to_string()))
}

/// 模型槽位摘要（T34 模型面板；**只读**，不触发下载）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestSlot {
    pub slot: String,
    pub model_id: String,
    pub quant: String,
    pub context_tokens: u32,
    pub approx_size: String,
    pub runnable: bool,
    pub is_default: bool,
    /// 清单是否已固化 sha256（未固化 = 校验只能是 UNVERIFIED）。
    pub digest_pinned: bool,
}

pub fn manifest_slots() -> Result<Vec<ManifestSlot>, AppError> {
    let manifest = ModelManifest::embedded().map_err(|e| AppError::Manifest(e.to_string()))?;
    Ok(manifest
        .entries
        .iter()
        .map(|entry| ManifestSlot {
            slot: entry.slot.clone(),
            model_id: entry.model_id.clone(),
            quant: entry.quant.clone(),
            context_tokens: entry.default_context_tokens,
            approx_size: entry.approx_size.clone(),
            runnable: entry.runnable,
            is_default: entry.is_default,
            digest_pinned: entry.sha256.is_some(),
        })
        .collect())
}

/// 全局技能包只读清单（T35；沿用「只展示不管理」）。
pub fn skills_listing() -> (Vec<String>, Vec<String>) {
    let (skills, issues) = discover_global(&default_roots());
    let lines = skills
        .iter()
        .map(|skill| {
            format!(
                "{}  v{}  {} 字节  sha256:{}…",
                skill.name,
                skill.version,
                skill.bytes,
                &skill.sha256[..12.min(skill.sha256.len())]
            )
        })
        .collect();
    (lines, issues)
}

/// 风险覆盖表只读清单（T35；写入仍只走 `fulu mcp risk set`，GUI 不越权写）。
pub fn risk_overrides_listing() -> (Vec<String>, Vec<String>) {
    let (overrides, issues) = RiskOverrides::load(&fulu_mcp::risk_override_paths());
    let lines = overrides
        .entries()
        .map(|(id, risk)| format!("{id}  {}", fulu_mcp::risk_key(*risk)))
        .collect();
    (lines, issues)
}

/// 从文本解析一张符：GUI 编辑器里的内容走这条，不落盘也能校验。
pub fn parse_fu(text: &str) -> Result<Talisman, AppError> {
    decode_fu(text).map_err(|e| AppError::Script(e.to_string()))
}

/// 从 `.fu` 文件加载；解析阶段即做结构校验，破符绝不进入法坛。
/// 符文的三种物理形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuForm {
    /// `.fu`：人可读 JSON（权威形态，符印基于它）
    Json,
    /// 中文 DSL：与 JSON 等价的另一种语法
    Dsl,
    /// `.fub`：紧凑二进制信封（自带符印）
    Fub,
}

impl FuForm {
    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Dsl => "中文 DSL",
            Self::Fub => "二进制 .fub",
        }
    }
}

/// 从文件加载符文：**按内容识别**三种形态，不靠扩展名（扩展名会骗人）。
///
/// 识别顺序：`.fub` magic → 首个非空白字符是否为 `{`（JSON）→ 否则当中文 DSL。
/// 三条路径最终都要过结构校验，破符即返回错误，绝不返回半成品。
pub fn load_fu_file(path: &Path) -> Result<(Talisman, FuForm), AppError> {
    let bytes =
        std::fs::read(path).map_err(|e| AppError::Io(format!("{}：{e}", path.display())))?;
    if bytes.starts_with(&fulu_script::FUB_MAGIC) {
        let fu = fulu_script::decode_fub(&bytes).map_err(|e| AppError::Script(e.to_string()))?;
        return Ok((fu, FuForm::Fub));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| AppError::Script("既不是 .fub 二进制，也不是 UTF-8 文本".into()))?;
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('{') {
        let fu = decode_fu(&text).map_err(|e| AppError::Script(e.to_string()))?;
        Ok((fu, FuForm::Json))
    } else {
        let fu = fulu_script::decode_dsl(&text).map_err(|e| AppError::Script(e.to_string()))?;
        Ok((fu, FuForm::Dsl))
    }
}

/// 从 `.fu` 文件加载（JSON 形态的快捷方式）。
///
/// 自 T39 起它**也接受**中文 DSL 与 `.fub`：同一个入口，三种形态，免得
/// 「CLI 能烧 DSL、GUI 只认 JSON」这种分叉出现。
pub fn load_fu(path: &Path) -> Result<Talisman, AppError> {
    load_fu_file(path).map(|(fu, _)| fu)
}

/// 解析太极符图 JSON（纯文本，不读磁盘）。
pub fn parse_taiji_graph(text: &str) -> Result<TaijiGraph, AppError> {
    parse_graph(text).map_err(|error| AppError::Ritual(error.to_string()))
}

/// 从文件读取符图，并把 `fu_path`（相对符图文件）解析成 `fu_text`。
///
/// 只读符文文件，不写任何配置；符图与符文都必须是用户显式给的路径。
pub fn load_taiji_graph(path: &Path) -> Result<TaijiGraph, AppError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| AppError::Io(format!("{}：{e}", path.display())))?;
    let mut graph = parse_taiji_graph(&text)?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    for node in &mut graph.nodes {
        if let Some(relative) = &node.fu_path {
            let full = if Path::new(relative).is_absolute() {
                PathBuf::from(relative)
            } else {
                base.join(relative)
            };
            node.fu_text = std::fs::read_to_string(&full).map_err(|e| {
                AppError::Io(format!("节点 {} 的符文 {}：{e}", node.id, full.display()))
            })?;
        }
    }
    Ok(graph)
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

/// 摄行（T16）进程级解锁判定。
///
/// 独立成纯函数：既能被环境变量路径复用，也能在测试里断言「只有 `1` 才算解锁」
/// （`0`/`true`/空白一律不算——最高权限不接受含糊值）。
fn shexing_unlock_matches(raw: Option<&str>) -> bool {
    matches!(raw.map(str::trim), Some("1"))
}

/// 问天（T15）的后端挑选与 MCP 适配。
///
/// 候选规则（宁缺勿滥，命中才算）：工具全名或描述含 `search` / `搜索`；
/// 查询参数名优先从 server 自述的 `inputSchema` 里认（query/q/search/
/// keyword/keywords/text），认不出时回退 `query`——错参数名会被外宗
/// 如实报错回喂，不会炸炉。
pub fn pick_search_candidates(listed: &[(String, String, String)]) -> Vec<(String, String)> {
    const QUERY_KEYS: &[&str] = &["query", "q", "search", "keyword", "keywords", "text"];
    const NAME_HINTS: &[&str] = &["search", "搜索"];
    let mut picked = Vec::new();
    for (id, description, schema_json) in listed {
        let lowered_id = id.to_lowercase();
        let lowered_desc = description.to_lowercase();
        let hit = NAME_HINTS
            .iter()
            .any(|hint| lowered_id.contains(hint) || lowered_desc.contains(hint));
        if !hit {
            continue;
        }
        let query_key = serde_json::from_str::<serde_json::Value>(schema_json)
            .ok()
            .and_then(|schema| {
                let props = schema.get("properties")?.as_object()?.clone();
                QUERY_KEYS
                    .iter()
                    .find(|key| props.contains_key(**key))
                    .map(|key| (*key).to_string())
            })
            .unwrap_or_else(|| "query".into());
        picked.push((id.clone(), query_key));
    }
    picked
}

/// 把一个已登记的外宗工具包成问天后端：直接调它的 `Tool::invoke`。
/// 安全闸门在 `ask.search` 自身（limited + Destructive，逐次确认）；
/// 本适配器不做二次核验，也不放宽任何边界。
struct McpToolSearchBackend {
    id: String,
    tool: Arc<dyn Tool>,
    query_key: String,
}

impl SearchBackend for McpToolSearchBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn search(&self, query: &str, _limit: usize) -> Result<String, ToolError> {
        // limit 仅作提示：外宗 schema 各异，不强塞未声明参数。
        let mut object = serde_json::Map::new();
        object.insert(
            self.query_key.clone(),
            serde_json::Value::String(query.to_owned()),
        );
        let args = ToolArguments {
            json: serde_json::Value::Object(object).to_string(),
        };
        self.tool
            .invoke(&args, &ToolPolicy::default())
            .map(|out| out.content)
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

pub(crate) fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…[TRUNCATED]")
}

/// 默认硬件快照的进程级缓存；只对「无工作区 + 关闭 IP」这一默认配置生效。
static DEFAULT_CONTEXT: OnceLock<SessionContext> = OnceLock::new();

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
    /// 召将子符工具（T10）；`None` = 未启用分派。
    summon: Option<Arc<SummonTool>>,
    /// 问天后端（T15）；空 = 未启用搜索（工具本体仍登记，调用时明确报错）。
    search_backends: Vec<String>,
    /// 摄行（T16）：默认 false，工具**不存在**于名册与风火轮。
    shexing: bool,
    /// 箓牒（T40）：凭据库。`None` = 未装，此时**任何非 local 引用都会被拒绝**。
    vault: Option<Box<dyn Vault>>,
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
            summon: None,
            search_backends: Vec::new(),
            shexing: false,
            vault: None,
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

    /// 透传重试策略到本地 provider（加法式：不设置就是 provider 默认 3 次 / 400ms）。
    pub fn set_retry_policy(&mut self, policy: RetryPolicy) {
        self.provider.set_retry_policy(policy);
    }

    /// 让符文 `harness.timeout_secs` 真正作用于单次 provider 请求。
    ///
    /// T23 真机暴露：provider 默认固定 30s，本机 CPU 上 LFM3 约 11.9 tokens/s，
    /// 稍长的回复会被掐断并触发重试（同一请求在服务端重复生成三次）。
    /// 夹在 1..=600 秒，避免符文写个荒谬值把整符超时绕过。
    pub fn set_provider_timeout(&mut self, timeout: std::time::Duration) {
        self.provider.timeout = timeout.clamp(
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(600),
        );
    }

    /// 当前单次请求超时（设置面板回显用；只读）。
    pub fn provider_timeout(&self) -> std::time::Duration {
        self.provider.timeout
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

    /// 当前会话可用的工具名（含外宗与召将；供 GUI 展示与测试）。
    pub fn tool_ids(&self) -> Vec<String> {
        self.register.tool_ids()
    }

    /// 启用召将子符（T10）：把 `summon.fu` 挂进风火轮与箓。
    ///
    /// 授权：受限级（符须 `sandbox: limited`）、风险 `Mutate`（每符确认一次）、配额 4 次。
    /// **须在设好端点之后调用**：子符使用启用时的端点快照。
    /// 子符的全部安全边界见 `summon.rs` 模块文档。
    pub fn enable_summoning(&mut self) {
        let tool = Arc::new(SummonTool::new(SummonConfig {
            endpoint: self.provider.endpoint.clone(),
            child_depth: 1,
            workspace: self.workspace.clone(),
        }));
        self.tools.register(tool.clone() as Arc<dyn Tool>);
        let mut grant = ToolGrant::limited(
            SUMMON_TOOL_ID,
            "分派一张子符在独立法坛上焚化（minimal 档、独立预算、禁环回、\n不继承父符高危授权）",
        )
        .with_risk(RiskClass::Mutate)
        .with_parameters_json(SUMMON_PARAMETERS_JSON);
        grant.max_calls = 4;
        self.register.grant_tool(grant);
        self.summon = Some(tool);
    }

    pub fn summoning_enabled(&self) -> bool {
        self.summon.is_some()
    }

    /// 启用摄行（T16）：电脑控制四件套。
    ///
    /// **默认不存在**——不调用本方法时，模型在箓与风火轮里都看不到这些工具。
    /// 解锁需要两把钥匙：进程环境变量 `FULU_ALLOW_SHEXING=1` + 本方法（CLI
    /// `--shexing` / GUI 开关）。授权为受限级 + `Destructive`（→ 敕门
    /// `EveryCall`，每一次动作都要人点头）+ 配额 20 次。风险评估见 docs/03 第 4 节。
    pub fn enable_shexing(&mut self) -> Result<(), AppError> {
        if !shexing_unlock_matches(std::env::var("FULU_ALLOW_SHEXING").ok().as_deref()) {
            return Err(AppError::Probe(
                "摄行默认禁用：需先设置环境变量 FULU_ALLOW_SHEXING=1（进程级）+ 本开关（会话级）\
                 ——最高权限要两把钥匙，任一缺失都不注册"
                    .into(),
            ));
        }
        self.enable_shexing_unlocked();
        Ok(())
    }

    /// 真正注册摄行四件套（环境变量已由调用方核验）。测试直接调它。
    fn enable_shexing_unlocked(&mut self) {
        self.tools
            .register(Arc::new(fulu_tools::ShexingTypeTool) as Arc<dyn Tool>);
        self.tools
            .register(Arc::new(fulu_tools::ShexingKeyTool) as Arc<dyn Tool>);
        self.tools
            .register(Arc::new(fulu_tools::ShexingClickTool) as Arc<dyn Tool>);
        self.tools
            .register(Arc::new(fulu_tools::ShexingScreenshotTool) as Arc<dyn Tool>);

        let specs: [(&str, &str, &str); 4] = [
            (
                fulu_tools::SHEXING_TOOL_IDS[0],
                "向当前前台窗口键入文本（最高权限，逐次确认）",
                r#"{"type":"object","properties":{"text":{"type":"string","description":"要键入的文本（≤2000 字符）"}},"required":["text"]}"#,
            ),
            (
                fulu_tools::SHEXING_TOOL_IDS[1],
                "发送组合键，SendKeys 语法子集（如 ^s、%{TAB}、{ENTER}）",
                r#"{"type":"object","properties":{"keys":{"type":"string","description":"键位串，只允许字母数字、^ % + 与 {NAME}"}},"required":["keys"]}"#,
            ),
            (
                fulu_tools::SHEXING_TOOL_IDS[2],
                "移动光标并点击（先确认坐标，误点不可撤销）",
                r#"{"type":"object","properties":{"x":{"type":"integer"},"y":{"type":"integer"},"button":{"type":"string","enum":["left","right"]}},"required":["x","y"]}"#,
            ),
            (
                fulu_tools::SHEXING_TOOL_IDS[3],
                "截屏并写入工作区 .png（不联网、不自动进提示词）",
                r#"{"type":"object","properties":{"path":{"type":"string","description":"相对工作区的 .png 路径"}},"required":["path"]}"#,
            ),
        ];
        for (id, description, parameters) in specs {
            let mut grant = ToolGrant::limited(id, description)
                .with_risk(RiskClass::Destructive)
                .with_parameters_json(parameters);
            grant.max_calls = 20;
            self.register.grant_tool(grant);
        }
        self.shexing = true;
    }

    pub fn shexing_enabled(&self) -> bool {
        self.shexing
    }

    /// 装箓牒（T40）。
    ///
    /// 规则很直白：**箓牒里登记了某条引用，就等于这一条已受箓**——所以装库的同时
    /// 把这些引用记进名册。反过来，库里没有的引用，`authorize` 仍然会驳回。
    ///
    /// 返回已入箓的引用清单（只有名字，没有密钥）。
    pub fn use_vault(&mut self, vault: Box<dyn Vault>) -> Result<Vec<String>, AppError> {
        let references = vault.list().map_err(|e| AppError::Vault(e.to_string()))?;
        for reference in &references {
            self.register.grant_credential(reference);
        }
        self.vault = Some(vault);
        Ok(references)
    }

    /// 当前箓牒后端名；未装时为 `None`（不是「未知」）。
    pub fn vault_backend(&self) -> Option<&'static str> {
        self.vault.as_ref().map(|vault| vault.backend())
    }

    /// provider 是否已装上 Bearer 令牌（**只回答有没有，不回答是什么**）。
    pub fn provider_auth_enabled(&self) -> bool {
        self.provider.auth_enabled()
    }

    /// 箓牒里的引用清单（原样返回后端结果，不做二次缓存）。
    pub fn vault_entries(&self) -> Result<Vec<String>, AppError> {
        match &self.vault {
            Some(vault) => vault.list().map_err(|e| AppError::Vault(e.to_string())),
            None => Ok(Vec::new()),
        }
    }

    /// 解析敕令凭据。返回 `(Bearer 令牌, 引用)`：
    /// 本地引用给 `(None, None)`；其余引用**必须**取到，否则报错（绝不返回空令牌）。
    fn resolve_credential(
        &self,
        fu: &Talisman,
    ) -> Result<(Option<BearerToken>, Option<CredentialRef>), AppError> {
        let reference =
            CredentialRef::parse(&fu.credential_ref).map_err(|e| AppError::Vault(e.to_string()))?;
        if reference.is_local() {
            return Ok((None, None));
        }
        let Some(vault) = self.vault.as_ref() else {
            return Err(AppError::Vault(format!(
                "符要求敕令 {reference}，但本会话没装箓牒；先执行 `fulu vault put {reference}` 并重开会话\
                 （或用 FULU_VAULT_PASSPHRASE 走文件级加密后端）"
            )));
        };
        let secret = vault
            .get(&reference)
            .map_err(|e| AppError::Vault(e.to_string()))?;
        // Secret 在这里就交给 BearerToken 并原值 drop（内部清零），不留副本。
        Ok((Some(BearerToken::new(secret.expose())), Some(reference)))
    }

    /// dry-run 里那一行「敕令」说明：能不能取到，必须在确认页上写清楚。
    fn credential_note(&self, fu: &Talisman) -> Result<String, AppError> {
        match self.resolve_credential(fu)? {
            (_, None) => Ok("敕令：lu://default/local（本地模型无需凭据）".to_owned()),
            (_, Some(reference)) => Ok(format!(
                "敕令：{reference} 已从箓牒取出（焚化时作为 Bearer 注入；符、日志、缴符均无明文）"
            )),
        }
    }

    /// 启用问天（T15）：从**已接入**的外宗工具里挑搜索类后端，绑给
    /// `ask.search`。没有候选时明确报错——绝不静默联网、绝不写全局配置。
    ///
    /// 安全语义：`ask.search` 是门面，代调外宗搜索工具。它自己的授权是
    /// `limited + Destructive`（每次召将都要人点头，见 `fulu-register`
    /// baseline），被代调的外宗工具自身的登记不受影响（模型直调仍走
    /// 它自己的箓核验与敕门）。
    pub fn enable_ask_search(&mut self) -> Result<Vec<String>, AppError> {
        // (id, description, schema_json) 三元组来自已接入的外宗登记。
        let mut listed: Vec<(String, String, String)> = Vec::new();
        for server in &self.outer_sect.servers {
            if !server.connected {
                continue;
            }
            for tool in &server.tools {
                if let Some(grant) = self.register.tool(&tool.id) {
                    listed.push((
                        tool.id.clone(),
                        grant.description.clone(),
                        grant.parameters_json.clone(),
                    ));
                }
            }
        }
        let picked = pick_search_candidates(&listed);
        if picked.is_empty() {
            return Err(AppError::Probe(
                "问天未启用：已接入的外宗里没有搜索类工具（名字或描述含 search/搜索）。\
                 可用 `fulu mcp list --attach` 核对外宗清单；本端不会静默联网"
                    .into(),
            ));
        }
        let mut backends: Vec<Arc<dyn SearchBackend>> = Vec::new();
        for (id, query_key) in picked {
            let Some(tool) = self.tools.get(&id) else {
                continue;
            };
            backends.push(Arc::new(McpToolSearchBackend {
                id: id.clone(),
                tool,
                query_key,
            }));
        }
        let ids = backends.iter().map(|b| b.id().to_owned()).collect();
        self.bind_ask_search_backends(backends);
        self.note_search_backends(ids);
        Ok(self.search_backend_ids())
    }

    /// 显式绑定问天后端（GUI/测试/自定义后端入口；绕过自动挑选）。
    /// 后端清单由 `note_search_backends` 单独记录，工具本体只认 trait。
    pub fn bind_ask_search_backends(&mut self, backends: Vec<Arc<dyn SearchBackend>>) {
        self.tools
            .register(Arc::new(AskSearchTool::new(backends)) as Arc<dyn Tool>);
    }

    /// 已绑定的问天后端标识（供展示；未启用时为空）。
    pub fn search_backend_ids(&self) -> Vec<String> {
        self.search_backends.clone()
    }

    /// 记录后端清单（bind 前调用；与 `bind_ask_search_backends` 配对）。
    fn note_search_backends(&mut self, ids: Vec<String>) {
        self.search_backends = ids;
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

    /// 默认硬件快照（无工作区、关闭 IP）在同一进程内只采一次。
    ///
    /// 平台采集脚本（PowerShell / sysctl 等）不便宜，GUI 与 CI 里频繁新建会话时
    /// 反复启动进程会拖慢启动，甚至拖累并行的外宗子进程。带自定义工作区或显式
    /// 开 IP 的会话仍各自采集，绝不复用别人的配置结果。
    fn ensure_context(&mut self) {
        if self.session_context.is_some() || !self.context_enabled {
            return;
        }
        let snapshot = if self.workspace.is_none() && !self.context_ip {
            DEFAULT_CONTEXT
                .get_or_init(|| collect_context(None, false))
                .clone()
        } else {
            collect_context(self.workspace.as_deref(), self.context_ip)
        };
        self.session_context = Some(snapshot);
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
        // 顺序是刻意的：**破符 → 凭据 → 授箓 → 其余**。
        // 结构都不对的符，谈凭据与授权没有意义；而凭据问题比笼统的「未受箓」更好定位——
        // 在 T40 的规则下「箓牒里有这条引用」就等于已受箓，所以先查凭据能直接给出补救命令。
        let violations = fu.validate();
        if !violations.is_empty() {
            return Err(AppError::Script(violations.to_string()));
        }
        let credential_line = self.credential_note(fu)?;
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
        // T10：启用召将子符时，确认页必须能看见子符的影响面与边界。
        if self.summoning_enabled() {
            plan.notes.push(format!(
                "召将子符：summon.fu 已启用（minimal 档、独立预算、步数上限 {SUB_MAX_STEPS}、禁环回、不继承父符高危授权；每次分派需敕可，配额 4 次）"
            ));
        }
        // T16：摄行是最高权限能力，确认页必须写明它已解锁、且每次动作都要点头。
        if self.shexing_enabled() {
            plan.notes.push(
                "摄行：shexing.* 已显式解锁（键鼠与截屏；每一次动作都要人点头，配额 20 次/符；默认不存在）"
                    .to_owned(),
            );
        }
        // T15：问天已绑后端时，确认页列出真实出网路径（Destructive 逐次确认）。
        let backends = self.search_backend_ids();
        if !backends.is_empty() {
            plan.notes.push(format!(
                "问天：ask.search 已绑定 {} 个外宗搜索后端（{}）；每次查天需敕可（Destructive）",
                backends.len(),
                backends.join("、")
            ));
        }
        // T40：敕令凭据在确认页之前就已确认「取得到」，这里把结论写进计划。
        plan.notes.push(credential_line);
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
        self.ignite_inner(fu, plan, confirmed, None)
    }

    /// 带流式显示的焚化。**只是显示**：不改变预算、工具核验、收敛判定与缴符主链。
    ///
    /// 单独一个入口而不是给 `ignite` 加 `Option` 参数，是为了让「不流式」这条
    /// 默认路径在类型上就与流式无关——老调用方一个字都不用改。
    pub fn ignite_with_stream(
        &mut self,
        fu: &Talisman,
        plan: &DryRunPlan,
        confirmed: bool,
        sink: &mut dyn FnMut(StreamEvent) -> StreamControl,
    ) -> Result<Receipt, AppError> {
        self.ignite_inner(fu, plan, confirmed, Some(sink))
    }

    fn ignite_inner(
        &mut self,
        fu: &Talisman,
        plan: &DryRunPlan,
        confirmed: bool,
        stream: Option<&mut dyn FnMut(StreamEvent) -> StreamControl>,
    ) -> Result<Receipt, AppError> {
        // T40：**没确认就不碰凭据库**。取密钥这件事只在用户已经点头之后才发生。
        // 取不到即返回错误，绝不把「空令牌」发出去当认证。
        if confirmed {
            let (token, reference) = self.resolve_credential(fu)?;
            self.provider.set_auth_token(token);
            if let Some(reference) = reference {
                let _ = self.audit.append(
                    &plan.seal,
                    TalismanState::Igniting,
                    EventKind::Credential,
                    &format!("敕令 {reference} 已从箓牒解析并注入，未写入任何明文"),
                );
            }
        }
        let assembled = self.ensure_prompt(fu);
        let mut altar = self.altar(fu, &assembled);
        if let Some(sink) = stream {
            altar.set_stream_sink(sink);
        }
        let result = altar.ignite(fu, plan, confirmed);
        // T10：无论成败，把分派经过与子符回执摘要落进父符缴符事件链。
        // 主链完整性已由法坛保证；此处尽力而为，不让记账失败掩盖焚化结果。
        if let Some(tool) = &self.summon {
            for record in tool.take_dispatch_log() {
                let _ = self.audit.append(
                    &plan.seal,
                    TalismanState::Igniting,
                    EventKind::ToolResult,
                    &format!("召将子符：{}", record.line()),
                );
            }
        }
        Ok(result?)
    }

    /// 太极：按符图的拓扑序串行焚化一组符（T09）。
    ///
    /// 每个节点都走完整仪轨（成符 → 盖印 → dry-run → 确认 → 焚化），共用一个
    /// 会话与一本缴符。依赖失败/跳过的节点不执行；`confirmed` 由用户显式给出，
    /// 编排器绝不替用户点头。符图的校验（环 / 未知依赖 / 规模）由编排器负责。
    pub fn run_taiji(
        &mut self,
        graph: &TaijiGraph,
        confirmed: bool,
    ) -> Result<OrchestrationReceipt, AppError> {
        self.run_taiji_with_progress(graph, confirmed, &mut |_| {})
    }

    /// 太极 + 逐节点进度回传（T36 GUI 用）。**加法式**：`run_taiji` 委托到这里，
    /// 传空闭包，签名与行为都不变。
    ///
    /// `on_node` 在每个节点**判定后**调用：成功 / 失败 / 跳过都会调，跳过的理由
    /// 在 `NodeResult::reason` 里。执行顺序仍**只来自** `TaijiGraph::validate()`；
    /// 跳过与预算判定逐字段镜像 `TaijiGraph::execute` 的语义（编排器本体不提供
    /// 进度钩子）——改编排器语义时必须同步改这里，下方测试会锁住行为。
    pub fn run_taiji_with_progress(
        &mut self,
        graph: &TaijiGraph,
        confirmed: bool,
        on_node: &mut dyn FnMut(&NodeResult),
    ) -> Result<OrchestrationReceipt, AppError> {
        struct SessionRunner<'a> {
            session: &'a mut AltarSession,
            confirmed: bool,
        }

        impl NodeExecutor for SessionRunner<'_> {
            fn execute(&mut self, node: &GraphNode) -> Result<NodeReceipt, String> {
                let fu = parse_fu(&node.fu_text).map_err(|error| error.to_string())?;
                let plan = self
                    .session
                    .dry_run(&fu)
                    .map_err(|error| error.to_string())?;
                let receipt = self
                    .session
                    .ignite(&fu, &plan, self.confirmed)
                    .map_err(|error| error.to_string())?;
                Ok(NodeReceipt {
                    id: node.id.clone(),
                    steps: receipt.steps,
                    text: receipt.text,
                    converged: receipt.converged,
                    events: receipt.events.len(),
                })
            }
        }

        let order = graph
            .validate()
            .map_err(|error| AppError::Ritual(error.to_string()))?;
        let max_steps = if graph.max_steps == 0 {
            fulu_orchestrator::DEFAULT_MAX_STEPS
        } else {
            graph.max_steps
        };
        let mut runner = SessionRunner {
            session: self,
            confirmed,
        };
        let mut status: BTreeMap<String, NodeStatus> = BTreeMap::new();
        let mut results = Vec::with_capacity(order.len());
        let mut total_steps = 0_u32;
        let mut stopped_by_budget = false;

        for &i in &order {
            let node = &graph.nodes[i];
            let blocked: Vec<String> = node
                .depends_on
                .iter()
                .filter(|dep| status.get(*dep).copied() != Some(NodeStatus::Succeeded))
                .cloned()
                .collect();
            let outcome = if !blocked.is_empty() {
                NodeResult {
                    id: node.id.clone(),
                    status: NodeStatus::Skipped,
                    receipt: None,
                    reason: Some(format!("依赖未完成：{}", blocked.join("、"))),
                }
            } else if stopped_by_budget || total_steps >= max_steps {
                stopped_by_budget = true;
                NodeResult {
                    id: node.id.clone(),
                    status: NodeStatus::Skipped,
                    receipt: None,
                    reason: Some(format!("总步数预算 {max_steps} 已耗尽")),
                }
            } else {
                match runner.execute(node) {
                    Ok(receipt) => {
                        total_steps = total_steps.saturating_add(receipt.steps);
                        if total_steps >= max_steps {
                            stopped_by_budget = true;
                        }
                        NodeResult {
                            id: node.id.clone(),
                            status: NodeStatus::Succeeded,
                            receipt: Some(receipt),
                            reason: None,
                        }
                    }
                    Err(reason) => NodeResult {
                        id: node.id.clone(),
                        status: NodeStatus::Failed,
                        receipt: None,
                        reason: Some(reason),
                    },
                }
            };
            status.insert(node.id.clone(), outcome.status);
            on_node(&outcome);
            results.push(outcome);
        }

        let succeeded = results
            .iter()
            .filter(|r| r.status == NodeStatus::Succeeded)
            .count();
        let failed = results
            .iter()
            .filter(|r| r.status == NodeStatus::Failed)
            .count();
        let skipped = results
            .iter()
            .filter(|r| r.status == NodeStatus::Skipped)
            .count();
        let converged = failed == 0
            && skipped == 0
            && results
                .iter()
                .filter_map(|r| r.receipt.as_ref())
                .all(|receipt| receipt.converged);
        Ok(OrchestrationReceipt {
            order: order.iter().map(|&i| graph.nodes[i].id.clone()).collect(),
            results,
            total_steps,
            succeeded,
            failed,
            skipped,
            converged,
            stopped_by_budget,
            max_steps,
        })
    }

    /// 缴符导出为 JSONL。
    pub fn export_audit(&self, path: &Path) -> Result<usize, AppError> {
        self.audit
            .export_jsonl(path)
            .map_err(|e| AppError::Io(e.to_string()))
    }

    /// 读回缴符 JSONL（T34 历史回放）：**只读**，绝不改写文件。
    ///
    /// 每行原样返回——界面按符印/关键字筛选即可，不在门面里做二次解析，
    /// 免得「回放」变成另一套与写入不一致的格式理解。
    pub fn replay_audit(&self, path: &Path) -> Result<Vec<String>, AppError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AppError::Io(format!("{}：{e}", path.display())))?;
        Ok(text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect())
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
    fn doctor_reports_model_without_touching_real_weights() {
        // 注入一个绝不存在的权重路径：本机真有权重时也不做 1.1 GB 全量哈希。
        let manifest = ModelManifest::runtime().unwrap();
        let absent =
            std::env::temp_dir().join(format!("fulu-doctor-absent-{}.gguf", std::process::id()));
        let report = doctor_with_path(&manifest, Some(&absent)).unwrap();
        assert!(report.model_status.contains("MISSING"));
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

    /// 取消必须能一路穿透到界面层：`RitualError::Cancelled` → `AppError::Cancelled`。
    /// 否则 GUI 只能按文案猜「这是人按了中止还是炸炉」，迟早显示成故障。
    #[test]
    fn cancelled_ritual_error_stays_distinguishable_through_app_error() {
        let error: AppError = fulu_runtime::RitualError::Cancelled("调用方取消".into()).into();
        assert!(
            matches!(error, AppError::Cancelled(ref msg) if msg.contains("取消")),
            "取消不许降级成 Ritual 字符串：{error:?}"
        );
        assert!(error.to_string().starts_with("中止："), "{error}");
        // 对照组：真炸炉仍是 Ritual，不能被误标成中止。
        let exploded: AppError = fulu_runtime::RitualError::Provider("断流".into()).into();
        assert!(matches!(exploded, AppError::Ritual(_)));
        assert!(exploded.to_string().contains("炸炉"), "{exploded}");
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

    #[test]
    fn provider_timeout_follows_fu_harness_but_is_clamped() {
        use std::time::Duration;
        let mut session = AltarSession::new();
        session.set_provider_timeout(Duration::from_secs(60));
        assert_eq!(session.provider.timeout, Duration::from_secs(60));
        session.set_provider_timeout(Duration::ZERO);
        assert_eq!(session.provider.timeout, Duration::from_secs(1));
        session.set_provider_timeout(Duration::from_secs(99_999));
        assert_eq!(session.provider.timeout, Duration::from_secs(600));
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

    /// T09：符图经门面读盘、跑完整仪轨；未确认的节点失败且下游跳过，不伪装成功。
    #[test]
    fn taiji_graph_loads_fu_paths_and_respects_confirmation() {
        let dir = std::env::temp_dir().join(format!("fulu_taiji_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sample = sample_fu_text().unwrap();
        std::fs::write(dir.join("a.fu"), &sample).unwrap();
        std::fs::write(dir.join("b.fu"), &sample).unwrap();
        let graph_path = dir.join("graph.json");
        std::fs::write(
            &graph_path,
            r#"{"nodes":[{"id":"a","fu_path":"a.fu"},{"id":"b","fu_path":"b.fu","depends_on":["a"]}],"max_steps":0}"#,
        )
        .unwrap();

        let graph = load_taiji_graph(&graph_path).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert!(graph.nodes[0].fu_text.contains("credential_ref"));

        let mut session = AltarSession::new();
        let receipt = session.run_taiji(&graph, false).unwrap();
        assert_eq!(receipt.order, vec!["a", "b"]);
        assert_eq!(receipt.failed, 1, "未确认的第一个节点必须失败");
        assert_eq!(receipt.skipped, 1, "依赖失败的第二个节点必须跳过");
        assert!(!receipt.converged);
        assert!(parse_taiji_graph("{").is_err(), "坏 JSON 必须明确报错");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T36：逐节点进度回调。回调序列必须与执行顺序一致，
    /// 且每个节点（成功 / 失败 / 跳过）恰好回调一次。
    #[test]
    fn taiji_progress_callbacks_cover_every_node_in_execution_order() {
        let sample = sample_fu_text().unwrap();
        let graph = TaijiGraph {
            nodes: vec![
                GraphNode {
                    id: "a".into(),
                    fu_text: sample.clone(),
                    fu_path: None,
                    depends_on: vec![],
                },
                GraphNode {
                    id: "b".into(),
                    fu_text: sample.clone(),
                    fu_path: None,
                    depends_on: vec!["a".into()],
                },
                GraphNode {
                    id: "c".into(),
                    fu_text: sample,
                    fu_path: None,
                    depends_on: vec![],
                },
            ],
            max_steps: 0,
        };
        let mut session = AltarSession::new();
        let mut seen: Vec<String> = Vec::new();
        let receipt = session
            .run_taiji_with_progress(&graph, false, &mut |result| {
                seen.push(result.id.clone());
            })
            .unwrap();
        assert_eq!(
            receipt.order,
            vec!["a", "c", "b"],
            "字典序分层：a、c 同批先行，b 随后"
        );
        assert_eq!(seen, receipt.order, "回调序列必须与执行顺序完全一致");
        assert_eq!(seen.len(), graph.nodes.len(), "跳过的节点也必须回调一次");
        assert!(
            receipt.failed + receipt.skipped == graph.nodes.len(),
            "未确认时不得有任何节点伪装成功"
        );
    }

    /// T10：召将子符的登记必须落在正确档位，且影响面进 dry-run 计划。
    #[test]
    fn summoning_registration_is_limited_mutate_with_quota() {
        let mut session = AltarSession::new();
        session.enable_summoning();
        assert!(session.summoning_enabled());
        assert!(session.tool_ids().iter().any(|id| id == SUMMON_TOOL_ID));
        let grant = session
            .register
            .tool(SUMMON_TOOL_ID)
            .expect("启用后箓里必须有 summon.fu");
        assert_eq!(
            grant.permission,
            fulu_register::Permission::Limited,
            "召将子符是受限级工具：只读符不得申请"
        );
        assert_eq!(grant.risk, RiskClass::Mutate, "分派子符每符确认一次");
        assert_eq!(grant.max_calls, 4, "分派配额必须有上限");
        let plan = session.dry_run(&Talisman::sample()).unwrap();
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("summon.fu") && note.contains("禁环回")),
            "确认页必须看见子符影响面：{:?}",
            plan.notes
        );
        // 只读符申请召将必须在箓层被双向核验驳回（子符不继承、父符也不能越权申请）。
        let mut strict = Talisman::sample();
        strict.tool_refs = vec![SUMMON_TOOL_ID.into()];
        strict.harness.allow_tools = vec![SUMMON_TOOL_ID.into()];
        assert!(matches!(
            session.register.authorize(&strict),
            Err(fulu_register::RegisterError::PermissionTooLow { .. })
        ));
    }

    // ---- T33/T34：保存、模型清单、缴符回放 ----

    #[test]
    fn dump_fu_round_trips_and_is_stable() {
        let fu = Talisman::sample();
        let text = dump_fu(&fu).unwrap();
        let back = parse_fu(&text).unwrap();
        assert_eq!(back, fu, "规范格式必须能原样解析回来");
        assert_eq!(dump_fu(&back).unwrap(), text, "二次规范化必须完全一致");
    }

    #[test]
    fn manifest_slots_mirror_the_embedded_manifest() {
        let slots = manifest_slots().unwrap();
        let manifest = ModelManifest::embedded().unwrap();
        assert_eq!(slots.len(), manifest.entries.len());
        for (slot, entry) in slots.iter().zip(manifest.entries.iter()) {
            assert_eq!(slot.slot, entry.slot);
            assert_eq!(slot.model_id, entry.model_id);
            // 「是否已固化摘要」必须与清单一致：未固化就是 false，不允许乐观默认。
            assert_eq!(slot.digest_pinned, entry.sha256.is_some());
        }
        assert!(slots.iter().any(|slot| slot.is_default && slot.runnable));
    }

    #[test]
    fn replay_audit_reads_lines_without_touching_the_file() {
        let dir = std::env::temp_dir().join(format!("fulu-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fulu-audit.jsonl");
        let body = "{\"seq\":1}\n\n{\"seq\":2}\n";
        std::fs::write(&path, body).unwrap();
        let session = AltarSession::new();
        let lines = session.replay_audit(&path).unwrap();
        assert_eq!(lines.len(), 2, "空行必须被忽略");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            body,
            "回放不得改文件"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T16 摄行：默认不存在、两把钥匙、逐次确认 ----

    #[test]
    fn shexing_is_absent_by_default_and_needs_both_keys() {
        let mut session = AltarSession::new();
        assert!(!session.shexing_enabled());
        assert!(
            !session
                .tool_ids()
                .iter()
                .any(|id| id.starts_with("shexing.")),
            "默认名册里不能出现摄行：{:?}",
            session.tool_ids()
        );
        // 环境变量判定：只有精确的 "1" 才算解锁，模糊值一概不算。
        assert!(shexing_unlock_matches(Some("1")));
        assert!(shexing_unlock_matches(Some(" 1 ")));
        for raw in [Some("0"), Some("true"), Some("yes"), Some(""), None] {
            assert!(!shexing_unlock_matches(raw), "不得把 {raw:?} 当解锁");
        }

        // 真正的解锁路径（跳过环境变量，直接测登记语义）。
        session.enable_shexing_unlocked();
        assert!(session.shexing_enabled());
        for id in fulu_tools::SHEXING_TOOL_IDS {
            assert!(
                session.tool_ids().iter().any(|known| known == id),
                "解锁后名册必须有 {id}"
            );
            let grant = session.register.tool(id).expect("授权必须登记");
            assert_eq!(grant.permission, fulu_register::Permission::Limited);
            assert_eq!(grant.risk, RiskClass::Destructive, "必须是逐次确认");
            assert_eq!(grant.max_calls, 20, "必须有配额，防无限点击");
        }
        // 确认等级映射：Destructive → EveryCall（不是每符一次）。
        assert_eq!(
            RiskClass::Destructive.confirmation(),
            Confirmation::EveryCall
        );
    }

    #[test]
    fn shexing_unlock_without_env_is_refused() {
        // 环境变量若被开发者手动设成 1，这条断言会自动跳过（不制造假红）。
        if shexing_unlock_matches(std::env::var("FULU_ALLOW_SHEXING").ok().as_deref()) {
            return;
        }
        let mut session = AltarSession::new();
        let error = session.enable_shexing().unwrap_err();
        assert!(format!("{error}").contains("FULU_ALLOW_SHEXING"));
        // 拒绝之后不能留下半开状态。
        assert!(!session.shexing_enabled());
        assert!(
            !session
                .tool_ids()
                .iter()
                .any(|id| id.starts_with("shexing."))
        );
    }

    /// T15：外宗里挑搜索类工具——只认名字/描述命中，query 键名优先采信 schema。
    #[test]
    fn pick_search_candidates_matches_names_and_schema_keys() {
        let listed = vec![
            (
                "mcp.cucumber.web_search".into(),
                "网页搜索".into(),
                r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#.into(),
            ),
            (
                "mcp.zzz.fetch".into(),
                "抓取页面内容".into(),
                r#"{"type":"object","properties":{"url":{"type":"string"}}}"#.into(),
            ),
            (
                "mcp.plain.search".into(),
                "search things".into(),
                "not-json".into(),
            ),
        ];
        let picked = pick_search_candidates(&listed);
        assert_eq!(
            picked,
            vec![
                ("mcp.cucumber.web_search".into(), "q".into()),
                // schema 解析失败 → 回退默认 query 键。
                ("mcp.plain.search".into(), "query".into()),
            ]
        );
    }

    /// T15：外宗没接 / 接了但没有搜索工具时，问天必须明确报错。
    #[test]
    fn ask_search_without_backend_fails_explicitly() {
        let mut session = AltarSession::new();
        let error = session.enable_ask_search().unwrap_err();
        assert!(error.to_string().contains("没有搜索类工具"), "{error}");
    }

    /// T15：显式绑定后端后，ask.search 可用且登记档位正确（limited + Destructive）。
    #[test]
    fn ask_search_binding_registers_limited_destructive_grant() {
        struct PongBackend;
        impl fulu_tools::SearchBackend for PongBackend {
            fn id(&self) -> &str {
                "test.pong"
            }
            fn search(&self, query: &str, _limit: usize) -> Result<String, fulu_tools::ToolError> {
                Ok(format!("pong: {query}"))
            }
        }
        let mut session = AltarSession::new();
        session.bind_ask_search_backends(vec![
            Arc::new(PongBackend) as Arc<dyn fulu_tools::SearchBackend>
        ]);
        session.note_search_backends(vec!["test.pong".into()]);
        assert_eq!(session.search_backend_ids(), vec!["test.pong".to_owned()]);
        let grant = session
            .register
            .tool("ask.search")
            .expect("baseline 已登记问天");
        assert_eq!(grant.permission, fulu_register::Permission::Limited);
        assert_eq!(
            grant.risk,
            RiskClass::Destructive,
            "代调外宗网络工具，逐次确认"
        );
        assert_eq!(grant.max_calls, 8);
        // 只读符申请问天在箓层被双向核验驳回。
        let mut strict = Talisman::sample();
        strict.tool_refs = vec!["ask.search".into()];
        strict.harness.allow_tools = vec!["ask.search".into()];
        assert!(matches!(
            session.register.authorize(&strict),
            Err(fulu_register::RegisterError::PermissionTooLow { .. })
        ));
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

    // ---- T40 箓牒：凭据库 ----

    /// 记账用的凭据库：包一层内存库，数一数 `get` 被调了几次。
    struct CountingVault {
        inner: fulu_vault::MemoryVault,
        /// 与测试共享的计数器：凭据库被搬进会话后，测试仍能看见它被摸了几次。
        gets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl CountingVault {
        fn new(
            reference: &str,
            secret: &str,
        ) -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
            let gets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            (
                Self {
                    inner: fulu_vault::MemoryVault::with_entry(reference, secret),
                    gets: gets.clone(),
                },
                gets,
            )
        }
    }

    impl fulu_vault::Vault for CountingVault {
        fn backend(&self) -> &'static str {
            "counting（测试）"
        }

        fn get(
            &self,
            reference: &fulu_vault::CredentialRef,
        ) -> Result<fulu_vault::Secret, fulu_vault::VaultError> {
            self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get(reference)
        }

        fn put(
            &self,
            reference: &fulu_vault::CredentialRef,
            secret: &fulu_vault::Secret,
        ) -> Result<(), fulu_vault::VaultError> {
            self.inner.put(reference, secret)
        }

        fn delete(
            &self,
            reference: &fulu_vault::CredentialRef,
        ) -> Result<(), fulu_vault::VaultError> {
            self.inner.delete(reference)
        }

        fn list(&self) -> Result<Vec<String>, fulu_vault::VaultError> {
            self.inner.list()
        }
    }

    /// 云端符：provider 不是本地回环，因此 `credential_ref` 也必须是真凭据引用。
    /// （本地 provider 会被 core 强制要求 `lu://default/local`，这是既有语义。）
    fn cloud_fu() -> Talisman {
        let mut fu = Talisman::sample();
        fu.head.provider_ref = "lu://cloud/openai-compatible".into();
        fu.credential_ref = "lu://default/cloud".into();
        fu
    }

    #[test]
    fn local_talismans_never_touch_a_vault() {
        let mut session = AltarSession::new();
        assert!(session.vault_backend().is_none(), "默认不装箓牒");
        let plan = session
            .dry_run(&Talisman::sample())
            .expect("本地符无需凭据");
        assert!(
            plan.notes.iter().any(|n| n.contains("本地模型无需凭据")),
            "确认页必须写明凭据来源：{:?}",
            plan.notes
        );
        assert!(session.vault_entries().expect("未装也应可查").is_empty());
    }

    #[test]
    fn cloud_talisman_without_a_vault_is_refused_before_burning() {
        let mut session = AltarSession::new();
        match session.dry_run(&cloud_fu()) {
            Err(AppError::Vault(detail)) => {
                assert!(detail.contains("没装箓牒"), "理由要说清楚：{detail}");
                assert!(detail.contains("fulu vault put"), "必须指路：{detail}");
            }
            other => panic!("未装箓牒时云端符必须被拒，实际：{other:?}"),
        }
    }

    #[test]
    fn vault_entries_become_granted_credentials() {
        let mut session = AltarSession::new();
        let granted = session
            .use_vault(Box::new(fulu_vault::MemoryVault::with_entry(
                "lu://default/cloud",
                "sk-1",
            )))
            .expect("装库");
        assert_eq!(granted, vec!["lu://default/cloud".to_owned()]);
        assert!(session.vault_backend().is_some());
        // 已入箓 → 授箓核验通过 → dry-run 能走到底
        let plan = session.dry_run(&cloud_fu()).expect("已入箓应可 dry-run");
        assert!(
            plan.notes.iter().any(|n| n.contains("已从箓牒取出")),
            "确认页必须写明凭据已就位：{:?}",
            plan.notes
        );
    }

    #[test]
    fn sealed_credential_is_injected_only_after_confirmation() {
        use std::sync::atomic::Ordering;
        let mut session = AltarSession::new();
        let (vault, gets) = CountingVault::new("lu://default/cloud", "sk-vault-1");
        session.use_vault(Box::new(vault)).expect("装库");
        let fu = cloud_fu();
        let plan = session.dry_run(&fu).expect("dry-run 通过");
        assert_eq!(
            gets.load(Ordering::SeqCst),
            1,
            "dry-run 取一次，用来判断取不取得到"
        );

        // 未确认：一个字都不多摸凭据库、不装令牌
        assert!(session.ignite(&fu, &plan, false).is_err());
        assert_eq!(gets.load(Ordering::SeqCst), 1, "未确认时不得再碰凭据库");
        assert_eq!(
            session
                .audit()
                .events()
                .iter()
                .filter(|e| e.kind == EventKind::Credential)
                .count(),
            0,
            "未确认前不得有凭据事件"
        );

        // 已确认：令牌注入 provider（随后因未设端点失败，但凭据这一步已经发生）
        assert!(
            session.ignite(&fu, &plan, true).is_err(),
            "未设端点必然失败"
        );
        assert!(session.provider_auth_enabled(), "确认后应已注入 Bearer");
        assert_eq!(gets.load(Ordering::SeqCst), 2, "确认后恰好再取一次");
        let credential_events: Vec<_> = session
            .audit()
            .events()
            .iter()
            .filter(|e| e.kind == EventKind::Credential)
            .collect();
        assert_eq!(credential_events.len(), 1);
        let detail = &credential_events[0].detail;
        assert!(detail.contains("lu://default/cloud"));
        assert!(
            !detail.contains("sk-vault-1"),
            "缴符里绝不能出现明文：{detail}"
        );
    }

    #[test]
    fn missing_entry_is_refused_even_when_the_vault_is_installed() {
        let mut session = AltarSession::new();
        session
            .use_vault(Box::new(fulu_vault::MemoryVault::with_entry(
                "lu://default/other",
                "sk-1",
            )))
            .expect("装库");
        // 库里没有 cloud 这条：授箓核验就该驳回，轮不到焚化
        assert!(session.dry_run(&cloud_fu()).is_err());
    }

    #[test]
    fn malformed_reference_is_caught_by_the_gate_not_ignored() {
        let mut session = AltarSession::new();
        session
            .use_vault(Box::new(fulu_vault::MemoryVault::new()))
            .expect("装库");
        let mut fu = Talisman::sample();
        // core 只校验 `lu://` 前缀；具体段格式由箓牒这一层挡住。
        fu.head.provider_ref = "lu://cloud/openai-compatible".into();
        fu.credential_ref = "lu://Default/cloud".into();
        match session.dry_run(&fu) {
            Err(AppError::Vault(detail)) => assert!(detail.contains("不合法")),
            other => panic!("畸形引用必须被拒，实际：{other:?}"),
        }
    }

    #[test]
    fn load_fu_file_accepts_all_three_forms_and_reports_which() {
        let dir = std::env::temp_dir().join(format!("fulu-app-t39-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let fu = Talisman::sample();

        let json_path = dir.join("a.fu");
        std::fs::write(&json_path, encode_fu(&fu).expect("编码")).expect("写 JSON");
        let (loaded, form) = load_fu_file(&json_path).expect("读 JSON");
        assert_eq!(form, FuForm::Json);
        assert_eq!(loaded, fu);

        let dsl_path = dir.join("b.dsl");
        std::fs::write(&dsl_path, fulu_script::encode_dsl(&fu)).expect("写 DSL");
        let (loaded, form) = load_fu_file(&dsl_path).expect("读 DSL");
        assert_eq!(form, FuForm::Dsl);
        assert_eq!(loaded, fu);

        let fub_path = dir.join("c.fub");
        std::fs::write(&fub_path, fulu_script::encode_fub(&fu, true).expect("打包"))
            .expect("写 fub");
        let (loaded, form) = load_fu_file(&fub_path).expect("读 fub");
        assert_eq!(form, FuForm::Fub);
        assert_eq!(loaded, fu);

        // 老入口 `load_fu` 也应当认这三种（CLI 的 altar run 直接受益）
        assert_eq!(load_fu(&dsl_path).expect("读 DSL"), fu);
        assert_eq!(load_fu(&fub_path).expect("读 fub"), fu);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_fu_file_rejects_garbage_instead_of_guessing() {
        let dir = std::env::temp_dir().join(format!("fulu-app-t39-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let bad = dir.join("bad.fu");
        std::fs::write(&bad, "这不是符，也不是 DSL\n").expect("写坏文件");
        assert!(load_fu_file(&bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
