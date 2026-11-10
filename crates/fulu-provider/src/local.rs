//! 内置 2B 级 LFM 的本地 provider：与 `llama-server` 的 OpenAI 兼容 HTTP 对话。
//!
//! 架构红线（docs/02 第 8.5 节）：
//! - Rust 主进程**不链接 C++ 推理库**、不解析 GGUF；只连 `127.0.0.1` 的 HTTP；
//! - 模型差异（LFM2 的 Pythonic 工具调用 / LFM3 的原生工具调用）在此层归一为
//!   统一 `ToolCall`，绝不外泄到符文、Harness 或 UI；
//! - 缺权重 / 服务未启动时给出明确指引，不静默联网、不伪装成推理成功。

use crate::http::{self, Endpoint, HttpError};
use crate::{
    Capabilities, InvocationReply, InvocationRequest, Provider, ProviderError, ToolCall, Usage,
};
use serde_json::{Map, Value, json};
use std::time::Duration;

/// `/health` 探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub ready: bool,
    pub detail: String,
}

/// 本地推理服务适配器。端点由 `fulu-model` 的进程管理给出，默认不预设端口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLfmProvider {
    /// 例如 `http://127.0.0.1:随机端口`；空串表示尚未拉起推理进程。
    pub endpoint: String,
    pub model_id: String,
    /// 清单 `min_runtime`，启动时断言，避免静默加载不支持的架构
    pub min_runtime: String,
    pub context_tokens: u32,
    /// 单次 HTTP 读写超时；法坛另有整符超时，二者取更小者。
    pub timeout: Duration,
}

impl LocalLfmProvider {
    pub fn from_model(model_id: &str, context_tokens: u32, min_runtime: &str) -> Self {
        Self {
            endpoint: String::new(),
            model_id: model_id.to_owned(),
            min_runtime: min_runtime.to_owned(),
            context_tokens,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 本地服务只允许回环地址，避免把推理端口暴露到网络。
    pub fn set_endpoint(&mut self, endpoint: &str) -> Result<(), ProviderError> {
        Endpoint::parse(endpoint).map_err(|e| ProviderError::Transport(e.to_string()))?;
        self.endpoint = endpoint.trim_end_matches('/').to_owned();
        Ok(())
    }

    fn parsed_endpoint(&self) -> Result<Endpoint, ProviderError> {
        if self.endpoint.is_empty() {
            return Err(ProviderError::NotConfigured(
                "尚未拉起本地 llama-server；请先执行 `fulu model import <权重>` 并启动推理服务，或用 FULU_LLAMA_ENDPOINT 指向已有服务".into(),
            ));
        }
        Endpoint::parse(&self.endpoint).map_err(|e| ProviderError::Transport(e.to_string()))
    }

    /// 探活。llama.cpp 的 `/health` 在模型加载中返回 503，加载完成返回 200。
    pub fn health(&self) -> Result<Health, ProviderError> {
        let endpoint = self.parsed_endpoint()?;
        let response = http::request(&endpoint, "GET", "/health", None, self.timeout)
            .map_err(map_http_error)?;
        match response.status {
            200 => Ok(Health {
                ready: true,
                detail: response
                    .body_text()
                    .unwrap_or_else(|_| "{\"status\":\"ok\"}".into()),
            }),
            503 => Ok(Health {
                ready: false,
                detail: "模型仍在加载或权重缺失".into(),
            }),
            status => Err(ProviderError::Transport(format!(
                "/health 返回 HTTP {status}"
            ))),
        }
    }

    /// 列出服务端已加载的模型，用于校验「神明名号」是否与运行时一致。
    pub fn loaded_models(&self) -> Result<Vec<String>, ProviderError> {
        let endpoint = self.parsed_endpoint()?;
        let response = http::request(&endpoint, "GET", "/v1/models", None, self.timeout)
            .map_err(map_http_error)?;
        response
            .require_success()
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let text = response
            .body_text()
            .map_err(|e| ProviderError::Protocol(e.to_string()))?;
        let root: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Protocol(format!("/v1/models 不是 JSON：{e}")))?;
        Ok(root["data"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|item| item["id"].as_str().map(str::to_owned))
            .collect())
    }
}

fn map_http_error(error: HttpError) -> ProviderError {
    match error {
        HttpError::Timeout => ProviderError::Timeout,
        HttpError::Connect(msg) => {
            ProviderError::ModelUnavailable(format!("本地推理服务未启动或权重未加载：{msg}"))
        }
        other => ProviderError::Transport(other.to_string()),
    }
}

impl Provider for LocalLfmProvider {
    fn id(&self) -> &str {
        "lu://local/llama-server"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::local_lfm(self.context_tokens)
    }

