//! Plugin kernel: App, inject, events.
//! See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md`.

#![allow(clippy::doc_markdown)]
#![allow(unused_imports)]

mod app;
mod events;
mod plugin;

pub use app::{App, KernelError};
pub use events::{AppEvent, EventBus};
pub use plugin::Plugin;
