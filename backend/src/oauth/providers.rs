//! Mail OAuth provider registry (Microsoft, Yandex, …).

use crate::accounts;
use crate::auth::AuthState;

pub const MICROSOFT: &str = "microsoft";
pub const YANDEX: &str = "yandex";

#[derive(Debug, Clone)]
pub struct ProviderDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
}

pub const PROVIDER_CATALOG: &[ProviderDefinition] = &[
    ProviderDefinition {
        id: MICROSOFT,
        display_name: "Microsoft",
    },
    ProviderDefinition {
        id: YANDEX,
        display_name: "Yandex",
    },
];

#[derive(Debug, Clone)]
pub struct MailServerDefaults {
    pub imap_host: &'static str,
    pub imap_port: i32,
    pub smtp_host: &'static str,
    pub smtp_port: i32,
    pub smtp_security: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub configured: bool,
}

/// Resolve provider id from mailbox email domain.
pub fn resolve_provider(email: &str) -> Option<&'static str> {
    let email = email.trim();
    if !email.contains('@') {
        return None;
    }
    let domain = email.split('@').nth(1)?.trim();
    if accounts::is_microsoft_mail_domain(domain) {
        Some(MICROSOFT)
    } else if accounts::is_yandex_mail_domain(domain) {
        Some(YANDEX)
    } else {
        None
    }
}

pub fn is_configured(state: &AuthState, provider: &str) -> bool {
    match provider {
        MICROSOFT => state.ms_oauth.is_some(),
        YANDEX => state.yandex_oauth.is_some(),
        _ => false,
    }
}

pub fn list_providers(state: &AuthState) -> Vec<ProviderInfo> {
    PROVIDER_CATALOG
        .iter()
        .map(|def| ProviderInfo {
            id: def.id.into(),
            display_name: def.display_name.into(),
            configured: is_configured(state, def.id),
        })
        .collect()
}

pub fn mail_servers(provider: &str) -> Option<MailServerDefaults> {
    match provider {
        MICROSOFT => Some(MailServerDefaults {
            imap_host: "outlook.office365.com",
            imap_port: 993,
            smtp_host: "smtp-mail.outlook.com",
            smtp_port: 587,
            smtp_security: "starttls",
        }),
        YANDEX => Some(MailServerDefaults {
            imap_host: "imap.yandex.com",
            imap_port: 993,
            smtp_host: "smtp.yandex.com",
            smtp_port: 465,
            smtp_security: "tls",
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_microsoft_from_live_in() {
        assert_eq!(resolve_provider("user@live.in"), Some(MICROSOFT));
    }

    #[test]
    fn resolve_yandex_from_yandex_ru() {
        assert_eq!(resolve_provider("user@yandex.ru"), Some(YANDEX));
    }

    #[test]
    fn reject_non_oauth_domain() {
        assert_eq!(resolve_provider("user@fastmail.com"), None);
    }
}
