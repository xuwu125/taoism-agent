//! 模型供应商抽象层（docs/02 第 5、8.5 节）。
//!
//! 符文与 Harness **不得依赖具体供应商**：流式、工具调用格式、ChatML 模板差异
//! 全部收敛到 `Provider` trait + `Capabilities` 能力协商。
//!
//! M2 状态：`LocalLfmProvider` 已实现真实的 OpenAI 兼容 HTTP 调用（`/health`、
//! `/v1/models`、`/v1/chat/completions`），并把 LFM2 的 Pythonic 工具调用与
//! LFM3 的原生工具调用归一为统一 `ToolCall`。Rust 主进程仍不链接任何推理库：
//! `llama-server` 是独立进程，仅绑定 `127.0.0.1`。

pub mod http;
pub mod local;
pub mod sse;
pub mod stub;

pub use http::{Endpoint, HttpError, HttpResponse, MAX_RESPONSE_BYTES};
pub use local::{
    BearerToken, Health, LocalLfmProvider, build_request_body, normalize_tool_calls, parse_reply,
};
pub use sse::{SseError, StreamAssembler, StreamParts};
pub use stub::{StubProvider, text_reply};

use fulu_core::Sampling;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 能力协商：**未知能力一律取保守值**（`false` / `0`），绝不默认「支持」。
///
/// T13 新增的四个字段全部带 `#[serde(default)]`，旧实现用
/// `Capabilities { ..Default::default() }` 即可继续编译；反序列化旧数据也不会炸。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub streaming: bool,
    pub tool_calls: bool,
    /// 数据不出本机，无需任何凭据
    pub local_only: bool,
    pub max_context_tokens: u32,
    /// 一次回复里的多个 `tool_calls` 是否可并行执行。
    ///
    /// **保守起点是 `false`**：并行工具调用会同时触碰多个敕门，串行更容易审计。
    #[serde(default)]
    pub parallel_tool_calls: bool,
    /// 是否支持 `response_format = json_schema`（而非只有 `json_object`）。
    #[serde(default)]
    pub json_schema_mode: bool,
    /// 是否接受图像内容块（与视觉模型上游配套，本项目默认关闭）。
    #[serde(default)]
    pub vision: bool,
    /// 是否返回独立的 reasoning 文本（需在提示词层过滤，不能混进正文）。
    #[serde(default)]
    pub reasoning_blocks: bool,
    /// 单次请求的工具表上限；`0` = 不限。用于箓层裁剪，避免工具表撑爆上下文。
    #[serde(default)]
    pub max_tools_per_request: u16,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            streaming: false,
            tool_calls: false,
            local_only: true,
            max_context_tokens: 0,
            parallel_tool_calls: false,
            json_schema_mode: false,
            vision: false,
            reasoning_blocks: false,
            max_tools_per_request: 0,
        }
    }
}

impl Capabilities {
    pub fn local_lfm(context_tokens: u32) -> Self {
        Self {
            streaming: true,
            tool_calls: true,
            local_only: true,
            max_context_tokens: context_tokens,
            // llama-server 能并行返回多个 tool_calls，但执行侧仍是串行且逐次过敕门；
            // 这里保持 false，直到执行层真的支持并行。
            parallel_tool_calls: false,
            json_schema_mode: false,
            vision: false,
            reasoning_blocks: false,
            max_tools_per_request: 0,
        }
    }
}

/// 流式事件。
///
/// **关键语义**：工具调用必须**全部收齐**后才过箓核验与敕门，绝不边流边执行；
/// `Done` 之后不得再有事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCallDelta {
        index: u16,
        id: Option<String>,
        name: Option<String>,
        arguments_chunk: String,
    },
    Usage(Usage),
    Done {
        finish_reason: String,
    },
}

/// 调用方对每个事件的回执。`Cancel` 只能收窄，不能扩大——它没有任何放行权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamControl {
    Continue,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 统一 JSON 字符串；LFM 的 Pythonic 函数调用在适配层归一为此格式
    pub arguments_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// 服务端未返回 usage 时按字符数保守估算，并**明确标注**，不伪称精确值。
    pub estimated: bool,
}

