//! `jobs` entity (delayed-job queue).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "jobs")]
pub struct Model {
    /// TEXT in both dialects (UUIDv7, not a native UUID column).
    #[sea_orm(primary_key)]
    pub id: String,
    pub kind: String,
    /// RFC3339 TEXT; lexicographic ordering == chronological.
    pub run_at: String,
    pub payload: String,
    /// `pending` | `running` | `done` | `failed`.
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    /// Scrubbed cause chain for operator UI (never raw credentials).
    pub last_error_detail: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
