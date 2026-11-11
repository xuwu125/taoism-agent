//! SSE 流式解析（docs/03 第 2.4 节）。
//!
//! 这里**只做解析**：喂一行 `data:`，吐出事件与「是否结束」。网络、超时、
//! 取消都归 `http` / `local` 两层，测试因此不需要开 socket。
//!
//! 口径（不可含糊）：
//! - 只认 `data:` 行；`event:` 仅用于区分 `error`，不改变主路径；
//! - `[DONE]` 是结束，空行是事件分隔不是结束；
//! - `tool_calls` 按 `index` 归并，`id` / `name` 只在首个增量出现，
//!   `arguments` 逐段拼接——拼完必须**是合法 JSON**，否则报错而不是猜；
//! - 单事件上限 1 MiB，整轮上限与 `MAX_RESPONSE_BYTES` 同源（8 MiB）；
//! - 服务端不给 `usage` 就不编，由上游标 `estimated`。

use crate::{MAX_RESPONSE_BYTES, ProviderError, StreamEvent, ToolCall, Usage};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

/// 单个 SSE 事件的上限。正常推理增量是几十字节，1 MiB 已经宽松到只剩防滥用意义。
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseError {
    /// 单事件超限：宁可断流，也不把内存交给对端决定。
    EventTooLarge { bytes: usize },
    /// 整轮超限（与 `MAX_RESPONSE_BYTES` 同源）。
    StreamTooLarge { bytes: usize },
    /// 服务端显式报错（`event: error` 或 `{"error": ...}`）。
    ServerError(String),
    /// 帧本身不是 JSON，或结构不符合 OpenAI 兼容约定。
    Malformed(String),
    /// 增量拼完之后不是合法 JSON 参数——**不猜**，直接判协议错误。
    AssembledArguments {
        index: u16,
        raw: String,
        reason: String,
    },
    /// 有参数却没有函数名：这种调用无法过箓，不能当空名放过。
    MissingToolName { index: u16 },
}

impl fmt::Display for SseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EventTooLarge { bytes } => {
                write!(
                    f,
                    "单个流事件超过 {MAX_EVENT_BYTES} 字节上限（实收 {bytes}）"
                )
            }
            Self::StreamTooLarge { bytes } => {
                write!(
                    f,
                    "流式响应超过 {MAX_RESPONSE_BYTES} 字节上限（累计 {bytes}）"
                )
            }
            Self::ServerError(msg) => write!(f, "推理服务在流中报错：{msg}"),
            Self::Malformed(msg) => write!(f, "流式响应格式错误：{msg}"),
            Self::AssembledArguments { index, raw, reason } => write!(
                f,
                "第 {index} 个工具调用的参数拼接后不是合法 JSON（{reason}）：{}",
                truncate(raw, 120)
            ),
            Self::MissingToolName { index } => {
                write!(f, "第 {index} 个工具调用只有参数、没有函数名")
            }
        }
    }
}

impl std::error::Error for SseError {}

