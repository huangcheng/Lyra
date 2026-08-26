//! Mail-account OAuth — authorization code + refresh for IMAP/SMTP XOAUTH2.
//!
//! Lyra app login stays username/password + TOTP. This module only covers
//! **mail account** credentials (`auth_type = oauth2`).

mod config;
mod exchange;
mod http;
mod microsoft;
mod providers;
mod tokens;
mod yandex;

pub use config::OAuthRegistry;
pub use http::routes;
pub use microsoft::MsOAuthConfig;
pub use tokens::resolve_mail_access_secret;
pub use yandex::YandexOAuthConfig;

/// Shared OAuth redirect URI for all mail providers, derived from [`LYRA_PUBLIC_URL`].
pub fn oauth_callback_url(public_url: &str) -> String {
    format!("{}/api/v1/oauth/callback", public_url.trim_end_matches('/'))
}

/// Percent-encode a URI query component (RFC 3986 unreserved characters only).
///
/// Shared by all providers so allowlists cannot drift: `@` is a reserved
/// gen-delim and must be encoded (e.g. `login_hint=user%40yandex.ru`).
pub(crate) fn urlencode_component(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Shared HTTP client for OAuth token/profile endpoints.
///
/// Bounded so a hung IdP cannot stall sync workers indefinitely (the reqwest
/// default is no timeout at all).
pub(crate) fn oauth_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{oauth_callback_url, urlencode_component};

    #[test]
    fn oauth_callback_url_is_shared() {
        assert_eq!(
            oauth_callback_url("http://localhost:3000"),
            "http://localhost:3000/api/v1/oauth/callback"
        );
        assert_eq!(
            oauth_callback_url("https://mail.example.com/"),
            "https://mail.example.com/api/v1/oauth/callback"
        );
    }

    #[test]
    fn encodes_reserved_characters() {
        assert_eq!(urlencode_component("user@yandex.ru"), "user%40yandex.ru");
        assert_eq!(urlencode_component("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode_component("safe-._~"), "safe-._~");
    }
}
