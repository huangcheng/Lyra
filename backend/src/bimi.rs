//! BIMI VMC (Verified Mark Certificate) validation.
//!
//! A BIMI `a=` (authority) URL points at a Mark Verifying Authority evidence
//! document: a PEM bundle holding the VMC leaf plus any intermediates. Before
//! Lyra trusts the `l=` logo, the leaf must satisfy, for the From domain:
//!
//! 1. chain to an embedded MVA root (`roots`, DigiCert + Entrust),
//! 2. be inside its validity window,
//! 3. bind the domain exactly — a SAN dNSName (fallback: subject CN) equal,
//!    case-insensitively, to the From domain. Neither direction of the
//!    parent/subdomain relation qualifies: a VMC for `example.com` does not
//!    vouch for `mail.example.com`, nor the reverse,
//! 4. carry the logotype extension OID 1.3.6.1.5.5.7.1.12 (RFC 3709;
//!    presence is the gate, the payload is never parsed),
//! 5. not be revoked: for every CRL distribution point (cDP, OID
//!    2.5.29.31) on the leaf and intermediates, fetch the CRL, require it to
//!    be signed by the issuing CA and unexpired, and reject if the cert's
//!    serial is listed.
//!
//! Steps 1-2 run through rustls-webpki, 3-5 through x509-parser. CRL
//! fetch/parse/signature/freshness failures are soft (logged + accepted):
//! only a positively verified CRL listing the serial rejects. This matches
//! BIMI-receiver guidance that revocation data is best-effort, and keeps a
//! MVA-side CRL outage from blanking legitimate logos — while a forged or
//! stripped CRL can never *prove* revocation, which is the safe direction.

mod roots;

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use rustls_pki_types::{CertificateDer, TrustAnchor, UnixTime};
use webpki::{EndEntityCert, ExtendedKeyUsageValidator, KeyPurposeIdIter};
use x509_parser::asn1_rs::{FromDer, oid};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::{DistributionPointName, GeneralName, ParsedExtension};
use x509_parser::oid_registry::OID_X509_EXT_CRL_DISTRIBUTION_POINTS;
use x509_parser::revocation_list::CertificateRevocationList;
use x509_parser::x509::SubjectPublicKeyInfo;

/// Logotype extensions (RFC 3709 id-pe-logotype): required on a VMC.
static OID_LOGOTYPE: x509_parser::asn1_rs::Oid<'static> = oid!(1.3.6.1.5.5.7.1.12);

/// Cap on a fetched CRL body (CRLs are small; 1 MiB is generous).
const MAX_CRL_BYTES: u64 = 1024 * 1024;

/// Why a VMC evidence document failed validation. Specifics (the webpki /
/// parser detail strings) are logged server-side only — callers collapse
/// every variant to a silent miss.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VmcError {
    /// PEM/DER could not be parsed, or no certificates in the bundle.
    Malformed(String),
    /// No chain from the leaf to a trusted MVA anchor (bad signature,
    /// unknown issuer, policy/constraint violation, …).
    Chain(String),
    /// Leaf (or a chain cert) is expired or not yet valid.
    Expired,
    /// No SAN dNSName / subject CN on the leaf equals the From domain.
    DomainMismatch,
    /// Logotype extension OID 1.3.6.1.5.5.7.1.12 absent from the leaf.
    NoLogotype,
    /// An issuer-signed, fresh CRL lists the cert's serial.
    Revoked,
}

impl fmt::Display for VmcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(d) => write!(f, "malformed VMC evidence: {d}"),
            Self::Chain(d) => write!(f, "VMC chain validation failed: {d}"),
            Self::Expired => write!(f, "VMC outside its validity window"),
            Self::DomainMismatch => write!(f, "VMC does not name the From domain"),
            Self::NoLogotype => write!(f, "VMC lacks the logotype extension"),
            Self::Revoked => write!(f, "VMC revoked by its issuer's CRL"),
        }
    }
}

impl std::error::Error for VmcError {}

/// VMCs are not TLS server certs and do not carry the serverAuth EKU; the
/// EKU extension is typically absent entirely. Accept any (or no) EKU but
/// still propagate malformed-EKU DER errors.
struct AnyEku;

