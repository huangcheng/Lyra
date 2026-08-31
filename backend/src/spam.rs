//! Server-side anti-spam: sender lists, heuristic verdicts, settings.
//!
//! Deliberately deterministic and cheap — no model, no network. The engine
//! scores subject/from signals that are stable across languages (keyword
//! hits, all-caps shouting, excessive punctuation, currency/lottery bait)
//! and defers to the user's sender lists first: an explicit allow always
//! wins over a block, which always wins over heuristics.

use crate::auth::{AuthState, AuthUser};
use crate::sync::SyncError;
use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

/// Scoring sensitivity: higher = more aggressive (lower threshold).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Lenient,
    Standard,
    Strict,
}

impl Sensitivity {
    fn threshold(self) -> u8 {
        match self {
            Sensitivity::Lenient => 3,
            Sensitivity::Standard => 2,
            Sensitivity::Strict => 1,
        }
    }
}

/// Per-user spam preferences (persisted in `spam_settings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpamSettings {
    pub enabled: bool,
    pub learn: bool,
    pub auto_delete: bool,
    pub sensitivity: Sensitivity,
}

impl Default for SpamSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            learn: true,
            auto_delete: false,
            sensitivity: Sensitivity::Standard,
        }
    }
}

/// One sender-list entry; `list` distinguishes blocked from allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderEntry {
    pub email: String,
    pub list: SenderList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SenderList {
    Blocked,
    Allowed,
}

/// The minimal envelope slice the engine needs.
#[derive(Debug, Clone, Default)]
pub struct SpamEnvelope<'a> {
    pub from_email: Option<&'a str>,
    pub from_name: Option<&'a str>,
    pub subject: Option<&'a str>,
}

/// Why a message was judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Sender-list override: `blocked` or `allowed`.
    Listed(SenderList),
    /// Heuristic score met the sensitivity threshold.
    Scored(u8),
    Clean,
}

impl Verdict {
    /// True when this verdict means "sort into spam" (unit tests assert on
    /// it; the pass itself dispatches on the string form).
    #[must_use]
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn is_spam(&self) -> bool {
        matches!(
            self,
            Verdict::Listed(SenderList::Blocked) | Verdict::Scored(_)
        )
    }
}

/// Signals that always indicate marketing bait regardless of language.
const BAIT_KEYWORDS: &[&str] = &[
    "viagra",
    "casino",
    "lottery",
    "crypto bonus",
    "bitcoin earnings",
    "wire transfer urgently",
    "claim your prize",
    "winner notification",
    "limited time offer",
    "act now",
    "100% free",
    "make money fast",
    "work from home millionaire",
    "no credit check",
    "unsubscribe click here",
    "seo services guaranteed",
    "cheap meds",
    // Classic Chinese-language bait: fake invoices, prize scams, gambling.
    "代开发票",
    "中奖通知",
    "恭喜获得",
    "赌博",
    "贷款秒批",
];

/// Score the envelope's heuristic signals (0 = clean).
fn heuristic_score(env: &SpamEnvelope) -> u8 {
    let subject = env.subject.unwrap_or_default();
    let name = env.from_name.unwrap_or_default();
    let mut score = 0u8;

    let haystack = format!("{subject} {name}").to_lowercase();
    let bait_hits: u8 = BAIT_KEYWORDS
        .iter()
        .filter(|kw| haystack.contains(*kw))
        .count()
        .min(3)
        .try_into()
        .unwrap_or(u8::MAX);
    score += bait_hits;

    let letters: Vec<char> = subject.chars().filter(char::is_ascii_alphabetic).collect();
    if letters.len() >= 8 {
        let caps = letters.iter().filter(|c| c.is_ascii_uppercase()).count();
        if caps * 10 >= letters.len() * 8 {
            score += 1; // shouting subject
        }
    }
    let bangs = subject.chars().filter(|c| *c == '!').count();
    if bangs >= 3 {
        score += 1;
    }
    score
}

/// Does `sender` (an email) match a list entry? Entries may be a full
/// address (`a@b.com`) or a domain (`@b.com` / `b.com`).
fn sender_matches(entry_email: &str, sender: &str) -> bool {
    let entry = entry_email.trim().to_lowercase();
    let sender = sender.trim().to_lowercase();
    if entry == sender {
        return true;
    }
    let entry_domain = entry.strip_prefix('@').unwrap_or(&entry);
    if entry.starts_with('@') || !entry.contains('@') {
        return sender
            .rsplit('@')
            .next()
            .is_some_and(|d| d == entry_domain || d.ends_with(&format!(".{entry_domain}")));
    }
    false
}

