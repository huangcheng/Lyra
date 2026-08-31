#![allow(clippy::doc_markdown)]
#![allow(dead_code)]
#![allow(clippy::zero_sized_map_values)]

use crate::kernel::events::EventBus;
use crate::kernel::plugin::Plugin;
use crate::protocol::{ReceiveHandle, SendHandle};
use crate::sync::SyncError;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("unknown receive protocol '{0}'")]
    UnknownReceive(String),
    #[error("unknown send protocol '{0}'")]
    UnknownSend(String),
    #[error("plugin '{plugin}' injects '{service}' which is not provided")]
    MissingInject { plugin: String, service: String },
}

pub struct App {
    provided: HashMap<&'static str, ()>,
    receive: HashMap<String, ReceiveHandle>,
    send: HashMap<String, SendHandle>,
    pub events: EventBus,
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self {
            provided: HashMap::new(),
            receive: HashMap::new(),
            send: HashMap::new(),
            events: EventBus::new(),
        }
    }

    pub fn provide(&mut self, name: &'static str) {
        self.provided.insert(name, ());
    }

    pub fn register_plugin(&mut self, plugin: &dyn Plugin) -> Result<(), KernelError> {
        for service in plugin.inject() {
            if !self.provided.contains_key(service) {
                return Err(KernelError::MissingInject {
                    plugin: plugin.name().into(),
                    service: (*service).into(),
                });
            }
        }
        plugin.register(self);
        Ok(())
    }

    pub fn register_receive(&mut self, plugin: ReceiveHandle) {
        self.receive.insert(plugin.id().into(), plugin);
    }

    pub fn receive(&self, id: &str) -> Result<ReceiveHandle, KernelError> {
        self.receive
            .get(id)
            .cloned()
            .ok_or_else(|| KernelError::UnknownReceive(id.into()))
    }

    pub fn register_send(&mut self, plugin: SendHandle) {
        self.send.insert(plugin.id().into(), plugin);
    }

    pub fn send(&self, id: &str) -> Result<SendHandle, KernelError> {
        self.send
            .get(id)
            .cloned()
            .ok_or_else(|| KernelError::UnknownSend(id.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::Plugin;
    use crate::protocol::{ReceiveCaps, ReceivePlugin, SyncCtx, SyncOutcome};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FakeImap;

    impl Plugin for FakeImap {
        fn name(&self) -> &'static str {
            "imap"
        }
        fn inject(&self) -> &'static [&'static str] {
            &["storage"]
        }
        fn register(&self, app: &mut App) {
            app.register_receive(Arc::new(FakeImapReceive));
        }
    }

    struct FakeImapReceive;

    #[async_trait]
    impl ReceivePlugin for FakeImapReceive {
        fn id(&self) -> &'static str {
            "imap"
        }
        fn capabilities(&self) -> ReceiveCaps {
            ReceiveCaps {
                folders: true,
                flags: true,
                push: false,
                delete_on_fetch: false,
            }
        }
        async fn sync_account(&self, ctx: &SyncCtx) -> Result<SyncOutcome, SyncError> {
            assert_eq!(ctx.account_id, "acc-1");
            Ok(SyncOutcome {
                folders_synced: 1,
                messages_synced: 2,
            })
        }
    }

    #[tokio::test]
    async fn registers_receive_and_looks_up_by_id() {
        let mut app = App::new();
        app.provide("storage");
        app.register_plugin(&FakeImap).unwrap();
        let recv = app.receive("imap").expect("imap registered");
        let out = recv
            .sync_account(&SyncCtx {
                account_id: "acc-1".into(),
                user_id: "user-1".into(),
            })
            .await
            .unwrap();
        assert_eq!(out.messages_synced, 2);
    }

    #[test]
    fn unknown_receive_is_error() {
        let app = App::new();
        let err = app.receive("pop3").err().expect("expected UnknownReceive");
        assert!(matches!(err, KernelError::UnknownReceive(id) if id == "pop3"));
    }

    #[test]
    fn unknown_send_is_error() {
        let app = App::new();
        let err = app.send("graph").err().expect("expected UnknownSend");
        assert!(matches!(err, KernelError::UnknownSend(id) if id == "graph"));
    }

    #[test]
    fn missing_inject_fails_closed() {
        struct NeedsDb;
        impl Plugin for NeedsDb {
            fn name(&self) -> &'static str {
                "needs-db"
            }
            fn inject(&self) -> &'static [&'static str] {
                &["storage"]
            }
            fn register(&self, _app: &mut App) {}
        }
        let mut app = App::new();
        let err = app.register_plugin(&NeedsDb).unwrap_err();
        assert!(matches!(err, KernelError::MissingInject { plugin, service }
            if plugin == "needs-db" && service == "storage"));
    }
}
