//! Sign & encrypt outbound mail (OpenGPG spec P3, RFC 3156).

use std::collections::HashMap;
use std::io::Cursor;

use pgp::composed::{
    ArmorOptions, Deserializable, MessageBuilder, SignedPublicKey, SignedSecretKey,
    StandaloneSignature,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::packet::{SignatureConfig, SignatureType};
use pgp::types::{CompressionAlgorithm, Password};
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::keys::{OpengpgError, extract_email, public_armored_from_stored};
use super::session::UnlockRing;
use super::store::{StoredKey, list_keys};
use crate::auth::AuthState;

/// Compose-time OpenGPG options (independent sign / encrypt toggles).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpengpgSendOptions {
    #[serde(default)]
    pub sign: bool,
    #[serde(default)]
    pub encrypt: bool,
    #[serde(default)]
    pub attach_public_key: bool,
    #[serde(default)]
    pub signing_key_id: Option<String>,
    /// When multiple keys match a recipient email, pick by key id.
    #[serde(default)]
    pub recipient_key_ids: HashMap<String, String>,
}

/// MIME body replacement for SMTP (RFC 3156 wrapper).
#[derive(Debug, Clone)]
pub struct OpengpgMimeBody {
    pub content_type: String,
    pub body: String,
}

/// Recipient key lookup for compose UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientKeyLookup {
    pub email: String,
    pub keys: Vec<RecipientKeyMatch>,
    pub ambiguous: bool,
    pub selected_key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientKeyMatch {
    pub id: String,
    pub fingerprint: String,
    pub primary_email: String,
}

/// Resolve public keys for outbound encryption.
pub async fn lookup_recipient_keys(
    db: &crate::storage::DbPool,
    user_id: &str,
    emails: &[String],
) -> Result<Vec<RecipientKeyLookup>, OpengpgError> {
    let keys = list_keys(db, user_id).await?;
    let mut out = Vec::new();
    for email in emails {
        let lower = email.trim().to_lowercase();
        if !lower.contains('@') {
            continue;
        }
        let matches: Vec<RecipientKeyMatch> = keys
            .iter()
            .filter(|k| !k.revoked && k.emails.iter().any(|e| e.eq_ignore_ascii_case(&lower)))
            .map(|k| RecipientKeyMatch {
                id: k.id.clone(),
                fingerprint: k.fingerprint.clone(),
                primary_email: k.primary_email.clone(),
            })
            .collect();
        let ambiguous = matches.len() > 1;
        let selected_key_id = if matches.len() == 1 {
            Some(matches[0].id.clone())
        } else {
            None
        };
        out.push(RecipientKeyLookup {
            email: lower,
            keys: matches,
            ambiguous,
            selected_key_id,
        });
    }
    Ok(out)
}

/// Wrap plain/html bodies in OpenPGP/MIME when requested.
pub async fn wrap_outbound_opengpg(
    state: &AuthState,
    user_id: &str,
    session_token: &str,
    opts: &OpengpgSendOptions,
    body_text: Option<&str>,
    body_html: Option<&str>,
    recipient_emails: &[String],
) -> Result<Option<OpengpgMimeBody>, OpengpgError> {
    if !opts.sign && !opts.encrypt && !opts.attach_public_key {
        return Ok(None);
    }

    let keys = list_keys(&state.db, user_id).await?;
    let inner = build_inner_body(body_text, body_html);
    let mut payload = inner;

    if opts.sign {
        let (secret, pw) =
            resolve_signing_secret(&keys, &state.opengpg_unlock, session_token, opts)?;
        payload = wrap_signed(&payload, &secret, &pw)?;
    }

    if opts.encrypt {
        let pub_keys =
            resolve_recipient_public_keys(&keys, recipient_emails, &opts.recipient_key_ids)?;
        payload = wrap_encrypted(&payload, &pub_keys)?;
    }

    if opts.attach_public_key {
        let pub_armor = signing_public_armor(&keys, opts)?;
        payload = attach_public_key_part(&payload, &pub_armor);
    }

    Ok(Some(payload))
}

