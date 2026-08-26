//! Decrypt & verify OpenGPG on message read (opengpg-spec P2).
//!
//! Decrypt is per-request (not persisted). Unlocked secret keys come from
//! the session unlock ring; HTML still passes ammonia via `persist_body_html`.

use std::io::Cursor;

use pgp::composed::{
    CleartextSignedMessage, Deserializable, Message, SignedPublicKey, SignedSecretKey,
    VerificationResult,
};
use pgp::types::{Password, VerifyingKey};
use serde::Serialize;
use zeroize::Zeroizing;

use super::keys::OpengpgError;
use super::session::UnlockRing;
use super::store::{StoredKey, list_keys};
use crate::auth::AuthState;
use crate::sanitize::persist_body_html;
use crate::storage::DbPool;

const BEGIN_MESSAGE: &str = "-----BEGIN PGP MESSAGE-----";
const BEGIN_SIGNED: &str = "-----BEGIN PGP SIGNED MESSAGE-----";
const BEGIN_SIGNATURE: &str = "-----BEGIN PGP SIGNATURE-----";

/// OpenGPG status block on message responses (camelCase for `/api/v1`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OpengpgMessageStatus {
    pub encrypted: bool,
    pub decrypted: bool,
    pub signatures: Vec<OpengpgSignatureStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OpengpgSignatureStatus {
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DecryptedBodies {
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub status: OpengpgMessageStatus,
}

/// Detect OpenGPG ciphertext / cleartext signature in body or attachment bytes.
#[must_use]
pub fn looks_encrypted(text: &str) -> bool {
    text.contains(BEGIN_MESSAGE)
}

#[must_use]
pub fn looks_cleartext_signed(text: &str) -> bool {
    text.contains(BEGIN_SIGNED) && text.contains(BEGIN_SIGNATURE)
}

/// Extract the first armored PGP MESSAGE block (inclusive).
#[must_use]
pub fn extract_armored_message(text: &str) -> Option<String> {
    extract_armored_block(text, BEGIN_MESSAGE, "-----END PGP MESSAGE-----")
}

#[must_use]
pub fn extract_cleartext_signed(text: &str) -> Option<String> {
    let start = text.find(BEGIN_SIGNED)?;
    let end_marker = "-----END PGP SIGNATURE-----";
    let end = text[start..].find(end_marker)? + start + end_marker.len();
    Some(text[start..end].to_string())
}

fn extract_armored_block(text: &str, begin: &str, end: &str) -> Option<String> {
    let start = text.find(begin)?;
    let end_pos = text[start..].find(end)? + start + end.len();
    Some(text[start..end_pos].to_string())
}

/// Try to decrypt / verify bodies for a message read.
///
/// `session_token` selects the unlock ring. Attachment candidates are optional
/// raw bytes (e.g. OpenPGP/MIME `application/octet-stream` part).
pub fn process_message_bodies(
    body_text: Option<&str>,
    body_html: Option<&str>,
    attachment_candidates: &[Vec<u8>],
    secret_keys: &[(StoredKey, Zeroizing<String>)],
    public_keys: &[StoredKey],
) -> Option<DecryptedBodies> {
    let text = body_text.unwrap_or("");
    let html = body_html.unwrap_or("");

    let armored = extract_armored_message(text)
        .or_else(|| extract_armored_message(html))
        .or_else(|| find_armored_in_attachments(attachment_candidates));

    let cleartext = if armored.is_none() {
        extract_cleartext_signed(text).or_else(|| extract_cleartext_signed(html))
    } else {
        None
    };

    if armored.is_none() && cleartext.is_none() {
        // Binary OpenPGP packet starting with tag byte (common in MIME part 2).
        if let Some(bin) = attachment_candidates
            .iter()
            .find(|b| looks_like_binary_openpgp(b))
        {
            return Some(decrypt_bytes(bin, secret_keys, public_keys, true));
        }
        return None;
    }

    if let Some(armor) = armored {
        return Some(decrypt_armored(&armor, secret_keys, public_keys));
    }

    if let Some(csf) = cleartext {
        return Some(verify_cleartext(&csf, public_keys));
    }

    None
}

fn find_armored_in_attachments(atts: &[Vec<u8>]) -> Option<String> {
    for data in atts {
        if let Ok(s) = std::str::from_utf8(data)
            && let Some(a) = extract_armored_message(s)
        {
            return Some(a);
        }
    }
    None
}

fn looks_like_binary_openpgp(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    // Old or new format OpenPGP packet tag (MSB set).
    data[0] & 0x80 != 0
}

fn decrypt_armored(
    armor: &str,
    secret_keys: &[(StoredKey, Zeroizing<String>)],
    public_keys: &[StoredKey],
) -> DecryptedBodies {
    decrypt_bytes(armor.as_bytes(), secret_keys, public_keys, true)
}

fn decrypt_bytes(
    data: &[u8],
    secret_keys: &[(StoredKey, Zeroizing<String>)],
    public_keys: &[StoredKey],
    encrypted: bool,
) -> DecryptedBodies {
    if secret_keys.is_empty() {
        return DecryptedBodies {
            body_text: None,
            body_html: None,
            status: OpengpgMessageStatus {
                encrypted,
                decrypted: false,
                signatures: Vec::new(),
                error: Some(if encrypted {
                    "locked".into()
                } else {
                    "no matching secret key".into()
                }),
            },
        };
    }

    let mut last_err = "no matching secret key".to_string();
    for (stored, passphrase) in secret_keys {
        let Ok((secret, _)) = SignedSecretKey::from_string(&stored.key_data) else {
            continue;
        };
        let Ok(message) = Message::from_bytes(Cursor::new(data))
            .or_else(|_| Message::from_armor(Cursor::new(data)).map(|(m, _)| m))
        else {
            return DecryptedBodies {
                body_text: None,
                body_html: None,
                status: OpengpgMessageStatus {
                    encrypted,
                    decrypted: false,
                    signatures: Vec::new(),
                    error: Some("malformed OpenGPG message".into()),
                },
            };
        };
        let pw = Password::from(passphrase.as_str());
        match message.decrypt(&pw, &secret) {
            Ok(decrypted) => {
                let Ok(mut msg) = decrypted.decompress() else {
                    last_err = "decompress failed".into();
                    continue;
                };
                // Drain literal for verify + content.
                let content = msg.as_data_vec().unwrap_or_default();
                let signatures = verify_message_signatures(&msg, public_keys);
                let (body_text, body_html) = bodies_from_decrypted(&content);
                return DecryptedBodies {
                    body_text,
                    body_html,
                    status: OpengpgMessageStatus {
                        encrypted: true,
                        decrypted: true,
                        signatures,
                        error: None,
                    },
                };
            }
            Err(e) => {
                last_err = format!("decrypt failed: {e}");
            }
        }
    }

    let _ = last_err;
    DecryptedBodies {
        body_text: None,
        body_html: None,
        status: OpengpgMessageStatus {
            encrypted: true,
            decrypted: false,
            signatures: Vec::new(),
            error: Some("no matching secret key".into()),
        },
    }
}

fn verify_cleartext(csf: &str, public_keys: &[StoredKey]) -> DecryptedBodies {
    let Ok((msg, _)) = CleartextSignedMessage::from_string(csf) else {
        return DecryptedBodies {
            body_text: None,
            body_html: None,
            status: OpengpgMessageStatus {
                encrypted: false,
                decrypted: false,
                signatures: Vec::new(),
                error: Some("malformed cleartext signature".into()),
            },
        };
    };

    let text = msg.text().to_string();
    let mut signatures = Vec::new();
    for key in public_keys {
        let Ok((pk, _)) = SignedPublicKey::from_string(&key.key_data) else {
            continue;
        };
        let valid = msg.verify(&pk).is_ok();
        if valid || signatures.is_empty() {
            signatures.push(OpengpgSignatureStatus {
                fingerprint: key.fingerprint.clone(),
                email: Some(key.primary_email.clone()),
                valid,
                time: None,
            });
            if valid {
                // Prefer reporting the matching key; keep only valid hits.
                signatures.retain(|s| s.valid);
                break;
            }
        }
    }

    if signatures.is_empty() {
        signatures.push(OpengpgSignatureStatus {
            fingerprint: String::new(),
            email: None,
            valid: false,
            time: None,
        });
    }

    DecryptedBodies {
        body_text: Some(text),
        body_html: None,
        status: OpengpgMessageStatus {
            encrypted: false,
            decrypted: false,
            signatures,
            error: None,
        },
    }
}

fn verify_message_signatures(
    msg: &Message<'_>,
    public_keys: &[StoredKey],
) -> Vec<OpengpgSignatureStatus> {
    if !msg.is_signed() && !msg.is_one_pass_signed() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for key in public_keys {
        let Ok((pk, _)) = SignedPublicKey::from_string(&key.key_data) else {
            continue;
        };
        let refs: Vec<&dyn VerifyingKey> = vec![&pk as &dyn VerifyingKey];
        let Ok(results) = msg.verify_nested(&refs) else {
            continue;
        };
        let valid = results
            .iter()
            .any(|r| matches!(r, VerificationResult::Valid(_)));
        if valid {
            out.push(OpengpgSignatureStatus {
                fingerprint: key.fingerprint.clone(),
                email: Some(key.primary_email.clone()),
                valid: true,
                time: None,
            });
        }
    }
    out
}

fn bodies_from_decrypted(content: &[u8]) -> (Option<String>, Option<String>) {
    // Inner payload may be raw text or a full MIME message.
    if let Some(message) = mail_parser::MessageParser::default().parse(content) {
        let body_text = message.body_text(0).map(std::borrow::Cow::into_owned);
        let body_html = persist_body_html(message.body_html(0).as_deref());
        if body_text.is_some() || body_html.is_some() {
            return (body_text, body_html);
        }
    }
    let text = String::from_utf8_lossy(content).into_owned();
    if text
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("<!doctype")
        || text.trim_start().to_ascii_lowercase().starts_with("<html")
    {
        (None, persist_body_html(Some(&text)))
    } else {
        (Some(text), None)
    }
}

/// Load unlocked secrets + all keys, try decrypt/verify for a message body.
pub async fn enrich_message_opengpg(
    state: &AuthState,
    user_id: &str,
    session_token: &str,
    message_id: &str,
    body_text: Option<&str>,
    body_html: Option<&str>,
) -> Result<Option<DecryptedBodies>, OpengpgError> {
    let text = body_text.unwrap_or("");
    let html = body_html.unwrap_or("");
    let may_need_atts = !looks_encrypted(text)
        && !looks_encrypted(html)
        && !looks_cleartext_signed(text)
        && !looks_cleartext_signed(html);

    let attachment_candidates = if may_need_atts {
        load_pgpish_attachments(&state.db, &state.data_dir, message_id).await?
    } else {
        Vec::new()
    };

    let has_signal = looks_encrypted(text)
        || looks_encrypted(html)
        || looks_cleartext_signed(text)
        || looks_cleartext_signed(html)
        || !attachment_candidates.is_empty();

    if !has_signal {
        return Ok(None);
    }

    let keys = list_keys(&state.db, user_id).await?;
    let unlocked = collect_unlocked_secrets(&keys, &state.opengpg_unlock, session_token);

    // Encrypted content but secrets exist and none unlocked → locked.
    let encrypted_signal = looks_encrypted(text)
        || looks_encrypted(html)
        || attachment_candidates.iter().any(|b| {
            std::str::from_utf8(b).is_ok_and(looks_encrypted) || looks_like_binary_openpgp(b)
        });

    if encrypted_signal && unlocked.is_empty() {
        let err = if keys.iter().any(|k| k.is_secret) {
            "locked"
        } else {
            "no matching secret key"
        };
        return Ok(Some(DecryptedBodies {
            body_text: None,
            body_html: None,
            status: OpengpgMessageStatus {
                encrypted: true,
                decrypted: false,
                signatures: Vec::new(),
                error: Some(err.into()),
            },
        }));
    }

    Ok(process_message_bodies(
        body_text,
        body_html,
        &attachment_candidates,
        &unlocked,
        &keys,
    ))
}

fn collect_unlocked_secrets(
    keys: &[StoredKey],
    ring: &UnlockRing,
    session_token: &str,
) -> Vec<(StoredKey, Zeroizing<String>)> {
    let mut out = Vec::new();
    for key in keys {
        if !key.is_secret {
            continue;
        }
        if let Some(pw) = ring.get(session_token, &key.id) {
            out.push((key.clone(), pw));
        }
    }
    out
}

async fn load_pgpish_attachments(
    db: &DbPool,
    data_dir: &std::path::Path,
    message_id: &str,
) -> Result<Vec<Vec<u8>>, OpengpgError> {
    use crate::db_row::id_param;
    use sqlx::Row;

    let bind = id_param(db, message_id).map_err(|_| OpengpgError::InvalidInput("id".into()))?;
    let rows = db_fetch_all!(
        db,
        r"
        SELECT filename, content_type, storage_path
        FROM attachment
        WHERE message_id = ?
        ",
        |row| {
            let filename: Option<String> = row.get("filename");
            let content_type: Option<String> = row.get("content_type");
            let storage_path: String = row.get("storage_path");
            (filename, content_type, storage_path)
        },
        &bind
    )
    .map_err(OpengpgError::Database)?;

    let mut out = Vec::new();
    for (filename, content_type, storage_path) in rows {
        let ct = content_type.unwrap_or_default().to_ascii_lowercase();
        let name = filename.unwrap_or_default().to_ascii_lowercase();
        let interesting = ct.contains("pgp")
            || ct.contains("octet-stream")
            || std::path::Path::new(&name).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("asc")
                    || ext.eq_ignore_ascii_case("pgp")
                    || ext.eq_ignore_ascii_case("gpg")
            });
        if !interesting {
            continue;
        }
        let path = crate::blobs::resolve_storage_path(data_dir, &storage_path);
        if let Ok(bytes) = tokio::fs::read(&path).await
            && !bytes.is_empty()
            && bytes.len() < 8 * 1024 * 1024
        {
            out.push(bytes);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opengpg::keys::tests_support::gen_test_secret_armor;
    use crate::opengpg::keys::{parse_armored_key, public_armored_from_stored};
    use pgp::composed::{ArmorOptions, MessageBuilder};
    use pgp::crypto::sym::SymmetricKeyAlgorithm;
    use pgp::types::CompressionAlgorithm;
    use rand::SeedableRng;

    fn encrypt_for(secret_armor: &str, plaintext: &str) -> String {
        let (skey, _) = SignedSecretKey::from_string(secret_armor).unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let mut builder = MessageBuilder::from_bytes("", plaintext.as_bytes().to_vec());
        builder.compression(CompressionAlgorithm::ZLIB);
        let mut builder = builder.seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES128);
        // Encryption subkey (RSA-4096 / cv25519 layouts from keygen).
        let enc = &skey.secret_subkeys[0].public_key();
        builder.encrypt_to_key(&mut rng, enc).unwrap();
        builder
            .to_armored_string(&mut rng, ArmorOptions::default())
            .unwrap()
    }

    #[test]
    fn extract_armored_message_block() {
        let body = format!("hello\n{BEGIN_MESSAGE}\nversion\n-----END PGP MESSAGE-----\nbye");
        let block = extract_armored_message(&body).expect("block");
        assert!(block.starts_with(BEGIN_MESSAGE));
        assert!(block.ends_with("-----END PGP MESSAGE-----"));
    }

    #[test]
    fn decrypt_inline_armored_with_passphrase() {
        let armor = gen_test_secret_armor(Some("test-pass"));
        let parsed = parse_armored_key(&armor).unwrap();
        let ciphertext = encrypt_for(&armor, "secret hello");
        let stored = StoredKey {
            id: "k1".into(),
            user_id: "u".into(),
            fingerprint: parsed.fingerprint.clone(),
            primary_email: parsed.primary_email.clone(),
            emails: parsed.emails.clone(),
            is_secret: true,
            is_primary: true,
            revoked: false,
            key_data: armor,
            created_at: None,
            updated_at: None,
        };
        let secrets = vec![(stored.clone(), Zeroizing::new("test-pass".into()))];
        let out = process_message_bodies(
            Some(&ciphertext),
            None,
            &[],
            &secrets,
            std::slice::from_ref(&stored),
        )
        .expect("detected");
        assert!(out.status.encrypted);
        assert!(out.status.decrypted);
        assert_eq!(out.body_text.as_deref(), Some("secret hello"));
        assert!(out.status.error.is_none());
    }

    #[test]
    fn locked_when_no_unlocked_secrets() {
        let armor = gen_test_secret_armor(Some("test-pass"));
        let ciphertext = encrypt_for(&armor, "nope");
        let out = process_message_bodies(Some(&ciphertext), None, &[], &[], &[]).expect("detected");
        assert!(out.status.encrypted);
        assert!(!out.status.decrypted);
        assert_eq!(out.status.error.as_deref(), Some("locked"));
    }

    #[test]
    fn cleartext_signed_verifies() {
        let armor = gen_test_secret_armor(Some("test-pass"));
        let (skey, _) = SignedSecretKey::from_string(&armor).unwrap();
        let mut rng = rand::thread_rng();
        let csf = CleartextSignedMessage::sign(
            &mut rng,
            "signed body",
            &skey.primary_key,
            &Password::from("test-pass"),
        )
        .unwrap();
        let armored = csf.to_armored_string(ArmorOptions::default()).unwrap();
        let pub_armor = public_armored_from_stored(&armor).unwrap();
        let parsed = parse_armored_key(&pub_armor).unwrap();
        let stored = StoredKey {
            id: "k1".into(),
            user_id: "u".into(),
            fingerprint: parsed.fingerprint,
            primary_email: parsed.primary_email,
            emails: parsed.emails,
            is_secret: false,
            is_primary: false,
            revoked: false,
            key_data: pub_armor,
            created_at: None,
            updated_at: None,
        };
        let out = process_message_bodies(Some(&armored), None, &[], &[], &[stored]).expect("csf");
        assert!(!out.status.encrypted);
        assert_eq!(out.body_text.as_deref(), Some("signed body"));
        assert!(out.status.signatures.iter().any(|s| s.valid));
    }
}
