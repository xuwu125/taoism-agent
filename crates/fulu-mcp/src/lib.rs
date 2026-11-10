//! 外宗箓：全局 MCP 配置发现（只读）、stdio JSON-RPC 客户端、MCP 工具归一化适配
//!
//! v0.3 状态：**占位骨架，不含任何业务逻辑**。计划在 M2.5 落地（docs/02 第 8 节）。
//! 依赖方向严格单向（docs/02 第 5 节），领域层可无头测试。

/// 计划落地里程碑。
pub const MILESTONE: &str = "M2.5";

/// 该层的职责摘要，供自检与文档生成使用。
pub const DUTY: &str = "全局 MCP 配置发现（只读）、stdio JSON-RPC 客户端、MCP 工具归一化适配";

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
