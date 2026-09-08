//! Chunked import-upload store. Staging files live at
//! `data_dir/backups/staging/upload-<id>.part` (0600) and are renamed to
//! `upload-<id>.lyra` on finish; the per-user registry
//! `backup:uploads:{user_id}` (JSON map id → [`UploadMeta`]) tracks the
//! received-chunk bitmap. Chunking sidesteps the Cloudflare 100 MB body cap
//! on production deploys.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §6.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::kv::KvStore;

use super::BackupError;

/// 8 MiB — fits comfortably under the API body limit and the Cloudflare cap.
pub const CHUNK_SIZE: usize = 8 * 1024 * 1024;
/// 512 chunks × 8 MiB = 4 GiB archive cap.
pub const MAX_CHUNKS: u32 = 512;

/// Registry entry: `received[i]` is true once chunk `i` has been written.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadMeta {
    pub received: Vec<bool>,
    pub created_at: String, // RFC3339
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    /// Unknown id OR another user's upload — never distinguish the two.
    #[error("upload not found")]
    NotFound,
    #[error("chunk index {0} out of range (max {MAX_CHUNKS} chunks)")]
    ChunkOutOfRange(u32),
    #[error("empty chunk body")]
    EmptyChunk,
    #[error("chunk exceeds the 8 MiB chunk size")]
    OversizedChunk,
    #[error("upload incomplete; missing chunks: {0:?}")]
    Incomplete(Vec<u32>),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Backup(#[from] BackupError),
}

fn key(user_id: &str) -> String {
    format!("backup:uploads:{user_id}")
}

async fn load(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
) -> Result<HashMap<String, UploadMeta>, BackupError> {
    match kv.get(&key(user_id)).await {
        Ok(Some(raw)) => Ok(serde_json::from_str(&raw)?),
        Ok(None) => Ok(HashMap::new()),
        Err(e) => Err(BackupError::Internal(e.to_string())),
    }
}

async fn save(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    map: &HashMap<String, UploadMeta>,
) -> Result<(), BackupError> {
    let raw = serde_json::to_string(map)?;
    kv.set(&key(user_id), &raw, Some(24 * 3600))
        .await
        .map_err(|e| BackupError::Internal(e.to_string()))
}

fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("backups").join("staging")
}

/// `data_dir/backups/staging/upload-<id>.<ext>` — `id` must parse as a UUID,
/// so upload ids can never traverse out of the staging dir.
fn upload_path(data_dir: &Path, id: &str, ext: &str) -> Result<PathBuf, UploadError> {
    let uuid = Uuid::parse_str(id).map_err(|_| UploadError::NotFound)?;
    Ok(staging_dir(data_dir).join(format!("upload-{uuid}.{ext}")))
}

/// Mint an upload id, create the empty `.part` file, and register it.
/// Returns the upload id.
pub async fn start(
    kv: &Arc<dyn KvStore>,
    data_dir: &Path,
    user_id: &str,
) -> Result<String, UploadError> {
    let dir = staging_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }

    let id = Uuid::now_v7().to_string();
    let path = upload_path(data_dir, &id, "part")?;
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    opts.open(&path).await?;

    let mut map = load(kv, user_id).await?;
    map.insert(
        id.clone(),
        UploadMeta {
            received: Vec::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
    );
    save(kv, user_id, &map).await?;
    Ok(id)
}

/// Write one chunk at offset `n * CHUNK_SIZE` and mark it received.
/// Size/index validation lives here (not only in the handler) so the store
/// invariants hold for every caller.
pub async fn put_chunk(
    kv: &Arc<dyn KvStore>,
    data_dir: &Path,
    user_id: &str,
    upload_id: &str,
    n: u32,
    bytes: &[u8],
) -> Result<(), UploadError> {
    if bytes.is_empty() {
        return Err(UploadError::EmptyChunk);
    }
    if bytes.len() > CHUNK_SIZE {
        return Err(UploadError::OversizedChunk);
    }
    if n >= MAX_CHUNKS {
        return Err(UploadError::ChunkOutOfRange(n));
    }

    let mut map = load(kv, user_id).await?;
    let Some(meta) = map.get_mut(upload_id) else {
        return Err(UploadError::NotFound);
    };

    let path = upload_path(data_dir, upload_id, "part")?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .await?;
    file.seek(std::io::SeekFrom::Start(u64::from(n) * CHUNK_SIZE as u64))
        .await?;
    file.write_all(bytes).await?;
    file.flush().await?;

    let idx = n as usize;
    if meta.received.len() <= idx {
        meta.received.resize(idx + 1, false);
    }
    meta.received[idx] = true;
    save(kv, user_id, &map).await?;
    Ok(())
}

/// Verify all chunks `0..total_chunks` arrived and the staged file size
/// matches, then rename `.part` → `.lyra` and drop the registry entry.
/// Returns the final staged path for the import job.
pub async fn finish(
    kv: &Arc<dyn KvStore>,
    data_dir: &Path,
    user_id: &str,
    upload_id: &str,
    total_chunks: u32,
) -> Result<PathBuf, UploadError> {
    if total_chunks == 0 || total_chunks > MAX_CHUNKS {
        return Err(UploadError::ChunkOutOfRange(total_chunks));
    }

    let mut map = load(kv, user_id).await?;
    let Some(meta) = map.get(upload_id) else {
        return Err(UploadError::NotFound);
    };
    let missing: Vec<u32> = (0..total_chunks)
        .filter(|i| meta.received.get(*i as usize) != Some(&true))
        .collect();
    if !missing.is_empty() {
        return Err(UploadError::Incomplete(missing));
    }

    let part = upload_path(data_dir, upload_id, "part")?;
    let file_len = tokio::fs::metadata(&part).await?.len();
    // Every chunk but the last must be full-size, so the file must reach
    // into the last chunk's range.
    let min_expected = u64::from(total_chunks - 1) * CHUNK_SIZE as u64 + 1;
    if file_len < min_expected {
        return Err(UploadError::Incomplete((0..total_chunks).collect()));
    }

    let final_path = upload_path(data_dir, upload_id, "lyra")?;
    tokio::fs::rename(&part, &final_path).await?;
    map.remove(upload_id);
    save(kv, user_id, &map).await?;
    Ok(final_path)
}

