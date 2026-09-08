//! Artifact registry in kv: `backup:artifacts:{user_id}` = JSON array of
//! [`ArtifactMeta`]. The artifact files themselves live at
//! `data_dir/backups/<id>.lyra`.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §5.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;

use crate::kv::KvStore;

use super::BackupError;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArtifactMeta {
    pub id: String,
    pub filename: String, // lyra-backup-YYYYMMDD-HHmmss.lyra
    pub size_bytes: u64,
    pub created_at: String, // RFC3339
}

fn key(user_id: &str) -> String {
    format!("backup:artifacts:{user_id}")
}

/// Every registered artifact of the user, oldest first. No TTL: the registry
/// lives as long as the artifacts themselves.
pub async fn list(kv: &Arc<dyn KvStore>, user_id: &str) -> Result<Vec<ArtifactMeta>, BackupError> {
    match kv.get(&key(user_id)).await {
        Ok(Some(raw)) => Ok(serde_json::from_str(&raw)?),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(BackupError::Internal(e.to_string())),
    }
}

pub async fn add(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    meta: ArtifactMeta,
) -> Result<(), BackupError> {
    let mut items = list(kv, user_id).await?;
    items.push(meta);
    let raw = serde_json::to_string(&items)?;
    kv.set(&key(user_id), &raw, None)
        .await
        .map_err(|e| BackupError::Internal(e.to_string()))
}

/// Remove one entry; returns it when present so callers can log/confirm.
pub async fn remove(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    id: &str,
) -> Result<Option<ArtifactMeta>, BackupError> {
    let mut items = list(kv, user_id).await?;
    let Some(pos) = items.iter().position(|m| m.id == id) else {
        return Ok(None);
    };
    let meta = items.remove(pos);
    let raw = serde_json::to_string(&items)?;
    kv.set(&key(user_id), &raw, None)
        .await
        .map_err(|e| BackupError::Internal(e.to_string()))?;
    Ok(Some(meta))
}

/// `data_dir/backups/<id>.lyra` — `id` must parse as a UUID, so registry ids
/// can never traverse out of the backups dir.
pub fn artifact_path(data_dir: &Path, id: &str) -> Result<PathBuf, BackupError> {
    let uuid = Uuid::parse_str(id)
        .map_err(|_| BackupError::Internal(format!("invalid artifact id: {id}")))?;
    Ok(data_dir.join("backups").join(format!("{uuid}.lyra")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::MemoryKv;

    fn kv() -> Arc<dyn KvStore> {
        Arc::new(MemoryKv::new())
    }

    fn meta(id: &str, size: u64) -> ArtifactMeta {
        ArtifactMeta {
            id: id.into(),
            filename: format!("lyra-backup-20260909-120000-{id}.lyra"),
            size_bytes: size,
            created_at: "2026-09-09T12:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn add_list_remove_roundtrip() {
        let kv = kv();
        assert!(list(&kv, "u1").await.unwrap().is_empty());

        add(&kv, "u1", meta("a", 10)).await.unwrap();
        add(&kv, "u1", meta("b", 20)).await.unwrap();
        // Another user's registry is independent.
        add(&kv, "u2", meta("c", 30)).await.unwrap();

        let items = list(&kv, "u1").await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "a");
        assert_eq!(items[1].size_bytes, 20);

        let removed = remove(&kv, "u1", "a").await.unwrap().unwrap();
        assert_eq!(removed.id, "a");
        let items = list(&kv, "u1").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "b");

        // Removing an unknown id is a no-op.
        assert!(remove(&kv, "u1", "nope").await.unwrap().is_none());
        assert_eq!(list(&kv, "u1").await.unwrap().len(), 1);
        // u2 untouched.
        assert_eq!(list(&kv, "u2").await.unwrap().len(), 1);
    }

    #[test]
    fn artifact_path_rejects_traversal() {
        let dir = Path::new("/data");
        assert!(artifact_path(dir, "../etc").is_err());
        assert!(artifact_path(dir, "not-a-uuid").is_err());
        assert!(artifact_path(dir, "").is_err());
        let ok = artifact_path(dir, "01994470-6f6a-7cc1-9f0a-2c8c1d70f8a5").unwrap();
        assert_eq!(
            ok,
            dir.join("backups")
                .join("01994470-6f6a-7cc1-9f0a-2c8c1d70f8a5.lyra")
        );
    }
}
