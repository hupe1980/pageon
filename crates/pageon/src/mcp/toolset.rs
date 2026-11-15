//! The model vendors' browser tools, executed on the session: Anthropic's
//! `browser_toolset_20260801` and `computer_toolset_20260801`, and OpenAI's `computer` tool.
//! An application sends the vendor's calls to `pageon/toolset/run` and returns the results;
//! the tabs, refs, recording and policy are the session's own.
//!
//! The vendors' contract is kept exactly: calls run in order; the first failure stops the
//! turn and every later call is answered with the vendor's not-executed text; every call gets
//! one result; Anthropic results echo `toolset_name`. Members the definition did not offer
//! are refused by name.

use base64::Engine as _;
use serde_json::{Value, json};

use super::{Backend, MAX_TABS};
use crate::{Action, ActionError, At, Direction, Filter, Outcome, Read, ScrollBy};

/// Which vendor tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// Anthropic `browser_toolset_20260801`.
    Browser,
    /// Anthropic `computer_toolset_20260801`.
    Computer,
    /// OpenAI `computer`.
    OpenAi,
}

impl Kind {
    pub(super) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "browser_toolset_20260801" => Self::Browser,
            "computer_toolset_20260801" => Self::Computer,
            "computer" => Self::OpenAi,
            _ => return None,
        })
    }

    fn toolset_name(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            _ => "computer",
        }
    }

    fn not_executed(self) -> &'static str {
        match self {
            Self::Browser => "Not executed: an earlier action in this turn failed.",
            _ => "Not executed: an earlier computer action in this turn failed.",
        }
    }
}

/// What the session offers beyond the vendors' defaults.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Offer {
    /// The operator allowed `javascript_exec` (`--allow-javascript-exec`).
    pub javascript_exec: bool,
    /// Files are staged (`--file`), so `file_upload` can work.
    pub file_upload: bool,
    /// The application asked for `read_console`.
    pub console: bool,
    /// The application asked for `read_network`.
    pub network: bool,
}

/// What the executor keeps between turns.
#[derive(Debug, Default)]
pub(super) struct State {
    /// The pointer, in viewport pixels (`cursor_position`, button presses without a point).
    cursor: (f64, f64),
    /// Console lines since the last `read_console` (the latest 200).
    console: Vec<String>,
    /// Requests since the last `read_network` (the latest 500).
    network: Vec<String>,
}

/// `read_page` and `get_page_text` output cap (the vendor's).
const MAX_CHARS: usize = 50_000;
/// `read_page`'s token budget: about 50,000 characters of o200k text.
const READ_TOKENS: usize = 12_000;
/// Pixels per scroll-wheel click.
const WHEEL_PX: f64 = 100.0;

/// The tool entry to send to the vendor.
pub(super) fn definition(kind: Kind, offer: Offer) -> Value {
    match kind {
        Kind::Browser => {
            let mut configs = serde_json::Map::new();
            for (member, on) in [
                ("javascript_exec", offer.javascript_exec),
                ("file_upload", offer.file_upload),
                ("read_console", offer.console),
                ("read_network", offer.network),
            ] {
                if on {
                    configs.insert(member.into(), json!({"enabled": true}));
                }
            }
            if configs.is_empty() {
                json!({"type": "browser_toolset_20260801"})
            } else {
                json!({"type": "browser_toolset_20260801", "configs": configs})
            }
        }
        Kind::Computer => json!({"type": "computer_toolset_20260801"}),
        Kind::OpenAi => json!({"type": "computer"}),
    }
}

/// Runs one turn's calls. `confirmed` names the OpenAI calls whose pending safety checks the
/// application approved.
pub(super) fn run(
    kind: Kind,
    b: &mut dyn Backend,
    offer: Offer,
    st: &mut State,
    calls: &[Value],
    confirmed: &[String],
) -> Vec<Value> {
    if kind == Kind::OpenAi {
        return calls
            .iter()
            .filter(|c| c["type"] == "computer_call")
            .map(|c| openai_call(b, st, c, confirmed))
            .collect();
    }
    let mut failed = false;
    let mut out = Vec::new();
    for call in calls.iter().filter(|c| {
        c["type"] == "tool_use"
            && c.get("toolset_name")
                .and_then(Value::as_str)
                .is_none_or(|t| t == kind.toolset_name())
    }) {
        let id = call["id"].as_str().unwrap_or_default();
        if failed {
            out.push(result(kind, id, vec![text(kind.not_executed())], true));
            continue;
        }
        let member = call["name"].as_str().unwrap_or_default();
        let input = call.get("input").cloned().unwrap_or(json!({}));
        let r = match kind {
            Kind::Browser => browser(b, offer, st, member, &input),
            _ => computer(b, st, member, &input),
        };
        match r {
            Ok(content) => out.push(result(kind, id, content, false)),
            Err(e) => {
                failed = true;
                out.push(result(kind, id, vec![text(&e)], true));
            }
        }
    }
    out
}

