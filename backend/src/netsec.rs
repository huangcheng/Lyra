//! Network security guards shared by account setup and protocol adapters.
//!
//! Centralizes the checks that keep user-supplied hosts and server-supplied
//! URLs from turning the backend into an SSRF oracle or leaking credentials:
//!
//! - [`validate_domain`]: strict ASCII DNS name validation for the probe endpoint.
//! - [`is_blocked_addr`] / [`filter_public_addrs`]: reject loopback, private,
//!   link-local, and reserved addresses before outbound TCP connects.
//! - [`validate_server_url`]: scheme enforcement for DAV/JMAP base URLs
//!   (https required unless the host is loopback or a private/LAN address).
//! - [`origin_of`]: origin pinning so credentials are only sent to the
//!   configured server's origin.
//! - [`normalize_security_mode`]: allowlist for IMAP/SMTP security modes.

#![allow(clippy::doc_markdown)]

use std::net::IpAddr;

/// Validate a DNS domain name: ASCII labels only, no IP literals.
///
/// Returns the normalized domain (trimmed, trailing dot stripped, lowercased)
/// on success — callers must use the returned value, not the raw input.
///
/// Wildcard-DNS names (e.g. `10.0.0.5.nip.io`) are syntactically valid and
/// accepted here; the address filter in [`filter_public_addrs`] is what stops
/// them from reaching internal hosts.
pub fn validate_domain(domain: &str) -> Result<String, String> {
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() {
        return Err("domain is empty".into());
    }
    if d.len() > 253 {
        return Err(format!("domain '{domain}' is too long"));
    }
    // IP literals are never valid probe targets.
    if d.parse::<IpAddr>().is_ok() || d.contains([':', '[', ']']) {
        return Err(format!(
            "domain '{domain}' is an IP literal, not a DNS name"
        ));
    }
    for label in d.split('.') {
        if label.is_empty() {
            return Err(format!("domain '{domain}' contains an empty label"));
        }
        if label.len() > 63 {
            return Err(format!("domain '{domain}' has a label over 63 bytes"));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!(
                "domain '{domain}' has a label starting/ending with '-'"
            ));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!("domain '{domain}' contains non-DNS characters"));
        }
    }
    Ok(d)
}

/// Whether an IP address must not be used as an outbound destination:
/// loopback, private (RFC 1918), link-local, unspecified, CGNAT,
/// documentation, benchmarking, reserved, multicast, broadcast, or an
/// IPv4-embedding translation prefix (6to4 / Teredo / local-use NAT64).
pub fn is_blocked_addr(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()          // 127/8
            || v4.is_private()        // 10/8, 172.16/12, 192.168/16
            || v4.is_link_local()     // 169.254/16
            || v4.is_unspecified()    // 0.0.0.0
            || v4.octets()[0] == 0    // 0/8 "this network"
            || v4.is_broadcast()      // 255.255.255.255
            // 100.64/10 (CGNAT shared space)
            || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
            || v4.is_documentation()  // 192.0.2/24, 198.51.100/24, 203.0.113/24
            // 198.18/15 (benchmarking)
            || (v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18)
            || v4.octets()[0] >= 240  // 240/4 reserved
            || v4.is_multicast() // 224/4
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped/compatible addresses inherit the v4 rules.
            if let Some(v4) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                return is_blocked_addr(&IpAddr::V4(v4));
            }
            let seg = v6.segments();
            // 6to4 (2002::/16) embeds a plain IPv4 address in bits 16..48.
            if seg[0] == 0x2002 {
                let [a, b] = seg[1].to_be_bytes();
                let [c, d] = seg[2].to_be_bytes();
                return is_blocked_addr(&IpAddr::V4(std::net::Ipv4Addr::new(a, b, c, d)));
            }
            // Teredo (2001::/32) embeds the client IPv4 address, bit-inverted,
            // in the last 32 bits.
            if seg[0] == 0x2001 && seg[1] == 0 {
                let bits = !((u32::from(seg[6]) << 16) | u32::from(seg[7]));
                return is_blocked_addr(&IpAddr::V4(std::net::Ipv4Addr::from_bits(bits)));
            }
            v6.is_loopback()               // ::1
            || v6.is_unspecified()         // ::
            || v6.is_multicast()           // ff00::/8
            || v6.is_unique_local()        // fc00::/7
            || v6.is_unicast_link_local()  // fe80::/10
            // 2001:db8::/32 (documentation)
            || (seg[0] == 0x2001 && seg[1] == 0x0db8)
            // 64:ff9b:1::/48 (local-use NAT64; the embedded-IPv4 position is
            // not fixed at this prefix length, so the whole prefix is blocked)
            || (seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2] == 0x0001)
        }
    }
}

