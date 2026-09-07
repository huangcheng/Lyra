# Lyra Web Push Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver OS-level new-mail notifications while Lyra is closed, via Web Push (RFC 8030/8291/8292) from a server-side fan-out task.

**Architecture:** A new `backend/src/push/` module subscribes to the existing `EventBus` (`SyncComplete`), diffs newest incoming messages against a per-account kv baseline (mirroring `frontend/src/lib/notifications.ts`), and sends RFC 8291-encrypted pushes with the `web-push` crate (message building only — HTTP goes through the existing reqwest 0.12). Subscriptions, baselines, mute prefs, and the VAPID keypair live in the existing `KvStore` (no DB migration). Frontend adds a `push` listener to `sw.js`, a `lib/push.ts` helper, and a Background Push section in the Settings notifications card.

**Tech Stack:** Rust/Axum backend, `web-push` 0.11 (default-features off), `p256` for VAPID keygen, reqwest 0.12, sea-orm `DbPool`; React/TS frontend, service worker `sw.js`, vitest.

**Spec:** `docs/superpowers/specs/2026-09-07-lyra-web-push-design.md`

**Verification baseline before starting:** `cd backend && cargo test --bin lyra_backend` (512 pass), `cd frontend && npm test` (225 pass), `make lint` clean (7 pre-existing oxlint warnings are baseline).

---

### Task 1: Backend deps + VAPID key identity in kv

**Files:**
- Modify: `backend/Cargo.toml` (dependencies section, after `rustls-pki-types` line ~85)
- Modify: `backend/src/auth/mod.rs` (add one re-export after line 28)
- Create: `backend/src/push/mod.rs`
- Create: `backend/src/push/store.rs`
- Modify: `backend/src/main.rs` (`mod push;` next to other mod declarations ~line 51)

- [ ] **Step 1: Add dependencies**

In `backend/Cargo.toml`, after the `rustls-pki-types = "1"` line:

```toml
# Web Push (RFC 8030/8291/8292): message building + VAPID signing only; the
# HTTP POST goes through our reqwest 0.12 (default-features off = no isahc).
web-push = { version = "0.11", default-features = false }
# VAPID ES256 keypair generation (PKCS#8 PEM for web-push, uncompressed point
# for the browser's applicationServerKey).
p256 = { version = "0.13", features = ["ecdsa", "pkcs8", "pem"] }
```

- [ ] **Step 2: Expose the master key accessor to the push module**

In `backend/src/auth/mod.rs`, change line 28 area from:

```rust
#[cfg(test)]
pub(crate) use dek::{TEST_MASTER_KEY, install_test_master_key};
```

to:

```rust
pub(crate) use dek::master_key;
#[cfg(test)]
pub(crate) use dek::{TEST_MASTER_KEY, install_test_master_key};
```

- [ ] **Step 3: Write the failing VAPID store test**

Create `backend/src/push/mod.rs`:

```rust
//! Web Push (RFC 8030/8291/8292): closed-app new-mail notifications.
//!
//! See `docs/superpowers/specs/2026-09-07-lyra-web-push-design.md`.

#![allow(clippy::doc_markdown)]

mod store;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::auth::install_test_master_key;
    use crate::kv::{KvStore, MemoryKv};

    #[tokio::test]
    async fn vapid_identity_is_generated_once_then_reused() {
        install_test_master_key();
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());

        let first = super::store::load_or_generate_vapid(&kv).await.unwrap();
        let second = super::store::load_or_generate_vapid(&kv).await.unwrap();

        assert_eq!(first.private_pem, second.private_pem);
        assert_eq!(first.public_key_b64, second.public_key_b64);
        // Uncompressed P-256 point: 65 bytes, base64url-no-pad = 87 chars.
        assert_eq!(first.public_key_b64.len(), 87);
        assert!(first.private_pem.contains("BEGIN PRIVATE KEY"));
        // The kv blob must not contain the raw PEM (encrypted at rest).
        let raw = kv.get(super::store::VAPID_KEY).await.unwrap().unwrap();
        assert!(!raw.contains("BEGIN PRIVATE KEY"));
    }
}
```

Run: `cd backend && cargo test --bin lyra_backend push:: -- --nocapture`
Expected: FAIL — `unresolved module crate::push` (module not wired yet).

- [ ] **Step 4: Wire the module and implement the VAPID store**

In `backend/src/main.rs` add `mod push;` alongside the other `mod` declarations (near line 51).

Create `backend/src/push/store.rs`:

```rust
//! kv persistence for push state: VAPID identity, subscriptions, per-account
//! baselines, and the server-side copy of mute prefs.
//!
//! Layout (all keys in the shared `KvStore`, Redis or in-memory):
//!   server:push-vapid          encrypted VAPID identity (JSON)
//!   push:subs:{user_id}        JSON array of StoredSubscription (cap 10)
//!   push:baseline:{account_id} JSON array of ≤15 message identities
//!   push:prefs:{user_id}       JSON StoredPrefs (mute lists + locale)

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::{EncodePrivateKey, LineEnding};
use serde::{Deserialize, Serialize};

use crate::kv::{KvError, KvStore};

pub(crate) const VAPID_KEY: &str = "server:push-vapid";
const SUBS_CAP: usize = 10;
const BASELINE_CAP: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VapidIdentity {
    pub(crate) private_pem: String,
    pub(crate) public_key_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredSubscription {
    pub(crate) endpoint: String,
    pub(crate) keys: StoredKeys,
    pub(crate) created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StoredKeys {
    pub(crate) p256dh: String,
    pub(crate) auth: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredPrefs {
    #[serde(default)]
    pub(crate) muted_folder_ids: Vec<String>,
    #[serde(default)]
    pub(crate) muted_thread_ids: Vec<String>,
    /// BCP-47-ish UI locale ("en" | "zh") for server-rendered summary pushes.
    #[serde(default = "default_locale")]
    pub(crate) locale: String,
}

fn default_locale() -> String {
    "en".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredVapid {
    private_pem_encrypted: crate::crypto::EncryptedCredential,
    public_key_b64: String,
}

/// Encryption subkey for the VAPID identity, HKDF-derived from the master key
/// (same pattern as captcha settings).
fn vapid_key() -> Result<[u8; 32], crate::crypto::CryptoError> {
    Ok(crate::crypto::derive_user_kek(
        crate::auth::master_key()?,
        "server:push-vapid",
    ))
}

/// Load the VAPID identity from kv, generating + persisting it on first use.
pub(crate) async fn load_or_generate_vapid(
    kv: &Arc<dyn KvStore>,
) -> Result<VapidIdentity, KvError> {
    if let Some(raw) = kv.get(VAPID_KEY).await? {
        if let Ok(stored) = serde_json::from_str::<StoredVapid>(&raw) {
            let pem_bytes = crate::crypto::decrypt(
                &vapid_key().map_err(|e| KvError::Internal(e.to_string()))?,
                &stored.private_pem_encrypted,
            )
            .map_err(|e| KvError::Internal(e.to_string()))?;
            let private_pem = String::from_utf8(pem_bytes)
                .map_err(|e| KvError::Internal(e.to_string()))?;
            return Ok(VapidIdentity {
                private_pem,
                public_key_b64: stored.public_key_b64,
            });
        }
        // Corrupted blob: fall through and regenerate.
    }

    let signing = SigningKey::random(&mut rand::rngs::OsRng);
    let private_pem = signing
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| KvError::Internal(format!("vapid key encode: {e}")))?
        .to_string();
    let point = signing.verifying_key().to_encoded_point(false);
    let public_key_b64 = URL_SAFE_NO_PAD.encode(point.as_bytes());

    let encrypted = crate::crypto::encrypt(
        &vapid_key().map_err(|e| KvError::Internal(e.to_string()))?,
        private_pem.as_bytes(),
    )
    .map_err(|e| KvError::Internal(e.to_string()))?;
    let blob = serde_json::to_string(&StoredVapid {
        private_pem_encrypted: encrypted,
        public_key_b64: public_key_b64.clone(),
    })
    .map_err(|e| KvError::Internal(e.to_string()))?;
    kv.set(VAPID_KEY, &blob, None).await?;

    Ok(VapidIdentity {
        private_pem,
        public_key_b64,
    })
}

fn subs_key(user_id: &str) -> String {
    format!("push:subs:{user_id}")
}

fn baseline_key(account_id: &str) -> String {
    format!("push:baseline:{account_id}")
}

fn prefs_key(user_id: &str) -> String {
    format!("push:prefs:{user_id}")
}

pub(crate) async fn load_subscriptions(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
) -> Result<Vec<StoredSubscription>, KvError> {
    match kv.get(&subs_key(user_id)).await? {
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        None => Ok(Vec::new()),
    }
}

async fn save_subscriptions(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    subs: &[StoredSubscription],
) -> Result<(), KvError> {
    let raw = serde_json::to_string(subs).map_err(|e| KvError::Internal(e.to_string()))?;
    kv.set(&subs_key(user_id), &raw, None).await
}

/// Upsert by endpoint; oldest entries drop off past the cap.
pub(crate) async fn upsert_subscription(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    sub: StoredSubscription,
) -> Result<(), KvError> {
    let mut subs = load_subscriptions(kv, user_id).await?;
    subs.retain(|s| s.endpoint != sub.endpoint);
    subs.push(sub);
    if subs.len() > SUBS_CAP {
        let excess = subs.len() - SUBS_CAP;
        subs.drain(0..excess);
    }
    save_subscriptions(kv, user_id, &subs).await
}

/// Remove one endpoint; returns true when something was removed.
pub(crate) async fn remove_subscription(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    endpoint: &str,
) -> Result<bool, KvError> {
    let mut subs = load_subscriptions(kv, user_id).await?;
    let before = subs.len();
    subs.retain(|s| s.endpoint != endpoint);
    let changed = subs.len() != before;
    if changed {
        save_subscriptions(kv, user_id, &subs).await?;
    }
    Ok(changed)
}

pub(crate) async fn load_baseline(
    kv: &Arc<dyn KvStore>,
    account_id: &str,
) -> Result<Vec<String>, KvError> {
    match kv.get(&baseline_key(account_id)).await? {
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        None => Ok(Vec::new()),
    }
}

pub(crate) async fn save_baseline(
    kv: &Arc<dyn KvStore>,
    account_id: &str,
    identities: &[String],
) -> Result<(), KvError> {
    let capped: Vec<String> = identities.iter().take(BASELINE_CAP).cloned().collect();
    let raw = serde_json::to_string(&capped).map_err(|e| KvError::Internal(e.to_string()))?;
    kv.set(&baseline_key(account_id), &raw, None).await
}

pub(crate) async fn load_prefs(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
) -> Result<StoredPrefs, KvError> {
    match kv.get(&prefs_key(user_id)).await? {
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        None => Ok(StoredPrefs::default()),
    }
}

pub(crate) async fn save_prefs(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    prefs: &StoredPrefs,
) -> Result<(), KvError> {
    let raw = serde_json::to_string(prefs).map_err(|e| KvError::Internal(e.to_string()))?;
    kv.set(&prefs_key(user_id), &raw, None).await
}
```