/// Judge one envelope under the user's settings and sender lists.
/// Precedence: allowed list → blocked list → (if enabled) heuristics.
#[must_use]
pub fn spam_verdict(
    env: &SpamEnvelope,
    settings: &SpamSettings,
    senders: &[SenderEntry],
) -> Verdict {
    let from = env.from_email.unwrap_or_default();
    if from.is_empty() {
        return Verdict::Clean; // drafts/system rows never judge
    }
    if senders
        .iter()
        .any(|s| s.list == SenderList::Allowed && sender_matches(&s.email, from))
    {
        return Verdict::Listed(SenderList::Allowed);
    }
    if senders
        .iter()
        .any(|s| s.list == SenderList::Blocked && sender_matches(&s.email, from))
    {
        return Verdict::Listed(SenderList::Blocked);
    }
    if !settings.enabled {
        return Verdict::Clean;
    }
    let score = heuristic_score(env);
    if score >= settings.sensitivity.threshold() {
        Verdict::Scored(score)
    } else {
        Verdict::Clean
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    const ON: SpamSettings = SpamSettings {
        enabled: true,
        learn: true,
        auto_delete: false,
        sensitivity: Sensitivity::Standard,
    };

    fn env<'a>(from: &'a str, name: &'a str, subject: &'a str) -> SpamEnvelope<'a> {
        SpamEnvelope {
            from_email: Some(from),
            from_name: Some(name),
            subject: Some(subject),
        }
    }

    #[test]
    fn clean_mail_stays_clean() {
        let v = spam_verdict(
            &env("team@github.com", "GitHub", "Your weekly digest"),
            &ON,
            &[],
        );
        assert_eq!(v, Verdict::Clean);
    }

    #[test]
    fn blocked_sender_spams_even_when_filtering_disabled() {
        let off = SpamSettings {
            enabled: false,
            ..ON
        };
        let blocked = [SenderEntry {
            email: "promo@example.net".into(),
            list: SenderList::Blocked,
        }];
        let v = spam_verdict(&env("promo@example.net", "Promo", "Hello"), &off, &blocked);
        assert_eq!(v, Verdict::Listed(SenderList::Blocked));
    }

    #[test]
    fn allowed_overrides_blocked() {
        // Learn may have written both; allow must win.
        let lists = [
            SenderEntry {
                email: "@example.com".into(),
                list: SenderList::Blocked,
            },
            SenderEntry {
                email: "friend@example.com".into(),
                list: SenderList::Allowed,
            },
        ];
        let v = spam_verdict(
            &env("friend@example.com", "", "URGENT!!! WINNER casino"),
            &ON,
            &lists,
        );
        assert_eq!(v, Verdict::Listed(SenderList::Allowed));
    }

    #[test]
    fn domain_block_catches_subdomains() {
        let blocked = [SenderEntry {
            email: "@mail.example.com".into(),
            list: SenderList::Blocked,
        }];
        let v = spam_verdict(&env("noreply@eu.mail.example.com", "", "hi"), &ON, &blocked);
        assert!(v.is_spam());
    }

    #[test]
    fn heuristics_respect_sensitivity() {
        let loud = env(
            "stranger@x.com",
            "",
            "Claim your prize WINNER notification!!!",
        );
        // Two keyword hits + !!! => 3.
        assert_eq!(
            spam_verdict(&loud, &ON, &[]),
            Verdict::Scored(3),
            "standard threshold is 2"
        );
        let lenient = SpamSettings {
            sensitivity: Sensitivity::Lenient,
            ..ON
        };
        assert_eq!(spam_verdict(&loud, &lenient, &[]), Verdict::Scored(3));
        let single = env("stranger@x.com", "", "casino night invite");
        assert_eq!(spam_verdict(&single, &ON, &[]), Verdict::Clean, "1 < 2");
        let strict = SpamSettings {
            sensitivity: Sensitivity::Strict,
            ..ON
        };
        assert!(spam_verdict(&single, &strict, &[]).is_spam(), "1 >= 1");
    }

    #[test]
    fn shouting_subject_counts() {
        let v = spam_verdict(
            &env("a@b.com", "", "BUY CHEAP MEDS ONLINE TODAY ONLY"),
            &ON,
            &[],
        );
        assert!(v.is_spam(), "keyword + all-caps: {v:?}");
    }

    #[test]
    fn disabled_filtering_skips_heuristics() {
        let off = SpamSettings {
            enabled: false,
            ..ON
        };
        let v = spam_verdict(&env("a@b.com", "", "FREE VIAGRA!!!"), &off, &[]);
        assert_eq!(v, Verdict::Clean);
    }

    #[test]
    fn missing_sender_is_clean() {
        let v = spam_verdict(&SpamEnvelope::default(), &ON, &[]);
        assert_eq!(v, Verdict::Clean);
    }
}

