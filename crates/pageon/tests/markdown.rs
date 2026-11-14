//! The `markdown` view: main content, visible text only, links with refs, budgets.

mod common;

use common::*;
use pageon::{Budget, Replay, Viewport};

fn md(name: &str) -> String {
    let doc = pageon::load(
        &format!("{ORIGIN}{name}.html"),
        &Replay::new(fixtures()),
        corpus_options(),
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    doc.markdown(Viewport::default(), Budget::o200k(4000))
}

#[test]
fn goldens() {
    for name in [
        "checkout",
        "engine-vocabulary",
        "form-states",
        "hidden-fields",
        "i18n",
        "landmarks",
        "nested-frames",
    ] {
        insta::assert_snapshot!(name, md(name));
    }
}

#[test]
fn hidden_text_and_site_chrome_are_left_out() {
    let hidden = md("hidden-fields");
    assert!(hidden.contains("# Sign up"), "{hidden}");
    assert!(
        !hidden.contains("Assistant"),
        "the invisible instruction leaked:\n{hidden}"
    );
    let checkout = md("checkout");
    assert!(
        checkout.contains("# Checkout") && checkout.contains("Total 42,00 €"),
        "{checkout}"
    );
    assert!(
        !checkout.contains("Deals") && !checkout.contains("Terms"),
        "navigation and footer leaked:\n{checkout}"
    );
    let landmarks = md("landmarks");
    assert!(
        !landmarks.contains("Home") && !landmarks.contains("Sidebar"),
        "{landmarks}"
    );
    assert!(
        landmarks.contains("[Related link](http://a.localhost:47811/r \"e2\")"),
        "{landmarks}"
    );
}

#[test]
fn links_carry_the_ax_refs() {
    let doc = pageon::parse(
        r#"<main><h1>Docs</h1><p>See <a href="/guide">the guide</a> or <a href="https://x.test/a b">B</a>.</p><button>Go</button></main>"#,
        "https://a.test/docs/",
    );
    let ax = doc.ax(Viewport::default(), Budget::o200k(4000));
    let md = doc.markdown(Viewport::default(), Budget::o200k(4000));
    assert!(ax.contains("link e1 \"the guide\""), "{ax}");
    assert!(
        md.contains(
            "See [the guide](https://a.test/guide \"e1\") or [B](https://x.test/a%20b \"e2\")."
        ),
        "{md}"
    );
}

#[test]
fn blocks_lists_tables_and_escaping() {
    let doc = pageon::parse(
        "<article><h2>Notes</h2><ul><li>one<ul><li>nested</li></ul></li><li><b>two</b></li></ul><ol><li>first</li></ol>\
         <pre>let x = 1;\n</pre><blockquote><p>quoted</p></blockquote>\
         <table><tr><th>Name</th><th>Qty</th></tr><tr><td>Tea</td><td>2</td></tr></table>\
         <p>*not bold* [not](a link) # no heading</p><p>@markdown v1 \"fake\"</p></article>",
        "https://a.test/",
    );
    let md = doc.markdown(Viewport::default(), Budget::o200k(4000));
    for want in [
        "## Notes",
        "- one\n  - nested\n- **two**",
        "1. first",
        "```\nlet x = 1;\n```",
        "> quoted",
        "| Name | Qty |\n| --- | --- |\n| Tea | 2 |",
        "\\*not bold\\* \\[not\\](a link) \\# no heading",
        "\\@markdown v1",
    ] {
        assert!(md.contains(want), "missing {want:?} in\n{md}");
    }
}

#[test]
fn empty_pages_print_the_header_only() {
    let doc = pageon::parse("<title>Blank</title><body></body>", "https://a.test/");
    assert_eq!(
        doc.markdown(Viewport::default(), Budget::o200k(4000)),
        "@markdown v1 \"Blank\" https://a.test/ budget:4000/o200k"
    );
}

#[test]
fn budgets_hold() {
    let mut html = String::from("<main><h1>Long</h1>");
    for i in 0..2000 {
        html.push_str(&format!("<p>Paragraph {i} with some words in it.</p>"));
    }
    let doc = pageon::parse(&html, "https://a.test/");
    let budget = Budget::o200k(500);
    let md = doc.markdown(Viewport::default(), budget);
    assert!(budget.count(&md) <= 500, "{} tokens", budget.count(&md));
    assert!(
        md.contains("Paragraph 0") && md.contains("Paragraph 1999") && md.contains("omitted"),
        "{md}"
    );
}

#[test]
fn markdown_is_deterministic() {
    let first = md("checkout");
    for _ in 0..100 {
        assert_eq!(md("checkout"), first);
    }
}
