//! GnuPG CLI interop checks (opengpg-spec Verification / CHE-70).
//!
//! Tests skip automatically when `gpg` is not on `PATH`.

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    use pgp::composed::{
        ArmorOptions, Deserializable, DetachedSignature, MessageBuilder, SignedSecretKey,
    };
    use pgp::crypto::hash::HashAlgorithm;
    use pgp::crypto::sym::SymmetricKeyAlgorithm;
    use pgp::types::{CompressionAlgorithm, Password};
    use rand::SeedableRng;
    use zeroize::Zeroizing;

    use crate::opengpg::keys::tests_support::gen_test_secret_armor;
    use crate::opengpg::keys::{parse_armored_key, public_armored_from_stored};
    use crate::opengpg::read::process_message_bodies;
    use crate::opengpg::store::StoredKey;

    static GPG_HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

    const PASSPHRASE: &str = "interop-pass";
    const PLAINTEXT: &str = "Lyra ↔ GnuPG interop hello";

    fn gpg_available() -> bool {
        Command::new("gpg")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn fresh_gpg_home() -> PathBuf {
        let n = GPG_HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lyra-opengpg-interop-{n}"));
        std::fs::create_dir_all(&dir).expect("gpg temp home");
        dir
    }

    fn run_gpg(home: &Path, args: &[&str]) -> Output {
        Command::new("gpg")
            .env("GNUPGHOME", home)
            .args(args)
            .output()
            .expect("spawn gpg")
    }

    fn import_armored_key(home: &Path, armor: &str) {
        let path = home.join("key.asc");
        std::fs::write(&path, armor).expect("write key");
        let out = run_gpg(
            home,
            &[
                "--batch",
                "--yes",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                PASSPHRASE,
                "--import",
                path.to_str().expect("utf8 path"),
            ],
        );
        assert!(
            out.status.success(),
            "gpg --import failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn lyra_encrypt_armored(secret_armor: &str, plaintext: &str) -> String {
        let (skey, _) = SignedSecretKey::from_string(secret_armor).expect("parse secret");
        let pub_key = skey.to_public_key();
        let mut rng = rand::rngs::StdRng::seed_from_u64(77);
        let mut builder = MessageBuilder::from_bytes("", plaintext.as_bytes().to_vec());
        builder.compression(CompressionAlgorithm::ZLIB);
        let mut builder = builder.seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
        let enc = pub_key.public_subkeys.first().expect("encryption subkey");
        builder
            .encrypt_to_key(&mut rng, enc)
            .expect("encrypt_to_key");
        builder
            .to_armored_string(&mut rng, ArmorOptions::default())
            .expect("armor")
    }

    fn stored_from_armor(armor: &str, id: &str) -> StoredKey {
        let parsed = parse_armored_key(armor).expect("parse");
        StoredKey {
            id: id.into(),
            user_id: "u".into(),
            fingerprint: parsed.fingerprint,
            primary_email: parsed.primary_email.clone(),
            emails: parsed.emails,
            is_secret: parsed.is_secret,
            is_primary: true,
            revoked: false,
            key_data: parsed.key_data,
            created_at: None,
            updated_at: None,
        }
    }

    struct Fixture {
        home: PathBuf,
        secret_armor: String,
        email: String,
        secret_stored: StoredKey,
        public_stored: StoredKey,
    }

    fn setup() -> Option<Fixture> {
        if !gpg_available() {
            eprintln!("skip interop: gpg not on PATH");
            return None;
        }
        let home = fresh_gpg_home();
        let secret_armor = gen_test_secret_armor(Some(PASSPHRASE));
        import_armored_key(&home, &secret_armor);
        let parsed = parse_armored_key(&secret_armor).expect("parse secret");
        let pub_armor = public_armored_from_stored(&secret_armor).expect("public half");
        let secret_stored = stored_from_armor(&secret_armor, "sec");
        let public_stored = stored_from_armor(&pub_armor, "pub");
        Some(Fixture {
            home,
            secret_armor,
            email: parsed.primary_email,
            secret_stored,
            public_stored,
        })
    }

    #[test]
    fn gpg_decrypts_lyra_encrypted_message() {
        let Some(fix) = setup() else {
            return;
        };
        let ciphertext = lyra_encrypt_armored(&fix.secret_armor, PLAINTEXT);
        let cipher_path = fix.home.join("lyra.asc");
        std::fs::write(&cipher_path, &ciphertext).expect("write ciphertext");

        let out = run_gpg(
            &fix.home,
            &[
                "--batch",
                "--yes",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                PASSPHRASE,
                "--decrypt",
                cipher_path.to_str().expect("utf8"),
            ],
        );
        assert!(
            out.status.success(),
            "gpg --decrypt failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let decrypted = String::from_utf8_lossy(&out.stdout);
        assert!(
            decrypted.contains(PLAINTEXT),
            "expected plaintext in gpg output, got: {decrypted}"
        );
    }

    #[test]
    fn lyra_decrypts_gpg_encrypted_message() {
        let Some(fix) = setup() else {
            return;
        };
        let mut child = Command::new("gpg")
            .env("GNUPGHOME", &fix.home)
            .args([
                "--batch",
                "--yes",
                "--trust-model",
                "always",
                "--armor",
                "--encrypt",
                "-r",
                &fix.email,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gpg encrypt");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(PLAINTEXT.as_bytes())
            .expect("write plaintext");
        let out = child.wait_with_output().expect("gpg encrypt output");
        assert!(
            out.status.success(),
            "gpg --encrypt failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let armored = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(armored.contains("BEGIN PGP MESSAGE"));

        let unlock = [(
            fix.secret_stored.clone(),
            Zeroizing::new(PASSPHRASE.to_string()),
        )];
        let out = process_message_bodies(Some(&armored), None, &[], &unlock, &[fix.public_stored])
            .expect("opengpg detected");
        assert!(out.status.encrypted);
        assert!(out.status.decrypted, "error: {:?}", out.status.error);
        assert_eq!(out.body_text.as_deref(), Some(PLAINTEXT));
    }

    #[test]
    fn gpg_verifies_lyra_detached_signature() {
        let Some(fix) = setup() else {
            return;
        };
        let (skey, _) = SignedSecretKey::from_string(&fix.secret_armor).expect("secret");
        let mut rng = rand::rngs::StdRng::seed_from_u64(88);
        let sig = DetachedSignature::sign_binary_data(
            &mut rng,
            &skey.primary_key,
            &Password::from(PASSPHRASE),
            HashAlgorithm::Sha256,
            PLAINTEXT.as_bytes(),
        )
        .expect("sign");
        let sig_armor = sig
            .to_armored_string(ArmorOptions::default())
            .expect("armor sig");
        let sig_path = fix.home.join("lyra.sig.asc");
        let data_path = fix.home.join("lyra.txt");
        std::fs::write(&sig_path, sig_armor).expect("write sig");
        std::fs::write(&data_path, PLAINTEXT).expect("write data");

        let out = run_gpg(
            &fix.home,
            &[
                "--batch",
                "--verify",
                sig_path.to_str().expect("utf8"),
                data_path.to_str().expect("utf8"),
            ],
        );
        assert!(
            out.status.success(),
            "gpg --verify failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
