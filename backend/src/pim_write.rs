//! VCARD / VEVENT builders for DAV writes (create contact / create event).
//!
//! Pure string assembly with RFC 5545 text escaping — unit-tested without
//! any network or database. The DAV PUT itself lives in `pim_dav`.

use chrono::Utc;

/// RFC 5545 §3.3.11 text escaping: backslash first, then `;` `,` and
/// newlines (folded to literal `\n`).
pub fn escape_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' | '\r' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

pub struct NewContact {
    pub display_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub organisation: Option<String>,
}

/// Minimal VCARD 4.0 for a new contact.
pub fn build_vcard(uid: &str, c: &NewContact) -> String {
    let mut lines = vec![
        "BEGIN:VCARD".to_string(),
        "VERSION:4.0".to_string(),
        format!("UID:{}", escape_text(uid)),
        format!("FN:{}", escape_text(&c.display_name)),
        format!("N:{}", escape_text(&c.display_name)),
    ];
    if let Some(email) = c.email.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("EMAIL:{}", escape_text(email.trim())));
    }
    if let Some(phone) = c.phone.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("TEL:{}", escape_text(phone.trim())));
    }
    if let Some(org) = c.organisation.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("ORG:{}", escape_text(org.trim())));
    }
    lines.push("END:VCARD".to_string());
    lines.join("\r\n")
}

pub struct NewEvent {
    pub summary: String,
    /// RFC3339 (timed) or `YYYY-MM-DD` (all-day).
    pub dtstart: String,
    pub dtend: Option<String>,
    pub is_all_day: bool,
    pub location: Option<String>,
    pub description: Option<String>,
}

fn ical_utc(rfc3339: &str) -> Result<String, String> {
    let dt = chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map_err(|_| format!("invalid dtstart/dtend '{rfc3339}': use RFC3339"))?;
    Ok(dt.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ").to_string())
}

fn ical_date(ymd: &str) -> Result<String, String> {
    // Strict YYYY-MM-DD (chrono's %m/%d accept single digits).
    if ymd.len() != 10
        || ymd.as_bytes().get(4) != Some(&b'-')
        || ymd.as_bytes().get(7) != Some(&b'-')
    {
        return Err(format!("invalid date '{ymd}': use YYYY-MM-DD"));
    }
    chrono::NaiveDate::parse_from_str(ymd, "%Y-%m-%d")
        .map_err(|_| format!("invalid date '{ymd}': use YYYY-MM-DD"))?;
    Ok(ymd.replace('-', ""))
}

/// Minimal VEVENT inside a VCALENDAR wrapper. All-day events use
/// VALUE=DATE with an exclusive DTEND; timed events are forced to UTC.
pub fn build_vevent(uid: &str, e: &NewEvent) -> Result<String, String> {
    if e.summary.trim().is_empty() {
        return Err("summary must not be empty".into());
    }
    let (dtstart, dtend) = if e.is_all_day {
        (
            format!("DTSTART;VALUE=DATE:{}", ical_date(&e.dtstart)?),
            match e.dtend.as_deref() {
                Some(d) => format!("\r\nDTEND;VALUE=DATE:{}", ical_date(d)?),
                None => String::new(),
            },
        )
    } else {
        (
            format!("DTSTART:{}", ical_utc(&e.dtstart)?),
            match e.dtend.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(d) => format!("\r\nDTEND:{}", ical_utc(d)?),
                None => String::new(),
            },
        )
    };
    let mut extra = String::new();
    if let Some(loc) = e.location.as_deref().filter(|s| !s.trim().is_empty()) {
        extra.push_str("\r\nLOCATION:");
        extra.push_str(&escape_text(loc.trim()));
    }
    if let Some(desc) = e.description.as_deref().filter(|s| !s.trim().is_empty()) {
        extra.push_str("\r\nDESCRIPTION:");
        extra.push_str(&escape_text(desc.trim()));
    }
    Ok(format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Lyra//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{}\r\n{dtstart}{dtend}{extra}\r\nDTSTAMP:{}\r\nEND:VEVENT\r\nEND:VCALENDAR",
        escape_text(e.summary.trim()),
        Utc::now().format("%Y%m%dT%H%M%SZ"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_rfc5545_text() {
        assert_eq!(escape_text("a,b;c\\d"), r"a\,b\;c\\d");
        assert_eq!(escape_text("line1\nline2"), r"line1\nline2");
    }

    #[test]
    fn builds_minimal_vcard() {
        let v = build_vcard(
            "u1",
            &NewContact {
                display_name: "张三;测试".into(),
                email: Some("z@example.com".into()),
                phone: None,
                organisation: Some("Acme, Inc".into()),
            },
        );
        assert!(v.starts_with("BEGIN:VCARD\r\nVERSION:4.0"));
        assert!(v.contains("FN:张三\\;测试"));
        assert!(v.contains("EMAIL:z@example.com"));
        assert!(v.contains("ORG:Acme\\, Inc"));
        assert!(v.ends_with("END:VCARD"));
        // round-trips through the sync parser's field extraction shape
        assert_eq!(v.matches("\r\n").count(), v.lines().count() - 1);
    }

    #[test]
    fn builds_timed_and_all_day_vevents() {
        let timed = build_vevent(
            "e1",
            &NewEvent {
                summary: "Standup, weekly".into(),
                dtstart: "2026-09-07T01:30:00+08:00".into(),
                dtend: Some("2026-09-07T02:00:00+08:00".into()),
                is_all_day: false,
                location: Some("Room 1".into()),
                description: None,
            },
        )
        .unwrap();
        assert!(timed.contains("SUMMARY:Standup\\, weekly"));
        assert!(timed.contains("DTSTART:20260906T173000Z"));
        assert!(timed.contains("DTEND:20260906T180000Z"));
        assert!(timed.contains("LOCATION:Room 1"));

        let allday = build_vevent(
            "e2",
            &NewEvent {
                summary: "休假".into(),
                dtstart: "2026-10-01".into(),
                dtend: Some("2026-10-08".into()),
                is_all_day: true,
                location: None,
                description: None,
            },
        )
        .unwrap();
        assert!(allday.contains("DTSTART;VALUE=DATE:20261001"));
        assert!(allday.contains("DTEND;VALUE=DATE:20261008"));
    }

    #[test]
    fn rejects_bad_inputs() {
        assert!(
            build_vevent(
                "e",
                &NewEvent {
                    summary: "  ".into(),
                    dtstart: "2026-09-07T01:00:00Z".into(),
                    dtend: None,
                    is_all_day: false,
                    location: None,
                    description: None
                }
            )
            .is_err()
        );
        assert!(
            build_vevent(
                "e",
                &NewEvent {
                    summary: "x".into(),
                    dtstart: "not-a-date".into(),
                    dtend: None,
                    is_all_day: false,
                    location: None,
                    description: None
                }
            )
            .is_err()
        );
        assert!(
            build_vevent(
                "e",
                &NewEvent {
                    summary: "x".into(),
                    dtstart: "2026-9-7".into(),
                    dtend: None,
                    is_all_day: true,
                    location: None,
                    description: None
                }
            )
            .is_err()
        );
    }
}
