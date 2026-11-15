//! Editing: the browser's edits in text controls and editable content, each wrapped in
//! `beforeinput`/`input` (Input Events), caret movement, the session clipboard with
//! `copy`/`cut`/`paste`, `execCommand`, and selection by pointer.

use html5ever::ns;
use pageon_dom::{Boundary, Direction, Dom, NodeData, NodeId};

use crate::cv::{Cx, Error, Obj, R};
use crate::dom::gone;
use crate::dom::ranges::{is_text, root_of};
use crate::dom::selecting::{anchor_focus, collapse_at, current, select};
use crate::generated::Iface;
use crate::realm::{EventData, Slot, Target};

/// The session clipboard's contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    /// `text/plain`.
    pub text: String,
    /// `text/html`, when a copy produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

// ----- units of movement -----

/// How far a caret moves (`Selection.modify` granularities).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// A grapheme.
    Character,
    /// A word.
    Word,
    /// To the line above or below.
    Line,
    /// To the start or end of the line.
    LineBoundary,
    /// To the start or end of the block.
    ParagraphBoundary,
    /// To the start or end of the editable content (or the document).
    DocumentBoundary,
}

impl Granularity {
    /// The granularities pageon moves by.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "character" => Self::Character,
            "word" => Self::Word,
            "line" => Self::Line,
            "lineboundary" => Self::LineBoundary,
            "paragraphboundary" => Self::ParagraphBoundary,
            "documentboundary" => Self::DocumentBoundary,
            _ => return None,
        })
    }
}

/// A caret movement.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    /// Forward (right) or backward (left).
    pub forward: bool,
    /// How far.
    pub unit: Granularity,
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn graphemes(units: &[u16]) -> Vec<usize> {
    icu_segmenter::GraphemeClusterSegmenter::new()
        .segment_utf16(units)
        .collect()
}

/// Word boundaries with whether the segment before each is a word.
fn words(units: &[u16]) -> Vec<(usize, bool)> {
    let seg = icu_segmenter::WordSegmenter::new_auto(Default::default());
    let mut it = seg.segment_utf16(units);
    let mut out = Vec::new();
    while let Some(b) = it.next() {
        out.push((b, it.is_word_like()));
    }
    out
}

/// The offset one unit from `at` in `units` (graphemes or words; words end forward, start
/// backward).
fn step_in(units: &[u16], at: usize, forward: bool, word: bool) -> Option<usize> {
    if word {
        let w = words(units);
        if forward {
            w.iter()
                .find(|&&(b, like)| b > at && like)
                .or_else(|| w.iter().find(|&&(b, _)| b > at))
                .map(|&(b, _)| b)
        } else {
            // The start of the word before `at`: the boundary before a word-like segment.
            let mut prev = None;
            for pair in w.windows(2) {
                let (start, (end, like)) = (pair[0].0, pair[1]);
                if start < at && like {
                    prev = Some(start);
                }
                if end >= at {
                    break;
                }
            }
            prev.or_else(|| (at > 0).then_some(0))
        }
    } else {
        let g = graphemes(units);
        if forward {
            g.into_iter().find(|&b| b > at)
        } else {
            g.into_iter().rev().find(|&b| b < at)
        }
    }
}

// ----- editable content -----

const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "ul",
    "body",
];

fn tag(dom: &Dom, n: NodeId) -> Option<String> {
    dom.get(n)
        .filter(|x| x.namespace() == Some(pageon_dom::Namespace::Html))
        .and_then(|x| x.local_name())
        .map(str::to_string)
}

fn is_block(dom: &Dom, n: NodeId) -> bool {
    tag(dom, n).is_some_and(|t| BLOCKS.contains(&t.as_str()))
}

/// Leaves a caret can stop beside that are not text.
fn is_atomic(dom: &Dom, n: NodeId) -> bool {
    tag(dom, n).is_some_and(|t| matches!(t.as_str(), "br" | "img" | "hr" | "input" | "iframe"))
}

/// The editing host of `n`: the outermost `contenteditable` ancestor before one set to `false`.
pub(crate) fn editing_host(dom: &Dom, n: NodeId) -> Option<NodeId> {
    let mut host = None;
    let mut cur = Some(n);
    while let Some(c) = cur {
        match dom.get(c).and_then(|x| x.attr("contenteditable")) {
            Some(v) if v.eq_ignore_ascii_case("false") => break,
            Some(v)
                if v.is_empty()
                    || v.eq_ignore_ascii_case("true")
                    || v.eq_ignore_ascii_case("plaintext-only") =>
            {
                host = Some(c);
            }
            _ => {}
        }
        cur = dom.parent(c);
    }
    host
}

/// The nearest block at or above `n` inside `scope` (else `scope`).
fn block_of(dom: &Dom, n: NodeId, scope: NodeId) -> NodeId {
    let mut cur = Some(n);
    while let Some(c) = cur {
        if c == scope {
            return scope;
        }
        if is_block(dom, c) {
            return c;
        }
        cur = dom.parent(c);
    }
    scope
}

/// Text nodes and atomic leaves of `scope`, in tree order.
fn leaves(dom: &Dom, scope: NodeId) -> Vec<NodeId> {
    dom.traverse(scope)
        .filter_map(|e| match e {
            pageon_dom::Edge::Enter(n) if n != scope && (is_text(dom, n) || is_atomic(dom, n)) => {
                Some(n)
            }
            _ => None,
        })
        .collect()
}

/// The first caret position in `n`.
fn first_position(dom: &Dom, n: NodeId) -> Boundary {
    leaves(dom, n)
        .into_iter()
        .next()
        .map_or(Boundary::new(n, 0), |l| {
            if is_text(dom, l) {
                Boundary::new(l, 0)
            } else {
                Boundary::new(dom.parent(l).unwrap_or(n), dom.index_of(l))
            }
        })
}

/// The last caret position in `n`: never after a block's trailing `<br>`.
fn last_position(dom: &Dom, n: NodeId) -> Boundary {
    let l = leaves(dom, n).into_iter().next_back();
    l.map_or(Boundary::new(n, dom.node_length(n)), |l| {
        let par = dom.parent(l).unwrap_or(n);
        if is_text(dom, l) {
            Boundary::new(l, dom.node_length(l))
        } else if is_br(dom, l) {
            Boundary::new(par, dom.index_of(l))
        } else {
            Boundary::new(par, dom.index_of(l) + 1)
        }
    })
}

fn is_br(dom: &Dom, n: NodeId) -> bool {
    tag(dom, n).as_deref() == Some("br")
}

fn text_units(dom: &Dom, n: NodeId) -> Vec<u16> {
    match dom.get(n).map(|x| &x.data) {
        Some(NodeData::Text(t)) => utf16(t),
        _ => Vec::new(),
    }
}

/// Where `p` points among leaves: the leaf before it and the leaf after it.
fn around(dom: &Dom, scope: NodeId, p: Boundary) -> (Option<NodeId>, Option<NodeId>) {
    let all = leaves(dom, scope);
    let mut before = None;
    let mut after = None;
    for l in all {
        let at = if is_text(dom, l) {
            Boundary::new(l, 0)
        } else {
            match dom.parent(l) {
                Some(par) => Boundary::new(par, dom.index_of(l)),
                None => continue,
            }
        };
        if dom.compare_points(at, p) == Some(pageon_dom::Position::Before) {
            before = Some(l);
        } else if after.is_none() {
            after = Some(l);
            break;
        }
    }
    (before, after)
}

/// The scope a caret moves in: the editing host, else the document body (or root).
fn scope_of(cx: &Cx<'_, '_, '_>, n: NodeId) -> NodeId {
    let dom = cx.dom();
    editing_host(&dom, n).unwrap_or_else(|| body_of(&dom).unwrap_or_else(|| dom.root()))
}

