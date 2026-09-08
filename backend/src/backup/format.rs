//! Archive layout: manifest, mbox escaping, per-folder sidecar lines.
//! Spec: docs/superpowers/specs/2026-09-08-lyra-backup-export-import-design.md §3.

use serde::{Deserialize, Serialize};
use std::io::Write;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub app: String,
    pub app_version: String,
    pub created_at: String,
    pub sections: Sections,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sections {
    pub settings: bool,
    pub accounts: u32,
    pub messages: u64,
    pub contacts: u32,
    pub calendars: u32,
    pub blobs: u64,
}

/// One `.meta.jsonl` line: everything mbox cannot carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaLine {
    pub message_id: Option<String>,
    pub flags: Vec<String>, // "seen" | "flagged"
    pub date: Option<String>,
    pub sha256: String,
    #[serde(default)]
    pub reconstructed: bool,
}

/// Append one message to an mbox stream (RFC 4155): separator line,
/// `>From `-escaped body, trailing CRLF. When the body does not end in
/// `\n`, the trailing CRLF terminates the last line rather than adding a
/// blank separator line — import must strip exactly one trailing CRLF per
/// message.
pub fn write_mbox_message(
    out: &mut impl Write,
    from_addr: &str,
    epoch: i64,
    raw: &[u8],
) -> std::io::Result<()> {
    write!(out, "From {from_addr} {epoch}\r\n")?;
    // Escape any line starting with "From " (and keep existing '>' chains).
    for (i, line) in raw.split(|b| *b == b'\n').enumerate() {
        if i > 0 {
            out.write_all(b"\n")?;
        }
        if line.starts_with(b"From ") || line.starts_with(b">") {
            out.write_all(b">")?;
        }
        out.write_all(line)?;
    }
    out.write_all(b"\r\n")?;
    Ok(())
}

/// Reverse of the escaping in `write_mbox_message`, used by import.
pub fn unescape_mbox_line(line: &[u8]) -> &[u8] {
    if let Some(rest) = line.strip_prefix(b">") {
        rest
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbox_escapes_from_lines() {
        let raw = b"Subject: x\r\n\r\nFrom nowhere\r\n>From quoted\r\n";
        let mut out = Vec::new();
        write_mbox_message(&mut out, "a@b.com", 1_700_000_000, raw).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("From a@b.com 1700000000\r\n"));
        assert!(text.contains("\r\n>From nowhere\r\n"));
        assert!(text.contains("\r\n>>From quoted\r\n"));
        assert!(text.ends_with("\r\n"));
    }

    #[test]
    fn unescape_strips_exactly_one_angle() {
        assert_eq!(unescape_mbox_line(b">>From x"), b">From x");
        assert_eq!(unescape_mbox_line(b">From x"), b"From x");
        assert_eq!(unescape_mbox_line(b"plain line"), b"plain line");
        assert_eq!(unescape_mbox_line(b""), b"");
    }

    #[test]
    fn manifest_roundtrip() {
        let m = Manifest {
            format: 1,
            app: "lyra".into(),
            app_version: "0.1.0".into(),
            created_at: "2026-09-08T12:00:00Z".into(),
            sections: Sections {
                settings: true,
                accounts: 2,
                messages: 10,
                contacts: 3,
                calendars: 1,
                blobs: 4,
            },
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.format, 1);
        assert_eq!(back.sections.messages, 10);
    }

    #[test]
    fn meta_line_roundtrip() {
        let line = MetaLine {
            message_id: Some("<a@b>".into()),
            flags: vec!["seen".into()],
            date: Some("2026-09-08T12:00:00Z".into()),
            sha256: "ab".repeat(32),
            reconstructed: false,
        };
        let s = serde_json::to_string(&line).unwrap();
        let back: MetaLine = serde_json::from_str(&s).unwrap();
        assert_eq!(back.sha256, line.sha256);
    }
}
