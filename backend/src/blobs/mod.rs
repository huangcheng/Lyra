//! Content-addressable blob storage under `<data_dir>/blobs/`.
//!
//! Blobs are keyed by SHA-256 and sharded under `blobs/{account_id}/{prefix}/`.
//! The database stores only the relative path.
//!
//! See `docs/specs/2026-08-20-lyra-data-model-spec.md` §7.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Error from blob store/read operations.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// SHA-256 hex digest of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Relative path for a blob: `blobs/{account_id}/{prefix}/{hash}`.
pub fn relative_blob_path(account_id: &str, hash: &str) -> String {
    let prefix = hash.get(..2).unwrap_or(hash);
    format!("blobs/{account_id}/{prefix}/{hash}")
}

/// Resolve a stored `storage_path` (relative or legacy absolute) under `data_dir`.
pub fn resolve_storage_path(data_dir: &Path, storage_path: &str) -> PathBuf {
    let path = Path::new(storage_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        data_dir.join(path)
    }
}

/// Store bytes content-addressably; skips write when the blob already exists.
///
/// Returns the relative path to store in the database.
pub async fn store(data_dir: &Path, account_id: &str, data: &[u8]) -> Result<String, BlobError> {
    let hash = sha256_hex(data);
    let rel = relative_blob_path(account_id, &hash);
    let full = data_dir.join(&rel);

    if full.exists() {
        return Ok(rel);
    }

    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(&full, data).await?;
    Ok(rel)
}

/// Read blob bytes from `storage_path` relative to `data_dir`.
pub async fn read(data_dir: &Path, storage_path: &str) -> Result<Vec<u8>, BlobError> {
    let path = resolve_storage_path(data_dir, storage_path);
    Ok(tokio::fs::read(&path).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn relative_path_shards_by_hash_prefix() {
        let hash = sha256_hex(b"test");
        let rel = relative_blob_path("acc-1", &hash);
        assert!(rel.starts_with("blobs/acc-1/"));
        assert!(rel.ends_with(&hash));
    }

    #[tokio::test]
    async fn store_is_content_addressable_and_deduped() {
        let dir = std::env::temp_dir().join(format!(
            "lyra-blob-test-{}",
            Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let data = b"attachment bytes";

        let rel1 = store(&dir, "acct", data).await.unwrap();
        let rel2 = store(&dir, "acct", data).await.unwrap();
        assert_eq!(rel1, rel2);

        let bytes = read(&dir, &rel1).await.unwrap();
        assert_eq!(bytes, data);

        let full = dir.join(&rel1);
        assert!(full.is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_handles_relative_and_absolute() {
        let data_dir = Path::new("/data");
        assert_eq!(
            resolve_storage_path(data_dir, "blobs/a/ab/hash"),
            PathBuf::from("/data/blobs/a/ab/hash")
        );
        assert_eq!(
            resolve_storage_path(data_dir, "/legacy/abs/path"),
            PathBuf::from("/legacy/abs/path")
        );
    }
}
