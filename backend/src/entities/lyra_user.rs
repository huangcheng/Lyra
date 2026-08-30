//! `lyra_user` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "lyra_user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    /// Encrypted; NULL while 2FA is disabled.
    pub totp_secret: Option<String>,
    pub totp_enabled: bool,
    pub display_name: Option<String>,
    pub locale: String,
    pub encrypted_dek: Option<String>,
    /// Bumped to invalidate every session for the user.
    pub sess_epoch: i32,
    /// Single-user guard (CHECK = 1).
    pub singleton: bool,
    /// `on_open` | `on_read`.
    pub mark_read_policy: String,
    /// UI view-state JSON blob (selected account/folder, …); NULL until set.
    pub ui_state: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
