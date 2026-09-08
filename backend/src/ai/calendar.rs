//! Calendar-from-email (roadmap P5, suggest-only): detect a meeting in a
//! message, propose an event, the user edits and confirms in a dialog,
//! creation goes through the existing CalDAV create seam. The LLM never
//! writes calendars.

use serde::Serialize;
use serde_json::Value;

use super::{AiError, SettingsView, load_settings};
use crate::auth::AuthState;

/// A proposed event, matching `CreateEventRequest` field-for-field so the
/// confirm dialog can post it unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSuggestion {
    pub summary: String,
    /// RFC3339 (timed) or `YYYY-MM-DD` (all-day) — the create seam's forms.
    pub dtstart: String,
    pub dtend: Option<String>,
    pub is_all_day: bool,
    pub location: Option<String>,
    pub description: Option<String>,
}

/// Normalize one datetime the model returned into the create seam's forms:
/// RFC3339-with-offset passes through, naive timestamps are treated as UTC,
/// date-only means all-day (returns `(ymd, None)`).
fn normalize_dt(raw: &str) -> Option<(String, Option<String>)> {
    let raw = raw.trim();
    if raw.len() == 10 {
        // Date-only: strict YYYY-MM-DD.
        let ok = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").is_ok();
        return ok.then(|| (raw.to_string(), None));
    }
    if chrono::DateTime::parse_from_rfc3339(raw).is_ok() {
        return Some((raw.to_string(), Some(raw.to_string())));
    }
    // Naive datetime → assume UTC (the dialog is editable).
    if chrono::DateTime::parse_from_rfc3339(&format!("{raw}Z")).is_ok() {
        return Some((format!("{raw}Z"), Some(format!("{raw}Z"))));
    }
    None
}

