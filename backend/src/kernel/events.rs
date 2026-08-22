#![allow(clippy::doc_markdown)]
#![allow(dead_code)]
#![allow(clippy::enum_variant_names)]

use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum AppEvent {
    SyncStarted { account_id: String },
    SyncComplete { account_id: String },
    SyncError { account_id: String, error: String },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AppEvent>,
}

impl EventBus {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx }
    }

    pub fn emit(&self, event: AppEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }
}
