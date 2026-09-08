//! Chunked import-upload store. Staging files live at
//! `data_dir/backups/staging/upload-<id>.part` (0600) and are renamed to
//! `upload-<id>.lyra` on finish; the per-user registry
//! `backup:uploads:{user_id}` (JSON map id → [`UploadMeta`]) tracks the
//! received size of each chunk (0 = not received, so a short non-final
//! chunk can never hide a zero-filled hole). Chunking sidesteps the
//! Cloudflare 100 MB body cap on production deploys.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §6.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::kv::KvStore;

use super::BackupError;

/// 8 MiB — fits comfortably under the API body limit and the Cloudflare cap.
pub const CHUNK_SIZE: usize = 8 * 1024 * 1024;
/// 512 chunks × 8 MiB = 4 GiB archive cap.
pub const MAX_CHUNKS: u32 = 512;
/// Abandoned uploads (kv entry expired, file left behind) are swept once
/// their mtime is this old.
const STALE_UPLOAD_AGE: Duration = Duration::from_hours(24);

/// Registry entry: `chunk_sizes[i]` is the byte length of chunk `i` once
/// written; 0 means not received.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadMeta {
    pub chunk_sizes: Vec<u32>,
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
            chunk_sizes: Vec::new(),
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
    if meta.chunk_sizes.len() <= idx {
        meta.chunk_sizes.resize(idx + 1, 0);
    }
    meta.chunk_sizes[idx] = u32::try_from(bytes.len()).expect("chunk length fits u32");
    save(kv, user_id, &map).await?;
    Ok(())
}

/// Verify every chunk `0..total_chunks` arrived with the right length (all
/// non-final chunks exactly `CHUNK_SIZE`, final chunk 1..=`CHUNK_SIZE`) and
/// the staged file size matches the recorded sizes, then rename
/// `.part` → `.lyra` and drop the registry entry. Returns the final staged
/// path for the import job.
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
    let last = (total_chunks - 1) as usize;
    let bad: Vec<u32> = (0..total_chunks)
        .filter(|i| {
            let size = meta.chunk_sizes.get(*i as usize).copied().unwrap_or(0) as usize;
            if *i as usize == last {
                // Final chunk: anything from 1 byte to a full chunk.
                size == 0 || size > CHUNK_SIZE
            } else {
                // A short non-final chunk would leave a zero-filled hole.
                size != CHUNK_SIZE
            }
        })
        .collect();
    if !bad.is_empty() {
        return Err(UploadError::Incomplete(bad));
    }

    let part = upload_path(data_dir, upload_id, "part")?;
    let file_len = tokio::fs::metadata(&part).await?.len();
    let expected: u64 = (0..total_chunks)
        .map(|i| u64::from(meta.chunk_sizes[i as usize]))
        .sum();
    if file_len != expected {
        // Registry and file disagree (crash between write and kv save, or
        // external truncation) — the client must re-send everything.
        return Err(UploadError::Incomplete((0..total_chunks).collect()));
    }

    let final_path = upload_path(data_dir, upload_id, "lyra")?;
    tokio::fs::rename(&part, &final_path).await?;
    map.remove(upload_id);
    save(kv, user_id, &map).await?;
    Ok(final_path)
}