// ── Storage ──────────────────────────────────────────────────────────

use crate::db_row::{IdParam, id_param};
use crate::storage::DbPool;
use sea_orm::sea_query::{Alias, Expr, Query as Sq};
use sea_orm::{ConnectionTrait, ExprTrait};

#[derive(Debug)]
pub enum SpamStoreError {
    InvalidId(String),
    Database(sqlx::Error),
}

impl From<sqlx::Error> for SpamStoreError {
    fn from(e: sqlx::Error) -> Self {
        SpamStoreError::Database(e)
    }
}

fn orm_err(err: sea_orm::DbErr) -> SpamStoreError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => {
            std::sync::Arc::try_unwrap(e).unwrap_or_else(|s| sqlx::Error::Protocol(s.to_string()))
        }
        other => sqlx::Error::Protocol(other.to_string()),
    };
    SpamStoreError::Database(sqlx_err)
}

/// UUID-column id bind: TEXT on SQLite, native Uuid on Postgres.
fn id_value(db: &DbPool, id: &str) -> Result<sea_orm::Value, SpamStoreError> {
    Ok(
        match id_param(db, id).map_err(|e| SpamStoreError::InvalidId(e.to_string()))? {
            IdParam::Text(s) => sea_orm::Value::String(Some(s)),
            IdParam::Uuid(u) => sea_orm::Value::Uuid(Some(u)),
        },
    )
}

/// Load the user's settings, creating defaults on first read.
pub async fn load_settings(db: &DbPool, user_id: &str) -> Result<SpamSettings, SpamStoreError> {
    let user = id_value(db, user_id)?;
    let mut sel = Sq::select();
    sel.columns([
        Alias::new("enabled"),
        Alias::new("learn"),
        Alias::new("auto_delete"),
        Alias::new("sensitivity"),
    ])
    .from(Alias::new("spam_settings"))
    .and_where(Expr::cust("user_id").eq(Expr::val(user)));
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    let Some(row) = row else {
        return Ok(SpamSettings::default());
    };
    Ok(SpamSettings {
        enabled: row.try_get("", "enabled").unwrap_or(false),
        learn: row.try_get("", "learn").unwrap_or(true),
        auto_delete: row.try_get("", "auto_delete").unwrap_or(false),
        sensitivity: match row
            .try_get::<String>("", "sensitivity")
            .unwrap_or_default()
            .as_str()
        {
            "lenient" => Sensitivity::Lenient,
            "strict" => Sensitivity::Strict,
            _ => Sensitivity::Standard,
        },
    })
}

/// Upsert the user's settings row.
pub async fn save_settings(
    db: &DbPool,
    user_id: &str,
    settings: &SpamSettings,
) -> Result<(), SpamStoreError> {
    let user = id_value(db, user_id)?;
    let sensitivity = match settings.sensitivity {
        Sensitivity::Lenient => "lenient",
        Sensitivity::Standard => "standard",
        Sensitivity::Strict => "strict",
    };
    let mut ins = Sq::insert();
    ins.into_table(Alias::new("spam_settings"))
        .columns([
            Alias::new("user_id"),
            Alias::new("enabled"),
            Alias::new("learn"),
            Alias::new("auto_delete"),
            Alias::new("sensitivity"),
        ])
        .values_panic(vec![
            Expr::val(user),
            Expr::val(settings.enabled),
            Expr::val(settings.learn),
            Expr::val(settings.auto_delete),
            Expr::val(sensitivity),
        ])
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(Alias::new("user_id"))
                .update_columns([
                    Alias::new("enabled"),
                    Alias::new("learn"),
                    Alias::new("auto_delete"),
                    Alias::new("sensitivity"),
                ])
                .to_owned(),
        );
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(())
}

