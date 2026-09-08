//! LLM client seam with dialect adapters.
//!
//! One `complete(system, prompt) → text` call per dialect; base-URL and
//! auth conventions differ per dialect (documented in the settings UI):
//!
//! | dialect           | endpoint                       | base-URL convention              | auth               |
//! |-------------------|--------------------------------|----------------------------------|--------------------|
//! | `openai_chat`     | `POST {base}/chat/completions` | versioned root (`…/v1`)          | `Authorization: Bearer` |
//! | `openai_responses`| `POST {base}/responses`        | versioned root (`…/v1`)          | `Authorization: Bearer` |
//! | `anthropic`       | `POST {base}/v1/messages`      | API root (e.g. `https://api.anthropic.com`) | `x-api-key` + `anthropic-version` |
//!
//! Any Chat-Completions-compatible endpoint (DashScope compatible-mode,
//! Ollama `/v1`, vLLM) works through `openai_chat`. `chat()` additionally
//! carries multi-turn history and tool definitions; tool-call round trips
//! are replayed per dialect so all three support the assistant loop.

use std::time::Duration;

use serde_json::{Value, json};

use super::{AiDialect, AiError, SettingsView};

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_TOKENS: u32 = 1024;

// ── Multi-turn + tool shapes (assistant loop) ─────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LlmRole {
    User,
    Assistant,
}

/// One persisted conversation turn (tool exchanges are ephemeral).
#[derive(Debug, Clone)]
pub(crate) struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
}

/// A callable we advertise to the model.
pub(crate) struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

/// The model asking us to run a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    /// Arguments as a JSON string (wire form; parsed by the executor).
    pub arguments: String,
}

/// One completed tool round: the calls the model made plus their results,
/// in call order. Replayed to the model on the next request.
pub(crate) struct ToolExchange<'a> {
    pub calls: &'a [ToolCall],
    /// `(call id, result payload)` pairs.
    pub results: &'a [(String, String)],
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LlmTurnReply {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

pub(crate) struct LlmClient<'a> {
    dialect: AiDialect,
    base_url: String,
    model: String,
    api_key: &'a str,
}

impl<'a> LlmClient<'a> {
    pub(crate) fn new(view: &SettingsView, api_key: &'a str) -> Self {
        Self {
            dialect: view.dialect,
            base_url: view.base_url.trim_end_matches('/').to_string(),
            model: view.model.clone(),
            api_key,
        }
    }