    fn invoke(&self, request: &InvocationRequest) -> Result<InvocationReply, ProviderError> {
        let endpoint = self.parsed_endpoint()?;
        let payload = build_request_body(request)?;
        let response = http::request(
            &endpoint,
            "POST",
            "/v1/chat/completions",
            Some(payload.as_bytes()),
            self.timeout,
        )
        .map_err(map_http_error)?;
        response
            .require_success()
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let text = response
            .body_text()
            .map_err(|e| ProviderError::Protocol(e.to_string()))?;
        parse_reply(&text, request)
    }
}

/// 构造 OpenAI 兼容请求体。采样参数由整数毫单位换算；0 值不下发，交给服务端默认。
pub fn build_request_body(request: &InvocationRequest) -> Result<String, ProviderError> {
    let tools: Value = serde_json::from_str(&request.tools_json)
        .map_err(|e| ProviderError::Protocol(format!("工具定义不是合法 JSON：{e}")))?;
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model_id));
    body.insert(
        "messages".into(),
        json!([
            {"role": "system", "content": request.system},
            {"role": "user", "content": request.user}
        ]),
    );
    body.insert("stream".into(), json!(false));
    body.insert("max_tokens".into(), json!(request.budget_tokens));
    if request.sampling.temperature_milli > 0 {
        body.insert(
            "temperature".into(),
            json!(request.sampling.temperature_milli as f64 / 1000.0),
        );
    }
    if request.sampling.top_k > 0 {
        body.insert("top_k".into(), json!(request.sampling.top_k));
    }
    if request.sampling.repeat_penalty_milli > 0 {
        body.insert(
            "repeat_penalty".into(),
            json!(request.sampling.repeat_penalty_milli as f64 / 1000.0),
        );
    }
    if !tools.as_array().is_some_and(|array| array.is_empty()) {
        body.insert("tools".into(), tools);
    }
    serde_json::to_string(&Value::Object(body))
        .map_err(|e| ProviderError::Protocol(format!("请求体序列化失败：{e}")))
}

/// 解析 OpenAI 兼容响应，并把三种工具调用载体归一为统一的 `ToolCall`。
pub fn parse_reply(
    raw: &str,
    request: &InvocationRequest,
) -> Result<InvocationReply, ProviderError> {
    let root: Value = serde_json::from_str(raw)
        .map_err(|e| ProviderError::Protocol(format!("响应不是 JSON：{e}")))?;
    let choice = root["choices"]
        .get(0)
        .ok_or_else(|| ProviderError::Protocol("响应缺少 choices[0]".into()))?;
    let message = &choice["message"];
    let text = message["content"].as_str().unwrap_or("").to_owned();
    let tool_calls = normalize_tool_calls(message);

    let prompt_usage = root["usage"]["prompt_tokens"].as_u64();
    let completion_usage = root["usage"]["completion_tokens"].as_u64();
    let (prompt_tokens, completion_tokens, estimated) = match (prompt_usage, completion_usage) {
        (Some(prompt), Some(completion)) => (prompt as u32, completion as u32, false),
        _ => (
            estimate_tokens(&request.system) + estimate_tokens(&request.user),
            estimate_tokens(&text),
            true,
        ),
    };

    Ok(InvocationReply {
        text,
        tool_calls,
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            estimated,
        },
    })
}

/// 服务端没给 usage 时按字符数做**明确标注的**保守估算；不伪称为精确值。
fn estimate_tokens(text: &str) -> u32 {
    (text.chars().count() as u32).div_ceil(4)
}

fn arguments_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => "{}".into(),
    }
}

fn call_from_parts(id: Option<&str>, name: &str, arguments: String, index: usize) -> ToolCall {
    ToolCall {
        id: id
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("call-{index}")),
        name: name.to_owned(),
        arguments_json: arguments,
    }
}