In `backend/src/push/mod.rs`, add `mod store;` above the test module (already in the file from Step 3).

- [ ] **Step 5: Run test to verify it passes**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: PASS (`vapid_identity_is_generated_once_then_reused`)

- [ ] **Step 6: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/src/auth/mod.rs backend/src/main.rs backend/src/push/
git commit -m "feat(push): VAPID identity + kv subscription store"
```

---

### Task 2: kv store unit tests (subscriptions, baseline, prefs)

**Files:**
- Modify: `backend/src/push/mod.rs` (extend `mod tests`)

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `backend/src/push/mod.rs`:

```rust
    use super::store::{
        StoredKeys, StoredPrefs, StoredSubscription, load_baseline, load_prefs,
        load_subscriptions, remove_subscription, save_baseline, save_prefs, upsert_subscription,
    };

    fn sub(endpoint: &str) -> StoredSubscription {
        StoredSubscription {
            endpoint: endpoint.to_string(),
            keys: StoredKeys {
                p256dh: "p256dh".into(),
                auth: "auth".into(),
            },
            created_at: "2026-09-07T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn subscriptions_upsert_remove_and_cap() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());

        upsert_subscription(&kv, "u1", sub("https://push.example/a")).await.unwrap();
        upsert_subscription(&kv, "u1", sub("https://push.example/b")).await.unwrap();
        // Same endpoint upserts instead of duplicating.
        upsert_subscription(&kv, "u1", sub("https://push.example/a")).await.unwrap();
        let subs = load_subscriptions(&kv, "u1").await.unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[1].endpoint, "https://push.example/a");

        // Users are isolated.
        assert!(load_subscriptions(&kv, "u2").await.unwrap().is_empty());

        // Cap at 10, dropping the oldest.
        for i in 0..12 {
            upsert_subscription(&kv, "u1", sub(&format!("https://push.example/{i}")))
                .await
                .unwrap();
        }
        let subs = load_subscriptions(&kv, "u1").await.unwrap();
        assert_eq!(subs.len(), 10);
        assert_eq!(subs[0].endpoint, "https://push.example/2");

        assert!(remove_subscription(&kv, "u1", "https://push.example/2").await.unwrap());
        assert!(!remove_subscription(&kv, "u1", "https://push.example/nope").await.unwrap());
        assert_eq!(load_subscriptions(&kv, "u1").await.unwrap().len(), 9);
    }

    #[tokio::test]
    async fn baseline_roundtrips_and_caps_at_15() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        assert!(load_baseline(&kv, "acc").await.unwrap().is_empty());

        let ids: Vec<String> = (0..20).map(|i| format!("<{i}@example.com>")).collect();
        save_baseline(&kv, "acc", &ids).await.unwrap();
        let loaded = load_baseline(&kv, "acc").await.unwrap();
        assert_eq!(loaded.len(), 15);
        assert_eq!(loaded[0], "<0@example.com>");
    }

    #[tokio::test]
    async fn prefs_default_and_roundtrip() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let defaults = load_prefs(&kv, "u1").await.unwrap();
        assert_eq!(defaults, StoredPrefs { muted_folder_ids: vec![], muted_thread_ids: vec![], locale: "en".into() });

        let prefs = StoredPrefs {
            muted_folder_ids: vec!["f1".into()],
            muted_thread_ids: vec!["t1".into()],
            locale: "zh".into(),
        };
        save_prefs(&kv, "u1", &prefs).await.unwrap();
        assert_eq!(load_prefs(&kv, "u1").await.unwrap(), prefs);
    }
```

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: PASS immediately (store was implemented in Task 1). If any fail, fix the store.

- [ ] **Step 2: Commit**

```bash
git add backend/src/push/mod.rs
git commit -m "test(push): kv store round-trips, caps, isolation"
```

---

### Task 3: New-mail diff (pure function mirroring the frontend)

**Files:**
- Create: `backend/src/push/diff.rs`
- Modify: `backend/src/push/mod.rs` (`mod diff;`)

The semantics MUST mirror `frontend/src/lib/notifications.ts`: incoming = folder role not in {sent, drafts, trash, spam, junk, outbox} (archive and custom folders count); identity = `message_id_header` else row id; newest-first slice of 15; baseline records all incoming identities (mutes only gate sends); empty baseline seeds silently.

- [ ] **Step 1: Write the failing tests**

Create `backend/src/push/diff.rs` with tests first (implementation below them):

```rust
//! New-mail diff for the push fan-out. Mirrors the frontend notifier
//! (`frontend/src/lib/notifications.ts`) exactly so open-app banners and
//! closed-app pushes agree on what counts as "new".

use crate::sync::queries::MessageResponse;

/// Folder roles that never count as incoming mail. Archive IS incoming.
const NON_INCOMING_ROLES: [&str; 6] = ["sent", "drafts", "trash", "spam", "junk", "outbox"];

const DIFF_LIMIT: usize = 15;