/// All sender-list entries for the user (both lists).
pub async fn list_senders(db: &DbPool, user_id: &str) -> Result<Vec<SenderEntry>, SpamStoreError> {
    let user = id_value(db, user_id)?;
    let mut sel = Sq::select();
    sel.columns([Alias::new("list"), Alias::new("email")])
        .from(Alias::new("spam_sender"))
        .and_where(Expr::cust("user_id").eq(Expr::val(user)))
        .order_by_expr(Expr::cust("created_at"), sea_orm::sea_query::Order::Asc);
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    Ok(rows
        .iter()
        .map(|r| SenderEntry {
            email: r.try_get("", "email").unwrap_or_default(),
            list: if r.try_get::<String>("", "list").unwrap_or_default() == "allowed" {
                SenderList::Allowed
            } else {
                SenderList::Blocked
            },
        })
        .collect())
}

/// Add a sender to a list; replaces a same-address entry on the *other*
/// list first (a sender is either blocked or allowed, never both).
pub async fn add_sender(
    db: &DbPool,
    user_id: &str,
    email: &str,
    list: SenderList,
) -> Result<(), SpamStoreError> {
    let user = id_value(db, user_id)?;
    let email = email.trim().to_lowercase();
    let (list_str, other_str) = match list {
        SenderList::Blocked => ("blocked", "allowed"),
        SenderList::Allowed => ("allowed", "blocked"),
    };
    let mut del = sea_orm::sea_query::DeleteStatement::new();
    del.from_table(Alias::new("spam_sender"))
        .and_where(Expr::cust("user_id").eq(user.clone()))
        .and_where(Expr::cust("email").eq(Expr::val(email.clone())))
        .and_where(Expr::cust("list").eq(Expr::val(other_str)));
    db.orm().execute(&del).await.map_err(orm_err)?;

    let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let mut ins = Sq::insert();
    ins.into_table(Alias::new("spam_sender"))
        .columns([
            Alias::new("id"),
            Alias::new("user_id"),
            Alias::new("list"),
            Alias::new("email"),
        ])
        .values_panic(vec![
            Expr::val(id_value(db, &id)?),
            Expr::val(user),
            Expr::val(list_str),
            Expr::val(email),
        ])
        .on_conflict(
            sea_orm::sea_query::OnConflict::new()
                .do_nothing_on([
                    Alias::new("user_id"),
                    Alias::new("list"),
                    Alias::new("email"),
                ])
                .to_owned(),
        );
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(())
}

/// Remove a sender entry (any list) by exact email.
pub async fn remove_sender(
    db: &DbPool,
    user_id: &str,
    email: &str,
) -> Result<bool, SpamStoreError> {
    let user = id_value(db, user_id)?;
    let mut del = sea_orm::sea_query::DeleteStatement::new();
    del.from_table(Alias::new("spam_sender"))
        .and_where(Expr::cust("user_id").eq(Expr::val(user)))
        .and_where(Expr::cust("email").eq(Expr::val(email.trim().to_lowercase())));
    let res = db.orm().execute(&del).await.map_err(orm_err)?;
    Ok(res.rows_affected() > 0)
}

// ── Tests ────────────────────────────────────────────────────────────

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
             VALUES ('u1', 'spamtest', 'hash', '[]')",
        )
        .execute(p)
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn settings_default_then_roundtrip() {
        let db = pool().await;
        let default = load_settings(&db, "u1").await.unwrap();
        assert_eq!(default, SpamSettings::default());

        let on = SpamSettings {
            enabled: true,
            learn: false,
            auto_delete: true,
            sensitivity: Sensitivity::Strict,
        };
        save_settings(&db, "u1", &on).await.unwrap();
        assert_eq!(load_settings(&db, "u1").await.unwrap(), on);

        // Upsert overwrites, not duplicates.
        save_settings(&db, "u1", &SpamSettings::default())
            .await
            .unwrap();
        assert_eq!(
            load_settings(&db, "u1").await.unwrap(),
            SpamSettings::default()
        );
    }

    #[tokio::test]
    async fn sender_lists_replace_across_lists() {
        let db = pool().await;
        add_sender(&db, "u1", "A@Example.com", SenderList::Blocked)
            .await
            .unwrap();
        // Same address on the other list replaces the entry.
        add_sender(&db, "u1", "a@example.com", SenderList::Allowed)
            .await
            .unwrap();
        assert_eq!(
            list_senders(&db, "u1").await.unwrap(),
            vec![SenderEntry {
                email: "a@example.com".into(),
                list: SenderList::Allowed,
            }]
        );
        assert!(remove_sender(&db, "u1", "a@example.com").await.unwrap());
        assert!(!remove_sender(&db, "u1", "a@example.com").await.unwrap());
        assert!(list_senders(&db, "u1").await.unwrap().is_empty());
    }
}

