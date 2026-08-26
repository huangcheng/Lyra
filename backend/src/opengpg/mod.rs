//! OpenGPG (OpenPGP wire format) — key store + management API.
//!
//! Library: **rPGP** (`pgp` crate) — pure Rust, MIT/Apache (opengpg-spec P1).
//! Migration: `0008_opengpg_keys` (0007 already used for folder role overrides).

mod http;
#[cfg(test)]
mod interop;
pub mod keys;
pub mod read;
pub mod send;
pub mod session;
pub mod store;

pub use http::routes;
pub use read::{OpengpgMessageStatus, enrich_message_opengpg};
pub use session::UnlockRing;
