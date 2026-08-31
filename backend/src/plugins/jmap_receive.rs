//! JMAP receive plugin — wraps `run_jmap_sync`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::kernel::{App, Plugin};
use crate::protocol::{ReceivePlugin, SyncCtx, SyncOutcome};
use crate::sync::SyncError;

pub struct JmapReceivePlugin;

impl Plugin for JmapReceivePlugin {
    fn name(&self) -> &'static str {
        "jmap"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["storage"]
    }

    fn register(&self, app: &mut App) {
        app.register_receive(Arc::new(JmapReceivePlugin));
    }
}

#[async_trait]
impl ReceivePlugin for JmapReceivePlugin {
    fn id(&self) -> &'static str {
        "jmap"
    }

    async fn sync_account(&self, ctx: &SyncCtx) -> Result<SyncOutcome, SyncError> {
        let db = super::storage().map_err(SyncError::Protocol)?;
        crate::sync::jmap_sync_account(&db, &ctx.user_id, &ctx.account_id).await
    }

    fn capabilities(&self) -> crate::protocol::ReceiveCaps {
        crate::protocol::ReceiveCaps {
            folders: true,
            flags: true,
            push: true, // EventSource when session provides URL (`jmap_push` supervisor)
            delete_on_fetch: false,
        }
    }
}
