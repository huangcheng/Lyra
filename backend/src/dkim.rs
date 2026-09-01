//! DKIM verification (RFC 6376) at view time.
//!
//! Sync never holds raw RFC 822 bytes (metadata-only IMAP, parsed JMAP
//! bodies), so verification runs where bytes appear: the IMAP body-fill hook
//! and the lazy on-open refetch. The verdict model and the best-signature
//! selection policy are pure and unit-tested; mail-auth does crypto + DNS.
#![allow(dead_code)] // seam module; callers land with the body-fill hook task

use mail_auth::{AuthenticatedMessage, DkimResult, MessageAuthenticator};

/// Stored verdict for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DkimVerdict {
    pub status: DkimStatus,
    pub sdid: Option<String>,
    pub auid: Option<String>,
    pub selector: Option<String>,
    pub algorithm: Option<String>,
    pub signed_headers: Vec<String>,
    pub warnings: Vec<String>,
    pub signed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DkimStatus {
    Pass,
    Fail,
    None,
    TempError,
}

impl DkimStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::None => "none",
            Self::TempError => "temperror",
        }
    }
}

/// One evaluated signature, flattened off mail-auth's types so the selection
/// policy stays pure and testable without DNS.
#[derive(Debug, Clone)]
pub(crate) struct SigOutcome {
    pub status: DkimStatus,
    pub sdid: Option<String>,
    pub auid: Option<String>,
    pub selector: Option<String>,
    pub algorithm: Option<String>,
    pub signed_headers: Vec<String>,
    pub signed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Headers Thunderbird-style verifiers warn about when unsigned.
const EXPECTED_SIGNED: &[&str] = &["from", "to", "subject", "date"];

/// d= aligns with the From domain when equal or a subdomain (relaxed
/// alignment, RFC 7489 §3.1.1 — the client-side analog).
fn aligns(sdid: &str, from_domain: &str) -> bool {
    sdid == from_domain || sdid.ends_with(&format!(".{from_domain}"))
}

fn warnings_for(signed_headers: &[String]) -> Vec<String> {
    EXPECTED_SIGNED
        .iter()
        .filter(|h| !signed_headers.iter().any(|s| s.eq_ignore_ascii_case(h)))
        .map(|h| format!("Header '{}' is not signed", title_case(h)))
        .collect()
}

fn title_case(h: &str) -> String {
    let mut c = h.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Best of multiple signatures: aligned pass > any pass > first outcome.
pub(crate) fn select_best(outputs: &[SigOutcome], from_domain: &str) -> DkimVerdict {
    let from_domain = from_domain.to_ascii_lowercase();
    let pick = outputs
        .iter()
        .find(|o| {
            o.status == DkimStatus::Pass
                && o.sdid
                    .as_deref()
                    .is_some_and(|d| aligns(&d.to_ascii_lowercase(), &from_domain))
        })
        .or_else(|| outputs.iter().find(|o| o.status == DkimStatus::Pass))
        .or_else(|| outputs.first());
    match pick {
        None => DkimVerdict {
            status: DkimStatus::None,
            sdid: None,
            auid: None,
            selector: None,
            algorithm: None,
            signed_headers: Vec::new(),
            warnings: Vec::new(),
            signed_at: None,
            expires_at: None,
        },
        Some(o) => DkimVerdict {
            status: o.status,
            sdid: o.sdid.clone(),
            auid: o.auid.clone(),
            selector: o.selector.clone(),
            algorithm: o.algorithm.clone(),
            signed_headers: o.signed_headers.clone(),
            warnings: if o.status == DkimStatus::Pass {
                warnings_for(&o.signed_headers)
            } else {
                Vec::new()
            },
            signed_at: o.signed_at,
            expires_at: o.expires_at,
        },
    }
}

fn epoch(secs: u64) -> Option<chrono::DateTime<chrono::Utc>> {
    if secs == 0 {
        None
    } else {
        chrono::DateTime::from_timestamp(i64::try_from(secs).ok()?, 0)
    }
}

fn map_status(r: &DkimResult) -> DkimStatus {
    match r {
        DkimResult::Pass => DkimStatus::Pass,
        DkimResult::TempError(_) => DkimStatus::TempError,
        DkimResult::None => DkimStatus::None,
        // Fail / Neutral / PermError all mean "does not validate".
        _ => DkimStatus::Fail,
    }
}

fn flatten(o: &mail_auth::DkimOutput<'_>) -> SigOutcome {
    let sig = o.signature();
    SigOutcome {
        status: map_status(o.result()),
        sdid: sig.map(|s| s.d.clone()),
        auid: sig.map(|s| s.i.clone()).filter(|i| !i.is_empty()),
        selector: sig.map(|s| s.s.clone()),
        algorithm: sig.map(|s| format!("{:?}", s.a)),
        signed_headers: sig.map(|s| s.h.clone()).unwrap_or_default(),
        signed_at: sig.and_then(|s| epoch(s.t)),
        expires_at: sig.and_then(|s| epoch(s.x)),
    }
}

fn authenticator() -> Result<&'static MessageAuthenticator, DkimStatus> {
    static AUTH: std::sync::OnceLock<MessageAuthenticator> = std::sync::OnceLock::new();
    if let Some(a) = AUTH.get() {
        return Ok(a);
    }
    match MessageAuthenticator::new_system_conf() {
        Ok(a) => Ok(AUTH.get_or_init(|| a)),
        Err(e) => {
            tracing::warn!(error = %e, "DKIM: DNS resolver init failed");
            Err(DkimStatus::TempError)
        }
    }
}

/// Verify all DKIM signatures on a raw RFC 822 message. `from_domain` is the
/// lowercased domain of the message's primary From address.
///
/// Never fails the caller: unparseable messages yield `none`; DNS resolver
/// init failure yields `temperror`.
pub(crate) async fn verify_raw(raw: &[u8], from_domain: &str) -> DkimVerdict {
    let Ok(auth) = authenticator() else {
        return select_best(
            &[SigOutcome {
                status: DkimStatus::TempError,
                sdid: None,
                auid: None,
                selector: None,
                algorithm: None,
                signed_headers: Vec::new(),
                signed_at: None,
                expires_at: None,
            }],
            from_domain,
        );
    };
    let Some(msg) = AuthenticatedMessage::parse(raw) else {
        // Not parseable as RFC 5322 — treat as unsigned rather than broken.
        return select_best(&[], from_domain);
    };
    let outputs = auth.verify_dkim(&msg).await;
    let outcomes: Vec<SigOutcome> = outputs.iter().map(flatten).collect();
    select_best(&outcomes, from_domain)
}

#[cfg(test)]
fn tests_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig_outcome(status: DkimStatus, sdid: &str, selector: &str) -> SigOutcome {
        SigOutcome {
            status,
            sdid: Some(sdid.to_string()),
            auid: Some(format!("ops@{sdid}")),
            selector: Some(selector.to_string()),
            algorithm: Some("RsaSha256".to_string()),
            signed_headers: vec!["from".into(), "to".into(), "subject".into(), "date".into()],
            signed_at: None,
            expires_at: None,
        }
    }

