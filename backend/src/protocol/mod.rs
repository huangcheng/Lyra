//! Protocol plugin traits (receive, send).
#![allow(clippy::doc_markdown)]
#![allow(dead_code)]
#![allow(clippy::struct_excessive_bools)]

use crate::sync::SyncError;
use async_trait::async_trait;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiveCaps {
    pub folders: bool,
    pub flags: bool,
    pub push: bool,
    pub delete_on_fetch: bool,
}

pub struct SyncCtx {
    pub account_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct SyncOutcome {
    pub folders_synced: u32,
    pub messages_synced: u32,
}

#[async_trait]
pub trait ReceivePlugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> ReceiveCaps {
        ReceiveCaps {
            folders: true,
            flags: true,
            ..ReceiveCaps::default()
        }
    }
    async fn sync_account(&self, ctx: &SyncCtx) -> Result<SyncOutcome, SyncError>;
}

pub type ReceiveHandle = Arc<dyn ReceivePlugin>;

#[async_trait]
pub trait SendPlugin: Send + Sync {
    fn id(&self) -> &'static str;
    async fn send(&self, account_id: &str, raw: &str) -> Result<(), String>;
}

pub type SendHandle = Arc<dyn SendPlugin>;
