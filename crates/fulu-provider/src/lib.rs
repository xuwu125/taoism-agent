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
pub mod stub;

pub use http::{Endpoint, HttpError, HttpResponse, MAX_RESPONSE_BYTES};
pub use local::{Health, LocalLfmProvider, build_request_body, normalize_tool_calls, parse_reply};
pub use stub::{StubProvider, text_reply};

use fulu_core::Sampling;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub streaming: bool,
    pub tool_calls: bool,
    /// 数据不出本机，无需任何凭据
    pub local_only: bool,
    pub max_context_tokens: u32,
}

impl Capabilities {
    pub fn local_lfm(context_tokens: u32) -> Self {
        Self {
            streaming: true,
            tool_calls: true,
            local_only: true,
            max_context_tokens: context_tokens,
        }
    }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    NotImplemented(&'static str),
    /// 本地推理服务尚未拉起 / 端点缺失：属于配置问题，不是推理失败。
    NotConfigured(String),
    ModelUnavailable(String),
    Transport(String),
    /// 响应协议不符合 OpenAI 兼容约定
    Protocol(String),
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
            Self::Protocol(msg) => write!(f, "推理服务响应协议错误：{msg}"),
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
