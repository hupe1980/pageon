//! Origins and sites (schemeful), with the Public Suffix List.

use url::Url;

/// The registrable domain of a host (`www.example.co.uk` → `example.co.uk`), if it has one.
pub fn registrable(host: &str) -> Option<&str> {
    psl::domain_str(host)
}

/// Whether `domain` is a public suffix (`com`, `co.uk`, `github.io`, an unknown TLD).
pub fn is_public_suffix(domain: &str) -> bool {
    let d = domain.trim_end_matches('.');
    psl::suffix_str(d) == Some(d)
}

/// The serialized origin (`https://example.com:8443`), or `null` for opaque origins.
pub fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// The site of a URL: scheme and registrable domain (`https://example.com`), the host when it
/// has none (an IP, `localhost`), or `null` for opaque origins.
pub fn site(url: &Url) -> String {
    match url.origin() {
        url::Origin::Opaque(_) => "null".into(),
        url::Origin::Tuple(scheme, host, _) => {
            let h = host.to_string();
            let reg = match host {
                url::Host::Domain(d) => registrable(&d).map(str::to_string),
                _ => None,
            };
            format!("{scheme}://{}", reg.unwrap_or(h))
        }
    }
}

/// Whether two URLs are same-origin.
pub fn same_origin(a: &Url, b: &Url) -> bool {
    a.origin() == b.origin()
}

/// Whether a URL is potentially trustworthy: `https`/`wss`, `file`, or a loopback or
/// `localhost` host.
pub fn trustworthy(url: &Url) -> bool {
    match url.scheme() {
        "https" | "wss" | "file" => true,
        _ => match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => {
                let d = d.trim_end_matches('.');
                d == "localhost" || d.ends_with(".localhost")
            }
            None => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn sites() {
        assert_eq!(
            site(&u("https://www.example.co.uk/x")),
            "https://example.co.uk"
        );
        assert_eq!(site(&u("https://a.github.io/")), "https://a.github.io");
        assert_eq!(
            site(&u("http://home.example.org:8888/")),
            "http://example.org"
        );
        assert_eq!(site(&u("http://127.0.0.1:3000/")), "http://127.0.0.1");
        assert_eq!(site(&u("http://a.test/")), "http://a.test");
        assert_ne!(site(&u("http://a.test/")), site(&u("https://a.test/")));
        assert!(is_public_suffix("co.uk"));
        assert!(is_public_suffix("com"));
        assert!(!is_public_suffix("example.com"));
        assert!(trustworthy(&u("http://localhost:3000/")));
        assert!(!trustworthy(&u("http://example.com/")));
    }
}
