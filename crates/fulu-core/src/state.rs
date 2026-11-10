//! 符箓仪轨 ↔ 状态机（docs/02 第 4 节）。
//!
//! 纯函数 `can_transition` 是唯一迁移裁判；GUI、CLI、法坛运行时都必须调用它，
//! 不允许各自硬编码状态跳转。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TalismanState {
    /// 草稿 · 净坛 / 书符
    Draft,
    /// 成符 · 一笔呵成（schema 通过、内容冻结）
    Formed,
    /// 已盖印 · 敕符开光（签名、权限与配额已核验）
    Sealed,
    /// 待焚 · dry-run 完成，等待明确确认
    Pending,
    /// 焚化中 · 已提交，**此点起不可撤销**
    Igniting,
    /// 回执 · 神回话
    Answered,
    /// 归档 · 缴符复命，只读可回放
    Archived,
    /// 破符 · 校验或哈希不符
    Broken,
    /// 驳回 · 未受箓或越权
    Rejected,
    /// 炸炉 · 运行时失败 / 预算耗尽
    Exploded,
    /// 过坛 · 超时
    TimedOut,
}

impl TalismanState {
    /// 终态：不再接受任何迁移，只能由上层新建一张符重新走仪轨。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Archived | Self::Broken | Self::Rejected | Self::Exploded | Self::TimedOut
        )
    }

    /// 内容是否已冻结：冻结后任何字段变更都必须重新盖印。
    pub fn is_frozen(self) -> bool {
        !matches!(self, Self::Draft)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "草稿",
            Self::Formed => "成符",
            Self::Sealed => "已盖印",
            Self::Pending => "待焚",
            Self::Igniting => "焚化中",
            Self::Answered => "回执",
            Self::Archived => "归档",
            Self::Broken => "破符",
            Self::Rejected => "驳回",
            Self::Exploded => "炸炉",
            Self::TimedOut => "过坛",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// 该迁移不在仪轨表中。
    Illegal {
        from: TalismanState,
        to: TalismanState,
    },
    /// 源状态为终态。
    Terminal(TalismanState),
    /// 焚化开始后不可撤销，也不允许回退。
    Irreversible(TalismanState),
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Illegal { from, to } => {
                write!(f, "不允许的迁移：{} → {}", from.label(), to.label())
            }
            Self::Terminal(state) => write!(f, "{} 是终态，只能新建符重走仪轨", state.label()),
            Self::Irreversible(state) => write!(f, "{} 起不可撤销，禁止回退", state.label()),
        }
    }
}

impl std::error::Error for TransitionError {}

/// 唯一迁移裁判。返回 `Ok(())` 表示允许迁移，调用方负责写事件日志。
pub fn can_transition(from: TalismanState, to: TalismanState) -> Result<(), TransitionError> {
    use TalismanState as S;

    if from.is_terminal() {
        return Err(TransitionError::Terminal(from));
    }
    if from == S::Igniting && to != S::Answered && to != S::Exploded && to != S::TimedOut {
        return Err(TransitionError::Irreversible(from));
    }

    let allowed = match from {
        S::Draft => matches!(to, S::Formed | S::Broken),
        S::Formed => matches!(to, S::Sealed | S::Broken),
        S::Sealed => matches!(to, S::Pending | S::Rejected | S::Broken),
        S::Pending => matches!(to, S::Igniting | S::Draft),
        S::Igniting => matches!(to, S::Answered | S::Exploded | S::TimedOut),
        S::Answered => matches!(to, S::Archived | S::Igniting),
        S::Archived | S::Broken | S::Rejected | S::Exploded | S::TimedOut => false,
    };

    if allowed {
        Ok(())
    } else {
        Err(TransitionError::Illegal { from, to })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TalismanState as S;

    #[test]
    fn full_happy_path_is_allowed() {
        let path = [
            (S::Draft, S::Formed),
            (S::Formed, S::Sealed),
            (S::Sealed, S::Pending),
            (S::Pending, S::Igniting),
            (S::Igniting, S::Answered),
            (S::Answered, S::Archived),
        ];
        for (from, to) in path {
            assert!(can_transition(from, to).is_ok(), "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn cannot_skip_seal_or_confirmation() {
        assert!(can_transition(S::Draft, S::Sealed).is_err());
        assert!(can_transition(S::Formed, S::Igniting).is_err());
        assert!(can_transition(S::Sealed, S::Igniting).is_err());
    }

    #[test]
    fn igniting_is_irreversible() {
        assert_eq!(
            can_transition(S::Igniting, S::Draft),
            Err(TransitionError::Irreversible(S::Igniting))
        );
        assert!(can_transition(S::Igniting, S::Exploded).is_ok());
    }

    #[test]
    fn archive_is_read_only() {
        assert_eq!(
            can_transition(S::Archived, S::Draft),
            Err(TransitionError::Terminal(S::Archived))
        );
        assert!(S::Archived.is_terminal());
        assert!(S::Answered.is_frozen());
        assert!(!S::Draft.is_frozen());
    }

    #[test]
    fn abnormal_states_are_terminal() {
        for state in [S::Broken, S::Rejected, S::Exploded, S::TimedOut] {
            assert!(state.is_terminal());
            assert_eq!(
                can_transition(state, S::Draft),
                Err(TransitionError::Terminal(state))
            );
        }
    }
}
