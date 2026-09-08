//! Full-instance backup: age-encrypted zip export/import.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md

// Consumed by export/import (later tasks); remove once wired up.
#![allow(dead_code)]

pub mod artifacts;
pub mod crypto;
pub mod export;
pub mod format;
mod http;
pub mod import;
mod upload;

pub(crate) use http::routes;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("invalid backup password")]
    InvalidPassword,
    #[error("corrupt archive")]
    CorruptArchive,
    #[error("unsupported backup format")]
    UnsupportedFormat,
    #[error("upload incomplete")]
    UploadIncomplete,
    #[error("crypto: {0}")]
    Crypto(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("internal: {0}")]
    Internal(String),
}