pub(crate) struct PushCandidate {
    pub(crate) id: String,
    pub(crate) identity: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

pub(crate) struct DiffOutcome {
    /// Messages to notify for (mute-filtered). Empty when seeding.
    pub(crate) fresh: Vec<PushCandidate>,
    /// The baseline to persist (all incoming identities, mute-independent).
    pub(crate) new_baseline: Vec<String>,
    /// True when this run only seeded the baseline (no prior baseline).
    pub(crate) seeded: bool,
}

fn is_incoming(role: Option<&str>) -> bool {
    !NON_INCOMING_ROLES.contains(&role.unwrap_or(""))
}

fn identity(m: &MessageResponse) -> String {
    m.message_id_header.clone().unwrap_or_else(|| m.id.clone())
}

/// Display label for the sender: first address entry's name, else email,
/// else the raw string. `from_address` is a JSON array string (or bare).
fn sender_label(from_address: Option<&str>) -> String {
    let raw = from_address.unwrap_or("");
    if let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(raw)
        .map(|v| v)
    {
        if let Some(first) = entries.first() {
            if let Some(name) = first.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
            if let Some(email) = first.get("email").and_then(|v| v.as_str()) {
                return email.to_string();
            }
        }
    }
    raw.to_string()
}

/// Diff the account's newest messages (already newest-first) against the
/// stored baseline. `muted_folders`/`muted_threads` gate sends only.
pub(crate) fn diff_new_messages(
    messages: &[MessageResponse],
    baseline: &[String],
    muted_folders: &[String],
    muted_threads: &[String],
) -> DiffOutcome {
    let incoming: Vec<&MessageResponse> = messages
        .iter()
        .filter(|m| is_incoming(m.folder_role.as_deref()))
        .take(DIFF_LIMIT)
        .collect();
    let new_baseline: Vec<String> = incoming.iter().map(|m| identity(m)).collect();
    if baseline.is_empty() {
        return DiffOutcome { fresh: Vec::new(), new_baseline, seeded: true };
    }
    let known: std::collections::HashSet<&str> =
        baseline.iter().map(String::as_str).collect();
    let fresh = incoming
        .iter()
        .filter(|m| !known.contains(identity(m).as_str()))
        .filter(|m| !muted_folders.contains(&m.folder_id))
        .filter(|m| {
            m.thread_id
                .as_ref()
                .is_none_or(|t| !muted_threads.contains(t))
        })
        .map(|m| PushCandidate {
            id: m.id.clone(),
            identity: identity(m),
            title: sender_label(m.from_address.as_deref()),
            body: m.subject.clone().unwrap_or_default(),
        })
        .collect();
    DiffOutcome { fresh, new_baseline, seeded: false }
}
```

Check: `MessageResponse` fields used here (`id`, `folder_id`, `folder_role`, `thread_id`, `message_id_header`, `from_address`, `subject`) all exist per `backend/src/sync/queries.rs:390-413`. `is_none_or` is stable since Rust 1.82; toolchain is 1.94. `queries` is declared `pub(crate) mod queries` in `sync/mod.rs`, and `MessageResponse` is `pub` — reachable as `crate::sync::queries::MessageResponse`.

Append tests to `mod tests` in `backend/src/push/mod.rs`:

```rust
    fn msg(id: &str, message_id: Option<&str>, role: Option<&str>, thread: Option<&str>) -> crate::sync::queries::MessageResponse {
        crate::sync::queries::MessageResponse {
            id: id.into(),
            account_id: "acc".into(),
            folder_id: format!("folder-{id}"),
            folder_role: role.map(str::to_string),
            thread_id: thread.map(str::to_string),
            message_id_header: message_id.map(str::to_string),
            in_reply_to: None,
            references_headers: None,
            subject: Some(format!("Subject {id}")),
            from_address: Some(r#"[{"name":"Alice","email":"alice@example.com"}]"#.into()),
            to_addresses: None,
            cc_addresses: None,
            date: None,
            snippet: None,
            body_text: None,
            body_html: None,
            is_read: false,
            is_starred: false,
            is_draft: false,
            has_attachments: false,
            labels: None,
            remote_content_blocked: false,
            opengpg: None,
        }
    }

    #[test]
    fn diff_seeds_silently_then_notifies_only_new() {
        use super::diff::diff_new_messages;
        let messages = vec![msg("m1", Some("<a@x>"), Some("inbox"), None)];
        let first = diff_new_messages(&messages, &[], &[], &[]);
        assert!(first.seeded);
        assert!(first.fresh.is_empty());
        assert_eq!(first.new_baseline, vec!["<a@x>"]);

        // Same state again: nothing new.
        let again = diff_new_messages(&messages, &first.new_baseline, &[], &[]);
        assert!(!again.seeded);
        assert!(again.fresh.is_empty());

        // New message arrives (newest first).
        let messages = vec![
            msg("m2", Some("<b@x>"), Some("inbox"), None),
            msg("m1", Some("<a@x>"), Some("inbox"), None),
        ];
        let out = diff_new_messages(&messages, &first.new_baseline, &[], &[]);
        assert_eq!(out.fresh.len(), 1);
        assert_eq!(out.fresh[0].identity, "<b@x>");
        assert_eq!(out.fresh[0].title, "Alice");
        assert_eq!(out.fresh[0].body, "Subject m2");
    }

    #[test]
    fn diff_incoming_roles_and_identity_fallback() {
        use super::diff::diff_new_messages;
        let messages = vec![
            msg("sent1", Some("<s@x>"), Some("sent"), None),
            msg("arch1", Some("<ar@x>"), Some("archive"), None),
            msg("custom1", Some("<c@x>"), None, None),
            msg("norole", None, Some("inbox"), None), // identity falls back to row id
        ];
        let seeded = diff_new_messages(&messages, &[], &[], &[]);
        assert_eq!(seeded.new_baseline, vec!["<ar@x>", "<c@x>", "norole"]);

        let baseline = vec!["<c@x>".to_string()];
        let out = diff_new_messages(&messages, &baseline, &[], &[]);
        let ids: Vec<&str> = out.fresh.iter().map(|c| c.identity.as_str()).collect();
        assert_eq!(ids, vec!["<ar@x>", "norole"]);
    }

    #[test]
    fn diff_mutes_gate_sends_but_not_baseline() {
        use super::diff::diff_new_messages;
        let messages = vec![
            msg("m1", Some("<a@x>"), Some("inbox"), None),
            msg("m2", Some("<b@x>"), Some("inbox"), Some("thread-9")),
        ];
        let out = diff_new_messages(
            &messages,
            &["<old@x>".to_string()],
            &["folder-m1".to_string()],
            &["thread-9".to_string()],
        );
        assert!(out.fresh.is_empty(), "both muted");
        assert_eq!(out.new_baseline, vec!["<a@x>", "<b@x>"], "mutes never touch the baseline");
    }
```

Note: the `msg` helper constructs every `MessageResponse` field — if the struct gained fields since this plan was written, add them here (the compiler will point at them).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: FAIL — `unresolved import crate::push::diff` if `mod diff;` missing, or compile errors pointing at field mismatches.

- [ ] **Step 3: Wire the module**

In `backend/src/push/mod.rs` add `mod diff;` next to `mod store;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: PASS (5 push tests total)

- [ ] **Step 5: Commit**

```bash
git add backend/src/push/
git commit -m "feat(push): new-mail diff mirroring frontend notifier semantics"
```

---

### Task 4: Push send via web-push + reqwest

**Files:**
- Create: `backend/src/push/send.rs`
- Modify: `backend/src/push/mod.rs` (`mod send;`)

`web-push` 0.11 with default-features off still exports `SubscriptionInfo`, `VapidSignatureBuilder`, `WebPushMessageBuilder`, `ContentEncoding`, `Urgency` (verified against the 0.11.0 source). We assemble the reqwest request manually from `WebPushMessage` fields, avoiding its http 0.2 types entirely.

- [ ] **Step 1: Write the failing test (mock push service over TCP)**

Create `backend/src/push/send.rs`:

```rust
//! Sending one encrypted push through a push service.
//!
//! `web-push` builds the VAPID JWT (RFC 8292) and encrypts the payload
//! (RFC 8291 aes128gcm); the POST itself goes through reqwest 0.12 like the
//! rest of the backend.

use web_push::{ContentEncoding, SubscriptionInfo, Urgency, VapidSignatureBuilder, WebPushMessageBuilder};

use super::store::StoredSubscription;

const TTL_SECS: u32 = 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendOutcome {
    Delivered,
    /// 404/410 — the subscription is dead and must be deleted.
    Gone,
    /// 401/403 — VAPID rejected; operator misconfiguration.
    Unauthorized,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SendBuildError {
    #[error("push message build failed: {0}")]
    Build(#[from] web_push::WebPushError),
}

/// Build + POST one push. `vapid_subject` is the VAPID `sub` contact claim
/// (e.g. `mailto:admin@example.com`).
pub(crate) async fn send_push(
    client: &reqwest::Client,
    vapid_pem: &str,
    vapid_subject: &str,
    sub: &StoredSubscription,
    payload_json: &str,
) -> Result<SendOutcome, SendBuildError> {
    let info = SubscriptionInfo::new(&sub.endpoint, &sub.keys.p256dh, &sub.keys.auth);

    let mut sig_builder = VapidSignatureBuilder::from_pem(vapid_pem.as_bytes(), &info)?;
    sig_builder.add_claim("sub", vapid_subject);
    let signature = sig_builder.build()?;

    let mut builder = WebPushMessageBuilder::new(&info);
    builder.set_ttl(TTL_SECS);
    builder.set_urgency(Urgency::Normal);
    builder.set_vapid_signature(signature);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload_json.as_bytes());
    let message = builder.build()?;

    let mut req = client
        .post(message.endpoint.to_string())
        .header("TTL", message.ttl.to_string());
    if let Some(urgency) = message.urgency {
        req = req.header("Urgency", urgency.to_string());
    }
    if let Some(topic) = &message.topic {
        req = req.header("Topic", topic);
    }
    if let Some(payload) = message.payload {
        req = req
            .header("Content-Encoding", payload.content_encoding.to_str())
            .header("Content-Type", "application/octet-stream");
        for (name, value) in payload.crypto_headers {
            req = req.header(name, value);
        }
        req = req.body(payload.content);
    }

    let outcome = match req.send().await {
        Ok(resp) => match resp.status().as_u16() {
            200..=299 => SendOutcome::Delivered,
            404 | 410 => SendOutcome::Gone,
            401 | 403 => SendOutcome::Unauthorized,
            status => SendOutcome::Failed(format!("push service returned {status}")),
        },
        Err(e) => SendOutcome::Failed(format!("push service unreachable: {e}")),
    };
    Ok(outcome)
}
```

Append to `mod tests` in `backend/src/push/mod.rs`:

```rust
    /// Spin up a one-shot TCP server that answers the first HTTP request
    /// with `status`, capturing the raw request for assertions.
    async fn mock_push_server(status: u16) -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let reason = match status {
                201 => "Created",
                410 => "Gone",
                403 => "Forbidden",
                500 => "Internal Server Error",
                _ => "OK",
            };
            let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\n\r\n");
            socket.write_all(response.as_bytes()).await.unwrap();
            let _ = tx.send(request);
        });
        (format!("http://{addr}/wpush/v1/abc"), rx)
    }

    /// A real P-256 subscription keypair (p256dh) + 16-byte auth secret,
    /// base64url-no-pad, as a browser would produce them.
    fn test_subscription(endpoint: String) -> super::store::StoredSubscription {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let ua = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        let point = ua.verifying_key().to_encoded_point(false);
        super::store::StoredSubscription {
            endpoint,
            keys: super::store::StoredKeys {
                p256dh: URL_SAFE_NO_PAD.encode(point.as_bytes()),
                auth: URL_SAFE_NO_PAD.encode([7u8; 16]),
            },
            created_at: "2026-09-07T00:00:00Z".into(),
        }
    }

    fn test_vapid_pem() -> String {
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng)
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn send_builds_encrypted_request_and_maps_statuses() {
        use super::send::{SendOutcome, send_push};
        let client = reqwest::Client::new();
        let vapid_pem = test_vapid_pem();

        // 201 → Delivered, with the RFC 8291/8292 headers on the wire.
        let (endpoint, rx) = mock_push_server(201).await;
        let outcome = send_push(&client, &vapid_pem, "mailto:test@example.com",
                                &test_subscription(endpoint), r#"{"title":"t"}"#)
            .await
            .unwrap();
        assert_eq!(outcome, SendOutcome::Delivered);
        let request = rx.await.unwrap();
        assert!(request.starts_with("POST /wpush/v1/abc HTTP/1.1"), "{request}");
        assert!(request.contains("\r\nTTL: 3600\r\n"), "{request}");
        assert!(request.contains("\r\nUrgency: normal\r\n"), "{request}");
        assert!(request.contains("\r\nContent-Encoding: aes128gcm\r\n"), "{request}");
        assert!(request.contains("\r\nAuthorization: vapid t="), "{request}");
        assert!(request.contains("\r\nEncryption: salt="), "{request}");

        // 410 → Gone (subscription must be deleted).
        let (endpoint, _rx) = mock_push_server(410).await;
        let outcome = send_push(&client, &vapid_pem, "mailto:test@example.com",
                                &test_subscription(endpoint), "{}")
            .await
            .unwrap();
        assert_eq!(outcome, SendOutcome::Gone);

        // 403 → Unauthorized (VAPID misconfiguration).
        let (endpoint, _rx) = mock_push_server(403).await;
        let outcome = send_push(&client, &vapid_pem, "mailto:test@example.com",
                                &test_subscription(endpoint), "{}")
            .await
            .unwrap();
        assert_eq!(outcome, SendOutcome::Unauthorized);

        // 500 → Failed, no panic.
        let (endpoint, _rx) = mock_push_server(500).await;
        let outcome = send_push(&client, &vapid_pem, "mailto:test@example.com",
                                &test_subscription(endpoint), "{}")
            .await
            .unwrap();
        assert!(matches!(outcome, SendOutcome::Failed(_)));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: FAIL — `unresolved import crate::push::send`

- [ ] **Step 3: Wire the module**

In `backend/src/push/mod.rs` add `mod send;`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: PASS (6 push tests). If `Authorization: vapid t=` is absent, the VAPID signature did not fold into `crypto_headers` — check that `set_vapid_signature` is called before `set_payload`+`build()` (it is, in the code above; the crate merges it in `build()`).

- [ ] **Step 5: Commit**

```bash
git add backend/src/push/
git commit -m "feat(push): RFC 8291/8292 push send via web-push + reqwest"
```

---

### Task 5: Fan-out task (EventBus → diff → send)

**Files:**
- Create: `backend/src/push/fanout.rs`
- Modify: `backend/src/push/mod.rs` (`mod fanout; pub(crate) use fanout::spawn_fanout;`)
- Modify: `backend/src/main.rs` (spawn after `scheduler::start_scheduler`, ~line 125)

- [ ] **Step 1: Write the failing integration test**

Append to `mod tests` in `backend/src/push/mod.rs`:

```rust
    /// Seed an in-memory SQLite db with one user/account/INBOX and `n`
    /// messages (uid 1..=n, message-ids <1@x>..<n@x>), oldest first.
    async fn seed_mail_db(n: u32) -> (crate::storage::DbPool, String, String) {
        let storage = crate::storage::Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let crate::storage::DbPool::Sqlite(pool) = &db else { panic!("sqlite") };
        let user_id = uuid::Uuid::new_v4().to_string();
        let account_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES (?, ?, 'hash', '[]')",
        )
        .bind(&user_id)
        .bind(format!("push-{user_id}"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (\
                 id, user_id, display_name, email_address, protocol, auth_type, \
                 credential, imap_host, imap_port, imap_security, is_active, sync_enabled\
             ) VALUES (?, ?, 'Push', 'push@example.com', 'imap', 'password', \
                       'cred', 'imap.example.com', 993, 'tls', 1, 1)",
        )
        .bind(&account_id)
        .bind(&user_id)
        .execute(pool)
        .await
        .unwrap();
        crate::sync::upsert_folder(&db, &account_id, "INBOX", None, &[]).await.unwrap();
        let folder_id = crate::sync::get_folder_id(&db, &account_id, "INBOX").await.unwrap();
        for uid in 1..=n {
            crate::sync::upsert_message(
                &db,
                &account_id,
                &folder_id,
                &crate::imap::ImapMessage {
                    uid,
                    message_id: Some(format!("<{uid}@x>")),
                    subject: Some(format!("Mail {uid}")),
                    from: Some("alice@example.com".into()),
                    to: Some("me@example.com".into()),
                    cc: None,
                    date: Some(chrono::DateTime::from_timestamp(1_757_000_000 + i64::from(uid), 0).unwrap()),
                    in_reply_to: None,
                    references: None,
                    flags: vec![],
                    size: Some(1),
                    body: None,
                    body_text: None,
                    body_html: None,
                    has_attachments: false,
                    attachments: vec![],
                },
            )
            .await
            .unwrap();
        }
        (db, user_id, account_id)
    }

    #[tokio::test]
    async fn fanout_seeds_then_pushes_new_mail_and_drops_gone_subs() {
        use super::fanout::fan_out_account;
        use super::store::{load_baseline, load_subscriptions, upsert_subscription};
        install_test_master_key();
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let client = reqwest::Client::new();

        let (db, user_id, account_id) = seed_mail_db(1).await;

        // Run 1: one live sub (201) + one dead sub (410). First run seeds
        // silently — no sends — so the dead sub survives (no send attempted).
        let (ep1, _rx1) = mock_push_server(201).await;
        let (gone_endpoint, _gone_rx) = mock_push_server(410).await;
        upsert_subscription(&kv, &user_id, test_subscription(ep1)).await.unwrap();
        upsert_subscription(&kv, &user_id, test_subscription(gone_endpoint.clone())).await.unwrap();
        fan_out_account(&db, &kv, &client, &account_id, "mailto:test@example.com").await.unwrap();
        assert_eq!(load_baseline(&kv, &account_id).await.unwrap(), vec!["<1@x>"]);
        assert_eq!(load_subscriptions(&kv, &user_id).await.unwrap().len(), 2);

        // New mail arrives; run 2 sends. Mock servers are one-shot, so
        // register a fresh live endpoint and a fresh dead (410) endpoint.
        let (live2, rx2) = mock_push_server(201).await;
        let (gone2, _g2) = mock_push_server(410).await;
        upsert_subscription(&kv, &user_id, test_subscription(live2)).await.unwrap();
        upsert_subscription(&kv, &user_id, test_subscription(gone2.clone())).await.unwrap();

        let folder_id = crate::sync::get_folder_id(&db, &account_id, "INBOX").await.unwrap();
        crate::sync::upsert_message(
            &db, &account_id, &folder_id,
            &crate::imap::ImapMessage {
                uid: 2,
                message_id: Some("<2@x>".into()),
                subject: Some("Mail 2".into()),
                from: Some("alice@example.com".into()),
                to: Some("me@example.com".into()),
                cc: None,
                date: Some(chrono::DateTime::from_timestamp(1_757_000_100, 0).unwrap()),
                in_reply_to: None, references: None, flags: vec![], size: Some(1),
                body: None, body_text: None, body_html: None,
                has_attachments: false, attachments: vec![],
            },
        ).await.unwrap();

        fan_out_account(&db, &kv, &client, &account_id, "mailto:test@example.com").await.unwrap();

        // Live endpoint got one POST carrying the encrypted payload.
        let request = rx2.await.unwrap();
        assert!(request.starts_with("POST /wpush/v1/abc HTTP/1.1"), "{request}");
        // Dead endpoints were pruned (run-1's 410 listener is consumed, but
        // gone2 answered this run); the two live subscriptions remain.
        let endpoints: Vec<String> = load_subscriptions(&kv, &user_id)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.endpoint)
            .collect();
        assert!(!endpoints.contains(&gone2), "410 endpoint pruned: {endpoints:?}");
        assert_eq!(load_baseline(&kv, &account_id).await.unwrap(), vec!["<2@x>", "<1@x>"]);
    }
```

Note: `crate::sync::upsert_folder`, `get_folder_id`, `upsert_message` are re-exported `#[cfg(test)]` from `sync/mod.rs` — available here because tests compile with cfg(test). `ImapMessage.date` type: check `crate::imap::ImapMessage` — the store.rs seed at line ~2085 uses `date: None`; the field type is `Option<DateTime<Utc>>` (adjust if the compiler disagrees).

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: FAIL — `unresolved import crate::push::fanout`

- [ ] **Step 3: Implement the fan-out**

Create `backend/src/push/fanout.rs`:

```rust
//! EventBus-driven fan-out: on `SyncComplete`, diff the account's newest
//! mail and push to every registered subscription of the account's owner.

use std::sync::Arc;

use crate::kernel::App;
use crate::kernel::events::AppEvent;
use crate::kv::KvStore;
use crate::storage::DbPool;

use super::diff::diff_new_messages;
use super::send::{SendOutcome, send_push};
use super::store::{
    load_baseline, load_or_generate_vapid, load_prefs, load_subscriptions, remove_subscription,
    save_baseline,
};

/// Never fire more than this many individual pushes per sync; the rest fold
/// into one summary push (mirrors the frontend notifier).
const MAX_PER_SYNC: usize = 3;

#[derive(Debug, serde::Serialize)]
struct PushPayload {
    title: String,
    body: String,
    tag: String,
    data: PushData,
}

#[derive(Debug, serde::Serialize)]
struct PushData {
    #[serde(rename = "messageId")]
    message_id: String,
}

/// Account → owner lookup. Returns None for unknown accounts.
async fn account_user_id(db: &DbPool, account_id: &str) -> Option<String> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
    let value = crate::sync::queries::id_value_pub(db, account_id).ok()?;
    crate::entities::mail_account::Entity::find()
        .select_only()
        .column(crate::entities::mail_account::Column::UserId)
        .filter(crate::entities::mail_account::Column::Id.eq(value))
        .into_tuple::<String>()
        .one(&db.orm())
        .await
        .ok()?
}

/// One fan-out pass for a freshly synced account. Public for tests and for
/// the event loop below.
pub(crate) async fn fan_out_account(
    db: &DbPool,
    kv: &Arc<dyn KvStore>,
    client: &reqwest::Client,
    account_id: &str,
    vapid_subject: &str,
) -> Result<(), crate::kv::KvError> {
    let Some(user_id) = account_user_id(db, account_id).await else {
        return Ok(());
    };
    let subs = load_subscriptions(kv, &user_id).await?;
    if subs.is_empty() {
        return Ok(());
    }

    let messages = crate::sync::queries::query_user_messages(db, &user_id, None, Some(account_id), None)
        .await
        .map_err(|e| crate::kv::KvError::Internal(e.to_string()))?;
    let baseline = load_baseline(kv, account_id).await?;
    let prefs = load_prefs(kv, &user_id).await?;
    let outcome = diff_new_messages(
        &messages,
        &baseline,
        &prefs.muted_folder_ids,
        &prefs.muted_thread_ids,
    );
    save_baseline(kv, account_id, &outcome.new_baseline).await?;
    if outcome.seeded || outcome.fresh.is_empty() {
        return Ok(());
    }

    let vapid = load_or_generate_vapid(kv).await?;

    let mut payloads: Vec<PushPayload> = outcome
        .fresh
        .iter()
        .take(MAX_PER_SYNC)
        .map(|c| PushPayload {
            title: if c.title.is_empty() { "New message".to_string() } else { c.title.clone() },
            body: c.body.clone(),
            tag: format!("lyra-{}", c.id),
            data: PushData { message_id: c.id.clone() },
        })
        .collect();
    let more = outcome.fresh.len() - payloads.len();
    if more > 0 {
        let title = if prefs.locale == "zh" {
            format!("还有 {more} 封新邮件")
        } else {
            format!("{more} more new messages")
        };
        payloads.push(PushPayload {
            title,
            body: String::new(),
            tag: "lyra-summary".to_string(),
            data: PushData { message_id: outcome.fresh[MAX_PER_SYNC].id.clone() },
        });
    }

    for payload in &payloads {
        let json = serde_json::to_string(payload)
            .map_err(|e| crate::kv::KvError::Internal(e.to_string()))?;
        for sub in &subs {
            match send_push(client, &vapid.private_pem, vapid_subject, sub, &json).await {
                Ok(SendOutcome::Delivered) => {}
                Ok(SendOutcome::Gone) => {
                    tracing::info!(endpoint = %sub.endpoint, "push subscription gone; removing");
                    let _ = remove_subscription(kv, &user_id, &sub.endpoint).await;
                }
                Ok(SendOutcome::Unauthorized) => {
                    tracing::error!("push service rejected VAPID identity; check LYRA_VAPID_SUBJECT");
                }
                Ok(SendOutcome::Failed(e)) => {
                    tracing::debug!(error = %e, "push send failed (best-effort)");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "push message build failed");
                }
            }
        }
    }
    Ok(())
}

/// Subscribe to sync events and fan out forever. Spawned from `main`.
pub(crate) fn spawn_fanout(
    db: DbPool,
    kv: Arc<dyn KvStore>,
    app: Arc<App>,
    vapid_subject: String,
) {
    let mut rx = app.events.subscribe();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        loop {
            match rx.recv().await {
                Ok(AppEvent::SyncComplete { account_id }) => {
                    if let Err(e) =
                        fan_out_account(&db, &kv, &client, &account_id, &vapid_subject).await
                    {
                        tracing::warn!(error = %e, account = %account_id, "push fan-out failed");
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
```

`PushPayload` owns its `String`s (not borrows) so the locale-dependent summary title can be built inline.

Check `db.orm()` exists: `DbPool::orm()` is used in `sync/mod.rs` (`db.orm().execute(&stmt)`), and `id_value_pub` is `pub` in `sync/queries.rs:47`. `query_user_messages` is `pub(crate)` there. `kernel::events::AppEvent` — check `events` is a pub module or re-exported: `sync/http.rs` imports `AppEvent`; verify the path (`crate::kernel::events::AppEvent` or `crate::kernel::AppEvent`) with a grep before writing, and fix the import accordingly.

- [ ] **Step 4: Wire module + spawn in main**

In `backend/src/push/mod.rs`: add `mod fanout;` and `pub(crate) use fanout::spawn_fanout;`.

In `backend/src/main.rs` after `jmap_push::start_jmap_push_supervisor(auth_state.db.clone());` (~line 127), add:

```rust
    push::spawn_fanout(
        auth_state.db.clone(),
        auth_state.kv().clone(),
        std::sync::Arc::clone(&auth_state.app),
        config.vapid_subject.clone(),
    );
```

(`auth_state.kv()` returns `&Arc<dyn KvStore>` per `auth/state.rs:68`; `config.vapid_subject` is added in Task 6 — until then, pass `String::new()` temporarily or do Task 6's config change first. Recommended: do Task 6's Step 1 config change before this wiring.)

- [ ] **Step 5: Run test to verify it passes**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: PASS (7 push tests)

- [ ] **Step 6: Commit**

```bash
git add backend/src/push/ backend/src/main.rs
git commit -m "feat(push): EventBus fan-out on sync complete"
```

---

### Task 6: Config — `LYRA_VAPID_SUBJECT`

**Files:**
- Modify: `backend/src/config.rs` (Config struct + `from_env`)
- Modify: `.env.example`

- [ ] **Step 1: Add the config field**

In `backend/src/config.rs`, find the `Config` struct field near `public_url` (search `pub public_url: String`) and add after it:

```rust
    /// VAPID `sub` contact for Web Push (RFC 8292). Defaults to
    /// `mailto:admin@<LYRA_PUBLIC_URL host>`.
    pub vapid_subject: String,
```

In `from_env`, right after the `public_url` normalization (~line 271), add:

```rust
        let vapid_subject = env::var("LYRA_VAPID_SUBJECT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let host = public_url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap_or("localhost");
                format!("mailto:admin@{host}")
            });
```

and add `vapid_subject,` to the struct literal returned by `from_env`.

Also update the doc comment above `from_env` (the `///   - LYRA_PUBLIC_URL — required; ...` list) with:

```rust
    ///   - `LYRA_VAPID_SUBJECT` — optional; VAPID contact for Web Push
```

- [ ] **Step 2: Fix every other Config construction site**

Run `cd backend && cargo check 2>&1 | grep "vapid_subject"` — every test/helper that builds a `Config` literal now fails. Add `vapid_subject: "mailto:test@example.com".to_string(),` to each.

- [ ] **Step 3: Document in `.env.example`**

Append to `.env.example` (near other optional server settings):

```bash
# Web Push (RFC 8292) contact claim, shown to push services (Google/Mozilla/
# Apple) for abuse reports. Defaults to mailto:admin@<LYRA_PUBLIC_URL host>.
# LYRA_VAPID_SUBJECT=mailto:admin@example.com
```

- [ ] **Step 4: Verify**

Run: `cd backend && cargo test --bin lyra_backend config`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add backend/src/config.rs .env.example
git commit -m "feat(push): LYRA_VAPID_SUBJECT config with public-url default"
```

---

### Task 7: HTTP endpoints

**Files:**
- Create: `backend/src/push/http.rs`
- Modify: `backend/src/push/mod.rs` (`mod http; pub(crate) use http::routes;`)
- Modify: `backend/src/main.rs` (`.merge(push::routes())` in `api_router`, after `.merge(auth::routes())` ~line 174)
- Modify: `backend/src/auth/state.rs` (add `vapid_subject: String` to `AuthState` + `AuthState::new` from `config`)

Endpoints (all under existing `AuthUser` bearer auth):

| Route | Handler |
|---|---|
| `GET /api/v1/push/vapid-key` | `{ publicKey }` |
| `GET /api/v1/push/status` | `{ devices }` |
| `PUT /api/v1/push/subscription` | upsert `{ endpoint, keys: { p256dh, auth } }` |
| `DELETE /api/v1/push/subscription` | remove by `{ endpoint }` |
| `PUT /api/v1/push/prefs` | `{ mutedFolderIds, mutedThreadIds, locale }` |
| `POST /api/v1/push/test` | send test push; `{ sent, removed }` |

- [ ] **Step 1: Add `vapid_subject` to AuthState**

In `backend/src/auth/state.rs`, add to the struct (after `pub captcha: CaptchaConfig,`):

```rust
    /// VAPID contact claim for Web Push.
    pub vapid_subject: String,
```

and in `AuthState::new`, after `captcha: config.captcha.clone(),`:

```rust
            vapid_subject: config.vapid_subject.clone(),
```

Run `cd backend && cargo check 2>&1 | grep -c error` — every `AuthState::new(...)` call site in tests uses a `Config`, so they pick this up automatically; only struct-literal constructions of `AuthState` (if any) need the field. Fix what the compiler flags.

- [ ] **Step 2: Write the failing handler tests**

Append to `mod tests` in `backend/src/push/mod.rs`. The state helper copies the `test_config()` literal from `backend/src/auth/tests.rs` (~line 163) verbatim plus the new `vapid_subject` field:

```rust
    fn push_test_config() -> crate::config::Config {
        crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            max_attachment_bytes: 25 * 1024 * 1024,
            redis_url: None,
            master_key: crate::auth::TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
            captcha: crate::config::CaptchaConfig::None,
            vapid_subject: "mailto:test@example.com".into(),
        }
    }

    async fn push_state(db: crate::storage::DbPool, kv: Arc<dyn KvStore>) -> crate::auth::AuthState {
        install_test_master_key();
        crate::auth::AuthState::new(
            db,
            &push_test_config(),
            Arc::new(crate::kernel::App::new()),
            kv,
        )
        .unwrap()
    }
