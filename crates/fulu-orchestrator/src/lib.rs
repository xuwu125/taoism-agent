//! 太极：多 agent 编排、DAG、模型路由与回执汇总
//!
//! M0 状态：**占位骨架，不含任何业务逻辑**。计划在 M3 落地。
//! 依赖方向严格单向（docs/02 第 5 节），领域层可无头测试，GUI 可整体替换。

/// 计划落地里程碑。
pub const MILESTONE: &str = "M3";

/// 该层的职责摘要，供自检与文档生成使用。
pub const DUTY: &str = "多 agent 编排、DAG、模型路由与回执汇总";

pub fn milestone() -> &'static str {
    MILESTONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_declares_its_scope() {
        assert!(!milestone().is_empty());
        assert!(!DUTY.is_empty());
    }
}
