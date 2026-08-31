//! Dual-DB repository seam — query normalization without a separate ORM.
//!
//! Lyra does not use a standalone repository crate. The pieces below form the
//! repository layer described in `docs/specs/2026-08-20-lyra-data-model-spec.md`
//! §1.1:
//!
//! | Layer | Module | Role |
//! |-------|--------|------|
//! | Pool + migrations | [`crate::storage`] | `DbPool`, `DbTxn`, migration runner |
//! | Dual-DB SQL macros | [`crate::db_sql`] | `db_fetch!`, `db_execute!`, … compile against SQLite and Postgres |
//! | ID / timestamp params | [`crate::db_row`] | `id_param`, `message_date_param`, row decoding |
//! | Sync persistence | [`crate::sync::store`] | Folder/message upserts, cursors, `load_account_sync_row` |
//! | Mail HTTP reads | [`crate::sync::http`] + [`crate::sync::queries`] | account-scoped message/folder handlers, read-side query builders |
//! | PIM cache reads | [`crate::pim`] | `contact`, `calendar`, `calendar_event` queries |
//!
//! Handlers stay thin: they call domain modules that accept `user_id` and use
//! the macros above so the same SQL runs on both engines. New queries should
//! follow this pattern rather than introducing a second abstraction.