/// Delete every `upload-*.part` / `upload-*.lyra` staging file whose mtime
/// is older than [`STALE_UPLOAD_AGE`] relative to `now` — abandoned chunked
/// uploads and leftovers from crashed imports. The kv registry entry
/// expires after 24 h on its own; this reclaims the (up to 4 GiB) file.
/// `now` is a parameter so tests can sweep with a future instant. Returns
/// the number of files removed.
pub async fn sweep_stale_uploads(data_dir: &Path, now: SystemTime) -> Result<u64, BackupError> {
    let dir = staging_dir(data_dir);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut removed = 0u64;
    while let Some(entry) = entries.next_entry().await? {
        // Only upload staging FILES; export staging dirs and temp zips are
        // owned (and cleaned) by the export job.
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let is_upload = name.starts_with("upload-")
            && Path::new(&name).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("part") || ext.eq_ignore_ascii_case("lyra")
            });
        if !is_upload {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|mtime| now.duration_since(mtime).ok())
            .is_some_and(|age| age >= STALE_UPLOAD_AGE);
        if !stale {
            continue;
        }
        match tokio::fs::remove_file(entry.path()).await {
            Ok(()) => {
                removed += 1;
                tracing::info!(file = %name, "swept stale upload staging file");
            }
            Err(e) => {
                tracing::warn!(file = %name, error = %e, "stale upload sweep failed to remove file");
            }
        }
    }
    Ok(removed)
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

        // Non-final chunks must be full-size, so chunk 0 fills CHUNK_SIZE.
        put_chunk(&kv, dir.path(), "u1", &upload_id, 0, &vec![7u8; CHUNK_SIZE])
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

    /// A short NON-final chunk leaves a zero-filled hole in the file; the
    /// per-chunk size record must catch it even though the byte count and
    /// the file length both "reach" the final chunk.
    #[tokio::test]
    async fn finish_rejects_short_non_final_chunk() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let upload_id = start(&kv, dir.path(), "u1").await.unwrap();

        put_chunk(
            &kv,
            dir.path(),
            "u1",
            &upload_id,
            0,
            b"only-100-bytes-would-hole",
        )
        .await
        .unwrap();
        put_chunk(&kv, dir.path(), "u1", &upload_id, 1, b"final")
            .await
            .unwrap();

        let err = finish(&kv, dir.path(), "u1", &upload_id, 2)
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::Incomplete(bad) if bad == vec![0]));

        // Re-sending chunk 0 at full size fixes the upload.
        put_chunk(&kv, dir.path(), "u1", &upload_id, 0, &vec![3u8; CHUNK_SIZE])
            .await
            .unwrap();
        let final_path = finish(&kv, dir.path(), "u1", &upload_id, 2).await.unwrap();
        let assembled = tokio::fs::read(&final_path).await.unwrap();
        assert_eq!(assembled.len(), CHUNK_SIZE + 5);
        assert!(assembled[..CHUNK_SIZE].iter().all(|&b| b == 3));
        assert_eq!(&assembled[CHUNK_SIZE..], b"final");
    }

    /// A registry/file size mismatch (crash between write and kv save, or
    /// external truncation) is reported as incomplete, never imported.
    #[tokio::test]
    async fn finish_rejects_registry_file_size_mismatch() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let upload_id = start(&kv, dir.path(), "u1").await.unwrap();
        put_chunk(&kv, dir.path(), "u1", &upload_id, 0, b"final")
            .await
            .unwrap();
        // Truncate the file behind the registry's back.
        tokio::fs::write(upload_path(dir.path(), &upload_id, "part").unwrap(), b"")
            .await
            .unwrap();

        let err = finish(&kv, dir.path(), "u1", &upload_id, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, UploadError::Incomplete(bad) if bad == vec![0]));
    }

    #[tokio::test]
    async fn sweep_removes_only_stale_upload_files() {
        let kv = kv();
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();

        let stale_part = staging_dir(dir.path()).join(format!("upload-{}.part", Uuid::now_v7()));
        let stale_lyra = staging_dir(dir.path()).join(format!("upload-{}.lyra", Uuid::now_v7()));
        let export_zip = staging_dir(dir.path()).join("some-job.zip");
        tokio::fs::create_dir_all(staging_dir(dir.path()))
            .await
            .unwrap();
        tokio::fs::write(&stale_part, b"old").await.unwrap();
        tokio::fs::write(&stale_lyra, b"old").await.unwrap();
        tokio::fs::write(&export_zip, b"export-owned")
            .await
            .unwrap();

        // Everything is fresh relative to real now: nothing is swept.
        assert_eq!(sweep_stale_uploads(dir.path(), now).await.unwrap(), 0);
        assert!(stale_part.exists());

        // 48 h later: both stale upload files are gone; the export-owned
        // zip (name does not match `upload-*`) survives.
        let later = now + Duration::from_hours(48);
        assert_eq!(sweep_stale_uploads(dir.path(), later).await.unwrap(), 2);
        assert!(!stale_part.exists());
        assert!(!stale_lyra.exists());
        assert!(export_zip.exists());

        // A genuinely fresh upload started after the sweep is untouched.
        let fresh_id = start(&kv, dir.path(), "u1").await.unwrap();
        assert_eq!(
            sweep_stale_uploads(dir.path(), SystemTime::now())
                .await
                .unwrap(),
            0
        );
        assert!(upload_path(dir.path(), &fresh_id, "part").unwrap().exists());

        // A missing staging dir is not an error.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(sweep_stale_uploads(empty.path(), later).await.unwrap(), 0);
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
        assert!(map[&upload_id].chunk_sizes.is_empty());
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
