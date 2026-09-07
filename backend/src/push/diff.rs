//! New-mail diff for the push fan-out. Mirrors the frontend notifier
//! (`frontend/src/lib/notifications.ts`) exactly so open-app banners and
//! closed-app pushes agree on what counts as "new".

use crate::sync::queries::MessageResponse;

/// Folder roles that never count as incoming mail. Archive IS incoming.
const NON_INCOMING_ROLES: [&str; 6] = ["sent", "drafts", "trash", "spam", "junk", "outbox"];

const DIFF_LIMIT: usize = 15;

pub(crate) struct PushCandidate {
    pub(crate) id: String,
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
/// unchanged); anything else (non-array, empty array, unparseable) → the
/// raw string. `from_address` is a JSON array string (or bare).
fn sender_label(from_address: Option<&str>) -> String {
    let raw = from_address.unwrap_or("");
    if let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(first) = entries.first()
    {
        if let Some(entry) = first.as_str() {
            return entry.to_string();
        }
        return first
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| first.get("email").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
    }
    raw.to_string()
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
