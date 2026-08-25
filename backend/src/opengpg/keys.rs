//! OpenGPG key parsing and fingerprinting (rPGP / RFC 9580).
//!
//! Secret material is never unwrapped here — armored passphrase-locked
//! blobs are stored as-is (opengpg-spec key protection).

#![allow(dead_code)] // HTTP surface lands in CHE-61

use pgp::composed::{Deserializable, SignedPublicKey, SignedSecretKey};
use pgp::types::KeyDetails;
use thiserror::Error;

/// Parsed key metadata ready for persistence (no unlocked secret material).
#[derive(Debug, Clone)]
pub struct ParsedKey {
    pub fingerprint: String,
    pub primary_email: String,
    pub emails: Vec<String>,
    pub is_secret: bool,
    pub revoked: bool,
    /// Original armored form (secret keys remain passphrase-locked).
    pub key_data: String,
}

#[derive(Debug, Error)]
pub enum OpengpgError {
    #[error("invalid OpenGPG key: {0}")]
    InvalidKey(String),
    #[error("no user id / email on key")]
    MissingEmail,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not found")]
    NotFound,
}

/// Parse an armored public or secret key and extract metadata.
pub fn parse_armored_key(armored: &str) -> Result<ParsedKey, OpengpgError> {
    let trimmed = armored.trim();
    if trimmed.is_empty() {
        return Err(OpengpgError::InvalidKey("empty armored key".into()));
    }

    // Prefer secret: a secret key armor also contains the public half.
    if let Ok((secret, _)) = SignedSecretKey::from_string(trimmed) {
        return parsed_from_details(
            &secret.fingerprint(),
            &secret.details,
            true,
            trimmed.to_string(),
        );
    }

    let (public, _) = SignedPublicKey::from_string(trimmed)
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    parsed_from_details(
        &public.fingerprint(),
        &public.details,
        false,
        trimmed.to_string(),
    )
}

fn parsed_from_details(
    fingerprint: &pgp::types::Fingerprint,
    details: &pgp::composed::SignedKeyDetails,
    is_secret: bool,
    key_data: String,
) -> Result<ParsedKey, OpengpgError> {
    let fingerprint = fingerprint.to_string().to_uppercase();
    let emails = collect_emails(details);
    let primary_email = emails.first().cloned().ok_or(OpengpgError::MissingEmail)?;
    let revoked = !details.revocation_signatures.is_empty();
    Ok(ParsedKey {
        fingerprint,
        primary_email,
        emails,
        is_secret,
        revoked,
        key_data,
    })
}

fn collect_emails(details: &pgp::composed::SignedKeyDetails) -> Vec<String> {
    let mut out = Vec::new();
    for user in &details.users {
        let uid = String::from_utf8_lossy(user.id.id());
        if let Some(email) = extract_email(&uid) {
            let lower = email.to_lowercase();
            if !out.contains(&lower) {
                out.push(lower);
            }
        }
    }
    out
}

/// Pull `user@host` from a User ID string (`Name <user@host>` or bare email).
pub(crate) fn extract_email(uid: &str) -> Option<String> {
    let uid = uid.trim();
    if let Some(start) = uid.find('<') {
        let end = uid.rfind('>')?;
        if end > start + 1 {
            let email = uid[start + 1..end].trim();
            if email.contains('@') {
                return Some(email.to_string());
            }
        }
    }
    if uid.contains('@') && !uid.contains(' ') {
        return Some(uid.to_string());
    }
    None
}

#[cfg(test)]
pub(crate) mod tests_support {
    use pgp::composed::{KeyType, SecretKeyParamsBuilder};
    use pgp::types::Password;
    use rand::thread_rng;

    pub fn gen_test_secret_armor(passphrase: Option<&str>) -> String {
        let mut rng = thread_rng();
        let mut builder = SecretKeyParamsBuilder::default();
        builder
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id("Lyra Test <test@example.com>".into());
        if let Some(pw) = passphrase {
            builder.passphrase(Some(pw.into()));
        }
        let params = builder.build().expect("params");
        let secret = params.generate(&mut rng).expect("generate");
        let pw = Password::from(passphrase.unwrap_or(""));
        let signed = secret.sign(&mut rng, &pw).expect("sign");
        signed.to_armored_string(None.into()).expect("armor")
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::gen_test_secret_armor;
    use super::*;
    use pgp::composed::{Deserializable, SignedPublicKey, SignedSecretKey};

    #[test]
    fn extract_email_from_uid_forms() {
        assert_eq!(
            extract_email("Ada Lovelace <ada@example.com>").as_deref(),
            Some("ada@example.com")
        );
        assert_eq!(
            extract_email("bob@example.org").as_deref(),
            Some("bob@example.org")
        );
        assert_eq!(extract_email("no-email-here"), None);
    }

    #[test]
    fn reject_empty_armor() {
        assert!(matches!(
            parse_armored_key("  "),
            Err(OpengpgError::InvalidKey(_))
        ));
    }

    #[test]
    fn parse_generated_secret_key_roundtrip_metadata() {
        let armor = gen_test_secret_armor(Some("test-pass"));
        let parsed = parse_armored_key(&armor).expect("parse secret");
        assert!(parsed.is_secret);
        assert_eq!(parsed.primary_email, "test@example.com");
        assert!(parsed.emails.contains(&"test@example.com".to_string()));
        assert_eq!(parsed.fingerprint.len(), 40); // v4 hex
        assert!(
            parsed.key_data.contains("BEGIN PGP PRIVATE KEY BLOCK")
                || parsed.key_data.contains("BEGIN PGP SECRET KEY BLOCK")
        );

        let again = parse_armored_key(&parsed.key_data).expect("reparse");
        assert_eq!(again.fingerprint, parsed.fingerprint);
    }

    #[test]
    fn parse_public_certificate_from_secret() {
        let armor = gen_test_secret_armor(None);
        let (secret, _) = SignedSecretKey::from_string(&armor).expect("secret");
        let public_armor = secret
            .signed_public_key()
            .to_armored_string(None.into())
            .expect("pub armor");
        let parsed = parse_armored_key(&public_armor).expect("parse public");
        assert!(!parsed.is_secret);
        assert_eq!(parsed.primary_email, "test@example.com");
        let _ = SignedPublicKey::from_string(&public_armor);
    }
}