```

(If `Config` has more fields than listed here, the compiler will name them — copy their values from `test_config()` in `auth/tests.rs`. `AuthState::new` returns `Result<_, anyhow::Error>`.)

```rust
    #[tokio::test]
    async fn vapid_key_endpoint_returns_public_key() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let storage = crate::storage::Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let state = push_state(storage.pool().clone(), kv).await;

        let axum::Json(body) = super::http::get_vapid_key(
            axum::extract::State(state),
            crate::auth::AuthUser("alice".into()),
        )
        .await;
        assert_eq!(body.public_key.len(), 87);
    }

    #[tokio::test]
    async fn subscription_put_and_delete() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let storage = crate::storage::Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let state = push_state(storage.pool().clone(), kv.clone()).await;

        super::http::put_subscription(
            axum::extract::State(state.clone()),
            crate::auth::AuthUser("alice".into()),
            axum::Json(super::http::PutSubscription {
                endpoint: "https://push.example/abc".into(),
                keys: super::store::StoredKeys { p256dh: "p".into(), auth: "a".into() },
            }),
        )
        .await
        .unwrap();
        assert_eq!(super::store::load_subscriptions(&kv, "alice").await.unwrap().len(), 1);

        super::http::delete_subscription(
            axum::extract::State(state),
            crate::auth::AuthUser("alice".into()),
            axum::Json(super::http::DeleteSubscription { endpoint: "https://push.example/abc".into() }),
        )
        .await
        .unwrap();
        assert!(super::store::load_subscriptions(&kv, "alice").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn put_subscription_rejects_non_https_endpoint() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let storage = crate::storage::Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        let state = push_state(storage.pool().clone(), kv).await;

        let err = super::http::put_subscription(
            axum::extract::State(state),
            crate::auth::AuthUser("alice".into()),
            axum::Json(super::http::PutSubscription {
                endpoint: "ftp://evil.example/x".into(),
                keys: super::store::StoredKeys { p256dh: "p".into(), auth: "a".into() },
            }),
        )
        .await
        .unwrap_err();
        // 400
        assert!(matches!(err, super::http::PushHttpError::BadRequest(_)));
    }
