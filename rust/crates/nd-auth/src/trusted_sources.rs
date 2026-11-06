//! ExtAuth trusted-source validation and unauthenticated body caps.
//!
//! Go source: `server/auth.go:418-452` (`validateIPAgainstList` — CIDR
//! network containment per `net.Contains`, plus the unix-socket `@`
//! special form of golang/go#49825) and `server/auth.go:100` /
//! `server/subsonic/middlewares.go:34` (the 8KiB login and 10MB form
//! body caps). The Go test table lives in `server/auth_test.go:341-381`.
//!
//! Fail-closed: every parse or validation miss denies — a malformed IP,
//! an empty list, or an unusable CIDR never admits a request.

use thiserror::Error;

/// Bounds the payload of unauthenticated login routes across all APIs
/// (`server/auth.go:100`, `MaxLoginBodySize = 8 << 10`).
pub const MAX_LOGIN_BODY_SIZE: usize = 8 << 10;

/// Bounds the Subsonic form payload (`server/subsonic/middlewares.go:34`,
/// `http.MaxBytesReader(..., 10 << 20)`).
pub const MAX_SUBSONIC_BODY_SIZE: usize = 10 << 20;

/// A request body exceeded its cap (`http.MaxBytesReader` surfaces this
/// as a read error; the port makes the check explicit).
#[derive(Debug, Error)]
#[error("request body of {actual} bytes exceeds the {cap}-byte cap")]
pub struct BodyTooLarge {
    /// The observed body length.
    pub actual: usize,
    /// The enforced cap.
    pub cap: usize,
}

/// Enforces a body cap (`LimitLoginBody`, `middlewares.go:34`): a length
/// at or under `cap` passes, anything larger is rejected.
pub fn check_body_cap(len: usize, cap: usize) -> Result<(), BodyTooLarge> {
    if len <= cap {
        Ok(())
    } else {
        Err(BodyTooLarge { actual: len, cap })
    }
}

/// An IP address, v4 or v6 — the piece of `net.IP` this module needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ip {
    /// IPv4, in network byte order.
    V4(u32),
    /// IPv6, in network byte order.
    V6(u128),
}

impl Ip {
    /// The bits in one address of this family.
    fn bit_len(self) -> u32 {
        match self {
            Ip::V4(_) => 32,
            Ip::V6(_) => 128,
        }
    }
}

/// `net.ParseIP` for the accepted textual forms: dotted-quad IPv4 and
/// IPv6 with `::` compression, an optional embedded IPv4 tail, and an
/// optional bracket pair. Ports are not part of the address.
pub fn parse_ip(s: &str) -> Option<Ip> {
    let s = s.trim_start_matches('[').trim_end_matches(']');
    if let Some(v4) = parse_ipv4(s) {
        return Some(Ip::V4(v4));
    }
    parse_ipv6(s).map(Ip::V6)
}

/// Dotted-quad IPv4: exactly four 0-255 decimal octets (`net.ParseIP`
/// rejects shorthand and leading zeros in Go 1.17+).
fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = 0u32;
    for part in parts {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let val: u32 = part.parse().ok()?;
        if val > 255 || (part.len() > 1 && part.starts_with('0')) {
            return None;
        }
        out = (out << 8) | val;
    }
    Some(out)
}

/// IPv6 per RFC 4291: up to 8 `x:x:x:x:x:x:x:x` groups, at most one `::`
/// compression, and an optional `a.b.c.d` tail for the last 32 bits.
fn parse_ipv6(s: &str) -> Option<u128> {
    let (head, tail) = match s.split_once("::") {
        Some((h, t)) => (h, Some(t)),
        None => (s, None),
    };
    let mut groups: Vec<u16> = Vec::new();
    let mut head_parts: Vec<&str> = head.split(':').filter(|p| !p.is_empty()).collect();
    if tail.is_some() && head.is_empty() {
        head_parts.clear();
    }
    for part in &head_parts {
        if part.contains('.') {
            // An embedded IPv4 is only valid as the final head group.
            if std::ptr::eq(*part, *head_parts.last().unwrap()) {
                let v4 = parse_ipv4(part)?;
                groups.push((v4 >> 16) as u16);
                groups.push((v4 & 0xffff) as u16);
            } else {
                return None;
            }
        } else {
            if part.len() > 4 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            groups.push(u16::from_str_radix(part, 16).ok()?);
        }
    }

    let head_len = head_split_len(&head_parts);
    let total_groups = match tail {
        None => groups.len(),
        Some(t) => {
            if t.is_empty() {
                head_len // "::" alone or a trailing "::"
            } else {
                let mut tail_groups = 0usize;
                for part in t.split(':') {
                    if part.contains('.') {
                        let v4 = parse_ipv4(part)?;
                        groups.push((v4 >> 16) as u16);
                        groups.push((v4 & 0xffff) as u16);
                        tail_groups += 2;
                    } else {
                        if part.is_empty()
                            || part.len() > 4
                            || !part.bytes().all(|b| b.is_ascii_hexdigit())
                        {
                            return None;
                        }
                        groups.push(u16::from_str_radix(part, 16).ok()?);
                        tail_groups += 1;
                    }
                }
                head_len + tail_groups
            }
        }
    };

    if tail.is_some() {
        // Compression must fill exactly the missing groups; at least one
        // group must be elided (Go's net.ParseIP rejects 8-group forms
        // containing "::").
        if total_groups >= 8 {
            return None;
        }
        let fill = 8 - total_groups;
        let fill_pos = head_len;
        let mut expanded: Vec<u16> = Vec::new();
        expanded.extend_from_slice(&groups[..fill_pos]);
        expanded.extend(std::iter::repeat_n(0, fill));
        expanded.extend_from_slice(&groups[fill_pos..]);
        return Some(
            u128::from(expanded[0]) << 112
                | u128::from(expanded[1]) << 96
                | u128::from(expanded[2]) << 80
                | u128::from(expanded[3]) << 64
                | u128::from(expanded[4]) << 48
                | u128::from(expanded[5]) << 32
                | u128::from(expanded[6]) << 16
                | u128::from(expanded[7]),
        );
    }
    if total_groups != 8 {
        return None;
    }
    Some(groups.iter().enumerate().fold(0u128, |acc, (i, g)| {
        acc | (u128::from(*g) << (112 - 16 * i))
    }))
}

