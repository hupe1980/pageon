//! Random documents and stylesheets through layout and every query: nothing panics.

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
    "div", "span", "p", "table", "tr", "td", "ul", "li", "a", "button", "input", "select",
    "textarea", "img", "label", "svg", "section", "pre", "b", "dialog", "details", "summary",
    "iframe", "math",
];
const DECLS: &[&str] = &[
    "display:flex",
    "display:grid",
    "display:none",
    "display:contents",
    "display:table",
    "display:inline-block",
    "float:left",
    "float:right",
    "position:absolute",
    "position:fixed",
    "position:sticky",
    "position:relative",
    "width:-10px",
    "width:1e9px",
    "height:100%",
    "min-height:100vh",
    "margin:-500px",
    "padding:50%",
    "opacity:0",
    "visibility:hidden",
    "transform:rotate(45deg)",
    "overflow:hidden",
    "z-index:-1",
    "z-index:99999",
    "grid-template-columns:repeat(1000,1fr)",
    "flex:1 1 0",
    "columns:3",
    "writing-mode:vertical-rl",
    "font-size:0",
    "font-size:500px",
    "line-height:-1",
    "white-space:pre",
    "content:'x'",
    "background:url(x.png)",
    "left:50%",
];

fn doc(rng: &mut Lcg) -> String {
    let mut s = String::from("<!DOCTYPE html><style>");
    for _ in 0..rng.below(6) {
        s.push_str(&format!(
            "{} {{ {}; {} }} ",
            rng.pick(TAGS),
            rng.pick(DECLS),
            rng.pick(DECLS)
        ));
    }
    s.push_str("@import url(x.css); @media (max-width: 1px) { * { display: none } }</style>");
    let mut open = Vec::new();
    for _ in 0..rng.below(120) {
        match rng.below(5) {
            0 if !open.is_empty() => {
                let t: &str = open.pop().unwrap_or("div");
                s.push_str(&format!("</{t}>"));
            }
            1 => s.push_str("text with some words "),
            _ => {
                let t = rng.pick(TAGS);
                s.push_str(&format!(
                    "<{t} style=\"{};{}\">",
                    rng.pick(DECLS),
                    rng.pick(DECLS)
                ));
                open.push(t);
            }
        }
    }
    s
}

#[test]
fn random_layouts_never_panic() {
    let mut rng = Lcg(3);
    for _ in 0..2_000 {
        let html = doc(&mut rng);
        let d = pageon::parse(&html, "https://a.test/");
        let vp = pageon::Viewport {
            width: 1 + rng.below(2000) as u32,
            height: 1 + rng.below(2000) as u32,
        };
        let layout = d.layout(vp);
        let ids: Vec<_> = d
            .dom()
            .traverse(d.root())
            .filter_map(|e| match e {
                pageon_dom::Edge::Enter(id) => Some(id),
                pageon_dom::Edge::Leave(_) => None,
            })
            .collect();
        for id in ids.iter().take(60) {
            let _ = (
                layout.box_of(*id),
                layout.visibility(*id),
                layout.in_viewport(*id),
                layout.covered_by(*id),
            );
            let _ = layout.style(*id, "display");
        }
        let _ = layout.element_at(rng.below(2100) as f32, rng.below(2100) as f32);
        let _ = d.boxes(vp);
    }
}
