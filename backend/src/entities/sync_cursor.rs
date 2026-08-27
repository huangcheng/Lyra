//! `sync_cursor` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "sync_cursor")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub folder_id: Uuid,
    /// `jmap` | `imap`.
    pub protocol: String,
    /// `modseq` | `uidvalidity_uid` | `state_token`.
    pub cursor_type: String,
    pub cursor_value: String,
    pub updated_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
