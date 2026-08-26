//! Shared OAuth token exchange result.

#[derive(Debug)]
pub struct ExchangedTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,
    pub scope: Option<String>,
    pub email: Option<String>,
}
