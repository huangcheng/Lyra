//! JMAP EmailSubmission send plugin — wraps `deliver_jmap`.
//!
//! Used when `mail_account.send_protocol = "jmap"` (session advertised
//! `urn:ietf:params:jmap:submission` at account setup).

use std::sync::Arc;

use async_trait::async_trait;

use crate::kernel::{App, Plugin};
use crate::protocol::SendPlugin;

pub struct JmapSendPlugin;

impl Plugin for JmapSendPlugin {
    fn name(&self) -> &'static str {
        "jmap"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["storage"]
    }

    fn register(&self, app: &mut App) {
        app.register_send(Arc::new(JmapSendPlugin));
    }
}

#[async_trait]
impl SendPlugin for JmapSendPlugin {
    fn id(&self) -> &'static str {
        "jmap"
    }

    async fn send(&self, account_id: &str, raw: &str) -> Result<(), String> {
        let db = super::storage()?;
        let (base_url, email, password, outbound) =
            crate::sync::prepare_jmap_send(&db, account_id, raw)
                .await
                .map_err(jmap_send_err)?;
        crate::sync::deliver_jmap(&base_url, &email, &password, outbound)
            .await
            .map(|_| ())
            .map_err(jmap_send_err)
    }
}

fn jmap_send_err(err: crate::sync::SyncError) -> String {
    match err {
        crate::sync::SyncError::Jmap(jmap) => match &jmap {
            crate::jmap::JmapError::Http(_) => "JMAP transient".into(),
            other => format!("JMAP permanent: {other}"),
        },
        other => other.to_string(),
    }
}
