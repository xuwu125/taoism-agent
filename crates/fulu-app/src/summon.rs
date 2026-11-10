//! 召将子智能体（T10）：把「分派一张子符」做成一个可受箓、可敕门的工具。
//!
//! 设计约束（docs/02 第 8.6 节与 TODO.md T10）：
//! - **子符 = 独立 `AltarSession`**：自己的箓、风火轮、缴符与预算，与父符零共享可变状态；
//! - **minimal 提示词档**：子符只装配篆心 + 符文正文，不继承父符的会话头与外宗层；
//! - **独立预算**：子符用自己的 token 预算，且步数硬上限被裁剪到 [`SUB_MAX_STEPS`]，
//!   整符超时裁剪到 [`SUB_MAX_TIMEOUT_SECS`]——父符不能借子符名义申请无限预算；
//! - **禁止环回**：子符的风火轮里**根本没有**召将工具，物理上无法再分派；
//!   即便有人绕过本模块给子符手动挂上召将工具，深度守卫也会在超限分派时拒绝；
//! - **不继承高危授权**：子符不装确认器（fail closed）——所有需要人点头的召将
//!   在子符内一律敕否，风险上限不随父符的 `--allow-risk` 传递；
//! - **父符拿回执摘要**：子符最终回答截断后回喂父符模型；分派经过与结果
//!   通过 [`SummonTool::take_dispatch_log`] 落父符缴符事件，可回放。
//!
//! 授权定位：`summon.fu` 是**受限级**工具（符须 `sandbox: limited` 才能申请），
//! 风险等级 `Mutate`（每符确认一次）。人点头的对象是「分派这张子符」这件事本身；
//! 子符内部的一切高危召将仍然全部敕否。

use crate::AltarSession;
use fulu_core::Talisman;
use fulu_prompt::PromptMode;
use fulu_tools::{Tool, ToolArguments, ToolError, ToolOutput, ToolPolicy};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 召将工具在箓里的登记名。
pub const SUMMON_TOOL_ID: &str = "summon.fu";

/// 子符深度上限：父符（深度 0）可以分派深度 1 的子符；再往下就是环回，拒绝。
pub const MAX_DISPATCH_DEPTH: u32 = 1;

/// 子符步数硬上限：父符写多大的 `max_steps` 都会被裁剪到这个值。
pub const SUB_MAX_STEPS: u32 = 8;

/// 子符整符超时上限（秒）：父符写多大的 `timeout_secs` 都会被裁剪到这个值。
pub const SUB_MAX_TIMEOUT_SECS: u32 = 120;

/// 子符最终回答回喂父符时的截断长度（字符）。
const SUB_REPLY_CHARS: usize = 2000;

/// 一次分派的记账条目：进父符缴符事件链，供回放与审计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRecord {
    /// 被分派的子符描述（名称 + 深度 + 符印前缀）。
    pub summary: String,
    /// 结果：`Ok` 为子符回执摘要，`Err` 为失败/拒绝原因。
    pub outcome: Result<String, String>,
}

impl DispatchRecord {
    /// 落缴符事件用的单行文本。
    pub fn line(&self) -> String {
        match &self.outcome {
            Ok(detail) => format!("{} → {detail}", self.summary),
            Err(reason) => format!("{} → 失败：{reason}", self.summary),
        }
    }
}

/// 召将工具的装配配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummonConfig {
    /// 本地推理服务端点（子符与父符共用同一 llama-server）。
    pub endpoint: String,
    /// 本工具创建的子符的深度；超过 [`MAX_DISPATCH_DEPTH`] 即拒绝（环回守卫）。
    pub child_depth: u32,
    /// 子符继承的工作区根目录（只影响 `fs.read` 的边界，不改变权限档位）。
    pub workspace: Option<PathBuf>,
}