/// A caret position one step from `p`.
pub(crate) fn step(cx: &Cx<'_, '_, '_>, p: Boundary, s: Step) -> Boundary {
    let scope = scope_of(cx, p.node);
    match s.unit {
        Granularity::Character | Granularity::Word => {
            let word = s.unit == Granularity::Word;
            let dom = cx.dom();
            if is_text(&dom, p.node) {
                let units = text_units(&dom, p.node);
                if let Some(o) = step_in(&units, p.offset as usize, s.forward, word) {
                    return Boundary::new(p.node, o as u32);
                }
            }
            let (before, after) = around(
                &dom,
                scope,
                if is_text(&dom, p.node) && s.forward {
                    Boundary::new(p.node, dom.node_length(p.node))
                } else {
                    p
                },
            );
            let next = if s.forward {
                after.filter(|&a| a != p.node)
            } else {
                before.filter(|&b| b != p.node)
            };
            let Some(l) = next else { return p };
            let same_block = block_of(&dom, l, scope) == block_of(&dom, p.node, scope);
            if is_text(&dom, l) {
                let units = text_units(&dom, l);
                let len = units.len();
                if s.forward {
                    let o = if same_block {
                        step_in(&units, 0, true, word).unwrap_or(len)
                    } else {
                        0
                    };
                    Boundary::new(l, o as u32)
                } else {
                    let o = if same_block {
                        step_in(&units, len, false, word).unwrap_or(0)
                    } else {
                        len
                    };
                    Boundary::new(l, o as u32)
                }
            } else {
                let par = dom.parent(l).unwrap_or(scope);
                Boundary::new(par, dom.index_of(l) + u32::from(s.forward))
            }
        }
        Granularity::Line => {
            let layout = cx.realm.layout();
            let here = if is_text(&cx.dom(), p.node) {
                layout
                    .text_rects(p.node, p.offset, p.offset)
                    .first()
                    .copied()
            } else {
                None
            };
            let Some(r) = here else {
                return step(
                    cx,
                    p,
                    Step {
                        forward: s.forward,
                        unit: Granularity::ParagraphBoundary,
                    },
                );
            };
            let y = if s.forward {
                r.y + r.height + 1.0
            } else {
                r.y - 1.0
            };
            match layout.caret_at(r.x, y) {
                Some((n, o)) if editing_host(&cx.dom(), n) == editing_host(&cx.dom(), p.node) => {
                    Boundary::new(n, o)
                }
                _ => step(
                    cx,
                    p,
                    Step {
                        forward: s.forward,
                        unit: Granularity::DocumentBoundary,
                    },
                ),
            }
        }
        Granularity::LineBoundary => {
            let layout = cx.realm.layout();
            match is_text(&cx.dom(), p.node)
                .then(|| layout.line_bounds(p.node, p.offset))
                .flatten()
            {
                Some((a, b)) => {
                    let (n, o) = if s.forward { b } else { a };
                    Boundary::new(n, o)
                }
                None => step(
                    cx,
                    p,
                    Step {
                        forward: s.forward,
                        unit: Granularity::ParagraphBoundary,
                    },
                ),
            }
        }
        Granularity::ParagraphBoundary => {
            let dom = cx.dom();
            let b = block_of(&dom, p.node, scope);
            if s.forward {
                last_position(&dom, b)
            } else {
                first_position(&dom, b)
            }
        }
        Granularity::DocumentBoundary => {
            let dom = cx.dom();
            if s.forward {
                last_position(&dom, scope)
            } else {
                first_position(&dom, scope)
            }
        }
    }
}

// ----- text controls -----

/// Whether `el` is a text control (`<textarea>` or a text-like `<input>`), editable or not.
pub(crate) fn text_control(dom: &Dom, el: NodeId) -> bool {
    match tag(dom, el).as_deref() {
        Some("textarea") => true,
        Some("input") => matches!(
            dom.get(el)
                .and_then(|n| n.attr("type"))
                .unwrap_or("text")
                .to_ascii_lowercase()
                .as_str(),
            "text" | "search" | "url" | "tel" | "password" | "email" | "number"
        ),
        _ => false,
    }
}

/// Whether the selection API applies (`selectionStart` is not null).
pub(crate) fn has_selection_api(dom: &Dom, el: NodeId) -> bool {
    match tag(dom, el).as_deref() {
        Some("textarea") => true,
        Some("input") => matches!(
            dom.get(el)
                .and_then(|n| n.attr("type"))
                .unwrap_or("text")
                .to_ascii_lowercase()
                .as_str(),
            "text" | "search" | "url" | "tel" | "password"
        ),
        _ => false,
    }
}

