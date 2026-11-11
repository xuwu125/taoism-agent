//! 法坛（Runtime）：把符箓仪轨落成真实约束（docs/02 第 4、5 节）。
//!
//! 一次执行严格分两段，中间**必须**有人的明确确认：
//! 1. `prepare`：成符 → 盖印（授箓核验）→ 待焚（dry-run）。全程零网络请求；
//! 2. `ignite`：确认 → 焚化 → 请神 / 召将循环 → 符脚判定 → 回执 → 缴符归档。
//!
//! 状态迁移一律走 `fulu_core::can_transition` 这个唯一裁判；异常态写事件后
//! 立即返回，不做静默重试。M2 采用同步执行器 + 可注入 `Provider`，
//! tokio 调度留在 M3 —— 单符循环不需要引入整个异步运行时。

use fulu_audit::{AuditEvent, AuditLog, EventKind};
use fulu_core::{Talisman, TalismanState, can_transition};
use fulu_provider::{InvocationRequest, Provider, ProviderError, StreamControl, StreamEvent};
use fulu_register::{Authorization, Confirmation, Register, RegisterError, RiskClass};
use fulu_script::{digest_hex, seal_hex};
use fulu_tools::{ToolArguments, ToolRegistry};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::converge::{Convergence, evaluate, parse};
use crate::gate::{ConfirmRequest, ConfirmVerdict, Confirmer};

/// 同一工具连续被拒到这个次数就停止焚化。
///
/// 模型很容易「换个说法再试一次」，而每次拒绝都会消耗 token 预算。与其让它
/// 撞到预算耗尽，不如在第二次被拒后明确停下——这不是惩罚，是省下无意义开销。
pub const MAX_DENIALS_PER_TOOL: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RitualError {
    Broken(String),
    Rejected(String),
    UndecidableCriteria(String),
    ConfirmationRequired,
    BudgetExceeded(String),
    Timeout(String),
    StepLimitExceeded(u32),
    Provider(String),
    /// 调用方主动中止（`StreamControl::Cancel`）。**不是故障**：既不并入炸炉，
    /// 也不并入过坛，与 `TalismanState::Cancelled` 一一对应。
    Cancelled(String),
    ToolRejected(String),
    Transition(String),
    Audit(String),
}

impl fmt::Display for RitualError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Broken(msg) => write!(f, "破符：{msg}"),
            Self::Rejected(msg) => write!(f, "驳回：{msg}"),
            Self::UndecidableCriteria(msg) => write!(f, "所请事项不可判定：{msg}"),
            Self::ConfirmationRequired => {
                write!(f, "待焚状态必须显式确认才能焚化（--confirm）")
            }
            Self::BudgetExceeded(msg) => write!(f, "炸炉 · 预算耗尽：{msg}"),
            Self::Timeout(msg) => write!(f, "过坛 · 超时：{msg}"),
            Self::StepLimitExceeded(limit) => write!(f, "炸炉 · 步数达到硬上限 {limit}"),
            Self::Provider(msg) => write!(f, "炸炉 · 推理失败：{msg}"),
            Self::Cancelled(msg) => write!(f, "中止 · 调用方取消：{msg}"),
            Self::ToolRejected(msg) => write!(f, "炸炉 · 召将失败：{msg}"),
            Self::Transition(msg) => write!(f, "仪轨迁移被拒：{msg}"),
            Self::Audit(msg) => write!(f, "缴符失败：{msg}"),
        }
    }
}

impl std::error::Error for RitualError {}

impl From<fulu_audit::AuditError> for RitualError {
    fn from(error: fulu_audit::AuditError) -> Self {
        Self::Audit(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RitualConfig {
    /// 整符超时（与 provider 的单次 HTTP 超时独立）
    pub timeout: Duration,
}

impl RitualConfig {
    pub fn from_talisman(fu: &Talisman) -> Self {
        Self {
            timeout: Duration::from_secs(u64::from(fu.harness.timeout_secs)),
        }
    }
}

/// dry-run 计划：焚化前必须向人展示的全部影响面。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunPlan {
    pub seal: String,
    pub model_id: String,
    pub provider_id: String,
    pub credential_ref: String,
    pub tools: Vec<String>,
    pub tool_quota: Vec<(String, u32)>,
    pub budget_tokens: u32,
    pub max_steps: u32,
    pub max_iterations: u32,
    pub timeout_secs: u32,
    pub network: bool,
    pub workspace_root: Option<PathBuf>,
    pub criteria: String,
    pub transitions: Vec<String>,
    pub notes: Vec<String>,
    /// 装配好的系统提示词摘要（T01/T02）：没有装配时为 None。
    /// 焚烧时重算并比对，防止「dry-run 时看的是一套提示词、执行时换了另一套」。
    pub system_digest: Option<String>,
}

impl DryRunPlan {
    /// 供 CLI 打印 / GUI 展示的逐行摘要。
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("符印       : sha256:{}", self.seal),
            format!("神明名号   : {}", self.model_id),
            format!("符头       : {}", self.provider_id),
            format!("敕令       : {}（仅引用，符内无密钥）", self.credential_ref),
            format!(
                "风火轮     : {}（配额 {:?}）",
                if self.tools.is_empty() {
                    "无".into()
                } else {
                    self.tools.join(", ")
                },
                self.tool_quota
            ),
            format!("token 预算 : {}", self.budget_tokens),
            format!(
                "硬上限     : max_steps={} / max_iterations={} / timeout={}s",
                self.max_steps, self.max_iterations, self.timeout_secs
            ),
            format!(
                "网络       : {}",
                if self.network { "开放" } else { "禁止" }
            ),
            format!(
                "文件边界   : {}",
                self.workspace_root
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "未配置（文件工具关闭）".into())
            ),
            format!("符脚判据   : {}", self.criteria),
            format!("仪轨       : {}", self.transitions.join(" → ")),
        ];
        if let Some(digest) = &self.system_digest {
            lines.push(format!("提示词装配 : sha256:{}", digest));
        }
        for note in &self.notes {
            lines.push(format!("提示       : {note}"));
        }
        lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub seal: String,
    pub state: TalismanState,
    pub text: String,
    pub steps: u32,
    pub usage_total: u32,
    pub estimated_usage: bool,
    /// 是否真正满足 `success_criteria`，不因循环耗尽而伪装成功
    pub converged: bool,
    /// 各步推理里 provider 实际重试的总次数（0 = 一次就成，仅展示用）
    pub retries_total: u32,
    pub events: Vec<AuditEvent>,
}

