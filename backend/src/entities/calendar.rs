//! `calendar` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "calendar")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub external_id: Option<String>,
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
    pub timezone: Option<String>,
    pub calendar_url: Option<String>,
    pub etag: Option<String>,
    pub is_active: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub sync_token: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
