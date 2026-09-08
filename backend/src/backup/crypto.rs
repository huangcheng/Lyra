//! age passphrase encryption for the archive file (scrypt recipient;
//! decryptable with the OSS `rage` CLI).
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §3.

use std::io::{Read, Write};
use std::path::Path;

use crate::backup::BackupError;

pub fn encrypt_file(src: &Path, dst: &Path, password: &str) -> Result<(), BackupError> {
    // v1 reads the whole archive into memory; add a streaming variant when
    // archive sizes justify it.
    let plaintext = std::fs::read(src)?;
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        password.to_string(),
    ));
    let mut writer = encryptor
        .wrap_output(std::fs::File::create(dst)?)
        .map_err(|e| BackupError::Crypto(e.to_string()))?;
    writer.write_all(&plaintext)?;
    writer
        .finish()
        .map_err(|e| BackupError::Crypto(e.to_string()))?;
    Ok(())
}

pub fn decrypt_file(src: &Path, dst: &Path, password: &str) -> Result<(), BackupError> {
    // v1 reads the whole archive into memory; add a streaming variant when
    // archive sizes justify it.
    let file = std::fs::File::open(src)?;
    let Ok(decryptor) = age::Decryptor::new(file) else {
        return Err(BackupError::CorruptArchive);
    };
    if !decryptor.is_scrypt() {
        return Err(BackupError::Crypto("not a passphrase archive".into()));
    }
    let identity =
        age::scrypt::Identity::new(age::secrecy::SecretString::from(password.to_string()));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as _))
        .map_err(|_| BackupError::InvalidPassword)?;
    let mut plaintext = Vec::new();
    reader.read_to_end(&mut plaintext)?;
    std::fs::write(dst, &plaintext)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("a.bin");
        let enc = dir.path().join("a.bin.age");
        let dec = dir.path().join("a.out.bin");
        let bytes = "backup bytes \u{1f600}".as_bytes();
        std::fs::write(&plain, bytes).unwrap();
        encrypt_file(&plain, &enc, "test-password-1").unwrap();
        decrypt_file(&enc, &dec, "test-password-1").unwrap();
        assert_eq!(std::fs::read(&dec).unwrap(), bytes);
    }

    #[test]
    fn wrong_password_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("a.bin");
        let enc = dir.path().join("a.bin.age");
        let dec = dir.path().join("a.out.bin");
        std::fs::write(&plain, b"x").unwrap();
        encrypt_file(&plain, &enc, "right").unwrap();
        let err = decrypt_file(&enc, &dec, "wrong").unwrap_err();
        assert!(matches!(err, BackupError::InvalidPassword));
    }
}