fn result(kind: Kind, id: &str, content: Vec<Value>, error: bool) -> Value {
    let mut r = json!({
        "type": "tool_result",
        "tool_use_id": id,
        "toolset_name": kind.toolset_name(),
        "content": content,
    });
    if error {
        r["is_error"] = json!(true);
    }
    r
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn png(bytes: &[u8]) -> Value {
    json!({"type": "image", "source": {"type": "base64", "media_type": "image/png",
        "data": base64::engine::general_purpose::STANDARD.encode(bytes)}})
}

/// The vendors' error text for an action error.
fn action_error(e: &ActionError) -> String {
    match e {
        ActionError::StaleRef(r) | ActionError::UnknownRef(r) => format!(
            "Error: {r} is stale or not found on the current page. Re-read the page to get fresh references."
        ),
        e => format!("Error: {e}. {}.", e.guidance()),
    }
}

/// Runs an action, keeping its requests and console lines for the reads.
fn act(b: &mut dyn Backend, st: &mut State, action: Action) -> Result<Outcome, String> {
    let o = b.act(action).map_err(|e| action_error(&e))?;
    keep(st, &o);
    Ok(o)
}

fn keep(st: &mut State, o: &Outcome) {
    st.network
        .extend(o.requests.iter().map(ToString::to_string));
    let n = st.network.len().saturating_sub(500);
    st.network.drain(..n);
    st.console
        .extend(o.console.iter().map(|(level, t)| format!("[{level}] {t}")));
    let n = st.console.len().saturating_sub(200);
    st.console.drain(..n);
}

/// What an action did beyond its own verb, as one line (empty when nothing).
fn did(o: &Outcome) -> String {
    let line = o.to_string();
    match line.split_once(" · ") {
        Some((_, rest)) => format!("\n{rest}"),
        None => String::new(),
    }
}

// ------------------------------------------------------------------------------------------
// Anthropic browser toolset

/// The members on by default, and those the session must offer.
fn offered(member: &str, offer: Offer) -> Option<bool> {
    Some(match member {
        "navigate" | "screenshot" | "zoom" | "left_click" | "right_click" | "middle_click"
        | "double_click" | "triple_click" | "hover" | "left_click_drag" | "left_mouse_down"
        | "left_mouse_up" | "mouse_move" | "scroll" | "scroll_to" | "type" | "key" | "hold_key"
        | "wait" | "read_page" | "find" | "get_page_text" | "form_input" | "new_tab"
        | "list_tabs" | "switch_tab" | "close_tab" => true,
        "file_upload" => offer.file_upload,
        "read_console" => offer.console,
        "read_network" => offer.network,
        "javascript_exec" => offer.javascript_exec,
        _ => return None,
    })
}

fn browser(
    b: &mut dyn Backend,
    offer: Offer,
    st: &mut State,
    member: &str,
    input: &Value,
) -> Result<Vec<Value>, String> {
    match offered(member, offer) {
        None => return Err(format!("Error: {member} is not a browser toolset member.")),
        Some(false) => {
            return Err(format!(
                "Error: {member} is not enabled in this environment."
            ));
        }
        Some(true) => {}
    }
    // A call on another tab runs there, then the active tab is restored.
    let tab = input.get("tab_id").and_then(Value::as_str);
    let tab_member = matches!(member, "new_tab" | "list_tabs" | "switch_tab" | "close_tab");
    let previous = b.tabs().current_id();
    let other = match tab {
        Some(t) if !tab_member => {
            let h = handle(t).filter(|h| b.list().iter().any(|(x, ..)| x == h));
            let h = h.ok_or_else(|| format!("Error: no tab {t:?}. Use list_tabs."))?;
            (Some(h) != previous).then(|| b.select(h)).map(|_| h)
        }
        _ => None,
    };
    let r = browser_member(b, st, member, input);
    if other.is_some()
        && let Some(p) = previous
    {
        b.select(p);
    }
    r
}

fn browser_member(
    b: &mut dyn Backend,
    st: &mut State,
    member: &str,
    input: &Value,
) -> Result<Vec<Value>, String> {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    let need = |k: &str| s(k).ok_or_else(|| format!("Error: {member} needs {k}."));
    match member {
        "navigate" => {
            let url = need("url")?;
            let action = match url {
                "back" => Action::Back,
                "forward" => Action::Forward,
                "reload" => Action::Goto(current_url(b).ok_or("Error: no page to reload.")?),
                u => Action::Goto(web_url(u)?),
            };
            let o = if b.has_tab() {
                act(b, st, action)?
            } else {
                let Action::Goto(u) = action else {
                    return Err("Error: no page is open.".into());
                };
                let o = b
                    .open(&u)
                    .map_err(|e| format!("Error: {e}. {}.", e.guidance()))?;
                keep(st, &o);
                o
            };
            let url = current_url(b).unwrap_or_default();
            Ok(with_state(b, format!("Navigated to {url}.{}", did(&o)), &o))
        }
        "screenshot" => Ok(vec![png(&screenshot(b, None)?)]),
        "zoom" => Ok(vec![png(&zoom(b, input.get("region"))?)]),
        "left_click" | "right_click" | "middle_click" | "double_click" | "triple_click" => {
            let at = target(input.get("target"), member)?;
            let mods = mods(s("modifiers"))?;
            let (button, count) = click_kind(member);
            click(b, st, at, button, count, mods)
        }
        "hover" => {
            let at = target(input.get("target"), member)?;
            let (action, what) = match &at {
                At::Ref(r) => (Action::Hover(r.clone()), r.clone()),
                At::Point(x, y) => {
                    st.cursor = (*x, *y);
                    (Action::HoverAt(*x, *y), format!("{x},{y}"))
                }
            };
            let o = act(b, st, action)?;
            Ok(with_state(b, format!("Hovered {what}.{}", did(&o)), &o))
        }
        "left_click_drag" => {
            let (fx, fy) = point(input.get("from"), "from")?;
            let (tx, ty) = point(input.get("target"), "target")?;
            drag(b, st, (fx, fy), (tx, ty))
        }
        "left_mouse_down" | "left_mouse_up" | "mouse_move" => {
            let (x, y) = point(input.get("target"), "target")?;
            st.cursor = (x, y);
            let action = match member {
                "left_mouse_down" => Action::PointerDown(x, y),
                "left_mouse_up" => Action::PointerUp(x, y),
                _ => Action::HoverAt(x, y),
            };
            let o = act(b, st, action)?;
            let verb = match member {
                "left_mouse_down" => "Pressed the left button",
                "left_mouse_up" => "Released the left button",
                _ => "Moved the mouse",
            };
            Ok(with_state(b, format!("{verb} at {x},{y}.{}", did(&o)), &o))
        }
        "scroll" => {
            let (x, y) = point(input.get("target"), "target")?;
            let dir = s("scroll_direction")
                .and_then(direction)
                .ok_or("Error: scroll needs scroll_direction up, down, left or right.")?;
            let amount = input
                .get("scroll_amount")
                .and_then(Value::as_f64)
                .unwrap_or(3.0);
            if !(1.0..=10.0).contains(&amount) {
                return Err("Error: scroll_amount is 1 to 10.".into());
            }
            st.cursor = (x, y);
            wheel(b, st, (x, y), dir, amount)
        }
        "scroll_to" => {
            let At::Ref(r) = target(input.get("target"), member)? else {
                return Err("Error: scroll_to takes a ref target.".into());
            };
            let o = act(
                b,
                st,
                Action::Scroll {
                    target: Some(r.clone()),
                    by: ScrollBy::IntoView,
                    point: None,
                },
            )?;
            Ok(with_state(
                b,
                format!("Scrolled {r} into view.{}", did(&o)),
                &o,
            ))
        }
        "type" => type_text(b, st, need("text")?),
        "key" => {
            let repeat = repeat(input)?;
            key(b, st, need("text")?, repeat)
        }
        "hold_key" => hold(b, st, need("text")?, seconds(input, 30.0)?),
        "wait" => {
            let ms = seconds(input, 30.0)?;
            let o = act(b, st, Action::Wait(ms))?;
            Ok(with_state(b, format!("Waited {ms} ms.{}", did(&o)), &o))
        }
        "read_page" => {
            let filter = match s("filter") {
                None => Filter::Visible,
                Some("interactive") => Filter::Interactive,
                Some("all") => Filter::All,
                Some(f) => return Err(format!("Error: filter is interactive or all, not {f:?}.")),
            };
            let depth = input.get("depth").and_then(Value::as_u64).unwrap_or(15);
            if depth < 1 {
                return Err("Error: depth is at least 1.".into());
            }
            let page = b
                .read(Read::Page {
                    filter,
                    depth: u32::try_from(depth).unwrap_or(u32::MAX),
                    root: s("ref").map(str::to_string),
                    budget: crate::Budget::o200k(READ_TOKENS),
                })
                .map_err(|e| action_error(&e))?;
            Ok(vec![text(&capped(
                page,
                "[read_page output truncated at 50,000 characters; narrow it with depth or ref]",
            ))])
        }
        "find" => {
            let found = b
                .read(Read::Find {
                    query: need("query")?.to_string(),
                    limit: 20,
                })
                .map_err(|e| action_error(&e))?;
            Ok(vec![text(&found)])
        }
        "get_page_text" => {
            let page = b
                .read(Read::Markdown(crate::Budget::o200k(READ_TOKENS)))
                .map_err(|e| action_error(&e))?;
            Ok(vec![text(&capped(
                page,
                "[get_page_text output truncated at 50,000 characters]",
            ))])
        }
        "form_input" => {
            let At::Ref(r) = target(input.get("target"), member)? else {
                return Err("Error: form_input takes a ref target.".into());
            };
            form_input(b, st, &r, input.get("value"))
        }
        "file_upload" => {
            let At::Ref(r) = target(input.get("target"), member)? else {
                return Err("Error: file_upload takes a ref target.".into());
            };
            if input
                .get("paths")
                .and_then(Value::as_array)
                .is_some_and(|p| !p.is_empty())
            {
                return Err("Error: file_upload takes staged files as document_ids (pageon --file NAME@ORIGIN=PATH); paths are refused.".into());
            }
            let names: Vec<String> = input
                .get("document_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            if names.is_empty() {
                return Err(
                    "Error: file_upload needs document_ids: the names of staged files.".into(),
                );
            }
            let o = act(b, st, Action::Upload(r.clone(), names.clone()))?;
            Ok(with_state(
                b,
                format!("Uploaded {} to {r}.{}", names.join(", "), did(&o)),
                &o,
            ))
        }
        "read_console" => {
            let lines = std::mem::take(&mut st.console);
            Ok(vec![text(&if lines.is_empty() {
                "No console messages since the last read.".to_string()
            } else {
                lines.join("\n")
            })])
        }
        "read_network" => {
            let lines = std::mem::take(&mut st.network);
            Ok(vec![text(&if lines.is_empty() {
                "No requests since the last read.".to_string()
            } else {
                lines.join("\n")
            })])
        }
        "javascript_exec" => {
            let o = act(b, st, Action::Eval(need("text")?.to_string()))?;
            let value = o.value.clone().unwrap_or_else(|| "undefined".into());
            Ok(with_state(b, format!("{value}{}", did(&o)), &o))
        }
        "new_tab" => {
            if b.tabs().count() >= MAX_TABS {
                return Err(format!(
                    "Error: at most {MAX_TABS} tabs are open at once. Close one first."
                ));
            }
            let o = b
                .new_tab("about:blank")
                .map_err(|e| format!("Error: {e}."))?;
            keep(st, &o);
            let id = b.tabs().current_id().unwrap_or_default();
            Ok(vec![state(
                b,
                vec![json!({"type": "tab_opened", "tab_id": format!("t{id}")})],
            )])
        }
        "list_tabs" => Ok(vec![state(b, Vec::new())]),
        "switch_tab" | "close_tab" => {
            let t = need("tab_id")?;
            let h = handle(t).ok_or_else(|| format!("Error: no tab {t:?}. Use list_tabs."))?;
            let ok = if member == "switch_tab" {
                b.select(h)
            } else {
                b.close(h)
            };
            if !ok {
                return Err(format!("Error: no tab {t:?}. Use list_tabs."));
            }
            Ok(vec![state(b, Vec::new())])
        }
        _ => Err(format!("Error: {member} is not a browser toolset member.")),
    }
}

/// `tN` → N.
fn handle(t: &str) -> Option<u32> {
    t.strip_prefix('t')?.parse().ok()
}

fn current_url(b: &mut dyn Backend) -> Option<String> {
    let id = b.tabs().current_id()?;
    b.list()
        .into_iter()
        .find(|(h, ..)| *h == id)
        .map(|(_, u, _)| u)
}

/// An `http`/`https` URL (scheme-less means `https://`); anything else is refused.
fn web_url(u: &str) -> Result<String, String> {
    let u = u.trim();
    let candidate =
        if u.contains("://") || u.starts_with("about:") || u.contains(':') && !u.contains('.') {
            u.to_string()
        } else {
            format!("https://{u}")
        };
    match url::Url::parse(&candidate) {
        Ok(p) if matches!(p.scheme(), "http" | "https") => Ok(p.to_string()),
        Ok(p) if p.scheme() == "file" => {
            Err("Error: Navigation refused. Only http and https URLs are allowed.".into())
        }
        Ok(_) | Err(_)
            if !candidate.starts_with("https://") || url::Url::parse(&candidate).is_err() =>
        {
            match url::Url::parse(&format!("https://{u}")) {
                Ok(p) if !u.contains("://") && !u.contains(':') => Ok(p.to_string()),
                _ => Err("Error: Navigation refused. Only http and https URLs are allowed.".into()),
            }
        }
        _ => Err("Error: Navigation refused. Only http and https URLs are allowed.".into()),
    }
}

/// A `target`: `{type: "ref", ref}` or `{type: "coordinate", x, y}`.
fn target(t: Option<&Value>, member: &str) -> Result<At, String> {
    let t = t.ok_or_else(|| format!("Error: {member} needs a target."))?;
    match t["type"].as_str() {
        Some("ref") => t["ref"]
            .as_str()
            .map(|r| At::Ref(r.to_string()))
            .ok_or_else(|| format!("Error: {member}: a ref target needs ref.")),
        Some("coordinate") => {
            let (x, y) = point(Some(t), "target")?;
            Ok(At::Point(x, y))
        }
        _ => Err(format!(
            "Error: {member}: target is {{\"type\": \"ref\", \"ref\": …}} or {{\"type\": \"coordinate\", \"x\": …, \"y\": …}}."
        )),
    }
}

/// A coordinate target's point, checked against the viewport.
fn point(t: Option<&Value>, what: &str) -> Result<(f64, f64), String> {
    let t = t.ok_or_else(|| format!("Error: {what} is missing."))?;
    let (x, y) = match (t.get("x"), t.get("y")) {
        (Some(x), Some(y)) => (x.as_f64(), y.as_f64()),
        _ => match t.as_array().map(Vec::as_slice) {
            Some([x, y]) => (x.as_f64(), y.as_f64()),
            _ => (None, None),
        },
    };
    let (Some(x), Some(y)) = (x, y) else {
        return Err(format!("Error: {what} needs x and y in viewport pixels."));
    };
    let vp = crate::Viewport::default();
    if !(0.0..f64::from(vp.width)).contains(&x) || !(0.0..f64::from(vp.height)).contains(&y) {
        return Err(format!(
            "Error: {x},{y} is outside the {}×{} viewport.",
            vp.width, vp.height
        ));
    }
    Ok((x, y))
}

fn mods(spec: Option<&str>) -> Result<pageon_js::Mods, String> {
    let mut m = pageon_js::Mods::default();
    for part in spec
        .unwrap_or_default()
        .split('+')
        .filter(|p| !p.is_empty())
    {
        m = m.with(part.trim()).ok_or_else(|| {
            format!("Error: unknown modifier {part:?} (shift, ctrl, alt, super).")
        })?;
    }
    Ok(m)
}

fn click_kind(member: &str) -> (pageon_js::Button, u8) {
    match member {
        "right_click" => (pageon_js::Button::Right, 1),
        "middle_click" => (pageon_js::Button::Middle, 1),
        "double_click" => (pageon_js::Button::Left, 2),
        "triple_click" => (pageon_js::Button::Left, 3),
        _ => (pageon_js::Button::Left, 1),
    }
}

fn click(
    b: &mut dyn Backend,
    st: &mut State,
    at: At,
    button: pageon_js::Button,
    count: u8,
    mods: pageon_js::Mods,
) -> Result<Vec<Value>, String> {
    if let At::Point(x, y) = at {
        st.cursor = (x, y);
    }
    let action = match (&at, button, count, mods.none()) {
        (At::Ref(r), pageon_js::Button::Left, 1, true) => Action::Click(r.clone()),
        _ => Action::ClickWith {
            at: at.clone(),
            button,
            count,
            mods,
            nth: false,
        },
    };
    let o = act(b, st, action)?;
    let verb = match (button, count) {
        (pageon_js::Button::Right, _) => "Right-clicked",
        (pageon_js::Button::Middle, _) => "Middle-clicked",
        (_, 2) => "Double-clicked",
        (_, 3) => "Triple-clicked",
        _ => "Clicked",
    };
    let what = match (&at, &o.target) {
        (At::Point(x, y), Some(t)) => format!("{t} at {x},{y}"),
        (at, _) => at.to_string(),
    };
    Ok(with_state(b, format!("{verb} {what}.{}", did(&o)), &o))
}

fn drag(
    b: &mut dyn Backend,
    st: &mut State,
    from: (f64, f64),
    to: (f64, f64),
) -> Result<Vec<Value>, String> {
    act(b, st, Action::PointerDown(from.0, from.1))?;
    act(b, st, Action::HoverAt(to.0, to.1))?;
    let o = act(b, st, Action::PointerUp(to.0, to.1))?;
    st.cursor = to;
    let how = match &o.drag {
        Some(d) if d.dropped => " (dropped)",
        Some(_) => " (not accepted)",
        None => "",
    };
    Ok(with_state(
        b,
        format!(
            "Dragged from {},{} to {},{}{how}.{}",
            from.0,
            from.1,
            to.0,
            to.1,
            did(&o)
        ),
        &o,
    ))
}

fn direction(d: &str) -> Option<Direction> {
    Some(match d {
        "up" => Direction::Up,
        "down" => Direction::Down,
        "left" => Direction::Left,
        "right" => Direction::Right,
        _ => return None,
    })
}

fn wheel(
    b: &mut dyn Backend,
    st: &mut State,
    at: (f64, f64),
    dir: Direction,
    clicks: f64,
) -> Result<Vec<Value>, String> {
    let px = clicks * WHEEL_PX;
    let (dx, dy) = match dir {
        Direction::Up => (0.0, -px),
        Direction::Down => (0.0, px),
        Direction::Left => (-px, 0.0),
        Direction::Right => (px, 0.0),
    };
    let o = act(
        b,
        st,
        Action::Scroll {
            target: None,
            by: ScrollBy::Pixels(dx, dy),
            point: Some(at),
        },
    )?;
    let what = o.scrolled.join("; ");
    Ok(with_state(
        b,
        format!("Scrolled {dir} at {},{}: {what}.", at.0, at.1),
        &o,
    ))
}

fn type_text(b: &mut dyn Backend, st: &mut State, t: &str) -> Result<Vec<Value>, String> {
    let o = act(b, st, Action::TypeFocused(t.to_string()))?;
    let note = if o.unfocused {
        " Nothing editable had focus: the keys went to the page."
    } else {
        ""
    };
    Ok(with_state(
        b,
        format!("Typed the text.{note}{}", did(&o)),
        &o,
    ))
}

/// A key spec as the vendors send it: `Return`, `ctrl+a`, a sequence, ` ` for the space bar.
fn keys(spec: &str) -> String {
    if spec == " " {
        "space".into()
    } else {
        spec.trim().to_string()
    }
}

fn repeat(input: &Value) -> Result<usize, String> {
    let n = input.get("repeat").and_then(Value::as_u64).unwrap_or(1);
    if !(1..=100).contains(&n) {
        return Err("Error: repeat is 1 to 100.".into());
    }
    Ok(usize::try_from(n).unwrap_or(1))
}

fn key(b: &mut dyn Backend, st: &mut State, spec: &str, n: usize) -> Result<Vec<Value>, String> {
    let k = keys(spec);
    crate::tab::key_sequence(&k).map_err(|e| format!("Error: unknown key: {e}."))?;
    let o = act(b, st, Action::Press(vec![k.as_str(); n].join(" ")))?;
    let times = if n > 1 {
        format!(" {n} times")
    } else {
        String::new()
    };
    Ok(with_state(
        b,
        format!("Pressed {spec}{times}.{}", did(&o)),
        &o,
    ))
}

/// A `duration` in seconds, 0 to `max`, as milliseconds.
fn seconds(input: &Value, max: f64) -> Result<u64, String> {
    let s = input
        .get("duration")
        .and_then(Value::as_f64)
        .ok_or("Error: duration (seconds) is missing.")?;
    if !(0.0..=max).contains(&s) {
        return Err(format!("Error: duration is 0 to {max} seconds."));
    }
    Ok((s * 1000.0).round() as u64)
}

fn hold(b: &mut dyn Backend, st: &mut State, spec: &str, ms: u64) -> Result<Vec<Value>, String> {
    let k = keys(spec);
    pageon_js::chord(&k).ok_or_else(|| format!("Error: unknown key {spec:?}."))?;
    let o = act(b, st, Action::Hold(k, ms))?;
    Ok(with_state(
        b,
        format!("Held {spec} for {ms} ms.{}", did(&o)),
        &o,
    ))
}

fn form_input(
    b: &mut dyn Backend,
    st: &mut State,
    r: &str,
    value: Option<&Value>,
) -> Result<Vec<Value>, String> {
    let value = value.ok_or("Error: form_input needs value.")?;
    let line = b
        .read(Read::Line(r.to_string()))
        .map_err(|e| action_error(&e))?;
    let role = line.split_whitespace().next().unwrap_or_default();
    let action = match role {
        "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio" => {
            let want = value
                .as_bool()
                .ok_or_else(|| format!("Error: {r} is a {role}: its value is true or false."))?;
            let checked = line
                .split_whitespace()
                .any(|w| w == "checked" || w == "pressed");
            if checked == want {
                return Ok(vec![text(&format!(
                    "{r} is already {}.",
                    if want { "checked" } else { "unchecked" }
                ))]);
            }
            Action::Click(r.to_string())
        }
        "combobox" | "listbox" if line.contains('{') => {
            Action::Select(r.to_string(), vec![scalar(value, r)?])
        }
        "textbox" | "searchbox" | "spinbutton" | "combobox" | "slider" => {
            Action::Type(r.to_string(), scalar(value, r)?)
        }
        other => {
            return Err(format!(
                "Error: {r} is a {other}, not a form field. Use left_click or type."
            ));
        }
    };
    let o = act(b, st, action)?;
    Ok(with_state(b, format!("Set {r}.{}", did(&o)), &o))
}

fn scalar(v: &Value, r: &str) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(format!("Error: {r} takes a string or a number.")),
    }
}

