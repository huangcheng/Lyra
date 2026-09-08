//! New-mail diff for the push fan-out. Mirrors the frontend notifier
//! (`frontend/src/lib/notifications.ts`) exactly so open-app banners and
//! closed-app pushes agree on what counts as "new".

use crate::sync::queries::MessageResponse;

/// Folder roles that never count as incoming mail. Archive IS incoming.
const NON_INCOMING_ROLES: [&str; 6] = ["sent", "drafts", "trash", "spam", "junk", "outbox"];

const DIFF_LIMIT: usize = 15;

pub(crate) struct PushCandidate {
    pub(crate) id: String,
    // Kept for test assertions on the diff semantics; the fan-out sends by
    // row `id`.
    #[allow(dead_code)]
    pub(crate) identity: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

pub(crate) struct DiffOutcome {
    /// Messages to notify for (mute-filtered). Empty when seeding.
    pub(crate) fresh: Vec<PushCandidate>,
    /// The baseline to persist (all incoming identities, mute-independent).
    pub(crate) new_baseline: Vec<String>,
    /// True when this run only seeded the baseline (no prior baseline).
    pub(crate) seeded: bool,
}

fn is_incoming(role: Option<&str>) -> bool {
    !NON_INCOMING_ROLES.contains(&role.unwrap_or(""))
}

fn identity(m: &MessageResponse) -> String {
    // Matches the frontend's `msg.messageIdHeader || msg.id`: an empty
    // header string is falsy and falls back to the row id.
    m.message_id_header
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| m.id.clone())
}

/// Display label for the sender, mirroring the frontend `senderLabel`:
/// JSON array → first entry (a bare string is returned as-is; an object
/// yields `name ?? email ?? ''`, where an empty-string name is returned
/// unchanged); a JSON object is the persist layer's `{"raw": "Name
/// <email>"}` shape → the display name, else the bare address; anything
/// else (empty array, unparseable) → the raw string.
fn sender_label(from_address: Option<&str>) -> String {
    let raw = from_address.unwrap_or("");
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Array(entries)) => {
            let Some(first) = entries.first() else {
                return raw.to_string();
            };
            if let Some(entry) = first.as_str() {
                return entry.to_string();
            }
            first
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| first.get("email").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string()
        }
        Ok(serde_json::Value::Object(obj)) => {
            let text = obj
                .get("raw")
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("email").and_then(|v| v.as_str()))
                .unwrap_or("");
            display_from_address(text)
        }
        _ => raw.to_string(),
    }
}

/// `"Name <email>"` → `Name`; anything else is returned unchanged.
/// Mirrors the frontend `parseOneAddress` + label choice (`name ?? email`).
fn display_from_address(text: &str) -> String {
    if let Some(start) = text.rfind('<')
        && text.ends_with('>')
    {
        let name = text[..start].trim();
        if !name.is_empty() {
            return name.to_string();
        }
        return text[start + 1..text.len() - 1].trim().to_string();
    }
    text.to_string()
}

/// Diff the account's newest messages (already newest-first) against the
/// stored baseline. `muted_folders`/`muted_threads` gate sends only.
pub(crate) fn diff_new_messages(
    messages: &[MessageResponse],
    baseline: &[String],
    muted_folders: &[String],
    muted_threads: &[String],
) -> DiffOutcome {
    let incoming: Vec<&MessageResponse> = messages
        .iter()
        .filter(|m| is_incoming(m.folder_role.as_deref()))
        .take(DIFF_LIMIT)
        .collect();
    let new_baseline: Vec<String> = incoming.iter().map(|m| identity(m)).collect();
    if baseline.is_empty() {
        return DiffOutcome {
            fresh: Vec::new(),
            new_baseline,
            seeded: true,
        };
    }
    let known: std::collections::HashSet<&str> = baseline.iter().map(String::as_str).collect();
    let fresh = incoming
        .iter()
        .filter(|m| !known.contains(identity(m).as_str()))
        .filter(|m| !muted_folders.contains(&m.folder_id))
        .filter(|m| {
            m.thread_id
                .as_ref()
                .is_none_or(|t| !muted_threads.contains(t))
        })
        .map(|m| PushCandidate {
            id: m.id.clone(),
            identity: identity(m),
            title: sender_label(m.from_address.as_deref()),
            body: m.subject.clone().unwrap_or_default(),
        })
        .collect();
    DiffOutcome {
        fresh,
        new_baseline,
        seeded: false,
    }
}

#[cfg(test)]
mod tests {
    use super::sender_label;

    #[test]
    fn sender_label_unwraps_raw_object() {
        // The persist layer stores `from` as `{"raw": "Name <email>"}` for
        // both IMAP and JMAP — the label must never leak the JSON wrapper.
        assert_eq!(
            sender_label(Some(r#"{"raw":"QQ邮箱管理员 <10000@qq.com>"}"#)),
            "QQ邮箱管理员"
        );
        assert_eq!(
            sender_label(Some(r#"{"raw":"10000@qq.com"}"#)),
            "10000@qq.com"
        );
    }

    #[test]
    fn sender_label_array_and_bare_forms() {
        assert_eq!(
            sender_label(Some(
                r#"[{"name":"Ada Lovelace","email":"ada@example.com"}]"#
            )),
            "Ada Lovelace"
        );
        assert_eq!(
            sender_label(Some(r#"[{"email":"ada@example.com"}]"#)),
            "ada@example.com"
        );
        assert_eq!(sender_label(Some("grace@example.com")), "grace@example.com");
        assert_eq!(sender_label(Some("")), "");
        assert_eq!(sender_label(None), "");
    }
}
