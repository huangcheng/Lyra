//! `message` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "message")]
// Schema-mapped struct: bool columns mirror the DB 1:1.
#[allow(clippy::struct_excessive_bools)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub folder_id: Uuid,
    /// IMAP: `{folder_id}:{uid}`; JMAP: opaque email id.
    pub external_id: Option<String>,
    pub thread_id: Option<Uuid>,
    pub message_id_header: Option<String>,
    pub subject: Option<String>,
    pub from_address: Option<Json>,
    pub to_addresses: Option<Json>,
    pub cc_addresses: Option<Json>,
    pub bcc_addresses: Option<Json>,
    pub reply_to: Option<Json>,
    pub date: Option<DateTimeUtc>,
    pub received_at: Option<DateTimeUtc>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub body_blob_path: Option<String>,
    pub is_read: bool,
    pub is_starred: bool,
    pub is_draft: bool,
    pub is_deleted: bool,
    pub flags: Option<Json>,
    pub has_attachments: bool,
    pub size_bytes: Option<i64>,
    pub in_reply_to: Option<String>,
    pub references_headers: Option<String>,
    pub labels: Option<Json>,
    pub snippet: Option<String>,
    pub snoozed_until: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