/// 符胆（技能包）摘要重校验。
///
/// 法坛不碰文件系统：由会话层注入实现（扫描用户全局技能目录，只读）。
/// 符文只引用 `sha256:` 摘要，两次焚符之间技能文件被改动就必须破符。
pub trait SkillVerifier: Send {
    fn verify(&self, refs: &[String]) -> Result<(), String>;
}

pub struct Altar<'a> {
    pub provider: &'a dyn Provider,
    pub tool_registry: &'a ToolRegistry,
    pub register: &'a Register,
    pub audit: &'a mut AuditLog,
    pub config: RitualConfig,
    /// 会话层装配好的系统提示词；None = 沿用符文自带的 `fu.system`。
    system_prompt: Option<String>,
    /// 装配说明（哪几层、多少字符），进 dry-run notes 供人复核。
    prompt_note: Option<String>,
    /// 符胆校验器；未装时不校验（库级调用保持旧行为）。
    skill_verifier: Option<&'a dyn SkillVerifier>,
    /// 未装确认器时一切需要确认的召将都被敕否（fail closed）。
    confirmer: Option<&'a mut dyn Confirmer>,
    /// 本符内已确认过的工具（对应 `Confirmation::OncePerTalisman`）
    confirmed_once: BTreeSet<String>,
    /// 每个工具被拒的次数，用于及时止损
    denials: BTreeMap<String, u32>,
    /// 流式增量去向。`None` = 非流式调用（默认）。
    ///
    /// 只在**显示层**用：增量不参与预算、不参与收敛判定、不改仪轨——
    /// 它只是让人早点看到字，而不是把「边流边执行」偷渡进来。
    stream_sink: Option<&'a mut dyn FnMut(StreamEvent) -> StreamControl>,
}

impl<'a> Altar<'a> {
    pub fn new(
        provider: &'a dyn Provider,
        tool_registry: &'a ToolRegistry,
        register: &'a Register,
        audit: &'a mut AuditLog,
        config: RitualConfig,
    ) -> Self {
        Self {
            provider,
            tool_registry,
            register,
            audit,
            config,
            system_prompt: None,
            prompt_note: None,
            skill_verifier: None,
            confirmer: None,
            confirmed_once: BTreeSet::new(),
            denials: BTreeMap::new(),
            stream_sink: None,
        }
    }

    /// 装上确认器。**不装就是拒绝**：需要确认的召将一律敕否并记账。
    ///
    /// 借而不是拿走，是为了让会话层（CLI / GUI）持有确认器并在多次仪轨间复用；
    /// 反过来若法坛持有 `Box`，每次焚符都得重新造一个，交互状态就断了。
    pub fn set_confirmer(&mut self, confirmer: &'a mut dyn Confirmer) {
        self.confirmer = Some(confirmer);
    }

    /// 注入会话层装配好的系统提示词（七层分层 + 会话头）。
    /// `note` 会原样进 dry-run 计划，让用户确认前看得见用了哪几层。
    pub fn set_system_prompt(&mut self, text: String, note: String) {
        self.system_prompt = Some(text);
        self.prompt_note = Some(note);
    }

    pub fn set_skill_verifier(&mut self, verifier: &'a dyn SkillVerifier) {
        self.skill_verifier = Some(verifier);
    }

    /// 装上流式增量接收器。**不装就不流**（默认非流式，行为与以前完全一致）。
    pub fn set_stream_sink(&mut self, sink: &'a mut dyn FnMut(StreamEvent) -> StreamControl) {
        self.stream_sink = Some(sink);
    }

    /// 符胆校验：只在装了校验器时生效，失败即破符并留痕，不静默跳过。
    fn verify_skills(&mut self, seal: &str, fu: &Talisman) -> Result<(), RitualError> {
        let Some(verifier) = self.skill_verifier else {
            return Ok(());
        };
        if fu.skill_refs.is_empty() {
            return Ok(());
        }
        if let Err(reason) = verifier.verify(&fu.skill_refs) {
            let message = format!("符胆摘要失配：{reason}");
            self.audit
                .append(seal, TalismanState::Broken, EventKind::Broken, &message)?;
            return Err(RitualError::Broken(message));
        }
        Ok(())
    }

    fn transition(&self, from: TalismanState, to: TalismanState) -> Result<(), RitualError> {
        can_transition(from, to).map_err(|e| RitualError::Transition(e.to_string()))
    }

