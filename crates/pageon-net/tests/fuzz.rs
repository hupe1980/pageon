//! random `Set-Cookie` strings, cookie dates and URLs never panic the jar, the site
//! logic or the egress check.

use pageon_net::cookie::{Context, Jar, parse_date};
use pageon_net::site;
use url::Url;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() as usize) % v.len()]
    }
}

const PIECES: &[&str] = &[
    "a",
    "=",
    ";",
    " ",
    "\t",
    "\u{0}",
    "\u{7f}",
    "é",
    "Domain",
    "domain=",
    ".",
    "..",
    "co.uk",
    "a.test",
    "localhost",
    "Path=",
    "/",
    "//",
    "Expires=",
    "Wed, 21 Oct 2015 07:28:00 GMT",
    "99:99:99",
    "Feb 30 1600",
    "Max-Age=",
    "-1",
    "0",
    "99999999999999999999",
    "Secure",
    "HttpOnly",
    "SameSite=",
    "None",
    "Lax",
    "Strict",
    "Partitioned",
    "__Host-",
    "__Secure-",
    "\"",
    ",",
    "1.2.3.4",
    "[::1]",
    "%",
    "\u{fffd}",
];

const URLS: &[&str] = &[
    "https://a.test/",
    "http://a.test/x/y",
    "https://www.example.co.uk/a",
    "http://127.0.0.1:8080/",
    "http://[::1]/",
    "https://localhost/",
    "file:///tmp/x",
    "http://xn--nxasmq6b.test/",
    "https://a.github.io/p",
    "http://0x7f.1/",
];

fn random_text(rng: &mut Lcg) -> String {
    (0..rng.next() % 12).map(|_| rng.pick(PIECES)).collect()
}

#[test]
fn random_cookies_dates_and_urls_never_panic() {
    let mut rng = Lcg(7);
    let mut jar = Jar::default();
    for i in 0..10_000u64 {
        let url = Url::parse(rng.pick(URLS)).unwrap_or_else(|e| panic!("{e}"));
        let top = site::site(&url);
        let cx = Context {
            url: &url,
            top_site: &top,
            same_site: rng.next().is_multiple_of(2),
            top_level_safe_nav: rng.next().is_multiple_of(3),
            script: rng.next().is_multiple_of(4),
        };
        let header = random_text(&mut rng);
        let _ = jar.set(&header, &cx, 1_800_000_000_000 + i);
        let _ = jar.header(&cx, 1_800_000_000_000 + i);
        let _ = parse_date(&random_text(&mut rng));
        let raw = format!("http://{}/", random_text(&mut rng));
        if let Ok(u) = Url::parse(&raw) {
            let _ = site::site(&u);
            let _ = site::trustworthy(&u);
            let _ = pageon_env::private_host_of(u.as_str());
        }
    }
    assert!(jar.all(0).len() <= pageon_net::cookie::MAX_TOTAL);
}
