//! IMAP receive plugin — wraps `run_imap_sync`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::kernel::{App, Plugin};
use crate::protocol::{ReceivePlugin, SyncCtx, SyncOutcome};
use crate::sync::SyncError;

pub struct ImapReceivePlugin;

impl Plugin for ImapReceivePlugin {
    fn name(&self) -> &'static str {
        "imap"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["storage"]
    }

    fn register(&self, app: &mut App) {
        app.register_receive(Arc::new(ImapReceivePlugin));
    }
}

#[async_trait]
impl ReceivePlugin for ImapReceivePlugin {
    fn id(&self) -> &'static str {
        "imap"
    }

    async fn sync_account(&self, ctx: &SyncCtx) -> Result<SyncOutcome, SyncError> {
        let db = super::storage().map_err(SyncError::Protocol)?;
        crate::sync::imap_sync_account(&db, &ctx.user_id, &ctx.account_id).await
    }

    fn capabilities(&self) -> crate::protocol::ReceiveCaps {
        crate::protocol::ReceiveCaps {
            folders: true,
            flags: true,
            push: true, // RFC 2177 IDLE when server advertises it (`imap_idle` supervisor)
            delete_on_fetch: false,
        }
    }
}