    /// 成符 → 盖印 → 待焚。**不发起任何网络请求**。
    #[allow(clippy::too_many_lines)]
    pub fn prepare(&mut self, fu: &Talisman) -> Result<DryRunPlan, RitualError> {
        let seal = seal_hex(fu).map_err(|e| RitualError::Broken(e.to_string()))?;
        let problems = fu.validate();
        if !problems.is_empty() {
            self.audit.append(
                &seal,
                TalismanState::Broken,
                EventKind::Broken,
                &problems.to_string(),
            )?;
            return Err(RitualError::Broken(problems.to_string()));
        }

        // 符胆：成符时就重新校验内容摘要，技能包被改动即破符。
        self.verify_skills(&seal, fu)?;

        self.transition(TalismanState::Draft, TalismanState::Formed)?;
        self.audit.append(
            &seal,
            TalismanState::Formed,
            EventKind::Formed,
            "结构校验通过，内容冻结",
        )?;

        let auth = match self.register.authorize(fu) {
            Ok(auth) => auth,
            Err(error @ RegisterError::CredentialNotGranted(_))
            | Err(error @ RegisterError::ToolNotInRegister(_))
            | Err(error @ RegisterError::SandboxEscalation { .. })
            | Err(error @ RegisterError::PermissionTooLow { .. })
            | Err(error @ RegisterError::NetworkNotGranted(_))
            | Err(error @ RegisterError::QuotaNotPositive(_)) => {
                self.audit.append(
                    &seal,
                    TalismanState::Rejected,
                    EventKind::Rejected,
                    &error.to_string(),
                )?;
                return Err(RitualError::Rejected(error.to_string()));
            }
            Err(error) => {
                self.audit.append(
                    &seal,
                    TalismanState::Broken,
                    EventKind::Broken,
                    &error.to_string(),
                )?;
                return Err(RitualError::Broken(error.to_string()));
            }
        };

        // 判据不可判定时，必须在待焚前就破符，而不是焚化后假装完成。
        if parse(&fu.goal.success_criteria).is_none() {
            let message = format!(
                "不支持的判据「{}」；请使用 success / nonempty / output.len <= N / contains:<文本>",
                fu.goal.success_criteria
            );
            self.audit
                .append(&seal, TalismanState::Broken, EventKind::Broken, &message)?;
            return Err(RitualError::UndecidableCriteria(message));
        }

        self.transition(TalismanState::Formed, TalismanState::Sealed)?;
        self.audit.append(
            &seal,
            TalismanState::Sealed,
            EventKind::Sealed,
            &format!("授箓核验通过：{} 已受箓", auth.credential_ref),
        )?;
        self.transition(TalismanState::Sealed, TalismanState::Pending)?;

        let mut notes = vec![
            "dry-run 未发起任何网络请求，也未启动推理进程".into(),
            "权重与 llama-server 属进程外资源；未就绪时焚化会明确报「运行时未就绪」".into(),
        ];
        if auth.network {
            notes.push("符申请了网络能力，请复核域名白名单".into());
        }
        if let Some(note) = &self.prompt_note {
            notes.push(note.clone());
        }
        // 确认门必须在待焚前说清楚：哪些召将会停下来等人点头。
        let gated = auth.tools_needing_confirmation();
        if gated.is_empty() {
            notes.push("确认门无拦截：本次授权的工具均为只读观察，焚化不会中途停下来问人".into());
        } else {
            notes.push(format!(
                "确认门：{}——焚化时逐个请示；无人值守（未装确认器）一律敕否",
                gated
                    .iter()
                    .map(|(id, risk)| format!(
                        "{id}（{}·{}）",
                        risk.label(),
                        risk.confirmation().label()
                    ))
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }

        let plan = DryRunPlan {
            seal: seal.clone(),
            model_id: fu.deity.model_id.clone(),
            provider_id: self.provider.id().to_owned(),
            credential_ref: auth.credential_ref.clone(),
            tools: auth.allowed_tools.clone(),
            tool_quota: auth
                .max_calls
                .iter()
                .map(|(id, limit)| (id.clone(), *limit))
                .collect(),
            budget_tokens: fu.harness.budget_tokens,
            max_steps: fu.harness.max_steps,
            max_iterations: fu.output.max_iterations,
            timeout_secs: fu.harness.timeout_secs,
            network: auth.network,
            workspace_root: self.tool_registry.policy().root.clone(),
            criteria: fu.goal.success_criteria.clone(),
            transitions: vec![
                "草稿".into(),
                "成符".into(),
                "已盖印".into(),
                "待焚(dry-run)".into(),
            ],
            notes,
            system_digest: self
                .system_prompt
                .as_deref()
                .map(|text| digest_hex(text.as_bytes())),
        };
        self.audit.append(
            &seal,
            TalismanState::Pending,
            EventKind::DryRun,
            &format!(
                "dry-run：model={} tools={:?} budget={} network={}",
                plan.model_id, plan.tools, plan.budget_tokens, plan.network
            ),
        )?;
        Ok(plan)
    }

    /// 焚化。**`confirmed` 必须为 true**，否则原地停在待焚。
    #[allow(clippy::too_many_lines)]
    pub fn ignite(
        &mut self,
        fu: &Talisman,
        plan: &DryRunPlan,
        confirmed: bool,
    ) -> Result<Receipt, RitualError> {
        if !confirmed {
            return Err(RitualError::ConfirmationRequired);
        }
        let seal = seal_hex(fu).map_err(|e| RitualError::Broken(e.to_string()))?;
        if seal != plan.seal {
            self.audit.append(
                &seal,
                TalismanState::Broken,
                EventKind::Broken,
                "符文在 dry-run 之后被改动，必须重新成符盖印",
            )?;
            return Err(RitualError::Broken(
                "符文在 dry-run 之后被改动，必须重新成符盖印".into(),
            ));
        }
        // dry-run 与焚化之间提示词不能偷换：装配结果也纳入一致性校验。
        let system_digest = self
            .system_prompt
            .as_deref()
            .map(|text| digest_hex(text.as_bytes()));
        if system_digest != plan.system_digest {
            let message = "系统提示词在 dry-run 之后被改动，必须重新成符盖印".to_owned();
            self.audit
                .append(&seal, TalismanState::Broken, EventKind::Broken, &message)?;
            return Err(RitualError::Broken(message));
        }
        self.verify_skills(&seal, fu)?;
        let auth = self
            .register
            .authorize(fu)
            .map_err(|e| RitualError::Rejected(e.to_string()))?;
        let criterion = parse(&fu.goal.success_criteria)
            .ok_or_else(|| RitualError::UndecidableCriteria(fu.goal.success_criteria.clone()))?;
        let tools_json = build_tools_json(&auth, self.register);

        self.transition(TalismanState::Pending, TalismanState::Igniting)?;
        self.audit.append(
            &seal,
            TalismanState::Igniting,
            EventKind::Ignited,
            "已显式确认，焚符上表；此点起不可撤销",
        )?;

        let started = Instant::now();
        let mut remaining = fu.harness.budget_tokens;
        let mut spent_total = 0_u32;
        let mut estimated_usage = false;
        let mut retries_total = 0_u32;
        let mut used_tools: BTreeMap<String, u32> = BTreeMap::new();
        let mut user_input = fu.user.clone();
        let mut final_text = String::new();
        let mut steps = 0_u32;
        let mut converged = false;
        let mut completed = false;

        for step in 0..fu.harness.max_steps {
            steps = step + 1;
            if started.elapsed() >= self.config.timeout {
                return self.abort(
                    &seal,
                    TalismanState::TimedOut,
                    EventKind::TimedOut,
                    "整符超时",
                    RitualError::Timeout(format!("超过 {:?}", self.config.timeout)),
                );
            }
            if remaining == 0 {
                return self.abort(
                    &seal,
                    TalismanState::Exploded,
                    EventKind::Exploded,
                    "token 预算耗尽",
                    RitualError::BudgetExceeded(format!("累计消耗 {spent_total} tokens")),
                );
            }

            let request = InvocationRequest {
                model_id: fu.deity.model_id.clone(),
                system: self
                    .system_prompt
                    .clone()
                    .unwrap_or_else(|| fu.system.clone()),
                user: user_input.clone(),
                tools_json: tools_json.clone(),
                sampling: fu.deity.sampling.clone(),
                budget_tokens: remaining,
                context_tokens: self.provider.capabilities().max_context_tokens,
            };
            // 流式只在显示层：增量已由 sink 接走，返回值仍是一份完整回执，
            // 后面的预算 / 工具核验 / 收敛判定一个字都不改。
            let mut partial_chars = 0_usize;
            let provider = self.provider;
            let reply = {
                let streamed = match self.stream_sink.as_mut() {
                    Some(sink) if provider.capabilities().streaming => {
                        let mut forward = |event: StreamEvent| -> StreamControl {
                            if let StreamEvent::TextDelta(text) = &event {
                                partial_chars += text.chars().count();
                            }
                            sink(event)
                        };
                        Some(provider.invoke_stream(&request, &mut forward))
                    }
                    // 装了 sink 但 provider 不支持流式：**不静默退化**，直接报错，
                    // 免得界面以为自己在看增量，其实拿到一整块。
                    Some(_) => Some(Err(ProviderError::NotImplemented(
                        "该 provider 不支持流式；请去掉 --stream 或换本地 provider",
                    ))),
                    None => None,
                };
                match streamed {
                    Some(result) => result,
                    None => provider.invoke(&request),
                }
            };
            let reply = match reply {
                Ok(reply) => reply,
                Err(ProviderError::Cancelled(reason)) => {
                    let detail = if partial_chars == 0 {
                        format!("焚化被取消（{reason}）")
                    } else {
                        format!("焚化被取消（{reason}）；已收 {partial_chars} 字符，仅记账不回喂")
                    };
                    return self.abort(
                        &seal,
                        TalismanState::Cancelled,
                        EventKind::Cancelled,
                        &detail,
                        RitualError::Cancelled(detail.clone()),
                    );
                }
                Err(ProviderError::Timeout) => {
                    return self.abort(
                        &seal,
                        TalismanState::TimedOut,
                        EventKind::TimedOut,
                        "推理调用超时",
                        RitualError::Timeout("provider 返回超时".into()),
                    );
                }
                Err(ProviderError::NotConfigured(msg)) => {
                    return self.abort(
                        &seal,
                        TalismanState::Exploded,
                        EventKind::Exploded,
                        &msg,
                        RitualError::Provider(msg.clone()),
                    );
                }
                Err(other) => {
                    let mut message = other.to_string();
                    if partial_chars > 0 {
                        // 半途断流不许装成功：把已收字数如实记账，再按炸炉处理。
                        message.push_str(&format!(
                            "（流式中断：已收 {partial_chars} 字符，仅记账不回喂模型）"
                        ));
                    }
                    return self.abort(
                        &seal,
                        TalismanState::Exploded,
                        EventKind::Exploded,
                        &message,
                        RitualError::Provider(message.clone()),
                    );
                }
            };
            if started.elapsed() >= self.config.timeout {
                return self.abort(
                    &seal,
                    TalismanState::TimedOut,
                    EventKind::TimedOut,
                    "推理返回时已超过整符时限",
                    RitualError::Timeout(format!("超过 {:?}", self.config.timeout)),
                );
            }

            let spent = reply.usage.total();
            estimated_usage |= reply.usage.estimated;
            retries_total += u32::from(reply.retries);
            spent_total = spent_total.saturating_add(spent);
            // 至少递减 1：服务端不给 usage 时也不会因 0 消耗而无限循环。
            remaining = remaining.saturating_sub(spent.max(1));
            self.audit.append(
                &seal,
                TalismanState::Igniting,
                EventKind::Invoked,
                &format!(
                    "请神 provider={} tokens={} estimated={} 重试={} 剩余预算={}",
                    self.provider.id(),
                    spent,
                    reply.usage.estimated,
                    reply.retries,
                    remaining
                ),
            )?;

            if !reply.tool_calls.is_empty() {
                for call in &reply.tool_calls {
                    self.audit.append(
                        &seal,
                        TalismanState::Igniting,
                        EventKind::ToolCall,
                        &format!("召将 name={} id={}", call.name, call.id),
                    )?;
                    // 敕门：先问人，再动手。风险等级来自箓牒；外宗工具的那一维
                    // 来自 server 自述，只用于收紧确认，从不放宽权限。
                    let (risk, description) = match self.register.tool(&call.name) {
                        Some(grant) => (grant.risk, grant.description.clone()),
                        // 能走到这里说明箓层已放行；查不到仍按最严处理。
                        None => (RiskClass::Destructive, String::new()),
                    };
                    let confirmation = risk.confirmation();
                    let needs_confirm = match confirmation {
                        Confirmation::None => false,
                        Confirmation::OncePerTalisman => !self.confirmed_once.contains(&call.name),
                        Confirmation::EveryCall => true,
                    };
                    if needs_confirm {
                        let request = ConfirmRequest {
                            seal: seal.clone(),
                            tool: call.name.clone(),
                            risk,
                            confirmation,
                            description,
                            arguments_json: call.arguments_json.clone(),
                            attempt: used_tools.get(&call.name).copied().unwrap_or(0) + 1,
                        };
                        let verdict = match self.confirmer.as_mut() {
                            Some(confirmer) => confirmer.confirm(&request),
                            None => ConfirmVerdict::Deny,
                        };
                        if verdict == ConfirmVerdict::Allow {
                            self.audit.append(
                                &seal,
                                TalismanState::Igniting,
                                EventKind::Confirmed,
                                &format!(
                                    "敕可 {} 参数={}",
                                    request.summary(),
                                    clip(&request.arguments_json, 240)
                                ),
                            )?;
                            if confirmation == Confirmation::OncePerTalisman {
                                self.confirmed_once.insert(call.name.clone());
                            }
                        } else {
                            let count = {
                                let entry = self.denials.entry(call.name.clone()).or_insert(0);
                                *entry += 1;
                                *entry
                            };
                            let why = if self.confirmer.is_none() {
                                "无人值守：未装确认器，需要确认的召将一律敕否"
                            } else {
                                "确认被拒"
                            };
                            self.audit.append(
                                &seal,
                                TalismanState::Igniting,
                                EventKind::Denied,
                                &format!("敕否 {} 原因={why} 累计={count}", request.summary()),
                            )?;
                            if count > MAX_DENIALS_PER_TOOL {
                                let message =
                                    format!("{} 连续 {count} 次未获确认，停止焚化", call.name);
                                return self.abort(
                                    &seal,
                                    TalismanState::Exploded,
                                    EventKind::Exploded,
                                    &message,
                                    RitualError::ToolRejected(message.clone()),
                                );
                            }
                            // 不把拒绝当炸炉：把理由交回模型，让它换一条路。
                            user_input.push_str(&format!(
                                "\n\n[召将未获确认 {}]\n{why}。风险等级：{}。\n\
                                 不要重复请求该工具；改用不需要确认的路径，\
                                 或在回答中说明你需要它做什么。",
                                call.name,
                                risk.label()
                            ));
                            continue;
                        }
                    }

                    let arguments = ToolArguments {
                        json: call.arguments_json.clone(),
                    };
                    let output = match self.tool_registry.execute(
                        &call.name,
                        &arguments,
                        &auth,
                        &mut used_tools,
                    ) {
                        Ok(output) => output,
                        Err(error) => {
                            let message = error.to_string();
                            return self.abort(
                                &seal,
                                TalismanState::Exploded,
                                EventKind::Exploded,
                                &message,
                                RitualError::ToolRejected(message.clone()),
                            );
                        }
                    };
                    self.audit.append(
                        &seal,
                        TalismanState::Igniting,
                        EventKind::ToolResult,
                        &format!("将回 name={} bytes={}", call.name, output.content.len()),
                    )?;
                    user_input
                        .push_str(&format!("\n\n[工具回执 {}]\n{}", call.name, output.content));
                }
                continue;
            }

            final_text = reply.text.clone();
            match evaluate(&criterion, &final_text) {
                Convergence::Achieved => {
                    converged = true;
                    completed = true;
                    break;
                }
                Convergence::Pending(reason) => {
                    self.audit.append(
                        &seal,
                        TalismanState::Igniting,
                        EventKind::Invoked,
                        &format!("符脚未收敛：{reason}"),
                    )?;
                    if step + 1 >= fu.output.max_iterations {
                        completed = true;
                        break;
                    }
                    user_input.push_str(&format!(
                        "\n\n[符脚反馈] {reason}；请按 success_criteria 重写。"
                    ));
                }
            }
        }

        if !completed {
            return self.abort(
                &seal,
                TalismanState::Exploded,
                EventKind::Exploded,
                "步数达到硬上限",
                RitualError::StepLimitExceeded(fu.harness.max_steps),
            );
        }

        self.transition(TalismanState::Igniting, TalismanState::Answered)?;
        self.audit.append(
            &seal,
            TalismanState::Answered,
            EventKind::Answered,
            &format!(
                "回执 steps={} converged={} usage={} estimated={}",
                steps, converged, spent_total, estimated_usage
            ),
        )?;
        self.transition(TalismanState::Answered, TalismanState::Archived)?;
        self.audit.append(
            &seal,
            TalismanState::Archived,
            EventKind::Archived,
            "缴符归档，事件链只读可回放",
        )?;
        self.audit.verify_chain()?;

        Ok(Receipt {
            seal: seal.clone(),
            state: TalismanState::Archived,
            text: final_text,
            steps,
            usage_total: spent_total,
            estimated_usage,
            converged,
            retries_total,
            events: self.audit.replay(&seal).into_iter().cloned().collect(),
        })
    }

    fn abort<T>(
        &mut self,
        seal: &str,
        state: TalismanState,
        kind: EventKind,
        detail: &str,
        error: RitualError,
    ) -> Result<T, RitualError> {
        self.audit.append(seal, state, kind, detail)?;
        Err(error)
    }
}

/// 截断过长文本用于事件详情，避免一条缴符事件被整段参数撑爆。
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(limit).collect();
    clipped.push('…');
    clipped
}

/// 把「箓」中已受箓的工具定义转成 OpenAI 兼容 tools 数组。
///
/// 参数 schema 一律取自箓牒（`ToolGrant::parameters_json`）：内置工具在
/// baseline 里写好，外宗（MCP）工具用 server 声明的真实 `inputSchema`。
/// 解析失败退回空参数——宁可让模型少知道，也不把坏 JSON 递进请求体。
pub fn build_tools_json(auth: &Authorization, register: &Register) -> String {
    let tools: Vec<Value> = auth
        .allowed_tools
        .iter()
        .filter_map(|id| {
            let grant = register.tool(id)?;
            let parameters = serde_json::from_str::<Value>(&grant.parameters_json)
                .ok()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            Some(json!({
                "type": "function",
                "function": {
                    "name": id,
                    "description": grant.description,
                    "parameters": parameters
                }
            }))
        })
        .collect();
    serde_json::to_string(&Value::Array(tools)).unwrap_or_else(|_| "[]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_audit::EventKind;
    use fulu_provider::{InvocationReply, StubProvider, ToolCall, Usage, text_reply};
    use fulu_register::ToolGrant;
    use std::sync::{Arc, Mutex};

    fn fixture() -> (Talisman, Register, ToolRegistry, AuditLog) {
        (
            Talisman::sample(),
            Register::baseline(),
            ToolRegistry::default(),
            AuditLog::new(),
        )
    }

    fn altar<'a>(
        provider: &'a StubProvider,
        registry: &'a ToolRegistry,
        register: &'a Register,
        audit: &'a mut AuditLog,
    ) -> Altar<'a> {
        Altar::new(
            provider,
            registry,
            register,
            audit,
            RitualConfig {
                timeout: Duration::from_secs(2),
            },
        )
    }