impl Usage {
    pub fn total(&self) -> u32 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationRequest {
    pub model_id: String,
    pub system: String,
    pub user: String,
    /// 已由「箓」核验过的工具定义（JSON 数组字符串）
    pub tools_json: String,
    pub sampling: Sampling,
    /// 本轮剩余预算，直接作为 `max_tokens` 上限
    pub budget_tokens: u32,
    pub context_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationReply {
    pub text: String,
    /// 归一后的工具调用；执行前仍须经「箓」+「风火轮」白名单复核
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    /// 成功前实际重试的次数（0 = 一次就成）。旧数据反序列化时按 0 处理，
    /// 调用方只用于展示「重试 N 次后成功」，不参与任何判定。
    #[serde(default)]
    pub retries: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    NotImplemented(&'static str),
    /// 本地推理服务尚未拉起 / 端点缺失：属于配置问题，不是推理失败。
    NotConfigured(String),
    ModelUnavailable(String),
    Transport(String),
    /// 推理服务限流（HTTP 429）。是瞬态过载，值得按策略退避重试。
    RateLimited(String),
    /// 推理服务内部错误（HTTP 5xx）。状态码保留，供 `should_retry` 分类。
    ServerError {
        status: u16,
        detail: String,
    },
    /// 响应协议不符合 OpenAI 兼容约定
    Protocol(String),
    /// 流式调用被调用方主动取消（`StreamControl::Cancel`）。
    ///
    /// 取消不是故障：已收到的文本由调用方自行留存（缴符事件标 `partial`），
    /// 不得拼成「半个回执」回喂模型。
    Cancelled(String),
    BudgetExceeded,
    Timeout,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented(stage) => write!(f, "该 provider 尚未实现（计划阶段：{stage}）"),
            Self::NotConfigured(msg) => write!(f, "本地推理运行时未就绪：{msg}"),
            Self::ModelUnavailable(msg) => write!(f, "内置模型不可用：{msg}"),
            Self::Transport(msg) => write!(f, "本地推理服务通信失败：{msg}"),
            Self::RateLimited(msg) => write!(f, "推理服务限流（HTTP 429）：{msg}"),
            Self::ServerError { status, detail } => {
                write!(f, "推理服务内部错误（HTTP {status}）：{detail}")
            }
            Self::Protocol(msg) => write!(f, "推理服务响应协议错误：{msg}"),
            Self::Cancelled(msg) => write!(f, "推理调用已取消：{msg}"),
            Self::BudgetExceeded => write!(f, "token 预算耗尽"),
            Self::Timeout => write!(f, "推理超时（过坛）"),
        }
    }
}

impl std::error::Error for ProviderError {}

pub trait Provider: Send + Sync {
    /// 稳定标识，写入事件日志，便于回放
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    fn invoke(&self, request: &InvocationRequest) -> Result<InvocationReply, ProviderError>;

    /// 流式调用。**默认不支持**——老实现零改动，且「不支持」是显式错误而不是静默退化，
    /// 免得调用方以为自己在流式，实际上拿到一整块。
    ///
    /// 返回值仍是**完整** `InvocationReply`：流只是让调用方提前看到字，
    /// 不改变「收齐才过箓、才敕门、才执行」的仪轨。
    fn invoke_stream(
        &self,
        _request: &InvocationRequest,
        _sink: &mut dyn FnMut(StreamEvent) -> StreamControl,
    ) -> Result<InvocationReply, ProviderError> {
        Err(ProviderError::NotImplemented("streaming"))
    }
}

/// 重试范围。**只在「没有产生任何可见副作用」时重试**：流式一旦发出首个
/// `TextDelta` 就不可重试（避免重复计费与重复输出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySet {
    pub timeout: bool,
    pub transport: bool,
    pub rate_limited: bool,
    pub server_error: bool,
}

impl Default for RetrySet {
    fn default() -> Self {
        Self {
            timeout: true,
            transport: true,
            rate_limited: true,
            server_error: true,
        }
    }
}

