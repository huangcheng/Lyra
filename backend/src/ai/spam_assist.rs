//! AI spam assist (roadmap P4, suggest + auto modes).
//!
//! Suggest: on-demand verdict for one message, shown in the reader.
//! Auto: the post-sync pass asks the model about unjudged inbox mail
//! and files spam through the same move seam as the heuristic engine.
//! Auto-delete and Report are deferred (destructive/best-effort SMTP).

use serde_json::Value;

use super::{AiError, SettingsView, load_settings};
use crate::auth::AuthState;

/// Per-user spam-assist mode, persisted in `ai_settings.spam_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SpamMode {
    #[default]
    Off,
    /// On-demand: the reader asks for one verdict at a time.
    Suggest,
    /// The post-sync pass files AI-spam automatically (audited verdict).
    Auto,
}

impl SpamMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SpamMode::Off => "off",
            SpamMode::Suggest => "suggest",
            SpamMode::Auto => "auto",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "off" => Some(SpamMode::Off),
            "suggest" => Some(SpamMode::Suggest),
            "auto" => Some(SpamMode::Auto),
            _ => None,
        }
    }
}

/// The verdict the model returns for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpamVerdict {
    pub is_spam: bool,
    /// 0-100 self-reported confidence.
    pub confidence: u8,
    pub reason: String,
}

/// Parse the model's verdict reply. The prompt asks for strict JSON, but
/// models fence or chatter — extract the first JSON object leniently.
pub fn parse_spam_verdict(reply: &str) -> Option<SpamVerdict> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let v: Value = serde_json::from_str(&reply[start..=end]).ok()?;
    let is_spam = match v.get("is_spam") {
        Some(Value::Bool(b)) => *b,
        _ => return None,
    };
    let confidence = v
        .get("confidence")
        .and_then(Value::as_i64)
        .unwrap_or(50)
        .clamp(0, 100)
        .try_into()
        .unwrap_or(50);
    let reason = v
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(SpamVerdict {
        is_spam,
        confidence,
        reason,
    })
}

/// Envelope slice sent to the model — subject/sender plus a body head,
/// enough to judge without shipping whole bodies.
pub(crate) fn spam_prompt(subject: &str, from: &str, body_head: &str) -> String {
    format!(
        "Classify this email as spam or not.\n\nFrom: {from}\nSubject: {subject}\n\nBody (start):\n{body_head}\n\n\
         Reply with ONLY a JSON object: {{\"is_spam\": true|false, \"confidence\": 0-100, \"reason\": \"short, in the email's language\"}}\n\
         Marketing you opted into is NOT spam; transactional mail is NOT spam."
    )
}

/// Suggest mode: one on-demand verdict. Never files anything.
pub async fn suggest(
    state: &AuthState,
    user_id: &str,
    message_id: &str,
) -> Result<SpamVerdict, AiError> {
    let db = state.db();
    let settings = load_settings(db, user_id).await?;
    if !settings.enabled || settings.spam_mode != SpamMode::Suggest {
        return Err(AiError::FeatureDisabled);
    }
    let view = SettingsView::ready(&settings).ok_or(AiError::NotConfigured)?;
    let dek = crate::auth::AuthState::get_user_dek(db, user_id).await?;
    let key = view.decrypt_key(&dek)?;
    let row = crate::sync::queries::load_ai_message_context(db, user_id, message_id)
        .await
        .map_err(|e| AiError::InvalidInput(e.to_string()))?;
    let from = crate::spam::from_json_email(row.from_address.as_deref())
        .unwrap_or_else(|| "unknown".into());
    let body_head: String = row
        .body_text
        .unwrap_or_default()
        .chars()
        .take(600)
        .collect();
    let reply = super::client::LlmClient::new(&view, &key)
        .complete(
            "You are a precise spam classifier. Output only the JSON object.",
            &spam_prompt(row.subject.as_deref().unwrap_or(""), &from, &body_head),
        )
        .await?;
    parse_spam_verdict(&reply)
        .ok_or_else(|| AiError::Provider("model did not return a valid verdict".into()))
}

/// Auto mode: judge one envelope during the post-sync pass. Returns the
/// verdict to stamp ('ai_spam'/'ai_clean' is decided by the caller).
pub(crate) async fn auto_judge(
    view: &SettingsView,
    key: &str,
    subject: &str,
    from: &str,
    body_head: &str,
) -> Result<Option<SpamVerdict>, AiError> {
    let reply = super::client::LlmClient::new(view, key)
        .complete(
            "You are a precise spam classifier. Output only the JSON object.",
            &spam_prompt(subject, from, body_head),
        )
        .await?;
    Ok(parse_spam_verdict(&reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let v = parse_spam_verdict(r#"{"is_spam": true, "confidence": 90, "reason": "垃圾广告"}"#);
        assert_eq!(
            v,
            Some(SpamVerdict {
                is_spam: true,
                confidence: 90,
                reason: "垃圾广告".into()
            })
        );
    }

    #[test]
    fn parses_fenced_or_chatty_replies() {
        let v = parse_spam_verdict(
            "Sure! Here is my assessment:\n```json\n{\"is_spam\": false, \"confidence\": 75, \"reason\": \"receipt\"}\n```",
        );
        assert!(v.is_some_and(|v| !v.is_spam && v.confidence == 75));
    }

    #[test]
    fn rejects_non_json_and_missing_flag() {
        assert!(parse_spam_verdict("I think it's spam").is_none());
        assert!(parse_spam_verdict(r#"{"confidence": 50}"#).is_none());
    }

    #[test]
    fn confidence_is_clamped_and_reason_optional() {
        let v = parse_spam_verdict(r#"{"is_spam": true, "confidence": 500}"#).unwrap();
        assert_eq!(v.confidence, 100);
        assert_eq!(v.reason, "");
    }

    #[test]
    fn mode_round_trips() {
        for m in [SpamMode::Off, SpamMode::Suggest, SpamMode::Auto] {
            assert_eq!(SpamMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(SpamMode::parse("auto_delete"), None);
    }
}
