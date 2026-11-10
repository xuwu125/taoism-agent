//! 测试 / dry-run 用的桩 provider：**不访问任何网络**，回复由调用方预先排入队列。
//!
//! 它是 `Provider` trait 的可注入实现，让法坛运行时可以完全无头地测试
//! 预算、超时、工具循环与异常态，而不依赖真实权重或推理服务。

use crate::{Capabilities, InvocationReply, InvocationRequest, Provider, ProviderError, Usage};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
pub struct StubProvider {
    id: String,
    capabilities: Capabilities,
    replies: Mutex<VecDeque<Result<InvocationReply, ProviderError>>>,
    last_request: Mutex<Option<InvocationRequest>>,
    calls: AtomicUsize,
    delay: Duration,
}

impl Default for StubProvider {
    fn default() -> Self {
        Self::new("lu://test/stub")
    }
}

impl StubProvider {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            capabilities: Capabilities::local_lfm(8192),
            replies: Mutex::new(VecDeque::new()),
            last_request: Mutex::new(None),
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
        }
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn push_reply(&self, reply: InvocationReply) {
        self.replies.lock().unwrap().push_back(Ok(reply));
    }

    pub fn push_error(&self, error: ProviderError) {
        self.replies.lock().unwrap().push_back(Err(error));
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    pub fn last_request(&self) -> Option<InvocationRequest> {
        self.last_request.lock().unwrap().clone()
    }
}

pub fn text_reply(text: &str, prompt_tokens: u32, completion_tokens: u32) -> InvocationReply {
    InvocationReply {
        text: text.to_owned(),
        tool_calls: Vec::new(),
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            estimated: false,
        },
    }
}

impl Provider for StubProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn invoke(&self, request: &InvocationRequest) -> Result<InvocationReply, ProviderError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        *self.last_request.lock().unwrap() = Some(request.clone());
        if !self.delay.is_zero() {
            thread::sleep(self.delay);
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(text_reply("（桩：未配置回复）", 0, 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_core::Sampling;

    fn request() -> InvocationRequest {
        InvocationRequest {
            model_id: "stub".into(),
            system: String::new(),
            user: "你好".into(),
            tools_json: "[]".into(),
            sampling: Sampling::default(),
            budget_tokens: 100,
            context_tokens: 8192,
        }
    }

    #[test]
    fn queued_replies_are_consumed_in_order_and_requests_are_recorded() {
        let stub = StubProvider::default();
        stub.push_reply(text_reply("第一", 1, 1));
        stub.push_reply(text_reply("第二", 1, 1));
        assert_eq!(stub.invoke(&request()).unwrap().text, "第一");
        assert_eq!(stub.invoke(&request()).unwrap().text, "第二");
        assert_eq!(stub.call_count(), 2);
        assert_eq!(stub.last_request().unwrap().user, "你好");
    }

    #[test]
    fn queued_error_is_propagated() {
        let stub = StubProvider::default();
        stub.push_error(ProviderError::BudgetExceeded);
        assert_eq!(stub.invoke(&request()), Err(ProviderError::BudgetExceeded));
    }
}