#[cfg(test)]
#[cfg(feature = "postgres")]
mod postgres_live {
    use super::*;

    // Live roundtrips for the anti-spam tables (UUID ids, bool decoding,
    // composite conflict target). See `pgtest` for the harness contract.

    use crate::pgtest::support;
    use crate::storage::DbPool;

    #[test]
    #[ignore = "needs postgres"]
    fn settings_and_senders_roundtrip() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;

            assert_eq!(
                load_settings(&db, &user_id).await.unwrap(),
                SpamSettings::default(),
                "fresh user reads defaults without a row"
            );
            let on = SpamSettings {
                enabled: true,
                learn: false,
                auto_delete: true,
                sensitivity: Sensitivity::Lenient,
            };
            save_settings(&db, &user_id, &on).await.unwrap();
            assert_eq!(load_settings(&db, &user_id).await.unwrap(), on);

            add_sender(&db, &user_id, "spam@example.com", SenderList::Blocked)
                .await
                .unwrap();
            add_sender(&db, &user_id, "spam@example.com", SenderList::Allowed)
                .await
                .unwrap();
            let senders = list_senders(&db, &user_id).await.unwrap();
            assert_eq!(senders.len(), 1, "list switch replaces, not duplicates");
            assert_eq!(senders[0].list, SenderList::Allowed);

            assert!(
                remove_sender(&db, &user_id, "spam@example.com")
                    .await
                    .unwrap()
            );
        });
    }

    #[test]
    #[ignore = "needs postgres"]
    fn message_verdict_column_writes() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_account(&db, &user_id, "verdict@example.com").await;
            let folder_id = support::seed_inbox(&db, &account_id).await;
            crate::sync::store::upsert_message(
                &db,
                &account_id,
                &folder_id,
                &support::message(31, "Verdict target", "v@example.com"),
            )
            .await
            .unwrap();

            let DbPool::Postgres(pool) = &db else {
                panic!()
            };
            sqlx::query(
                "UPDATE message SET spam_verdict = 'blocked' \
                 WHERE account_id = $1::uuid AND external_id = $2",
            )
            .bind(&account_id)
            .bind(crate::sync::store::imap_message_external_id(&folder_id, 31))
            .execute(pool)
            .await
            .unwrap();

            let verdict: Option<String> = sqlx::query_scalar(
                "SELECT spam_verdict FROM message WHERE account_id = $1::uuid LIMIT 1",
            )
            .bind(&account_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(verdict.as_deref(), Some("blocked"));
        });
    }
}