pub(crate) fn control_value(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<String> {
    if tag(&cx.dom(), el).as_deref() == Some("textarea") {
        crate::dom::html_text_area_element::get_value(cx, Obj::Node(el))
    } else {
        crate::dom::html_input_element::get_value(cx, Obj::Node(el))
    }
}

fn control_len(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<u32> {
    Ok(control_value(cx, el)?.encode_utf16().count() as u32)
}

/// A text control's selection, clamped (`None` in state: a caret at the end).
pub(crate) fn control_sel(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<(u32, u32, Direction)> {
    let len = control_len(cx, el)?;
    let sel = cx.dom().control(el).and_then(|c| c.selection);
    Ok(match sel {
        Some((s, e, d)) => (s.min(len), e.min(len), d),
        None => (len, len, Direction::None),
    })
}

/// Sets a text control's selection (clamped); `selectionchange` fires at it.
pub(crate) fn set_control_sel(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    s: u32,
    e: u32,
    d: Direction,
) -> R<()> {
    let len = control_len(cx, el)?;
    let e = e.min(len);
    let s = s.min(e);
    let old = cx.dom().control(el).and_then(|c| c.selection);
    if old != Some((s, e, d)) {
        cx.dom_quiet().control_mut(el).selection = Some((s, e, d));
        let mut st = cx.st();
        if !st.control_sel.contains(&el) {
            st.control_sel.push(el);
        }
    }
    Ok(())
}

fn set_control_value(cx: &Cx<'_, '_, '_>, el: NodeId, v: String) {
    let mut dom = cx.dom_mut();
    let c = dom.control_mut(el);
    c.value = Some(v);
    c.from_vault = false;
}

/// The selected text of a focused text control (`None` when not a control or empty).
pub(crate) fn control_selected_text(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> Option<String> {
    if !text_control(&cx.dom(), el) {
        return None;
    }
    let (s, e, _) = control_sel(cx, el).ok()?;
    let v = control_value(cx, el).ok()?;
    let u = utf16(&v);
    Some(String::from_utf16_lossy(&u[s as usize..e as usize]))
}

fn line_start(u: &[u16], at: usize) -> usize {
    u[..at]
        .iter()
        .rposition(|&c| c == u16::from(b'\n'))
        .map_or(0, |i| i + 1)
}

fn line_end(u: &[u16], at: usize) -> usize {
    u[at..]
        .iter()
        .position(|&c| c == u16::from(b'\n'))
        .map_or(u.len(), |i| at + i)
}

/// A caret position one step from `at` in a control's value (its lines are its `\n` lines).
fn control_step(u: &[u16], at: usize, s: Step) -> usize {
    match s.unit {
        Granularity::Character => {
            step_in(u, at, s.forward, false).unwrap_or(if s.forward { u.len() } else { 0 })
        }
        Granularity::Word => {
            step_in(u, at, s.forward, true).unwrap_or(if s.forward { u.len() } else { 0 })
        }
        Granularity::Line => {
            let (ls, col) = (line_start(u, at), at - line_start(u, at));
            if s.forward {
                let le = line_end(u, at);
                if le == u.len() {
                    return u.len();
                }
                let next = le + 1;
                next + col.min(line_end(u, next) - next)
            } else {
                if ls == 0 {
                    return 0;
                }
                let prev = line_start(u, ls - 1);
                prev + col.min(ls - 1 - prev)
            }
        }
        Granularity::LineBoundary | Granularity::ParagraphBoundary => {
            if s.forward {
                line_end(u, at)
            } else {
                line_start(u, at)
            }
        }
        Granularity::DocumentBoundary => {
            if s.forward {
                u.len()
            } else {
                0
            }
        }
    }
}

// ----- events -----

/// An edit's input type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Typed text.
    InsertText,
    /// Text replacing the content (a fill).
    InsertReplacementText,
    /// Shift+Enter (a `<br>`, or `\n` in a textarea).
    InsertLineBreak,
    /// Enter in editable content.
    InsertParagraph,
    /// Backspace.
    DeleteBackward,
    /// Delete.
    DeleteForward,
    /// Ctrl+Backspace.
    DeleteWordBackward,
    /// Ctrl+Delete.
    DeleteWordForward,
    /// A paste.
    InsertFromPaste,
    /// A cut.
    DeleteByCut,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::InsertText => "insertText",
            Self::InsertReplacementText => "insertReplacementText",
            Self::InsertLineBreak => "insertLineBreak",
            Self::InsertParagraph => "insertParagraph",
            Self::DeleteBackward => "deleteContentBackward",
            Self::DeleteForward => "deleteContentForward",
            Self::DeleteWordBackward => "deleteWordBackward",
            Self::DeleteWordForward => "deleteWordForward",
            Self::InsertFromPaste => "insertFromPaste",
            Self::DeleteByCut => "deleteByCut",
        }
    }

    fn inserts(self) -> bool {
        matches!(
            self,
            Self::InsertText | Self::InsertReplacementText | Self::InsertFromPaste
        )
    }
}

fn now(cx: &Cx<'_, '_, '_>) -> f64 {
    let sh = cx.realm.shared.borrow();
    sh.now - sh.origin
}

/// A trusted `beforeinput` (cancelable) or `input` `InputEvent`.
fn input_event(
    cx: &mut Cx<'_, '_, '_>,
    target: NodeId,
    ty: &str,
    kind: Kind,
    data: Option<&str>,
    ranges: Vec<(Boundary, Boundary)>,
) -> R<bool> {
    if ty == "input" {
        cx.dom_mut().control_mut(target).user_edited = true;
    }
    let data = (!matches!(kind, Kind::InsertFromPaste) || text_control(&cx.dom(), target))
        .then(|| data.map(str::to_string))
        .flatten();
    let ev = EventData {
        ty: ty.into(),
        bubbles: true,
        cancelable: ty == "beforeinput",
        composed: true,
        initialized: true,
        trusted: true,
        time: now(cx),
        input_kind: Some(Box::new((kind.name().to_string(), data, false))),
        ranges,
        ..EventData::default()
    };
    let e = cx.alloc(Iface::InputEvent, Slot::Event(Box::new(ev)));
    crate::dom::event_target::dispatch(cx, Target::Node(target), e)
}

/// What an edit applies to: a text control, or an editing host with the document selection.
enum Place {
    Control(NodeId),
    Host(NodeId),
}

fn place(cx: &Cx<'_, '_, '_>, el: NodeId) -> Option<Place> {
    let dom = cx.dom();
    if text_control(&dom, el) {
        let n = dom.get(el)?;
        if n.attr("disabled").is_some() || n.attr("readonly").is_some() {
            return None;
        }
        return Some(Place::Control(el));
    }
    editing_host(&dom, el).map(Place::Host)
}

/// The selection inside `host`, put at the end of its content when it is elsewhere.
fn host_selection(cx: &Cx<'_, '_, '_>, host: NodeId) -> (Boundary, Boundary) {
    if let Some((s, e, _, _)) = current(cx) {
        let dom = cx.dom();
        if dom.is_inclusive_ancestor(host, s.node) && dom.is_inclusive_ancestor(host, e.node) {
            return (s, e);
        }
    }
    let p = last_position(&cx.dom(), host);
    collapse_at(cx, p);
    (p, p)
}

/// Performs an edit on the editable element `el` as the browser does: `beforeinput` (unless
/// a script's `execCommand`), the change if not cancelled, then `input`. Returns whether
/// anything changed.
pub(crate) fn edit(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    kind: Kind,
    data: Option<&str>,
    scripted: bool,
) -> R<bool> {
    match place(cx, el) {
        Some(Place::Control(c)) => edit_control(cx, c, kind, data, scripted),
        Some(Place::Host(h)) => edit_host(cx, h, kind, data, scripted),
        None => Ok(false),
    }
}

fn edit_control(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    kind: Kind,
    data: Option<&str>,
    scripted: bool,
) -> R<bool> {
    let textarea = tag(&cx.dom(), el).as_deref() == Some("textarea");
    if matches!(kind, Kind::InsertParagraph | Kind::InsertLineBreak) && !textarea {
        return Ok(false);
    }
    let value = control_value(cx, el)?;
    let u = utf16(&value);
    let (s, e, _) = control_sel(cx, el)?;
    let (s, e) = (s as usize, e as usize);
    let (from, to, mut insert) = match kind {
        Kind::InsertText | Kind::InsertReplacementText | Kind::InsertFromPaste => {
            (s, e, data.unwrap_or_default().to_string())
        }
        Kind::InsertLineBreak | Kind::InsertParagraph => (s, e, "\n".to_string()),
        Kind::DeleteByCut => (s, e, String::new()),
        Kind::DeleteBackward | Kind::DeleteWordBackward if s == e => {
            let Some(f) = step_in(&u, s, false, kind == Kind::DeleteWordBackward) else {
                return Ok(false);
            };
            (f, s, String::new())
        }
        Kind::DeleteForward | Kind::DeleteWordForward if s == e => {
            let Some(t) = step_in(&u, s, true, kind == Kind::DeleteWordForward) else {
                return Ok(false);
            };
            (s, t, String::new())
        }
        _ => (s, e, String::new()),
    };
    if from == to && insert.is_empty() {
        return Ok(false);
    }
    if !textarea {
        insert.retain(|c| c != '\n' && c != '\r');
    }
    // `maxlength` cuts what is inserted to fit.
    if kind.inserts() || matches!(kind, Kind::InsertLineBreak | Kind::InsertParagraph) {
        let max = cx
            .dom()
            .get(el)
            .and_then(|n| n.attr("maxlength"))
            .and_then(|m| m.trim().parse::<usize>().ok());
        if let Some(max) = max {
            let room = max.saturating_sub(u.len() - (to - from));
            let units = utf16(&insert);
            if units.len() > room {
                insert = String::from_utf16_lossy(&units[..room]);
                if insert.is_empty() && from == to {
                    return Ok(false);
                }
            }
        }
    }
    if !scripted
        && !input_event(
            cx,
            el,
            "beforeinput",
            kind,
            kind.inserts().then_some(insert.as_str()),
            Vec::new(),
        )?
    {
        return Ok(false);
    }
    if cx.dom().get(el).is_none() {
        return Ok(false);
    }
    let mut out: Vec<u16> = u[..from].to_vec();
    let ins = utf16(&insert);
    out.extend_from_slice(&ins);
    out.extend_from_slice(&u[to..]);
    set_control_value(cx, el, String::from_utf16_lossy(&out));
    let caret = (from + ins.len()) as u32;
    set_control_sel(cx, el, caret, caret, Direction::None)?;
    input_event(
        cx,
        el,
        "input",
        kind,
        kind.inserts().then_some(insert.as_str()),
        Vec::new(),
    )?;
    Ok(true)
}

/// What a collapsed delete in editable content removes.
enum Del {
    Range(Boundary, Boundary),
    /// Join block `from` into block `into`.
    Merge {
        into: NodeId,
        from: NodeId,
    },
    Nothing,
}

fn delete_at(cx: &Cx<'_, '_, '_>, host: NodeId, p: Boundary, forward: bool, word: bool) -> Del {
    let dom = cx.dom();
    if is_text(&dom, p.node) {
        let units = text_units(&dom, p.node);
        if let Some(o) = step_in(&units, p.offset as usize, forward, word) {
            let o = o as u32;
            return if forward {
                Del::Range(p, Boundary::new(p.node, o))
            } else {
                Del::Range(Boundary::new(p.node, o), p)
            };
        }
    }
    let probe = if is_text(&dom, p.node) && forward {
        Boundary::new(p.node, dom.node_length(p.node))
    } else {
        p
    };
    let (before, after) = around(&dom, host, probe);
    let leaf = if forward {
        after.filter(|&a| a != p.node)
    } else {
        before.filter(|&b| b != p.node)
    };
    let Some(l) = leaf else { return Del::Nothing };
    let (here, there) = (block_of(&dom, p.node, host), block_of(&dom, l, host));
    if here != there {
        let (into, from) = if forward {
            (here, there)
        } else {
            (there, here)
        };
        if from == host
            || dom.is_inclusive_ancestor(from, into)
            || dom.is_inclusive_ancestor(into, from)
        {
            return Del::Nothing;
        }
        return Del::Merge { into, from };
    }
    if is_text(&dom, l) {
        let units = text_units(&dom, l);
        let len = units.len() as u32;
        if forward {
            let o = step_in(&units, 0, true, word).unwrap_or(units.len()) as u32;
            Del::Range(Boundary::new(l, 0), Boundary::new(l, o))
        } else {
            let o = step_in(&units, units.len(), false, word).unwrap_or(0) as u32;
            Del::Range(Boundary::new(l, o), Boundary::new(l, len))
        }
    } else {
        let par = dom.parent(l).unwrap_or(host);
        let i = dom.index_of(l);
        Del::Range(Boundary::new(par, i), Boundary::new(par, i + 1))
    }
}

fn new_element(cx: &Cx<'_, '_, '_>, name: &str) -> NodeId {
    crate::dom::create_element(&mut cx.dom_mut(), ns!(html), name)
}

/// Gives an empty block a `<br>` so its line stays.
fn keep_line(cx: &Cx<'_, '_, '_>, block: NodeId) -> R<()> {
    let empty = {
        let dom = cx.dom();
        dom.get(block).is_some()
            && leaves(&dom, block)
                .iter()
                .all(|&l| is_text(&dom, l) && dom.node_length(l) == 0)
    };
    if empty {
        let br = new_element(cx, "br");
        crate::dom::insert(cx, block, br, None)?;
    }
    Ok(())
}

fn merge(cx: &Cx<'_, '_, '_>, into: NodeId, from: NodeId) -> R<Boundary> {
    // A placeholder `<br>` at the end of the block joined into goes.
    let last = cx.dom().last_child(into);
    if let Some(l) = last.filter(|&l| is_br(&cx.dom(), l)) {
        crate::dom::detach(cx, l)?;
    }
    let caret = last_position(&cx.dom(), into);
    let kids: Vec<NodeId> = cx.dom().children(from).collect();
    let only_br = kids.len() == 1 && is_br(&cx.dom(), kids[0]);
    if !only_br {
        for k in kids {
            crate::dom::insert(cx, into, k, None)?;
        }
    }
    crate::dom::detach(cx, from)?;
    keep_line(cx, into)?;
    Ok(caret)
}

/// Inserts text at `p`, into an adjacent text node where there is one.
fn insert_text_at(cx: &Cx<'_, '_, '_>, p: Boundary, text: &str) -> R<Boundary> {
    let n = utf16(text).len() as u32;
    if is_text(&cx.dom(), p.node) {
        cx.dom_mut()
            .replace_data(p.node, p.offset, 0, text)
            .map_err(|_| gone())?;
        return Ok(Boundary::new(p.node, p.offset + n));
    }
    let (before, after, kids) = {
        let dom = cx.dom();
        let before = p
            .offset
            .checked_sub(1)
            .and_then(|o| dom.child_at(p.node, o));
        (
            before,
            dom.child_at(p.node, p.offset),
            dom.children(p.node).count(),
        )
    };
    if let Some(b) = before.filter(|&b| is_text(&cx.dom(), b)) {
        let len = cx.dom().node_length(b);
        cx.dom_mut()
            .replace_data(b, len, 0, text)
            .map_err(|_| gone())?;
        return Ok(Boundary::new(b, len + n));
    }
    if let Some(a) = after.filter(|&a| is_text(&cx.dom(), a)) {
        cx.dom_mut()
            .replace_data(a, 0, 0, text)
            .map_err(|_| gone())?;
        return Ok(Boundary::new(a, n));
    }
    // Typing into an empty block replaces its placeholder `<br>`.
    let mut reference = after;
    let only = (kids == 1)
        .then(|| cx.dom().first_child(p.node))
        .flatten()
        .filter(|&c| is_br(&cx.dom(), c));
    if let Some(only) = only {
        reference = None;
        crate::dom::detach(cx, only)?;
    }
    let t = cx.dom_mut().create(NodeData::Text(text.to_string()));
    crate::dom::insert(cx, p.node, t, reference)?;
    Ok(Boundary::new(t, n))
}

/// Splits the block at `p` (Enter): the content after `p` moves to a new block after it.
fn insert_paragraph(cx: &Cx<'_, '_, '_>, host: NodeId, p: Boundary) -> R<Boundary> {
    let block = block_of(&cx.dom(), p.node, host);
    let (container, new) = if block == host {
        (host, new_element(cx, "div"))
    } else {
        let copy = crate::dom::copy_within(&mut cx.dom_mut(), block, false).ok_or_else(gone)?;
        (block, copy)
    };
    let end = Boundary::new(container, cx.dom().node_length(container));
    let tail = crate::dom::ranges::extract(cx, p, end)?;
    if block == host {
        crate::dom::insert(cx, host, new, None)?;
    } else {
        let parent = cx.dom().parent(block).ok_or_else(gone)?;
        let next = cx.dom().next_sibling(block);
        crate::dom::insert(cx, parent, new, next)?;
    }
    crate::dom::insert(cx, new, tail, None)?;
    if block != host {
        keep_line(cx, block)?;
    }
    keep_line(cx, new)?;
    Ok(first_position(&cx.dom(), new))
}

fn insert_line_break(cx: &Cx<'_, '_, '_>, p: Boundary) -> R<Boundary> {
    let br = new_element(cx, "br");
    let (parent, reference) = if is_text(&cx.dom(), p.node) {
        let parent = cx.dom().parent(p.node).ok_or_else(gone)?;
        let tail = cx
            .dom_mut()
            .split_text(p.node, p.offset)
            .map_err(|_| gone())?;
        (parent, Some(tail))
    } else {
        (p.node, cx.dom().child_at(p.node, p.offset))
    };
    crate::dom::insert(cx, parent, br, reference)?;
    let i = cx.dom().index_of(br);
    Ok(Boundary::new(parent, i + 1))
}

/// Deletes `s..e` in editable content, joining the end's block into the start's.
fn delete_range(cx: &Cx<'_, '_, '_>, host: NodeId, s: Boundary, e: Boundary) -> R<Boundary> {
    let (sb, eb) = {
        let dom = cx.dom();
        (block_of(&dom, s.node, host), block_of(&dom, e.node, host))
    };
    let p = crate::dom::ranges::delete(cx, s, e)?;
    let alive = cx.dom().get(sb).is_some() && cx.dom().get(eb).is_some();
    if alive
        && sb != eb
        && eb != host
        && !cx.dom().is_inclusive_ancestor(eb, sb)
        && !cx.dom().is_inclusive_ancestor(sb, eb)
    {
        return merge(cx, sb, eb);
    }
    if sb != host && cx.dom().get(sb).is_some() {
        keep_line(cx, sb)?;
    }
    Ok(p)
}

fn edit_host(
    cx: &mut Cx<'_, '_, '_>,
    host: NodeId,
    kind: Kind,
    data: Option<&str>,
    scripted: bool,
) -> R<bool> {
    let (s, e) = host_selection(cx, host);
    let collapsed = s == e;
    let del = match kind {
        Kind::DeleteBackward | Kind::DeleteWordBackward if collapsed => Some(delete_at(
            cx,
            host,
            s,
            false,
            kind == Kind::DeleteWordBackward,
        )),
        Kind::DeleteForward | Kind::DeleteWordForward if collapsed => Some(delete_at(
            cx,
            host,
            s,
            true,
            kind == Kind::DeleteWordForward,
        )),
        _ => None,
    };
    let target = match &del {
        Some(Del::Nothing) => return Ok(false),
        Some(Del::Range(a, b)) => (*a, *b),
        Some(Del::Merge { into, from }) => {
            let dom = cx.dom();
            (last_position(&dom, *into), first_position(&dom, *from))
        }
        None => (s, e),
    };
    if matches!(kind, Kind::DeleteByCut) && collapsed {
        return Ok(false);
    }
    if !scripted
        && !input_event(
            cx,
            host,
            "beforeinput",
            kind,
            data.filter(|_| kind.inserts()),
            vec![target],
        )?
    {
        return Ok(false);
    }
    if cx.dom().get(host).is_none() {
        return Ok(false);
    }
    // A listener may have moved the selection: the edit applies to where it is now.
    let (s, e) = host_selection(cx, host);
    let caret = match kind {
        Kind::InsertText | Kind::InsertReplacementText | Kind::InsertFromPaste => {
            let mut p = if s == e {
                s
            } else {
                delete_range(cx, host, s, e)?
            };
            let text = data.unwrap_or_default();
            let mut lines = text.split('\n').peekable();
            while let Some(line) = lines.next() {
                let line = line.trim_end_matches('\r');
                if !line.is_empty() {
                    p = insert_text_at(cx, p, line)?;
                }
                if lines.peek().is_some() {
                    p = insert_paragraph(cx, host, p)?;
                }
            }
            p
        }
        Kind::InsertParagraph => {
            let p = if s == e {
                s
            } else {
                delete_range(cx, host, s, e)?
            };
            insert_paragraph(cx, host, p)?
        }
        Kind::InsertLineBreak => {
            let p = if s == e {
                s
            } else {
                delete_range(cx, host, s, e)?
            };
            insert_line_break(cx, p)?
        }
        Kind::DeleteByCut => delete_range(cx, host, s, e)?,
        _ if s != e => delete_range(cx, host, s, e)?,
        _ => {
            let word = matches!(kind, Kind::DeleteWordBackward | Kind::DeleteWordForward);
            let forward = matches!(kind, Kind::DeleteForward | Kind::DeleteWordForward);
            match delete_at(cx, host, s, forward, word) {
                Del::Range(a, b) => delete_range(cx, host, a, b)?,
                Del::Merge { into, from } => merge(cx, into, from)?,
                Del::Nothing => s,
            }
        }
    };
    if cx.dom().get(caret.node).is_some() {
        collapse_at(cx, caret);
    }
    input_event(
        cx,
        host,
        "input",
        kind,
        data.filter(|_| kind.inserts()),
        Vec::new(),
    )?;
    Ok(true)
}

// ----- caret keys -----

/// Moves (or with `extend`, extends) the caret of the focused editable element or the document
/// selection. Returns whether something editable handled it.
pub(crate) fn move_caret(
    cx: &mut Cx<'_, '_, '_>,
    el: Option<NodeId>,
    s: Step,
    extend: bool,
) -> R<bool> {
    if let Some(el) = el
        && text_control(&cx.dom(), el)
    {
        let value = control_value(cx, el)?;
        let u = utf16(&value);
        let (a, b, d) = control_sel(cx, el)?;
        let (anchor, focus) = if d == Direction::Backward {
            (b, a)
        } else {
            (a, b)
        };
        let to = if !extend && a != b && matches!(s.unit, Granularity::Character) {
            if s.forward { b } else { a }
        } else {
            control_step(&u, focus as usize, s) as u32
        };
        if extend {
            let (lo, hi) = (anchor.min(to), anchor.max(to));
            let dir = if to < anchor {
                Direction::Backward
            } else {
                Direction::Forward
            };
            set_control_sel(cx, el, lo, hi, dir)?;
        } else {
            set_control_sel(cx, el, to, to, Direction::None)?;
        }
        return Ok(true);
    }
    let Some((anchor, focus)) = anchor_focus(cx) else {
        return Ok(false);
    };
    let collapsed = anchor == focus;
    let to = if !extend && !collapsed && matches!(s.unit, Granularity::Character) {
        let (st, en, _, _) = current(cx).ok_or_else(gone)?;
        if s.forward { en } else { st }
    } else {
        step(cx, focus, s)
    };
    if extend {
        select(cx, anchor, to);
    } else {
        collapse_at(cx, to);
    }
    Ok(el.is_some_and(|e| editing_host(&cx.dom(), e).is_some()))
}

/// Selects everything in the focused editable element, else the document's body.
pub(crate) fn select_all(cx: &mut Cx<'_, '_, '_>, el: Option<NodeId>) -> R<()> {
    if let Some(el) = el {
        if text_control(&cx.dom(), el) {
            let len = control_len(cx, el)?;
            return set_control_sel(cx, el, 0, len, Direction::None);
        }
        let host = editing_host(&cx.dom(), el);
        if let Some(h) = host {
            let (a, b) = {
                let dom = cx.dom();
                (first_position(&dom, h), last_position(&dom, h))
            };
            select(cx, a, b);
            return Ok(());
        }
    }
    let body = {
        let dom = cx.dom();
        body_of(&dom)
    };
    if let Some(b) = body {
        let len = cx.dom().node_length(b);
        select(cx, Boundary::new(b, 0), Boundary::new(b, len));
    }
    Ok(())
}

/// Focusing editable content puts the caret at its start unless the selection is in it;
/// focusing a text control collapses the document selection before it.
pub(crate) fn focused(cx: &mut Cx<'_, '_, '_>, el: NodeId) {
    let (control, host) = {
        let dom = cx.dom();
        (text_control(&dom, el), editing_host(&dom, el))
    };
    if control {
        let at = {
            let dom = cx.dom();
            dom.parent(el).map(|p| Boundary::new(p, dom.index_of(el)))
        };
        if let Some(at) = at {
            collapse_at(cx, at);
        }
    } else if let Some(h) = host {
        let inside = current(cx).is_some_and(|(s, e, _, _)| {
            let dom = cx.dom();
            dom.is_inclusive_ancestor(h, s.node) && dom.is_inclusive_ancestor(h, e.node)
        });
        if !inside {
            let p = first_position(&cx.dom(), h);
            collapse_at(cx, p);
        }
    }
}

/// Selects `text` (its first occurrence) inside `el`: within a text control's value, or in
/// the element's text. Returns whether it was found.
pub(crate) fn select_text(cx: &mut Cx<'_, '_, '_>, el: NodeId, text: &str) -> R<bool> {
    let needle = utf16(text);
    if needle.is_empty() {
        return Ok(false);
    }
    if text_control(&cx.dom(), el) {
        let v = utf16(&control_value(cx, el)?);
        let Some(i) = v.windows(needle.len()).position(|w| w == needle.as_slice()) else {
            return Ok(false);
        };
        crate::dom::event_target::move_focus(cx, Some(el))?;
        return set_control_sel(
            cx,
            el,
            i as u32,
            (i + needle.len()) as u32,
            Direction::Forward,
        )
        .map(|()| true);
    }
    // The element's text nodes concatenated, searched, mapped back to nodes.
    let nodes: Vec<(NodeId, Vec<u16>)> = {
        let dom = cx.dom();
        dom.traverse(el)
            .filter_map(|e| match e {
                pageon_dom::Edge::Enter(n) if is_text(&dom, n) => Some((n, text_units(&dom, n))),
                _ => None,
            })
            .collect()
    };
    let all: Vec<u16> = nodes.iter().flat_map(|(_, u)| u.iter().copied()).collect();
    let Some(i) = all
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
    else {
        return Ok(false);
    };
    let at = |pos: usize, end: bool| {
        let mut base = 0;
        for (n, u) in &nodes {
            let len = u.len();
            if pos < base + len || (end && pos == base + len) {
                return Boundary::new(*n, (pos - base) as u32);
            }
            base += len;
        }
        let (n, u) = nodes.last().expect("found text has nodes");
        Boundary::new(*n, u.len() as u32)
    };
    let (a, b) = (at(i, false), at(i + needle.len(), true));
    let host = editing_host(&cx.dom(), el);
    if let Some(h) = host
        && cx.st().focused != Some(h)
    {
        crate::dom::event_target::move_focus(cx, Some(h))?;
    }
    select(cx, a, b);
    Ok(true)
}

// ----- pointer selection -----

/// Selection by a pointer press at a client point: a caret there, a word on a double click,
/// a block on a triple click; with `extend`, from the anchor to the point.
pub(crate) fn pointer_down(
    cx: &mut Cx<'_, '_, '_>,
    x: f64,
    y: f64,
    count: u32,
    extend: bool,
) -> R<()> {
    let hit = cx.realm.layout().caret_at(x as f32, y as f32);
    let Some((node, offset)) = hit else {
        return Ok(());
    };
    let p = Boundary::new(node, offset);
    // A press inside a text control puts its caret at the end of its value.
    let control = {
        let dom = cx.dom();
        std::iter::once(node)
            .chain(dom.ancestors(node))
            .find(|&a| text_control(&dom, a))
    };
    if let Some(c) = control {
        let len = control_len(cx, c)?;
        return set_control_sel(cx, c, len, len, Direction::None);
    }
    match count {
        2 => {
            let a = step(
                cx,
                Boundary::new(p.node, p.offset),
                Step {
                    forward: false,
                    unit: Granularity::Word,
                },
            );
            let a = if is_text(&cx.dom(), p.node) && a.node == p.node {
                word_start(&cx.dom(), p)
            } else {
                a
            };
            let b = word_end(&cx.dom(), p);
            select(cx, a, b);
        }
        3 => {
            let scope = scope_of(cx, node);
            let (a, b) = {
                let dom = cx.dom();
                let bl = block_of(&dom, node, scope);
                (first_position(&dom, bl), last_position(&dom, bl))
            };
            select(cx, a, b);
        }
        _ if extend => {
            let anchor = anchor_focus(cx).map_or(p, |(a, _)| a);
            select(cx, anchor, p);
        }
        _ => collapse_at(cx, p),
    }
    Ok(())
}

fn word_start(dom: &Dom, p: Boundary) -> Boundary {
    let units = text_units(dom, p.node);
    let w = words(&units);
    let mut start = 0;
    for &(b, _) in &w {
        if b > p.offset as usize {
            break;
        }
        start = b;
    }
    if w.iter().any(|&(b, like)| b > p.offset as usize && like) || start == p.offset as usize {
        Boundary::new(p.node, start as u32)
    } else {
        p
    }
}

fn word_end(dom: &Dom, p: Boundary) -> Boundary {
    if !is_text(dom, p.node) {
        return p;
    }
    let units = text_units(dom, p.node);
    words(&units)
        .into_iter()
        .find(|&(b, _)| b > p.offset as usize)
        .map_or(p, |(b, _)| Boundary::new(p.node, b as u32))
}

/// A pointer drag's end: the selection extends from where the press was to the point.
pub(crate) fn pointer_drag(cx: &mut Cx<'_, '_, '_>, x: f64, y: f64) -> R<()> {
    let hit = cx.realm.layout().caret_at(x as f32, y as f32);
    if let (Some((n, o)), Some((anchor, _))) = (hit, anchor_focus(cx)) {
        select(cx, anchor, Boundary::new(n, o));
    }
    Ok(())
}

// ----- the clipboard -----

/// Whether the page has transient activation (a recent trusted click or key).
pub(crate) fn activated(cx: &Cx<'_, '_, '_>) -> bool {
    let now = now(cx);
    cx.st().history.activated(now)
}

/// Whether text holds a value the vault filled into this page.
fn holds_vault_value(cx: &Cx<'_, '_, '_>, text: &str) -> bool {
    cx.st()
        .vault_values
        .iter()
        .any(|v| !v.is_empty() && text.contains(v.as_str()))
}

/// Puts `clip` on the session clipboard, unless it carries a vault value.
pub(crate) fn write_clipboard(cx: &Cx<'_, '_, '_>, clip: Option<Clip>) {
    if let Some(c) = &clip
        && (holds_vault_value(cx, &c.text)
            || c.html.as_deref().is_some_and(|h| holds_vault_value(cx, h)))
    {
        return;
    }
    let mut sh = cx.realm.shared.borrow_mut();
    sh.clipboard = clip;
    sh.clipboard_changed = true;
}

/// What a copy takes: the focused control's selection (never a password or a vault value) or
/// the document selection's text and markup.
fn selection_clip(cx: &mut Cx<'_, '_, '_>, focus: Option<NodeId>) -> Option<Clip> {
    if let Some(f) = focus.filter(|&f| text_control(&cx.dom(), f)) {
        let secret = {
            let dom = cx.dom();
            dom.get(f)
                .and_then(|n| n.attr("type"))
                .is_some_and(|t| t.eq_ignore_ascii_case("password"))
                || dom.control(f).is_some_and(|c| c.from_vault)
        };
        if secret {
            return None;
        }
        let t = control_selected_text(cx, f).filter(|t| !t.is_empty())?;
        return Some(Clip {
            text: t,
            html: None,
        });
    }
    let (s, e, _, _) = current(cx).filter(|(s, e, _, _)| s != e)?;
    let vault = {
        let dom = cx.dom();
        [s.node, e.node].iter().any(|&n| {
            editing_host(&dom, n).is_some_and(|h| dom.control(h).is_some_and(|c| c.from_vault))
        })
    };
    if vault {
        return None;
    }
    let text = crate::dom::ranges::text(&cx.dom(), s, e);
    let html = crate::dom::ranges::clone(cx, s, e)
        .ok()
        .map(|f| cx.dom().to_html(f));
    Some(Clip { text, html })
}

fn clipboard_target(cx: &Cx<'_, '_, '_>) -> Option<NodeId> {
    let focused = cx.st().focused.filter(|&f| cx.dom().get(f).is_some());
    focused
        .or_else(|| {
            current(cx).map(|(s, _, _, _)| {
                let dom = cx.dom();
                if is_text(&dom, s.node) {
                    dom.parent(s.node).unwrap_or(s.node)
                } else {
                    s.node
                }
            })
        })
        .or_else(|| body_of(&cx.dom()))
}

fn clipboard_event(
    cx: &mut Cx<'_, '_, '_>,
    target: NodeId,
    ty: &str,
    items: Vec<crate::dom::dnd::Item>,
    read_only: bool,
) -> R<(bool, u32)> {
    let mut dt = crate::dom::dnd::DtData::drag(items);
    dt.mode = if read_only {
        crate::dom::dnd::Mode::ReadOnly
    } else {
        crate::dom::dnd::Mode::ReadWrite
    };
    let dt_id = cx
        .st()
        .alloc(Iface::DataTransfer, Slot::DataTransfer(Box::new(dt)));
    let ev = EventData {
        ty: ty.into(),
        bubbles: true,
        cancelable: true,
        composed: true,
        initialized: true,
        trusted: true,
        time: now(cx),
        drag: Some(dt_id),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::ClipboardEvent, Slot::Event(Box::new(ev)));
    let go = crate::dom::event_target::dispatch(cx, Target::Node(target), e)?;
    Ok((go, dt_id))
}

fn transfer_clip(cx: &Cx<'_, '_, '_>, dt: u32) -> Option<Clip> {
    let st = cx.st();
    let Some(Slot::DataTransfer(d)) = st.slot(dt) else {
        return None;
    };
    let pairs = d.strings();
    let get = |t: &str| pairs.iter().find(|(ty, _)| ty == t).map(|(_, v)| v.clone());
    let text = get("text/plain");
    let html = get("text/html");
    (text.is_some() || html.is_some()).then(|| Clip {
        text: text.unwrap_or_default(),
        html,
    })
}

/// Copy (Ctrl+C, `execCommand('copy')`): the `copy` event, then the selection or what the
/// page set on the clipboard.
pub(crate) fn copy(cx: &mut Cx<'_, '_, '_>, cut: bool) -> R<()> {
    let Some(target) = clipboard_target(cx) else {
        return Ok(());
    };
    let focus = cx.st().focused;
    let (go, dt) = clipboard_event(
        cx,
        target,
        if cut { "cut" } else { "copy" },
        Vec::new(),
        false,
    )?;
    if !go {
        let clip = transfer_clip(cx, dt);
        write_clipboard(cx, clip);
        return Ok(());
    }
    let Some(clip) = selection_clip(cx, focus) else {
        return Ok(());
    };
    write_clipboard(cx, Some(clip));
    if cut && let Some(f) = focus.or(Some(target)) {
        edit(cx, f, Kind::DeleteByCut, None, false)?;
    }
    Ok(())
}

/// Paste (Ctrl+V): the `paste` event with the session clipboard, then the text inserted at the
/// caret unless the page handled it.
pub(crate) fn paste(cx: &mut Cx<'_, '_, '_>) -> R<()> {
    let Some(target) = clipboard_target(cx) else {
        return Ok(());
    };
    let clip = cx.realm.shared.borrow().clipboard.clone();
    let mut items = Vec::new();
    if let Some(c) = &clip {
        items.push(crate::dom::dnd::Item::Text {
            ty: "text/plain".into(),
            data: c.text.clone(),
        });
        if let Some(h) = &c.html {
            items.push(crate::dom::dnd::Item::Text {
                ty: "text/html".into(),
                data: h.clone(),
            });
        }
    }
    let (go, _) = clipboard_event(cx, target, "paste", items, true)?;
    if go && let Some(c) = clip {
        edit(cx, target, Kind::InsertFromPaste, Some(&c.text), false)?;
    }
    Ok(())
}

// ----- execCommand -----

const COMMANDS: &[&str] = &[
    "copy",
    "cut",
    "delete",
    "forwarddelete",
    "insertlinebreak",
    "insertparagraph",
    "inserttext",
    "selectall",
];

fn focused_editable(cx: &Cx<'_, '_, '_>) -> Option<NodeId> {
    let f = cx.st().focused.filter(|&f| cx.dom().get(f).is_some())?;
    place(cx, f).map(|_| f)
}

/// `document.execCommand`.
pub(crate) fn exec(cx: &mut Cx<'_, '_, '_>, command: &str, value: &str) -> R<bool> {
    let c = command.to_ascii_lowercase();
    match c.as_str() {
        "copy" | "cut" => {
            if !activated(cx) {
                return Ok(false);
            }
            copy(cx, c == "cut")?;
            Ok(true)
        }
        "selectall" => {
            let f = cx.st().focused;
            select_all(cx, f)?;
            Ok(true)
        }
        _ => {
            let kind = match c.as_str() {
                "inserttext" => Kind::InsertText,
                "delete" => Kind::DeleteBackward,
                "forwarddelete" => Kind::DeleteForward,
                "insertlinebreak" => Kind::InsertLineBreak,
                "insertparagraph" => Kind::InsertParagraph,
                _ => return Ok(false),
            };
            let Some(f) = focused_editable(cx) else {
                return Ok(false);
            };
            edit(cx, f, kind, Some(value), true)?;
            Ok(true)
        }
    }
}

/// `queryCommandSupported`.
pub(crate) fn supported(command: &str) -> bool {
    COMMANDS.contains(&command.to_ascii_lowercase().as_str())
}

/// `queryCommandEnabled`.
pub(crate) fn enabled(cx: &Cx<'_, '_, '_>, command: &str) -> bool {
    let c = command.to_ascii_lowercase();
    supported(&c) && (matches!(c.as_str(), "selectall" | "copy") || focused_editable(cx).is_some())
}

/// The DataTransfer of a clipboard or input event (slab id).
pub(crate) fn event_transfer(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
    let Obj::Slab(id) = this else {
        return Err(Error::Type("Illegal invocation".into()));
    };
    match cx.st().slot(id) {
        Some(Slot::Event(e)) => Ok(e.drag.map(Obj::Slab)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// `true` when `n` is in the document of this realm (not a parsed one).
pub(crate) fn in_this_document(dom: &Dom, n: NodeId) -> bool {
    root_of(dom, n) == dom.root()
}

/// The document's `<body>`.
pub(crate) fn body_of(dom: &Dom) -> Option<NodeId> {
    let html = dom
        .children(dom.root())
        .find(|&c| tag(dom, c).as_deref() == Some("html"))?;
    dom.children(html)
        .find(|&c| tag(dom, c).as_deref() == Some("body"))
}

/// `ElementContentEditable`.
pub mod element_content_editable {
    use super::*;

    pub fn get_content_editable(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let v = cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.attr("contenteditable").map(str::to_ascii_lowercase));
        Ok(match v.as_deref() {
            Some("" | "true") => "true",
            Some("false") => "false",
            Some("plaintext-only") => "plaintext-only",
            _ => "inherit",
        }
        .into())
    }

    pub fn set_content_editable(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let id = this.node()?;
        match v.to_ascii_lowercase().as_str() {
            "inherit" => cx
                .dom_mut()
                .remove_attr(id, "contenteditable")
                .map_err(|_| gone()),
            l @ ("true" | "false" | "plaintext-only") => cx
                .dom_mut()
                .set_attr(id, "contenteditable", l)
                .map_err(|_| gone()),
            _ => Err(Error::dom(
                "SyntaxError",
                format!(
                    "The value provided ('{v}') is not one of 'true', 'false', 'plaintext-only', or 'inherit'."
                ),
            )),
        }
    }

    pub fn get_is_content_editable(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        let id = this.node()?;
        let dom = cx.dom();
        Ok(editing_host(&dom, id).is_some() && in_this_document(&dom, id))
    }
}

/// `navigator.clipboard`.
pub mod navigator {
    use super::*;

    pub fn get_clipboard(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(crate::dom::window_same(
            cx,
            "clipboard",
            Iface::Clipboard,
            Slot::Clipboard,
        ))
    }

    pub fn get_credentials(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(crate::dom::credentials::container(cx))
    }
}

/// `Clipboard`: writing needs transient activation; reading is never granted (pages read what
/// the agent pastes from the `paste` event).
pub mod clipboard {
    use super::*;

    pub fn read_text(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<crate::cv::Value> {
        crate::dom::net::settled(
            cx,
            Err(Error::dom("NotAllowedError", "Read permission denied.")),
        )
    }

    pub fn write_text(cx: &mut Cx<'_, '_, '_>, _this: Obj, data: String) -> R<crate::cv::Value> {
        if !activated(cx) {
            return crate::dom::net::settled(
                cx,
                Err(Error::dom(
                    "NotAllowedError",
                    "Document is not focused or has no user activation.",
                )),
            );
        }
        write_clipboard(
            cx,
            Some(Clip {
                text: data,
                html: None,
            }),
        );
        crate::dom::net::settled_unit(cx, Ok(()))
    }
}

/// `ClipboardEvent`.
pub mod clipboard_event {
    use super::*;
    use crate::generated::ClipboardEventInit;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: ClipboardEventInit) -> R<Obj> {
        let drag = match init.clipboard_data {
            Some(Obj::Slab(id)) => Some(id),
            _ => None,
        };
        let data = EventData {
            ty,
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            initialized: true,
            time: now(cx),
            drag,
            ..EventData::default()
        };
        Ok(cx.alloc(Iface::ClipboardEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_clipboard_data(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        event_transfer(cx, this)
    }
}

/// The selection members of text controls (HTML §4.10.18.7).
pub(crate) mod sel {
    use super::*;

    fn el(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Option<NodeId>> {
        let id = this.node()?;
        Ok(has_selection_api(&cx.dom(), id).then_some(id))
    }

    fn need(cx: &Cx<'_, '_, '_>, this: Obj) -> R<NodeId> {
        el(cx, this)?.ok_or_else(|| {
            Error::dom(
                "InvalidStateError",
                "The input element's type does not support selection.",
            )
        })
    }

    fn dir_of(s: Option<&str>) -> Direction {
        match s {
            Some("forward") => Direction::Forward,
            Some("backward") => Direction::Backward,
            _ => Direction::None,
        }
    }

    fn dir_name(d: Direction) -> &'static str {
        match d {
            Direction::Backward => "backward",
            // Chrome reports "forward" for a selection without a direction.
            _ => "forward",
        }
    }

    pub fn select(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let Some(id) = el(cx, this)? else {
            return Ok(());
        };
        let len = control_len(cx, id)?;
        set_control_sel(cx, id, 0, len, Direction::None)
    }

    pub fn start(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<u32>> {
        let Some(id) = el(cx, this)? else {
            return Ok(None);
        };
        Ok(Some(control_sel(cx, id)?.0))
    }

    pub fn end(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<u32>> {
        let Some(id) = el(cx, this)? else {
            return Ok(None);
        };
        Ok(Some(control_sel(cx, id)?.1))
    }

    pub fn direction(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        let Some(id) = el(cx, this)? else {
            return Ok(None);
        };
        Ok(Some(dir_name(control_sel(cx, id)?.2).into()))
    }

    pub fn set_start(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        let id = need(cx, this)?;
        let (_, e, d) = control_sel(cx, id)?;
        set_control_sel(cx, id, v, e.max(v), d)
    }

    pub fn set_end(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        let id = need(cx, this)?;
        let (s, _, d) = control_sel(cx, id)?;
        set_control_sel(cx, id, s.min(v), v, d)
    }

    pub fn set_direction(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<String>) -> R<()> {
        let id = need(cx, this)?;
        let (s, e, _) = control_sel(cx, id)?;
        set_control_sel(cx, id, s, e, dir_of(v.as_deref()))
    }

    pub fn set_range(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        start: u32,
        end: u32,
        direction: Option<String>,
    ) -> R<()> {
        let id = need(cx, this)?;
        let start = start.min(end);
        set_control_sel(cx, id, start, end, dir_of(direction.as_deref()))
    }

    /// `setRangeText(replacement[, start, end[, selectionMode]])`.
    pub fn set_range_text(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        replacement: String,
        range: Option<(u32, u32, String)>,
    ) -> R<()> {
        let id = need(cx, this)?;
        let value = control_value(cx, id)?;
        let u = utf16(&value);
        let len = u.len() as u32;
        let (sel_s, sel_e, _) = control_sel(cx, id)?;
        let (start, end, mode) = match range {
            Some((s, e, m)) => {
                if s > e {
                    return Err(Error::dom(
                        "IndexSizeError",
                        "The start index is larger than the end index.",
                    ));
                }
                (s.min(len), e.min(len), m)
            }
            None => (sel_s, sel_e, "preserve".into()),
        };
        let r = utf16(&replacement);
        let mut out = u[..start as usize].to_vec();
        out.extend_from_slice(&r);
        out.extend_from_slice(&u[end as usize..]);
        set_control_value(cx, id, String::from_utf16_lossy(&out));
        let new_end = start + r.len() as u32;
        let delta = r.len() as i64 - i64::from(end - start);
        let (s, e) = match mode.as_str() {
            "select" => (start, new_end),
            "start" => (start, start),
            "end" => (new_end, new_end),
            _ => {
                let shift = |p: u32| -> u32 {
                    if p > end {
                        (i64::from(p) + delta) as u32
                    } else if p > start {
                        start
                    } else {
                        p
                    }
                };
                let (a, b) = (shift(sel_s), shift(sel_e));
                if sel_s == start && sel_e == end {
                    (start, new_end)
                } else {
                    (a, b)
                }
            }
        };
        set_control_sel(cx, id, s, e, Direction::None)
    }
}

/// `setRangeText` with its overloads (one or three/four arguments), for both text controls.
pub(crate) fn set_range_text_any(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        let this = cx
            .this(args, Iface::HTMLInputElement)
            .or_else(|_| cx.this(args, Iface::HTMLTextAreaElement))?;
        if args.length() == 0 {
            return Err(Error::args("HTMLInputElement.setRangeText", 0));
        }
        let replacement = crate::cv::string(cx, args.get(0))?;
        let range = if args.length() >= 3 {
            let s = crate::cv::int::<u32>(cx, args.get(1))?;
            let e = crate::cv::int::<u32>(cx, args.get(2))?;
            let m = args.get(3);
            let mode = if m.is_undefined() {
                "preserve".to_string()
            } else {
                crate::cv::string(cx, m)?
            };
            if !matches!(mode.as_str(), "select" | "start" | "end" | "preserve") {
                return Err(Error::Type(format!(
                    "The provided value '{mode}' is not a valid enum value of type SelectionMode."
                )));
            }
            Some((s, e, mode))
        } else if args.length() == 2 {
            return Err(Error::Type(
                "Failed to execute 'setRangeText': 2 arguments is not valid.".into(),
            ));
        } else {
            None
        };
        sel::set_range_text(cx, this, replacement, range)?;
        Ok(crate::cv::Out::Undefined)
    });
}

/// Puts the caret at the end of an editable element's content.
pub(crate) fn caret_to_end(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<()> {
    if text_control(&cx.dom(), el) {
        let len = control_len(cx, el)?;
        return set_control_sel(cx, el, len, len, Direction::None);
    }
    let host = editing_host(&cx.dom(), el);
    if let Some(h) = host {
        let p = last_position(&cx.dom(), h);
        collapse_at(cx, p);
    }
    Ok(())
}
