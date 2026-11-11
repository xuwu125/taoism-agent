//! 测试 / dry-run 用的桩 provider：**不访问任何网络**，回复由调用方预先排入队列。
//!
//! 它是 `Provider` trait 的可注入实现，让法坛运行时可以完全无头地测试
//! 预算、超时、工具循环与异常态，而不依赖真实权重或推理服务。

use crate::{
    Capabilities, InvocationReply, InvocationRequest, Provider, ProviderError, StreamControl,
    StreamEvent, Usage,
};
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

    /// 造一个「不支持流式」的桩，用来测「装了 sink 但不该静默退化」这条路径。
    pub fn disable_streaming(mut self) -> Self {
        self.capabilities.streaming = false;
        self
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
        // 桩不重试；真实重试计数只来自 LocalLfmProvider 的退避循环。
        retries: 0,
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

    /// 桩的「流式」：把已排队的整段回复切成若干段吐出去。
    ///
    /// 目的不是模拟真实生成速度，而是让法坛的流式路径（取消、半途断流、
    /// 事件记账）能在**不需要权重、不开 socket** 的情况下被测到。
    fn invoke_stream(
        &self,
        request: &InvocationRequest,
        sink: &mut dyn FnMut(StreamEvent) -> StreamControl,
    ) -> Result<InvocationReply, ProviderError> {
        const CHUNK_CHARS: usize = 8;
        let reply = self.invoke(request)?;
        let chars: Vec<char> = reply.text.chars().collect();
        for (index, chunk) in chars.chunks(CHUNK_CHARS).enumerate() {
            let text: String = chunk.iter().collect();
            if sink(StreamEvent::TextDelta(text)) == StreamControl::Cancel {
                return Err(ProviderError::Cancelled(format!(
                    "调用方在第 {index} 段之后取消"
                )));
            }
        }
        for (index, call) in reply.tool_calls.iter().enumerate() {
            let index = u16::try_from(index).unwrap_or(u16::MAX);
            let control = sink(StreamEvent::ToolCallDelta {
                index,
                id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                arguments_chunk: call.arguments_json.clone(),
            });
            if control == StreamControl::Cancel {
                return Err(ProviderError::Cancelled(
                    "调用方在工具调用增量处取消".into(),
                ));
            }
        }
        if sink(StreamEvent::Usage(reply.usage.clone())) == StreamControl::Cancel {
            return Err(ProviderError::Cancelled("调用方在 usage 处取消".into()));
        }
        sink(StreamEvent::Done {
            finish_reason: if reply.tool_calls.is_empty() {
                "stop".to_owned()
            } else {
                "tool_calls".to_owned()
            },
        });
        Ok(reply)
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
