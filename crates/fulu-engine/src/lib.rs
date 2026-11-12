//! 本地推理引擎的**进程生命周期**（T22，docs/02 第 40 节）。
//!
//! 铁律：
//! - **内置模型 ≠ 内嵌推理引擎**：Rust 主进程不链接任何 C++ 推理库、不解析 GGUF，
//!   只按需拉起独立的 `llama-server` 进程，通过 `127.0.0.1` 的 OpenAI 兼容 HTTP 对话。
//! - **不静默启动**：本 crate 的 `spawn` 只由显式命令（`fulu model start`）调用；
//!   没有后台轮询、没有「发现缺服务就自动拉起」的隐式 fallback。
//! - **只绑回环**：host 硬编码 `127.0.0.1`，端口占用时明确报错而不是悄悄连上别人的服务。
//! - **用完必停**：`EngineHandle` 的 `Drop` 无条件回收自己拉起的进程；闲置超时由调用方
//!   每轮 `poll_idle()` 判定（CLI 不代理请求，感知不到外部调用，因此 `touch()` 要显式调）。
//! - **不引入 tokio**：同步启动 + 同步探活 + `std::thread` 只用于读标准输入，与项目
//!   「单符串行、可注入 provider」的执行器架构一致。

pub mod config;
pub mod pidfile;
pub mod process;

pub use config::{
    BINARY_ENV, DEFAULT_PORT, EngineConfig, LOOPBACK_HOST, RUN_RECORD_ENV, RuntimeInfo, app_dir,
    bin_dir, check_runtime, default_log_path, discover_binary, log_dir, record_path, run_dir,
};
pub use pidfile::{
    RunRecord, StopReport, read_record, remove_record, stop_owned_service, write_record,
};
pub use process::{EngineHandle, assert_port_free, loaded_models, probe_health, request_once};

/// 健康检查四态。**「连不上」与「还在加载」必须可区分**：前者要如实报错，
/// 后者只是继续等；绝不把两者合并成一个模糊的 false。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthState {
    Ready { detail: String },
    Loading { detail: String },
    HttpStatus(u16),
    Unreachable(String),
}

use std::fmt;

/// 进程生命周期错误。**每个分支都带可行动的上下文**，失败时附日志尾部，
/// 绝不把「启动失败」伪装成「服务未就绪」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    Config(String),
    BinaryNotFound(String),
    PortInUse(u16),
    Spawn(String),
    StartupTimeout { seconds: u64, log_tail: String },
    ExitedEarly { code: Option<i32>, log_tail: String },
    Health(String),
    Io(String),
    Record(String),
    Stop(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "启动配置不合法：{msg}"),
            Self::BinaryNotFound(msg) => write!(f, "找不到 llama-server 二进制：{msg}"),
            Self::PortInUse(port) => write!(
                f,
                "回环端口 {port} 已有服务监听；拒绝悄悄复用别人的进程。请用 `fulu model serve` 探活，或换 --port"
            ),
            Self::Spawn(msg) => write!(f, "拉起 llama-server 失败：{msg}"),
            Self::StartupTimeout { seconds, log_tail } => write!(
                f,
                "等待 {seconds}s 仍未就绪（进程已回收）。日志尾部：\n{log_tail}"
            ),
            Self::ExitedEarly { code, log_tail } => write!(
                f,
                "llama-server 启动途中退出（exit={code:?}，进程已回收）。日志尾部：\n{log_tail}"
            ),
            Self::Health(msg) => write!(f, "健康检查失败：{msg}"),
            Self::Io(msg) => write!(f, "进程管理 I/O 失败：{msg}"),
            Self::Record(msg) => write!(f, "运行记录不可用：{msg}"),
            Self::Stop(msg) => write!(f, "停止服务失败：{msg}"),
        }
    }
}

impl std::error::Error for EngineError {}