fn capped(s: String, note: &str) -> String {
    if s.chars().count() <= MAX_CHARS {
        return s;
    }
    let mut t: String = s.chars().take(MAX_CHARS).collect();
    t.push('\n');
    t.push_str(note);
    t
}

fn screenshot(b: &mut dyn Backend, area: Option<crate::Area>) -> Result<Vec<u8>, String> {
    let o = crate::ScreenshotOptions {
        area: area.unwrap_or_default(),
        ..crate::ScreenshotOptions::default()
    };
    b.screenshot(&o)
        .map(|i| i.bytes)
        .map_err(|e| format!("Error: {e}."))
}

/// A region `[x0, y0, x1, y1]` of the viewport, magnified up to 2× (long edge ≤ 1,280).
fn zoom(b: &mut dyn Backend, region: Option<&Value>) -> Result<Vec<u8>, String> {
    let r: Vec<f64> = region
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let [x0, y0, x1, y1] = r[..] else {
        return Err("Error: zoom needs region [x0, y0, x1, y1] in viewport pixels.".into());
    };
    if x1 <= x0 || y1 <= y0 {
        return Err("Error: zoom's region needs x1 > x0 and y1 > y0.".into());
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let scale = (1280.0 / w.max(h)).clamp(0.25, 2.0) as f32;
    let o = crate::ScreenshotOptions {
        area: crate::Area::ViewportClip(pageon_layout::Rect {
            x: x0 as f32,
            y: y0 as f32,
            width: w as f32,
            height: h as f32,
        }),
        scale,
        ..crate::ScreenshotOptions::default()
    };
    b.screenshot(&o)
        .map(|i| i.bytes)
        .map_err(|e| format!("Error: {e}."))
}

/// A page string fit for the tab inventory: no control or line-separator characters, at most
/// 4,096 characters.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'))
        .take(4096)
        .collect()
}