    #[test]
    fn select_best_prefers_aligned_pass() {
        let outputs = vec![
            sig_outcome(DkimStatus::Pass, "lists.example.org", "sel1"),
            sig_outcome(DkimStatus::Pass, "example.com", "sel2"),
        ];
        let v = select_best(&outputs, "example.com");
        assert_eq!(v.status, DkimStatus::Pass);
        assert_eq!(v.sdid.as_deref(), Some("example.com"));
        assert_eq!(v.selector.as_deref(), Some("sel2"));
    }

    #[test]
    fn select_best_unaligned_pass_beats_fail() {
        let outputs = vec![
            sig_outcome(DkimStatus::Fail, "example.com", "sel1"),
            sig_outcome(DkimStatus::Pass, "bounces.example.org", "sel2"),
        ];
        let v = select_best(&outputs, "example.com");
        assert_eq!(v.status, DkimStatus::Pass);
        assert_eq!(v.sdid.as_deref(), Some("bounces.example.org"));
    }

    #[test]
    fn select_best_no_signatures_is_none() {
        let v = select_best(&[], "example.com");
        assert_eq!(v.status, DkimStatus::None);
        assert!(v.sdid.is_none());
    }

    #[test]
    fn select_best_temperror_when_only_temperror() {
        let outputs = vec![sig_outcome(DkimStatus::TempError, "example.com", "sel1")];
        let v = select_best(&outputs, "example.com");
        assert_eq!(v.status, DkimStatus::TempError);
    }

    #[test]
    fn warnings_flag_unsigned_common_headers() {
        let mut o = sig_outcome(DkimStatus::Pass, "example.com", "sel1");
        o.signed_headers = vec!["from".into(), "to".into(), "date".into()];
        let v = select_best(&[o], "example.com");
        assert_eq!(
            v.warnings,
            vec!["Header 'Subject' is not signed".to_string()]
        );
    }

    #[test]
    fn subdomain_signature_counts_as_aligned() {
        let outputs = vec![sig_outcome(DkimStatus::Pass, "mail.example.com", "s")];
        let v = select_best(&outputs, "example.com");
        assert_eq!(v.status, DkimStatus::Pass);
        assert_eq!(v.sdid.as_deref(), Some("mail.example.com"));
    }

    #[test]
    fn unsigned_raw_parses_to_none() {
        // No DNS involved: unsigned messages short-circuit before lookup.
        let raw = b"From: a@example.com\r\nTo: b@example.org\r\nSubject: hi\r\n\r\nbody\r\n";
        let v = tests_rt().block_on(verify_raw(raw, "example.com"));
        assert_eq!(v.status, DkimStatus::None);
    }
}