impl ExtendedKeyUsageValidator for AnyEku {
    fn validate(&self, iter: KeyPurposeIdIter<'_, '_>) -> Result<(), webpki::Error> {
        for id in iter {
            id?;
        }
        Ok(())
    }
}

/// Production trust anchors: the embedded MVA roots.
fn mva_anchors() -> &'static [TrustAnchor<'static>] {
    static ANCHORS: OnceLock<Vec<TrustAnchor<'static>>> = OnceLock::new();
    ANCHORS.get_or_init(|| {
        [roots::DIGICERT_VMC_ROOT_PEM, roots::ENTRUST_VMC_ROOT_PEM]
            .iter()
            .filter_map(|pem| {
                let certs = parse_pem_certs(pem.as_bytes()).ok()?;
                let anchor = webpki::anchor_from_trusted_cert(certs.first()?)
                    .map_err(|e| {
                        tracing::error!(error = %e, "embedded MVA root failed to parse");
                    })
                    .ok()?;
                Some(anchor.to_owned())
            })
            .collect()
    })
}

/// Extract every `CERTIFICATE` PEM block as DER, in document order
/// (VMC bundles list the leaf first).
fn parse_pem_certs(bundle: &[u8]) -> Result<Vec<CertificateDer<'static>>, VmcError> {
    let mut certs = Vec::new();
    for block in x509_parser::pem::Pem::iter_from_buffer(bundle) {
        let pem = block.map_err(|e| VmcError::Malformed(format!("pem: {e}")))?;
        if pem.label == "CERTIFICATE" {
            certs.push(CertificateDer::from(pem.contents));
        }
    }
    if certs.is_empty() {
        return Err(VmcError::Malformed("no certificates in bundle".into()));
    }
    Ok(certs)
}

/// Domain names the cert speaks for: SAN dNSName entries, falling back to
/// the subject CN when the cert carries no dNSName at all (older VMCs).
fn cert_domain_names(cert: &X509Certificate<'_>) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            if let GeneralName::DNSName(dns) = name {
                names.push((*dns).to_string());
            }
        }
    }
    if names.is_empty() {
        for cn in cert.subject().iter_common_name() {
            if let Ok(value) = cn.attr_value().as_str() {
                names.push(value.to_string());
            }
        }
    }
    names
}

/// HTTP(S) CRL distribution point URLs carried by the cert (cDP OID
/// 2.5.29.31). LDAP/other schemes and name-relative DPs are ignored.
fn crl_urls(cert: &X509Certificate<'_>) -> Vec<String> {
    let mut urls = Vec::new();
    let Ok(Some(ext)) = cert.get_extension_unique(&OID_X509_EXT_CRL_DISTRIBUTION_POINTS) else {
        return urls;
    };
    if let ParsedExtension::CRLDistributionPoints(points) = ext.parsed_extension() {
        for point in points.iter() {
            let Some(DistributionPointName::FullName(names)) = &point.distribution_point else {
                continue;
            };
            for name in names {
                if let GeneralName::URI(uri) = name
                    && (uri.starts_with("http://") || uri.starts_with("https://"))
                {
                    urls.push((*uri).to_string());
                }
            }
        }
    }
    urls
}

/// Normalize CRL bytes to DER (PEM-armoured or bare DER input).
fn crl_der(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if let Ok((_, pem)) = x509_parser::pem::parse_x509_pem(bytes)
        && pem.label == "X509 CRL"
    {
        return std::borrow::Cow::Owned(pem.contents);
    }
    std::borrow::Cow::Borrowed(bytes)
}

/// DER *content* octets of a Name TLV: webpki trust anchors store only the
/// content of the subject SEQUENCE, while x509-parser's `X509Name::as_raw`
/// is the full TLV. Compare on content octets so the two agree.
fn name_content(name_tlv: &[u8]) -> Option<&[u8]> {
    let (_, any) = x509_parser::asn1_rs::Any::from_der(name_tlv).ok()?;
    Some(any.data)
}

