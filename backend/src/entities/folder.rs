//! `folder` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "folder")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub account_id: Uuid,
    /// Wire-encoded IMAP name or JMAP mailbox id.
    pub external_id: Option<String>,
    pub name: String,
    pub parent_id: Option<Uuid>,
    /// `inbox` | `sent` | `drafts` | `trash` | `spam` | `archive` | NULL.
    pub role: Option<String>,
    pub role_override: Option<String>,
    pub sort_order: i32,
    pub total_messages: i64,
    pub unread_messages: i64,
    pub sync_state: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
