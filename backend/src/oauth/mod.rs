//! Mail-account OAuth (Microsoft Outlook / M365) — authorization code + refresh.
//!
//! Lyra app login stays username/password + TOTP. This module only covers
//! **mail account** credentials (`auth_type = oauth2`).

mod http;
mod microsoft;
mod tokens;

pub use http::routes;
pub use microsoft::MsOAuthConfig;
pub use tokens::resolve_mail_access_secret;