/// Keep only publicly routable addresses from a resolution result.
///
/// Returns an empty vector when every address is blocked — callers must
/// treat that as "do not connect".
pub fn filter_public_addrs(addrs: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    addrs.into_iter().filter(|a| !is_blocked_addr(a)).collect()
}

/// Normalize an IMAP/SMTP security mode string against the allowlist.
///
/// Only `tls` and `starttls` are supported (case-insensitive input, returned
/// normalized to lowercase). Anything else — including the removed insecure
/// `none` mode — is an error telling the user to update the account.
pub fn normalize_security_mode(value: &str) -> Result<&'static str, String> {
    match value.to_ascii_lowercase().as_str() {
        "tls" => Ok("tls"),
        "starttls" => Ok("starttls"),
        other => Err(format!(
            "unsupported security mode '{other}': only 'tls' and 'starttls' are supported \
             (insecure plaintext mode was removed); update the account to fix this"
        )),
    }
}

/// Validate a user-supplied server URL (CardDAV/CalDAV/JMAP base).
///
/// Requires `https://` unless the host is loopback or a private/LAN address
/// (self-hosted LAN servers legitimately use plain HTTP). Only literal IP
/// hosts and `localhost` names qualify as local; DNS names are treated as
/// public (no resolution at config-entry time).
pub fn validate_server_url(url: &str) -> Result<(), String> {
    let parsed =
        reqwest::Url::parse(url).map_err(|e| format!("invalid server URL '{url}': {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "server URL '{url}' uses unsupported scheme '{scheme}': use https://"
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("server URL '{url}' has no host"))?;
    if scheme == "http" && !host_is_local(host) {
        return Err(format!(
            "server URL '{url}' uses insecure http:// for a public host: use https://"
        ));
    }
    Ok(())
}

/// Whether a URL host is loopback or a private/LAN address.
///
/// Only literal IPs (checked by range) and `localhost` names qualify; DNS
/// names are treated as public because we do not resolve at config-entry time.
pub fn host_is_local(host: &str) -> bool {
    let bare = host.trim_matches(['[', ']']);
    if bare.eq_ignore_ascii_case("localhost") || bare.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_blocked_addr(&ip);
    }
    false
}

