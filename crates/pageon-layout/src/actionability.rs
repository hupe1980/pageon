//! Visibility, viewport, cover and hit-testing: the facts behind safe acting.

use pageon_dom::NodeId;

use crate::{Layout, Rect};

/// How a node presents to a sighted user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// No box (`display:none`, not mirrored, or outside the layout tree).
    NotRendered,
    /// Rendered but not perceivable: `visibility:hidden`, effective opacity 0, or zero area.
    Hidden,
    /// Rendered and perceivable.
    Visible,
}

/// How an element clips (itself or its content).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Clip {
    /// `overflow-x` hidden or clip: content is cut at the box horizontally.
    x: bool,
    /// `overflow-y` hidden or clip.
    y: bool,
    /// `clip: rect(…)` or `clip-path: inset(…)` that leaves nothing: the element itself.
    all: bool,
}

/// Whether a computed `clip`/`clip-path` leaves no visible area.
fn clips_everything(clip: &str, clip_path: &str) -> bool {
    let nums = |s: &str| -> Vec<f32> {
        s.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .filter_map(|t| t.parse().ok())
            .collect()
    };
    if let Some(r) = clip.strip_prefix("rect(") {
        let n = nums(r);
        if let [top, right, bottom, left] = n[..] {
            return right - left < 1.0 || bottom - top < 1.0;
        }
    }
    if let Some(i) = clip_path.strip_prefix("inset(") {
        // `inset(50%)` and more cuts everything away.
        return i.contains('%') && nums(i).first().is_some_and(|&p| p >= 50.0);
    }
    false
}

impl Layout {
    fn clippers(&self) -> &std::collections::HashMap<blitz_dom::NodeId, Clip> {
        self.clippers.get_or_init(|| {
            let cut = |v: Option<String>| v.is_some_and(|v| v == "hidden" || v == "clip");
            self.map
                .pageon_ids()
                .filter_map(|id| {
                    let c = Clip {
                        x: cut(self.style(id, "overflow-x")),
                        y: cut(self.style(id, "overflow-y")),
                        all: clips_everything(
                            &self.style(id, "clip").unwrap_or_default(),
                            &self.style(id, "clip-path").unwrap_or_default(),
                        ),
                    };
                    (c.x || c.y || c.all).then(|| Some((self.map.blitz(id)?, c)))?
                })
                .collect()
        })
    }

    /// Whether the element's box is clipped away: `clip`/`clip-path` that leaves nothing, or
    /// clipping ancestors (`overflow` hidden or clip) that leave less than 1 px². Scroll
    /// containers do not count (a person can scroll to what they hide).
    pub fn clipped(&self, id: NodeId) -> bool {
        let clippers = self.clippers();
        if clippers.is_empty() {
            return false;
        }
        let Some(mut r) = self.box_of(id) else {
            return false;
        };
        let Some(b) = self.map.blitz(id) else {
            return false;
        };
        let mut cur = Some(b);
        let mut first = true;
        while let Some(c) = cur {
            let node = self.doc.get_node(c);
            if let Some(clip) = clippers.get(&c) {
                if clip.all {
                    return true;
                }
                if !first && let Some(port) = self.map.pageon(c).and_then(|p| self.box_of(p)) {
                    if clip.x {
                        let (x0, x1) = (r.x.max(port.x), (r.x + r.width).min(port.x + port.width));
                        r.x = x0;
                        r.width = x1 - x0;
                    }
                    if clip.y {
                        let (y0, y1) =
                            (r.y.max(port.y), (r.y + r.height).min(port.y + port.height));
                        r.y = y0;
                        r.height = y1 - y0;
                    }
                    if r.width < 1.0 || r.height < 1.0 {
                        return true;
                    }
                }
            }
            // A fixed box is not clipped by its ancestors' overflow.
            if self.fixed.contains(&c) {
                break;
            }
            first = false;
            cur = node.and_then(|n| n.parent);
        }
        false
    }

    /// Whether a node is rendered and perceivable.
    pub fn visibility(&self, id: NodeId) -> Visibility {
        let Some(b) = self.map.blitz(id) else {
            return Visibility::NotRendered;
        };
        let Some(node) = self.doc.get_node(b) else {
            return Visibility::NotRendered;
        };
        let is_element = node.element_data().is_some();
        // display:none on the node or an ancestor: no box.
        if is_element && node.primary_styles().is_none() {
            return Visibility::NotRendered; // inside an undisplayed subtree
        }
        // Computed values are read directly (not serialized): this runs for every node, over
        // every ancestor.
        let mut cur = Some(b);
        while let Some(c) = cur {
            let Some(n) = self.doc.get_node(c) else { break };
            if n.element_data().is_some()
                && n.primary_styles()
                    .is_none_or(|s| s.get_box().display.is_none())
            {
                return Visibility::NotRendered;
            }
            cur = n.parent;
        }
        let Some(rect) = self.box_of(id) else {
            // Styled and not under `display:none`, so rendered: an element with no fragments (an
            // empty inline, `display:contents`) has no area; text follows its element.
            return match (is_element, node.parent.and_then(|p| self.map.pageon(p))) {
                (false, Some(parent)) => self.visibility(parent),
                _ => Visibility::Hidden,
            };
        };
        if is_element {
            let v = self.style(id, "visibility").unwrap_or_default();
            if v == "hidden" || v == "collapse" {
                return Visibility::Hidden;
            }
        }
        // Effective opacity: the product over the node and its ancestors.
        let mut opacity = 1.0;
        let mut cur = Some(b);
        while let Some(c) = cur {
            let Some(n) = self.doc.get_node(c) else { break };
            if n.element_data().is_some()
                && let Some(st) = n.primary_styles()
            {
                opacity *= st.get_effects().opacity;
            }
            cur = n.parent;
        }
        if opacity <= 0.01 || rect.area() < 1.0 {
            return Visibility::Hidden;
        }
        Visibility::Visible
    }