```

Note on the https check: real push endpoints are always https; allow `http://127.0.0.1`/`http://localhost` too so dev and the test suite work. `AuthUser` tuple construction: verify against `auth/mod.rs` (`AuthUser("alice".into())` is used in captcha handler tests at `auth/tests.rs:1500`).

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: FAIL — `unresolved import crate::push::http`

- [ ] **Step 4: Implement the handlers**

Create `backend/src/push/http.rs`:

```rust
//! HTTP surface for Web Push: VAPID key, subscription CRUD, mute-pref
//! write-through, and a full-path test push. All routes require the bearer
//! session (`AuthUser`).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthState, AuthUser};
use crate::kv::KvStore;

use super::send::{SendOutcome, send_push};
use super::store::{
    StoredKeys, StoredPrefs, StoredSubscription, load_or_generate_vapid, load_subscriptions,
    remove_subscription, save_prefs, upsert_subscription,
};

pub(crate) fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/push/vapid-key", get(get_vapid_key))
        .route("/api/v1/push/status", get(get_status))
        .route(
            "/api/v1/push/subscription",
            put(put_subscription).delete(delete_subscription),
        )
        .route("/api/v1/push/prefs", put(put_prefs))
        .route("/api/v1/push/test", post(post_test))
}

#[derive(Debug)]
pub(crate) enum PushHttpError {
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for PushHttpError {
    fn into_response(self) -> Response {
        let (status, message, code) = match self {
            PushHttpError::BadRequest(m) => (StatusCode::BAD_REQUEST, m, "bad_request"),
            PushHttpError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m, "internal"),
        };
        (status, Json(serde_json::json!({ "error": message, "code": code }))).into_response()
    }
}

impl From<crate::kv::KvError> for PushHttpError {
    fn from(e: crate::kv::KvError) -> Self {
        PushHttpError::Internal(e.to_string())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VapidKeyResponse {
    pub(crate) public_key: String,
}

pub(crate) async fn get_vapid_key(
    State(state): State<AuthState>,
    AuthUser(_user_id): AuthUser,
) -> Json<VapidKeyResponse> {
    let identity = load_or_generate_vapid(state.kv())
        .await
        .expect("vapid identity");
    Json(VapidKeyResponse {
        public_key: identity.public_key_b64,
    })
}
```

