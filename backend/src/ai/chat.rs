//! Assistant chat: persisted per-user history plus the tool-call loop.
//!
//! One user message → up to `MAX_TOOL_ROUNDS` tool exchanges → final text.
//! Only user/assistant text turns are persisted; tool exchanges are
//! ephemeral (the model gets fresh tool capability every turn).

use sea_orm::sea_query::{Alias, DeleteStatement, Expr, Query as Sq};
use sea_orm::{ConnectionTrait, ExprTrait};
use serde::Serialize;

use super::client::{LlmClient, LlmMessage, LlmRole, LlmTurnReply, ToolCall, ToolExchange};
use super::tools;
use super::{AiError, SettingsView, load_settings};
use crate::auth::AuthState;
use crate::db_row::{IdParam, id_param};
use crate::storage::DbPool;

/// History turns sent as context (older turns drop off).
const HISTORY_CAP: usize = 30;
/// Tool exchanges allowed per user message before forcing a text answer.
const MAX_TOOL_ROUNDS: usize = 3;
/// Context cap for the attached open message (body text).
const CONTEXT_BODY_CAP: usize = 8 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryEntry {
    pub role: String,
    pub content: String,
    pub created_at: String,
}

fn orm_err(err: sea_orm::DbErr) -> AiError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => {
            std::sync::Arc::try_unwrap(e).unwrap_or_else(|s| sqlx::Error::Protocol(s.to_string()))
        }
        other => sqlx::Error::Protocol(other.to_string()),
    };
    AiError::Db(sqlx_err)
}

fn user_value(db: &DbPool, user_id: &str) -> Result<sea_orm::Value, AiError> {
    Ok(
        match id_param(db, user_id).map_err(|e| AiError::InvalidInput(e.to_string()))? {
            IdParam::Text(s) => sea_orm::Value::String(Some(s)),
            IdParam::Uuid(u) => sea_orm::Value::Uuid(Some(u)),
        },
    )
}

/// Recent history, oldest first.
pub async fn load_history(db: &DbPool, user_id: &str) -> Result<Vec<ChatHistoryEntry>, AiError> {
    let user = user_value(db, user_id)?;
    let mut sel = Sq::select();
    sel.columns([
        Alias::new("role"),
        Alias::new("content"),
        Alias::new("created_at"),
    ])
    .from(Alias::new("ai_chat_message"))
    .and_where(Expr::cust("user_id").eq(Expr::val(user)))
    .order_by_expr(Expr::cust("created_at"), sea_orm::sea_query::Order::Asc);
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    Ok(rows
        .iter()
        .map(|r| ChatHistoryEntry {
            role: r.try_get("", "role").unwrap_or_default(),
            content: r.try_get("", "content").unwrap_or_default(),
            created_at: r
                .try_get::<Option<String>>("", "created_at")
                .ok()
                .flatten()
                .map(|s| s.replace(' ', "T"))
                .unwrap_or_default(),
        })
        .collect())
}

async fn append(db: &DbPool, user_id: &str, role: &str, content: &str) -> Result<(), AiError> {
    let user = user_value(db, user_id)?;
    let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let mut ins = Sq::insert();
    ins.into_table(Alias::new("ai_chat_message"))
        .columns([
            Alias::new("id"),
            Alias::new("user_id"),
            Alias::new("role"),
            Alias::new("content"),
        ])
        .values_panic(vec![
            Expr::val(
                match id_param(db, &id).map_err(|e| AiError::InvalidInput(e.to_string()))? {
                    IdParam::Text(s) => sea_orm::Value::String(Some(s)),
                    IdParam::Uuid(u) => sea_orm::Value::Uuid(Some(u)),
                },
            ),
            Expr::val(user),
            Expr::val(role),
            Expr::val(content),
        ]);
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(())
}

/// Clear the conversation wholesale.
pub async fn clear(db: &DbPool, user_id: &str) -> Result<(), AiError> {
    let user = user_value(db, user_id)?;
    let mut del = DeleteStatement::new();
    del.from_table(Alias::new("ai_chat_message"))
        .and_where(Expr::cust("user_id").eq(Expr::val(user)));
    db.orm().execute(&del).await.map_err(orm_err)?;
    Ok(())
}

/// The assistant system prompt, plus the open message's content when the
/// panel attached one.
fn system_prompt(context_body: Option<&str>) -> String {
    let today = chrono::Utc::now().format("%Y-%m-%d");
    let mut p = format!(
        "You are Lyra's mail assistant, helping the user with their mailbox. \
         Today is {today}. Answer in the user's language, concisely. \
         You can call search_mail to find messages before answering questions \
         about them; cite subjects/dates from the hits. You cannot send, \
         move, or modify mail — suggest what the user could do instead."
    );
    if let Some(body) = context_body {
        p.push_str("\n\nThe user currently has this message open:\n\n");
        p.push_str(body);
    }
    p
}

