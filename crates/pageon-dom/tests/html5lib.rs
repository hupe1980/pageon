//! html5lib tree-construction suite through pageon's DOM, differential against html5ever's
//! reference DOM (`markup5ever_rcdom`): pageon must print the same tree as the reference for
//! every case, except where pageon matches the expected tree and the reference does not.
//! Cases pageon does not pass are listed in `tests/html5lib/KNOWN.md`
//! (regenerate with `UPDATE_KNOWN=1 cargo test -p pageon-dom --test html5lib`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use html5ever::tendril::TendrilSink;
use html5ever::{LocalName, ParseOpts, QualName, ns};
use markup5ever_rcdom::{Handle, NodeData as RcData, RcDom};
use pageon_dom::{Dom, Namespace, NodeData, NodeId, ParseOptions, parse_document, parse_fragment};

#[derive(Debug, Default)]
struct Case {
    file: String,
    index: usize,
    data: String,
    fragment: Option<String>,
    scripting: bool,
    expected: String,
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/html5lib")
}

fn cases() -> Vec<Case> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "dat"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("{e}"));
        let name = f
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut cur: Option<Case> = None;
        let mut section = "";
        let mut index = 0;
        for line in text.split('\n') {
            if line == "#data" {
                if let Some(c) = cur.take() {
                    out.push(finish(c));
                }
                index += 1;
                cur = Some(Case {
                    file: name.clone(),
                    index,
                    scripting: true,
                    ..Case::default()
                });
                section = "data";
                continue;
            }
            let Some(c) = cur.as_mut() else { continue };
            match line {
                "#errors" | "#new-errors" => section = "errors",
                "#document-fragment" => section = "fragment",
                "#script-off" => c.scripting = false,
                "#script-on" => c.scripting = true,
                "#document" => section = "document",
                _ => match section {
                    "data" => {
                        c.data.push_str(line);
                        c.data.push('\n');
                    }
                    "fragment" => c.fragment = Some(line.to_string()),
                    "document" => {
                        c.expected.push_str(line);
                        c.expected.push('\n');
                    }
                    _ => {}
                },
            }
        }
        if let Some(c) = cur.take() {
            out.push(finish(c));
        }
    }
    out
}

fn finish(mut c: Case) -> Case {
    if c.data.ends_with('\n') {
        c.data.pop();
    }
    c.expected = c.expected.trim_end_matches('\n').to_string();
    c
}

fn prefix(ns: Namespace) -> &'static str {
    match ns {
        Namespace::Svg => "svg ",
        Namespace::MathMl => "math ",
        _ => "",
    }
}

fn attr_prefix(ns: Namespace) -> &'static str {
    match ns {
        Namespace::Xlink => "xlink ",
        Namespace::Xml => "xml ",
        Namespace::Xmlns => "xmlns ",
        _ => "",
    }
}