(The `camelCase` rename makes the wire field `publicKey`, which is what `lib/push.ts` reads.)

Continue `http.rs`:

```rust
#[derive(Serialize)]
pub(crate) struct StatusResponse {
    pub(crate) devices: usize,
}

pub(crate) async fn get_status(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<StatusResponse>, PushHttpError> {
    let subs = load_subscriptions(state.kv(), &user_id).await?;
    Ok(Json(StatusResponse { devices: subs.len() }))
}

#[derive(Deserialize)]
pub(crate) struct PutSubscription {
    pub(crate) endpoint: String,
    pub(crate) keys: StoredKeys,
}

fn valid_endpoint(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else { return false };
    if url.scheme() == "https" {
        return true;
    }
    // Local dev / test listeners.
    url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost") | Some("[::1]"))
}

pub(crate) async fn put_subscription(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<PutSubscription>,
) -> Result<StatusCode, PushHttpError> {
    if !valid_endpoint(&req.endpoint) || req.keys.p256dh.is_empty() || req.keys.auth.is_empty() {
        return Err(PushHttpError::BadRequest(
            "endpoint must be an https push URL and keys must be non-empty".into(),
        ));
    }
    let sub = StoredSubscription {
        endpoint: req.endpoint,
        keys: req.keys,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    upsert_subscription(state.kv(), &user_id, sub).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct DeleteSubscription {
    pub(crate) endpoint: String,
}

pub(crate) async fn delete_subscription(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<DeleteSubscription>,
) -> Result<StatusCode, PushHttpError> {
    remove_subscription(state.kv(), &user_id, &req.endpoint).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PutPrefs {
    #[serde(default)]
    pub(crate) muted_folder_ids: Vec<String>,
    #[serde(default)]
    pub(crate) muted_thread_ids: Vec<String>,
    #[serde(default = "default_locale")]
    pub(crate) locale: String,
}

fn default_locale() -> String {
    "en".to_string()
}

pub(crate) async fn put_prefs(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<PutPrefs>,
) -> Result<StatusCode, PushHttpError> {
    let locale = if req.locale == "zh" { "zh" } else { "en" }.to_string();
    save_prefs(
        state.kv(),
        &user_id,
        &StoredPrefs {
            muted_folder_ids: req.muted_folder_ids,
            muted_thread_ids: req.muted_thread_ids,
            locale,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub(crate) struct TestResponse {
    pub(crate) sent: usize,
    pub(crate) removed: usize,
}

pub(crate) async fn post_test(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<TestResponse>, PushHttpError> {
    let kv: &Arc<dyn KvStore> = state.kv();
    let subs = load_subscriptions(kv, &user_id).await?;
    let prefs = super::store::load_prefs(kv, &user_id).await?;
    let payload = if prefs.locale == "zh" {
        serde_json::json!({ "title": "Lyra 通知已启用", "body": "后台推送工作正常。", "tag": "lyra-test", "data": { "messageId": "" } })
    } else {
        serde_json::json!({ "title": "Lyra notifications are on", "body": "Background push is working.", "tag": "lyra-test", "data": { "messageId": "" } })
    };
    let vapid = load_or_generate_vapid(kv).await?;
    let client = reqwest::Client::new();
    let mut sent = 0;
    let mut removed = 0;
    for sub in &subs {
        match send_push(&client, &vapid.private_pem, &state.vapid_subject, sub, &payload.to_string()).await {
            Ok(SendOutcome::Delivered) => sent += 1,
            Ok(SendOutcome::Gone) => {
                removed += 1;
                let _ = remove_subscription(kv, &user_id, &sub.endpoint).await;
            }
            _ => {}
        }
    }
    Ok(Json(TestResponse { sent, removed }))
}
```

- [ ] **Step 5: Register routes**

In `backend/src/push/mod.rs` add `mod http;` and `pub(crate) use http::routes;`. In `backend/src/main.rs` `api_router`, after `.merge(auth::routes())` add:

```rust
        .merge(push::routes())
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd backend && cargo test --bin lyra_backend push::`
Expected: PASS. Then the full suite: `cd backend && cargo test --bin lyra_backend` — 512 + new tests all green.

- [ ] **Step 7: Commit**

```bash
git add backend/src/push/ backend/src/main.rs backend/src/auth/state.rs
git commit -m "feat(push): /api/v1/push endpoints (vapid-key, subscription, prefs, status, test)"
```

---

### Task 8: OpenAPI contract

**Files:**
- Modify: `docs/openapi/api-v1.yaml`

- [ ] **Step 1: Add the push paths**

Read the existing `/settings/captcha` entry in `docs/openapi/api-v1.yaml` and mirror its style. Add paths:

- `/push/vapid-key` — get → 200 `{ publicKey: string }`
- `/push/status` — get → 200 `{ devices: integer }`
- `/push/subscription` — put (body `{ endpoint: string, keys: { p256dh: string, auth: string } }`, 204) + delete (body `{ endpoint: string }`, 204)
- `/push/prefs` — put (body `{ mutedFolderIds: string[], mutedThreadIds: string[], locale: string }`, 204)
- `/push/test` — post → 200 `{ sent: integer, removed: integer }`