/// Best-effort cleanup of an abandoned upload (kv entry + `.part`/`.lyra`).
#[allow(dead_code)] // used by staged-upload GC (later task)
pub async fn discard(
    kv: &Arc<dyn KvStore>,
    data_dir: &Path,
    user_id: &str,
    upload_id: &str,
) -> Result<(), BackupError> {
    let mut map = load(kv, user_id).await?;
    map.remove(upload_id);
    save(kv, user_id, &map).await?;
    for ext in ["part", "lyra"] {
        if let Ok(path) = upload_path(data_dir, upload_id, ext) {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::MemoryKv;
    use sha2::{Digest, Sha256};

    fn kv() -> Arc<dyn KvStore> {
        Arc::new(MemoryKv::new())
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    /// 20 MiB across 3 chunks (8 + 8 + 4), written out of order; the
    /// assembled file must hash identically to the source buffer.
    #[tokio::test]
    async fn chunked_roundtrip_assembles_original_bytes() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let upload_id = start(&kv, dir.path(), "u1").await.unwrap();

        let mut source = vec![0u8; 20 * 1024 * 1024];
        for (i, b) in source.iter_mut().enumerate() {
            *b = u8::try_from(i % 251).unwrap();
        }
        let chunks: Vec<&[u8]> = vec![
            &source[..CHUNK_SIZE],
            &source[CHUNK_SIZE..2 * CHUNK_SIZE],
            &source[2 * CHUNK_SIZE..],
        ];
        // Out-of-order delivery must still assemble correctly.
        put_chunk(&kv, dir.path(), "u1", &upload_id, 2, chunks[2])
            .await
            .unwrap();
        put_chunk(&kv, dir.path(), "u1", &upload_id, 0, chunks[0])
            .await
            .unwrap();
        put_chunk(&kv, dir.path(), "u1", &upload_id, 1, chunks[1])
            .await
            .unwrap();

        let final_path = finish(&kv, dir.path(), "u1", &upload_id, 3).await.unwrap();
        assert!(final_path.ends_with(format!("upload-{upload_id}.lyra")));
        let assembled = tokio::fs::read(&final_path).await.unwrap();
        assert_eq!(sha256(&assembled), sha256(&source));
        // Registry entry is gone after finish.
        assert!(load(&kv, "u1").await.unwrap().is_empty());
        // The .part file no longer exists.
        assert!(
            !upload_path(dir.path(), &upload_id, "part")
                .unwrap()
                .exists()
        );
    }

    #[tokio::test]
    async fn finish_with_missing_chunk_lists_missing_indices() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let upload_id = start(&kv, dir.path(), "u1").await.unwrap();

        put_chunk(&kv, dir.path(), "u1", &upload_id, 0, b"chunk0")
            .await
            .unwrap();
        put_chunk(&kv, dir.path(), "u1", &upload_id, 2, b"chunk2")
            .await
            .unwrap();

        let err = finish(&kv, dir.path(), "u1", &upload_id, 3)
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::Incomplete(missing) if missing == vec![1]));
        // Registry entry survives an incomplete finish so the client can retry.
        assert!(load(&kv, "u1").await.unwrap().contains_key(&upload_id));
    }

    #[tokio::test]
    async fn rejects_oversized_empty_and_out_of_range_chunks() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let upload_id = start(&kv, dir.path(), "u1").await.unwrap();

        let oversized = vec![0u8; CHUNK_SIZE + 1];
        let err = put_chunk(&kv, dir.path(), "u1", &upload_id, 0, &oversized)
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::OversizedChunk));

        let err = put_chunk(&kv, dir.path(), "u1", &upload_id, 0, b"")
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::EmptyChunk));

        let err = put_chunk(&kv, dir.path(), "u1", &upload_id, MAX_CHUNKS, b"x")
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::ChunkOutOfRange(n) if n == MAX_CHUNKS));

        // Nothing was marked received.
        let map = load(&kv, "u1").await.unwrap();
        assert!(map[&upload_id].received.is_empty());
    }

    #[tokio::test]
    async fn other_users_upload_is_not_found() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let upload_id = start(&kv, dir.path(), "u1").await.unwrap();

        let err = put_chunk(&kv, dir.path(), "u2", &upload_id, 0, b"x")
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::NotFound));

        let err = finish(&kv, dir.path(), "u2", &upload_id, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::NotFound));

        // A non-UUID id can never resolve to a path, let alone a file.
        let err = put_chunk(&kv, dir.path(), "u1", "../etc/passwd", 0, b"x")
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::NotFound));
    }

    #[tokio::test]
    async fn finish_rejects_zero_and_too_many_chunks() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let upload_id = start(&kv, dir.path(), "u1").await.unwrap();
        put_chunk(&kv, dir.path(), "u1", &upload_id, 0, b"x")
            .await
            .unwrap();

        assert!(matches!(
            finish(&kv, dir.path(), "u1", &upload_id, 0)
                .await
                .unwrap_err(),
            UploadError::ChunkOutOfRange(0)
        ));
        assert!(matches!(
            finish(&kv, dir.path(), "u1", &upload_id, MAX_CHUNKS + 1)
                .await
                .unwrap_err(),
            UploadError::ChunkOutOfRange(n) if n == MAX_CHUNKS + 1
        ));
    }
}