/// The open-message context block (subject/sender/body, truncated).
async fn context_block(db: &DbPool, user_id: &str, message_id: &str) -> Option<String> {
    let row = crate::sync::queries::load_ai_message_context(db, user_id, message_id)
        .await
        .ok()?;
    let from = crate::spam::from_json_email(row.from_address.as_deref())
        .unwrap_or_else(|| "unknown sender".into());
    let subject = row.subject.unwrap_or_else(|| "(no subject)".into());
    let body = row.body_text.unwrap_or_default();
    let body = if body.len() > CONTEXT_BODY_CAP {
        format!("{}\n…(truncated)", &body[..CONTEXT_BODY_CAP])
    } else {
        body
    };
    Some(format!("From: {from}\nSubject: {subject}\n\n{body}"))
}

/// One assistant turn: gates, context build, tool loop, persistence.
/// Returns the assistant's final text.
pub async fn chat(
    state: &AuthState,
    user_id: &str,
    message: &str,
    message_id: Option<&str>,
) -> Result<String, AiError> {
    let db = state.db();
    let settings = load_settings(db, user_id).await?;
    if !settings.enabled {
        return Err(AiError::NotConfigured);
    }
    if !settings.features.assistant {
        return Err(AiError::FeatureDisabled);
    }
    let view = SettingsView::ready(&settings).ok_or(AiError::NotConfigured)?;
    let dek = AuthState::get_user_dek(db, user_id).await?;
    let key = view.decrypt_key(&dek)?;
    let client = LlmClient::new(&view, &key);

    // History (cap) + this turn's user message.
    let mut history: Vec<LlmMessage> = load_history(db, user_id)
        .await?
        .into_iter()
        .rev()
        .take(HISTORY_CAP)
        .rev()
        .map(|m| LlmMessage {
            role: if m.role == "user" {
                LlmRole::User
            } else {
                LlmRole::Assistant
            },
            content: m.content,
        })
        .collect();
    history.push(LlmMessage {
        role: LlmRole::User,
        content: message.to_string(),
    });

    let context = match message_id.filter(|id| !id.is_empty()) {
        Some(id) => context_block(db, user_id, id).await,
        None => None,
    };
    let system = system_prompt(context.as_deref());

    // Accumulated tool exchanges for THIS turn, replayed natively per
    // dialect on every subsequent round.
    let mut exchanges: Vec<ToolRound> = Vec::new();
    let mut reply = String::new();
    for round in 0..=MAX_TOOL_ROUNDS {
        let tools = if round == MAX_TOOL_ROUNDS {
            // Final round: no tools, force a text answer.
            &[][..]
        } else {
            &tools::tool_specs()[..]
        };
        let tool_replay: Vec<ToolExchange> = exchanges
            .iter()
            .map(|(calls, results)| ToolExchange { calls, results })
            .collect();
        let turn = client.chat(&system, &history, tools, &tool_replay).await?;
        match turn {
            LlmTurnReply::Text(text) => {
                reply = text;
                break;
            }
            LlmTurnReply::ToolCalls(calls) => {
                let results = run_tools(db, user_id, &calls).await;
                exchanges.push((calls, results));
            }
        }
    }

    append(db, user_id, "user", message).await?;
    append(db, user_id, "assistant", &reply).await?;
    Ok(reply)
}

/// One completed round: the model's calls plus their (id, payload) results.
type ToolRound = (Vec<ToolCall>, Vec<(String, String)>);

/// Execute every call; results align with `calls` by position (id, payload).
async fn run_tools(db: &DbPool, user_id: &str, calls: &[ToolCall]) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(calls.len());
    for call in calls {
        out.push((
            call.id.clone(),
            tools::execute_tool(db, user_id, &call.name, &call.arguments).await,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_includes_date_and_context() {
        let p = system_prompt(None);
        assert!(p.contains("Today is"));
        let with_ctx = system_prompt(Some("From: a@b.com\nSubject: hi\n\nbody"));
        assert!(with_ctx.contains("Subject: hi"));
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;

    async fn pool() -> DbPool {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let DbPool::Sqlite(p) = &db else {
            panic!("sqlite");
        };
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES ('u1', 'aitest', 'hash', '[]')",
        )
        .execute(p)
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn history_roundtrips_in_order_and_clears() {
        let db = pool().await;
        assert!(load_history(&db, "u1").await.unwrap().is_empty());

        append(&db, "u1", "user", "find my invoice").await.unwrap();
        append(&db, "u1", "assistant", "found 2 invoices")
            .await
            .unwrap();
        append(&db, "u1", "user", "thanks").await.unwrap();

        let history = load_history(&db, "u1").await.unwrap();
        assert_eq!(
            history.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user", "assistant", "user"]
        );
        assert_eq!(history[0].content, "find my invoice");
        assert_eq!(history[2].content, "thanks");
        assert!(!history[0].created_at.is_empty());

        clear(&db, "u1").await.unwrap();
        assert!(load_history(&db, "u1").await.unwrap().is_empty());
    }
}