All with bearer auth, tagged consistently with neighboring entries.

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('docs/openapi/api-v1.yaml'))"` (or `npx @redocly/cli lint docs/openapi/api-v1.yaml` if available)
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add docs/openapi/api-v1.yaml
git commit -m "docs(openapi): /api/v1/push endpoints"
```

---

### Task 9: Frontend `lib/push.ts`

**Files:**
- Create: `frontend/src/lib/push.ts`
- Create: `frontend/src/lib/push.test.ts`

- [ ] **Step 1: Write the failing tests**

Create `frontend/src/lib/push.test.ts`:

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({
  api: vi.fn(),
}));

import { api } from '@/lib/api-client';
import { pushStatus, subscribePush, syncPushPrefs, unsubscribePush } from './push';

const mockedApi = vi.mocked(api);

function mockRegistration(subscription: PushSubscription | null) {
  const reg = {
    pushManager: {
      getSubscription: vi.fn().mockResolvedValue(subscription),
      subscribe: vi.fn(),
    },
  };
  Object.defineProperty(navigator, 'serviceWorker', {
    value: { getRegistration: vi.fn().mockResolvedValue(reg), ready: Promise.resolve(reg) },
    configurable: true,
  });
  return reg;
}

function fakeSubscription(endpoint = 'https://push.example/abc') {
  return {
    endpoint,
    toJSON: () => ({ endpoint, keys: { p256dh: 'p256dh', auth: 'auth' } }),
    unsubscribe: vi.fn().mockResolvedValue(true),
  } as unknown as PushSubscription;
}

describe('push', () => {
  beforeEach(() => {
    mockedApi.mockResolvedValue({ publicKey: 'BKey' } as never);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    // @ts-expect-error cleanup
    delete navigator.serviceWorker;
  });

  it('reports unsupported without a service worker', async () => {
    // @ts-expect-error absence
    delete navigator.serviceWorker;
    expect(await pushStatus()).toBe('unsupported');
  });

  it('subscribes with the server VAPID key and PUTs the subscription', async () => {
    const sub = fakeSubscription();
    const reg = mockRegistration(null);
    reg.pushManager.subscribe.mockResolvedValue(sub);

    await subscribePush();

    expect(mockedApi).toHaveBeenCalledWith('/push/vapid-key');
    const [keyArg] = reg.pushManager.subscribe.mock.calls[0];
    expect(keyArg.userVisibleOnly).toBe(true);
    expect(keyArg.applicationServerKey).toBeInstanceOf(Uint8Array);
    expect(mockedApi).toHaveBeenCalledWith('/push/subscription', {
      method: 'PUT',
      body: JSON.stringify({
        endpoint: 'https://push.example/abc',
        keys: { p256dh: 'p256dh', auth: 'auth' },
      }),
    });
    expect(await pushStatus()).toBe('subscribed');
  });

  it('unsubscribes locally and on the server', async () => {
    const sub = fakeSubscription();
    mockRegistration(sub);
    await unsubscribePush();
    expect(mockedApi).toHaveBeenCalledWith('/push/subscription', {
      method: 'DELETE',
      body: JSON.stringify({ endpoint: 'https://push.example/abc' }),
    });
    expect(sub.unsubscribe).toHaveBeenCalled();
  });

  it('syncPushPrefs PUTs only when subscribed', async () => {
    mockRegistration(null);
    await syncPushPrefs({ mutedFolderIds: ['f1'], mutedThreadIds: [], locale: 'en' });
    expect(mockedApi).not.toHaveBeenCalledWith('/push/prefs', expect.anything());

    mockRegistration(fakeSubscription());
    await syncPushPrefs({ mutedFolderIds: ['f1'], mutedThreadIds: ['t1'], locale: 'zh' });
    expect(mockedApi).toHaveBeenCalledWith('/push/prefs', {
      method: 'PUT',
      body: JSON.stringify({ mutedFolderIds: ['f1'], mutedThreadIds: ['t1'], locale: 'zh' }),
    });
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/push.test.ts`
Expected: FAIL — module `./push` does not exist

- [ ] **Step 3: Implement `push.ts`**

Create `frontend/src/lib/push.ts`:

```ts
/**
 * Web Push plumbing: subscription lifecycle against /api/v1/push and the
 * server-side copy of the mute prefs. Page-driven banners stay in
 * notifications.ts; this module only manages closed-app push.
 */

import { api } from '@/lib/api-client';

export type PushStatus = 'unsupported' | 'denied' | 'subscribed' | 'unsubscribed';

function urlBase64ToUint8Array(base64: string): Uint8Array {
  const padding = '='.repeat((4 - (base64.length % 4)) % 4);
  const raw = atob((base64 + padding).replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}

async function currentSubscription(): Promise<PushSubscription | null> {
  if (!('serviceWorker' in navigator)) return null;
  const reg = await navigator.serviceWorker.getRegistration();
  return reg?.pushManager.getSubscription() ?? null;
}

export async function pushStatus(): Promise<PushStatus> {
  if (typeof window === 'undefined') return 'unsupported';
  if (!('serviceWorker' in navigator) || !('PushManager' in window)) return 'unsupported';
  if (typeof Notification !== 'undefined' && Notification.permission === 'denied') return 'denied';
  return (await currentSubscription()) ? 'subscribed' : 'unsubscribed';
}

/** Subscribe this browser and register the subscription with the server. */
export async function subscribePush(): Promise<void> {
  const reg = await navigator.serviceWorker.ready;
  const { publicKey } = await api<{ publicKey: string }>('/push/vapid-key');
  const sub = await reg.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: urlBase64ToUint8Array(publicKey) as BufferSource,
  });
  const json = sub.toJSON();
  await api('/push/subscription', {
    method: 'PUT',
    body: JSON.stringify({ endpoint: json.endpoint, keys: json.keys }),
  });
}

/** Remove the server-side subscription, then unsubscribe the browser. */
export async function unsubscribePush(): Promise<void> {
  const sub = await currentSubscription();
  if (!sub) return;
  try {
    await api('/push/subscription', {
      method: 'DELETE',
      body: JSON.stringify({ endpoint: sub.endpoint }),
    });
  } finally {
    await sub.unsubscribe().catch(() => {});
  }
}

export interface PushPrefsPayload {
  mutedFolderIds: string[];
  mutedThreadIds: string[];
  locale: string;
}

/**
 * Write-through of the mute lists so the server can filter before sending.
 * No-op when this browser has no active push subscription. Fire-and-forget:
 * failures are swallowed (the next prefs write or app open retries).
 */
export async function syncPushPrefs(prefs: PushPrefsPayload): Promise<void> {
  if (!(await currentSubscription())) return;
  try {
    await api('/push/prefs', { method: 'PUT', body: JSON.stringify(prefs) });
  } catch {
    // best-effort
  }
}

/**
 * Heal-on-open: when the user has banners enabled, make sure the server's
 * subscription matches this browser's (covers pushservice endpoint rotation
 * and re-installs). Cheap: one local read + one idempotent PUT.
 */
export async function reconcilePushSubscription(enabled: boolean): Promise<void> {
  if (!enabled) return;
  const sub = await currentSubscription();
  if (!sub) return;
  const json = sub.toJSON();
  try {
    await api('/push/subscription', {
      method: 'PUT',
      body: JSON.stringify({ endpoint: json.endpoint, keys: json.keys }),
    });
  } catch {
    // best-effort
  }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/push.test.ts`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/push.ts frontend/src/lib/push.test.ts
git commit -m "feat(push): frontend subscription lifecycle helper"
```

---

### Task 10: Service worker push handling

**Files:**
- Modify: `frontend/public/sw.js`

- [ ] **Step 1: Add the `push` and `pushsubscriptionchange` listeners + bump VERSION**

In `frontend/public/sw.js`:

1. Change `const VERSION = 'lyra-v1';` to `const VERSION = 'lyra-v2';`
2. Update the header comment responsibilities list — add:
   ```
   *  - Receiving Web Push messages (RFC 8030) and showing them as
   *    notifications when no Lyra tab is open to do it.
   ```
3. Append at the end of the file:

```js
self.addEventListener('push', (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = {};
  }
  const title = typeof data.title === 'string' && data.title ? data.title : 'Lyra';
  event.waitUntil(
    self.registration.showNotification(title, {
      body: typeof data.body === 'string' ? data.body : '',
      tag: typeof data.tag === 'string' ? data.tag : undefined,
      icon: '/icons/icon-192.png',
      badge: '/icons/icon-192.png',
      data: data.data && typeof data.data === 'object' ? data.data : {},
    }),
  );
});

// The push service rotated our subscription. Re-subscribe with the same key;
// the server record is healed by the page's reconcilePushSubscription on the
// next app open (the SW cannot reach the auth token in localStorage).
self.addEventListener('pushsubscriptionchange', (event) => {
  event.waitUntil(
    (async () => {
      const options = event.oldSubscription?.options ?? { userVisibleOnly: true };
      await self.registration.pushManager.subscribe(options).catch(() => {});
    })(),
  );
});
```

The existing `notificationclick` handler already routes `data.messageId` — no change needed.

- [ ] **Step 2: Verify the built app ships the new worker**

Run: `cd frontend && npm run build && grep -c "pushsubscriptionchange" dist/sw.js`
Expected: `1`

- [ ] **Step 3: Commit**

```bash
git add frontend/public/sw.js
git commit -m "feat(push): service worker push + subscription-change handlers"
```

---

### Task 11: Settings UI — Background Push section

**Files:**
- Modify: `frontend/src/components/notification-settings.tsx`
- Modify: `frontend/src/lib/notifications.ts` (write-through)
- Modify: `frontend/src/i18n/en.json` + `frontend/src/i18n/zh.json`
- Modify: `frontend/src/main.tsx` (reconcile on boot)
- Modify: `frontend/src/lib/notifications.test.ts` (write-through test)

- [ ] **Step 1: Write-through in `writeNotificationPrefs`**

In `frontend/src/lib/notifications.ts`, import and extend:

```ts
import { syncPushPrefs } from '@/lib/push';
```

In `writeNotificationPrefs`, after `for (const listener of prefsListeners) listener();` add:

```ts
  // Server-side copy for the push fan-out (no-op when push isn't subscribed).
  void syncPushPrefs({
    mutedFolderIds: prefs.mutedFolderIds,
    mutedThreadIds: prefs.mutedThreadIds,
    locale: useUIStore.getState().locale,
  });
```

Add a test to `frontend/src/lib/notifications.test.ts`:

```ts
import { syncPushPrefs } from './push';
vi.mock('./push', () => ({ syncPushPrefs: vi.fn() }));