/// 统一退避策略。`backoff_millis` 是纯函数（抖动由调用方给种子），
/// 因此不需要为「等 400ms」写会变慢的测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u8,
    pub base_backoff_ms: u64,
    pub retry_on: RetrySet,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_ms: 400,
            retry_on: RetrySet::default(),
        }
    }
}

impl RetryPolicy {
    /// 第 `attempt` 次失败后的等待（`attempt` 从 1 起）。指数退避 + ±20% 抖动。
    ///
    /// `jitter_permille` 由调用方给（0..=2000 的「实际倍率千分比」），
    /// 这样测试可以断言精确值，生产环境再传随机数。
    pub fn backoff_millis(&self, attempt: u8, jitter_permille: u16) -> u64 {
        let exponent = u32::from(attempt.saturating_sub(1)).min(10);
        let base = self.base_backoff_ms.saturating_mul(1_u64 << exponent);
        // 1000‰ = 原样；范围夹在 800‰..=1200‰，抖动不会翻转退避方向。
        let factor = u64::from(jitter_permille.clamp(800, 1200));
        base.saturating_mul(factor) / 1000
    }

    /// 是否值得重试。**总尝试次数受 `max_attempts` 硬约束**；
    /// 且 `has_visible_output == true` 时一律不重试。
    pub fn should_retry(
        &self,
        attempt: u8,
        error: &ProviderError,
        has_visible_output: bool,
    ) -> bool {
        if has_visible_output || attempt >= self.max_attempts {
            return false;
        }
        match error {
            ProviderError::Timeout => self.retry_on.timeout,
            ProviderError::Transport(_) => self.retry_on.transport,
            // 429 是服务端明确喊「稍后再来」，属瞬态过载，按策略退避重试。
            ProviderError::RateLimited(_) => self.retry_on.rate_limited,
            // 5xx 是服务端内部错误，多数本地推理场景属瞬态（加载中、短暂崩溃），
            // 状态码已保留在 `ServerError.status`，按策略退避重试。
            ProviderError::ServerError { .. } => self.retry_on.server_error,
            // 以下一律不重试，理由如下：
            // - `Cancelled`：取消是人主动按停，重试等于违抗调用方；
            // - `Protocol`：响应格式错了，原样再发大概率还是错，重试只会重复计费；
            // - `NotConfigured`：端点没设，纯配置问题，重试无意义；
            // - `ModelUnavailable`：「服务没起 / 权重没加载」，属配置或部署错误，
            //   不是瞬态——绝不能给「连不上就狂试」留口子；
            // - `NotImplemented`：能力缺失，重试一万次也不会实现；
            // - `BudgetExceeded`：预算烧完是硬边界，重试只会更糟。
            ProviderError::Cancelled(_)
            | ProviderError::Protocol(_)
            | ProviderError::NotConfigured(_)
            | ProviderError::ModelUnavailable(_)
            | ProviderError::NotImplemented(_)
            | ProviderError::BudgetExceeded => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_local_and_offline() {
        let caps = LocalLfmProvider::from_model("lfm3-1.7b-q4km", 8192, "llama.cpp >= b11960")
            .capabilities();
        assert!(caps.local_only);
        assert!(caps.tool_calls);
        assert_eq!(caps.max_context_tokens, 8192);
    }

    #[test]
    fn endpoint_must_be_loopback() {
        let mut provider =
            LocalLfmProvider::from_model("lfm3-1.7b-q4km", 8192, "llama.cpp >= b11960");
        assert!(provider.set_endpoint("http://127.0.0.1:8080").is_ok());
        assert!(provider.set_endpoint("http://localhost:9000/v1").is_ok());
        assert!(provider.set_endpoint("http://0.0.0.0:8080").is_err());
        assert!(provider.set_endpoint("https://api.example.com").is_err());
    }

    #[test]
    fn usage_total_saturates_instead_of_overflowing() {
        let usage = Usage {
            prompt_tokens: u32::MAX,
            completion_tokens: 10,
            estimated: false,
        };
        assert_eq!(usage.total(), u32::MAX);
    }
}
