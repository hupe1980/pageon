//! Randomized inputs through every new input-handling path: nothing panics, every case ends.

mod common;

use pageon::{Dump, LoadOptions, Replay};
use pageon_dom::encoding;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

const PIECES: &[&str] = &[
    "<",
    ">",
    "</",
    "<!--",
    "-->",
    "<!DOCTYPE html>",
    "<meta charset=",
    "\"",
    "'",
    "=",
    "<table>",
    "<td>",
    "<a>",
    "<svg>",
    "<math>",
    "<template>",
    "<select>",
    "<option selected>",
    "<selectedcontent>",
    "<plaintext>",
    "<script>",
    "</script>",
    "<p>",
    "<b>",
    "</b>",
    "&amp;",
    "&#x0;",
    "\0",
    "\r\n",
    "é",
    "日本",
    "\u{feff}",
    "<frameset>",
    "<noscript>",
    " ",
    "x",
    "<form>",
    "<button>",
    "<pre>\n",
    "<textarea>",
    "<!x",
    "<?pi?>",
    "<!--x--!>",
];

fn input(rng: &mut Lcg) -> Vec<u8> {
    let mut s = Vec::new();
    for _ in 0..rng.below(80) {
        match rng.below(10) {
            0 => s.extend((0..rng.below(16)).map(|_| rng.next() as u8)),
            1 => s.extend(
                std::iter::repeat_n(b'<', 1)
                    .chain(b"div>".iter().copied())
                    .cycle()
                    .take(5 * rng.below(400)),
            ),
            _ => s.extend(PIECES[rng.below(PIECES.len())].as_bytes()),
        }
    }
    if rng.below(4) == 0 {
        let cut = rng.below(s.len() + 1);
        s.truncate(cut);
    }
    s
}

#[test]
fn random_documents_never_panic() {
    let mut rng = Lcg(7);
    for _ in 0..10_000 {
        let bytes = input(&mut rng);
        let charset = ["utf-8", "shift_jis", "utf-16le", "bogus", ""][rng.below(5)];
        let (text, _) = encoding::decode(&bytes, Some(charset).filter(|c| !c.is_empty()));
        let doc = pageon::parse(&text, "https://a.test/");
        for kind in [Dump::Text, Dump::Html, Dump::Tree] {
            let _ = doc.dump(kind);
        }
        let _ = encoding::prescan(&bytes);
    }
}

#[test]
fn malformed_urls_and_recordings_never_panic() {
    let mut rng = Lcg(11);
    let urls = [
        "",
        " ",
        "http://",
        "https://[::1",
        "file://",
        "file:///../../etc",
        "javascript:x",
        "//x",
        "a:b",
        "http://a.test/%zz",
        "http://xn--/",
        "C:\\x",
    ];
    for _ in 0..1_000 {
        let mut u = urls[rng.below(urls.len())].to_string();
        u.push_str(std::str::from_utf8(&input(&mut rng)).unwrap_or(""));
        let loc = urls[rng.below(urls.len())];
        let l = common::log(vec![
            common::redirect("https://a.test/", loc),
            common::page(
                "https://a.test/x",
                rng.below(600) as u16,
                &[("content-type", PIECES[rng.below(PIECES.len())])],
                "<p>",
            ),
        ]);
        let _ = pageon::load(&u, &Replay::new(l.clone()), LoadOptions::default());
        let _ = pageon::load("https://a.test/", &Replay::new(l), LoadOptions::default());
        let junk = String::from_utf8_lossy(&input(&mut rng)).into_owned();
        let _ = serde_json::from_str::<pageon_env::Log>(&junk);
    }
}
