//! LLM client seam with dialect adapters.
//!
//! One `complete(system, prompt) → text` call per dialect; base-URL and
//! auth conventions differ per dialect (documented in the settings UI):
//!
//! | dialect           | endpoint                    | base-URL convention              | auth               |
//! |-------------------|-----------------------------|----------------------------------|--------------------|
//! | `openai_chat`     | `POST {base}/chat/completions` | versioned root (`…/v1`)       | `Authorization: Bearer` |
//! | `openai_responses`| `POST {base}/responses`     | versioned root (`…/v1`)          | `Authorization: Bearer` |
//! | `anthropic`       | `POST {base}/v1/messages`   | API root (e.g. `https://api.anthropic.com`) | `x-api-key` + `anthropic-version` |
//!
//! Any Chat-Completions-compatible endpoint (DashScope compatible-mode,
//! Ollama `/v1`, vLLM) works through `openai_chat`.

use std::time::Duration;

use serde_json::{Value, json};

use super::{AiDialect, AiError, SettingsView};

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_TOKENS: u32 = 1024;

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

    /// One completion call; the reply text on success.
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
    fn missing_content_is_none() {
        let v = json!({ "error": { "message": "bad model" } });
        assert!(parse_chat_completions(&v).is_none());
        assert!(parse_responses(&v).is_none());
        assert!(parse_anthropic(&v).is_none());
    }
}
