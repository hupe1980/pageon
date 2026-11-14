//! Which addresses are public. Everything else (loopback, private, link-local, unique-local,
//! CGNAT, benchmarking, unspecified, multicast, reserved, broadcast; IPv4 embedded in IPv6) is
//! the private network, which pageon refuses unless a request allows it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is a public unicast address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0                                   // "this network", 0.0.0.0
        || a == 10                             // private
        || a == 127                            // loopback
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10
        || (a == 169 && b == 254)              // link-local (cloud metadata)
        || (a == 172 && (16..32).contains(&b)) // private
        || (a == 192 && b == 0 && c == 0)      // IETF protocol assignments
        || (a == 192 && b == 168)              // private
        || (a == 198 && (b == 18 || b == 19))  // benchmarking
        || a >= 224) // multicast, reserved, broadcast
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_v4(ip) {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // site-local (deprecated) fec0::/10
        || (s[0] & 0xff00) == 0xff00 // multicast
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || s[..6] == [0, 0, 0, 0, 0, 0]) // IPv4-compatible (deprecated) and the rest of ::/96
}

/// IPv4 carried in an IPv6 address: mapped `::ffff:a.b.c.d`, NAT64 `64:ff9b::a.b.c.d`.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let tail = || Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if s[..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff {
        return Some(tail());
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return Some(tail());
    }
    None
}

/// Whether a URL host is on the private network without asking DNS: an IP literal that is not
/// public, or `localhost` / `*.localhost` (trailing dots ignored).
pub fn is_private_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Ipv4(v4) => !is_public_v4(*v4),
        url::Host::Ipv6(v6) => !is_public_v6(*v6),
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
    }
}

/// The private address of a URL's host, for messages (`127.0.0.1`, `localhost`), if it is one.
pub fn private_host_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let h = u.host()?;
    is_private_host(&h).then(|| h.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private(s: &str) -> bool {
        !is_public(s.parse().unwrap())
    }

    #[test]
    fn classifies_addresses() {
        for p in [
            "0.0.0.0",
            "10.1.2.3",
            "127.0.0.1",
            "127.255.255.254",
            "100.64.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.170",
            "192.168.1.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "::127.0.0.1",
        ] {
            assert!(private(p), "{p} must be private");
        }
        for p in [
            "8.8.8.8",
            "1.1.1.1",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!private(p), "{p} must be public");
        }
    }

    #[test]
    fn classifies_url_hosts() {
        for u in [
            "http://localhost/",
            "http://LOCALHOST./",
            "http://api.localhost:3000/",
            "http://0x7f.1/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://[::1]/",
            "http://[::ffff:7f00:1]/",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            assert!(private_host_of(u).is_some(), "{u} must be private");
        }
        for u in ["https://example.com/", "http://localhost.example.com/"] {
            assert!(private_host_of(u).is_none(), "{u} must be public");
        }
    }
}
