//! `contact` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "contact")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub external_id: Option<String>,
    pub vcard_blob: Option<String>,
    pub display_name: Option<String>,
    pub email_addresses: Option<Json>,
    pub phone_numbers: Option<Json>,
    pub organisation: Option<String>,
    pub photo_path: Option<String>,
    pub addressbook_url: Option<String>,
    pub etag: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
