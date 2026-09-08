//! AI settings types (the stored row minus encryption plumbing).

use serde::{Deserialize, Serialize};

/// Provider wire dialect — decides request/response shaping at the
/// [`super::client`] seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiDialect {
    /// OpenAI Chat Completions (and any compatible endpoint: DashScope
    /// compatible-mode, Ollama `/v1`, vLLM, …).
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    /// OpenAI Responses API.
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    /// Anthropic Messages API.
    #[serde(rename = "anthropic")]
    Anthropic,
}

impl AiDialect {
    pub const ALL: &'static [Self] = &[Self::OpenAiChat, Self::OpenAiResponses, Self::Anthropic];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat",
            Self::OpenAiResponses => "openai_responses",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|d| d.as_str() == raw)
    }

    /// Endpoint path appended to the configured base URL.
    pub fn path(self) -> &'static str {
        match self {
            Self::OpenAiChat => "/chat/completions",
            Self::OpenAiResponses => "/responses",
            Self::Anthropic => "/v1/messages",
        }
    }
}

/// Per-feature flags — every feature defaults **off**; the master switch
/// (`AiSettings.enabled`) is the additional gate in front of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AiFeatures {
    /// P2: compose reply/forward suggestion.
    pub draft_reply: bool,
}

/// The loaded settings row (api key stays the encrypted blob here; only
/// [`super::SettingsView`] unlocks it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSettings {
    pub enabled: bool,
    pub dialect: AiDialect,
    pub base_url: String,
    pub model: String,
    /// DEK-encrypted JSON blob, or empty when unset.
    api_key_blob: String,
    pub features: AiFeatures,
}

impl AiSettings {
    pub fn empty() -> Self {
        Self {
            enabled: false,
            dialect: AiDialect::OpenAiChat,
            base_url: String::new(),
            model: String::new(),
            api_key_blob: String::new(),
            features: AiFeatures::default(),
        }
    }

    pub fn with_key_blob(mut self, blob: String) -> Self {
        self.api_key_blob = blob;
        self
    }

    /// Build from stored column values (store layer only).
    pub(crate) fn from_columns(
        enabled: bool,
        dialect: AiDialect,
        base_url: String,
        model: String,
        api_key_blob: String,
        features: AiFeatures,
    ) -> Self {
        Self {
            enabled,
            dialect,
            base_url,
            model,
            api_key_blob,
            features,
        }
    }

    pub fn has_key(&self) -> bool {
        !self.api_key_blob.is_empty()
    }

    /// The stored ciphertext blob (already-encrypted form; never plaintext).
    pub(crate) fn key_blob(&self) -> &str {
        &self.api_key_blob
    }

    /// Ciphertext for decryption — `None` when no key is stored.
    pub(crate) fn api_key_cipher(&self) -> Option<String> {
        if self.api_key_blob.is_empty() {
            None
        } else {
            Some(self.api_key_blob.clone())
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AiSettingsError {
    #[error("invalid AI settings: {0}")]
    Invalid(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_settings_are_not_ready_and_hide_the_key() {
        let s = AiSettings::empty();
        assert!(!s.has_key());
        assert!(!s.enabled);
    }

    #[test]
    fn dialect_round_trips() {
        for d in AiDialect::ALL {
            assert_eq!(AiDialect::parse(d.as_str()), Some(*d));
        }
        assert_eq!(AiDialect::parse("nope"), None);
    }
}
