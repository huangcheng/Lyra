//! JMAP receive plugin — wraps `run_jmap_sync`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::kernel::{App, Plugin};
use crate::protocol::{ReceivePlugin, SyncCtx, SyncOutcome};

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

    async fn sync_account(&self, ctx: &SyncCtx) -> Result<SyncOutcome, String> {
        let db = super::storage()?;
        crate::sync::jmap_sync_account(&db, &ctx.user_id, &ctx.account_id)
            .await
            .map_err(|e| e.to_string())
    }
}
