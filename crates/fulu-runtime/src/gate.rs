//! 敕门（Gate）：高危召将前的人确认（docs/02 第 16.3 节）。
//!
//! 箓层回答「这张符能不能碰这个工具」，敕门回答「这一次碰的时候，人点头了吗」。
//! 两者正交：一个已受箓的 `stop_process` 在权限上完全合法，但**不该在无人
//! 复核时静默执行**；反之，符没受箓时连问都不该问（那是驳回，不是确认）。
//!
//! 默认方向是 **fail closed**：`Altar` 没有装确认器时，一切需要确认的召将
//! 都被记为「敕否」，绝不因为「没人管」而放行。

use fulu_register::{Confirmation, RiskClass};

/// 一次「请人点头」的完整上下文。
///
/// 给人看的时候必须足够判断：哪个工具、多危险、要传什么参数、这是第几次。
/// `arguments_json` 是模型输出，属不可信文本，展示时必须原样包裹而不解释。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmRequest {
    pub seal: String,
    pub tool: String,
    pub risk: RiskClass,
    pub confirmation: Confirmation,
    pub description: String,
    pub arguments_json: String,
    /// 该工具在本符内第几次被召（从 1 起）
    pub attempt: u32,
}

impl ConfirmRequest {
    /// 单行摘要，供 CLI 一行式提问与缴符事件复用。
    pub fn summary(&self) -> String {
        format!(
            "{}（{}·{}）第 {} 次调用",
            self.tool,
            self.risk.label(),
            self.confirmation.label(),
            self.attempt
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmVerdict {
    /// 敕可：本次放行
    Allow,
    /// 敕否：本次拒绝
    Deny,
}

/// 确认器。
///
/// 之所以是一个 trait 而不是布尔开关：CLI 要在终端里问人，GUI 要把请求丢回
/// 界面线程，批处理要按上限放行——三种「人」不一样，法坛不该替它们选。
pub trait Confirmer: Send {
    fn confirm(&mut self, request: &ConfirmRequest) -> ConfirmVerdict;
}

/// 无人值守的默认确认器：一律拒绝。
///
/// 显式实现一份而不是靠 `Option::None` 的分支，是为了让「没人值守」这件事
/// 在调用点可见，并且可以被替换成更严格或更宽松的策略。
pub struct DenyAll;

impl Confirmer for DenyAll {
    fn confirm(&mut self, _request: &ConfirmRequest) -> ConfirmVerdict {
        ConfirmVerdict::Deny
    }
}

/// 按风险上限放行：`AllowUpTo(RiskClass::Mutate)` 只放行可变更及以下，
/// 破坏性工具仍然逐次拒绝。
///
/// 这是「我知道我在干什么」的显式承诺，只在用户亲手开了开关时使用
/// （CLI `--allow-risk`、GUI 的勾选框），并且必须在缴符里留下痕迹。
pub struct AllowUpTo(pub RiskClass);

impl Confirmer for AllowUpTo {
    fn confirm(&mut self, request: &ConfirmRequest) -> ConfirmVerdict {
        if request.risk <= self.0 {
            ConfirmVerdict::Allow
        } else {
            ConfirmVerdict::Deny
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(risk: RiskClass) -> ConfirmRequest {
        ConfirmRequest {
            seal: "abc".into(),
            tool: "mcp.fake.stop_process".into(),
            risk,
            confirmation: risk.confirmation(),
            description: "停掉一个进程".into(),
            arguments_json: r#"{"pid":42}"#.into(),
            attempt: 1,
        }
    }

    #[test]
    fn deny_all_denies_everything() {
        let mut gate = DenyAll;
        assert_eq!(
            gate.confirm(&request(RiskClass::Observe)),
            ConfirmVerdict::Deny
        );
    }

    #[test]
    fn allow_up_to_respects_the_ceiling() {
        let mut mutate = AllowUpTo(RiskClass::Mutate);
        assert_eq!(
            mutate.confirm(&request(RiskClass::Observe)),
            ConfirmVerdict::Allow
        );
        assert_eq!(
            mutate.confirm(&request(RiskClass::Mutate)),
            ConfirmVerdict::Allow
        );
        assert_eq!(
            mutate.confirm(&request(RiskClass::Destructive)),
            ConfirmVerdict::Deny
        );

        let mut destructive = AllowUpTo(RiskClass::Destructive);
        assert_eq!(
            destructive.confirm(&request(RiskClass::Destructive)),
            ConfirmVerdict::Allow
        );
    }

    #[test]
    fn summary_is_single_line_and_names_the_risk() {
        let line = request(RiskClass::Destructive).summary();
        assert!(line.contains("mcp.fake.stop_process"));
        assert!(line.contains("破坏性"));
        assert!(!line.contains('\n'));
    }
}
