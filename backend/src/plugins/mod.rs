//! Built-in protocol plugins (IMAP, JMAP, SMTP).

mod imap_receive;
mod jmap_receive;
mod smtp_send;

use std::sync::OnceLock;

use crate::kernel::Plugin;
use crate::storage::DbPool;

pub use imap_receive::ImapReceivePlugin;
pub use jmap_receive::JmapReceivePlugin;
pub use smtp_send::SmtpSendPlugin;

static STORAGE: OnceLock<DbPool> = OnceLock::new();

/// Bind the process-wide storage pool used by protocol plugins at sync/send time.
pub fn bind_storage(db: DbPool) {
    let _ = STORAGE.set(db);
}

pub(crate) fn storage() -> Result<DbPool, String> {
    STORAGE
        .get()
        .cloned()
        .ok_or_else(|| "storage not bound".into())
}

/// Built-in receive/send plugins registered at process boot.
#[must_use]
pub fn builtin_plugins() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(ImapReceivePlugin),
        Box::new(JmapReceivePlugin),
        Box::new(SmtpSendPlugin),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::App;

    #[test]
    fn builtin_registers_imap_jmap_smtp() {
        let mut app = App::new();
        app.provide("storage");
        for p in builtin_plugins() {
            app.register_plugin(p.as_ref()).unwrap();
        }
        assert!(app.receive("imap").is_ok());
        assert!(app.receive("jmap").is_ok());
        assert!(app.send("smtp").is_ok());
        assert!(app.receive("pop3").is_err());
    }
}