fn build_inner_body(body_text: Option<&str>, body_html: Option<&str>) -> OpengpgMimeBody {
    let text = body_text
        .unwrap_or("")
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    match body_html.filter(|h| !h.is_empty()) {
        Some(html) => {
            let html = html.replace("\r\n", "\n").replace('\n', "\r\n");
            let boundary = mime_boundary();
            let body = format!(
                "This is a multi-part message in MIME format.\r\n\r\n\
--{boundary}\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\n\
{text}\r\n\
--{boundary}\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\n\
{html}\r\n\
--{boundary}--\r\n"
            );
            OpengpgMimeBody {
                content_type: format!("multipart/alternative; boundary=\"{boundary}\""),
                body,
            }
        }
        None => OpengpgMimeBody {
            content_type: "text/plain; charset=utf-8".into(),
            body: text,
        },
    }
}

fn wrap_signed(
    payload: &OpengpgMimeBody,
    secret_armor: &str,
    pw: &str,
) -> Result<OpengpgMimeBody, OpengpgError> {
    let (secret, _) = SignedSecretKey::from_string(secret_armor.trim())
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    let signing_key = &*secret;
    let password = Password::from(pw);

    let mut rng = thread_rng();
    let mut config = SignatureConfig::from_key(&mut rng, signing_key, SignatureType::Binary)
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    config.hash_alg = HashAlgorithm::Sha256;

    let sig = config
        .sign(signing_key, &password, Cursor::new(payload.body.as_bytes()))
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    let armored = StandaloneSignature::new(sig)
        .to_armored_string(ArmorOptions::default())
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;

    let boundary = mime_boundary();
    let body = format!(
        "This is a multi-part message in MIME format.\r\n\r\n\
--{boundary}\r\n\
Content-Type: {inner_ct}\r\n\r\n\
{inner_body}\
--{boundary}\r\n\
Content-Type: application/pgp-signature\r\n\r\n\
{armored}\r\n\
--{boundary}--\r\n",
        inner_ct = payload.content_type,
        inner_body = payload.body,
    );
    Ok(OpengpgMimeBody {
        content_type: format!(
            "multipart/signed; protocol=\"application/pgp-signature\"; micalg=pgp-sha256; boundary=\"{boundary}\""
        ),
        body,
    })
}

fn wrap_encrypted(
    payload: &OpengpgMimeBody,
    recipients: &[SignedPublicKey],
) -> Result<OpengpgMimeBody, OpengpgError> {
    if recipients.is_empty() {
        return Err(OpengpgError::InvalidInput(
            "encrypt requires at least one recipient public key".into(),
        ));
    }
    let mut rng = thread_rng();
    let mut builder = MessageBuilder::from_bytes("", payload.body.as_bytes().to_vec());
    builder.compression(CompressionAlgorithm::ZLIB);
    let mut builder = builder.seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
    for pk in recipients {
        let enc = pk.public_subkeys.first().ok_or_else(|| {
            OpengpgError::InvalidInput("no encryption subkey for recipient public key".into())
        })?;
        builder
            .encrypt_to_key(&mut rng, &enc.key)
            .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    }
    let encrypted = builder
        .to_armored_string(&mut rng, ArmorOptions::default())
        .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;

    let boundary = mime_boundary();
    let body = format!(
        "This is a multi-part message in MIME format.\r\n\r\n\
--{boundary}\r\n\
Content-Type: application/pgp-encrypted\r\n\r\n\
Version: 1\r\n\r\n\
--{boundary}\r\n\
Content-Type: application/octet-stream\r\n\r\n\
{encrypted}\r\n\
--{boundary}--\r\n"
    );
    Ok(OpengpgMimeBody {
        content_type: format!(
            "multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"{boundary}\""
        ),
        body,
    })
}

fn attach_public_key_part(payload: &OpengpgMimeBody, pub_armor: &str) -> OpengpgMimeBody {
    let boundary = mime_boundary();
    let body = format!(
        "This is a multi-part message in MIME format.\r\n\r\n\
--{boundary}\r\n\
Content-Type: {inner_ct}\r\n\r\n\
{inner_body}\
--{boundary}\r\n\
Content-Type: application/pgp-keys; name=\"public-key.asc\"\r\n\
Content-Disposition: attachment; filename=\"public-key.asc\"\r\n\r\n\
{pub_armor}\r\n\
--{boundary}--\r\n",
        inner_ct = payload.content_type,
        inner_body = payload.body,
    );
    OpengpgMimeBody {
        content_type: format!("multipart/mixed; boundary=\"{boundary}\""),
        body,
    }
}

