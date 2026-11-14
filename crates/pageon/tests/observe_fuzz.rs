//! Random documents with ARIA attributes, label graphs and `srcdoc` frames through the
//! accessibility tree, `ax`, `markdown` and ref resolution: nothing panics.

use pageon::{Budget, Episode, Mode, Viewport};

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
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[self.below(v.len())]
    }
}

const TAGS: &[&str] = &[
    "div",
    "span",
    "p",
    "a",
    "button",
    "input",
    "select",
    "option",
    "textarea",
    "label",
    "h1",
    "h3",
    "ul",
    "li",
    "table",
    "tr",
    "td",
    "th",
    "img",
    "main",
    "nav",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "details",
    "summary",
    "fieldset",
    "legend",
    "form",
    "dialog",
    "iframe",
    "pre",
    "blockquote",
    "b",
    "em",
    "code",
    "svg",
    "title",
];
const ATTRS: &[&str] = &[
    "role=\"button link\"",
    "role=\"none\"",
    "role=\"presentation\"",
    "role=\"region\"",
    "role=\"ReGiOn group\"",
    "role=\"checkbox\" aria-checked=\"mixed\"",
    "aria-hidden=\"true\"",
    "aria-label=\"L\"",
    "aria-label=\"\"",
    "aria-labelledby=\"a b a\"",
    "aria-labelledby=\"self\"",
    "id=\"a\"",
    "id=\"b\"",
    "id=\"self\"",
    "for=\"a\"",
    "hidden",
    "style=\"display:none\"",
    "style=\"opacity:0\"",
    "style=\"visibility:hidden\"",
    "tabindex=\"0\"",
    "href=\"/x\"",
    "href=\"javascript:void(0)\"",
    "alt=\"\"",
    "alt=\"pic\"",
    "title=\"T\"",
    "type=\"checkbox\" checked",
    "type=\"radio\"",
    "type=\"range\"",
    "type=\"password\" value=\"secret\"",
    "value=\"v\"",
    "disabled",
    "required",
    "aria-invalid=\"spelling\"",
    "aria-expanded=\"true\"",
    "open",
    "selected",
    "multiple",
    "size=\"3\"",
    "srcdoc=\"<button aria-labelledby=x id=x>in</button><iframe srcdoc=y></iframe>\"",
    "src=\"https://elsewhere.test/\"",
    "onclick=\"x()\"",
    "aria-modal=\"true\"",
    "aria-level=\"9\"",
];

fn doc(rng: &mut Lcg) -> String {
    let mut s = String::from("<!DOCTYPE html><title>Fuzz</title>");
    let mut open = Vec::new();
    for _ in 0..rng.below(150) {
        match rng.below(6) {
            0 if !open.is_empty() => {
                let t: &str = open.pop().unwrap_or("div");
                s.push_str(&format!("</{t}>"));
            }
            1 => s.push_str(
                [
                    "words ",
                    "* [x](y) # @markdown ",
                    "  ",
                    "é漢字 ",
                    "| a | b |",
                ][rng.below(5)],
            ),
            _ => {
                let t = rng.pick(TAGS);
                let mut attrs = String::new();
                for _ in 0..rng.below(4) {
                    attrs.push(' ');
                    attrs.push_str(rng.pick(ATTRS));
                }
                s.push_str(&format!("<{t}{attrs}>"));
                open.push(t);
            }
        }
    }
    s
}

#[test]
fn random_documents_observe_without_panic() {
    let mut rng = Lcg(11);
    for _ in 0..2_000 {
        let html = doc(&mut rng);
        let d = pageon::parse(&html, "https://a.test/");
        let vp = Viewport {
            width: 1 + rng.below(1600) as u32,
            height: 1 + rng.below(1200) as u32,
        };
        let budget = Budget::o200k(50 + rng.below(3000));
        let ax = d.ax(vp, budget);
        assert!(
            budget.count(&ax) <= budget.max_tokens,
            "ax over budget for\n{html}"
        );
        let md = d.markdown(vp, budget);
        assert!(
            budget.count(&md) <= budget.max_tokens,
            "markdown over budget for\n{html}"
        );
        let mut ep = Episode::new(budget, Mode::Diff);
        let cap = d.capture(vp);
        let _ = ep.observe(&cap);
        if let Ok(t) = ep.resolve("e1", &cap) {
            let _ = d.node_for(&t);
        }
    }
}