/// A `browser_state` block: the full tab inventory and what changed.
fn state(b: &mut dyn Backend, changes: Vec<Value>) -> Value {
    let current = b.tabs().current_id();
    let tabs: Vec<Value> = b
        .list()
        .into_iter()
        .take(100)
        .map(|(h, url, title)| {
            let mut t =
                json!({"tab_id": format!("t{h}"), "title": clean(&title), "url": clean(&url)});
            if Some(h) == current {
                t["active"] = json!(true);
            }
            t
        })
        .collect();
    let mut s = json!({"type": "browser_state", "tabs": tabs});
    if !changes.is_empty() {
        s["state_changes"] = Value::Array(changes.into_iter().take(200).collect());
    }
    s
}

/// The result text, and a `browser_state` when the action started downloads.
fn with_state(b: &mut dyn Backend, t: String, o: &Outcome) -> Vec<Value> {
    let mut text_t = t;
    if !o.popups.is_empty() {
        text_t.push_str(&format!(
            "\nThe page asked to open {} in a new tab: open one with new_tab, then navigate.",
            o.popups
                .iter()
                .map(|u| pageon_agent::quote::quote(&clean(u)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut content = vec![text(&text_t)];
    let changes: Vec<Value> = o
        .downloads
        .iter()
        .flat_map(|d| {
            let id = d.handle.clone();
            [
                json!({"type": "download_started", "download_id": id, "url": clean(&d.url)}),
                json!({"type": "download_completed", "download_id": id, "url": clean(&d.url), "size_bytes": d.bytes.len()}),
            ]
        })
        .collect();
    if !changes.is_empty() {
        content.push(state(b, changes));
    }
    content
}

// ------------------------------------------------------------------------------------------
// Anthropic computer toolset

fn computer(
    b: &mut dyn Backend,
    st: &mut State,
    member: &str,
    input: &Value,
) -> Result<Vec<Value>, String> {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    let need = |k: &str| s(k).ok_or_else(|| format!("Error: {member} needs {k}."));
    let ok = || Ok(vec![text("OK")]);
    let at = |k: &str, st: &State| -> Result<(f64, f64), String> {
        match input.get(k) {
            Some(v) => point(Some(v), k),
            None => Ok(st.cursor),
        }
    };
    match member {
        "screenshot" => Ok(vec![png(&screenshot(b, None)?)]),
        "zoom" => Ok(vec![png(&zoom(b, input.get("region"))?)]),
        "left_click" | "right_click" | "middle_click" | "double_click" | "triple_click" => {
            let (x, y) = at("coordinate", st)?;
            let (button, count) = click_kind(member);
            click(b, st, At::Point(x, y), button, count, mods(s("text"))?)?;
            ok()
        }
        "left_click_drag" => {
            let from = point(input.get("start_coordinate"), "start_coordinate")?;
            let to = point(input.get("coordinate"), "coordinate")?;
            drag(b, st, from, to)?;
            ok()
        }
        "mouse_move" => {
            let (x, y) = point(input.get("coordinate"), "coordinate")?;
            st.cursor = (x, y);
            act(b, st, Action::HoverAt(x, y))?;
            ok()
        }
        "left_mouse_down" | "left_mouse_up" => {
            let (x, y) = st.cursor;
            act(
                b,
                st,
                if member == "left_mouse_down" {
                    Action::PointerDown(x, y)
                } else {
                    Action::PointerUp(x, y)
                },
            )?;
            ok()
        }
        "cursor_position" => Ok(vec![text(&format!(
            "X={}, Y={}",
            st.cursor.0.round(),
            st.cursor.1.round()
        ))]),
        "scroll" => {
            let (x, y) = at("coordinate", st)?;
            let dir = s("scroll_direction")
                .and_then(direction)
                .ok_or("Error: scroll needs scroll_direction up, down, left or right.")?;
            let amount = input
                .get("scroll_amount")
                .and_then(Value::as_f64)
                .unwrap_or(3.0);
            if !(0.0..=100.0).contains(&amount) {
                return Err("Error: scroll_amount is a number of wheel clicks, 0 to 100.".into());
            }
            st.cursor = (x, y);
            wheel(b, st, (x, y), dir, amount)?;
            ok()
        }
        "type" => {
            type_text(b, st, need("text")?)?;
            ok()
        }
        "key" => {
            let n = repeat(input)?;
            key(b, st, need("text")?, n)?;
            ok()
        }
        "hold_key" => {
            hold(b, st, need("text")?, seconds(input, 300.0)?)?;
            ok()
        }
        "wait" => {
            act(b, st, Action::Wait(seconds(input, 300.0)?))?;
            ok()
        }
        _ => Err(format!("Error: {member} is not a computer toolset member.")),
    }
}

// ------------------------------------------------------------------------------------------
// OpenAI computer

/// One `computer_call`: its actions in order, then a screenshot of the viewport.
fn openai_call(b: &mut dyn Backend, st: &mut State, call: &Value, confirmed: &[String]) -> Value {
    let id = call["call_id"].as_str().unwrap_or_default();
    let checks: Vec<Value> = call
        .get("pending_safety_checks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let approved = checks.is_empty() || confirmed.iter().any(|c| c == id);
    let mut error = None;
    if approved {
        let actions: Vec<Value> = match call.get("actions").and_then(Value::as_array) {
            Some(a) => a.clone(),
            None => call.get("action").cloned().into_iter().collect(),
        };
        for a in &actions {
            if let Err(e) = openai_action(b, st, a) {
                error = Some(e);
                break;
            }
        }
    }
    let shot = screenshot(b, None).unwrap_or_default();
    let mut out = json!({
        "type": "computer_call_output",
        "call_id": id,
        "output": {
            "type": "computer_screenshot",
            "image_url": format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&shot)),
        },
    });
    if approved && !checks.is_empty() {
        out["acknowledged_safety_checks"] = Value::Array(checks);
    }
    // OpenAI's output has no error field: a failure is reported beside it for the application.
    if let Some(e) = error {
        out["pageon_error"] = json!(e);
    } else if !approved {
        out["pageon_error"] =
            json!("Not executed: the call has pending safety checks nobody confirmed.");
    }
    out
}

fn openai_action(b: &mut dyn Backend, st: &mut State, a: &Value) -> Result<(), String> {
    let xy = || -> Result<(f64, f64), String> {
        point(Some(&json!({"x": a["x"], "y": a["y"]})), "x, y")
    };
    let held = || -> Result<pageon_js::Mods, String> {
        let mut m = pageon_js::Mods::default();
        for k in a
            .get("keys")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let k = k.as_str().unwrap_or_default();
            m = m
                .with(k)
                .ok_or_else(|| format!("Error: {k:?} is not a modifier key."))?;
        }
        Ok(m)
    };
    match a["type"].as_str().unwrap_or_default() {
        "click" => {
            let button = match a["button"].as_str().unwrap_or("left") {
                "back" => {
                    act(b, st, Action::Back)?;
                    return Ok(());
                }
                "forward" => {
                    act(b, st, Action::Forward)?;
                    return Ok(());
                }
                "right" => pageon_js::Button::Right,
                "wheel" | "middle" => pageon_js::Button::Middle,
                _ => pageon_js::Button::Left,
            };
            let (x, y) = xy()?;
            click(b, st, At::Point(x, y), button, 1, held()?).map(|_| ())
        }
        "double_click" => {
            let (x, y) = xy()?;
            click(b, st, At::Point(x, y), pageon_js::Button::Left, 2, held()?).map(|_| ())
        }
        "move" => {
            let (x, y) = xy()?;
            st.cursor = (x, y);
            act(b, st, Action::HoverAt(x, y)).map(|_| ())
        }
        "drag" => {
            let path: Vec<(f64, f64)> = a
                .get("path")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|p| point(Some(p), "path"))
                .collect::<Result<_, _>>()?;
            let (Some(&from), Some(&to)) = (path.first(), path.last()) else {
                return Err("Error: drag needs a path of points.".into());
            };
            act(b, st, Action::PointerDown(from.0, from.1))?;
            for &(x, y) in &path[1..] {
                act(b, st, Action::HoverAt(x, y))?;
            }
            act(b, st, Action::PointerUp(to.0, to.1))?;
            st.cursor = to;
            Ok(())
        }
        "scroll" => {
            let (x, y) = xy()?;
            let dx = a["scroll_x"].as_f64().unwrap_or(0.0);
            let dy = a["scroll_y"].as_f64().unwrap_or(0.0);
            st.cursor = (x, y);
            act(
                b,
                st,
                Action::Scroll {
                    target: None,
                    by: ScrollBy::Pixels(dx, dy),
                    point: Some((x, y)),
                },
            )
            .map(|_| ())
        }
        "keypress" => {
            let ks: Vec<String> = a
                .get("keys")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|k| k.as_str().map(str::to_string))
                .collect();
            if ks.is_empty() {
                return Err("Error: keypress needs keys.".into());
            }
            // The keys are pressed together: one chord.
            let chord = ks.join("+");
            pageon_js::chord(&chord).ok_or_else(|| format!("Error: unknown key in {ks:?}."))?;
            act(b, st, Action::Press(chord)).map(|_| ())
        }
        "type" => act(
            b,
            st,
            Action::TypeFocused(a["text"].as_str().unwrap_or_default().to_string()),
        )
        .map(|_| ()),
        "wait" => act(b, st, Action::Wait(1000)).map(|_| ()),
        "screenshot" => Ok(()),
        other => Err(format!("Error: unknown action {other:?}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_web_urls_only() {
        assert_eq!(web_url("example.com").unwrap(), "https://example.com/");
        assert_eq!(web_url("https://a.test/x").unwrap(), "https://a.test/x");
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "chrome://settings",
        ] {
            assert_eq!(
                web_url(bad).unwrap_err(),
                "Error: Navigation refused. Only http and https URLs are allowed."
            );
        }
    }

    #[test]
    fn page_strings_are_one_line() {
        assert_eq!(clean("a\nb\u{2028}c\u{0}d"), "abcd");
        assert_eq!(clean(&"x".repeat(5000)).len(), 4096);
    }

    #[test]
    fn definitions_offer_what_the_session_runs() {
        let d = definition(Kind::Browser, Offer::default());
        assert_eq!(d, json!({"type": "browser_toolset_20260801"}));
        let d = definition(
            Kind::Browser,
            Offer {
                javascript_exec: true,
                ..Offer::default()
            },
        );
        assert_eq!(d["configs"]["javascript_exec"]["enabled"], true);
        assert_eq!(offered("javascript_exec", Offer::default()), Some(false));
        assert_eq!(offered("nope", Offer::default()), None);
    }
}
