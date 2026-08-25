//! OpenGPG (OpenPGP wire format) — key store + management API.
//!
//! Library: **rPGP** (`pgp` crate) — pure Rust, MIT/Apache (opengpg-spec P1).
//! Migration: `0008_opengpg_keys` (0007 already used for folder role overrides).

mod http;
pub mod keys;
pub mod session;
pub mod store;

pub use http::routes;
pub use session::UnlockRing;
