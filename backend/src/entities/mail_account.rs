//! `mail_account` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "mail_account")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub user_id: Uuid,
    pub display_name: Option<String>,
    pub email_address: String,
    /// `jmap` | `imap`.
    pub protocol: String,
    /// `password` | `oauth2` | `app_password`.
    pub auth_type: String,
    /// DEK-encrypted credential blob.
    pub credential: String,
    pub imap_host: Option<String>,
    pub imap_port: Option<i32>,
    /// `tls` | `starttls` | `none`.
    pub imap_security: Option<String>,
    pub jmap_base_url: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_security: Option<String>,
    pub smtp_auth_type: Option<String>,
    pub smtp_credential: Option<String>,
    pub auto_config_source: Option<String>,
    /// Compose signature (plain text or simple HTML).
    pub signature: Option<String>,
    pub carddav_url: Option<String>,
    pub caldav_url: Option<String>,
    pub is_active: bool,
    pub sync_enabled: bool,
    pub last_sync_at: Option<DateTimeUtc>,
    pub receive_protocol: String,
    pub send_protocol: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
