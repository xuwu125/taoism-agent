//! 风火轮：工具 trait、最小权限执行、路径与域名白名单
//!
//! M0 状态：**占位骨架，不含任何业务逻辑**。计划在 M2 落地。
//! 依赖方向严格单向（docs/02 第 5 节），领域层可无头测试，GUI 可整体替换。

/// 计划落地里程碑。
pub const MILESTONE: &str = "M2";

/// 该层的职责摘要，供自检与文档生成使用。
pub const DUTY: &str = "工具 trait、最小权限执行、路径与域名白名单";

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
