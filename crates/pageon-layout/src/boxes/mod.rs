//! pageon's box tree and layout: boxes built from the document and the styler's styles, laid
//! out with Taffy (block, flex, grid, tables) and Parley (inline formatting contexts).

// Being ported: parts are not used until the switch.
#![allow(dead_code)]

pub(crate) mod construct;
pub(crate) mod paint;
pub(crate) mod inline;
pub(crate) mod layout;
pub(crate) mod list;
pub(crate) mod parley_style;
pub(crate) mod replaced;
pub(crate) mod resolved;
pub(crate) mod table;
pub(crate) mod text_transform;
pub(crate) mod tree;

use style::values::specified::box_::{DisplayInside, DisplayOutside};
use taffy::AvailableSpace;

use self::tree::{BoxId, Kind, Node, Tree};

/// Builds the box tree of `dom` and lays it out at the viewport.
pub(crate) fn lay_out(dom: &pageon_dom::Dom, build: &tree::Build<'_>) -> Tree {
    let mut tree = Tree::build(dom, build);
    let mut fonts = crate::fonts::context();
    let mut layouts = parley::LayoutContext::new();
    construct::Construct::new(&mut tree, build.styler, &mut fonts, &mut layouts).run();
    // Layout starts at the root element.
    let root = tree[tree.root()]
        .children
        .iter()
        .copied()
        .find(|&c| tree[c].kind == Kind::Element);
    if let Some(root) = root {
        let available = taffy::Size {
            width: AvailableSpace::Definite(build.viewport.width as f32),
            height: AvailableSpace::Definite(build.viewport.height as f32),
        };
        let mut pass = layout::Pass {
            tree: &mut tree,
            fonts: &mut fonts,
            layouts: &mut layouts,
        };
        taffy::compute_root_layout(&mut pass, taffy::NodeId::from(root), available);
        taffy::round_layout(&mut pass, taffy::NodeId::from(root));
    }
    tree
}

/// The box a box's location is relative to: its containing block when it is hoisted out of
/// flow, otherwise its parent in the box tree.
pub(crate) fn containing_block(n: &Node) -> Option<BoxId> {
    n.oof_containing_block.get().or_else(|| n.layout_parent.get())
}

/// The document position of a box's border box (rounded layout) offset by `x, y`, less the
/// scroll offsets of the boxes above it.
pub(crate) fn absolute_position(tree: &Tree, id: BoxId, x: f32, y: f32) -> (f32, f32) {
    let mut pos = (x, y);
    let mut current = Some(id);
    while let Some(c) = current {
        let n = &tree[c];
        pos.0 += n.final_layout().location.x - n.scroll_offset().x as f32;
        pos.1 += n.final_layout().location.y - n.scroll_offset().y as f32;
        current = containing_block(n);
    }
    pos
}

/// The document position of a box's border box from its unrounded layout.
fn unrounded_position(tree: &Tree, id: BoxId) -> (f32, f32) {
    let mut pos = (0.0, 0.0);
    let mut current = Some(id);
    while let Some(c) = current {
        let n = &tree[c];
        pos.0 += n.unrounded_layout().location.x - n.scroll_offset().x as f32;
        pos.1 += n.unrounded_layout().location.y - n.scroll_offset().y as f32;
        current = containing_block(n);
    }
    pos
}

/// A non-atomic inline element: no box of its own, it is a style span in its inline root's
/// text layout.
pub(crate) fn is_non_atomic_inline(n: &Node) -> bool {
    if !construct::has_element_data(n)
        || n.is_inline_root()
        || replaced::is_inline_box_element(&n.tag)
    {
        return false;
    }
    n.primary_styles().is_some_and(|s| {
        let d = s.clone_display();
        d.outside() == DisplayOutside::Inline && d.inside() == DisplayInside::Flow
    })
}

/// The inline root a box is in (itself, if it is one).
pub(crate) fn inline_root_ancestor(tree: &Tree, id: BoxId) -> Option<BoxId> {
    let mut at = id;
    loop {
        if tree[at].is_inline_root() {
            return Some(at);
        }
        at = tree[at].layout_parent.get()?;
    }
}

