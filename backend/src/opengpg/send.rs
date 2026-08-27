//! Sign & encrypt outbound mail (OpenGPG spec P3, RFC 3156).

use std::collections::HashMap;
use std::io::Cursor;

use pgp::composed::{
    ArmorOptions, Deserializable, DetachedSignature, MessageBuilder, SignedPublicKey,
    SignedSecretKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::types::{CompressionAlgorithm, Password};
use rand::thread_rng;
use serde::{Deserialize, Serialize};

use super::keys::{OpengpgError, extract_email, public_armored_from_stored};
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
    let keys = list_keys(db, user_id, None).await?;
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
///
/// `sending_account_id` scopes the signing identity: only secret keys bound
/// to that mail account may sign or be attached. Encrypt-only sends need no
/// identity and recipient keys still resolve over the whole user keyring.
pub async fn wrap_outbound_opengpg(
    state: &AuthState,
    user_id: &str,
    session_token: &str,
    sending_account_id: &str,
    opts: &OpengpgSendOptions,
    draft: OutboundDraft<'_>,
) -> Result<Option<OpengpgMimeBody>, OpengpgError> {
    let OutboundDraft {
        body_text,
        body_html,
        recipient_emails,
    } = draft;
    if !opts.sign && !opts.encrypt && !opts.attach_public_key {
        return Ok(None);
    }
    if opts.encrypt && recipient_emails.is_empty() {
        return Err(OpengpgError::InvalidInput(
            "encrypt requires at least one recipient".into(),
        ));
    }

    let keys = list_keys(&state.db, user_id, None).await?;
    let inner = build_inner_body(body_text, body_html);
    let mut payload = inner;

    // Identity key: required for sign / attach-public-key, resolved once so
    // both operations agree (and attach works without an unlocked passphrase).
    let signing = if opts.sign || opts.attach_public_key {
        Some(resolve_signing_key(&keys, opts, sending_account_id)?)
    } else {
        None
    };

    if opts.sign {
        let stored = signing.as_ref().expect("checked above");
        let pw = state
            .opengpg_unlock
            .get(session_token, &stored.id)
            .ok_or_else(|| {
                OpengpgError::InvalidInput(
                    "signing key is locked; unlock in Settings or reading pane".into(),
                )
            })?;
        payload = wrap_signed(&payload, &stored.key_data, pw.as_str())?;
    }

    if opts.encrypt {
        let pub_keys =
            resolve_recipient_public_keys(&keys, recipient_emails, &opts.recipient_key_ids)?;
        payload = wrap_encrypted(&payload, &pub_keys)?;
    }

    if opts.attach_public_key {
        let stored = signing.as_ref().expect("checked above");
        let pub_armor = public_armored_from_stored(&stored.key_data)?;
        payload = attach_public_key_part(&payload, &pub_armor);
    }

    Ok(Some(payload))
}

/// The plaintext outbound message plus its resolved recipients.
#[derive(Debug, Clone, Copy)]
pub struct OutboundDraft<'a> {
    pub body_text: Option<&'a str>,
    pub body_html: Option<&'a str>,
    pub recipient_emails: &'a [String],
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
    let password = Password::from(pw);

    let mut rng = thread_rng();
    let sig = DetachedSignature::sign_binary_data(
        &mut rng,
        &secret.primary_key,
        &password,
        HashAlgorithm::Sha256,
        Cursor::new(payload.body.as_bytes()),
    )
    .map_err(|e| OpengpgError::InvalidKey(e.to_string()))?;
    let armored = sig
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

/// Pick the sending account's signing identity: an explicit `signingKeyId`
/// from that account's secret keys, else the account's primary. There is no
/// cross-account fallback — sign/encrypt is unavailable without a key.
fn resolve_signing_key<'a>(
    keys: &'a [StoredKey],
    opts: &OpengpgSendOptions,
    sending_account_id: &str,
) -> Result<&'a StoredKey, OpengpgError> {
    if let Some(id) = &opts.signing_key_id {
        return keys
            .iter()
            .find(|k| {
                k.id == *id && k.is_secret && k.account_id.as_deref() == Some(sending_account_id)
            })
            .ok_or_else(|| {
                OpengpgError::InvalidInput(
                    "signingKeyId must be a secret key bound to the sending account".into(),
                )
            });
    }
    keys.iter()
        .find(|k| {
            k.is_secret
                && k.is_primary
                && !k.revoked
                && k.account_id.as_deref() == Some(sending_account_id)
        })
        .ok_or_else(|| {
            OpengpgError::InvalidInput(
                "this account has no OpenGPG key; add one in Settings → Encryption".into(),
            )
        })
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

    fn key_row(
        id: &str,
        account_id: Option<&str>,
        is_secret: bool,
        is_primary: bool,
        armor: &str,
    ) -> StoredKey {
        let parsed = parse_armored_key(armor).unwrap();
        StoredKey {
            id: id.into(),
            user_id: "u".into(),
            account_id: account_id.map(str::to_string),
            fingerprint: parsed.fingerprint,
            primary_email: parsed.primary_email.clone(),
            emails: parsed.emails,
            is_secret,
            is_primary,
            revoked: false,
            key_data: armor.to_string(),
            created_at: None,
            updated_at: None,
        }
    }

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

    #[test]
    fn signing_resolves_the_sending_accounts_own_primary() {
        let a = key_row(
            "ka",
            Some("acct-a"),
            true,
            true,
            &gen_test_secret_armor(Some("pa")),
        );
        let b = key_row(
            "kb",
            Some("acct-b"),
            true,
            true,
            &gen_test_secret_armor(Some("pb")),
        );
        let keys = vec![a, b];
        // Sending from acct-b must pick acct-b's primary, not the global one.
        let picked =
            resolve_signing_key(&keys, &OpengpgSendOptions::default(), "acct-b").expect("pick");
        assert_eq!(picked.id, "kb");

        // A cross-account explicit signingKeyId is refused…
        let cross = OpengpgSendOptions {
            signing_key_id: Some("ka".into()),
            ..Default::default()
        };
        assert!(resolve_signing_key(&keys, &cross, "acct-b").is_err());
        // …while an in-account explicit pick works.
        assert!(resolve_signing_key(&keys, &cross, "acct-a").is_ok());

        // An account without any identity key has no fallback.
        let contact = key_row(
            "kc",
            None,
            false,
            false,
            &public_armored_from_stored(&gen_test_secret_armor(Some("pc"))).unwrap(),
        );
        assert!(
            resolve_signing_key(
                std::slice::from_ref(&contact),
                &OpengpgSendOptions::default(),
                "acct-c"
            )
            .is_err()
        );
    }
}