    /// One assistant turn: history (ending with the latest user message),
    /// optional tools, and any completed tool exchanges from earlier rounds
    /// of this same turn. Returns model text or a tool-call request.
    pub(crate) async fn chat(
        &self,
        system: &str,
        history: &[LlmMessage],
        tools: &[ToolSpec],
        exchanges: &[ToolExchange<'_>],
    ) -> Result<LlmTurnReply, AiError> {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| AiError::Unreachable(e.to_string()))?;
        let (url, headers, body) = self.chat_request(system, history, tools, exchanges);
        let mut req = client.post(&url).json(&body);
        if !headers.iter().any(|(h, _)| *h == "x-api-key") {
            req = req.bearer_auth(self.api_key);
        }
        for (name, value) in headers {
            req = req.header(name, value);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                AiError::Timeout
            } else {
                AiError::Unreachable(e.to_string())
            }
        })?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AiError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            return Err(AiError::Provider(format!(
                "HTTP {status}: {}",
                truncate(&text, 300)
            )));
        }
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| AiError::Provider(format!("non-JSON reply: {e}")))?;
        let reply = match self.dialect {
            AiDialect::OpenAiChat => parse_chat_tool_turn(&parsed),
            AiDialect::OpenAiResponses => parse_responses_tool_turn(&parsed),
            AiDialect::Anthropic => parse_anthropic_tool_turn(&parsed),
        };
        match reply {
            LlmTurnReply::Text(t) if t.trim().is_empty() => {
                Err(AiError::Provider("empty reply".into()))
            }
            other => Ok(other),
        }
    }

    /// Build the per-dialect request for one assistant turn.
    #[allow(clippy::too_many_lines)] // one arm per dialect; splitting hides the shape
    fn chat_request(
        &self,
        system: &str,
        history: &[LlmMessage],
        tools: &[ToolSpec],
        exchanges: &[ToolExchange<'_>],
    ) -> (String, Vec<(&'static str, &str)>, Value) {
        let base = self.base_url.trim_end_matches('/');
        match self.dialect {
            AiDialect::OpenAiChat => {
                let mut messages = vec![json!({"role": "system", "content": system})];
                for m in history {
                    messages.push(json!({
                        "role": if m.role == LlmRole::User { "user" } else { "assistant" },
                        "content": m.content,
                    }));
                }
                for ex in exchanges {
                    messages.push(json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": ex.calls.iter().map(|c| json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.arguments },
                        })).collect::<Vec<_>>(),
                    }));
                    for (id, result) in ex.results {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": result,
                        }));
                    }
                }
                let mut body = json!({
                    "model": self.model,
                    "max_tokens": MAX_OUTPUT_TOKENS,
                    "messages": messages,
                });
                if !tools.is_empty() {
                    body["tools"] = Value::Array(
                        tools
                            .iter()
                            .map(|t| {
                                json!({
                                    "type": "function",
                                    "function": {
                                        "name": t.name,
                                        "description": t.description,
                                        "parameters": t.parameters,
                                    },
                                })
                            })
                            .collect(),
                    );
                }
                (format!("{base}/chat/completions"), vec![], body)
            }
            AiDialect::OpenAiResponses => {
                let mut input = Vec::new();
                for m in history {
                    input.push(json!({
                        "role": if m.role == LlmRole::User { "user" } else { "assistant" },
                        "content": m.content,
                    }));
                }
                for ex in exchanges {
                    for c in ex.calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": c.id,
                            "name": c.name,
                            "arguments": c.arguments,
                        }));
                    }
                    for (id, result) in ex.results {
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": id,
                            "output": result,
                        }));
                    }
                }
                let mut body = json!({
                    "model": self.model,
                    "max_output_tokens": MAX_OUTPUT_TOKENS,
                    "instructions": system,
                    "input": input,
                });
                if !tools.is_empty() {
                    body["tools"] = Value::Array(
                        tools
                            .iter()
                            .map(|t| {
                                json!({
                                    "type": "function",
                                    "name": t.name,
                                    "description": t.description,
                                    "parameters": t.parameters,
                                })
                            })
                            .collect(),
                    );
                }
                (format!("{base}/responses"), vec![], body)
            }
            AiDialect::Anthropic => {
                // Strict alternation: history alternates by construction, and
                // each exchange appends assistant(tool_use) + user(tool_result).
                let mut messages = Vec::new();
                for m in history {
                    messages.push(json!({
                        "role": if m.role == LlmRole::User { "user" } else { "assistant" },
                        "content": m.content,
                    }));
                }
                for ex in exchanges {
                    let tool_uses: Vec<Value> = ex
                        .calls
                        .iter()
                        .map(|c| {
                            let input: Value =
                                serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}));
                            json!({"type": "tool_use", "id": c.id, "name": c.name, "input": input})
                        })
                        .collect();
                    let tool_results: Vec<Value> = ex
                        .results
                        .iter()
                        .map(|(id, result)| {
                            json!({"type": "tool_result", "tool_use_id": id, "content": result})
                        })
                        .collect();
                    messages.push(json!({"role": "assistant", "content": tool_uses}));
                    messages.push(json!({"role": "user", "content": tool_results}));
                }
                let mut body = json!({
                    "model": self.model,
                    "max_tokens": MAX_OUTPUT_TOKENS,
                    "system": system,
                    "messages": messages,
                });
                if !tools.is_empty() {
                    body["tools"] = Value::Array(
                        tools
                            .iter()
                            .map(|t| {
                                json!({
                                    "name": t.name,
                                    "description": t.description,
                                    "input_schema": t.parameters,
                                })
                            })
                            .collect(),
                    );
                }
                (
                    format!("{base}/v1/messages"),
                    vec![
                        ("x-api-key", self.api_key),
                        ("anthropic-version", "2023-06-01"),
                    ],
                    body,
                )
            }
        }
    }
    /// One plain completion call; the reply text on success.
    pub(crate) async fn complete(&self, system: &str, prompt: &str) -> Result<String, AiError> {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| AiError::Unreachable(e.to_string()))?;
        let (headers, body) = match self.dialect {
            AiDialect::OpenAiChat => {
                let body = json!({
                    "model": self.model,
                    "max_tokens": MAX_OUTPUT_TOKENS,
                    "messages": [
                        { "role": "system", "content": system },
                        { "role": "user", "content": prompt },
                    ],
                });
                (vec![], body)
            }
            AiDialect::OpenAiResponses => {
                let body = json!({
                    "model": self.model,
                    "max_output_tokens": MAX_OUTPUT_TOKENS,
                    "input": [
                        { "role": "system", "content": system },
                        { "role": "user", "content": prompt },
                    ],
                });
                (vec![], body)
            }
            AiDialect::Anthropic => {
                let body = json!({
                    "model": self.model,
                    "max_tokens": MAX_OUTPUT_TOKENS,
                    "system": system,
                    "messages": [{ "role": "user", "content": prompt }],
                });
                (
                    vec![
                        ("x-api-key", self.api_key),
                        ("anthropic-version", "2023-06-01"),
                    ],
                    body,
                )
            }
        };
        let url = format!("{}{}", self.base_url, self.dialect.path());

        let mut req = client.post(&url).json(&body);
        // Bearer is the default for the OpenAI dialects.
        if !headers.iter().any(|(h, _)| *h == "x-api-key") {
            req = req.bearer_auth(self.api_key);
        }
        for (name, value) in headers {
            req = req.header(name, value);
        }

        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                AiError::Timeout
            } else {
                AiError::Unreachable(e.to_string())
            }
        })?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AiError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            // Provider detail helps the user fix base URL / model / key;
            // never includes our key, only their error body.
            return Err(AiError::Provider(format!(
                "HTTP {status}: {}",
                truncate(&text, 300)
            )));
        }
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| AiError::Provider(format!("non-JSON reply: {e}")))?;
        let reply = match self.dialect {
            AiDialect::OpenAiChat => parse_chat_completions(&parsed),
            AiDialect::OpenAiResponses => parse_responses(&parsed),
            AiDialect::Anthropic => parse_anthropic(&parsed),
        }
        .ok_or_else(|| AiError::Provider("reply had no text content".into()))?;
        if reply.trim().is_empty() {
            return Err(AiError::Provider("empty reply".into()));
        }
        Ok(reply)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

