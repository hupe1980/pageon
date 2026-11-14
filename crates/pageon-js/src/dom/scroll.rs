//! Scrolling (CSSOM View): the viewport's and scroll containers' positions, scrolling into
//! view, the wheel, and `scroll`/`scrollend` events at the end of the task.
//!
//! Positions live here, in the realm, because layouts are rebuilt from the DOM: the viewport's
//! is applied to each layout without re-laying it out, element offsets when it is built.
//! Every scroll is instant (smooth scrolling included), on the virtual clock.

use std::collections::BTreeMap;

use pageon_dom::NodeId;

use crate::cv::{Cx, R};
use crate::realm::Target;

/// The realm's scroll positions.
#[derive(Debug, Clone, Default)]
pub struct ScrollState {
    /// The viewport's offset (CSS px).
    pub viewport: (f64, f64),
    /// Scroll containers' offsets (non-zero ones).
    pub elements: BTreeMap<NodeId, (f64, f64)>,
    /// Bumped when an element offset changes (the layout must be rebuilt).
    pub generation: u64,
    /// Bumped on every change (observers re-evaluate).
    pub moves: u64,
    /// Targets scrolled during the current task, for `scroll` and `scrollend`.
    pub pending: Vec<Target>,
    /// The last pointer position (viewport), where the wheel lands.
    pub pointer: Option<(f64, f64)>,
}

/// What scrolls: the viewport or an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroller {
    /// The document's viewport.
    Viewport,
    /// A scroll container.
    Element(NodeId),
}

/// The document element.
pub(crate) fn root_element(cx: &Cx<'_, '_, '_>) -> Option<NodeId> {
    let dom = cx.dom();
    dom.children(dom.root())
        .find(|&c| dom.get(c).is_some_and(|n| n.local_name().is_some()))
}

/// The scroller an element's `scrollTop` addresses: the document element is the viewport.
pub(crate) fn scroller_of(cx: &Cx<'_, '_, '_>, el: NodeId) -> Scroller {
    if root_element(cx) == Some(el) {
        Scroller::Viewport
    } else {
        Scroller::Element(el)
    }
}

/// The current offset.
pub(crate) fn offset(cx: &Cx<'_, '_, '_>, s: Scroller) -> (f64, f64) {
    match s {
        Scroller::Viewport => {
            let (x, y) = cx.realm.layout().viewport_scroll();
            (f64::from(x), f64::from(y))
        }
        Scroller::Element(id) => {
            let (x, y) = cx.realm.layout().scroll_of(id);
            (f64::from(x), f64::from(y))
        }
    }
}

/// The largest offset (0,0 for elements that do not scroll).
pub(crate) fn range(cx: &Cx<'_, '_, '_>, s: Scroller) -> (f64, f64) {
    let l = cx.realm.layout();
    let (x, y) = match s {
        Scroller::Viewport => l.viewport_range(),
        Scroller::Element(id) => l.scroll_range(id).unwrap_or((0.0, 0.0)),
    };
    (f64::from(x), f64::from(y))
}

/// Scrolls to `x, y` (clamped). Returns whether the position changed.
pub(crate) fn set(cx: &Cx<'_, '_, '_>, s: Scroller, x: f64, y: f64) -> bool {
    let (mx, my) = range(cx, s);
    let to = (
        if x.is_finite() { x.clamp(0.0, mx) } else { 0.0 },
        if y.is_finite() { y.clamp(0.0, my) } else { 0.0 },
    );
    if offset(cx, s) == to {
        return false;
    }
    let mut st = cx.st();
    match s {
        Scroller::Viewport => {
            st.scroll.viewport = to;
            let root = cx.dom().root();
            if !st.scroll.pending.contains(&Target::Node(root)) {
                st.scroll.pending.push(Target::Node(root));
            }
        }
        Scroller::Element(id) => {
            if to == (0.0, 0.0) {
                st.scroll.elements.remove(&id);
            } else {
                st.scroll.elements.insert(id, to);
            }
            st.scroll.generation += 1;
            if !st.scroll.pending.contains(&Target::Node(id)) {
                st.scroll.pending.push(Target::Node(id));
            }
        }
    }
    st.scroll.moves += 1;
    drop(st);
    if s == Scroller::Viewport {
        // The cached layout follows at once (no re-layout).
        cx.realm
            .layout()
            .set_viewport_scroll(to.0 as f32, to.1 as f32);
    }
    true
}