/// Extract the origin (`scheme://host:port`) of an http(s) URL.
///
/// The port is always explicit (default ports are filled in) so origins
/// compare equal regardless of whether the URL spelled them out.
pub fn origin_of(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!("URL '{url}' uses unsupported scheme '{scheme}'"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("URL '{url}' has no host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| format!("URL '{url}' has no usable port"))?;
    Ok(format!("{scheme}://{host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // ── validate_domain ─────────────────────────────────────────────

    #[test]
    fn domain_accepts_normal_names() {
        assert!(validate_domain("example.com").is_ok());
        assert!(validate_domain("mail.example.co.uk").is_ok());
        assert!(validate_domain("a-b-c.example.org").is_ok());
        assert!(validate_domain("xn--nxasmq6b.example").is_ok());
    }

    #[test]
    fn domain_accepts_wildcard_dns_names_syntactically() {
        // nip.io-style wildcard names are valid DNS; the address filter
        // (not the syntax check) stops them from reaching internal hosts.
        assert!(validate_domain("10.0.0.5.nip.io").is_ok());
        assert!(validate_domain("192.168.1.1.sslip.io").is_ok());
    }

    #[test]
    fn domain_rejects_ip_literals() {
        assert!(validate_domain("10.0.0.5").is_err());
        assert!(validate_domain("127.0.0.1").is_err());
        assert!(validate_domain("8.8.8.8").is_err());
        assert!(validate_domain("::1").is_err());
        assert!(validate_domain("[::1]").is_err());
        assert!(validate_domain("fe80::1").is_err());
    }

    #[test]
    fn domain_rejects_malformed() {
        assert!(validate_domain("").is_err());
        assert!(validate_domain("   ").is_err());
        assert!(validate_domain("a..b").is_err());
        assert!(validate_domain(".example.com").is_err());
        assert!(validate_domain("-bad.example.com").is_err());
        assert!(validate_domain("bad-.example.com").is_err());
        assert!(validate_domain("under_score.example.com").is_err());
        assert!(validate_domain("exa mple.com").is_err());
        assert!(validate_domain("exámple.com").is_err());
        assert!(validate_domain("user@example.com").is_err());
        assert!(validate_domain("example.com/path").is_err());
        assert!(validate_domain("example.com:993").is_err());
        assert!(validate_domain(&"a".repeat(64)).is_err());
        assert!(validate_domain("example..com").is_err());
    }

    #[test]
    fn domain_trims_trailing_dot_and_whitespace() {
        assert!(validate_domain("example.com.").is_ok());
        assert!(validate_domain("  example.com  ").is_ok());
    }

    #[test]
    fn domain_returns_normalized_value() {
        // Callers must use the returned value, not the raw input.
        assert_eq!(validate_domain("  example.com  ").unwrap(), "example.com");
        assert_eq!(validate_domain("Example.COM.").unwrap(), "example.com");
        assert_eq!(
            validate_domain("Mail.Example.Org").unwrap(),
            "mail.example.org"
        );
    }

    // ── is_blocked_addr / filter_public_addrs ───────────────────────

    #[test]
    fn blocked_addr_loopback_private_linklocal() {
        let blocked = [
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 9)),
            IpAddr::V4(Ipv4Addr::new(127, 255, 0, 9)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
            IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255)),
            IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1)),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)), // CGNAT
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),  // documentation
            IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)),  // reserved
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST), // ::1
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V6("fc00::1".parse().unwrap()), // unique local
            IpAddr::V6("fdff::abcd".parse().unwrap()),
            IpAddr::V6("fe80::1".parse().unwrap()), // link-local
            IpAddr::V6("febf::1".parse().unwrap()),
            // IPv4-mapped IPv6 must not smuggle a private address through.
            IpAddr::V6("::ffff:10.0.0.5".parse().unwrap()),
            IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()),
        ];
        for addr in blocked {
            assert!(is_blocked_addr(&addr), "expected {addr} to be blocked");
        }
    }

    #[test]
    fn blocked_addr_allows_public() {
        let public = [
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 15, 0, 1)), // just outside 172.16/12
            IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 169, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(11, 0, 0, 1)),
            IpAddr::V6("2606:4700:4700::1111".parse().unwrap()),
            IpAddr::V6("2001:4860:4860::8888".parse().unwrap()),
            IpAddr::V6("::ffff:8.8.8.8".parse().unwrap()), // mapped public
        ];
        for addr in public {
            assert!(!is_blocked_addr(&addr), "expected {addr} to be allowed");
        }
    }

    #[test]
    fn blocked_addr_v6_special_ranges() {
        let blocked = [
            // 2001:db8::/32 documentation
            IpAddr::V6("2001:db8::1".parse().unwrap()),
            IpAddr::V6("2001:db8:ffff:ffff:ffff:ffff:ffff:ffff".parse().unwrap()),
            // 64:ff9b:1::/48 local-use NAT64 (can embed private IPv4;
            // extraction position is not fixed at this prefix length,
            // so the whole prefix is blocked)
            IpAddr::V6("64:ff9b:1::1".parse().unwrap()),
            IpAddr::V6("64:ff9b:1:ffff:ffff:ffff:ffff:ffff".parse().unwrap()),
            // 2002::/16 6to4 — embedded IPv4 (bits 16..48) inherits v4 rules
            IpAddr::V6("2002:0a00:0001::".parse().unwrap()), // 10.0.0.1
            IpAddr::V6("2002:7f00:0001::".parse().unwrap()), // 127.0.0.1
            IpAddr::V6("2002:a9fe:0001::".parse().unwrap()), // 169.254.0.1
            IpAddr::V6("2002:c0a8:0101::".parse().unwrap()), // 192.168.1.1
            // 2001::/32 Teredo — client IPv4 is the bit-inverted last 32 bits
            // f5ff:fffa = !10.0.0.5
            IpAddr::V6("2001:0:4136:e378:8000:63bf:f5ff:fffa".parse().unwrap()),
            // ffff:fffe = !0.0.0.1
            IpAddr::V6("2001::ffff:fffe".parse().unwrap()),
        ];
        for addr in blocked {
            assert!(is_blocked_addr(&addr), "expected {addr} to be blocked");
        }
    }

    #[test]
    fn blocked_addr_v6_special_range_boundaries() {
        let allowed = [
            // Just outside the documentation range
            IpAddr::V6("2001:db7::1".parse().unwrap()),
            IpAddr::V6("2001:db9::1".parse().unwrap()),
            // Well-known NAT64 64:ff9b::/96 is out of scope (see report);
            // just outside the local-use /48:
            IpAddr::V6("64:ff9b:2::1".parse().unwrap()),
            // 6to4 embedding a public IPv4
            IpAddr::V6("2002:0808:0808::".parse().unwrap()), // 8.8.8.8
            // Teredo with a public client IPv4 (f7f7:f7f7 = !8.8.8.8)
            IpAddr::V6("2001:0:4136:e378:8000:63bf:f7f7:f7f7".parse().unwrap()),
        ];
        for addr in allowed {
            assert!(!is_blocked_addr(&addr), "expected {addr} to be allowed");
        }
    }

    #[test]
    fn filter_public_addrs_drops_all_blocked() {
        let addrs = vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ];
        assert!(filter_public_addrs(addrs).is_empty());
    }

    #[test]
    fn filter_public_addrs_keeps_only_public_from_mix() {
        let addrs = vec![
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("2606:4700:4700::1111".parse().unwrap()),
        ];
        let kept = filter_public_addrs(addrs);
        assert_eq!(
            kept,
            vec![
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                IpAddr::V6("2606:4700:4700::1111".parse().unwrap()),
            ]
        );
    }

    // ── normalize_security_mode ─────────────────────────────────────

    #[test]
    fn security_mode_allowlist() {
        assert_eq!(normalize_security_mode("tls"), Ok("tls"));
        assert_eq!(normalize_security_mode("TLS"), Ok("tls"));
        assert_eq!(normalize_security_mode("starttls"), Ok("starttls"));
        assert_eq!(normalize_security_mode("STARTTLS"), Ok("starttls"));
        assert_eq!(normalize_security_mode("StartTls"), Ok("starttls"));
        assert!(normalize_security_mode("none").is_err());
        assert!(normalize_security_mode("NONE").is_err());
        assert!(normalize_security_mode("").is_err());
        assert!(normalize_security_mode("ssl").is_err());
        assert!(normalize_security_mode("plain").is_err());
        assert!(normalize_security_mode("tls;drop table").is_err());
    }

    #[test]
    fn security_mode_error_mentions_update() {
        let err = normalize_security_mode("none").unwrap_err();
        assert!(err.contains("update the account"), "got: {err}");
    }

    // ── validate_server_url ─────────────────────────────────────────

    #[test]
    fn server_url_requires_https_for_public_hosts() {
        assert!(validate_server_url("https://dav.example.com/").is_ok());
        assert!(validate_server_url("https://dav.example.com:8443/card/").is_ok());
        assert!(validate_server_url("https://8.8.8.8/dav/").is_ok());
        assert!(validate_server_url("http://dav.example.com/").is_err());
        assert!(validate_server_url("http://8.8.8.8/dav/").is_err());
        // DNS names are treated as public even if they might resolve locally.
        assert!(validate_server_url("http://nas.internal/").is_err());
    }

    #[test]
    fn server_url_allows_http_for_loopback_and_private() {
        assert!(validate_server_url("http://localhost:8080/").is_ok());
        assert!(validate_server_url("http://dav.localhost/").is_ok());
        assert!(validate_server_url("http://127.0.0.1:8080/").is_ok());
        assert!(validate_server_url("http://192.168.1.10/").is_ok());
        assert!(validate_server_url("http://10.0.0.5/carddav/").is_ok());
        assert!(validate_server_url("http://172.16.0.20/").is_ok());
        assert!(validate_server_url("http://169.254.1.1/").is_ok());
        assert!(validate_server_url("http://[::1]:8080/").is_ok());
        assert!(validate_server_url("http://[fd00::10]/").is_ok());
        assert!(validate_server_url("https://192.168.1.10/").is_ok());
    }

    #[test]
    fn server_url_rejects_garbage() {
        assert!(validate_server_url("").is_err());
        assert!(validate_server_url("not a url").is_err());
        assert!(validate_server_url("ftp://dav.example.com/").is_err());
        assert!(validate_server_url("dav.example.com").is_err());
        assert!(validate_server_url("file:///etc/passwd").is_err());
    }

    // ── origin pinning ──────────────────────────────────────────────

    #[test]
    fn origin_normalizes_default_ports() {
        assert_eq!(
            origin_of("https://dav.example.com/card/").unwrap(),
            "https://dav.example.com:443"
        );
        assert_eq!(
            origin_of("https://dav.example.com:443/x").unwrap(),
            "https://dav.example.com:443"
        );
        assert_eq!(
            origin_of("http://192.168.1.10/x").unwrap(),
            "http://192.168.1.10:80"
        );
        assert_eq!(
            origin_of("https://EXAMPLE.com:8443/x").unwrap(),
            "https://example.com:8443"
        );
    }

    #[test]
    fn origin_rejects_non_http_and_garbage() {
        assert!(origin_of("ftp://example.com/").is_err());
        assert!(origin_of("not a url").is_err());
    }
}