/// `choices[0].message.content`.
fn parse_chat_completions(v: &Value) -> Option<String> {
    v.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Responses API: concatenate `output[].content[].text` where
/// `type == "output_text"` (reasoning items carry no content and are
/// skipped).
fn parse_responses(v: &Value) -> Option<String> {
    let out = v.get("output")?.as_array()?;
    let mut text = String::new();
    for item in out {
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                text.push_str(part.get("text").and_then(Value::as_str).unwrap_or_default());
            }
        }
    }
    Some(text).filter(|t| !t.is_empty())
}

/// `content[].text` where `type == "text"`.
fn parse_anthropic(v: &Value) -> Option<String> {
    let content = v.get("content")?.as_array()?;
    let mut text = String::new();
    for part in content {
        if part.get("type").and_then(Value::as_str) == Some("text") {
            text.push_str(part.get("text").and_then(Value::as_str).unwrap_or_default());
        }
    }
    Some(text).filter(|t| !t.is_empty())
}

/// Chat Completions: `choices[0].message` — `tool_calls[]` when present,
/// else `content`.
fn parse_chat_tool_turn(v: &Value) -> LlmTurnReply {
    let Some(message) = v.pointer("/choices/0/message") else {
        return LlmTurnReply::Text(String::new());
    };
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        let parsed: Vec<ToolCall> = calls
            .iter()
            .filter_map(|c| {
                let function = c.get("function")?;
                Some(ToolCall {
                    id: c
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: function.get("name")?.as_str()?.to_string(),
                    arguments: function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_string(),
                })
            })
            .collect();
        if !parsed.is_empty() {
            return LlmTurnReply::ToolCalls(parsed);
        }
    }
    LlmTurnReply::Text(
        message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

/// Responses API: `output[]` — `function_call` items → tool calls, else the
/// joined `output_text` parts.
fn parse_responses_tool_turn(v: &Value) -> LlmTurnReply {
    let Some(out) = v.get("output").and_then(Value::as_array) else {
        return LlmTurnReply::Text(String::new());
    };
    let mut calls = Vec::new();
    let mut text = String::new();
    for item in out {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                if let (Some(name), Some(id)) = (
                    item.get("name").and_then(Value::as_str),
                    item.get("call_id").and_then(Value::as_str),
                ) {
                    calls.push(ToolCall {
                        id: id.to_string(),
                        name: name.to_string(),
                        arguments: item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string(),
                    });
                }
            }
            _ => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if part.get("type").and_then(Value::as_str) == Some("output_text") {
                            text.push_str(
                                part.get("text").and_then(Value::as_str).unwrap_or_default(),
                            );
                        }
                    }
                }
            }
        }
    }
    if calls.is_empty() {
        LlmTurnReply::Text(text)
    } else {
        LlmTurnReply::ToolCalls(calls)
    }
}

