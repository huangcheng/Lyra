//! Assistant tools: what we advertise to the model and how we run them.
//!
//! A tool failure is reported *to the model* as an error object so it can
//! apologize/retry — only infrastructure errors (DB down) fail the turn.

use serde_json::{Value, json};

use super::client::ToolSpec;
use crate::storage::DbPool;

/// Tools the assistant can call. v1: mail search only.
pub(crate) fn tool_specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "search_mail",
        description: "Search the user's mailbox by keyword (subject, body, sender). \
                      Use this before answering questions about specific emails, \
                      e.g. finding an invoice or a message from someone.",
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keywords; use the language of the emails (Chinese or English).",
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum hits to return (1-20, default 10).",
                },
            },
            "required": ["query"],
        }),
    }]
}

/// Run one tool call; the return value is the tool-result payload for the
/// model (always valid JSON).
pub(crate) async fn execute_tool(
    db: &DbPool,
    user_id: &str,
    name: &str,
    arguments: &str,
) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    match name {
        "search_mail" => search_mail(db, user_id, &args).await,
        other => json!({ "error": format!("unknown tool: {other}") }).to_string(),
    }
}

async fn search_mail(db: &DbPool, user_id: &str, args: &Value) -> String {
    let Some(query) = args.get("query").and_then(Value::as_str) else {
        return json!({ "error": "missing required argument: query" }).to_string();
    };
    let query = query.trim();
    if query.chars().count() < 2 {
        return json!({ "error": "query must be at least 2 characters" }).to_string();
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(10)
        .clamp(1, 20);
    match crate::sync::queries::ai_search_mail(db, user_id, query, limit).await {
        Ok(hits) => {
            if hits.is_empty() {
                json!({ "results": [], "note": "no matching messages" }).to_string()
            } else {
                json!({ "results": hits }).to_string()
            }
        }
        Err(e) => json!({ "error": e.to_string() }).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn short_queries_return_an_error_payload() {
        let db = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        let out = execute_tool(db.pool(), "u1", "search_mail", r#"{"query":"a"}"#).await;
        assert!(out.contains("at least 2 characters"));
    }

    #[tokio::test]
    async fn unknown_tools_are_reported_to_the_model() {
        let db = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        let out = execute_tool(db.pool(), "u1", "teleport", "{}").await;
        assert!(out.contains("unknown tool"));
    }

    #[test]
    fn tool_specs_advertise_search_mail() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "search_mail");
    }
}