/// 归一入口：① OpenAI `tool_calls` ② 旧式 `function_call` ③ 模型把调用写进 content。
pub fn normalize_tool_calls(message: &Value) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    if let Some(array) = message["tool_calls"].as_array() {
        for call in array {
            let name = call["function"]["name"]
                .as_str()
                .or_else(|| call["name"].as_str())
                .unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let raw_arguments = if call["function"]["arguments"].is_null() {
                &call["arguments"]
            } else {
                &call["function"]["arguments"]
            };
            let mut arguments = arguments_to_string(Some(raw_arguments));
            if arguments == "null" {
                arguments = "{}".into();
            }
            calls.push(call_from_parts(
                call["id"].as_str(),
                name,
                arguments,
                calls.len() + 1,
            ));
        }
    }
    if calls.is_empty() {
        let legacy = &message["function_call"];
        if let Some(name) = legacy["name"].as_str() {
            calls.push(call_from_parts(
                None,
                name,
                arguments_to_string(Some(&legacy["arguments"])),
                1,
            ));
        }
    }
    if calls.is_empty() {
        let content = message["content"].as_str().unwrap_or("");
        calls = extract_embedded_calls(content);
    }
    calls
}

/// 从正文里提取工具调用：JSON 对象/数组、````json` 代码块、Pythonic `name(k=v)`。
pub fn extract_embedded_calls(content: &str) -> Vec<ToolCall> {
    let trimmed = content.trim();
    let from_plain = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        serde_json::from_str::<Value>(trimmed)
            .map(|value| calls_from_json_value(&value))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if !from_plain.is_empty() {
        return from_plain;
    }
    let mut cursor = 0;
    while let Some(start) = content[cursor..].find("```json") {
        let begin = cursor + start + "```json".len();
        if let Some(end) = content[begin..].find("```") {
            let block = content[begin..begin + end].trim();
            if let Ok(value) = serde_json::from_str::<Value>(block) {
                let calls = calls_from_json_value(&value);
                if !calls.is_empty() {
                    return calls;
                }
            }
            cursor = begin + end + 3;
        } else {
            break;
        }
    }
    scan_pythonic_calls(content)
}

fn calls_from_json_value(value: &Value) -> Vec<ToolCall> {
    let items: Vec<&Value> = match value {
        Value::Array(array) => array.iter().collect(),
        Value::Object(_) if value["tool_calls"].is_array() => {
            value["tool_calls"].as_array().unwrap().iter().collect()
        }
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    };
    let mut calls = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let name = item["name"]
            .as_str()
            .or_else(|| item["function"]["name"].as_str())
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let arguments = item
            .get("arguments")
            .or_else(|| item.get("args"))
            .or_else(|| {
                item.get("function")
                    .and_then(|function| function.get("arguments"))
            })
            .map(arguments_to_string_owned)
            .unwrap_or_else(|| "{}".into());
        calls.push(call_from_parts(
            item["id"].as_str(),
            name,
            arguments,
            index + 1,
        ));
    }
    calls
}

