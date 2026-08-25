//! OpenGPG key parsing, fingerprinting, and keygen (rPGP / RFC 9580).
//!
//! Secret material is never unwrapped here — armored passphrase-locked
//! blobs are stored as-is (opengpg-spec key protection).

use pgp::composed::{
    Deserializable, KeyType, SecretKeyParamsBuilder, SignedPublicKey, SignedSecretKey,
    SubkeyParamsBuilder,
};
use pgp::crypto::ecc_curve::ECCCurve;
use pgp::types::{KeyDetails, Password};
use rand::thread_rng;
use thiserror::Error;

/// Keygen algorithm (opengpg-spec: RSA-4096 default; ed25519/cv25519 option).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyAlgorithm {
    #[default]
    Rsa4096,
    Ed25519,
}

impl KeyAlgorithm {
    pub fn parse(s: &str) -> Result<Self, OpengpgError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "rsa4096" | "rsa-4096" | "rsa" => Ok(Self::Rsa4096),
            "ed25519" | "cv25519" | "curve25519" => Ok(Self::Ed25519),
            other => Err(OpengpgError::InvalidInput(format!(
                "unsupported algorithm '{other}' (use rsa4096 or ed25519)"
            ))),
        }
    }
}

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
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("too many unlock attempts")]
    TooManyRequests,
}

/// Parse an armored public or secret key and extract metadata.
pub fn parse_armored_key(armored: &str) -> Result<ParsedKey, OpengpgError> {
    let trimmed = armored.trim();
    if trimmed.is_empty() {
        return Err(OpengpgError::InvalidKey("empty armored key".into()));
    }
    reject_multi_secret_bundle(trimmed)?;

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

fn reject_multi_secret_bundle(armored: &str) -> Result<(), OpengpgError> {
    let lower = armored.to_ascii_uppercase();
    let secrets = lower.matches("BEGIN PGP PRIVATE KEY BLOCK").count()
        + lower.matches("BEGIN PGP SECRET KEY BLOCK").count();
    if secrets > 1 {
        return Err(OpengpgError::InvalidInput(
            "multi-secret key bundles are not supported; import one key at a time".into(),
        ));
    }
    Ok(())
}

/// Generate a new passphrase-locked secret keypair (RSA-4096 or ed25519/cv25519).
pub fn generate_keypair(
    email: &str,
    name: &str,
    passphrase: &str,
    algorithm: KeyAlgorithm,
) -> Result<ParsedKey, OpengpgError> {
    let email = email.trim().to_lowercase();
    if !email.contains('@') {
        return Err(OpengpgError::InvalidInput(
            "email must be an address".into(),
        ));
    }
    if passphrase.is_empty() {
        return Err(OpengpgError::InvalidInput(
            "passphrase is required for key generation".into(),
        ));
    }
    let name = name.trim();
    let uid = if name.is_empty() {
        format!("<{email}>")
    } else {
        format!("{name} <{email}>")
    };

    let mut rng = thread_rng();
    let mut builder = SecretKeyParamsBuilder::default();
    match algorithm {
        KeyAlgorithm::Rsa4096 => {
            builder
                .key_type(KeyType::Rsa(4096))
                .can_certify(true)
                .can_sign(true)
                .subkey(
                    SubkeyParamsBuilder::default()
                        .key_type(KeyType::Rsa(4096))
                        .can_encrypt(true)
                        .passphrase(Some(passphrase.into()))
                        .build()
                        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?,
                );
        }
        KeyAlgorithm::Ed25519 => {
            builder
                .key_type(KeyType::Ed25519Legacy)
                .can_certify(true)
                .can_sign(true)
                .subkey(
                    SubkeyParamsBuilder::default()
                        .key_type(KeyType::ECDH(ECCCurve::Curve25519))
                        .can_encrypt(true)
                        .passphrase(Some(passphrase.into()))
                        .build()
                        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?,
                );
        }
    }
    builder
        .primary_user_id(uid)
        .passphrase(Some(passphrase.into()));

    let params = builder
        .build()
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    let secret = params
        .generate(&mut rng)
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    let pw = Password::from(passphrase);
    let signed = secret
        .sign(&mut rng, &pw)
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    let armor = signed
        .to_armored_string(None.into())
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    parse_armored_key(&armor)
}

/// Public certificate armor from a stored armored secret (or public) key.
pub fn public_armored_from_stored(key_data: &str) -> Result<String, OpengpgError> {
    let trimmed = key_data.trim();
    if let Ok((secret, _)) = SignedSecretKey::from_string(trimmed) {
        return secret
            .signed_public_key()
            .to_armored_string(None.into())
            .map_err(|e| OpengpgError::InvalidKey(e.to_string()));
    }
    // Already public — return as stored.
    let (_public, _) = SignedPublicKey::from_string(trimmed)
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    Ok(trimmed.to_string())
}

/// Verify that `passphrase` unlocks the primary secret key material.
pub fn verify_secret_passphrase(key_data: &str, passphrase: &str) -> Result<(), OpengpgError> {
    let trimmed = key_data.trim();
    let (secret, _) = SignedSecretKey::from_string(trimmed)
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    let pw = Password::from(passphrase);
    match secret.primary_key.unlock(&pw, |_, _| Ok(())) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) | Err(e) => Err(OpengpgError::InvalidInput(format!(
            "passphrase rejected: {e}"
        ))),
    }
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
    use super::{KeyAlgorithm, generate_keypair};

    pub fn gen_test_secret_armor(passphrase: Option<&str>) -> String {
        generate_keypair(
            "test@example.com",
            "Lyra Test",
            passphrase.unwrap_or("test-pass"),
            KeyAlgorithm::Ed25519,
        )
        .expect("gen")
        .key_data
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
    fn reject_multi_secret_bundle() {
        let a = gen_test_secret_armor(Some("a"));
        let b = gen_test_secret_armor(Some("b"));
        let bundled = format!("{a}\n{b}");
        assert!(matches!(
            parse_armored_key(&bundled),
            Err(OpengpgError::InvalidInput(_))
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
        let armor = gen_test_secret_armor(Some("pw"));
        let (secret, _) = SignedSecretKey::from_string(&armor).expect("secret");
        let public_armor = secret
            .signed_public_key()
            .to_armored_string(None.into())
            .expect("pub armor");
        let parsed = parse_armored_key(&public_armor).expect("parse public");
        assert!(!parsed.is_secret);
        assert_eq!(parsed.primary_email, "test@example.com");
        let _ = SignedPublicKey::from_string(&public_armor);

        let via_helper = public_armored_from_stored(&armor).expect("public half");
        assert!(
            via_helper.contains("BEGIN PGP PUBLIC KEY BLOCK"),
            "expected public armor"
        );
    }

    #[test]
    fn verify_passphrase_accepts_and_rejects() {
        let armor = gen_test_secret_armor(Some("correct-horse"));
        verify_secret_passphrase(&armor, "correct-horse").expect("ok");
        assert!(matches!(
            verify_secret_passphrase(&armor, "wrong"),
            Err(OpengpgError::InvalidInput(_))
        ));
    }
}
