//! 法坛（Runtime）：符箓仪轨的执行层（docs/02 第 4、5 节）。
//!
//! - `converge`：符脚 `success_criteria` 的可判定谓词；
//! - `ritual`：`prepare`（成符→盖印→待焚，零网络）与 `ignite`（确认→焚化→回执→归档）。
//!
//! M2 采用同步执行器 + 可注入 `Provider` / `ToolRegistry`，因此领域层与
//! 运行时都能完全无头测试；tokio 调度与多符并发留到 M3。

pub mod converge;
pub mod ritual;

pub use converge::{Convergence, Criterion, evaluate, parse};
pub use ritual::{Altar, DryRunPlan, Receipt, RitualConfig, RitualError, build_tools_json};

/// 里程碑标识，供自检与文档生成使用。
pub const MILESTONE: &str = "M2";

pub fn milestone() -> &'static str {
    MILESTONE
}