/// Parse the model's suggestion reply. Lenient about fencing/chatter, strict
/// about the start datetime and a non-empty summary.
pub fn parse_event_suggestion(reply: &str) -> Option<EventSuggestion> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let v: Value = serde_json::from_str(&reply[start..=end]).ok()?;
    let summary = v.get("summary").and_then(Value::as_str)?.trim().to_string();
    if summary.is_empty() {
        return None;
    }
    let raw_start = v.get("start").and_then(Value::as_str)?;
    let (dtstart, timed_start) = normalize_dt(raw_start)?;
    let is_all_day =
        timed_start.is_none() || v.get("allDay").and_then(Value::as_bool).unwrap_or(false);
    let dtend = match v.get("end").and_then(Value::as_str) {
        Some(raw_end) => {
            let (end_norm, timed_end) = normalize_dt(raw_end)?;
            // Keep the end only in matching form (timed↔timed, date↔date).
            (timed_end.is_some() == timed_start.is_some()).then_some(end_norm)
        }
        None => None,
    };
    Some(EventSuggestion {
        summary,
        dtstart,
        dtend,
        is_all_day,
        location: v
            .get("location")
            .and_then(Value::as_str)
            .map(str::to_string),
        description: v
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The extraction prompt; today + weekday anchors relative dates.
pub(crate) fn calendar_prompt(
    today: &str,
    weekday: &str,
    subject: &str,
    from: &str,
    body: &str,
) -> String {
    format!(
        "Today is {weekday}, {today}. Does this email contain a meeting, appointment, or deadline with a date/time? \
         If yes, propose a calendar event.\n\nFrom: {from}\nSubject: {subject}\n\n{body}\n\n\
         Reply with ONLY a JSON object: \
         {{\"summary\": \"short title\", \"start\": \"RFC3339 with timezone offset, or YYYY-MM-DD for all-day\", \
         \"end\": \"same format, optional\", \"allDay\": false, \"location\": \"optional\", \"description\": \"optional\"}}. \
         If there is no schedulable item, reply exactly: {{\"none\": true}}."
    )
}

/// The suggestion for one message. Gated on the `calendar` feature flag.
pub async fn suggest_event(
    state: &AuthState,
    user_id: &str,
    message_id: &str,
) -> Result<EventSuggestion, AiError> {
    let db = state.db();
    let settings = load_settings(db, user_id).await?;
    if !settings.enabled || !settings.features.calendar {
        return Err(AiError::FeatureDisabled);
    }
    let view = SettingsView::ready(&settings).ok_or(AiError::NotConfigured)?;
    let dek = crate::auth::AuthState::get_user_dek(db, user_id).await?;
    let key = view.decrypt_key(&dek)?;
    let row = crate::sync::queries::load_ai_message_context(db, user_id, message_id)
        .await
        .map_err(|e| AiError::InvalidInput(e.to_string()))?;
    let from = crate::spam::from_json_email(row.from_address.as_deref())
        .unwrap_or_else(|| "unknown".into());
    let body: String = row
        .body_text
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect();
    let now = chrono::Local::now();
    let prompt = calendar_prompt(
        &now.format("%Y-%m-%d").to_string(),
        &now.format("%A").to_string(),
        row.subject.as_deref().unwrap_or(""),
        &from,
        &body,
    );
    let reply = super::client::LlmClient::new(&view, &key)
        .complete(
            "You extract calendar events from emails. Output only the JSON object.",
            &prompt,
        )
        .await?;
    parse_event_suggestion(&reply)
        .ok_or_else(|| AiError::InvalidInput("no schedulable event found".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timed_event_with_offset() {
        let s = parse_event_suggestion(
            r#"{"summary":"项目评审会","start":"2026-09-11T14:00:00+08:00","end":"2026-09-11T15:00:00+08:00","allDay":false,"location":"会议室A","description":"过接口文档"}"#,
        )
        .unwrap();
        assert_eq!(s.summary, "项目评审会");
        assert_eq!(s.dtstart, "2026-09-11T14:00:00+08:00");
        assert_eq!(s.dtend.as_deref(), Some("2026-09-11T15:00:00+08:00"));
        assert!(!s.is_all_day);
        assert_eq!(s.location.as_deref(), Some("会议室A"));
    }

    #[test]
    fn date_only_becomes_all_day_without_end() {
        let s =
            parse_event_suggestion(r#"{"summary":"截止日","start":"2026-09-20","allDay":true}"#)
                .unwrap();
        assert_eq!(s.dtstart, "2026-09-20");
        assert!(s.is_all_day);
        assert_eq!(s.dtend, None);
    }

    #[test]
    fn naive_datetimes_are_treated_as_utc() {
        let s = parse_event_suggestion(
            r#"{"summary":"同步会","start":"2026-09-11T06:00:00","end":"2026-09-11T07:00:00"}"#,
        )
        .unwrap();
        assert_eq!(s.dtstart, "2026-09-11T06:00:00Z");
        assert_eq!(s.dtend.as_deref(), Some("2026-09-11T07:00:00Z"));
        assert!(!s.is_all_day);
    }

    #[test]
    fn mismatched_end_forms_are_dropped() {
        // Timed start + date-only end → end dropped, event kept.
        let s = parse_event_suggestion(
            r#"{"summary":"混合","start":"2026-09-11T06:00:00Z","end":"2026-09-12"}"#,
        )
        .unwrap();
        assert_eq!(s.dtend, None);
    }

    #[test]
    fn fenced_replies_parse_and_bad_ones_reject() {
        let s = parse_event_suggestion(
            "Here you go:\n```json\n{\"summary\":\"评审\",\"start\":\"2026-09-11\"}\n```",
        )
        .unwrap();
        assert!(s.is_all_day);
        assert!(parse_event_suggestion(r#"{"none": true}"#).is_none());
        assert!(parse_event_suggestion(r#"{"summary":"","start":"2026-09-11"}"#).is_none());
        assert!(
            parse_event_suggestion(r#"{"summary":"无时间","start":"下周吧"}"#).is_none(),
            "unparseable start rejects the whole suggestion"
        );
    }
}
