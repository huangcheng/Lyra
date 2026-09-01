//! DKIM verification (RFC 6376) at view time.
//!
//! Sync never holds raw RFC 822 bytes (metadata-only IMAP, parsed JMAP
//! bodies), so verification runs where bytes appear: the IMAP body-fill hook
//! and the lazy on-open refetch. The verdict model and the best-signature
//! selection policy are pure and unit-tested; mail-auth does crypto + DNS.

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
                // Without a From domain nothing can align; `aligns` would
                // suffix-match any d= ending in "." against "".
                && !from_domain.is_empty()
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

/// Overall DNS+verify budget per message (spec: inline verification must be
/// time-bounded; expiry maps to a retriable temperror verdict).
const VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) fn authenticator() -> Result<&'static MessageAuthenticator, DkimStatus> {
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

/// Bare `temperror` verdict — no signature fields are known when the failure
/// is infrastructural (resolver init failed, DNS+verify timed out).
fn temperror_verdict(from_domain: &str) -> DkimVerdict {
    select_best(
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
    )
}

/// Verify all DKIM signatures on a raw RFC 822 message. `from_domain` is the
/// lowercased domain of the message's primary From address.
///
/// Never fails the caller: unparseable messages yield `none`; DNS resolver
/// init failure or exhaustion of [`VERIFY_TIMEOUT`] yields `temperror`.
pub(crate) async fn verify_raw(raw: &[u8], from_domain: &str) -> DkimVerdict {
    let Ok(auth) = authenticator() else {
        return temperror_verdict(from_domain);
    };
    let Some(msg) = AuthenticatedMessage::parse(raw) else {
        // Not parseable as RFC 5322 — treat as unsigned rather than broken.
        return select_best(&[], from_domain);
    };
    if let Ok(outputs) = tokio::time::timeout(VERIFY_TIMEOUT, auth.verify_dkim(&msg)).await {
        let outcomes: Vec<SigOutcome> = outputs.iter().map(flatten).collect();
        select_best(&outcomes, from_domain)
    } else {
        tracing::warn!("DKIM: verify exceeded time budget");
        temperror_verdict(from_domain)
    }
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

    #[test]
    fn empty_from_domain_never_aligns() {
        // d= "rogue." ends_with(".") — without the guard it would
        // false-align against an empty From domain and jump the queue.
        let outputs = vec![
            sig_outcome(DkimStatus::Pass, "first.example.com", "sel1"),
            sig_outcome(DkimStatus::Pass, "rogue.", "sel2"),
        ];
        let v = select_best(&outputs, "");
        assert_eq!(v.status, DkimStatus::Pass);
        assert_eq!(v.sdid.as_deref(), Some("first.example.com"));
    }

    #[test]
    fn timeout_budget_maps_to_temperror() {
        // A timeout test with a real resolver would be flaky, so the
        // elapsed-to-verdict mapping is factored into `temperror_verdict`
        // (shared by the resolver-init path) and covered here.
        let v = temperror_verdict("example.com");
        assert_eq!(v.status, DkimStatus::TempError);
        assert!(v.sdid.is_none());
        assert!(v.selector.is_none());
    }

    // Real crypto/extraction path: sign with a fixed test-only RSA key
    // (generated once with `openssl genrsa -traditional 2048`; never used
    // anywhere else), then verify against a static TXT record served through
    // mail-auth's public `ResolverCache` seam — no network, no DNS fixture
    // process. This exercises `flatten`/`map_status` against real
    // `DkimOutput`s.
    mod signed {
        use super::*;
        use mail_auth::common::crypto::{RsaKey, Sha256};
        use mail_auth::common::parse::TxtRecordParser;
        use mail_auth::common::verify::DomainKey;
        use mail_auth::dkim::DkimSigner;
        use mail_auth::hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
        use mail_auth::{Parameters, ResolverCache, Txt};
        use rustls_pki_types::{PrivateKeyDer, PrivatePkcs1KeyDer, pem::PemObject};
        use std::borrow::Borrow;
        use std::hash::Hash;
        use std::net::{IpAddr, Ipv4Addr};
        use std::sync::Arc;

        /// Test-only fixture key (signs test messages only; never a real
        /// credential — see .gitleaks.toml allowlist).
        const TEST_RSA_PRIVATE_PEM: &str = include_str!("../testdata/dkim/test-rsa-key.pem");

        /// TXT record for `testsel._domainkey.example.com` (SPKI of the
        /// test-only key above).
        const TEST_DKIM_TXT: &str = concat!(
            "v=DKIM1; k=rsa; ",
            "p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEArnSRIyb6+zty/SR/vjDs",
            "qRCeqgwoA1Bsn5fNMaDMm+0zL5slW2QcXBTVu4F25nXvAPxOBEztsHYhMFvMGkd2",
            "wfKQCoIy2GskMZsoVEVGgkXAeuhSg4y2s1CWbpjC/o9LuCAV0neWG3UnXDZFn7kg",
            "TIZ5GLENJ4sduPPem+yfFklts8jdohHxHv9sy8uxPzVDYUMKszRPiUaqtHNEuo5O",
            "8CQ5hQIphj4eneeuHgSZMPUABjEIg1SuCu/F9Ts7KMYGLKRUor8Nx0qppaVHyE2s",
            "hBIe/2lhrKBuOBCZ48FgkiJAf5ALTT+jCcYPsJpXHPTAYKUoQaBSGbI82s8pa4a7",
            "6QIDAQAB",
        );

        const TEST_MESSAGE: &str = concat!(
            "From: bill@example.com\r\n",
            "To: jdoe@example.com\r\n",
            "Subject: TPS Report\r\n",
            "\r\n",
            "I'm going to need those TPS reports ASAP.\r\n",
        );

        /// Answers the one selector TXT query from memory; mail-auth checks
        /// the cache before any DNS, so the resolver is never contacted.
        struct StaticTxtCache {
            name: Box<str>,
            record: Txt,
        }

        impl ResolverCache<Box<str>, Txt> for StaticTxtCache {
            fn get<Q>(&self, name: &Q) -> Option<Txt>
            where
                Box<str>: Borrow<Q>,
                Q: Hash + Eq + ?Sized,
            {
                (Borrow::<Q>::borrow(&self.name) == name).then(|| self.record.clone())
            }

            fn remove<Q>(&self, _: &Q) -> Option<Txt>
            where
                Box<str>: Borrow<Q>,
                Q: Hash + Eq + ?Sized,
            {
                None
            }

            fn insert(&self, _: Box<str>, _: Txt, _: std::time::Instant) {}
        }

        fn sign(message: &str) -> Vec<u8> {
            let key = RsaKey::<Sha256>::from_key_der(PrivateKeyDer::Pkcs1(
                PrivatePkcs1KeyDer::from_pem_slice(TEST_RSA_PRIVATE_PEM.as_bytes())
                    .expect("test key PEM"),
            ))
            .expect("test key");
            let signature = DkimSigner::from_key(key)
                .domain("example.com")
                .selector("testsel")
                .headers(["From", "To", "Subject"])
                .sign(message.as_bytes())
                .expect("sign");
            let mut raw = Vec::new();
            signature.write(&mut raw, true);
            raw.extend_from_slice(message.as_bytes());
            raw
        }

        fn verify_fixture(raw: &[u8]) -> DkimVerdict {
            let parsed = AuthenticatedMessage::parse(raw).expect("parse");
            let cache = StaticTxtCache {
                name: "testsel._domainkey.example.com.".into(),
                record: Txt::DomainKey(Arc::new(
                    DomainKey::parse(TEST_DKIM_TXT.as_bytes()).expect("record"),
                )),
            };
            // Localhost resolver config: construction never touches system
            // DNS config, and the cache answers every lookup.
            let auth = MessageAuthenticator::new(
                ResolverConfig::from_parts(
                    None,
                    vec![],
                    vec![NameServerConfig::udp_and_tcp(IpAddr::V4(
                        Ipv4Addr::LOCALHOST,
                    ))],
                ),
                ResolverOpts::default(),
            )
            .expect("resolver");
            let outputs = tests_rt()
                .block_on(auth.verify_dkim(Parameters::new(&parsed).with_txt_cache(&cache)));
            let outcomes: Vec<SigOutcome> = outputs.iter().map(flatten).collect();
            select_best(&outcomes, "example.com")
        }

        #[test]
        fn signed_message_verifies_pass() {
            let v = verify_fixture(&sign(TEST_MESSAGE));
            assert_eq!(v.status, DkimStatus::Pass);
            assert_eq!(v.sdid.as_deref(), Some("example.com"));
            assert_eq!(v.selector.as_deref(), Some("testsel"));
            assert_eq!(v.algorithm.as_deref(), Some("RsaSha256"));
            for header in ["from", "to", "subject"] {
                assert!(
                    v.signed_headers
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(header)),
                    "signed_headers {header}: {:?}",
                    v.signed_headers
                );
            }
            assert_eq!(v.warnings, vec!["Header 'Date' is not signed"]);
            assert!(v.signed_at.is_some());
            assert!(v.expires_at.is_none());
        }

        #[test]
        fn tampered_body_fails() {
            let mut raw = sign(TEST_MESSAGE);
            raw.extend_from_slice(b"TAMPERED\r\n");
            let v = verify_fixture(&raw);
            assert_eq!(v.status, DkimStatus::Fail);
        }
    }
}