/// Rebuild a SEQUENCE TLV around content octets (the inverse of what
/// webpki stored — see [`name_content`]).
fn sequence_tlv(content: &[u8]) -> Vec<u8> {
    let mut out = vec![0x30];
    let len = content.len();
    #[allow(clippy::cast_possible_truncation)] // DER lengths here are far under u32
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let be = (len as u32).to_be_bytes();
        let first = be.iter().position(|&b| b != 0).unwrap_or(be.len() - 1);
        let sig = &be[first..];
        out.push(0x80 | sig.len() as u8);
        out.extend_from_slice(sig);
    }
    out.extend_from_slice(content);
    out
}

/// DER-encoded SPKI of the CA that issued `cert`, found among the chain's
/// intermediates or the trust anchors by exact issuer-DN match. Post
/// chain-validation this cannot miss; `None` is treated as a chain error.
fn issuer_spki(
    cert: &X509Certificate<'_>,
    intermediates: &[X509Certificate<'_>],
    anchors: &[TrustAnchor<'_>],
) -> Option<Vec<u8>> {
    let issuer = name_content(cert.issuer().as_raw())?;
    for inter in intermediates {
        if name_content(inter.subject().as_raw()) == Some(issuer) {
            // x509-parser keeps the full SPKI TLV.
            return Some(inter.subject_pki.raw.to_vec());
        }
    }
    for anchor in anchors {
        if anchor.subject.as_ref() == issuer {
            return Some(sequence_tlv(anchor.subject_public_key_info.as_ref()));
        }
    }
    None
}

/// CRL revocation check for one certificate. Every failure short of a
/// positively verified listing is soft: log and treat as not-revoked.
fn check_revocation(
    cert: &X509Certificate<'_>,
    intermediates: &[X509Certificate<'_>],
    anchors: &[TrustAnchor<'_>],
    crl_fetch: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<(), VmcError> {
    let urls = crl_urls(cert);
    if urls.is_empty() {
        return Ok(());
    }
    let Some(spki_der) = issuer_spki(cert, intermediates, anchors) else {
        return Err(VmcError::Chain("issuer not in verified path".into()));
    };
    let Ok((_, spki)) = SubjectPublicKeyInfo::from_der(&spki_der) else {
        tracing::debug!("bimi vmc: issuer SPKI unparsable; skipping CRL check");
        return Ok(());
    };
    for url in urls {
        let Some(bytes) = crl_fetch(&url) else {
            tracing::debug!(%url, "bimi vmc: CRL unavailable; accepting");
            continue;
        };
        let der = crl_der(&bytes);
        let Ok((_, crl)) = CertificateRevocationList::from_der(&der) else {
            tracing::debug!(%url, "bimi vmc: CRL unparsable; accepting");
            continue;
        };
        if crl.issuer().as_raw() != cert.issuer().as_raw() {
            tracing::debug!(%url, "bimi vmc: CRL issuer mismatch; accepting");
            continue;
        }
        if let Err(e) = crl.verify_signature(&spki) {
            tracing::debug!(%url, error = %e, "bimi vmc: CRL signature invalid; accepting");
            continue;
        }
        let now = i64::try_from(UnixTime::now().as_secs()).unwrap_or(i64::MAX);
        if let Some(next_update) = crl.next_update()
            && next_update.timestamp() < now
        {
            tracing::debug!(%url, "bimi vmc: CRL stale; accepting");
            continue;
        }
        let serial = cert.raw_serial();
        if crl
            .iter_revoked_certificates()
            .any(|revoked| revoked.raw_serial() == serial)
        {
            tracing::info!(%url, "bimi vmc: certificate revoked by issuer CRL");
            return Err(VmcError::Revoked);
        }
    }
    Ok(())
}

/// Validate a VMC evidence document (PEM bundle: leaf + intermediates) for
/// `domain` against the embedded MVA roots. See the module docs for the
/// five checks.
pub(crate) async fn validate_vmc(pem_bundle: &[u8], domain: &str) -> Result<(), VmcError> {
    // CRL fetching needs the network; the validation core is synchronous so
    // tests can drive it with in-memory CRLs. Collect every cDP URL first,
    // fetch what we can (failures are soft — see module docs), then hand a
    // lookup closure to the core.
    let certs = parse_pem_certs(pem_bundle)?;
    let mut urls = Vec::new();
    for der in &certs {
        let (_, cert) = X509Certificate::from_der(der.as_ref())
            .map_err(|e| VmcError::Malformed(format!("x509: {e}")))?;
        urls.extend(crl_urls(&cert));
    }
    urls.sort();
    urls.dedup();

    let mut crls: HashMap<String, Vec<u8>> = HashMap::new();
    for url in urls {
        match crate::media::fetch_bytes(&url, MAX_CRL_BYTES).await {
            Ok(bytes) => {
                crls.insert(url, bytes);
            }
            Err(e) => {
                tracing::debug!(%url, error = %e, "bimi vmc: CRL fetch failed; accepting");
            }
        }
    }

    validate_vmc_with_anchors(pem_bundle, domain, mva_anchors(), &|url| {
        crls.get(url).cloned()
    })
}

/// Validation core with explicit trust anchors and a CRL-lookup seam —
/// production passes the embedded MVA roots and a network-backed map, tests
/// pass the fixture root and in-memory CRLs. Both run the identical checks.
pub(crate) fn validate_vmc_with_anchors(
    pem_bundle: &[u8],
    domain: &str,
    anchors: &[TrustAnchor<'_>],
    crl_fetch: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<(), VmcError> {
    let certs = parse_pem_certs(pem_bundle)?;
    let leaf_der = &certs[0];
    let intermediates_der = &certs[1..];

    // 1 + 2. Chain to a trusted anchor, within the validity window.
    // Revocation is handled manually below (webpki's built-in CRL support
    // hard-fails on unknown status; we need soft-fail semantics).
    let leaf =
        EndEntityCert::try_from(leaf_der).map_err(|e| VmcError::Malformed(format!("leaf: {e}")))?;
    leaf.verify_for_usage(
        webpki::ALL_VERIFICATION_ALGS,
        anchors,
        intermediates_der,
        UnixTime::now(),
        AnyEku,
        None,
        None,
    )
    .map_err(|e| match e {
        webpki::Error::CertNotValidYet { .. } | webpki::Error::CertExpired { .. } => {
            VmcError::Expired
        }
        other => VmcError::Chain(other.to_string()),
    })?;

    let (_, leaf_x509) = X509Certificate::from_der(leaf_der.as_ref())
        .map_err(|e| VmcError::Malformed(format!("leaf x509: {e}")))?;
    let mut intermediates = Vec::with_capacity(intermediates_der.len());
    for der in intermediates_der {
        let (_, cert) = X509Certificate::from_der(der.as_ref())
            .map_err(|e| VmcError::Malformed(format!("intermediate x509: {e}")))?;
        intermediates.push(cert);
    }

    // 3. Domain binding: exact, case-insensitive; no parent/subdomain
    // shortcuts (module docs, check 3).
    if !cert_domain_names(&leaf_x509)
        .iter()
        .any(|name| name.eq_ignore_ascii_case(domain))
    {
        return Err(VmcError::DomainMismatch);
    }

    // 4. Logotype extension present on the leaf.
    if !leaf_x509
        .extensions()
        .iter()
        .any(|ext| ext.oid == OID_LOGOTYPE)
    {
        return Err(VmcError::NoLogotype);
    }

    // 5. CRL revocation for the leaf and every intermediate carrying a cDP.
    check_revocation(&leaf_x509, &intermediates, anchors, crl_fetch)?;
    for inter in &intermediates {
        check_revocation(inter, &intermediates, anchors, crl_fetch)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TESTDATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/bimi");
    /// cDP URL baked into `test-leaf-revoked.pem` (see generate.sh).
    const REVOKED_CDP_URL: &str = "http://vmc.example.com/test-root.crl";

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{TESTDATA}/{name}")).expect("fixture readable")
    }

    fn test_anchors() -> Vec<TrustAnchor<'static>> {
        let certs = parse_pem_certs(&fixture("test-root.pem")).unwrap();
        vec![
            webpki::anchor_from_trusted_cert(&certs[0])
                .unwrap()
                .to_owned(),
        ]
    }

    fn no_crls(_url: &str) -> Option<Vec<u8>> {
        None
    }

    #[test]
    fn valid_chain_passes() {
        let bundle = fixture("test-leaf-example-com.pem");
        validate_vmc_with_anchors(&bundle, "example.com", &test_anchors(), &no_crls).unwrap();
    }

    #[test]
    fn wrong_domain_fails() {
        // Cert names other.com; the From domain is example.com.
        let bundle = fixture("test-leaf-other-com.pem");
        let err = validate_vmc_with_anchors(&bundle, "example.com", &test_anchors(), &no_crls)
            .unwrap_err();
        assert_eq!(err, VmcError::DomainMismatch);
    }

    #[test]
    fn parent_subdomain_shortcuts_fail_both_directions() {
        // A VMC for example.com must NOT vouch for mail.example.com…
        let bundle = fixture("test-leaf-example-com.pem");
        let err = validate_vmc_with_anchors(&bundle, "mail.example.com", &test_anchors(), &no_crls)
            .unwrap_err();
        assert_eq!(err, VmcError::DomainMismatch);
        // …and case-insensitivity is the only leniency allowed.
        validate_vmc_with_anchors(&bundle, "EXAMPLE.com", &test_anchors(), &no_crls).unwrap();
    }

    #[test]
    fn expired_cert_fails() {
        let bundle = fixture("test-leaf-expired.pem");
        let err = validate_vmc_with_anchors(&bundle, "example.com", &test_anchors(), &no_crls)
            .unwrap_err();
        assert_eq!(err, VmcError::Expired);
    }

    #[test]
    fn untrusted_root_fails() {
        // Well-formed leaf for example.com, but signed by an unrelated root
        // that is not an embedded MVA anchor.
        let bundle = fixture("other-leaf-example-com.pem");
        let err = validate_vmc_with_anchors(&bundle, "example.com", &test_anchors(), &no_crls)
            .unwrap_err();
        assert!(matches!(err, VmcError::Chain(_)), "got {err}");
    }

    #[test]
    fn other_root_anchored_passes() {
        // Sanity: the same leaf validates when its own root is the anchor —
        // proves untrusted_root_fails is about trust, not fixture corruption.
        let bundle = fixture("other-leaf-example-com.pem");
        let certs = parse_pem_certs(&fixture("other-root.pem")).unwrap();
        let anchors = vec![
            webpki::anchor_from_trusted_cert(&certs[0])
                .unwrap()
                .to_owned(),
        ];
        validate_vmc_with_anchors(&bundle, "example.com", &anchors, &no_crls).unwrap();
    }

    #[test]
    fn missing_logotype_oid_fails() {
        let bundle = fixture("test-leaf-no-logotype.pem");
        let err = validate_vmc_with_anchors(&bundle, "example.com", &test_anchors(), &no_crls)
            .unwrap_err();
        assert_eq!(err, VmcError::NoLogotype);
    }

    #[test]
    fn revoked_leaf_fails() {
        let bundle = fixture("test-leaf-revoked.pem");
        let crl = fixture("test-root.crl.pem");
        let fetch = move |url: &str| (url == REVOKED_CDP_URL).then(|| crl.clone());
        let err =
            validate_vmc_with_anchors(&bundle, "example.com", &test_anchors(), &fetch).unwrap_err();
        assert_eq!(err, VmcError::Revoked);
    }

    #[test]
    fn revoked_leaf_with_unavailable_crl_soft_passes() {
        // CRL fetch failure is a soft fail (log + accept): the cert is NOT
        // marked valid by this — it simply isn't proven revoked.
        let bundle = fixture("test-leaf-revoked.pem");
        validate_vmc_with_anchors(&bundle, "example.com", &test_anchors(), &no_crls).unwrap();
    }

    #[test]
    fn malformed_bundle_fails() {
        let err = validate_vmc_with_anchors(b"not a pem", "example.com", &test_anchors(), &no_crls)
            .unwrap_err();
        assert!(matches!(err, VmcError::Malformed(_)), "got {err}");
    }

    #[test]
    fn embedded_mva_roots_parse_as_anchors() {
        // Guards the embed: both production roots must build trust anchors
        // (a corrupted constant would silently shrink the anchor set).
        assert_eq!(mva_anchors().len(), 2);
    }
}