/// Where an axis must be scrolled so the box `[e0, e1]` aligns in the port `[p0, p1]` (both in
/// the same coordinates), from the current offset `o`.
fn align(o: f64, (e0, e1): (f64, f64), (p0, p1): (f64, f64), how: &str, if_needed: bool) -> f64 {
    let inside = e0 >= p0 && e1 <= p1;
    if if_needed && inside {
        return o;
    }
    match how {
        "start" => o + (e0 - p0),
        "end" => o + (e1 - p1),
        "nearest" if inside => o,
        "nearest" if e0 < p0 || (e1 - e0) > (p1 - p0) => o + (e0 - p0),
        "nearest" => o + (e1 - p1),
        // "center", and `if_needed` scrolling.
        _ => o + (e0 + e1) / 2.0 - (p0 + p1) / 2.0,
    }
}

/// Scrolls every scroll container around `node`, innermost first, then the viewport, so that
/// `node` is aligned as asked (`start`, `center`, `end`, `nearest`). With `if_needed`, nothing
/// moves when it is already fully visible, and alignment is `center`. Returns whether
/// anything scrolled.
pub(crate) fn into_view(
    cx: &Cx<'_, '_, '_>,
    node: NodeId,
    block: &str,
    inline: &str,
    if_needed: bool,
) -> bool {
    let (block, inline) = if if_needed {
        ("center", "center")
    } else {
        (block, inline)
    };
    let layout = cx.realm.layout();
    let Some(r) = layout.box_of(node) else {
        return false;
    };
    let fixed = layout.is_fixed(node);
    let containers: Vec<NodeId> = cx
        .dom()
        .ancestors(node)
        .filter(|&a| {
            layout
                .scroll_range(a)
                .is_some_and(|(x, y)| x > 0.0 || y > 0.0)
        })
        .collect();
    drop(layout);
    let (mut x0, mut y0) = (f64::from(r.x), f64::from(r.y));
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    let mut moved = false;
    for c in containers {
        let Some(port) = cx.realm.layout().box_of(c) else {
            continue;
        };
        let (px, py) = (f64::from(port.x), f64::from(port.y));
        let (pw, ph) = (f64::from(port.width), f64::from(port.height));
        let (ox, oy) = offset(cx, Scroller::Element(c));
        let tx = align(ox, (x0, x0 + w), (px, px + pw), inline, if_needed);
        let ty = align(oy, (y0, y0 + h), (py, py + ph), block, if_needed);
        if set(cx, Scroller::Element(c), tx, ty) {
            moved = true;
        }
        let (nx, ny) = offset(cx, Scroller::Element(c));
        x0 -= nx - ox;
        y0 -= ny - oy;
    }
    if fixed {
        return moved;
    }
    let (ox, oy) = offset(cx, Scroller::Viewport);
    let v = cx.st().viewport;
    let (vw, vh) = (f64::from(v.width), f64::from(v.height));
    let tx = align(ox, (x0, x0 + w), (ox, ox + vw), inline, if_needed);
    let ty = align(oy, (y0, y0 + h), (oy, oy + vh), block, if_needed);
    set(cx, Scroller::Viewport, tx, ty) || moved
}

/// How far a user scroll goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Amount {
    /// CSS pixels.
    Pixels(f64, f64),
    /// Pages of the scroller (87.5% of its size, as Chromium pages), signed per axis.
    Pages(f64, f64),
    /// To the start (-1) or the end (1), per axis.
    Edge(f64, f64),
}

/// Chromium's paging fraction.
const PAGE: f64 = 0.875;

