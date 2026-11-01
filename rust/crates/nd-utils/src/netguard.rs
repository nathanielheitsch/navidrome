//! SSRF guard for outbound connections — port of `utils/netguard`. Keeps
//! connections driven by untrusted input away from internal addresses.

use std::net::IpAddr;

/// Error returned when a dial to a private address is blocked.
pub const ERR_PRIVATE_ADDRESS: &str = "dial to private/loopback address blocked";

/// Reports whether `ip` is loopback, private, link-local or unspecified
/// (`netguard.IsPrivateIP`).
#[must_use]
pub fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6.is_unique_local()
                // IPv4-mapped addresses delegate to their IPv4 form.
                || match v6.to_ipv4_mapped() {
                    Some(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
                    None => false,
                }
                // Link-local multicast (224.0.0.0/24 equivalent for v6).
                || (v6.segments()[0] & 0xff00) == 0xff00 && v6.is_multicast()
        }
    }
}

/// Checks a resolved `host:port` address against the private-address guard,
/// honoring `allowed` prefixes (`netguard.DialControl`). Returns an error
/// message (Go returns a wrapped `ErrPrivateAddress`) when the address is
/// private and not allowed.
///
/// This sees the resolved IP, so DNS names, redirects and rebinding cannot get
/// around it.
pub fn check_addr_port(address: &str, allowed: &[std::net::Ipv4Addr]) -> Result<(), String> {
    let (host, port) = split_host_port(address)?;
    let ip: IpAddr = if host.starts_with('[') {
        let inner = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .map_err(|_| format!("{ERR_PRIVATE_ADDRESS}: unparseable address {address:?}"))?;
        IpAddr::V6(inner)
    } else {
        host.parse()
            .map_err(|_| format!("{ERR_PRIVATE_ADDRESS}: unparseable address {address:?}"))?
    };
    let _ = port;
    // Unmap IPv4-mapped IPv6, like Go's Addr.Unmap().
    let addr = match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    };
    if !is_private_ip(&addr) {
        return Ok(());
    }
    if let IpAddr::V4(v4) = addr {
        for p in allowed {
            if octet_prefix_contains(*p, v4) {
                return Ok(());
            }
        }
    }
    Err(format!("{ERR_PRIVATE_ADDRESS}: {address}"))
}

/// The allowlist entries are IPv4 network prefixes written as addresses
/// (e.g. `10.0.0.0` covers `10.x.x.x`): the leading nonzero octets form the
/// mask, and a fully-specified address matches only itself.
fn octet_prefix_contains(prefix: std::net::Ipv4Addr, addr: std::net::Ipv4Addr) -> bool {
    let (p, a) = (prefix.octets(), addr.octets());
    let octets = p.iter().position(|&o| o == 0).unwrap_or(4);
    p[..octets] == a[..octets]
}

fn split_host_port(address: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(rest) = address.strip_prefix('[') {
        let close = rest
            .find(']')
            .ok_or_else(|| format!("{ERR_PRIVATE_ADDRESS}: unparseable address {address:?}"))?;
        (format!("[{}]", &rest[..close]), &rest[close + 1..])
    } else {
        let colon = address
            .rfind(':')
            .ok_or_else(|| format!("{ERR_PRIVATE_ADDRESS}: unparseable address {address:?}"))?;
        (address[..colon].to_string(), &address[colon + 1..])
    };
    let port: u16 = port
        .trim_start_matches(':')
        .parse()
        .map_err(|_| format!("{ERR_PRIVATE_ADDRESS}: unparseable address {address:?}"))?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("valid ip")
    }

    #[test]
    fn is_private_ip_table() {
        let cases = [
            ("127.0.0.1", true),        // IPv4 loopback
            ("127.0.0.2", true),        // IPv4 loopback range
            ("::1", true),              // IPv6 loopback
            ("::ffff:127.0.0.1", true), // IPv4-mapped IPv6 loopback
            ("10.1.2.3", true),
            ("172.16.0.1", true),
            ("192.168.1.10", true),
            ("fd00::1", true),               // IPv6 unique local
            ("169.254.169.254", true),       // link-local (cloud metadata)
            ("fe80::1", true),               // IPv6 link-local
            ("224.0.0.1", true),             // link-local multicast
            ("ff02::1", true),               // v6 link-local multicast
            ("0.0.0.0", true),               // IPv4 unspecified
            ("::", true),                    // IPv6 unspecified
            ("93.184.216.34", false),        // public IPv4
            ("172.32.0.1", false),           // outside the private block
            ("2606:4700:4700::1111", false), // public IPv6
        ];
        for (addr, expected) in cases {
            assert_eq!(is_private_ip(&ip(addr)), expected, "addr {addr}");
        }
    }

    #[test]
    fn dial_control_rejects_private_and_loopback_addresses() {
        let blocked = [
            "127.0.0.1:80",
            "[::1]:443",
            "169.254.169.254:80",
            "10.0.0.1:8080",
            "0.0.0.0:80",
        ];
        for addr in blocked {
            let err = check_addr_port(addr, &[]).expect_err("must block");
            assert!(err.contains(ERR_PRIVATE_ADDRESS), "{addr}: {err}");
        }
    }

    #[test]
    fn dial_control_allows_public_addresses() {
        assert!(check_addr_port("93.184.216.34:80", &[]).is_ok());
        assert!(check_addr_port("[2606:4700:4700::1111]:443", &[]).is_ok());
    }

    #[test]
    fn dial_control_allows_allowlisted_private_prefixes() {
        // 10.0.0.0/255.0.0.0 → allow everything in 10.x.
        let allowed = [std::net::Ipv4Addr::new(10, 0, 0, 0)];
        assert!(check_addr_port("10.1.2.3:80", &allowed).is_ok());
        // Non-allowlisted private stays blocked.
        assert!(check_addr_port("192.168.1.1:80", &allowed).is_err());
    }

    #[test]
    fn dial_control_rejects_unparseable_addresses() {
        let err = check_addr_port("not an address", &[]).expect_err("must fail");
        assert!(err.contains(ERR_PRIVATE_ADDRESS));
    }
}
