//! `opengpg_key` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "opengpg_key")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub user_id: Uuid,
    /// Owning mail account for identity keys; NULL = shared contact/legacy key.
    pub account_id: Option<Uuid>,
    pub fingerprint: String,
    pub primary_email: String,
    pub emails: Json,
    pub is_secret: bool,
    pub is_primary: bool,
    pub revoked: bool,
    pub key_data: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