/// How many head groups precede the `::` compression point.
fn head_split_len(head_parts: &[&str]) -> usize {
    head_parts
        .iter()
        .map(|p| if p.contains('.') { 2 } else { 1 })
        .sum()
}

/// An IP network — `net.IPNet` (mask + contained family).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    /// The network's family and masked base.
    ip: Ip,
    /// Prefix length in bits.
    prefix: u32,
}

impl IpNet {
    /// `net.IPNet.Contains`: same-family addresses only, and only those
    /// sharing the first `prefix` bits.
    #[must_use]
    pub fn contains(&self, ip: Ip) -> bool {
        // Compare in the family's own width: V6 needs all 128 bits, so
        // the mask is computed per family (a shared u64 would truncate).
        let shift = ip.bit_len() - self.prefix;
        match (self.ip, ip) {
            (Ip::V4(n), Ip::V4(a)) => {
                let shift = shift.min(32);
                if shift >= 32 {
                    true // /0 contains every v4 address
                } else {
                    (n >> shift) == (a >> shift)
                }
            }
            (Ip::V6(n), Ip::V6(a)) => {
                let shift = shift.min(128);
                if shift >= 128 {
                    true // /0 contains every v6 address
                } else {
                    (n >> shift) == (a >> shift)
                }
            }
            _ => false,
        }
    }
}

/// `net.ParseCIDR` for `addr/prefix` forms of either family.
pub fn parse_cidr(s: &str) -> Option<IpNet> {
    let (addr, prefix) = s.split_once('/')?;
    let ip = parse_ip(addr)?;
    let prefix: u32 = prefix.parse().ok()?;
    if prefix > ip.bit_len() {
        return None;
    }
    let masked = match ip {
        Ip::V4(v) => {
            let shift = 32 - prefix;
            Ip::V4(if shift >= 32 {
                0
            } else {
                v & (u32::MAX << shift)
            })
        }
        Ip::V6(v) => {
            let shift = 128 - prefix;
            Ip::V6(if shift >= 128 {
                0
            } else {
                v & (u128::MAX << shift)
            })
        }
    };
    Some(IpNet { ip: masked, prefix })
}

/// `net.SplitHostPort` for the host:port and [host]:port forms; returns
/// the host only, since the port is irrelevant to containment.
fn split_host_port(s: &str) -> Option<&str> {
    if let Some(rest) = s.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &rest[..end];
        let after = &rest[end + 1..];
        return if after.starts_with(':') {
            Some(host)
        } else {
            None
        };
    }
    let idx = s.rfind(':')?;
    let port = &s[idx + 1..];
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(&s[..idx])
}