fn arguments_to_string_owned(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn split_arguments(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = 0_u8;
    let mut depth = 0_i32;
    for (index, byte) in text.bytes().enumerate() {
        if quote != 0 {
            if byte == quote && index > 0 && text.as_bytes()[index - 1] != b'\\' {
                quote = 0;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = byte,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn parse_pythonic_arguments(text: &str) -> String {
    let mut map = Map::new();
    for part in split_arguments(text) {
        if let Some((key, raw_value)) = part.split_once('=') {
            let key = key.trim().trim_matches(['\'', '"']).to_owned();
            let raw = raw_value.trim();
            let value = if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                parsed
            } else if raw.len() >= 2
                && ((raw.starts_with('"') && raw.ends_with('"'))
                    || (raw.starts_with('\'') && raw.ends_with('\'')))
            {
                Value::String(raw[1..raw.len() - 1].to_owned())
            } else {
                Value::String(raw.to_owned())
            };
            map.insert(key, value);
        }
    }
    Value::Object(map).to_string()
}

/// 扫描 `get_weather(city="北京", days=3)` 这类调用；LFM2 家族常见。
fn scan_pythonic_calls(text: &str) -> Vec<ToolCall> {
    let bytes = text.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let name = &text[start..index];
            if index < bytes.len() && bytes[index] == b'(' {
                let mut depth = 1_i32;
                let mut cursor = index + 1;
                let mut quote = 0_u8;
                while cursor < bytes.len() && depth > 0 {
                    let byte = bytes[cursor];
                    if quote != 0 {
                        if byte == quote && cursor > 0 && bytes[cursor - 1] != b'\\' {
                            quote = 0;
                        }
                    } else {
                        match byte {
                            b'"' | b'\'' => quote = byte,
                            b'(' => depth += 1,
                            b')' => depth -= 1,
                            _ => {}
                        }
                    }
                    cursor += 1;
                }
                if depth == 0 {
                    let args = &text[index + 1..cursor - 1];
                    calls.push(ToolCall {
                        id: format!("py-{}", calls.len() + 1),
                        name: name.to_owned(),
                        arguments_json: parse_pythonic_arguments(args),
                    });
                    index = cursor;
                    continue;
                }
            }
        }
        index += 1;
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_core::Sampling;

    fn sample_request() -> InvocationRequest {
        InvocationRequest {
            model_id: "lfm3-1.7b-q4km".into(),
            system: "你是助手".into(),
            user: "读文件".into(),
            tools_json: "[]".into(),
            sampling: Sampling::default(),
            budget_tokens: 2048,
            context_tokens: 8192,
        }
    }

    #[test]
    fn request_body_is_openai_compatible_without_floats_in_talisman_space() {
        let body = build_request_body(&sample_request()).unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["model"], "lfm3-1.7b-q4km");
        assert_eq!(value["messages"][0]["role"], "system");
        assert_eq!(value["temperature"], 0.1);
        assert_eq!(value["top_k"], 50);
        assert_eq!(value["max_tokens"], 2048);
        assert_eq!(value["stream"], false);
        assert!(value.get("tools").is_none(), "空工具表不应下发");
    }

    #[test]
    fn parses_native_openai_tool_calls() {
        let raw = r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"c1","function":{"name":"fs.read","arguments":"{\"path\":\"a.txt\"}"}}]}}],"usage":{"prompt_tokens":12,"completion_tokens":7}}"#;
        let reply = parse_reply(raw, &sample_request()).unwrap();
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].name, "fs.read");
        assert_eq!(reply.tool_calls[0].id, "c1");
        assert_eq!(reply.tool_calls[0].arguments_json, "{\"path\":\"a.txt\"}");
        assert!(!reply.usage.estimated);
        assert_eq!(reply.usage.prompt_tokens, 12);
    }

    #[test]
    fn normalizes_legacy_function_call() {
        let message = json!({"content": "", "function_call": {"name": "fs.read", "arguments": {"path": "b.txt"}}});
        let calls = normalize_tool_calls(&message);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs.read");
        assert!(calls[0].arguments_json.contains("b.txt"));
    }

    #[test]
    fn normalizes_json_in_content() {
        let message = json!({"content": "```json\n{\"tool_calls\":[{\"name\":\"fs.read\",\"arguments\":{\"path\":\"c.txt\"}}]}\n```"});
        let calls = normalize_tool_calls(&message);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs.read");
    }

    #[test]
    fn normalizes_pythonic_lfm2_style_call() {
        let message = json!({"content": "需要读取：fs_read(path=\"d.txt\", limit=3)"});
        let calls = normalize_tool_calls(&message);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs_read");
        let args: Value = serde_json::from_str(&calls[0].arguments_json).unwrap();
        assert_eq!(args["path"], "d.txt");
        assert_eq!(args["limit"], 3);
    }

    #[test]
    fn missing_usage_is_estimated_and_flagged() {
        let raw = r#"{"choices":[{"message":{"content":"摘要完成"}}]}"#;
        let reply = parse_reply(raw, &sample_request()).unwrap();
        assert!(reply.usage.estimated);
        assert_eq!(reply.text, "摘要完成");
        assert!(reply.tool_calls.is_empty());
    }

    #[test]
    fn malformed_response_is_not_silently_accepted() {
        let raw = r#"{"choices":[]}"#;
        assert!(matches!(
            parse_reply(raw, &sample_request()),
            Err(ProviderError::Protocol(_))
        ));
    }

    #[test]
    fn unset_endpoint_is_reported_as_missing_runtime_not_as_inference_failure() {
        let provider = LocalLfmProvider::from_model("lfm3-1.7b-q4km", 8192, "llama.cpp >= b11960");
        assert!(matches!(
            provider.invoke(&sample_request()),
            Err(ProviderError::NotConfigured(_))
        ));
        assert!(provider.health().is_err());
    }
}