// ── HTTP ─────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutSpamSettingsRequest {
    enabled: bool,
    learn: bool,
    auto_delete: bool,
    sensitivity: Sensitivity,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SpamSettingsResponse {
    #[serde(flatten)]
    settings: SpamSettings,
    senders: Vec<SenderEntry>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddSenderRequest {
    email: String,
    list: SenderList,
}

fn spam_store_err(e: SpamStoreError) -> SyncError {
    match e {
        SpamStoreError::InvalidId(m) => SyncError::InvalidInput(m),
        SpamStoreError::Database(db) => SyncError::Database(db),
    }
}

async fn response_for(db: &DbPool, user_id: &str) -> Result<Json<SpamSettingsResponse>, SyncError> {
    let settings = load_settings(db, user_id).await.map_err(spam_store_err)?;
    let senders = list_senders(db, user_id).await.map_err(spam_store_err)?;
    Ok(Json(SpamSettingsResponse { settings, senders }))
}

async fn get_spam(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<SpamSettingsResponse>, SyncError> {
    response_for(state.db(), &user_id).await
}

async fn put_spam(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<PutSpamSettingsRequest>,
) -> Result<Json<SpamSettingsResponse>, SyncError> {
    let settings = SpamSettings {
        enabled: body.enabled,
        learn: body.learn,
        auto_delete: body.auto_delete,
        sensitivity: body.sensitivity,
    };
    save_settings(state.db(), &user_id, &settings)
        .await
        .map_err(spam_store_err)?;
    response_for(state.db(), &user_id).await
}

async fn post_sender(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<AddSenderRequest>,
) -> Result<Json<SpamSettingsResponse>, SyncError> {
    let email = body.email.trim().to_lowercase();
    // Accept `a@b.com` or a domain form `@b.com` / `b.com`.
    let address_like = email.contains('@') || !email.is_empty();
    if !address_like || email.contains(' ') {
        return Err(SyncError::InvalidInput(
            "email must be an address or a domain".into(),
        ));
    }
    add_sender(state.db(), &user_id, &email, body.list)
        .await
        .map_err(spam_store_err)?;
    response_for(state.db(), &user_id).await
}

async fn delete_sender(
    State(state): State<AuthState>,
    Path(email): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<SpamSettingsResponse>, SyncError> {
    remove_sender(state.db(), &user_id, &email)
        .await
        .map_err(spam_store_err)?;
    response_for(state.db(), &user_id).await
}

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/settings/spam", get(get_spam).put(put_spam))
        .route("/api/v1/settings/spam/senders", post(post_sender))
        .route(
            "/api/v1/settings/spam/senders/{email}",
            delete(delete_sender),
        )
}

// ── Sync hook ────────────────────────────────────────────────────────

/// Judge a message's stored envelope against the user's lists/settings.
/// Returns the verdict string to persist ('spam'/'clean'/'blocked'/'allowed').
#[must_use]
pub fn judge_message(
    env: &SpamEnvelope,
    settings: &SpamSettings,
    senders: &[SenderEntry],
) -> Option<String> {
    let verdict = spam_verdict(env, settings, senders);
    match verdict {
        Verdict::Listed(SenderList::Blocked) => Some("blocked".into()),
        Verdict::Listed(SenderList::Allowed) => Some("allowed".into()),
        Verdict::Scored(_) => Some("spam".into()),
        Verdict::Clean => Some("clean".into()),
    }
}

// ── Learning + sync pass ─────────────────────────────────────────────

/// Learn one sender from a user action (move-to-spam blocks, move-out
/// allows). No-op unless learning is enabled; also bypasses learning when
/// the address is missing.
pub async fn learn_sender(db: &DbPool, user_id: &str, email: &str, block: bool) -> bool {
    if email.trim().is_empty() || !email.contains('@') {
        return false;
    }
    let Ok(settings) = load_settings(db, user_id).await else {
        return false;
    };
    if !settings.learn {
        return false;
    }
    let list = if block {
        SenderList::Blocked
    } else {
        SenderList::Allowed
    };
    add_sender(db, user_id, email, list).await.is_ok()
}

/// Parse a bare address out of a stored `from_address` JSON text
/// (`{"raw": "Name <a@b.com>"}`, `{"email": ...}`, or an array).
#[must_use]
pub fn from_json_email(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let extract = |o: &serde_json::Value| -> Option<String> {
        if let Some(e) = o.get("email").and_then(|e| e.as_str()) {
            return Some(e.to_string());
        }
        // {"raw": "Name <a@b.com>"} — the shape our sync writes.
        o.get("raw")
            .and_then(|r| r.as_str())
            .and_then(|r| r.split('<').nth(1))
            .and_then(|r| r.split('>').next())
            .map(str::to_string)
    };
    match &v {
        serde_json::Value::Object(_) => extract(&v).or_else(|| {
            v.get("raw")
                .and_then(|r| r.as_str())
                .and_then(|r| r.split('<').nth(1))
                .and_then(|r| r.split('>').next())
                .map(str::to_string)
        }),
        serde_json::Value::Array(a) => a.first().and_then(extract),
        serde_json::Value::String(s) => extract(&serde_json::json!({"raw": s})),
        _ => None,
    }
}
