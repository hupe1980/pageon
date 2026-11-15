//! The browser-compatible profile is one Chrome at every layer (feature `chrome`):
//! the TLS ClientHello's JA4, the HTTP/2 preface (Akamai fingerprint), the request headers and
//! what scripts read all equal the pinned Chrome 149. A mismatch is a bug.
#![cfg(feature = "chrome")]

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use pageon::{Env, Limits, Live, LoadOptions, Profile, Session};
use pageon_env::Request;

/// Chrome 149's JA4 (ALPS 0x44cd; no trust-anchors extension, which arrived in 152).
const JA4: &str = "t13d1516h2_8daaf6152771_d8a2da3f94cd";
/// Chrome's Akamai HTTP/2 fingerprint.
const AKAMAI: &str = "1:65536;2:0;4:6291456;6:262144|15663105|0|m,a,s,p";

fn tls_config(alpn: &[u8]) -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::pem::PemObject as _;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls");
    let certs = rustls::pki_types::CertificateDer::pem_file_iter(dir.join("cert.pem"))
        .unwrap_or_else(|e| panic!("{e}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("{e}"));
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(dir.join("key.pem"))
        .unwrap_or_else(|e| panic!("{e}"));
    let mut c = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap_or_else(|e| panic!("{e}"));
    c.alpn_protocols = vec![alpn.to_vec()];
    Arc::new(c)
}

/// A stream that first replays bytes already read (the ClientHello).
struct Prefixed {
    head: Vec<u8>,
    pos: usize,
    inner: TcpStream,
}

impl Read for Prefixed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.head.len() {
            let n = buf.len().min(self.head.len() - self.pos);
            buf[..n].copy_from_slice(&self.head[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

impl Write for Prefixed {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Reads the first TLS record: the ClientHello.
fn read_hello(s: &mut TcpStream) -> Vec<u8> {
    let mut head = [0u8; 5];
    s.read_exact(&mut head).unwrap_or_else(|e| panic!("{e}"));
    let len = u16::from_be_bytes([head[3], head[4]]) as usize;
    let mut rec = vec![0u8; len];
    s.read_exact(&mut rec).unwrap_or_else(|e| panic!("{e}"));
    let mut all = head.to_vec();
    all.extend_from_slice(&rec);
    all
}

fn tls(
    s: TcpStream,
    hello: Vec<u8>,
    config: Arc<rustls::ServerConfig>,
) -> rustls::StreamOwned<rustls::ServerConnection, Prefixed> {
    let conn = rustls::ServerConnection::new(config).unwrap_or_else(|e| panic!("{e}"));
    rustls::StreamOwned::new(
        conn,
        Prefixed {
            head: hello,
            pos: 0,
            inner: s,
        },
    )
}

fn grease(v: u16) -> bool {
    v & 0x0f0f == 0x0a0a && (v >> 8) == (v & 0xff)
}

fn sha12(s: &str) -> String {
    let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, s.as_bytes());
    d.as_ref()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// JA4 (FoxIO, BSD-3-Clause) of a ClientHello record.
fn ja4(rec: &[u8]) -> String {
    let u16_at = |i: usize| u16::from_be_bytes([rec[i], rec[i + 1]]);
    // record (5) + handshake header (4) + version (2) + random (32)
    let mut i = 5 + 4 + 2 + 32;
    i += 1 + rec[i] as usize; // session id
    let n = u16_at(i) as usize;
    let ciphers: Vec<u16> = (0..n / 2)
        .map(|k| u16_at(i + 2 + 2 * k))
        .filter(|c| !grease(*c))
        .collect();
    i += 2 + n;
    i += 1 + rec[i] as usize; // compression
    let end = i + 2 + u16_at(i) as usize;
    i += 2;
    let (mut exts, mut sigs, mut alpn, mut version, mut sni) =
        (Vec::new(), Vec::new(), String::from("00"), 0x0303u16, 'i');
    while i + 4 <= end {
        let (t, l) = (u16_at(i), u16_at(i + 2) as usize);
        let d = i + 4;
        if !grease(t) {
            exts.push(t);
        }
        match t {
            0x0000 => sni = 'd',
            0x0010 => {
                let p = rec[d + 2] as usize;
                let v = &rec[d + 3..d + 3 + p];
                alpn = format!("{}{}", v[0] as char, v[p - 1] as char);
            }
            0x002b => {
                let m = rec[d] as usize;
                version = (0..m / 2)
                    .map(|k| u16::from_be_bytes([rec[d + 1 + 2 * k], rec[d + 2 + 2 * k]]))
                    .filter(|v| !grease(*v))
                    .max()
                    .unwrap_or(version);
            }
            0x000d => {
                let m = u16::from_be_bytes([rec[d], rec[d + 1]]) as usize;
                sigs = (0..m / 2)
                    .map(|k| u16::from_be_bytes([rec[d + 2 + 2 * k], rec[d + 3 + 2 * k]]))
                    .filter(|v| !grease(*v))
                    .collect();
            }
            _ => {}
        }
        i = d + l;
    }
    let ver = match version {
        0x0304 => "13",
        0x0303 => "12",
        _ => "00",
    };
    let a = format!(
        "t{ver}{sni}{:02}{:02}{alpn}",
        ciphers.len().min(99),
        exts.len().min(99)
    );
    let mut c: Vec<String> = ciphers.iter().map(|c| format!("{c:04x}")).collect();
    c.sort();
    let mut e: Vec<String> = exts
        .iter()
        .filter(|e| !matches!(e, 0x0000 | 0x0010))
        .map(|e| format!("{e:04x}"))
        .collect();
    e.sort();
    let s: Vec<String> = sigs.iter().map(|s| format!("{s:04x}")).collect();
    format!(
        "{a}_{}_{}",
        sha12(&c.join(",")),
        sha12(&format!("{}_{}", e.join(","), s.join(",")))
    )
}

/// The Akamai fingerprint of an HTTP/2 connection's first frames, up to the first HEADERS.
fn akamai(r: &mut impl Read) -> String {
    let mut preface = [0u8; 24];
    r.read_exact(&mut preface).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    let (mut settings, mut window, mut priorities) = (String::new(), 0u32, 0);
    let pseudo = loop {
        let mut h = [0u8; 9];
        r.read_exact(&mut h).unwrap_or_else(|e| panic!("{e}"));
        let len = u32::from_be_bytes([0, h[0], h[1], h[2]]) as usize;
        let (ty, flags) = (h[3], h[4]);
        let mut p = vec![0u8; len];
        r.read_exact(&mut p).unwrap_or_else(|e| panic!("{e}"));
        match ty {
            4 if flags & 1 == 0 => {
                settings = p
                    .chunks(6)
                    .map(|c| {
                        format!(
                            "{}:{}",
                            u16::from_be_bytes([c[0], c[1]]),
                            u32::from_be_bytes([c[2], c[3], c[4], c[5]])
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(";");
            }
            8 => window = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff,
            2 => priorities += 1,
            1 => {
                let mut i = 0;
                if flags & 0x08 != 0 {
                    i += 1;
                }
                if flags & 0x20 != 0 {
                    i += 5;
                }
                break pseudo_order(&p[i..]);
            }
            _ => {}
        }
    };
    format!("{settings}|{window}|{priorities}|{pseudo}")
}

/// The pseudo-header order of an HPACK block, from static-table name indexes.
fn pseudo_order(b: &[u8]) -> String {
    let int = |b: &[u8], i: &mut usize, bits: u8| -> usize {
        let max = (1usize << bits) - 1;
        let mut v = (b[*i] as usize) & max;
        *i += 1;
        if v == max {
            let mut m = 0;
            loop {
                let x = b[*i];
                *i += 1;
                v += ((x & 0x7f) as usize) << m;
                m += 7;
                if x & 0x80 == 0 {
                    break;
                }
            }
        }
        v
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let (idx, literal) = if c & 0x80 != 0 {
            (int(b, &mut i, 7), false)
        } else if c & 0xc0 == 0x40 {
            (int(b, &mut i, 6), true)
        } else if c & 0xe0 == 0x20 {
            int(b, &mut i, 5);
            continue;
        } else {
            (int(b, &mut i, 4), true)
        };
        let name = match idx {
            1 => 'a',
            2 | 3 => 'm',
            4 | 5 => 'p',
            6 | 7 => 's',
            _ => break,
        };
        out.push(name.to_string());
        if literal {
            let n = int(b, &mut i, 7);
            i += n;
        }
    }
    out.join(",")
}

fn chrome() -> Live {
    Live::browser_compatible_trusting_any(Limits::default()).unwrap_or_else(|e| panic!("{e}"))
}

const PROBE: &str = r#"<p id=out></p><script>
document.getElementById('out').textContent = JSON.stringify({
  ua: navigator.userAgent, webdriver: navigator.webdriver, platform: navigator.platform,
  data: navigator.userAgentData ? navigator.userAgentData.toJSON() : null });
</script>"#;

#[test]
fn one_chrome_at_every_layer() {
    // HTTP/1.1 server: the ClientHello (JA4), the headers, and the page scripts read.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = listener.local_addr().map_or(0, |a| a.port());
    type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    let config = tls_config(b"http/1.1");
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { continue };
            let hello = read_hello(&mut s);
            let print = ja4(&hello);
            let mut t = tls(s, hello, config.clone());
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if t.read(&mut b).unwrap_or(0) == 0 {
                    break;
                }
                head.push(b[0]);
            }
            let text = String::from_utf8_lossy(&head).to_string();
            let headers = text
                .lines()
                .skip(1)
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            kept.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((print, headers));
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{PROBE}",
                PROBE.len()
            );
            let _ = t.write_all(resp.as_bytes());
            let _ = t.flush();
        }
    });
    let live = chrome();
    assert_eq!(live.profile(), Profile::chrome());
    let session = Session::new(&live);
    let options = LoadOptions {
        allow_private_network: true,
        robots: false,
        ..LoadOptions::default()
    };
    let tab = session
        .open(&format!("https://localhost:{port}/"), options)
        .unwrap_or_else(|e| panic!("{e}"));
    let v: serde_json::Value =
        serde_json::from_str(&common::text_of(tab.document(), "out")).unwrap_or_default();
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (print, headers) = seen.first().cloned().unwrap_or_default();
    assert_eq!(print, JA4, "TLS: the ClientHello is not Chrome 149's");
    let h = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    let p = Profile::chrome();
    assert_eq!(h("user-agent"), Some(p.user_agent()), "{headers:?}");
    for (k, want) in p.client_hints() {
        assert_eq!(h(k), Some(want), "{k}");
    }
    assert_eq!(
        h("accept-encoding").as_deref(),
        Some(pageon_env::chrome::ACCEPT_ENCODING)
    );
    assert_eq!(
        h("accept").as_deref(),
        Some(pageon_env::chrome::ACCEPT_DOCUMENT)
    );
    // Scripts read what the headers said.
    assert_eq!(v["ua"], p.user_agent());
    assert_eq!(v["webdriver"], false);
    assert_eq!(v["platform"], p.navigator().platform);
    let brands: Vec<(String, String)> = v["data"]["brands"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| {
            (
                b["brand"].as_str().unwrap_or_default().into(),
                b["version"].as_str().unwrap_or_default().into(),
            )
        })
        .collect();
    assert_eq!(Some(pageon_env::brand_header(&brands)), h("sec-ch-ua"));
}

#[test]
fn http2_is_chromes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = listener.local_addr().map_or(0, |a| a.port());
    let config = tls_config(b"h2");
    let found = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap_or_else(|e| panic!("{e}"));
        let hello = read_hello(&mut s);
        let mut t = tls(s, hello, config);
        akamai(&mut t)
    });
    let live = chrome();
    let mut req = Request::get(format!("https://localhost:{port}/"));
    req.allow_private = true;
    let _ = live.fetch(&req); // the server closes after the first HEADERS
    let akamai = found.join().unwrap_or_else(|_| panic!("the server failed"));
    assert_eq!(
        akamai, AKAMAI,
        "HTTP/2: the connection preface is not Chrome's"
    );
}

#[test]
fn the_agent_profile_is_not_chrome() {
    // The control: pageon's own rustls ClientHello has another fingerprint.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = listener.local_addr().map_or(0, |a| a.port());
    let found = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap_or_else(|e| panic!("{e}"));
        ja4(&read_hello(&mut s))
    });
    let mut req = Request::get(format!("https://localhost:{port}/"));
    req.allow_private = true;
    let _ = Live::default().fetch(&req);
    let print = found.join().unwrap_or_else(|_| panic!("the server failed"));
    assert_ne!(print, JA4);
    assert!(print.starts_with("t13d"), "{print}");
}