fn resolve_signing_secret(
    keys: &[StoredKey],
    ring: &UnlockRing,
    session_token: &str,
    opts: &OpengpgSendOptions,
) -> Result<(String, Zeroizing<String>), OpengpgError> {
    let key = if let Some(id) = &opts.signing_key_id {
        keys.iter()
            .find(|k| k.id == *id && k.is_secret)
            .ok_or_else(|| OpengpgError::NotFound)?
    } else {
        keys.iter()
            .find(|k| k.is_secret && k.is_primary)
            .ok_or(OpengpgError::InvalidInput(
                "no primary secret key; import or generate one in Settings → Encryption".into(),
            ))?
    };
    let pw = ring.get(session_token, &key.id).ok_or_else(|| {
        OpengpgError::InvalidInput(
            "signing key is locked; unlock in Settings or reading pane".into(),
        )
    })?;
    Ok((key.key_data.clone(), pw))
}

fn signing_public_armor(
    keys: &[StoredKey],
    opts: &OpengpgSendOptions,
) -> Result<String, OpengpgError> {
    let key = if let Some(id) = &opts.signing_key_id {
        keys.iter().find(|k| k.id == *id)
    } else {
        keys.iter().find(|k| k.is_primary)
    }
    .ok_or(OpengpgError::NotFound)?;
    public_armored_from_stored(&key.key_data)
}

fn resolve_recipient_public_keys(
    keys: &[StoredKey],
    recipient_emails: &[String],
    picks: &HashMap<String, String>,
) -> Result<Vec<SignedPublicKey>, OpengpgError> {
    let mut out = Vec::new();
    for email in recipient_emails {
        let lower = email.trim().to_lowercase();
        if !lower.contains('@') {
            continue;
        }
        let matches: Vec<&StoredKey> = keys
            .iter()
            .filter(|k| !k.revoked && k.emails.iter().any(|e| e.eq_ignore_ascii_case(&lower)))
            .collect();
        let chosen = match matches.len() {
            0 => {
                return Err(OpengpgError::InvalidInput(format!(
                    "no public key for recipient {lower}; import their key first"
                )));
            }
            1 => matches[0],
            _ => {
                let pick = picks.get(&lower).ok_or_else(|| {
                    OpengpgError::InvalidInput(format!(
                        "ambiguous keys for {lower}; specify recipientKeyIds"
                    ))
                })?;
                matches
                    .iter()
                    .find(|k| k.id == *pick)
                    .copied()
                    .ok_or_else(|| {
                        OpengpgError::InvalidInput(format!("unknown key id for {lower}"))
                    })?
            }
        };
        let armor = public_armored_from_stored(&chosen.key_data)?;
        let (pk, _) = SignedPublicKey::from_string(&armor)
            .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
        out.push(pk);
    }
    Ok(out)
}

fn mime_boundary() -> String {
    format!(
        "_lyra_{}",
        uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).simple()
    )
}

/// Collect unique recipient emails from address lists.
pub fn collect_recipient_emails(values: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    for v in values {
        let email = if let Some(s) = v.as_str() {
            extract_email(s).or_else(|| Some(s.to_string()))
        } else {
            v.get("email").and_then(|e| e.as_str()).map(str::to_string)
        };
        if let Some(e) = email {
            let lower = e.to_lowercase();
            if lower.contains('@') && !out.contains(&lower) {
                out.push(lower);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opengpg::keys::tests_support::gen_test_secret_armor;
    use crate::opengpg::keys::{
        KeyAlgorithm, generate_keypair, parse_armored_key, public_armored_from_stored,
    };

    #[test]
    fn sign_only_produces_multipart_signed() {
        let armor = gen_test_secret_armor(Some("sign-pass"));
        let inner = build_inner_body(Some("hello signed"), None);
        let wrapped = wrap_signed(&inner, &armor, "sign-pass").expect("signed");
        assert!(wrapped.content_type.contains("multipart/signed"));
        assert!(wrapped.body.contains("BEGIN PGP SIGNATURE"));
        assert!(wrapped.body.contains("hello signed"));
    }

    #[test]
    fn lookup_recipient_finds_imported_public() {
        let pub_armor =
            generate_keypair("peer@example.com", "Peer", "pw", KeyAlgorithm::Ed25519).unwrap();
        let pub_only = public_armored_from_stored(&pub_armor.key_data).unwrap();
        let parsed = parse_armored_key(&pub_only).unwrap();
        assert_eq!(parsed.primary_email, "peer@example.com");
    }
}