/// The per-line fragment boxes of a non-atomic inline element, relative to its inline root's
/// border box (`None` for boxes with a box of their own).
pub(crate) fn inline_fragment_boxes(tree: &Tree, id: BoxId) -> Option<Vec<taffy::Rect<f32>>> {
    use parley::PositionedLayoutItem;

    if !is_non_atomic_inline(&tree[id]) {
        return None;
    }
    let root_id = inline_root_ancestor(tree, id)?;
    let root = &tree[root_id];
    let inline = root.inline_layout.as_ref()?;
    let layout = &inline.layout;
    let scale = layout.scale();
    // Whether a box is (inside) the target, below the inline root.
    let in_target = |mut at: BoxId| loop {
        if at == id {
            return true;
        }
        if at == root_id {
            return false;
        }
        match tree[at].parent {
            Some(p) => at = p,
            None => return false,
        }
    };
    let root_layout = root.unrounded_layout();
    let inset = root_layout.padding + root_layout.border;
    let (origin_x, origin_y) = (inset.left, inset.top + inline.block_offset);
    let union = |acc: &mut Option<taffy::Rect<f32>>, l: f32, t: f32, r: f32, b: f32| {
        *acc = Some(match *acc {
            Some(rect) => taffy::Rect {
                left: rect.left.min(l),
                top: rect.top.min(t),
                right: rect.right.max(r),
                bottom: rect.bottom.max(b),
            },
            None => taffy::Rect {
                left: l,
                top: t,
                right: r,
                bottom: b,
            },
        });
    };
    // One rect per line box: the union of the target's fragments on it.
    Some(
        layout
            .lines()
            .filter_map(|line| {
                let metrics = line.metrics();
                let mut rect: Option<taffy::Rect<f32>> = None;
                for item in line.items() {
                    match item {
                        PositionedLayoutItem::GlyphRun(run) => {
                            if !in_target(run.style().brush.id) {
                                continue;
                            }
                            let x0 = run.offset();
                            // The line box's block extent: the run's font metrics could clip
                            // the glyphs.
                            union(
                                &mut rect,
                                x0,
                                metrics.block_min_coord,
                                x0 + run.advance(),
                                metrics.block_max_coord,
                            );
                        }
                        PositionedLayoutItem::InlineBox(b) => {
                            if !in_target(b.id as BoxId) {
                                continue;
                            }
                            union(&mut rect, b.x, b.y, b.x + b.width, b.y + b.height);
                        }
                    }
                }
                rect.map(|r| taffy::Rect {
                    left: origin_x + r.left / scale,
                    top: origin_y + r.top / scale,
                    right: origin_x + r.right / scale,
                    bottom: origin_y + r.bottom / scale,
                })
            })
            .collect(),
    )
}

/// A geometry value snapped to a 1/64 px grid (accumulated `f32` error is not reported).
fn snap(v: f64) -> f32 {
    ((v * 64.0).round() / 64.0) as f32
}

/// A box's border box in document coordinates, as scripts see it (`getBoundingClientRect` at
/// scroll 0,0): from its unrounded layout, or for a non-atomic inline the union of its line
/// fragments (`None` when it has none).
pub(crate) fn border_box(tree: &Tree, id: BoxId) -> Option<crate::Rect> {
    if let Some(fragments) = inline_fragment_boxes(tree, id) {
        let root = inline_root_ancestor(tree, id)?;
        let (ox, oy) = unrounded_position(tree, root);
        return fragments
            .into_iter()
            .map(|r| crate::Rect {
                x: snap(f64::from(ox) + f64::from(r.left)),
                y: snap(f64::from(oy) + f64::from(r.top)),
                width: snap(f64::from(r.right - r.left)),
                height: snap(f64::from(r.bottom - r.top)),
            })
            .reduce(|a, c| {
                let (x0, y0) = (a.x.min(c.x), a.y.min(c.y));
                let x1 = (a.x + a.width).max(c.x + c.width);
                let y1 = (a.y + a.height).max(c.y + c.height);
                crate::Rect {
                    x: x0,
                    y: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                }
            });
    }
    let (x, y) = unrounded_position(tree, id);
    let size = tree[id].unrounded_layout().size;
    Some(crate::Rect {
        x: snap(f64::from(x)),
        y: snap(f64::from(y)),
        width: snap(f64::from(size.width)),
        height: snap(f64::from(size.height)),
    })
}

#[cfg(test)]
mod tests {
    use pageon_dom::{Dom, Edge, NodeData, NodeId, ParseOptions, parse_document};

    use super::tree::{Build, Tree};
    use crate::{Layout, LayoutOptions, Rect, Viewport};

    fn elements(dom: &Dom) -> Vec<NodeId> {
        dom.traverse(dom.root())
            .filter_map(|e| match e {
                Edge::Enter(id) => {
                    matches!(dom.get(id).map(|n| &n.data), Some(NodeData::Element { .. }))
                        .then_some(id)
                }
                Edge::Leave(_) => None,
            })
            .collect()
    }