/// The port of `validateIPAgainstList` (`server/auth.go:418-452`),
/// with the server address passed alongside so the unix-socket `@`
/// special form can check the `unix:` prefix exactly as Go reads
/// `conf.Server.Address`. Fail-closed: empty inputs, malformed IPs, and
/// unusable CIDRs all deny.
#[must_use]
pub fn validate_ip_against_list(
    ip: &str,
    comma_separated_list: &str,
    server_address: &str,
) -> bool {
    if comma_separated_list.is_empty() || ip.is_empty() {
        return false;
    }

    let cidrs: Vec<&str> = comma_separated_list.split(',').collect();

    // The remote address on a unix socket is '@' (golang/go#49825).
    if ip == "@" && server_address.starts_with("unix:") {
        return cidrs.contains(&"@");
    }

    let host = match parse_ip(ip) {
        Some(_) => ip,
        None => match split_host_port(ip) {
            Some(h) if !h.is_empty() => h,
            _ => return false,
        },
    };

    // Go re-wraps the address as ip/32 (or its v6 equivalent) and tests
    // containment, so unparseable addresses deny.
    let Some(tested) = parse_ip(host) else {
        return false;
    };
    let host_bits = tested.bit_len();
    let Some(net) = parse_cidr(&format!("{host}/{host_bits}")) else {
        return false;
    };
    let tested = net.ip;

    cidrs
        .iter()
        .any(|cidr| parse_cidr(cidr.trim()).is_some_and(|net| net.contains(tested)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "0.0.0.0";

    // Ported from server/auth_test.go:341-381 — same table, AAA pinned.
    #[test]
    fn empty_inputs_deny() {
        // Arrange/Act/Assert
        assert!(!validate_ip_against_list("", "", ADDR));
        assert!(!validate_ip_against_list("192.168.1.1", "", ADDR));
        assert!(!validate_ip_against_list("", "192.168.0.0/16", ADDR));
    }

    #[test]
    fn invalid_ip_denies() {
        // Arrange/Act/Assert
        assert!(!validate_ip_against_list(
            "invalidIP",
            "192.168.0.0/16",
            ADDR
        ));
    }

    #[test]
    fn ip_in_list_accepts() {
        // Arrange
        let list = "192.168.0.0/16,10.0.0.0/8";
        // Act/Assert
        assert!(validate_ip_against_list("192.168.1.1", list, ADDR));
        assert!(validate_ip_against_list("10.0.0.1", list, ADDR));
    }

    #[test]
    fn ip_not_in_list_denies() {
        // Arrange/Act/Assert
        assert!(!validate_ip_against_list(
            "172.16.0.1",
            "192.168.0.0/16,10.0.0.0/8",
            ADDR
        ));
    }

    #[test]
    fn invalid_cidrs_are_ignored() {
        // Arrange/Act/Assert
        assert!(validate_ip_against_list(
            "192.168.1.1",
            "192.168.0.0/16,invalidCIDR",
            ADDR
        ));
        assert!(validate_ip_against_list(
            "10.0.0.1",
            "invalidCIDR,10.0.0.0/8",
            ADDR
        ));
        assert!(!validate_ip_against_list(
            "172.16.0.1",
            "192.168.0.0/16,invalidCIDR",
            ADDR
        ));
    }

    #[test]
    fn ip_port_format_is_handled() {
        // Arrange
        let list = "192.168.0.0/16,10.0.0.0/8";
        // Act/Assert
        assert!(validate_ip_against_list("192.168.1.1:8080", list, ADDR));
        assert!(validate_ip_against_list("10.0.0.1:1234", list, ADDR));
        assert!(!validate_ip_against_list("172.16.0.1:9999", list, ADDR));
    }

    // Unix '@' special form (server/auth.go:425-429).
    #[test]
    fn unix_at_matches_only_when_listed_and_address_is_unix() {
        // Arrange/Act/Assert
        assert!(validate_ip_against_list(
            "@",
            "@,127.0.0.1/32",
            "unix:/tmp/nd.sock"
        ));
        assert!(validate_ip_against_list("@", "@", "unix:/tmp/nd.sock"));
        assert!(!validate_ip_against_list(
            "@",
            "127.0.0.1/32",
            "unix:/tmp/nd.sock"
        ));
        // '@' without a unix: server address is not special-cased.
        assert!(!validate_ip_against_list("@", "@", ADDR));
    }

    // IPv6 containment, per net.Contains semantics.
    #[test]
    fn ipv6_containment() {
        // Arrange
        let list = "fd00::/8,2001:db8::/32";
        // Act/Assert
        assert!(validate_ip_against_list("fd00::1", list, ADDR));
        assert!(validate_ip_against_list("2001:db8::dead:beef", list, ADDR));
        assert!(validate_ip_against_list("[2001:db8::1]:4533", list, ADDR));
        assert!(!validate_ip_against_list("2002:db8::1", list, ADDR));
        // Cross-family never contains.
        assert!(!validate_ip_against_list(
            "::ffff:192.168.1.1",
            "192.168.0.0/16",
            ADDR
        ));
    }

    #[test]
    fn body_caps() {
        // Arrange/Act/Assert — at the cap passes, one over fails.
        assert!(check_body_cap(MAX_LOGIN_BODY_SIZE, MAX_LOGIN_BODY_SIZE).is_ok());
        assert!(check_body_cap(MAX_LOGIN_BODY_SIZE + 1, MAX_LOGIN_BODY_SIZE).is_err());
        assert!(check_body_cap(MAX_SUBSONIC_BODY_SIZE, MAX_SUBSONIC_BODY_SIZE).is_ok());
        assert!(check_body_cap(MAX_SUBSONIC_BODY_SIZE + 1, MAX_SUBSONIC_BODY_SIZE).is_err());
        assert_eq!(MAX_LOGIN_BODY_SIZE, 8 << 10);
        assert_eq!(MAX_SUBSONIC_BODY_SIZE, 10 << 20);
    }
}