impl From<SseError> for ProviderError {
    fn from(error: SseError) -> Self {
        match error {
            SseError::ServerError(msg) => Self::Protocol(msg),
            other => Self::Protocol(other.to_string()),
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    if text.len() > end {
        out.push('…');
    }
    out
}

/// 喂一行之后的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub events: Vec<StreamEvent>,
    /// 收到 `[DONE]`：调用方应停止读取并关闭连接。
    pub done: bool,
}

#[derive(Debug, Default)]
struct PartialCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

/// 把零散的 SSE 行拼成一份完整回执。
#[derive(Debug, Default)]
pub struct StreamAssembler {
    text: String,
    calls: BTreeMap<u16, PartialCall>,
    usage: Option<Usage>,
    finish_reason: Option<String>,
    event_name: Option<String>,
    bytes: usize,
    done: bool,
}

/// `into_parts` 的产物：仍是**未归一**的原料，归一（Pythonic 调用、估算 usage）
/// 留给 `local` 层，保证流式与非流式走同一套归一逻辑。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamParts {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// `None` = 服务端没给，由上游按字符数保守估算并标 `estimated`。
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

impl StreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否已经吐过「用户看得见的东西」。重试判定靠它：
    /// 有了可见输出就绝不重试，避免重复计费与重复输出。
    pub fn has_visible_output(&self) -> bool {
        !self.text.is_empty() || !self.calls.is_empty()
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn received_bytes(&self) -> usize {
        self.bytes
    }

    /// 喂一行原始字节（可含 `\r\n`）。
    pub fn feed_line(&mut self, line: &[u8]) -> Result<Feed, SseError> {
        let text = std::str::from_utf8(line)
            .map_err(|e| SseError::Malformed(format!("流中出现非 UTF-8 字节：{e}")))?;
        self.feed_str(text)
    }

    /// 喂一行文本。空行、注释行、非 `data:` 字段都合法，直接跳过。
    pub fn feed_str(&mut self, line: &str) -> Result<Feed, SseError> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with(':') {
            return Ok(Feed::default());
        }
        let Some((field, value)) = line.split_once(':') else {
            // 规范要求无冒号的行按注释处理，这里同样跳过，不把它当错误。
            return Ok(Feed::default());
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => {
                self.event_name = Some(value.to_owned());
                Ok(Feed::default())
            }
            "data" => self.consume_data(value),
            _ => Ok(Feed::default()),
        }
    }

    fn consume_data(&mut self, data: &str) -> Result<Feed, SseError> {
        if data.len() > MAX_EVENT_BYTES {
            return Err(SseError::EventTooLarge { bytes: data.len() });
        }
        self.bytes = self.bytes.saturating_add(data.len());
        if self.bytes > MAX_RESPONSE_BYTES {
            return Err(SseError::StreamTooLarge { bytes: self.bytes });
        }
        if data.trim() == "[DONE]" {
            self.done = true;
            // usage 在它自己那一帧已经交出去了，这里**不再重复发**：
            // 同一个用量报两遍会让调用方重复记账。
            return Ok(Feed {
                events: vec![StreamEvent::Done {
                    finish_reason: self
                        .finish_reason
                        .clone()
                        .unwrap_or_else(|| "stop".to_owned()),
                }],
                done: true,
            });
        }
        let root: Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(error) => {
                // `event: error` 的载荷常常不是 JSON 而是纯文本，别硬当协议错。
                if self.event_name.as_deref() == Some("error") {
                    return Err(SseError::ServerError(truncate(data, 200)));
                }
                return Err(SseError::Malformed(format!("帧不是 JSON（{error}）")));
            }
        };
        if let Some(error) = root.get("error") {
            let message = error["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            return Err(SseError::ServerError(truncate(&message, 200)));
        }

        let mut events = Vec::new();
        if let Some(usage) = parse_usage(root.get("usage")) {
            self.usage = Some(usage.clone());
            events.push(StreamEvent::Usage(usage));
        }

        let choice = root.get("choices").and_then(|value| value.get(0));
        if let Some(choice) = choice {
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.finish_reason = Some(reason.to_owned());
            }
            let delta = &choice["delta"];
            if let Some(chunk) = delta["content"].as_str()
                && !chunk.is_empty()
            {
                self.text.push_str(chunk);
                events.push(StreamEvent::TextDelta(chunk.to_owned()));
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    events.push(self.merge_tool_call(call)?);
                }
            }
        }
        // 有些服务端在最后一个 chunk 用 `message` 而非 `delta` 再送一次全文：
        // 当前口径是**只认 delta**，不重复累加（重复会把正文变两遍）。
        Ok(Feed {
            events,
            done: false,
        })
    }

    fn merge_tool_call(&mut self, call: &Value) -> Result<StreamEvent, SseError> {
        let index = call["index"].as_u64().unwrap_or(0).min(u64::from(u16::MAX)) as u16;
        let id = call["id"]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        let name = call["function"]["name"]
            .as_str()
            .or_else(|| call["name"].as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        let chunk = call["function"]["arguments"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();

        let entry = self.calls.entry(index).or_default();
        if let Some(id) = &id
            && entry.id.is_none()
        {
            entry.id = Some(id.clone());
        }
        if let Some(name) = &name {
            // 函数名分片出现是异常情况，直接以最新一片为准并保留原值不动。
            if entry.name.is_empty() {
                entry.name = name.clone();
            }
        }
        entry.arguments.push_str(&chunk);
        Ok(StreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments_chunk: chunk,
        })
    }

    /// 收尾：把增量拼成工具调用。参数不合法就报错——**不猜、不补全**。
    pub fn into_parts(self) -> Result<StreamParts, SseError> {
        let mut calls = Vec::with_capacity(self.calls.len());
        for (index, call) in self.calls {
            if call.name.is_empty() {
                return Err(SseError::MissingToolName { index });
            }
            let raw = if call.arguments.trim().is_empty() {
                "{}".to_owned()
            } else {
                call.arguments
            };
            if let Err(error) = serde_json::from_str::<Value>(&raw) {
                return Err(SseError::AssembledArguments {
                    index,
                    raw,
                    reason: error.to_string(),
                });
            }
            calls.push(ToolCall {
                id: call.id.unwrap_or_else(|| format!("call-{index}")),
                name: call.name,
                arguments_json: raw,
            });
        }
        Ok(StreamParts {
            text: self.text,
            tool_calls: calls,
            usage: self.usage,
            finish_reason: self.finish_reason,
        })
    }
}