/// Scrolls to a URL fragment, as HTML's "scroll to the fragment": the element with that `id`,
/// else the first `<a name>` with it, to the top; the empty fragment and `top` to the start.
pub(crate) fn to_fragment(cx: &Cx<'_, '_, '_>, fragment: &str) -> bool {
    let decoded = percent_decode(fragment);
    if decoded.is_empty() || decoded.eq_ignore_ascii_case("top") {
        return set(cx, Scroller::Viewport, 0.0, 0.0);
    }
    let target = {
        let dom = cx.dom();
        let by = |attr: &str, tag: Option<&str>| {
            dom.traverse(dom.root()).find_map(|e| match e {
                pageon_dom::Edge::Enter(n)
                    if dom.get(n).is_some_and(|x| {
                        x.attr(attr) == Some(decoded.as_str()) && tag.is_none_or(|t| x.is_html(t))
                    }) =>
                {
                    Some(n)
                }
                _ => None,
            })
        };
        by("id", None).or_else(|| by("name", Some("a")))
    };
    target.is_some_and(|t| into_view(cx, t, "start", "nearest", false))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a user scroll did.
#[derive(Debug, Clone, PartialEq)]
pub enum Wheeled {
    /// This scroller moved, to this offset.
    Moved(Scroller, (f64, f64)),
    /// Nothing could move further that way.
    AtEnd,
    /// The page cancelled the `wheel` event.
    Prevented,
}

/// The size of a scroller's port (CSS px).
fn port(cx: &Cx<'_, '_, '_>, s: Scroller) -> (f64, f64) {
    match s {
        Scroller::Viewport => {
            let v = cx.st().viewport;
            (f64::from(v.width), f64::from(v.height))
        }
        Scroller::Element(id) => cx
            .realm
            .layout()
            .box_of(id)
            .map_or((0.0, 0.0), |r| (f64::from(r.width), f64::from(r.height))),
    }
}

/// The delta an amount means for a scroller.
fn delta(cx: &Cx<'_, '_, '_>, s: Scroller, a: Amount) -> (f64, f64) {
    match a {
        Amount::Pixels(x, y) => (x, y),
        Amount::Pages(x, y) => {
            let (w, h) = port(cx, s);
            (x * w * PAGE, y * h * PAGE)
        }
        Amount::Edge(x, y) => {
            let (mx, my) = range(cx, s);
            (x * (mx + 1.0), y * (my + 1.0))
        }
    }
}

/// Scrolls the innermost scroller from `from` outwards that can still move that way, ending
/// at the viewport, as a user's scroll does (`overflow: hidden` does not scroll for users).
pub(crate) fn chain(cx: &Cx<'_, '_, '_>, from: Option<NodeId>, a: Amount) -> Wheeled {
    let root = root_element(cx);
    let mut candidates: Vec<Scroller> = Vec::new();
    if let Some(f) = from {
        let layout = cx.realm.layout();
        let dom = cx.dom();
        for el in std::iter::once(f).chain(dom.ancestors(f)) {
            if Some(el) == root || layout.scroll_range(el).is_none() {
                continue;
            }
            let hidden = |p: &str| layout.style(el, p).as_deref() == Some("hidden");
            let textarea = dom.get(el).is_some_and(|n| n.is_html("textarea"));
            if textarea || !(hidden("overflow-x") && hidden("overflow-y")) {
                candidates.push(Scroller::Element(el));
            }
        }
    }
    candidates.push(Scroller::Viewport);
    for s in candidates {
        let (dx, dy) = delta(cx, s, a);
        let (ox, oy) = offset(cx, s);
        let (mx, my) = range(cx, s);
        let can = (dx > 0.0 && ox < mx)
            || (dx < 0.0 && ox > 0.0)
            || (dy > 0.0 && oy < my)
            || (dy < 0.0 && oy > 0.0);
        if can && set(cx, s, ox + dx, oy + dy) {
            return Wheeled::Moved(s, offset(cx, s));
        }
    }
    Wheeled::AtEnd
}

/// A trusted `wheel` event at `point` (viewport), on `target` (default: what is under the
/// point), then — unless it was cancelled — the scroll it asks for, from `target` outwards.
pub(crate) fn wheel(
    cx: &mut Cx<'_, '_, '_>,
    point: (f64, f64),
    target: Option<NodeId>,
    a: Amount,
) -> R<Wheeled> {
    let hit = target.or_else(|| cx.realm.layout().element_at(point.0 as f32, point.1 as f32));
    if let Some(el) = hit.or_else(|| root_element(cx)) {
        // The event reports the delta the outermost scroller would take.
        let (dx, dy) = delta(cx, Scroller::Viewport, a);
        if !crate::dom::input::wheel_event(cx, el, point, dx, dy)? {
            return Ok(Wheeled::Prevented);
        }
    }
    Ok(chain(cx, hit, a))
}

/// Fires `scroll`, then `scrollend`, at every target scrolled during the task (at the document,
/// bubbling to the window, for the viewport). Returns whether any target was due.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>) -> R<bool> {
    let pending = std::mem::take(&mut cx.st().scroll.pending);
    if pending.is_empty() {
        return Ok(false);
    }
    let root = cx.dom().root();
    for ty in ["scroll", "scrollend"] {
        for &t in &pending {
            if let Target::Node(n) = t
                && cx.dom().get(n).is_none()
            {
                continue;
            }
            let bubbles = t == Target::Node(root);
            crate::dom::event_target::fire(cx, t, ty, bubbles, false)?;
        }
    }
    Ok(true)
}
