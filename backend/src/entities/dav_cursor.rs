//! Per-account DAV sync cursor (RFC 6578 token), keyed `(account_id, kind)`.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "dav_cursor")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Text")]
    pub account_id: String,
    /// `carddav` | `caldav`
    #[sea_orm(primary_key, column_type = "Text")]
    pub kind: String,
    pub token: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
