//! The incremental parser builds the same tree as `parse_document` for every html5lib input,
//! however the input is split, and stops once per closed `<script>`.

use pageon_dom::{ParseOptions, Parser, Step, parse_document};

fn inputs() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/html5lib");
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(Result::ok)
        .collect();
    files.sort_by_key(|e| e.file_name());
    for f in files
        .iter()
        .filter(|f| f.path().extension().is_some_and(|x| x == "dat"))
    {
        let text = std::fs::read_to_string(f.path()).unwrap_or_default();
        for case in text
            .split("\n#data\n")
            .map(|c| c.trim_start_matches("#data\n"))
        {
            if case.contains("#document-fragment") {
                continue;
            }
            if let Some(data) = case.split("\n#errors").next() {
                out.push(data.to_string());
            }
        }
    }
    out
}

#[test]
fn same_trees_as_parse_document() {
    let all = inputs();
    assert!(all.len() > 1000, "{} inputs", all.len());
    for (i, html) in all.iter().enumerate() {
        for scripting in [false, true] {
            let options = ParseOptions { scripting };
            let want = parse_document(html, options);
            let mut p = Parser::new(options);
            // Feed in two pieces at a char boundary to exercise resumption.
            let mid = (0..=html.len() / 2)
                .rev()
                .find(|&k| html.is_char_boundary(k))
                .unwrap_or(0);
            p.feed(&html[..mid]);
            while let Step::Script(_) = p.step() {}
            p.feed(&html[mid..]);
            while let Step::Script(_) = p.step() {}
            let got = p.finish();
            let got = got.borrow();
            assert_eq!(
                got.to_html(got.root()),
                want.to_html(want.root()),
                "case {i}: {html:?}"
            );
        }
    }
}

#[test]
fn pauses_once_per_script() {
    let mut p = Parser::new(ParseOptions { scripting: true });
    p.feed("<head><script>a</script></head><body><p>x<script>b</script>y</p><svg><script>c</script></svg>");
    let mut seen = Vec::new();
    while let Step::Script(n) = p.step() {
        let dom = p.dom();
        let dom = dom.borrow();
        let text: String = dom
            .children(n)
            .filter_map(|c| match &dom.get(c)?.data {
                pageon_dom::NodeData::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        seen.push(text);
    }
    // HTML scripts pause with their text in place; html5ever does not pause for SVG scripts.
    assert_eq!(seen, ["a", "b"]);
}