    /// Whether the node's box intersects the viewport at its scroll position.
    pub fn in_viewport(&self, id: NodeId) -> bool {
        self.box_of(id)
            .and_then(|r| r.intersect(&self.viewport_rect()))
            .is_some()
    }

    /// The viewport in document coordinates.
    pub fn viewport_rect(&self) -> Rect {
        let (x, y) = self.viewport_scroll();
        Rect {
            x,
            y,
            width: self.viewport.width as f32,
            height: self.viewport.height as f32,
        }
    }

    /// The topmost mirrored element at a viewport point (`None` outside the viewport).
    pub fn element_at(&self, x: f32, y: f32) -> Option<NodeId> {
        let (w, h) = (self.viewport.width as f32, self.viewport.height as f32);
        if !(0.0..=w).contains(&x) || !(0.0..=h).contains(&y) {
            return None;
        }
        // Blitz hit-tests in document coordinates, except that fixed boxes stay where they are
        // at scroll 0,0: try the fixed layer at the viewport point, then the page at the
        // document point.
        if let Some(top) = self.hit(x, y).filter(|&t| self.is_fixed(t)) {
            return Some(top);
        }
        let (sx, sy) = self.viewport_scroll();
        let (dx, dy) = (x + sx, y + sy);
        match self.hit(dx, dy) {
            Some(top) if !self.is_fixed(top) => Some(top),
            // Over no box: the root element, as `elementFromPoint` answers.
            None => self.map.pageon(self.doc.try_root_element()?.id),
            // A fixed box matched at its unscrolled place: the topmost other element there,
            // approximated by tree order.
            Some(_) => self
                .map
                .pageon_ids()
                .filter(|&id| {
                    !self.is_fixed(id)
                        && self.visibility(id) == Visibility::Visible
                        && self.style(id, "pointer-events").as_deref() != Some("none")
                        && self.box_of(id).is_some_and(|r| {
                            dx >= r.x && dx <= r.x + r.width && dy >= r.y && dy <= r.y + r.height
                        })
                })
                .last()
                .or_else(|| self.map.pageon(self.doc.try_root_element()?.id)),
        }
    }

    /// Blitz's hit test at `x, y`, as the nearest mirrored element.
    fn hit(&self, x: f32, y: f32) -> Option<NodeId> {
        let mut b = self.doc.hit(x, y)?.node_id;
        loop {
            if let Some(id) = self.map.pageon(b)
                && self
                    .doc
                    .get_node(b)
                    .is_some_and(|n| n.element_data().is_some())
            {
                return Some(id);
            }
            b = self.doc.get_node(b)?.parent?;
        }
    }

    /// The element painted over the centre of the node's visible part, if it is neither an
    /// ancestor nor a descendant of the node (nor inside the node's `<label>`).
    pub fn covered_by(&self, id: NodeId) -> Option<NodeId> {
        let visible = self.box_of(id)?.intersect(&self.viewport_rect())?;
        let (sx, sy) = self.viewport_scroll();
        let (cx, cy) = (
            visible.x + visible.width / 2.0 - sx,
            visible.y + visible.height / 2.0 - sy,
        );
        let top = self.element_at(cx, cy)?;
        if self.related(id, top) || self.related(top, id) || self.in_label_of(id, top) {
            return None;
        }
        Some(top)
    }

    /// Whether `a` is `b` or an ancestor of `b` (in the layout tree).
    fn related(&self, a: NodeId, b: NodeId) -> bool {
        let (Some(a), Some(mut cur)) = (self.map.blitz(a), self.map.blitz(b)) else {
            return false;
        };
        loop {
            if cur == a {
                return true;
            }
            match self.doc.get_node(cur).and_then(|n| n.parent) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }

    /// Whether `top` lies inside the `<label>` associated with control `id`.
    fn in_label_of(&self, id: NodeId, top: NodeId) -> bool {
        let Some(b) = self.map.blitz(id) else {
            return false;
        };
        let Some(node) = self.doc.get_node(b) else {
            return false;
        };
        let control_id = node.attr(html5ever::local_name!("id")).map(str::to_string);
        // The wrapping label, or a label[for=id] anywhere: walk up from `top`.
        let mut cur = self.map.blitz(top);
        while let Some(c) = cur {
            let Some(n) = self.doc.get_node(c) else { break };
            if let Some(e) = n.element_data()
                && &*e.name.local == "label"
            {
                if self.related(self.map.pageon(c).unwrap_or(id), id) {
                    return true;
                }
                if let (Some(f), Some(cid)) =
                    (n.attr(html5ever::local_name!("for")), control_id.as_deref())
                    && f == cid
                {
                    return true;
                }
            }
            cur = n.parent;
        }
        false
    }
}
