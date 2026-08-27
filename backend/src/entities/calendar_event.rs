//! `calendar_event` entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "calendar_event")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub external_id: Option<String>,
    pub icalendar_blob: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
    pub location: Option<String>,
    pub is_all_day: bool,
    pub calendar_id: Option<Uuid>,
    pub calendar_url: Option<String>,
    pub etag: Option<String>,
    pub recurrence_rule: Option<String>,
    pub status: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
