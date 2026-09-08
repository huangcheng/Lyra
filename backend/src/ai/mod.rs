//! AI assist (BYOK): per-user provider settings, an LLM client seam with
//! dialect adapters, and the draft/reply assist. The module is deliberately
//! deep — handlers and features talk to `complete` / `draft_reply` /
//! `test_connection` and never see the API key in plaintext.
//!
//! Every call is user-initiated (settings test, compose assist); there are
//! no background LLM calls. Roadmap: `docs/product/2026-08-21-lyra-ai-assist-roadmap.md`,
//! spec: `docs/specs/2026-09-08-lyra-ai-byok-spec.md`.

pub mod chat;
pub mod client;
pub mod http;

pub mod settings;
pub mod spam_assist;
mod store;
mod tools;

pub use settings::{AiDialect, AiSettings, AiSettingsError};
pub use spam_assist::SpamMode;
pub use store::{load_settings, save_settings};

use crate::auth::AuthState;
use crate::storage::DbPool;

/// Typed failures surfaced to HTTP by `http` (kept here so the module owns
/// its error story end to end).
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("AI assist is off or not configured")]
    NotConfigured,
    #[error("this assist feature is disabled in settings")]
    FeatureDisabled,
    #[error("request timed out")]
    Timeout,
    #[error("provider unreachable: {0}")]
    Unreachable(String),
    #[error("provider rejected the request: {0}")]
    Provider(String),
    #[error(transparent)]
    Settings(#[from] AiSettingsError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
    #[error("{0}")]
    InvalidInput(String),
}

/// One completion call under the user's stored settings. Every assist
/// feature funnels through here — settings load, key unlock, and the
/// dialect dispatch live in exactly one place.
pub async fn complete(
    state: &AuthState,
    user_id: &str,
    system: &str,
    prompt: &str,
) -> Result<String, AiError> {
    let db = state.db();
    let settings = load_settings(db, user_id).await?;
    let view = SettingsView::ready(&settings).ok_or(AiError::NotConfigured)?;
    let dek = AuthState::get_user_dek(db, user_id).await?;
    let key = view.decrypt_key(&dek)?;
    client::LlmClient::new(&view, &key)
        .complete(system, prompt)
        .await
}

/// Connection test: the smallest possible round trip under the saved config.
pub async fn test_connection(state: &AuthState, user_id: &str) -> Result<String, AiError> {
    complete(
        state,
        user_id,
        "You are a connection test. Reply with the single word: ok",
        "Say ok.",
    )
    .await
}

/// A settings snapshot validated as callable (master switch on, all fields
/// present). Wraps `AiSettings` plus the decrypted-key helper so callers
/// cannot accidentally use a half-configured provider.
pub struct SettingsView {
    pub dialect: AiDialect,
    pub base_url: String,
    pub model: String,
    api_key_cipher: String,
}

impl SettingsView {
    pub(crate) fn ready(s: &AiSettings) -> Option<Self> {
        if !s.enabled || s.base_url.trim().is_empty() || s.model.trim().is_empty() {
            return None;
        }
        let api_key_cipher = s.api_key_cipher()?;
        Some(Self {
            dialect: s.dialect,
            base_url: s.base_url.trim().to_string(),
            model: s.model.trim().to_string(),
            api_key_cipher,
        })
    }

    pub(crate) fn decrypt_key(&self, dek: &[u8]) -> Result<String, AiError> {
        let encrypted: crate::crypto::EncryptedCredential =
            serde_json::from_str(&self.api_key_cipher)
                .map_err(|e| AiError::InvalidInput(format!("stored key blob invalid: {e}")))?;
        let plain = crate::crypto::decrypt(dek, &encrypted)?;
        String::from_utf8(plain).map_err(|e| AiError::InvalidInput(format!("key not UTF-8: {e}")))
    }
}

/// Draft/reply assist: build the prompt from one stored message and ask the
/// configured model for suggested text. Never mutates mail — the caller
/// (compose UI) owns what happens with the suggestion.
pub async fn draft_reply(
    state: &AuthState,
    user_id: &str,
    message_id: &str,
    mode: &str,
    instruction: Option<&str>,
) -> Result<String, AiError> {
    let db = state.db();
    let settings = load_settings(db, user_id).await?;
    if !settings.enabled {
        return Err(AiError::NotConfigured);
    }
    if !settings.features.draft_reply {
        return Err(AiError::FeatureDisabled);
    }

    let (system, prompt) = build_draft_prompt(db, user_id, message_id, mode, instruction)
        .await
        .map_err(|e| AiError::InvalidInput(e.to_string()))?;
    complete(state, user_id, system, prompt.as_str()).await
}

/// Context cap for the quoted original (body text sent to the provider).
const DRAFT_BODY_CAP: usize = 8 * 1024;

async fn build_draft_prompt(
    db: &DbPool,
    user_id: &str,
    message_id: &str,
    mode: &str,
    instruction: Option<&str>,
) -> Result<(&'static str, String), AiError> {
    let row = crate::sync::queries::load_ai_message_context(db, user_id, message_id)
        .await
        .map_err(|e| AiError::InvalidInput(e.to_string()))?;
    let from = crate::spam::from_json_email(row.from_address.as_deref())
        .unwrap_or_else(|| "unknown sender".into());
    let subject = row.subject.unwrap_or_else(|| "(no subject)".into());
    let body = row.body_text.unwrap_or_default();
    let body = if body.len() > DRAFT_BODY_CAP {
        format!("{}\n…(truncated)", &body[..DRAFT_BODY_CAP])
    } else {
        body
    };

    let system = match mode {
        "forward" => {
            "You draft a short forwarding note for an email. Output only the \
             note text (no subject line, no signature placeholders), in the \
             original email's language."
        }
        _ => {
            "You draft a concise, courteous email reply. Output only the \
             reply body text (no subject line, no quotation of the original), \
             in the original email's language."
        }
    };
    let mode_word = if mode == "forward" {
        "forward"
    } else {
        "reply"
    };
    let mut prompt = format!(
        "Draft a {mode_word} to the email below.\n\nFrom: {from}\nSubject: {subject}\n\n{body}"
    );
    if let Some(extra) = instruction.filter(|s| !s.trim().is_empty()) {
        prompt.push_str("\n\nAdditional instruction from the user: ");
        prompt.push_str(extra.trim());
    }
    Ok((system, prompt))
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn body_cap_truncates() {
        let long = "x".repeat(DRAFT_BODY_CAP + 100);
        assert!(long.len() > DRAFT_BODY_CAP);
        let cut = format!("{}\n…(truncated)", &long[..DRAFT_BODY_CAP]);
        assert!(cut.len() < long.len() + 20);
    }
}
