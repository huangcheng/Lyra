//! Assistant tools: what we advertise to the model and how we run them.
//!
//! A tool failure is reported *to the model* as an error object so it can
//! apologize/retry — only infrastructure errors (DB down) fail the turn.

use serde_json::{Value, json};

use super::client::ToolSpec;
use crate::storage::DbPool;

/// Tools the assistant can call. Read-only; mutations come later behind
/// explicit user confirmation.
pub(crate) fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
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
        },
        ToolSpec {
            name: "read_mail",
            description: "Read one message in full (sender, subject, date, body). \
                          Call this after search_mail when the body details matter, \
                          e.g. amounts, dates, or action items.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "string",
                        "description": "The message id exactly as it appears in a search_mail hit.",
                    },
                },
                "required": ["message_id"],
            }),
        },
        ToolSpec {
            name: "list_folders",
            description: "List the user's mail folders with unread counts. \
                          Use when asked where mail lives or for folder overviews.",
            parameters: json!({
                "type": "object",
                "properties": {},
            }),
        },
    ]
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
        "read_mail" => read_mail(db, user_id, &args).await,
        "list_folders" => list_folders(db, user_id).await,
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

async fn read_mail(db: &DbPool, user_id: &str, args: &Value) -> String {
    // Models send snake_case, camelCase, or a bare id — accept all three
    // rather than failing a turn on convention drift.
    let id = ["message_id", "messageId", "id"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str));
    let Some(id) = id else {
        return json!({ "error": "missing required argument: message_id" }).to_string();
    };
    match crate::sync::queries::load_ai_message_context(db, user_id, id).await {
        Ok(row) => {
            let from = crate::spam::from_json_email(row.from_address.as_deref())
                .unwrap_or_else(|| "unknown".into());
            json!({
                "from": from,
                "subject": row.subject,
                "date": row.date,
                "body": row.body_text,
            })
            .to_string()
        }
        Err(_) => json!({ "error": "message not found" }).to_string(),
    }
}

async fn list_folders(db: &DbPool, user_id: &str) -> String {
    match crate::sync::queries::ai_list_folders(db, user_id).await {
        Ok(folders) => json!({ "folders": folders }).to_string(),
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
    fn tool_specs_advertise_search_read_list() {
        let names: Vec<_> = tool_specs().iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["search_mail", "read_mail", "list_folders"]);
    }

    async fn seeded() -> DbPool {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let DbPool::Sqlite(p) = &db else {
            panic!("sqlite")
        };
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES ('u1', 't', 'h', '[]')",
        )
        .execute(p)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (id, user_id, display_name, email_address, protocol, \
             auth_type, credential, is_active, sync_enabled, created_at, updated_at, receive_protocol) \
             VALUES ('a1','u1','QQ','qq@x.dev','imap','password','{}',1,0,datetime('now'),datetime('now'),'imap')",
        )
        .execute(p)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO folder (id, account_id, external_id, name, role, sort_order, \
             total_messages, unread_messages, created_at, updated_at) \
             VALUES ('f1','a1','1','INBOX','inbox',0,5,2,datetime('now'),datetime('now')), \
                    ('f2','a1','2','Junk','spam',1,7,0,datetime('now'),datetime('now'))",
        )
        .execute(p)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO message (id, account_id, folder_id, external_id, subject, from_address, \
             date, body_text, is_read, is_starred, is_draft, is_deleted, has_attachments, \
             created_at, updated_at) \
             VALUES ('m1','a1','f1','9','八月发票','{\"raw\":\"b@x.dev\"}','2026-09-02 09:00:00', \
             '金额41,200元。',0,0,0,0,0,datetime('now'),datetime('now'))",
        )
        .execute(p)
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn read_mail_returns_the_body_and_sender() {
        let db = seeded().await;
        let out = execute_tool(&db, "u1", "read_mail", r#"{"messageId":"m1"}"#).await;
        assert!(out.contains("八月发票"), "subject in payload: {out}");
        assert!(out.contains("41,200"), "body in payload: {out}");
        assert!(out.contains("b@x.dev"), "sender in payload: {out}");
    }

    #[tokio::test]
    async fn read_mail_reports_unknown_ids_to_the_model() {
        let db = seeded().await;
        let out = execute_tool(&db, "u1", "read_mail", r#"{"messageId":"nope"}"#).await;
        assert!(
            out.contains("not found") || out.contains("error"),
            "payload: {out}"
        );
    }

    #[tokio::test]
    async fn list_folders_returns_names_roles_and_unread() {
        let db = seeded().await;
        let out = execute_tool(&db, "u1", "list_folders", "{}").await;
        assert!(out.contains("INBOX"));
        assert!(out.contains("spam"));
        assert!(
            out.contains("\"unread\":2"),
            "unread counts in payload: {out}"
        );
    }
}