fn print_pageon(dom: &Dom, id: NodeId, depth: usize, out: &mut String) {
    // Iterative to survive deep nesting.
    let mut stack = vec![(id, depth)];
    while let Some((n, d)) = stack.pop() {
        let Some(node) = dom.get(n) else { continue };
        let pad = "  ".repeat(d);
        let mut kids: Vec<(NodeId, usize)> = dom.children(n).map(|k| (k, d + 1)).collect();
        match &node.data {
            NodeData::Element {
                attrs,
                template_contents,
                ..
            } => {
                let ns = node.namespace().unwrap_or(Namespace::Html);
                let _ = writeln!(
                    out,
                    "| {pad}<{}{}>",
                    prefix(ns),
                    node.local_name().unwrap_or_default()
                );
                let mut a: Vec<String> = attrs
                    .iter()
                    .map(|a| {
                        format!(
                            "{}{}=\"{}\"",
                            attr_prefix(a.namespace()),
                            a.local_name(),
                            a.value
                        )
                    })
                    .collect();
                a.sort();
                for x in a {
                    let _ = writeln!(out, "| {pad}  {x}");
                }
                if let Some(t) = template_contents {
                    let _ = writeln!(out, "| {pad}  content");
                    kids = dom.children(*t).map(|k| (k, d + 2)).collect();
                }
            }
            NodeData::Text(t) => {
                let _ = writeln!(out, "| {pad}\"{t}\"");
            }
            // Declarative shadow roots never reach the html5lib dumps (no fixture uses them in
            // a way the runner checks); listed for completeness.
            NodeData::ShadowRoot { .. } => {
                let _ = writeln!(out, "| {pad}#shadow-root");
            }
            NodeData::Comment(t) => {
                let _ = writeln!(out, "| {pad}<!-- {t} -->");
            }
            NodeData::Doctype {
                name,
                public_id,
                system_id,
            } => {
                if public_id.is_empty() && system_id.is_empty() {
                    let _ = writeln!(out, "| {pad}<!DOCTYPE {name}>");
                } else {
                    let _ = writeln!(
                        out,
                        "| {pad}<!DOCTYPE {name} \"{public_id}\" \"{system_id}\">"
                    );
                }
            }
            NodeData::ProcessingInstruction { target, data } => {
                let _ = writeln!(out, "| {pad}<?{target} {data}>");
            }
            NodeData::Document | NodeData::DocumentFragment => {
                stack.extend(
                    dom.children(n)
                        .map(|k| (k, d))
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                );
                continue;
            }
        }
        stack.extend(kids.into_iter().rev());
    }
}

fn rc_ns(ns: &html5ever::Namespace) -> Namespace {
    match &**ns {
        "http://www.w3.org/2000/svg" => Namespace::Svg,
        "http://www.w3.org/1998/Math/MathML" => Namespace::MathMl,
        "http://www.w3.org/1999/xlink" => Namespace::Xlink,
        "http://www.w3.org/XML/1998/namespace" => Namespace::Xml,
        "http://www.w3.org/2000/xmlns/" => Namespace::Xmlns,
        _ => Namespace::Html,
    }
}

fn print_rc(h: &Handle, depth: usize, out: &mut String) {
    let mut stack = vec![(h.clone(), depth)];
    while let Some((n, d)) = stack.pop() {
        let pad = "  ".repeat(d);
        let mut kids: Vec<(Handle, usize)> = n
            .children
            .borrow()
            .iter()
            .map(|k| (k.clone(), d + 1))
            .collect();
        match &n.data {
            RcData::Element {
                name,
                attrs,
                template_contents,
                ..
            } => {
                let _ = writeln!(out, "| {pad}<{}{}>", prefix(rc_ns(&name.ns)), name.local);
                let mut a: Vec<String> = attrs
                    .borrow()
                    .iter()
                    .map(|a| {
                        format!(
                            "{}{}=\"{}\"",
                            attr_prefix(rc_ns(&a.name.ns)),
                            a.name.local,
                            a.value
                        )
                    })
                    .collect();
                a.sort();
                for x in a {
                    let _ = writeln!(out, "| {pad}  {x}");
                }
                if let Some(t) = template_contents.borrow().as_ref() {
                    let _ = writeln!(out, "| {pad}  content");
                    kids = t
                        .children
                        .borrow()
                        .iter()
                        .map(|k| (k.clone(), d + 2))
                        .collect();
                }
            }
            RcData::Text { contents } => {
                let _ = writeln!(out, "| {pad}\"{}\"", contents.borrow());
            }
            RcData::Comment { contents } => {
                let _ = writeln!(out, "| {pad}<!-- {contents} -->");
            }
            RcData::Doctype {
                name,
                public_id,
                system_id,
            } => {
                if public_id.is_empty() && system_id.is_empty() {
                    let _ = writeln!(out, "| {pad}<!DOCTYPE {name}>");
                } else {
                    let _ = writeln!(
                        out,
                        "| {pad}<!DOCTYPE {name} \"{public_id}\" \"{system_id}\">"
                    );
                }
            }
            RcData::ProcessingInstruction { target, contents } => {
                let _ = writeln!(out, "| {pad}<?{target} {contents}>");
            }
            RcData::Document => {
                stack.extend(kids.into_iter().map(|(k, _)| (k, d)).rev());
                continue;
            }
        }
        stack.extend(kids.into_iter().rev());
    }
}