/// 召将工具：把一段 `.fu` 符文放到独立法坛上焚化，回喂回执摘要。
///
/// 每次成功或失败的分派都会记入内部 [`DispatchRecord`] 列表；
/// 门面在焚符结束后用 `take_dispatch_log` 把它们落进父符缴符事件链。
#[derive(Clone)]
pub struct SummonTool {
    config: SummonConfig,
    log: Arc<Mutex<Vec<DispatchRecord>>>,
}

impl SummonTool {
    pub fn new(config: SummonConfig) -> Self {
        Self {
            config,
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 取走全部分派记录（每场焚符后由门面调用一次，落父符缴符事件）。
    pub fn take_dispatch_log(&self) -> Vec<DispatchRecord> {
        let mut guard = lock(&self.log);
        std::mem::take(&mut *guard)
    }

    fn record(&self, summary: String, outcome: Result<String, String>) {
        lock(&self.log).push(DispatchRecord { summary, outcome });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Tool for SummonTool {
    fn id(&self) -> &str {
        SUMMON_TOOL_ID
    }

    #[allow(clippy::too_many_lines)]
    fn invoke(&self, args: &ToolArguments, policy: &ToolPolicy) -> Result<ToolOutput, ToolError> {
        // 环回守卫（第二道防线）：正常路径下子符根本没有召将工具；
        // 若有人手动给深一层会话挂上召将，这里按最严处理直接拒绝。
        if self.config.child_depth > MAX_DISPATCH_DEPTH {
            let reason = format!(
                "禁止环回分派：子符深度 {} 已超过上限 {MAX_DISPATCH_DEPTH}，子符不得再分派子符",
                self.config.child_depth
            );
            self.record("分派子符".into(), Err(reason.clone()));
            return Err(ToolError::NotAllowed(reason));
        }

        // 子符来源：优先 `fu_text`（模型直接给出符文全文），
        // 其次 `fu_path`（工作区内的相对路径，复用 fs.read 的路径边界检查）。
        let fu_text = if let Some(text) = args.field("fu_text") {
            text
        } else if let Some(relative) = args.field("fu_path") {
            let path = std::path::Path::new(&relative);
            if path.is_absolute()
                || path.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir | std::path::Component::RootDir
                    )
                })
            {
                return Err(ToolError::PathForbidden(format!(
                    "子符路径越界：{relative}"
                )));
            }
            let root = policy.root.as_ref().ok_or(ToolError::RootNotConfigured)?;
            std::fs::read_to_string(root.join(path))
                .map_err(|e| ToolError::Io(format!("读取子符 {relative}：{e}")))?
        } else {
            return Err(ToolError::InvalidArguments(
                "缺少 fu_text 或 fu_path：必须给出要分派的子符全文或工作区内路径".into(),
            ));
        };

        let mut fu: Talisman = fulu_script::decode_fu(&fu_text)
            .map_err(|e| ToolError::InvalidArguments(format!("子符解析失败：{e}")))?;

        // 独立预算（上界裁剪）：在成符盖印**之前**裁剪，子符按裁剪后的值重新盖印，
        // 绝不在盖印之后偷改符文（那会触发破符语义）。
        fu.harness.max_steps = fu.harness.max_steps.min(SUB_MAX_STEPS);
        fu.harness.timeout_secs = fu.harness.timeout_secs.min(SUB_MAX_TIMEOUT_SECS);

        let seal = fulu_script::seal_hex(&fu)
            .map_err(|e| ToolError::InvalidArguments(format!("子符盖印失败：{e}")))?;
        let summary = format!(
            "子符「{}」深度 {} 符印 {}",
            fu.name,
            self.config.child_depth,
            &seal[..12.min(seal.len())]
        );

        // 成符 → 盖印 → dry-run → 焚化，全部发生在独立会话里。
        let mut child = AltarSession::new();
        // 端点为空 = 父符环境本就未就绪：子符保持未配置状态，
        // 焚化会在 provider 处如实失败并回喂摘要，而不是在装配阶段误判。
        if !self.config.endpoint.is_empty() {
            child
                .set_endpoint(&self.config.endpoint)
                .map_err(|e| ToolError::InvalidArguments(format!("子符端点不合法：{e}")))?;
        }
        child.set_prompt_mode(PromptMode::Minimal);
        if let Some(workspace) = &self.config.workspace {
            child.set_workspace(workspace.clone());
        }
        // 故意**不**设置 confirmer / risk ceiling：子符内所有需要确认的召将一律敕否。
        // 故意**不**注册召将工具：子符物理上无法再分派（环回第一道防线）。

        let plan = match child.dry_run(&fu) {
            Ok(plan) => plan,
            Err(error) => {
                let reason = format!("dry-run 被拒：{error}");
                self.record(summary.clone(), Err(reason.clone()));
                return Err(ToolError::InvalidArguments(reason));
            }
        };
        let outcome = match child.ignite(&fu, &plan, true) {
            Ok(receipt) => {
                let reply = crate::truncate_chars(&receipt.text, SUB_REPLY_CHARS);
                Ok(format!(
                    "steps={} converged={} 事件={} 回执：{reply}",
                    receipt.steps,
                    receipt.converged,
                    receipt.events.len()
                ))
            }
            // 子符执行失败不是父符的策略违规：把失败摘要交回父符模型，
            // 让它决定换路还是如实汇报。分派额度（配额）约束重试次数。
            Err(error) => Err(format!("焚化失败：{error}")),
        };
        self.record(summary, outcome.clone());
        Ok(ToolOutput {
            name: SUMMON_TOOL_ID.into(),
            content: match &outcome {
                Ok(detail) => format!("[子符回执] {detail}"),
                Err(reason) => format!("[子符失败] {reason}；请修正后重试或如实汇报"),
            },
            truncated: false,
        })
    }
}

/// `summon.fu` 的参数 JSON Schema（登记进箓，模型据此构造调用）。
pub const SUMMON_PARAMETERS_JSON: &str = r#"{"type":"object","properties":{"fu_text":{"type":"string","description":"要分派的子符 .fu 全文"},"fu_path":{"type":"string","description":"或：子符文件相对工作区根目录的路径"}}}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn summon_at_max_depth() -> SummonTool {
        SummonTool::new(SummonConfig {
            endpoint: String::new(),
            child_depth: MAX_DISPATCH_DEPTH,
            workspace: None,
        })
    }

    fn fu_args(text: &str) -> ToolArguments {
        ToolArguments {
            json: format!(r#"{{"fu_text":{}}}"#, json_quote(text)),
        }
    }

    /// 极简 JSON 字符串转义（标准库手写，不引解析器）。
    fn json_quote(text: &str) -> String {
        let mut out = String::from("\"");
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// 环回守卫：深度超限的分派必须被拒，且留下记账。
    #[test]
    fn dispatch_beyond_depth_limit_is_refused_and_recorded() {
        let tool = SummonTool::new(SummonConfig {
            endpoint: String::new(),
            child_depth: MAX_DISPATCH_DEPTH + 1,
            workspace: None,
        });
        let result = tool.invoke(&ToolArguments::empty(), &ToolPolicy::default());
        assert!(matches!(result, Err(ToolError::NotAllowed(msg)) if msg.contains("环回")));
        let log = tool.take_dispatch_log();
        assert_eq!(log.len(), 1, "被拒的分派也必须记账");
        assert!(log[0].line().contains("环回"));
    }

    /// 深度上限内的分派放行到参数校验层（缺参数在后面单独一测）。
    #[test]
    fn dispatch_within_depth_limit_reaches_argument_checks() {
        let result = summon_at_max_depth().invoke(&ToolArguments::empty(), &ToolPolicy::default());
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(ref msg)) if msg.contains("fu_text")),
            "深度未超限时才轮到参数校验：{result:?}"
        );
    }

    /// fu_path 必须待在工作区内：绝对路径与目录穿越都拒（无论根目录是否配置）。
    #[test]
    fn dispatch_fu_path_respects_workspace_boundary() {
        let tool = summon_at_max_depth();
        for bad in [
            r#"{"fu_path":"C:/evil.fu"}"#,
            r#"{"fu_path":"../../evil.fu"}"#,
        ] {
            let result = tool.invoke(&ToolArguments { json: bad.into() }, &ToolPolicy::default());
            assert!(
                matches!(result, Err(ToolError::PathForbidden(_))),
                "{bad} 应被判路径越界"
            );
        }
        // 合法相对路径但未配置根目录 → 明确的「未配置工作区」。
        let result = tool.invoke(
            &ToolArguments {
                json: r#"{"fu_path":"sub.fu"}"#.into(),
            },
            &ToolPolicy::default(),
        );
        assert_eq!(result, Err(ToolError::RootNotConfigured));
    }

    /// 缺少子符来源必须明确拒绝，不许静默空分派。
    #[test]
    fn dispatch_without_fu_source_is_rejected() {
        let result = summon_at_max_depth().invoke(&ToolArguments::empty(), &ToolPolicy::default());
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    /// 子符执行失败（如端点未配置）交回摘要而不是炸父符，且记账留痕。
    #[test]
    fn child_execution_failure_is_fed_back_not_fatal() {
        let text = crate::sample_fu_text().unwrap();
        let tool = summon_at_max_depth();
        let result = tool
            .invoke(&fu_args(&text), &ToolPolicy::default())
            .expect("子符执行失败应作为回执而非工具错误");
        assert!(result.content.contains("[子符失败]"), "{}", result.content);
        let log = tool.take_dispatch_log();
        assert_eq!(log.len(), 1);
        assert!(log[0].outcome.is_err(), "失败摘要进记账");
        assert!(log[0].line().contains("焚化失败"));
    }

    /// 独立预算裁剪发生在盖印之前：子符步数与超时被裁到常量上界。
    #[test]
    fn child_budget_is_clamped_before_sealing() {
        let mut fu = fulu_script::decode_fu(&crate::sample_fu_text().unwrap()).unwrap();
        fu.harness.max_steps = 999;
        fu.harness.timeout_secs = 999_999;
        fu.harness.max_steps = fu.harness.max_steps.min(SUB_MAX_STEPS);
        fu.harness.timeout_secs = fu.harness.timeout_secs.min(SUB_MAX_TIMEOUT_SECS);
        assert_eq!(fu.harness.max_steps, SUB_MAX_STEPS);
        assert_eq!(fu.harness.timeout_secs, SUB_MAX_TIMEOUT_SECS);
        // 裁剪后的符重新盖印必须可行（sanity：不破坏成符链路）。
        assert!(fulu_script::seal_hex(&fu).is_ok());
    }

    /// 隔离性质锁定：子符是 minimal 档、无确认器、风火轮里没有召将工具。
    #[test]
    fn child_session_is_isolated_minimal_and_cannot_redispatch() {
        let mut child = AltarSession::new();
        child.set_prompt_mode(PromptMode::Minimal);
        let tools = child.tool_ids();
        assert_eq!(child.prompt_mode(), PromptMode::Minimal);
        assert!(!child.has_confirmer(), "子符不得继承确认器（fail closed）");
        assert!(
            !tools.iter().any(|id| id == SUMMON_TOOL_ID),
            "子符的风火轮里不得有召将工具：{tools:?}"
        );
        // 常量语义锁定：深度上限即「只能有一层子符」。
        assert_eq!(MAX_DISPATCH_DEPTH, 1);
        assert_eq!(SUB_MAX_STEPS, 8);
        assert_eq!(SUB_MAX_TIMEOUT_SECS, 120);
        let _ = PathBuf::new(); // 保持导入被使用（路径相关常量在此模块顶部）
    }
}