    #[test]
    fn prepare_never_touches_the_network_and_lands_in_pending() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        let plan = altar(&provider, &tools, &register, &mut audit)
            .prepare(&fu)
            .unwrap();
        assert_eq!(provider.call_count(), 0, "dry-run 不得请神");
        assert_eq!(plan.tools, vec!["fs.read"]);
        assert_eq!(plan.budget_tokens, 8000);
        assert_eq!(audit.events().last().unwrap().state, TalismanState::Pending);
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("未发起任何网络请求"))
        );
        audit.verify_chain().unwrap();
    }

    #[test]
    fn streaming_is_display_only_and_never_changes_the_receipt() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        provider.push_reply(text_reply("这是摘要", 10, 20));
        let mut collected = String::new();
        let mut events = 0_usize;
        let mut sink = |event: StreamEvent| -> StreamControl {
            events += 1;
            if let StreamEvent::TextDelta(text) = event {
                collected.push_str(&text);
            }
            StreamControl::Continue
        };
        let receipt = {
            let mut altar = altar(&provider, &tools, &register, &mut audit);
            altar.set_stream_sink(&mut sink);
            let plan = altar.prepare(&fu).unwrap();
            altar.ignite(&fu, &plan, true).unwrap()
        };
        // 流式与非流式的回执必须一致：字、用量、收敛、终态一个都不能变。
        assert_eq!(receipt.text, "这是摘要");
        assert_eq!(receipt.usage_total, 30);
        assert!(receipt.converged);
        assert_eq!(receipt.state, TalismanState::Archived);
        assert_eq!(collected, "这是摘要");
        assert!(
            events >= 3,
            "至少应有文本增量 + usage + done，实收 {events}"
        );
    }

    #[test]
    fn cancelling_mid_stream_lands_in_cancelled_and_records_it() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        provider.push_reply(text_reply("一二三四五六七八九十", 10, 20));
        let mut sink = |event: StreamEvent| -> StreamControl {
            if matches!(event, StreamEvent::TextDelta(_)) {
                // 第一段就取消：验证「取消即停」而不是把剩下的一起收完。
                return StreamControl::Cancel;
            }
            StreamControl::Continue
        };
        let error = {
            let mut altar = altar(&provider, &tools, &register, &mut audit);
            altar.set_stream_sink(&mut sink);
            let plan = altar.prepare(&fu).unwrap();
            let error = altar.ignite(&fu, &plan, true).unwrap_err();
            let last = altar.audit.events().last().cloned().unwrap();
            assert_eq!(last.kind, EventKind::Cancelled);
            assert_eq!(last.state, TalismanState::Cancelled);
            assert!(last.detail.contains("已收"), "应如实记账已收字数");
            error
        };
        // 取消不是成功，也不是炸炉：终态是「中止」，并且没有回执正文。
        // 错误类型也必须是独立的 `Cancelled`——否则调用方只能靠解析文案区分，
        // 迟早会把「人中止」显示成「炸炉」。
        assert!(
            matches!(error, RitualError::Cancelled(ref msg) if msg.contains("取消")),
            "取消必须落独立错误变体：{error:?}"
        );
        assert!(
            error.to_string().starts_with("中止"),
            "文案不许写成炸炉：{error}"
        );
    }

    /// 取消必须发生在**召将执行之前**：工具调用是「收齐才执行」的，
    /// 收一半就取消时一个工具都不许跑——否则「中止」会变成「已经动过手却没记账」。
    #[test]
    fn cancelling_during_tool_call_delta_executes_no_tool() {
        let (_, register, tools, mut audit) = fixture();
        let fu = fu_with_echo();
        let provider = StubProvider::default();
        provider.push_reply(call_echo());
        let mut sink = |event: StreamEvent| -> StreamControl {
            if matches!(event, StreamEvent::ToolCallDelta { .. }) {
                return StreamControl::Cancel;
            }
            StreamControl::Continue
        };
        let error = {
            let mut altar = altar(&provider, &tools, &register, &mut audit);
            altar.set_stream_sink(&mut sink);
            let plan = altar.prepare(&fu).unwrap();
            let error = altar.ignite(&fu, &plan, true).unwrap_err();
            assert!(
                !altar
                    .audit
                    .events()
                    .iter()
                    .any(|event| event.kind == EventKind::ToolResult),
                "取消不得让已收一半的召将执行"
            );
            let last = altar.audit.events().last().cloned().unwrap();
            assert_eq!(last.kind, EventKind::Cancelled);
            assert_eq!(last.state, TalismanState::Cancelled);
            error
        };
        assert!(matches!(error, RitualError::Cancelled(_)), "{error:?}");
        assert_eq!(provider.call_count(), 1, "取消不是故障，不得自动重试");
    }

    #[test]
    fn a_provider_without_streaming_is_refused_instead_of_silently_degrading() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default().disable_streaming();
        let mut sink = |_event: StreamEvent| -> StreamControl { StreamControl::Continue };
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        altar.set_stream_sink(&mut sink);
        let plan = altar.prepare(&fu).unwrap();
        let error = altar.ignite(&fu, &plan, true).unwrap_err();
        assert!(
            matches!(error, RitualError::Provider(ref msg) if msg.contains("不支持流式")),
            "应显式报错：{error:?}"
        );
        assert_eq!(provider.call_count(), 0, "不得退化成非流式调用");
    }

    #[test]
    fn confirmation_is_mandatory() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&fu).unwrap();
        assert_eq!(
            altar.ignite(&fu, &plan, false),
            Err(RitualError::ConfirmationRequired)
        );
        assert_eq!(provider.call_count(), 0);
    }

    #[test]
    fn full_happy_path_goes_through_every_state() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        provider.push_reply(text_reply("这是摘要", 10, 20));
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&fu).unwrap();
        let receipt = altar.ignite(&fu, &plan, true).unwrap();
        assert_eq!(receipt.state, TalismanState::Archived);
        assert_eq!(receipt.text, "这是摘要");
        assert!(receipt.converged);
        assert_eq!(receipt.usage_total, 30);
        let states: Vec<_> = receipt.events.iter().map(|event| event.state).collect();
        assert!(states.contains(&TalismanState::Formed));
        assert!(states.contains(&TalismanState::Sealed));
        assert!(states.contains(&TalismanState::Pending));
        assert!(states.contains(&TalismanState::Igniting));
        assert!(states.contains(&TalismanState::Answered));
        assert!(states.contains(&TalismanState::Archived));
        audit.verify_chain().unwrap();
    }

    /// `docs/03` 第 2.5 节的承诺：重试不许静默——provider 报上来的重试数
    /// 必须同时落在缴符事件与回执里（provider 侧的计数由 `retry_e2e` 真机覆盖）。
    #[test]
    fn provider_retry_count_lands_in_audit_event_and_receipt() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        // 桩不跑退避循环；这里手工给一个「provider 内部重试过 2 次」的回执，
        // 只验证法坛的记账：缴符事件带 重试=2，回执 retries_total == 2。
        provider.push_reply(InvocationReply {
            text: "这是摘要".into(),
            tool_calls: Vec::new(),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 20,
                estimated: false,
            },
            retries: 2,
        });
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&fu).unwrap();
        let receipt = altar.ignite(&fu, &plan, true).unwrap();
        assert_eq!(receipt.retries_total, 2);
        let invoked = receipt
            .events
            .iter()
            .find(|event| event.kind == EventKind::Invoked)
            .expect("一次成功的焚符必须有一条请神事件");
        assert!(
            invoked.detail.contains("重试=2"),
            "请神事件必须带重试计数：{}",
            invoked.detail
        );
    }

    #[test]
    fn tool_loop_feeds_results_back_into_the_next_request() {
        let (fu, register, tools, mut audit) = fixture();
        let mut custom = fu.clone();
        custom.tool_refs = vec!["echo".into()];
        custom.harness.allow_tools = vec!["echo".into()];
        let provider = StubProvider::default();
        provider.push_reply(InvocationReply {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments_json: r#"{"text":"回执内容"}"#.into(),
            }],
            usage: Usage {
                prompt_tokens: 5,
                completion_tokens: 5,
                estimated: false,
            },
            retries: 0,
        });
        provider.push_reply(text_reply("完成", 5, 5));
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&custom).unwrap();
        let receipt = altar.ignite(&custom, &plan, true).unwrap();
        assert_eq!(receipt.steps, 2);
        assert!(
            provider
                .last_request()
                .unwrap()
                .user
                .contains("[工具回执 echo]")
        );
        assert!(provider.last_request().unwrap().user.contains("回执内容"));
        assert!(
            receipt
                .events
                .iter()
                .any(|event| event.kind == EventKind::ToolResult)
        );
    }

    /// 把 echo 的风险改成指定等级：echo 本身已注册在默认 registry 里，
    /// 这样可以在不引入假工具的情况下验证确认门的三种策略。
    fn register_with_echo_risk(risk: RiskClass) -> Register {
        let mut register = Register::baseline();
        let mut grant = register.tool("echo").unwrap().clone();
        grant.risk = risk;
        register.grant_tool(grant);
        register
    }

    fn fu_with_echo() -> Talisman {
        let mut fu = Talisman::sample();
        fu.tool_refs = vec!["echo".into()];
        fu.harness.allow_tools = vec!["echo".into()];
        fu
    }

    fn call_echo() -> InvocationReply {
        InvocationReply {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c-echo".into(),
                name: "echo".into(),
                arguments_json: r#"{"text":"急急如律令"}"#.into(),
            }],
            usage: Usage {
                prompt_tokens: 5,
                completion_tokens: 5,
                estimated: false,
            },
            retries: 0,
        }
    }

    struct RecordingGate {
        allow: RiskClass,
        seen: Arc<Mutex<Vec<ConfirmRequest>>>,
    }

    impl RecordingGate {
        fn new(allow: RiskClass) -> (Self, Arc<Mutex<Vec<ConfirmRequest>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    allow,
                    seen: Arc::clone(&seen),
                },
                seen,
            )
        }
    }

    impl Confirmer for RecordingGate {
        fn confirm(&mut self, request: &ConfirmRequest) -> ConfirmVerdict {
            self.seen.lock().unwrap().push(request.clone());
            if request.risk <= self.allow {
                ConfirmVerdict::Allow
            } else {
                ConfirmVerdict::Deny
            }
        }
    }

    #[test]
    fn destructive_tool_is_denied_when_nobody_is_watching() {
        let register = register_with_echo_risk(RiskClass::Destructive);
        let tools = ToolRegistry::default();
        let fu = fu_with_echo();
        let provider = StubProvider::default();
        provider.push_reply(call_echo());
        provider.push_reply(text_reply("改用文字作答", 5, 5));
        let mut audit = AuditLog::new();
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&fu).unwrap();
        // 待焚计划必须先说清楚有门，且门是默认关着的。
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("确认门") && note.contains("无人值守"))
        );

        let receipt = altar.ignite(&fu, &plan, true).unwrap();
        assert!(receipt.converged);
        assert!(
            receipt
                .events
                .iter()
                .any(|event| event.kind == EventKind::Denied),
            "未装确认器时必须留下敕否事件"
        );
        assert!(
            !receipt
                .events
                .iter()
                .any(|event| event.kind == EventKind::ToolResult),
            "被敕否的工具绝不能被真的执行"
        );
        assert!(
            provider
                .last_request()
                .unwrap()
                .user
                .contains("召将未获确认 echo"),
            "模型必须知道为什么没召成，才能换一条路"
        );
        audit.verify_chain().unwrap();
    }

    #[test]
    fn destructive_tool_runs_only_under_an_explicit_ceiling() {
        let register = register_with_echo_risk(RiskClass::Destructive);
        let tools = ToolRegistry::default();
        let fu = fu_with_echo();
        let provider = StubProvider::default();
        provider.push_reply(call_echo());
        provider.push_reply(text_reply("完成", 5, 5));
        let mut audit = AuditLog::new();
        let (mut gate, seen) = RecordingGate::new(RiskClass::Destructive);
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        altar.set_confirmer(&mut gate);
        let plan = altar.prepare(&fu).unwrap();
        let receipt = altar.ignite(&fu, &plan, true).unwrap();

        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(
            receipt
                .events
                .iter()
                .any(|event| event.kind == EventKind::Confirmed)
        );
        assert!(
            receipt
                .events
                .iter()
                .any(|event| event.kind == EventKind::ToolResult)
        );
    }

    #[test]
    fn mutating_tool_asks_once_per_talisman() {
        let register = register_with_echo_risk(RiskClass::Mutate);
        let tools = ToolRegistry::default();
        let fu = fu_with_echo();
        let provider = StubProvider::default();
        provider.push_reply(call_echo());
        provider.push_reply(call_echo());
        provider.push_reply(text_reply("完成", 5, 5));
        let mut audit = AuditLog::new();
        let (mut gate, seen) = RecordingGate::new(RiskClass::Mutate);
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        altar.set_confirmer(&mut gate);
        let plan = altar.prepare(&fu).unwrap();
        let receipt = altar.ignite(&fu, &plan, true).unwrap();

        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "可变更工具同一张符只该问一次"
        );
        let tool_results = receipt
            .events
            .iter()
            .filter(|event| event.kind == EventKind::ToolResult)
            .count();
        assert_eq!(tool_results, 2, "确认过了就该真的执行两次");
    }

    #[test]
    fn repeated_denials_stop_the_ritual_instead_of_burning_budget() {
        let register = register_with_echo_risk(RiskClass::Destructive);
        let tools = ToolRegistry::default();
        let fu = fu_with_echo();
        let provider = StubProvider::default();
        for _ in 0..3 {
            provider.push_reply(call_echo());
        }
        let mut audit = AuditLog::new();
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&fu).unwrap();
        assert!(matches!(
            altar.ignite(&fu, &plan, true),
            Err(RitualError::ToolRejected(_))
        ));
        assert_eq!(
            audit
                .events()
                .iter()
                .filter(|event| event.kind == EventKind::Denied)
                .count(),
            (MAX_DENIALS_PER_TOOL + 1) as usize
        );
    }

    #[test]
    fn unregistered_tool_is_rejected_before_any_inference() {
        let (mut fu, register, tools, mut audit) = fixture();
        fu.tool_refs = vec!["shell.exec".into()];
        fu.harness.allow_tools = vec!["shell.exec".into()];
        let provider = StubProvider::default();
        let result = altar(&provider, &tools, &register, &mut audit).prepare(&fu);
        assert!(matches!(result, Err(RitualError::Rejected(_))));
        assert_eq!(provider.call_count(), 0);
        assert!(
            audit
                .events()
                .iter()
                .any(|event| event.state == TalismanState::Rejected)
        );
    }

    #[test]
    fn undecidable_criteria_breaks_before_pending() {
        let (mut fu, register, tools, mut audit) = fixture();
        fu.goal.success_criteria = "让它更好一点".into();
        let provider = StubProvider::default();
        assert!(matches!(
            altar(&provider, &tools, &register, &mut audit).prepare(&fu),
            Err(RitualError::UndecidableCriteria(_))
        ));
    }

    #[test]
    fn modifying_fu_after_dry_run_breaks_the_seal() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&fu).unwrap();
        let mut changed = fu.clone();
        changed.name = "被改动的符".into();
        assert!(matches!(
            altar.ignite(&changed, &plan, true),
            Err(RitualError::Broken(_))
        ));
    }

    #[test]
    fn budget_exhaustion_explodes() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        // 第一轮不达标（超过 200 字）且把预算吃满，第二轮必然在预算检查处炸炉。
        provider.push_reply(text_reply(&"符".repeat(201), 7000, 1000));
        provider.push_reply(text_reply("第二轮", 10, 10));
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        let plan = altar.prepare(&fu).unwrap();
        assert!(matches!(
            altar.ignite(&fu, &plan, true),
            Err(RitualError::BudgetExceeded(_))
        ));
        assert!(
            audit
                .events()
                .iter()
                .any(|event| event.state == TalismanState::Exploded)
        );
    }

    #[test]
    fn timeout_ends_the_ritual() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default().with_delay(Duration::from_millis(30));
        provider.push_reply(text_reply("太慢了", 1, 1));
        let mut altar = Altar::new(
            &provider,
            &tools,
            &register,
            &mut audit,
            RitualConfig {
                timeout: Duration::from_millis(5),
            },
        );
        let plan = altar.prepare(&fu).unwrap();
        assert!(matches!(
            altar.ignite(&fu, &plan, true),
            Err(RitualError::Timeout(_))
        ));
        assert!(
            audit
                .events()
                .iter()
                .any(|event| event.state == TalismanState::TimedOut)
        );
    }

    #[test]
    fn tool_definitions_come_from_the_register() {
        let (fu, register, _, _) = fixture();
        let auth = register.authorize(&fu).unwrap();
        let json = build_tools_json(&auth, &register);
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value[0]["function"]["name"], "fs.read");
        assert!(value[0]["function"]["parameters"]["properties"]["path"].is_object());
        let mut extended = Register::baseline();
        let mut grant = ToolGrant::read_only("http.get", "网络读取");
        grant.needs_network = true;
        extended.grant_tool(grant);
        assert_eq!(build_tools_json(&auth, &extended), json);
        assert!(!json.contains("http.get"), "未受箓的工具不得下发给模型");
    }

    #[test]
    fn outer_sect_schema_reaches_the_model_verbatim() {
        // 外宗工具的参数必须原样来自 server 声明的 inputSchema，
        // 否则模型只能猜参数名（M3 之前 build_tools_json 一律给空 properties）。
        let mut fu = Talisman::sample();
        fu.harness.sandbox = fulu_core::Sandbox::Limited;
        fu.tool_refs = vec!["mcp.fake.shout".into()];
        fu.harness.allow_tools = fu.tool_refs.clone();
        let mut register = Register::baseline();
        let mut grant = ToolGrant::read_only("mcp.fake.shout", "外宗回声");
        grant.permission = fulu_register::Permission::Limited;
        register.grant_tool(grant.with_parameters_json(
            r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#,
        ));
        let auth = register.authorize(&fu).unwrap();
        let value: Value = serde_json::from_str(&build_tools_json(&auth, &register)).unwrap();
        assert_eq!(value[0]["function"]["name"], "mcp.fake.shout");
        assert_eq!(
            value[0]["function"]["parameters"]["required"][0],
            Value::String("text".into())
        );
    }

    /// T02：装配好的系统提示词必须真的送进 provider，且进 dry-run 计划。
    #[test]
    fn assembled_system_prompt_reaches_provider() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        provider.push_reply(text_reply("完成", 10, 10));
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        altar.set_system_prompt(
            "## 篆心\n你是符箓\n\n## 符文正文\n测试正文".into(),
            "提示词装配：测试 2 层".into(),
        );
        let plan = altar.prepare(&fu).unwrap();
        assert!(plan.system_digest.is_some());
        assert!(plan.notes.iter().any(|note| note.contains("提示词装配")));
        altar.ignite(&fu, &plan, true).unwrap();
        let request = provider.last_request().unwrap();
        assert!(
            request.system.starts_with("## 篆心"),
            "{:?}",
            request.system
        );
        assert!(
            !request.system.contains(&fu.system),
            "不得再直接塞裸符文 system"
        );
        audit.verify_chain().unwrap();
    }

    /// dry-run 与焚化之间偷换提示词 = 破符，且一次都不许请神。
    #[test]
    fn prompt_changed_after_dry_run_breaks_before_invocation() {
        let (fu, register, tools, mut audit) = fixture();
        let provider = StubProvider::default();
        let plan = {
            let mut altar = altar(&provider, &tools, &register, &mut audit);
            altar.set_system_prompt("装配 A".into(), "提示词装配：A".into());
            altar.prepare(&fu).unwrap()
        };
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        altar.set_system_prompt("装配 B".into(), "提示词装配：B".into());
        assert!(matches!(
            altar.ignite(&fu, &plan, true),
            Err(RitualError::Broken(_))
        ));
        assert_eq!(provider.call_count(), 0);
    }

    struct RejectSkills;

    impl SkillVerifier for RejectSkills {
        fn verify(&self, _refs: &[String]) -> Result<(), String> {
            Err("技能包内容已变化".into())
        }
    }

    /// T03：符胆校验器在成符阶段就拦下失配的符，不留到焚化后才发现。
    #[test]
    fn skill_verifier_breaks_talisman_before_sealing() {
        let (mut fu, register, tools, mut audit) = fixture();
        fu.skill_refs = vec!["sha256:deadbeef".into()];
        let provider = StubProvider::default();
        let verifier = RejectSkills;
        let mut altar = altar(&provider, &tools, &register, &mut audit);
        altar.set_skill_verifier(&verifier);
        assert!(matches!(altar.prepare(&fu), Err(RitualError::Broken(_))));
        assert!(
            audit
                .events()
                .iter()
                .any(|event| event.kind == EventKind::Broken)
        );
        assert_eq!(provider.call_count(), 0);
    }
}
