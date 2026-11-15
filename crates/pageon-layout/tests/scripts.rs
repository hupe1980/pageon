//! Complex scripts (Chinese, Japanese, Thai, Lao, Khmer, Burmese) break lines by ICU4X's
//! dictionaries, as browsers do, and lay out without a single data warning.

use std::sync::atomic::{AtomicUsize, Ordering};

use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport};

static WARNINGS: AtomicUsize = AtomicUsize::new(0);

struct Counter;

impl log::Log for Counter {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= log::Level::Warn
    }

    fn log(&self, r: &log::Record<'_>) {
        if r.level() <= log::Level::Warn {
            eprintln!("{}: {}", r.target(), r.args());
            WARNINGS.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn flush(&self) {}
}

fn find(dom: &Dom, id: &str) -> NodeId {
    dom.traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("#{id} missing"))
}

#[test]
fn complex_scripts_break_by_dictionary_without_warnings() {
    let _ = log::set_logger(&Counter);
    log::set_max_level(log::LevelFilter::Warn);
    // Thai has no spaces between words: without a dictionary a line cannot break inside it.
    let html = "<div id=th style='width:120px;font-size:16px;line-height:20px'>\
                ภาษาไทยเป็นภาษาที่ไม่มีการเว้นวรรคระหว่างคำในประโยค</div>\
                <div id=ja style='width:120px;font-size:16px;line-height:20px'>\
                日本語の文章は単語の間に空白を入れずに書かれます。</div>\
                <p>中文 ລາວ ខ្មែរ မြန်မာ</p>";
    let dom = parse_document(html, ParseOptions::default());
    let layout = Layout::new(&dom, Viewport::default(), &[], None);
    assert!(layout.error().is_none(), "{:?}", layout.error());
    for id in ["th", "ja"] {
        let r = layout
            .client_rect(find(&dom, id))
            .unwrap_or_else(|| panic!("no box for #{id}"));
        assert!(r.height >= 39.0, "#{id} did not wrap: {r:?}");
    }
    assert_eq!(
        WARNINGS.load(Ordering::SeqCst),
        0,
        "ICU4X warned while laying out"
    );
}