fn run_pageon(c: &Case) -> String {
    let opts = ParseOptions {
        scripting: c.scripting,
    };
    let mut out = String::new();
    match &c.fragment {
        None => {
            let dom = parse_document(&c.data, opts);
            print_pageon(&dom, dom.root(), 0, &mut out);
        }
        Some(ctx) => {
            let dom = parse_fragment(&c.data, ctx, opts);
            if let Some(html) = dom.first_child(dom.root()) {
                for k in dom.children(html) {
                    print_pageon(&dom, k, 0, &mut out);
                }
            }
        }
    }
    out.trim_end_matches('\n').to_string()
}

fn run_rc(c: &Case) -> String {
    let mut opts = ParseOpts::default();
    opts.tree_builder.scripting_enabled = c.scripting;
    let mut out = String::new();
    match &c.fragment {
        None => {
            let dom = html5ever::parse_document(RcDom::default(), opts).one(c.data.as_str());
            print_rc(&dom.document, 0, &mut out);
        }
        Some(ctx) => {
            let (namespace, local) = match ctx.split_once(' ') {
                Some(("svg", l)) => (ns!(svg), l),
                Some(("math", l)) => (ns!(mathml), l),
                _ => (ns!(html), ctx.as_str()),
            };
            let name = QualName::new(None, namespace, LocalName::from(local));
            let dom =
                html5ever::parse_fragment(RcDom::default(), opts, name, Vec::new(), c.scripting)
                    .one(c.data.as_str());
            let html = dom.document.children.borrow()[0].clone();
            for k in html.children.borrow().iter() {
                print_rc(k, 0, &mut out);
            }
        }
    }
    out.trim_end_matches('\n').to_string()
}

#[test]
fn html5lib_tree_construction_matches_reference() {
    let all = cases();
    assert!(
        all.len() > 1500,
        "suite not found or truncated: {} cases",
        all.len()
    );
    let (mut ours_pass, mut ref_pass) = (0, 0);
    let mut known = Vec::new();
    let mut divergent = Vec::new();
    for c in &all {
        let ours = run_pageon(c);
        let reference = run_rc(c);
        let ok = ours == c.expected;
        ours_pass += usize::from(ok);
        ref_pass += usize::from(reference == c.expected);
        if !ok {
            let why = if reference == c.expected {
                "pageon differs from the reference"
            } else {
                "reference parser"
            };
            known.push(format!("| {} #{} | {} |", c.file, c.index, why));
        }
        if ours != reference && !ok {
            divergent.push(format!(
                "{} #{}\n--- data\n{}\n--- pageon\n{}\n--- reference\n{}",
                c.file, c.index, c.data, ours, reference
            ));
        }
    }
    assert!(
        divergent.is_empty(),
        "{} cases differ from the reference without matching the expectation:\n{}",
        divergent.len(),
        divergent.join("\n\n")
    );
    assert!(
        ours_pass >= ref_pass,
        "pageon passes {ours_pass}, reference passes {ref_pass}"
    );
    let report = format!(
        "# Known html5lib differences\n\n{ours_pass} of {} cases pass (reference DOM: {ref_pass}). Every case below fails\n\
         identically in html5ever's reference DOM, so the cause is the parser, not pageon's DOM.\n\n\
         | Case | Cause |\n| --- | --- |\n{}\n",
        all.len(),
        known.join("\n")
    );
    let path = dir().join("KNOWN.md");
    if std::env::var_os("UPDATE_KNOWN").is_some() {
        std::fs::write(&path, &report).unwrap_or_else(|e| panic!("{e}"));
    } else {
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, report,
            "KNOWN.md is out of date; run with UPDATE_KNOWN=1 and review the diff"
        );
    }
}