    fn sheets(dom: &Dom) -> Vec<String> {
        elements(dom)
            .into_iter()
            .filter(|&id| dom.get(id).is_some_and(|n| n.is_html("style")))
            .map(|id| {
                dom.children(id)
                    .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                        Some(NodeData::Text(t)) => Some(t.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }

    /// An element's box as [`Layout::box_of`] reports it (no scroll): none for a box not
    /// rendered, row and row group boxes the union of their cells.
    fn box_of(tree: &Tree, id: NodeId) -> Option<Rect> {
        let &b = tree.by_dom.get(&id)?;
        let n = &tree[b];
        if n.display_style().is_none_or(|d| d.is_none()) {
            return Some(Rect::default());
        }
        let rect = super::border_box(tree, b)?;
        if rect.area() > 0.0 {
            return Some(rect);
        }
        let display = n.style.as_ref().map(|s| s.clone_display());
        use style::values::specified::box_::DisplayInside as D;
        if display.is_some_and(|d| {
            matches!(
                d.inside(),
                D::TableRow | D::TableRowGroup | D::TableHeaderGroup | D::TableFooterGroup
            )
        }) {
            let union = n
                .children
                .iter()
                .filter_map(|&c| tree[c].dom)
                .filter(|&c| tree.of(c).is_some_and(|c| c.kind == super::Kind::Element))
                .filter_map(|c| box_of(tree, c))
                .filter(|c| c.area() > 0.0)
                .reduce(|a, c| {
                    let (x0, y0) = (a.x.min(c.x), a.y.min(c.y));
                    let x1 = (a.x + a.width).max(c.x + c.width);
                    let y1 = (a.y + a.height).max(c.y + c.height);
                    Rect {
                        x: x0,
                        y: y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    }
                });
            if union.is_some() {
                return union;
            }
        }
        Some(rect)
    }

    fn close(a: &Rect, b: &Rect) -> bool {
        (a.x - b.x).abs() <= 0.01
            && (a.y - b.y).abs() <= 0.01
            && (a.width - b.width).abs() <= 0.01
            && (a.height - b.height).abs() <= 0.01
    }

    #[test]
    fn boxes_equal_the_engines_on_every_geometry_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/geometry");
        let mut pages: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{e}"))
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "html"))
            .collect();
        pages.sort();
        let vp = Viewport::default();
        let mut compared = 0;
        let mut sized = 0;
        let mut failures: Vec<String> = Vec::new();
        for page in &pages {
            let name = page.file_name().unwrap_or_default().to_string_lossy().to_string();
            let html = std::fs::read_to_string(page).unwrap_or_else(|e| panic!("{e}"));
            let dom = parse_document(&html, ParseOptions::default());
            let css = sheets(&dom);
            let mut styler = pageon_style::Styler::with_metrics(
                vp.width as f32,
                vp.height as f32,
                "about:blank",
                crate::font_metrics(),
            );
            styler.set_sheets(
                &css.iter()
                    .map(|s| (pageon_style::text_hash(s), s.as_str()))
                    .collect::<Vec<_>>(),
            );
            styler.restyle_all(&dom);
            let theirs = Layout::with_options(
                &dom,
                vp,
                &[],
                None,
                &LayoutOptions {
                    styles: Some(&styler),
                    ..LayoutOptions::default()
                },
            );
            let ours = super::lay_out(
                &dom,
                &Build {
                    styler: &styler,
                    images: None,
                    redact: &[],
                    base: "about:blank",
                    viewport: vp,
                },
            );
            assert!(theirs.error().is_none(), "{name}: {:?}", theirs.error());
            for id in elements(&dom) {
                let tag = dom.get(id).and_then(|n| n.local_name()).unwrap_or_default();
                let expected = theirs.box_of(id);
                let ok = match (ours.by_dom.contains_key(&id), box_of(&ours, id), expected) {
                    // Not in the box tree (inside an undisplayed or replaced element).
                    (false, _, e) => e.is_none() || e == Some(Rect::default()),
                    (true, Some(a), Some(b)) => close(&a, &b),
                    (true, None, None) => true,
                    _ => false,
                };
                compared += 1;
                if ours.by_dom.contains_key(&id) && expected.is_some_and(|e| e.area() > 0.0) {
                    sized += 1;
                }
                if !ok {
                    failures.push(format!(
                        "{name}: <{tag}> {id:?}: ours {:?}, engine {:?}",
                        box_of(&ours, id),
                        expected
                    ));
                }
            }
        }
        eprintln!(
            "{} fixtures, {compared} elements compared ({sized} with a box), {} differ",
            pages.len(),
            failures.len()
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
