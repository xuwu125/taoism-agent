//! 模型供应商抽象层（docs/02 第 5、8.5 节）。
//!
//! 符文与 Harness **不得依赖具体供应商**：流式、工具调用格式、模板差异全部
//! 收敛到 `Provider` trait + `Capabilities` 能力协商。
//!
//! M0 只定义契约。`LocalLfmProvider::invoke` 明确返回 `NotImplemented`（M2 落地
//! 本地 `llama-server` 的 OpenAI 兼容 HTTP 调用），**不伪装成已可推理**。

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationRequest {
    pub model_id: String,
    pub system: String,
    pub user: String,
    /// 已由「箓」核验过的工具定义（JSON 数组字符串）
    pub tools_json: String,
    pub sampling: Sampling,
    pub budget_tokens: u32,
    pub context_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationReply {
    pub text: String,
    /// 归一后的工具调用；执行前仍须经 `fulu-tools` 白名单复核
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    NotImplemented(&'static str),
    ModelUnavailable(String),
    Transport(String),
    BudgetExceeded,
    Timeout,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented(stage) => write!(f, "该 provider 尚未实现（计划阶段：{stage}）"),
            Self::ModelUnavailable(msg) => write!(f, "内置模型不可用：{msg}"),
            Self::Transport(msg) => write!(f, "本地推理服务通信失败：{msg}"),
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

/// 内置 2B 级 LFM：只记录端点与模型标识，实际 HTTP 调用留到 M2。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLfmProvider {
    /// 例如 `http://127.0.0.1:随机端口`，由 `fulu-model` 的进程管理提供
    pub endpoint: String,
    pub model_id: String,
    /// 清单 `min_runtime`，启动时断言，避免静默加载不支持的架构
    pub min_runtime: String,
    pub context_tokens: u32,
}

impl LocalLfmProvider {
    pub fn from_model(model_id: &str, context_tokens: u32, min_runtime: &str) -> Self {
        Self {
            endpoint: String::new(),
            model_id: model_id.to_owned(),
            min_runtime: min_runtime.to_owned(),
            context_tokens,
        }
    }

    /// 本地服务只允许回环地址，避免把推理端口暴露到网络。
    pub fn set_endpoint(&mut self, endpoint: &str) -> Result<(), ProviderError> {
        if !(endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"))
        {
            return Err(ProviderError::Transport(
                "本地推理服务必须绑定回环地址".into(),
            ));
        }
        self.endpoint = endpoint.to_owned();
        Ok(())
    }
}

impl Provider for LocalLfmProvider {
    fn id(&self) -> &str {
        "lu://local/llama-server"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::local_lfm(self.context_tokens)
    }

    fn invoke(&self, _request: &InvocationRequest) -> Result<InvocationReply, ProviderError> {
        // 故意不实现：M0 是架构冻结，不写推理业务代码。
        Err(ProviderError::NotImplemented("M2 · 法坛运行时"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> LocalLfmProvider {
        LocalLfmProvider::from_model("lfm3-1.7b-q4km", 8192, "llama.cpp >= b11960 (v0.8.0)")
    }

    #[test]
    fn capabilities_are_local_and_offline() {
        let caps = provider().capabilities();
        assert!(caps.local_only);
        assert!(caps.tool_calls);
        assert_eq!(caps.max_context_tokens, 8192);
    }

    #[test]
    fn invoke_explicitly_reports_not_implemented() {
        let request = InvocationRequest {
            model_id: "lfm3-1.7b-q4km".into(),
            system: String::new(),
            user: "你好".into(),
            tools_json: "[]".into(),
            sampling: Sampling::default(),
            budget_tokens: 100,
            context_tokens: 8192,
        };
        assert_eq!(
            provider().invoke(&request),
            Err(ProviderError::NotImplemented("M2 · 法坛运行时"))
        );
    }

    #[test]
    fn endpoint_must_be_loopback() {
        let mut p = provider();
        assert!(p.set_endpoint("http://127.0.0.1:8080").is_ok());
        assert!(p.set_endpoint("http://0.0.0.0:8080").is_err());
        assert!(p.set_endpoint("https://api.example.com").is_err());
    }
}