// inside a describe:
it('writeNotificationPrefs mirrors mutes to the push server', () => {
  writeNotificationPrefs({ enabled: true, mutedFolderIds: ['f1'], mutedThreadIds: ['t1'] });
  expect(syncPushPrefs).toHaveBeenCalledWith({
    mutedFolderIds: ['f1'],
    mutedThreadIds: ['t1'],
    locale: expect.any(String),
  });
});
```

Adjust the existing import block of the test file to include `vi` from vitest if not already imported.

- [ ] **Step 2: Reconcile on boot**

In `frontend/src/main.tsx`, find where the app boots (near the `navigator.serviceWorker?.addEventListener('message', …)` at line 23). Add after it:

```ts
// Heal the server-side push subscription when banners are enabled (covers
// pushservice endpoint rotation; cheap idempotent PUT).
import { reconcilePushSubscription } from '@/lib/push';
import { readNotificationPrefs } from '@/lib/notifications';
void reconcilePushSubscription(readNotificationPrefs().enabled);
```

(Place imports at the top with the others, the call in the boot sequence.)

- [ ] **Step 3: Background Push section in the notifications card**

In `frontend/src/components/notification-settings.tsx`, add imports:

```ts
import { pushStatus, subscribePush, unsubscribePush, syncPushPrefs, type PushStatus } from '@/lib/push';
import { api } from '@/lib/api-client';
```

Inside `NotificationSettings`, add state:

```tsx
  const [push, setPush] = useState<PushStatus>('unsubscribed');
  const [pushBusy, setPushBusy] = useState(false);
  const [pushTestResult, setPushTestResult] = useState<string | null>(null);

  useEffect(() => {
    void pushStatus().then(setPush);
  }, []);

  const handlePushToggle = async (next: boolean) => {
    setPushBusy(true);
    try {
      if (!next) {
        await unsubscribePush();
        setPush('unsubscribed');
        return;
      }
      const granted = await requestNotificationPermission();
      setPermission(granted);
      if (granted !== 'granted') {
        setPush(granted === 'denied' ? 'denied' : 'unsubscribed');
        return;
      }
      await subscribePush();
      const prefs = readNotificationPrefs();
      await syncPushPrefs({
        mutedFolderIds: prefs.mutedFolderIds,
        mutedThreadIds: prefs.mutedThreadIds,
        locale,
      });
      setPush('subscribed');
    } finally {
      setPushBusy(false);
    }
  };

  const handlePushTest = async () => {
    setPushBusy(true);
    setPushTestResult(null);
    try {
      const res = await api<{ sent: number; removed: number }>('/push/test', { method: 'POST' });
      setPushTestResult(
        t('settings.notifications.push.testResult').replace('{{sent}}', String(res.sent)),
      );
    } catch {
      setPushTestResult(t('settings.notifications.push.testFailed'));
    } finally {
      setPushBusy(false);
    }
  };
```

(Add `useEffect` to the React import if missing.) After the existing notifications toggle row (and before the install card), render:

```tsx
      <div className="flex items-start justify-between gap-4 border-t pt-4">
        <div>
          <div className="text-sm font-medium">{t('settings.notifications.push.title')}</div>
          <p className="text-muted-foreground mt-1 text-sm">
            {t('settings.notifications.push.hint')}
          </p>
          {push === 'unsupported' && (
            <p className="text-muted-foreground mt-1 text-sm">
              {t('settings.notifications.push.unsupported')}
            </p>
          )}
          {push === 'denied' && (
            <p className="text-muted-foreground mt-1 text-sm">
              {t('settings.notifications.denied')}
            </p>
          )}
          {ios && !standalone && (
            <p className="text-muted-foreground mt-1 text-sm">
              {t('settings.notifications.push.iosHint')}
            </p>
          )}
          {push === 'subscribed' && (
            <div className="mt-2 flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={handlePushTest} disabled={pushBusy}>
                {t('settings.notifications.push.test')}
              </Button>
              {pushTestResult && (
                <span className="text-muted-foreground text-sm">{pushTestResult}</span>
              )}
            </div>
          )}
        </div>
        <Switch
          checked={push === 'subscribed'}
          onCheckedChange={handlePushToggle}
          disabled={pushBusy || push === 'unsupported' || push === 'denied'}
          aria-label={t('settings.notifications.push.title')}
        />
      </div>
```

- [ ] **Step 4: i18n strings**

In `frontend/src/i18n/en.json`, inside `settings.notifications` (after `"eventTitle"`):

```json
      "push": {
        "title": "Background push",
        "hint": "Get notified even when Lyra is closed. Uses your browser's push service; message content is encrypted end-to-end.",
        "test": "Send test push",
        "testResult": "Push sent to {{sent}} device(s).",
        "testFailed": "Test push failed — check the server logs.",
        "unsupported": "This browser does not support push notifications.",
        "iosHint": "On iPhone/iPad, install Lyra to the Home Screen first to enable push."
      }
```

In `frontend/src/i18n/zh.json`, same location:

```json
      "push": {
        "title": "后台推送",
        "hint": "即使 Lyra 未打开也能收到新邮件通知。经由浏览器推送服务发送，邮件内容端到端加密。",
        "test": "发送测试推送",
        "testResult": "已推送到 {{sent}} 台设备。",
        "testFailed": "测试推送失败，请查看服务端日志。",
        "unsupported": "当前浏览器不支持推送通知。",
        "iosHint": "在 iPhone/iPad 上，请先将 Lyra 添加到主屏幕，然后才能开启推送。"
      }
```

Also update the now-stale `"runningNote"` string in both files (it says push "comes later"): in en.json change to `"In-app banners need Lyra running; background push below covers closed apps."`, in zh.json `"应用内横幅需要 Lyra 处于打开状态；下方后台推送可覆盖应用关闭后的场景。"` — check the exact current strings with grep first and replace in place.

- [ ] **Step 5: Run frontend tests + typecheck**

Run: `cd frontend && npm test && npm run check`
Expected: 225+ tests PASS, oxlint/tsc clean (7 pre-existing warnings only)

- [ ] **Step 6: Commit**

```bash
git add frontend/src/components/notification-settings.tsx frontend/src/lib/notifications.ts frontend/src/lib/notifications.test.ts frontend/src/lib/push.ts frontend/src/main.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat(push): settings background-push section + mute write-through"
```

---

### Task 12: Spec note + AGENTS.md

**Files:**
- Modify: `docs/superpowers/specs/2026-09-07-lyra-web-push-design.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Spec amendment**

In the design doc's Frontend section, the `pushsubscriptionchange` bullet: append "In v1 the SW re-subscribes locally only; the server record is healed by `reconcilePushSubscription` on the next app open (the SW cannot reach the auth token)."

- [ ] **Step 2: AGENTS.md project map**

In `AGENTS.md`, in the backend src listing of the project map, add after the `oauth/` line:

```
      push/                     ← Web Push (RFC 8030/8291/8292): fan-out on sync events, kv subscriptions
```

and in the "Product truth" table, add a row:

```
| Closed-app push (Web Push, VAPID, kv subscriptions) | `docs/superpowers/specs/2026-09-07-lyra-web-push-design.md` |
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-09-07-lyra-web-push-design.md AGENTS.md
git commit -m "docs: push module in project map + spec amendment"
```

---

### Task 13: Full verification + deploy

- [ ] **Step 1: Full local gates**

```bash
make fmt && make lint && make test
```

Expected: format clean, clippy `-D warnings` clean, oxlint only the 7 baseline warnings, backend 512+ and frontend 225+ tests green.

- [ ] **Step 2: Rebuild the local container**

```bash
docker compose build --build-arg HTTP_PROXY=http://host.docker.internal:7897 --build-arg HTTPS_PROXY=http://host.docker.internal:7897 --build-arg ALL_PROXY=socks5://host.docker.internal:7897 lyra && docker compose up -d
```

(Backend change → release build takes ~10 min.)

- [ ] **Step 3: Live smoke test on http://127.0.0.1:3000**

1. Log in, open Settings → General → Notifications, enable Background push.
2. Verify with curl: `curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3000/api/v1/push/status` → `{"devices":1}`.
3. Click "Send test push" → OS banner appears even with the tab in the background.
4. Trigger a sync (wait for poll or restart an account), confirm no duplicate banners when the app is open (tag collapse).

- [ ] **Step 4: Commit, push, deploy**

```bash
git push
ssh vultr 'cd /opt/stacks/lyra && git pull && docker compose build lyra && docker compose up -d'
```

- [ ] **Step 5: Production smoke test on https://onemail.im**

Same as Step 3 against production (HTTPS is required for push services in practice; onemail.im has it).

---

## Self-review notes

- Spec coverage: VAPID identity (T1), kv storage (T1–2), diff mirror (T3), send + failure handling (T4), fan-out (T5), VAPID subject config (T6), all 5 spec endpoints + `/push/status` (T7), OpenAPI (T8), `push.ts` (T9), sw.js push + pushsubscriptionchange (T10), settings card + write-through + iOS hint (T11), spec/AGENTS.md updates (T12), verification + deploy (T13). The one spec deviation: `/push/status` was added (Settings needs the device count) and the SW `pushsubscriptionchange` no longer PUTs (no token access) — both amendments are recorded in Task 12.
- Type consistency: `StoredSubscription`/`StoredKeys`/`StoredPrefs` are the same types used by store, send, http, and fanout; `diff_new_messages` consumes `MessageResponse` directly; frontend `PushPrefsPayload` matches the backend `PutPrefs` camelCase contract.
- Watch-items for the implementer: (a) `crate::kernel::events::AppEvent` vs `crate::kernel::AppEvent` import path — grep before writing; (b) `ImapMessage.date` field type in the test seed — the compiler will say; (c) the config-literal helper in the push handler tests must be copied from the captcha handler tests in `auth/tests.rs`, not invented.
