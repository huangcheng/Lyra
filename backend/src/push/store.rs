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
    if let Some(raw) = kv.get(VAPID_KEY).await?
        && let Ok(stored) = serde_json::from_str::<StoredVapid>(&raw)
    {
        let pem_bytes = crate::crypto::decrypt(
            &vapid_key().map_err(|e| KvError::Internal(e.to_string()))?,
            &stored.private_pem_encrypted,
        )
        .map_err(|e| KvError::Internal(e.to_string()))?;
        let private_pem =
            String::from_utf8(pem_bytes).map_err(|e| KvError::Internal(e.to_string()))?;
        return Ok(VapidIdentity {
            private_pem,
            public_key_b64: stored.public_key_b64,
        });
    }
    // Missing or corrupted blob: fall through and (re)generate.

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