/// Anthropic: `content[]` — `tool_use` blocks → tool calls, else joined
/// `text` blocks.
fn parse_anthropic_tool_turn(v: &Value) -> LlmTurnReply {
    let Some(content) = v.get("content").and_then(Value::as_array) else {
        return LlmTurnReply::Text(String::new());
    };
    let mut calls = Vec::new();
    let mut text = String::new();
    for part in content {
        match part.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                calls.push(ToolCall {
                    id: part
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: part
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: serde_json::to_string(part.get("input").unwrap_or(&Value::Null))
                        .unwrap_or_else(|_| "{}".into()),
                });
            }
            Some("text") => {
                text.push_str(part.get("text").and_then(Value::as_str).unwrap_or_default());
            }
            _ => {}
        }
    }
    if calls.is_empty() {
        LlmTurnReply::Text(text)
    } else {
        LlmTurnReply::ToolCalls(calls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_reply_is_extracted() {
        let v = json!({
            "id": "x", "model": "qwen3-max",
            "choices": [{ "index": 0, "finish_reason": "stop",
                "message": { "role": "assistant", "content": "Hello!" } }]
        });
        assert_eq!(parse_chat_completions(&v).as_deref(), Some("Hello!"));
    }

    #[test]
    fn responses_output_text_parts_are_joined() {
        let v = json!({
            "output": [
                { "type": "reasoning", "summary": [] },
                { "type": "message", "content": [
                    { "type": "output_text", "text": "Hi " },
                    { "type": "output_text", "text": "there" },
                ]},
            ]
        });
        assert_eq!(parse_responses(&v).as_deref(), Some("Hi there"));
    }

    #[test]
    fn anthropic_text_blocks_are_joined() {
        let v = json!({
            "content": [
                { "type": "text", "text": "Bonjour" },
                { "type": "text", "text": "!" },
            ]
        });
        assert_eq!(parse_anthropic(&v).as_deref(), Some("Bonjour!"));
    }

    #[test]
    fn chat_tool_calls_are_parsed() {
        let v = json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "call_1", "type": "function",
                    "function": { "name": "search_mail", "arguments": "{\"query\":\"invoice\"}" } }],
            }}]
        });
        assert_eq!(
            parse_chat_tool_turn(&v),
            LlmTurnReply::ToolCalls(vec![ToolCall {
                id: "call_1".into(),
                name: "search_mail".into(),
                arguments: r#"{"query":"invoice"}"#.into(),
            }])
        );
    }

    #[test]
    fn chat_plain_reply_wins_without_tool_calls() {
        let v = json!({ "choices": [{ "message": { "content": "hi" } }] });
        assert_eq!(parse_chat_tool_turn(&v), LlmTurnReply::Text("hi".into()));
    }

    #[test]
    fn responses_function_call_items_are_parsed() {
        let v = json!({
            "output": [
                { "type": "function_call", "call_id": "fc_9", "name": "search_mail",
                  "arguments": "{\"query\":\"发票\"}" },
            ]
        });
        let LlmTurnReply::ToolCalls(calls) = parse_responses_tool_turn(&v) else {
            panic!("expected tool calls");
        };
        assert_eq!(
            (calls[0].id.as_str(), calls[0].name.as_str()),
            ("fc_9", "search_mail")
        );
    }

    #[test]
    fn anthropic_tool_use_blocks_are_parsed() {
        let v = json!({
            "content": [
                { "type": "tool_use", "id": "tu_1", "name": "search_mail",
                  "input": { "query": "invoice", "limit": 5 } },
            ]
        });
        let LlmTurnReply::ToolCalls(calls) = parse_anthropic_tool_turn(&v) else {
            panic!("expected tool calls");
        };
        assert_eq!(calls[0].id, "tu_1");
        let args: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(args["query"], "invoice");
        assert_eq!(args["limit"], 5);
    }

    #[test]
    fn missing_content_is_none() {
        let v = json!({ "error": { "message": "bad model" } });
        assert!(parse_chat_completions(&v).is_none());
        assert!(parse_responses(&v).is_none());
        assert!(parse_anthropic(&v).is_none());
    }
}
