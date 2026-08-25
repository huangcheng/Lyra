//! SMTP send plugin — wraps `deliver_smtp`.
//!
//! `raw` is either JSON-serialized [`OutboundMessage`](crate::smtp::OutboundMessage)
//! (compose / HTTP path) or a minimal RFC822-ish source (To/Subject headers + body).

use std::sync::Arc;

use async_trait::async_trait;

use crate::kernel::{App, Plugin};
use crate::protocol::SendPlugin;

pub struct SmtpSendPlugin;

impl Plugin for SmtpSendPlugin {
    fn name(&self) -> &'static str {
        "smtp"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["storage"]
    }

    fn register(&self, app: &mut App) {
        app.register_send(Arc::new(SmtpSendPlugin));
    }
}

#[async_trait]
impl SendPlugin for SmtpSendPlugin {
    fn id(&self) -> &'static str {
        "smtp"
    }

    async fn send(&self, account_id: &str, raw: &str) -> Result<(), String> {
        let db = super::storage()?;
        let (config, outbound) = crate::sync::prepare_smtp_send(&db, account_id, raw)
            .await
            .map_err(send_err_category)?;
        crate::sync::deliver_smtp(config, outbound)
            .await
            .map(|_| ())
            .map_err(send_err_category)
    }
}

/// Prefer SMTP job categories (transient/permanent) over raw error Display text.
fn send_err_category(err: crate::sync::SyncError) -> String {
    match err {
        crate::sync::SyncError::Smtp(smtp) => smtp.job_category().to_string(),
        other => other.to_string(),
    }
}
