//! 符层（Talisman）：整个系统的领域内核。
//!
//! 设计约束（docs/02 第 3、5 节）：
//! - 纯数据结构 + 纯函数，**零 I/O**，不依赖 tokio / 网络 / 文件系统；
//! - 错误与事件类型统一放在这里，避免跨 crate 循环依赖；
//! - 所有数值字段使用整数（采样参数用「毫单位」），**禁止浮点**，
//!   以保证 `.fu` 的 canonical 序列化在 Windows / macOS / Linux 上得到同一哈希。
//!
//! M0 状态：数据模型、校验规则与状态机已可用；真实解析与执行分别属于
//! `fulu-script`（M1）与 `fulu-runtime`（M2）。

pub mod error;
pub mod state;
pub mod talisman;

pub use error::{Violation, ViolationSet};
pub use state::{TalismanState, TransitionError, can_transition};
pub use talisman::{Deity, Goal, Harness, Head, OutputSpec, Sampling, Sandbox, Talisman};

/// 当前符文 schema 版本。字段增删必须递增此值并写迁移。
pub const SCHEMA_VERSION: u32 = 1;

/// 内置模型默认槽位（M0 起生效，详见 docs/02 第 8.5 节）。
pub const DEFAULT_MODEL_SLOT: &str = "default";
