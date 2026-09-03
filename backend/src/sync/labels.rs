//! Message labels ↔ server keyword mapping (IMAP flags / JMAP keywords).
//!
//! Labels are lowercase slugs stored in `message.labels` (JSON array) and
//! mirrored best-effort onto the server as custom keywords (`label-<slug>`):
//! IMAP per-message flags (RFC 3501 STORE, arbitrary atom) and JMAP
//! `Email/set` keywords. Servers that reject custom keywords keep local
//! labels (Lyra remains the source of truth); readback happens once at
//! message ingest, so local edits are never overwritten by sync.

/// Keyword namespace prefix. Anything else (system flags, `$seen`,
/// Thunderbird's `$Label1`) is ignored.
pub(crate) const KEYWORD_PREFIX: &str = "label-";

/// Per-message cap; a mail row is not a tag garden.
pub(crate) const MAX_LABELS: usize = 8;
/// Slug length cap — IMAP keywords and JMAP keyword names are unbounded in
/// theory, but practical servers and UI chips want them short.
pub(crate) const MAX_LABEL_LEN: usize = 40;

/// Slugify one raw label: lowercase, `[a-z0-9-_]` runs joined by `-`.
/// Returns `None` when nothing usable remains (e.g. `"!!!"`).
pub(crate) fn sanitize_label(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = true; // suppress leading dashes
    for ch in raw.trim().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if ch.is_ascii_uppercase() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if (ch == '-' || ch == '_' || ch == ' ') && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    let slug = out.chars().take(MAX_LABEL_LEN).collect::<String>();
    let slug = slug.trim_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

/// Sanitize a request payload: dedupe (order-preserving), cap the count.
pub(crate) fn sanitize_labels(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for label in raw {
        let Some(slug) = sanitize_label(label) else {
            continue;
        };
        if !out.contains(&slug) {
            out.push(slug);
            if out.len() >= MAX_LABELS {
                break;
            }
        }
    }
    out
}

/// Keyword-set diff for a label replacement: `(add, remove)` keywords.
pub(crate) fn label_diff(old: &[String], next: &[String]) -> (Vec<String>, Vec<String>) {
    let add = next
        .iter()
        .filter(|l| !old.contains(l))
        .map(|l| label_keyword(l))
        .collect();
    let remove = old
        .iter()
        .filter(|l| !next.contains(l))
        .map(|l| label_keyword(l))
        .collect();
    (add, remove)
}

/// Server keyword for a sanitized label (`work` → `label-work`).
pub(crate) fn label_keyword(label: &str) -> String {
    format!("{KEYWORD_PREFIX}{label}")
}

/// Labels encoded as a JSON array text (`["work","travel"]`), `None` when
/// empty — the column stays NULL for the overwhelming majority of mail.
pub(crate) fn labels_json(labels: &[String]) -> Option<String> {
    (!labels.is_empty()).then(|| serde_json::to_string(labels).unwrap_or_else(|_| "[]".into()))
}

/// Derive labels from stored flags JSON. IMAP stores an array
/// (`["\\Seen","label-work"]`); JMAP stores a keywords object
/// (`{"$seen":true,"label-work":true}`). Both shapes are read; unknown
/// keywords are ignored.
pub(crate) fn labels_from_flags_json(flags_json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(flags_json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |flag: &str| {
        if let Some(label) = flag.strip_prefix(KEYWORD_PREFIX).filter(|l| !l.is_empty())
            && !out.iter().any(|l| l == label)
        {
            out.push(label.to_string());
        }
    };
    match &value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let serde_json::Value::String(flag) = item {
                    push(flag);
                }
            }
        }
        serde_json::Value::Object(map) => {
            for key in map.keys() {
                push(key);
            }
        }
        _ => {}
    }
    out.truncate(MAX_LABELS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_lowercases_and_slugifies() {
        assert_eq!(sanitize_label("Work Travel"), Some("work-travel".into()));
        assert_eq!(
            sanitize_label("  Projects_2026 "),
            Some("projects-2026".into())
        );
        assert_eq!(sanitize_label("Travel!!"), Some("travel".into()));
        assert_eq!(sanitize_label("---"), None);
        assert_eq!(sanitize_label("!!!"), None);
    }

    #[test]
    fn sanitize_caps_length() {
        let long = "a".repeat(80);
        let slug = sanitize_label(&long).unwrap();
        assert_eq!(slug.len(), MAX_LABEL_LEN);
    }

    #[test]
    fn sanitize_labels_dedupes_and_caps_count() {
        let raw: Vec<String> = ["Work", "work", "TRAVEL"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(sanitize_labels(&raw), vec!["work", "travel"]);
        let many: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        assert_eq!(sanitize_labels(&many).len(), MAX_LABELS);
    }

    #[test]
    fn keyword_roundtrip() {
        assert_eq!(label_keyword("work"), "label-work");
        let flags = format!(r#"["{}"]"#, label_keyword("work"));
        assert_eq!(labels_from_flags_json(&flags), vec!["work"]);
    }

    #[test]
    fn derives_from_imap_array_and_jmap_object() {
        let imap = r#"["\\Seen","label-work","label-travel"]"#;
        assert_eq!(labels_from_flags_json(imap), vec!["work", "travel"]);
        let jmap = r#"{"$seen":true,"label-work":true,"$flagged":true}"#;
        assert_eq!(labels_from_flags_json(jmap), vec!["work"]);
        assert!(labels_from_flags_json(r#"["\Seen","$Label1"]"#).is_empty());
        assert!(labels_from_flags_json("not json").is_empty());
    }

    #[test]
    fn label_diff_adds_and_removes() {
        let (add, remove) = label_diff(&["work".into()], &["travel".into(), "work".into()]);
        assert_eq!(add, vec!["label-travel"]);
        assert!(remove.is_empty());
        let (add, remove) = label_diff(&["work".into(), "old".into()], &[]);
        assert!(add.is_empty());
        assert_eq!(remove, vec!["label-work", "label-old"]);
    }

    #[test]
    fn labels_json_empty_is_none() {
        assert_eq!(labels_json(&[]), None);
        assert_eq!(
            labels_json(&["work".into()]),
            Some(r#"["work"]"#.to_string())
        );
    }
}
