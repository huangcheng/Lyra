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
        ToolSpec {
            name: "propose_draft",
            description: "Stage a reply/compose draft for the user. Nothing is sent — \
                          the user reviews the draft and decides. Use when asked to \
                          write or reply to mail.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Recipient address(es), comma separated." },
                    "subject": { "type": "string" },
                    "body": { "type": "string", "description": "Full draft body in the conversation's language." },
                },
                "required": ["to", "subject", "body"],
            }),
        },
        ToolSpec {
            name: "propose_move",
            description: "Propose filing one message. Nothing moves until the user \
                          confirms. action is one of spam, archive, trash, notSpam.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": { "type": "string", "description": "From a search_mail hit." },
                    "action": {
                        "type": "string",
                        "enum": ["spam", "archive", "trash", "notSpam"],
                        "description": "spam = move to the junk/spam folder; trash = deleted items; archive; notSpam = rescue from junk to inbox.",
                    },
                },
                "required": ["message_id", "action"],
            }),
        },
    ]
}

/// Run one tool call. Returns the tool-result payload for the model
/// (always valid JSON) plus a pending user-confirmation action when the
/// tool was a mutation proposal (nothing executed server-side).
pub(crate) async fn execute_tool(
    db: &DbPool,
    user_id: &str,
    name: &str,
    arguments: &str,
) -> (String, Option<PendingAction>) {
    let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    match name {
        "search_mail" => (search_mail(db, user_id, &args).await, None),
        "read_mail" => (read_mail(db, user_id, &args).await, None),
        "list_folders" => (list_folders(db, user_id).await, None),
        "propose_draft" => propose_draft(&args),
        "propose_move" => propose_move(&args),
        other => (
            json!({ "error": format!("unknown tool: {other}") }).to_string(),
            None,
        ),
    }
}

fn propose_draft(args: &Value) -> (String, Option<PendingAction>) {
    let pick = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    match (pick("to"), pick("subject"), pick("body")) {
        (Some(to), Some(subject), Some(body)) if !to.trim().is_empty() && !subject.trim().is_empty() => {
            (
                json!({ "ok": true, "status": "draft ready", "note": "The user must review the draft and send it themselves." }).to_string(),
                Some(PendingAction::OpenDraft { to, subject, body }),
            )
        }
        _ => (
            json!({ "error": "to, subject and body are all required" }).to_string(),
            None,
        ),
    }
}

fn propose_move(args: &Value) -> (String, Option<PendingAction>) {
    let id = ["message_id", "messageId", "id"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str));
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    match (id, action) {
        (Some(message_id), a) if !message_id.is_empty() && valid_move_action(a) => (
            json!({ "ok": true, "note": "Move staged — the user must confirm." }).to_string(),
            Some(PendingAction::MoveMessage {
                message_id: message_id.to_string(),
                action: a.to_string(),
            }),
        ),
        _ => (
            json!({
                "error": "message_id and action are required; action must be one of spam, archive, trash, notSpam"
            })
            .to_string(),
            None,
        ),
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
        let (out, action) = execute_tool(db.pool(), "u1", "search_mail", r#"{"query":"a"}"#).await;
        assert!(out.contains("at least 2 characters"));
        assert_eq!(action, None);
    }

    #[tokio::test]
    async fn unknown_tools_are_reported_to_the_model() {
        let db = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        let (out, action) = execute_tool(db.pool(), "u1", "teleport", "{}").await;
        assert!(out.contains("unknown tool"));
        assert_eq!(action, None);
    }

    #[test]
    fn tool_specs_advertise_search_read_list() {
        let names: Vec<_> = tool_specs().iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![
                "search_mail",
                "read_mail",
                "list_folders",
                "propose_draft",
                "propose_move",
            ]
        );
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
        let (out, _) = execute_tool(&db, "u1", "read_mail", r#"{"messageId":"m1"}"#).await;
        assert!(out.contains("八月发票"), "subject in payload: {out}");
        assert!(out.contains("41,200"), "body in payload: {out}");
        assert!(out.contains("b@x.dev"), "sender in payload: {out}");
    }

    #[tokio::test]
    async fn read_mail_reports_unknown_ids_to_the_model() {
        let db = seeded().await;
        let (out, _) = execute_tool(&db, "u1", "read_mail", r#"{"messageId":"nope"}"#).await;
        assert!(
            out.contains("not found") || out.contains("error"),
            "payload: {out}"
        );
    }

    #[tokio::test]
    async fn list_folders_returns_names_roles_and_unread() {
        let db = seeded().await;
        let (out, _) = execute_tool(&db, "u1", "list_folders", "{}").await;
        assert!(out.contains("INBOX"));
        assert!(out.contains("spam"));
        assert!(
            out.contains("\"unread\":2"),
            "unread counts in payload: {out}"
        );
    }
}

// ── Confirm-first actions (phase E) ─────────────────────────────────
//
// The assistant may PROPOSE mutations; nothing is executed server-side.
// Proposals ride back on the chat response and the panel renders a
// confirm card — the user's click runs the existing action endpoints.

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum PendingAction {
    /// Open a compose dialog prefilled with the draft (dialog = confirmation).
    OpenDraft {
        to: String,
        subject: String,
        body: String,
    },
    /// File one message via the existing per-role endpoints; `action` is
    /// `spam` | `archive` | `trash` | `notSpam`.
    MoveMessage { message_id: String, action: String },
}

fn valid_move_action(a: &str) -> bool {
    matches!(a, "spam" | "archive" | "trash" | "notSpam")
}

#[cfg(test)]
mod action_tests {
    use super::*;

    #[tokio::test]
    async fn propose_draft_yields_an_open_draft_action() {
        let db = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        let (result, action) = execute_tool(
            db.pool(),
            "u1",
            "propose_draft",
            r#"{"to":"zhangwei@partner.example.com","subject":"Re: 会议确认","body":"好的,周四见。"}"#,
        )
        .await;
        assert!(result.contains("draft ready"));
        assert_eq!(
            action,
            Some(PendingAction::OpenDraft {
                to: "zhangwei@partner.example.com".into(),
                subject: "Re: 会议确认".into(),
                body: "好的,周四见。".into(),
            })
        );
    }

    #[tokio::test]
    async fn propose_move_validates_the_action_and_message() {
        let db = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        let (_, action) = execute_tool(
            db.pool(),
            "u1",
            "propose_move",
            r#"{"message_id":"m1","action":"spam"}"#,
        )
        .await;
        assert_eq!(
            action,
            Some(PendingAction::MoveMessage {
                message_id: "m1".into(),
                action: "spam".into(),
            })
        );

        let (result, action) = execute_tool(
            db.pool(),
            "u1",
            "propose_move",
            r#"{"message_id":"m1","action":"delete-everything"}"#,
        )
        .await;
        assert!(result.contains("error"));
        assert_eq!(action, None);
    }
}