fn parse_usage(value: Option<&Value>) -> Option<Usage> {
    let value = value?;
    let prompt = value["prompt_tokens"].as_u64()? as u32;
    let completion = value["completion_tokens"].as_u64()? as u32;
    Some(Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        // 服务端明确给出即视为精确；`estimate_tokens` 那条路径才标 true。
        estimated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(asm: &mut StreamAssembler, line: &str) -> Feed {
        asm.feed_str(line).expect("应当解析成功")
    }

    fn text_frame(chunk: &str) -> String {
        format!(
            "data: {}",
            serde_json::json!({"choices":[{"delta":{"content":chunk}}]})
        )
    }

    #[test]
    fn text_deltas_are_emitted_and_concatenated() {
        let mut asm = StreamAssembler::new();
        let mut collected = String::new();
        for chunk in ["先", "盖印", "，后焚符"] {
            let feed = feed(&mut asm, &text_frame(chunk));
            for event in feed.events {
                match event {
                    StreamEvent::TextDelta(text) => collected.push_str(&text),
                    other => panic!("不应出现 {other:?}"),
                }
            }
        }
        feed(&mut asm, "data: [DONE]");
        assert_eq!(collected, "先盖印，后焚符");
        assert_eq!(asm.into_parts().expect("应拼装成功").text, "先盖印，后焚符");
    }

    #[test]
    fn tool_call_arguments_are_merged_by_index() {
        let mut asm = StreamAssembler::new();
        feed(
            &mut asm,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell.exec","arguments":"{\"cmd\":"}}]}}]}"#,
        );
        feed(
            &mut asm,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]}}]}"#,
        );
        let parts = asm.into_parts().expect("应拼装成功");
        assert_eq!(parts.tool_calls.len(), 1);
        assert_eq!(parts.tool_calls[0].name, "shell.exec");
        assert_eq!(parts.tool_calls[0].id, "call_1");
        assert_eq!(parts.tool_calls[0].arguments_json, r#"{"cmd":"ls"}"#);
    }

    #[test]
    fn a_missing_tool_name_is_an_error_not_an_empty_string() {
        let mut asm = StreamAssembler::new();
        feed(
            &mut asm,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#,
        );
        assert!(matches!(
            asm.into_parts(),
            Err(SseError::MissingToolName { index: 0 })
        ));
    }

    #[test]
    fn fragmented_arguments_that_do_not_form_json_are_refused() {
        let mut asm = StreamAssembler::new();
        feed(
            &mut asm,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"fs.write","arguments":"{\"path\":"}}]}}]}"#,
        );
        assert!(matches!(
            asm.into_parts(),
            Err(SseError::AssembledArguments { index: 0, .. })
        ));
    }

    #[test]
    fn comments_blank_lines_and_other_fields_are_ignored() {
        let mut asm = StreamAssembler::new();
        for line in [
            "",
            ": keep-alive",
            "event: message",
            "id: 42",
            "retry: 1000",
        ] {
            let feed = feed(&mut asm, line);
            assert!(feed.events.is_empty());
            assert!(!feed.done);
        }
        assert!(!asm.has_visible_output());
    }

    #[test]
    fn done_terminates_and_reports_finish_reason() {
        let mut asm = StreamAssembler::new();
        feed(
            &mut asm,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        let feed = feed(&mut asm, "data: [DONE]");
        assert!(feed.done);
        assert_eq!(
            feed.events.last(),
            Some(&StreamEvent::Done {
                finish_reason: "tool_calls".into()
            })
        );
    }

    #[test]
    fn usage_is_taken_from_the_stream_and_never_invented() {
        let mut asm = StreamAssembler::new();
        let mut usage_events = 0_usize;
        for line in [
            r#"data: {"choices":[{"delta":{"content":"好"}}]}"#,
            r#"data: {"choices":[{"delta":{}}],"usage":{"prompt_tokens":11,"completion_tokens":2}}"#,
            "data: [DONE]",
        ] {
            for event in feed(&mut asm, line).events {
                if matches!(event, StreamEvent::Usage(_)) {
                    usage_events += 1;
                }
            }
        }
        assert_eq!(usage_events, 1, "usage 只能报一次，否则调用方会重复记账");
        let parts = asm.into_parts().expect("应拼装成功");
        let usage = parts.usage.expect("服务端给了 usage");
        assert_eq!(usage.total(), 13);
        assert!(!usage.estimated);

        let mut silent = StreamAssembler::new();
        feed(&mut silent, &text_frame("好"));
        assert_eq!(silent.into_parts().expect("应拼装成功").usage, None);
    }

    #[test]
    fn server_error_frames_are_surfaced_with_the_server_message() {
        let mut asm = StreamAssembler::new();
        let error = asm
            .feed_str(r#"data: {"error":{"message":"context overflow"}}"#)
            .expect_err("必须报错");
        assert!(error.to_string().contains("context overflow"));

        let mut asm = StreamAssembler::new();
        asm.feed_str("event: error").expect("event 行应被接受");
        let error = asm.feed_str("data: upstream closed").expect_err("必须报错");
        assert!(matches!(error, SseError::ServerError(_)));
    }

    #[test]
    fn oversized_events_and_streams_are_cut_off() {
        let mut asm = StreamAssembler::new();
        let huge = format!(
            "data: {{\"choices\":[],\"pad\":\"{}\"}}",
            "x".repeat(MAX_EVENT_BYTES)
        );
        assert!(matches!(
            asm.feed_str(&huge),
            Err(SseError::EventTooLarge { .. })
        ));
    }

    #[test]
    fn malformed_frames_are_not_silently_dropped() {
        let mut asm = StreamAssembler::new();
        assert!(matches!(
            asm.feed_str("data: {not json}"),
            Err(SseError::Malformed(_))
        ));
    }

    #[test]
    fn non_utf8_bytes_are_reported_instead_of_lossy_decoded() {
        let mut asm = StreamAssembler::new();
        assert!(matches!(
            asm.feed_line(&[0x64, 0x61, 0x74, 0x61, 0x3a, 0x20, 0xff]),
            Err(SseError::Malformed(_))
        ));
    }
}
