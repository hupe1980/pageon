//! Tabs: live pages an agent acts on. Each action resolves a ref from the tab's observation,
//! checks actionability, dispatches trusted input with the browser's default actions, settles
//! on virtual time, follows the navigation it started, and returns a typed outcome, or a
//! typed error that says what to do next.

use std::fmt;
use std::rc::Rc;
use std::str::FromStr;

use pageon_agent::{Episode, Mode, RefError};
use pageon_dom::NodeId;
use pageon_env::Env;
use pageon_js::{Dialog, NavRequest};

use crate::load::{Loaded, Start};
use crate::{
    Budget, Document, LiveDoc, LoadError, LoadOptions, PageError, RequestEntry, Session, Viewport,
    Visibility,
};

/// An action on a tab.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Action {
    /// Click an element (pointer, mouse, focus, click, then its default action).
    Click(String),
    /// Click whatever is at a viewport point (CSS pixels).
    ClickAt(f64, f64),
    /// Move the pointer to a viewport point (CSS pixels).
    HoverAt(f64, f64),
    /// Replace an editable element's text, typing it key by key.
    Type(String, String),
    /// Put staged files (by name) or downloads (`download:N`) into a file input, or into the
    /// file chooser that activating the element opens.
    Upload(String, Vec<String>),
    /// Press a key on the focused element (`Enter`, `Tab`, `Escape`, `Backspace`, `a`, …).
    Press(String),
    /// Select options of a `<select>` by value or label; on any other element, select that
    /// text in it (the first occurrence).
    Select(String, Vec<String>),
    /// Put text on the session clipboard and paste it (Ctrl+V) into the element, or into the
    /// focused one.
    Paste(Option<String>, String),
    /// Move the pointer onto an element.
    Hover(String),
    /// Drag an element onto another (centre to centre): a trusted press, moves and release,
    /// with HTML drag and drop when the element is draggable.
    Drag(String, String),
    /// Press the pointer's button at a viewport point (CSS pixels; protocol clients).
    PointerDown(f64, f64),
    /// Release the pointer's button at a viewport point (protocol clients).
    PointerUp(f64, f64),
    /// A step of a drag the protocol client drives (CDP `Input.dispatchDragEvent`).
    DragEvent {
        /// Enter, over, drop or cancel.
        kind: pageon_js::DragKind,
        /// Viewport point.
        x: f64,
        /// Viewport point.
        y: f64,
        /// The drag's data: (type, value).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        items: Vec<(String, String)>,
    },
    /// Click with a button, a click count (1–3) and modifiers, on an element or at a viewport
    /// point (`click e12 right`, `click 640,380 double ctrl+shift`).
    ClickWith {
        /// The element or the point.
        at: At,
        /// The button.
        #[serde(default)]
        button: pageon_js::Button,
        /// Clicks (1 single, 2 double, 3 triple).
        #[serde(default = "one")]
        count: u8,
        /// Modifier keys held.
        #[serde(default)]
        mods: pageon_js::Mods,
        /// Only the `count`-th click of a sequence the protocol client drives press by press
        /// (CDP sends a double click as two clicks, the second with `clickCount` 2).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        nth: bool,
    },
    /// Hold a chord down for a time (ms of virtual time), as a person holding a key: `keydown`,
    /// repeats every 33 ms after 500 ms, `keyup` (`hold shift 2000`).
    Hold(String, u64),
    /// Type text at the end of the focused element, as keystrokes (`type "hello"`).
    TypeFocused(String),
    /// Evaluate a JavaScript expression in an isolated world over the page's document (its DOM,
    /// cookies and same-origin `fetch`, not its script globals); the value is the outcome's
    /// `value`. Only when the operator allows it (`--allow-javascript-exec`).
    Eval(String),
    /// Let virtual time pass (ms).
    Wait(u64),
    /// Load a URL in this tab.
    Goto(String),
    /// Go back in the tab's history.
    Back,
    /// Go forward in the tab's history.
    Forward,
    /// Scroll as a person does: the viewport (or the container `target`) with the wheel, or
    /// `target` into view.
    Scroll {
        /// The element: a container to scroll, or (with [`ScrollBy::IntoView`]) an element to
        /// bring into view. `None`: what is under the pointer, else the viewport.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        /// How far.
        by: ScrollBy,
        /// The viewport point the wheel turns at (without a target).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        point: Option<(f64, f64)>,
    },
    /// Call a tool the page registered (WebMCP) with a JSON object; `origin` picks
    /// among frames that registered the same name.
    Tool {
        /// The tool's name.
        name: String,
        /// The registering document's origin, when it matters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        /// The input object.
        input: serde_json::Value,
    },
}

/// Where a [`Action::ClickWith`] lands: an element (by ref) or a viewport point.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum At {
    /// An element.
    Ref(String),
    /// A viewport point (CSS pixels).
    Point(f64, f64),
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ref(r) => f.write_str(r),
            Self::Point(x, y) => write!(f, "{x},{y}"),
        }
    }
}

fn one() -> u8 {
    1
}

/// `ctrl+shift` for modifiers (empty for none).
fn mods_text(m: pageon_js::Mods) -> String {
    let mut parts = Vec::new();
    if m.ctrl {
        parts.push("ctrl");
    }
    if m.shift {
        parts.push("shift");
    }
    if m.alt {
        parts.push("alt");
    }
    if m.meta {
        parts.push("meta");
    }
    parts.join("+")
}

/// How far a [`Action::Scroll`] goes.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollBy {
    /// Bring the target into view (centred, if it is not fully visible).
    IntoView,
    /// One page (87.5% of what scrolls) in a direction.
    Page(Direction),
    /// CSS pixels: right and down are positive.
    Pixels(f64, f64),
}

// Pixel amounts are finite (parsing refuses NaN and infinities), so equality is total.
impl Eq for ScrollBy {}

/// A scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Towards the end.
    Down,
    /// Towards the start.
    Up,
    /// Leftwards.
    Left,
    /// Rightwards.
    Right,
}

impl Direction {
    fn sign(self) -> (f64, f64) {
        match self {
            Self::Down => (0.0, 1.0),
            Self::Up => (0.0, -1.0),
            Self::Left => (-1.0, 0.0),
            Self::Right => (1.0, 0.0),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "down" => Self::Down,
            "up" => Self::Up,
            "left" => Self::Left,
            "right" => Self::Right,
            _ => return None,
        })
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Down => "down",
            Self::Up => "up",
            Self::Left => "left",
            Self::Right => "right",
        })
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Click(r) => write!(f, "click {r}"),
            Self::ClickAt(x, y) => write!(f, "click {x},{y}"),
            Self::HoverAt(x, y) => write!(f, "hover {x},{y}"),
            Self::Type(r, t) => write!(f, "type {r} {t:?}"),
            Self::Upload(r, names) => write!(f, "upload {r} {}", names.join(",")),
            Self::Press(k) => write!(f, "press {k}"),
            Self::Select(r, v) => {
                write!(f, "select {r}")?;
                for x in v {
                    write!(f, " {x:?}")?;
                }
                Ok(())
            }
            Self::Hover(r) => write!(f, "hover {r}"),
            Self::Drag(a, b) => write!(f, "drag {a} {b}"),
            Self::PointerDown(x, y) => write!(f, "pointerdown {x},{y}"),
            Self::PointerUp(x, y) => write!(f, "pointerup {x},{y}"),
            Self::DragEvent { kind, x, y, .. } => write!(f, "dragevent {kind:?} {x},{y}"),
            Self::ClickWith {
                at,
                button,
                count,
                mods,
                ..
            } => {
                write!(f, "click {at}")?;
                match button {
                    pageon_js::Button::Left => {}
                    pageon_js::Button::Middle => f.write_str(" middle")?,
                    pageon_js::Button::Right => f.write_str(" right")?,
                }
                match count {
                    2 => f.write_str(" double")?,
                    3 => f.write_str(" triple")?,
                    _ => {}
                }
                if !mods.none() {
                    write!(f, " {}", mods_text(*mods))?;
                }
                Ok(())
            }
            Self::Hold(k, ms) => write!(f, "hold {k} {ms}"),
            Self::TypeFocused(t) => write!(f, "type {t:?}"),
            Self::Paste(Some(r), t) => write!(f, "paste {r} {t:?}"),
            Self::Paste(None, t) => write!(f, "paste {t:?}"),
            Self::Eval(e) => write!(f, "eval {e}"),
            Self::Wait(ms) => write!(f, "wait {ms}"),
            Self::Goto(u) => write!(f, "goto {u}"),
            Self::Back => f.write_str("back"),
            Self::Forward => f.write_str("forward"),
            Self::Scroll { target, by, point } => {
                f.write_str("scroll")?;
                if let Some(t) = target {
                    write!(f, " {t}")?;
                }
                if let Some((x, y)) = point {
                    write!(f, " {x},{y}")?;
                }
                match by {
                    ScrollBy::IntoView => Ok(()),
                    ScrollBy::Page(d) => write!(f, " {d}"),
                    ScrollBy::Pixels(x, y) => match (*x == 0.0, *y == 0.0) {
                        (true, false) if *y > 0.0 => write!(f, " down {y}"),
                        (true, false) => write!(f, " up {}", -y),
                        (false, true) if *x > 0.0 => write!(f, " right {x}"),
                        (false, true) => write!(f, " left {}", -x),
                        _ => write!(f, " by {x},{y}"),
                    },
                }
            }
            Self::Tool {
                name,
                origin,
                input,
            } => match origin {
                Some(o) => write!(f, "tool {name}@{o} {input}"),
                None => write!(f, "tool {name} {input}"),
            },
        }
    }
}

impl FromStr for Action {
    type Err = String;

    /// `click e5`, `click 120,40` (a viewport point), `type e3 hello world`, `drag e3 e7`,
    /// `press Enter`,
    /// `select e7 Germany`, `hover e2`, `hover 120,40`, `wait 500`, `goto https://…`, `back`,
    /// `forward`, `scroll down`, `scroll down 300`, `scroll by 120,-40`, `scroll e12` (into
    /// view), `scroll e5 down`.
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (verb, rest) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
        let rest = rest.trim();
        let (first, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let unquote = |t: &str| {
            let t = t.trim();
            t.strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(t)
                .to_string()
        };
        let need = |what: &str| {
            if first.is_empty() {
                Err(format!("`{verb}` needs {what}"))
            } else {
                Ok(first.to_string())
            }
        };
        let point = || {
            let (x, y) = first.split_once(',')?;
            Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?))
        };
        Ok(match verb.to_ascii_lowercase().as_str() {
            "click" => {
                let at = match point() {
                    Some((x, y)) => At::Point(x, y),
                    None => At::Ref(need("a ref, e.g. `click e5`")?),
                };
                let (mut button, mut count, mut mods) =
                    (pageon_js::Button::Left, 1u8, pageon_js::Mods::default());
                for w in tail.split_whitespace() {
                    match w.to_ascii_lowercase().as_str() {
                        "left" => button = pageon_js::Button::Left,
                        "middle" => button = pageon_js::Button::Middle,
                        "right" => button = pageon_js::Button::Right,
                        "double" => count = 2,
                        "triple" => count = 3,
                        m => {
                            for part in m.split('+') {
                                mods = mods.with(part).ok_or_else(|| {
                                    format!(
                                        "`click` takes left, middle, right, double, triple and modifiers (ctrl+shift), not {w:?}"
                                    )
                                })?;
                            }
                        }
                    }
                }
                match (at, button, count, mods.none()) {
                    (At::Ref(r), pageon_js::Button::Left, 1, true) => Self::Click(r),
                    (At::Point(x, y), pageon_js::Button::Left, 1, true) => Self::ClickAt(x, y),
                    (at, button, count, _) => Self::ClickWith {
                        at,
                        button,
                        count,
                        mods,
                        nth: false,
                    },
                }
            }
            "hover" => match point() {
                Some((x, y)) => Self::HoverAt(x, y),
                None => Self::Hover(need("a ref, e.g. `hover e5`")?),
            },
            // `type "text"`: into the focused element.
            "type" if rest.starts_with('"') => Self::TypeFocused(unquote(rest)),
            "type" => Self::Type(need("a ref and text, e.g. `type e3 hello`")?, unquote(tail)),
            // `paste "text"`: into the focused element.
            "paste" if rest.starts_with('"') => Self::Paste(None, unquote(rest)),
            "paste" => Self::Paste(
                Some(need("a ref and text, e.g. `paste e3 hello`")?),
                unquote(tail),
            ),
            "hold" => {
                let key = need("a key and milliseconds, e.g. `hold shift 2000`")?;
                let ms = tail.trim().parse().map_err(|_| {
                    format!("`hold` needs milliseconds after the key, not {tail:?}")
                })?;
                Self::Hold(key, ms)
            }
            "eval" => {
                if rest.is_empty() {
                    return Err("`eval` needs an expression, e.g. `eval document.title`".into());
                }
                Self::Eval(rest.to_string())
            }
            "drag" => {
                let help =
                    "`drag` needs two refs: the element and where to drop it, e.g. `drag e3 e7`";
                let to = tail.trim();
                if first.is_empty() || to.is_empty() || to.contains(char::is_whitespace) {
                    return Err(help.into());
                }
                Self::Drag(first.to_string(), to.to_string())
            }
            "upload" => {
                let r = need("a ref and file names, e.g. `upload e7 cv`")?;
                let names: Vec<String> = unquote(tail)
                    .split(',')
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .collect();
                if names.is_empty() {
                    return Err("`upload` needs a staged file name, e.g. `upload e7 cv`".into());
                }
                Self::Upload(r, names)
            }
            "select" => Self::Select(
                need("a ref and an option or text, e.g. `select e7 Germany`")?,
                vec![unquote(tail)],
            ),
            // A key, a chord or a sequence: `press Enter`, `press ctrl+a`, `press Down Down`.
            "press" if rest.is_empty() => {
                return Err("`press` needs a key, e.g. `press Enter`".into());
            }
            "press" => Self::Press(rest.to_string()),
            "wait" => Self::Wait(
                need("milliseconds, e.g. `wait 500`")?
                    .parse()
                    .map_err(|_| format!("`wait` needs milliseconds, not {first:?}"))?,
            ),
            "goto" => Self::Goto(need("a URL")?),
            "back" => Self::Back,
            "forward" => Self::Forward,
            "scroll" => parse_scroll(rest)?,
            "tool" => {
                let target =
                    need("a tool name and a JSON object, e.g. `tool search {\"q\": \"shoes\"}`")?;
                let (name, origin) = match target.split_once('@') {
                    Some((n, o)) => (n.to_string(), Some(o.to_string())),
                    None => (target, None),
                };
                let input: serde_json::Value = if tail.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(tail.trim())
                        .map_err(|e| format!("`tool {name}` needs a JSON object: {e}"))?
                };
                Self::Tool {
                    name,
                    origin,
                    input,
                }
            }
            other => {
                return Err(format!(
                    "unknown action {other:?}: use click, type, upload, drag, press, hold, select, hover, scroll, wait, goto, back, forward, tool or eval"
                ));
            }
        })
    }
}

/// The chords of a `press`: one key (`Enter`, ` `), a chord (`ctrl+a`), or a space-separated
/// sequence where `xN` repeats the key before it (`Down Down Enter`, `Tab x3`); key names as
/// the DOM names them or as computer-use models send them (`Return`, `Page_Down`, `super`).
pub(crate) fn key_sequence(spec: &str) -> Result<Vec<(pageon_js::Mods, String)>, String> {
    if spec.chars().count() == 1 {
        return Ok(vec![(pageon_js::Mods::default(), spec.to_string())]);
    }
    let mut keys: Vec<(pageon_js::Mods, String)> = Vec::new();
    for word in spec.split_whitespace() {
        let times = word
            .strip_prefix('x')
            .or_else(|| word.strip_prefix('*'))
            .and_then(|n| n.parse::<usize>().ok());
        match (times, keys.last().cloned()) {
            (Some(n), Some(last)) if (1..=100).contains(&n) => {
                keys.extend(std::iter::repeat_n(last, n - 1));
            }
            (Some(_), Some(_)) => return Err(format!("{word:?}: repeat a key 1 to 100 times")),
            _ => keys.push(
                pageon_js::chord(word).ok_or_else(|| {
                    format!("the key {word:?} (keys: Enter, Tab, Escape, ctrl+a, shift+Tab, ArrowDown or Down, Page_Down, F5, a)")
                })?,
            ),
        }
        if keys.len() > 1000 {
            return Err("more than 1,000 keys in one press".into());
        }
    }
    if keys.is_empty() {
        return Err("a press without a key".into());
    }
    Ok(keys)
}

/// `down`, `down 300`, `by 120,-40`, `e12`, `e5 down [PX]`.
fn parse_scroll(rest: &str) -> Result<Action, String> {
    const HELP: &str = "`scroll` needs a direction, a ref or both: `scroll down`, `scroll down 300`, `scroll by 0,300`, `scroll e12`, `scroll e5 down`";
    let words: Vec<&str> = rest.split_whitespace().collect();
    let is_ref = |w: &str| Direction::parse(w).is_none() && w != "by";
    // A viewport point (`scroll 640,380 down 300`): the wheel turns there.
    let at = |w: &str| {
        let (x, y) = w.split_once(',')?;
        Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?))
            .filter(|(x, y)| x.is_finite() && y.is_finite())
    };
    let (point, words) = match words.split_first() {
        Some((w, more)) if at(w).is_some() => (at(w), more),
        _ => (None, words.as_slice()),
    };
    let (target, words) = match words.split_first() {
        Some((w, more)) if point.is_none() && is_ref(w) => (Some((*w).to_string()), more),
        _ => (None, words),
    };
    let px = |w: &str| {
        w.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| format!("`scroll` needs pixels as a number, not {w:?}"))
    };
    let by = match words {
        [] if target.is_some() => ScrollBy::IntoView,
        [] if point.is_some() => return Err(HELP.into()),
        [d] => ScrollBy::Page(Direction::parse(d).ok_or(HELP)?),
        [d, n] if Direction::parse(d).is_some() => {
            let (sx, sy) = Direction::parse(d).map_or((0.0, 0.0), Direction::sign);
            let n = px(n)?;
            ScrollBy::Pixels(sx * n, sy * n)
        }
        ["by", xy] => {
            let (x, y) = xy.split_once(',').ok_or(HELP)?;
            let num = |v: &str| {
                v.trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| format!("`scroll by` needs two numbers, not {xy:?}"))
            };
            ScrollBy::Pixels(num(x)?, num(y)?)
        }
        _ => return Err(HELP.into()),
    };
    Ok(Action::Scroll { target, by, point })
}

/// A file a navigation delivered instead of a page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Download {
    /// The handle to upload it again (`download:1`), to its own origin.
    #[serde(default)]
    pub handle: String,
    /// URL.
    pub url: String,
    /// File name (`Content-Disposition`, or the URL's last segment).
    pub filename: String,
    /// MIME type essence.
    pub content_type: String,
    /// The bytes (base64 in JSON).
    #[serde(with = "pageon_env::b64")]
    pub bytes: Vec<u8>,
}

/// How settling ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Settle {
    /// Virtual time spent (ms).
    pub virtual_ms: u64,
    /// Timers still pending (0: the page went idle).
    pub pending_timers: usize,
}

/// Whether an action's effect is known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Done: what it did is in the outcome and the next view.
    #[default]
    Ok,
    /// Nothing changed yet, and a timer the action set is still pending ([`Outcome::due_ms`]):
    /// the page may still be working. Wait, then look again.
    Unknown,
}

impl Status {
    fn is_ok(&self) -> bool {
        *self == Self::Ok
    }
}

/// How far ahead an action's own timer still makes its outcome unknown (virtual ms).
pub(crate) const UNKNOWN_HORIZON_MS: u64 = 10_000;

/// What an action did.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Outcome {
    /// The action, as text (`click e5`).
    pub action: String,
    /// Whether its effect is known.
    #[serde(default, skip_serializing_if = "Status::is_ok")]
    pub status: Status,
    /// When unknown: virtual ms until the action's soonest pending timer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_ms: Option<u64>,
    /// The URL the tab navigated to, if it did.
    pub navigated: Option<String>,
    /// The navigation stayed in the page (the history API, a fragment): no document loaded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub same_document: bool,
    /// Dialogs the page opened, answered by the dialog policy.
    pub dialogs: Vec<Dialog>,
    /// Popup URLs (`window.open`, `target=_blank`): open them with [`Session::open`].
    pub popups: Vec<String>,
    /// Downloads the action started (also in [`Tab::downloads`]).
    pub downloads: Vec<Download>,
    /// New page errors.
    pub errors: Vec<PageError>,
    /// Requests made.
    pub requests: Vec<RequestEntry>,
    /// How settling ended.
    pub settled: Settle,
    /// The site refused the agent on the page this action navigated to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<crate::Refusal>,
    /// What a page tool returned, as JSON text: page data, cut at 32 KB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    /// The origin of the document whose tool answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_origin: Option<String>,
    /// The called tool declared itself consequential.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub consequential: bool,
    /// What scrolled (`scrolled to 0,1440`, `scrolled e5 to 0,300`), or why nothing did
    /// (`already at the end`, `the page prevented scrolling`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scrolled: Vec<String>,
    /// The form was not submitted: its invalid fields, in tree order (at most 5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalid: Vec<InvalidField>,
    /// Invalid fields beyond those listed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub invalid_more: usize,
    /// The agent's click opened a file chooser (on this ref); `upload REF NAME` fills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_chooser: Option<String>,
    /// What a coordinate action reached: the element's line (`button e12 "Pay now"`), marked
    /// third-party when it lies in another origin's frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// What `eval` returned, as text: page data, cut at 50,000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Console messages (level, text) the main document logged during the action (at most 200,
    /// each cut at 500 characters).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console: Vec<(String, String)>,
    /// `type "…"` found nothing editable focused: the keys went to the page.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unfocused: bool,
    /// What the action put on the session clipboard (a copy, a page's Copy button), cut at
    /// 2,000 characters: page data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied: Option<String>,
    /// What a drag did (HTML drag and drop, dropped or not; or the data of an intercepted
    /// drag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drag: Option<pageon_js::Dragged>,
    /// Dialogs beyond the 20 listed (answered by the dialog policy all the same).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dialogs_more: usize,
    /// Popups beyond the 20 listed (not opened).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub popups_more: usize,
    /// Downloads beyond the 20 listed (dropped).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub downloads_more: usize,
}

impl Outcome {
    /// Nothing the action did shows in the outcome itself.
    fn quiet(&self) -> bool {
        self.navigated.is_none()
            && self.dialogs.is_empty()
            && self.popups.is_empty()
            && self.downloads.is_empty()
            && self.errors.is_empty()
            && self.requests.is_empty()
            && self.refusal.is_none()
            && self.tool_result.is_none()
            && self.scrolled.is_empty()
            && self.invalid.is_empty()
            && self.file_chooser.is_none()
            && self.value.is_none()
            && self.copied.is_none()
            && self.drag.is_none()
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// A field whose validation blocked a submission.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InvalidField {
    /// The agent's ref for the field, if it has one.
    pub target: Option<String>,
    /// Its accessible name.
    pub name: String,
    /// Why (the validation message), clipped to 300 characters.
    pub message: String,
    /// The page set the message (`setCustomValidity`, a component's `setValidity`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub page_data: bool,
}

/// Most invalid fields an outcome lists.
const MAX_INVALID: usize = 5;

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let q = |s: &str| pageon_agent::quote::quote(&pageon_agent::quote::clip(s, 300));
        let mut parts = vec![match (self.status, self.due_ms) {
            (Status::Unknown, Some(ms)) => format!(
                "unknown {} · nothing changed yet; the page set a timer due in {ms} ms (browser_wait {ms})",
                self.action
            ),
            _ => format!("ok {}", self.action),
        }];
        parts.extend(self.scrolled.iter().cloned());
        if let Some(u) = &self.navigated {
            parts.push(if self.same_document {
                format!("navigated to {} (same page)", q(u.as_str()))
            } else {
                format!("navigated to {}", q(u.as_str()))
            });
        }
        for d in &self.dialogs {
            parts.push(if d.kind == "alert" {
                format!("alert {:?}", d.message)
            } else {
                format!(
                    "dialog {} {:?} → {}",
                    d.kind,
                    d.message,
                    if d.accepted { "accepted" } else { "dismissed" }
                )
            });
        }
        if !self.invalid.is_empty() {
            let fields: Vec<String> = self
                .invalid
                .iter()
                .map(|f| {
                    let who = match &f.target {
                        Some(r) => format!("{r} {}", q(&f.name)),
                        None => q(&f.name),
                    };
                    let why = f.message.replace('\n', " — ");
                    if f.page_data {
                        format!("{who} (page data) {}", q(&why))
                    } else {
                        format!("{who} ({why})")
                    }
                })
                .collect();
            let mut line = format!("not submitted: {}", fields.join(", "));
            if self.invalid_more > 0 {
                line.push_str(&format!(" and {} more", self.invalid_more));
            }
            parts.push(line);
        }
        if let Some(r) = &self.file_chooser {
            parts.push(format!("file chooser for {r} (upload {r} NAME)"));
        }
        if let Some(d) = &self.drag {
            parts.push(
                match (d.intercepted.is_some(), d.html, d.dropped) {
                    (true, _, _) => "drag intercepted",
                    (_, true, true) => "dragged (dropped)",
                    (_, true, false) => "dragged (not accepted: no drop)",
                    _ => "pointer drag",
                }
                .into(),
            );
        }
        if self.dialogs_more > 0 {
            parts.push(format!("and {} more dialogs", self.dialogs_more));
        }
        for p in &self.popups {
            parts.push(format!("popup {}", q(p)));
        }
        if self.popups_more > 0 {
            parts.push(format!("and {} more popups", self.popups_more));
        }
        for d in &self.downloads {
            parts.push(format!(
                "{} {} ({}, {} B)",
                if d.handle.is_empty() {
                    "download"
                } else {
                    &d.handle
                },
                q(&d.filename),
                q(&d.content_type),
                d.bytes.len()
            ));
        }
        if self.downloads_more > 0 {
            parts.push(format!("and {} more downloads", self.downloads_more));
        }
        if let Some(e) = self.errors.first() {
            parts.push(match self.errors.len() {
                1 => format!("page error (page data) {}", q(&e.message)),
                n => format!("{n} page errors, first (page data) {}", q(&e.message)),
            });
        }
        // Frames a page sent over WebSockets are writes of their own: counted apart.
        let (frames, requests): (Vec<_>, Vec<_>) =
            self.requests.iter().partition(|r| r.method == "SEND");
        if !requests.is_empty() {
            let blocked = requests
                .iter()
                .filter(|r| !matches!(r.outcome, crate::RequestOutcome::Ok))
                .count();
            let mut r = format!("{} request(s)", requests.len());
            if blocked > 0 {
                r.push_str(&format!(" ({blocked} blocked or failed)"));
            }
            parts.push(r);
        }
        if !frames.is_empty() {
            let refused = frames
                .iter()
                .filter(|r| !matches!(r.outcome, crate::RequestOutcome::Ok))
                .count();
            let mut r = format!("{} frame(s) sent", frames.len() - refused);
            if refused > 0 {
                r.push_str(&format!(" ({refused} refused)"));
            }
            parts.push(r);
        }
        if self.consequential {
            parts.push("consequential tool".into());
        }
        if let Some(c) = &self.copied {
            parts.push(format!(
                "copied (page data) {}",
                pageon_agent::quote::quote(c)
            ));
        }
        if let Some(r) = &self.tool_result {
            let from = self
                .tool_origin
                .as_deref()
                .map(|o| format!(" from {}", q(o)))
                .unwrap_or_default();
            parts.push(format!(
                "result{from} (page data) {}",
                pageon_agent::quote::quote(r)
            ));
        }
        if let Some(r) = &self.refusal {
            parts.push(format!("refused: {r}"));
        }
        if self.status == Status::Unknown {
            return f.write_str(&parts.join(" · "));
        }
        parts.push(match self.settled.pending_timers {
            0 => format!("settled idle after {} ms", self.settled.virtual_ms),
            n => format!(
                "settled after {} ms, {n} timer(s) pending",
                self.settled.virtual_ms
            ),
        });
        f.write_str(&parts.join(" · "))
    }
}

/// Why an element cannot be acted on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Reason {
    /// Not rendered, or not visible.
    Hidden,
    /// A disabled form control.
    Disabled,
    /// Another element is on top (its ref, if it has one).
    Covered {
        /// The covering element's ref.
        by: Option<String>,
    },
    /// Outside the open modal dialog.
    Inert,
    /// Not a text field, textarea or editable element (for `type`).
    NotEditable,
    /// Not a file input, and activating it opened no file chooser (for `upload`).
    NotFileInput,
    /// A file input without `multiple`, given several files.
    OneFile,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hidden => f.write_str("hidden"),
            Self::Disabled => f.write_str("disabled"),
            Self::Covered { by: Some(b) } => write!(f, "covered by {b}"),
            Self::Covered { by: None } => f.write_str("covered by another element"),
            Self::Inert => f.write_str("inert (outside the open modal dialog)"),
            Self::NotEditable => f.write_str("not editable"),
            Self::NotFileInput => {
                f.write_str("not a file input (and clicking it opened no file chooser)")
            }
            Self::OneFile => f.write_str("it takes one file (no `multiple`)"),
        }
    }
}

/// Why an action could not be done. The page is unchanged, except for [`Self::Navigation`]
/// (the action ran, the navigation it started failed; the tab keeps the old page).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ActionError {
    /// Not a ref from this tab's observations.
    UnknownRef(String),
    /// The element is gone.
    StaleRef(String),
    /// The element is there but cannot take this action.
    NotActionable {
        /// The ref.
        target: String,
        /// Why.
        reason: Reason,
    },
    /// No option matches.
    NoSuchOption {
        /// The ref of the `<select>`.
        target: String,
        /// The values that matched nothing.
        missing: Vec<String>,
        /// The options' labels.
        available: Vec<String>,
    },
    /// The text to select is not in the element.
    NoSuchText {
        /// The element's ref.
        target: String,
        /// The text asked for.
        text: String,
        /// How the element's text starts.
        starts: String,
    },
    /// The navigation the action started failed.
    Navigation(LoadError),
    /// Not supported.
    Unsupported(String),
    /// A `secret://` handle that is unknown, or not for the target's origin.
    Secret(String),
    /// A file that is not staged, not for the target's origin, or cannot be read.
    File(String),
    /// The content process failed (crashed or broke the protocol).
    Engine(String),
    /// A page tool could not be called, or failed; page text is quoted.
    Tool(String),
    /// `eval` threw or its promise was rejected (the message is page data).
    Script(String),
}

impl ActionError {
    /// What to do next.
    pub fn guidance(&self) -> String {
        match self {
            Self::UnknownRef(_) => "use a ref from the latest observation (observe first)".into(),
            Self::StaleRef(_) => "the element is gone: observe again and use the new ref".into(),
            Self::NotActionable { reason, .. } => match reason {
                Reason::Hidden => {
                    "reveal it first (open its menu, tab or section) or pick another element"
                        .into()
                }
                Reason::Disabled => {
                    "wait until it is enabled (complete the required fields) or pick another element"
                        .into()
                }
                Reason::Covered { by: Some(b) } => format!("close or act on {b} first"),
                Reason::Covered { by: None } => "close what covers it first".into(),
                Reason::Inert => "act inside the dialog, or close it first".into(),
                Reason::NotEditable => {
                    "type into a text field, a textarea or an editable element".into()
                }
                Reason::NotFileInput => {
                    "upload to the file input, or to the button or label that opens it".into()
                }
                Reason::OneFile => "upload one file".into(),
            },
            Self::NoSuchOption { available, .. } => format!("use one of: {}", available.join(", ")),
            Self::NoSuchText { starts, .. } => format!(
                "select text that is in it (it reads {})",
                pageon_agent::quote::quote(starts)
            ),
            Self::Navigation(e) => e.guidance(),
            Self::Unsupported(_) => "use another action".into(),
            Self::Secret(_) => {
                "bind the secret to this page's origin (Secrets::with, or --secret NAME@ORIGIN with PAGEON_SECRET_<NAME>)"
                    .into()
            }
            Self::File(_) => {
                "stage the file for this page's origin (--file NAME@ORIGIN=PATH, Files::with) and upload it by name"
                    .into()
            }
            Self::Engine(_) => "open the page again (a new content process starts)".into(),
            Self::Tool(_) => {
                "check the tool's name and parameters in the observation, or act on the page instead"
                    .into()
            }
            Self::Script(_) => {
                "fix the expression: it runs in an isolated world over the page's document (its DOM, not its script globals)"
                    .into()
            }
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRef(r) => write!(f, "unknown ref {r}"),
            Self::StaleRef(r) => write!(f, "{r} is gone from the page"),
            Self::NotActionable { target, reason } => {
                write!(f, "{target} is not actionable: {reason}")
            }
            Self::NoSuchOption {
                target, missing, ..
            } => write!(f, "{target} has no option {}", missing.join(", ")),
            Self::NoSuchText { target, text, .. } => {
                write!(
                    f,
                    "{target} has no text {}",
                    pageon_agent::quote::quote(text)
                )
            }
            Self::Navigation(e) => write!(f, "navigation failed: {e}"),
            Self::Unsupported(w) => write!(f, "{w} is not supported"),
            Self::Secret(m) => write!(f, "secret refused: {m}"),
            Self::File(m) => write!(f, "upload refused: {m}"),
            Self::Engine(m) => write!(f, "the content process failed: {m}"),
            Self::Tool(m) => write!(f, "tool failed: {m}"),
            Self::Script(m) => write!(f, "script failed: {m}"),
        }
    }
}

impl std::error::Error for ActionError {}

/// A resolved ref: frame index, node, and the point to click when the centre is covered.
type Resolved = (usize, NodeId, Option<(f64, f64)>);

/// Where a history move goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum History {
    Push,
    Replace,
    /// `back`/`forward`: the index was moved already.
    Keep,
}

/// One entry of a tab's history: a URL of one document, with the page's state.
#[derive(Debug, Clone)]
struct Entry {
    url: String,
    /// `history.state`, serialized.
    state: Option<pageon_js::SavedState>,
    /// The document it belongs to: entries of one document traverse in place.
    doc: u64,
    /// The viewport scroll when the entry was left.
    scroll: (f64, f64),
    /// The page added an entry from here without a user activation: the agent's `back` and
    /// `forward` skip it (Chrome's history-manipulation intervention).
    skippable: bool,
}

impl Entry {
    fn new(url: String, doc: u64) -> Self {
        Self {
            url,
            state: None,
            doc,
            scroll: (0.0, 0.0),
            skippable: false,
        }
    }
}

/// The most history entries a tab keeps (Chrome's figure); the oldest go first.
const MAX_ENTRIES: usize = 50;

/// A live page in a [`Session`]: its documents keep running between actions.
pub struct Tab<'s, E: Env> {
    pub(crate) session: &'s Session<E>,
    options: LoadOptions,
    pub(crate) doc: Document,
    history: Vec<Entry>,
    index: usize,
    /// The current document's number (entries with it traverse in place).
    doc_seq: u64,
    /// History requests being applied (a page answering traversals with traversals is cut).
    history_depth: u32,
    pub(crate) episode: Episode,
    downloads: Vec<Download>,
    opened: Outcome,
    /// Decoded images, once the tab has been painted.
    pub(crate) images: Option<Rc<pageon_layout::Images>>,
    /// Report steps to a recording; off for protocol tabs (CDP).
    pub(crate) record_steps: bool,
    /// Observations since the last step (they assign refs; a re-run repeats them).
    observed: u32,
    /// The page tools the agent last saw, to print them only when they change.
    tools_seen: Option<String>,
    /// The frame where the pointer's button went down (protocol clients' presses).
    pressed_frame: Option<usize>,
    /// The main document's engine and how many console messages the tab has reported from it.
    console_seen: (usize, u64),
    /// A drag that starts is handed to the protocol client instead of running (CDP
    /// `Input.setInterceptDrags`).
    intercept_drags: bool,
}

impl<E: Env> fmt::Debug for Tab<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tab")
            .field("url", &self.doc.final_url)
            .field("history", &self.history)
            .finish_non_exhaustive()
    }
}

/// The path (frame positions) from the top document to frame `idx` (capture order).
fn frame_path(root: &Document, idx: usize) -> Option<Vec<usize>> {
    let mut nodes: Vec<(Option<usize>, usize, Option<&Document>)> = vec![(None, 0, Some(root))];
    let mut i = 0;
    while i < nodes.len() {
        if let Some(d) = nodes[i].2 {
            for (k, fr) in d.frames().iter().enumerate() {
                nodes.push((Some(i), k, fr.document()));
            }
        }
        i += 1;
    }
    let mut path = Vec::new();
    let mut cur = idx;
    loop {
        let (parent, pos, _) = *nodes.get(cur)?;
        match parent {
            Some(p) => {
                path.push(pos);
                cur = p;
            }
            None => break,
        }
    }
    path.reverse();
    Some(path)
}

fn doc_ref<'a>(root: &'a Document, path: &[usize]) -> Option<&'a Document> {
    let mut d = root;
    for &k in path {
        d = d.frames.get(k)?.document.as_deref()?;
    }
    Some(d)
}

/// A frame of a tab in capture order (0: the main document), for protocol clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameMeta {
    /// The parent frame's index.
    pub(crate) parent: Option<usize>,
    /// URL.
    pub(crate) url: String,
    /// The `<iframe>`'s `name`.
    pub(crate) name: String,
    /// The `<iframe>` element in the parent document.
    pub(crate) owner: Option<NodeId>,
    /// Whether it has a document.
    pub(crate) loaded: bool,
}

fn px(layout: &pageon_layout::Layout, id: NodeId, prop: &str) -> f64 {
    layout
        .style(id, prop)
        .and_then(|v| v.trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// The origin of an `<iframe>`'s content box in its document's coordinates.
fn content_origin(doc: &Document, owner: NodeId) -> (f64, f64) {
    let layout = doc.layout(Viewport::default());
    let Some(b) = layout.client_rect(owner) else {
        return (0.0, 0.0);
    };
    (
        f64::from(b.x)
            + px(&layout, owner, "border-left-width")
            + px(&layout, owner, "padding-left"),
        f64::from(b.y) + px(&layout, owner, "border-top-width") + px(&layout, owner, "padding-top"),
    )
}

fn doc_at<'a>(root: &'a mut Document, path: &[usize]) -> Option<&'a mut Document> {
    let mut d = root;
    for &k in path {
        d = d.frames.get_mut(k)?.document.as_deref_mut()?;
    }
    Some(d)
}

/// Copies the live state of every document (DOM, modals, focus, errors) into its snapshot.
pub(crate) fn refresh(doc: &mut Document) {
    if let Some(live) = &doc.live {
        doc.dom = live.dom.borrow().clone();
        let engine = live.engine.borrow();
        if let Some(r) = engine.realm(live.realm) {
            let st = r.st.borrow();
            doc.js.modals.clone_from(&st.modals);
            doc.js.focused = st.focused;
        }
        doc.js.drop_targets = engine.drop_targets(live.realm);
        doc.js.scroll = crate::Scroll::of(&engine, live.realm);
        doc.js.errors = engine.errors_of(live.realm);
        doc.summary.errors = doc.js.errors.len();
    }
    for f in &mut doc.frames {
        if let Some(d) = f.document.as_deref_mut() {
            refresh(d);
        }
    }
}

/// A point of the element's box where the element (or something inside it) is on top: the
/// centre first, then a 5 × 5 grid.
fn visible_point(doc: &Document, node: NodeId) -> Option<(f64, f64)> {
    let layout = doc.layout(Viewport::default());
    let b = layout.client_rect(node)?;
    let inside = |h: NodeId| h == node || doc.dom.flat_ancestors(h).any(|a| a == node);
    let steps = [0.5, 0.1, 0.3, 0.7, 0.9];
    for fy in steps {
        for fx in steps {
            let x = b.x + b.width * fx;
            let y = b.y + b.height * fy;
            if layout.element_at(x, y).is_some_and(inside) {
                return Some((f64::from(x), f64::from(y)));
            }
        }
    }
    None
}

fn is_form_control(doc: &Document, id: NodeId) -> bool {
    doc.dom.get(id).is_some_and(|n| {
        matches!(
            n.local_name(),
            Some("button" | "input" | "select" | "textarea" | "option" | "optgroup" | "fieldset")
        )
    })
}

/// A form control disabled by its own attribute or a disabled `<fieldset>` ancestor.
fn disabled(doc: &Document, id: NodeId) -> bool {
    if pageon_dom::forms::is_form_associated(&doc.dom, id) {
        return pageon_dom::forms::disabled(&doc.dom, id);
    }
    if !is_form_control(doc, id) {
        return false;
    }
    let dom = &doc.dom;
    dom.get(id).is_some_and(|n| n.attr("disabled").is_some())
        || dom.ancestors(id).any(|a| {
            dom.get(a)
                .is_some_and(|n| n.is_html("fieldset") && n.attr("disabled").is_some())
        })
}

impl<E: Env> Drop for Tab<'_, E> {
    fn drop(&mut self) {
        self.close_streams();
    }
}

impl<'s, E: Env> Tab<'s, E> {
    /// Hands drags that start to the protocol client instead of running them (CDP
    /// `Input.setInterceptDrags`).
    pub(crate) fn set_intercept_drags(&mut self, on: bool) {
        self.intercept_drags = on;
    }

    /// Closes the realtime connections of the page's documents (it is going away).
    fn close_streams(&self) {
        let env = self.session.env();
        if let Some(page) = &self.doc.page {
            for e in page.engines.borrow().values() {
                if let Ok(mut e) = e.try_borrow_mut()
                    && e.has_streams()
                {
                    e.close_streams(env);
                }
            }
        } else if let Some(live) = &self.doc.live
            && let Ok(mut e) = live.engine.try_borrow_mut()
            && e.has_streams()
        {
            e.close_streams(env);
        }
    }

    pub(crate) fn open(
        session: &'s Session<E>,
        input: &str,
        options: LoadOptions,
        record_steps: bool,
    ) -> Result<Self, LoadError> {
        let net = session.net();
        {
            let mut n = net.borrow_mut();
            n.allow_private = options.allow_private_network;
            n.robots = options.robots;
            n.prefer_markdown = options.prefer_markdown;
            n.take_log();
        }
        let start = Start {
            keep: true,
            ..Start::default()
        };
        let loaded = crate::load::open(input, session.env(), net, options.clone(), start);
        let requests = net.borrow_mut().take_log();
        let mut doc = match loaded? {
            Loaded::Page(d) => *d,
            Loaded::Download(d) => return Err(LoadError::UnsupportedContentType(d.content_type)),
        };
        session.seen(doc.loaded_at);
        doc.summary.requests = requests.len();
        doc.summary.blocked = requests
            .iter()
            .filter(|e| matches!(e.outcome, crate::RequestOutcome::Blocked(_)))
            .count();
        let opened = Outcome {
            action: format!("open {input}"),
            navigated: Some(doc.final_url.clone()),
            dialogs: std::mem::take(&mut doc.js.dialogs),
            popups: std::mem::take(&mut doc.js.popups),
            downloads: Vec::new(),
            errors: doc.js.errors.clone(),
            requests: requests.clone(),
            settled: Settle {
                virtual_ms: doc.summary.virtual_ms,
                pending_timers: doc.summary.pending_timers,
            },
            refusal: doc.refusal.clone(),
            ..Outcome::default()
        };
        doc.requests = requests;
        let tab_options = options.clone();
        let tab = Self {
            session,
            options,
            history: vec![Entry::new(doc.final_url.clone(), 1)],
            index: 0,
            doc_seq: 1,
            history_depth: 0,
            doc,
            episode: Episode::new(Budget::o200k(4000), Mode::Diff),
            downloads: Vec::new(),
            opened,
            images: None,
            tools_seen: None,
            record_steps,
            observed: 0,
            pressed_frame: None,
            console_seen: (0, 0),
            intercept_drags: false,
        };
        let mut tab = tab;
        // History changes the page made while loading (a router's `replaceState`).
        let mut load_out = Outcome::default();
        tab.apply_history(&mut load_out);
        tab.record_step(
            format!("open {input}"),
            serde_json::json!({"open": {"url": input, "options": &tab_options}}),
            None,
        );
        let mut opened = std::mem::take(&mut tab.opened);
        tab.redact_outcome(&mut opened);
        tab.opened = opened;
        Ok(tab)
    }

    /// The state hash: FNV-1a 64 of the URL, the HTML of the document and its
    /// frames, the agent view (control values) and storage, with secrets redacted.
    pub fn state_hash(&self) -> String {
        self.state().0
    }

    fn state(&self) -> (String, String) {
        let view = self.view(
            crate::Dump::Ax,
            Viewport::default(),
            Budget::o200k(1_000_000),
        );
        let mut text = format!("{}\n", self.doc.final_url);
        for d in crate::capture::frame_documents(&self.doc)
            .into_iter()
            .flatten()
        {
            text.push_str(&d.dump(crate::Dump::Html));
            text.push('\n');
        }
        text.push_str(&view);
        text.push('\n');
        text.push_str(&serde_json::to_string(&self.session.storage_state()).unwrap_or_default());
        (pageon_env::fnv64(&self.redact(text)), view)
    }

    /// Reports the step that just ended to the session's recording, if it keeps steps.
    fn record_step(
        &mut self,
        action: String,
        input: serde_json::Value,
        target: Option<pageon_env::Target>,
    ) {
        let observed = std::mem::take(&mut self.observed);
        if !self.record_steps {
            return;
        }
        let Some(rec) = self.session.env().recorder() else {
            return;
        };
        let (hash, view) = self.state();
        rec.step(pageon_env::Step {
            action: self.redact(action),
            input,
            at: pageon_env::Cursor::default(),
            observed,
            hash,
            view,
            target,
            checks: Vec::new(),
        });
    }

    /// Replaces every secret value typed in this session by its handle, and the value of
    /// every credential URL parameter (`?token=…`) by `[redacted]`.
    pub(crate) fn redact(&self, mut s: String) -> String {
        for (value, handle) in self.session.secrets().iter() {
            if !value.is_empty() && s.contains(value.as_str()) {
                s = s.replace(value.as_str(), handle);
            }
        }
        match pageon_env::redact_params(&s) {
            std::borrow::Cow::Borrowed(_) => s,
            std::borrow::Cow::Owned(r) => r,
        }
    }

    /// The console messages the main document logged since the tab last reported (at most
    /// 200, each cut at 500 characters).
    fn collect_console(&mut self, out: &mut Outcome) {
        let Ok(live) = self.live(0) else { return };
        let id = Rc::as_ptr(&live.engine) as usize;
        let seen = if self.console_seen.0 == id {
            self.console_seen.1
        } else {
            // A new engine (another document): only what it logged since this action began
            // is new, which a fresh count cannot tell; report what it holds.
            0
        };
        let (lines, total) = live.engine.borrow().console_since(seen);
        self.console_seen = (id, total);
        let skip = lines.len().saturating_sub(200);
        out.console = lines
            .into_iter()
            .skip(skip)
            .map(|(level, text)| (level, cut_chars(&text, 500)))
            .collect();
    }

    /// Gives every started engine of the tab the session clipboard.
    fn push_clipboard(&self) {
        let clip = self.session.clipboard();
        for frame in 0..self.frames_meta().len() {
            if let Some(live) = self.live_if_started(frame) {
                live.engine.borrow_mut().set_clipboard(clip.clone());
            }
        }
    }

    /// Takes what the action put on the clipboard: kept for the session (never a secret's
    /// value) and reported, cut.
    fn pull_clipboard(&self, out: &mut Outcome) {
        let mut changed = None;
        for frame in 0..self.frames_meta().len() {
            if let Some(live) = self.live_if_started(frame)
                && let Some(c) = live.engine.borrow_mut().take_clipboard()
            {
                changed = Some(c);
            }
        }
        let Some(clip) = changed else { return };
        let secret = clip.as_ref().is_some_and(|c| {
            self.session.secrets().iter().any(|(v, _)| {
                !v.is_empty()
                    && (c.text.contains(v.as_str())
                        || c.html.as_deref().is_some_and(|h| h.contains(v.as_str())))
            })
        });
        if secret {
            return;
        }
        if let Some(c) = &clip {
            out.copied = Some(c.text.chars().take(2000).collect());
        }
        self.session.set_clipboard(clip);
    }

    fn redact_outcome(&self, o: &mut Outcome) {
        o.target = o.target.take().map(|t| self.redact(t));
        for (_, text) in &mut o.console {
            *text = self.redact(std::mem::take(text));
        }
        for d in &mut o.dialogs {
            d.message = self.redact(std::mem::take(&mut d.message));
        }
        for e in &mut o.errors {
            e.message = self.redact(std::mem::take(&mut e.message));
        }
        for r in &mut o.requests {
            r.url = self.redact(std::mem::take(&mut r.url));
            r.final_url = r.final_url.take().map(|u| self.redact(u));
        }
        o.navigated = o.navigated.take().map(|u| self.redact(u));
        o.tool_result = o.tool_result.take().map(|r| self.redact(r));
        o.copied = o.copied.take().map(|c| self.redact(c));
        o.popups = o.popups.drain(..).map(|u| self.redact(u)).collect();
        o.action = self.redact(std::mem::take(&mut o.action));
        for d in &mut o.downloads {
            d.url = self.redact(std::mem::take(&mut d.url));
        }
        for f in &mut o.invalid {
            f.message = self.redact(std::mem::take(&mut f.message));
            f.name = self.redact(std::mem::take(&mut f.name));
        }
    }

    /// A view of the page at a viewport and budget, with secrets redacted.
    pub fn view(&self, kind: crate::Dump, viewport: Viewport, budget: Budget) -> String {
        self.redact(self.doc.view(kind, viewport, budget))
    }

    /// The HTTP status of the page.
    pub fn status(&self) -> u16 {
        self.doc.status()
    }

    /// The page's status line (see [`Document::status_line`]), with secrets redacted.
    pub fn status_line(&self) -> String {
        self.redact(self.doc.status_line())
    }

    /// The init scripts every later document of this tab runs before its own (protocol
    /// clients' `addScriptToEvaluateOnNewDocument` / `addPreloadScript`).
    pub(crate) fn init_scripts_mut(&mut self) -> &mut Vec<crate::load::InitScript> {
        &mut self.options.init_scripts
    }

    /// The page as it is now (every view: `ax`, `markdown`, `boxes`, …).
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The current URL.
    pub fn url(&self) -> &str {
        &self.doc.final_url
    }

    /// The document's title (`<title>` text, whitespace collapsed), with secrets redacted.
    pub fn title(&self) -> String {
        let dom = &self.doc.dom;
        let title = dom
            .traverse(dom.root())
            .find_map(|e| match e {
                pageon_dom::Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("title")) => {
                    Some(n)
                }
                _ => None,
            })
            .map(|t| {
                let mut s = String::new();
                for c in dom.children(t) {
                    if let Some(pageon_dom::NodeData::Text(x)) = dom.get(c).map(|n| &n.data) {
                        s.push_str(x);
                    }
                }
                s.split_whitespace().collect::<Vec<_>>().join(" ")
            })
            .unwrap_or_default();
        self.redact(title)
    }

    /// The history of this tab, and the index of the current entry.
    pub fn history(&self) -> (Vec<String>, usize) {
        (
            self.history.iter().map(|e| e.url.clone()).collect(),
            self.index,
        )
    }

    /// Every download so far.
    pub fn downloads(&self) -> &[Download] {
        &self.downloads
    }

    /// What opening the tab did (dialogs, popups, errors and requests while loading).
    pub fn opened(&self) -> &Outcome {
        &self.opened
    }

    /// Sets the observation budget and mode (starts a new episode: refs are reissued).
    pub fn set_view(&mut self, budget: Budget, mode: Mode) {
        self.episode = Episode::new(budget, mode);
    }

    /// How much observations show from now on (refs stay; a change sends a complete view).
    pub fn set_detail(&mut self, detail: pageon_agent::Detail) {
        self.episode.set_detail(detail);
    }

    /// The observation budget from now on (refs stay; a change sends a complete view).
    pub fn set_budget(&mut self, budget: Budget) {
        self.episode.set_budget(budget);
    }

    /// The complete agent view (refs kept), as on the first observation.
    pub fn observe_full(&mut self) -> String {
        self.episode.reset_view();
        self.tools_seen = None;
        self.observe()
    }

    /// The agent view (`ax`), with refs that stay stable across actions: complete the first
    /// time and after navigation, then a diff. The outcomes of actions since the last
    /// observation come first.
    pub fn observe(&mut self) -> String {
        self.observed += 1;
        let capture = self.doc.capture(Viewport::default());
        let mut text = self.episode.observe(&capture);
        // Declared tools rank above elements: listed when the set is new or changed.
        let tools = self.tool_lines();
        if self.tools_seen.as_deref() != Some(tools.as_str()) {
            let shown = if tools.is_empty() {
                (self.tools_seen.is_some())
                    .then(|| "tools: none (the page unregistered them)".to_string())
            } else {
                Some(tools.clone())
            };
            self.tools_seen = Some(tools);
            if let Some(lines) = shown {
                text = format!("{lines}\n{text}");
            }
        }
        // A refusal comes first: the agent learns why before reading the page.
        if let Some(r) = &self.doc.refusal {
            text = format!("refused: {r}\n  {}\n{text}", r.guidance());
        }
        self.redact(text)
    }

    /// BrowserGym's observation objects of the page: the AX tree and DOM snapshot in
    /// CDP's formats with the agent's refs as `bid`s, the extra element properties, the focused
    /// bid, and `pageon_ax` (the full view). Refs are the ones [`Tab::act`] accepts; the agent's
    /// next diff is unaffected; secrets are redacted. Nothing is written into the page.
    pub fn browsergym(&mut self) -> serde_json::Value {
        let capture = self.doc.capture(Viewport::default());
        let mut v = pageon_agent::browsergym(&mut self.episode, &capture);
        v["pageon_ax"] = serde_json::json!(self.episode.full_view(&capture));
        if self.session.secrets().is_empty() {
            return v;
        }
        serde_json::from_str(&self.redact(v.to_string())).unwrap_or(v)
    }

    /// Performs an action: it settles, follows the navigation it started, and reports what
    /// happened, or why it could not be done.
    pub fn act(&mut self, action: Action) -> Result<Outcome, ActionError> {
        let window = self.options.settle_ms;
        self.act_within(action, window)
    }

    /// [`act`](Self::act), settling within `window_ms` of virtual time instead of the load
    /// options' window (protocol input: a keystroke is not an agent's step).
    pub(crate) fn act_within(
        &mut self,
        action: Action,
        window_ms: u64,
    ) -> Result<Outcome, ActionError> {
        let saved = self.options.settle_ms;
        self.options.settle_ms = window_ms;
        let r = self.act_inner(action, false);
        self.options.settle_ms = saved;
        r
    }

    /// Acts for a person (takeover input): the page gets the same trusted input, but it is not
    /// the agent's step. Nothing is recorded as a step, the next observation reports no outcome
    /// for it, and its requests carry no keystrokes as their cause.
    pub fn act_as_person(&mut self, action: Action) -> Result<Outcome, ActionError> {
        self.act_inner(action, true)
    }

    /// The action's target as the agent last saw it: its view line and its role,
    /// name and index among equal ones.
    fn target_of(&self, action: &Action) -> Option<(String, pageon_env::Target)> {
        let (Action::Click(r)
        | Action::Hover(r)
        | Action::Drag(r, _)
        | Action::Type(r, _)
        | Action::Select(r, _)
        | Action::Paste(Some(r), _)
        | Action::Upload(r, _)) = action
        else {
            return None;
        };
        let lines = self.episode.last_lines()?;
        let line = lines.iter().find(|l| &l.key == r)?;
        let target = crate::workflow::target_in(lines.iter().map(|l| l.text.as_str()), r)?;
        Some((self.redact(line.text.trim().to_string()), target))
    }

    fn act_inner(&mut self, action: Action, person: bool) -> Result<Outcome, ActionError> {
        let target = if person {
            None
        } else {
            self.target_of(&action)
        };
        let net = self.session.net().clone();
        {
            let mut n = net.borrow_mut();
            n.allow_private = self.options.allow_private_network;
            n.robots = self.options.robots;
            n.prefer_markdown = self.options.prefer_markdown;
            n.take_log();
            // What every request this action causes carries to the broker's guard:
            // the action, and its target as the agent last saw it.
            n.cause = Some(pageon_env::Cause {
                action: if person {
                    "person input (takeover)".into()
                } else {
                    self.redact(action.to_string())
                },
                target: target.as_ref().map(|(line, _)| line.clone()),
                target_untrusted: false,
            });
        }
        let mut out = Outcome {
            action: action.to_string(),
            ..Outcome::default()
        };
        let top = self.doc.js.scroll.viewport;
        let engines = self.engines();
        for e in &engines {
            e.borrow_mut().begin_action();
        }
        let changes = |es: &[Rc<std::cell::RefCell<pageon_js::Engine>>]| -> u64 {
            es.iter().map(|e| e.borrow().dom_changes()).sum()
        };
        let changed_before = changes(&engines);
        self.push_clipboard();
        let result = self.run(&action, &mut out);
        self.pull_clipboard(&mut out);
        // Any scroll the action caused (keys, in-page links, the page's own scripts), unless
        // the action reported it already or loaded another page.
        let now = self.doc.js.scroll.viewport;
        let loaded = out.navigated.is_some() && !out.same_document;
        if now != top && !loaded && !out.scrolled.iter().any(|s| s.starts_with("scrolled to")) {
            out.scrolled
                .push(format!("scrolled to {},{}", now.0.round(), now.1.round()));
        }
        net.borrow_mut().cause = None;
        if !person && matches!(result, Ok(()) | Err(ActionError::Navigation(_))) {
            self.record_step(
                action.to_string(),
                serde_json::to_value(&action).unwrap_or_default(),
                target.map(|(_, t)| t),
            );
        }
        let mut requests = net.borrow_mut().take_log();
        out.requests.append(&mut requests);
        // Nothing visible yet, but the page is still working on the action: say so.
        if !person && result.is_ok() && out.quiet() && changes(&engines) == changed_before {
            out.due_ms = engines
                .iter()
                .filter_map(|e| e.borrow().caused_due_ms())
                .min()
                .filter(|ms| *ms <= UNKNOWN_HORIZON_MS);
            if out.due_ms.is_some() {
                out.status = Status::Unknown;
            }
        }
        self.collect_console(&mut out);
        self.redact_outcome(&mut out);
        self.session.seen(self.doc.loaded_at.max(self.now()));
        // A page's tool can echo what was typed into it.
        let result = result.map_err(|e| match e {
            ActionError::Tool(m) => ActionError::Tool(self.redact(m)),
            e => e,
        });
        match result {
            Ok(()) => {
                if !person {
                    self.episode.note(out.to_string());
                }
                Ok(out)
            }
            Err(e) => {
                if !person {
                    self.episode.note(format!("err {action}: {e}"));
                }
                Err(e)
            }
        }
    }

    /// The frames in capture order (0: the main document; frames that failed to load too).
    pub(crate) fn frames_meta(&self) -> Vec<FrameMeta> {
        let mut out = vec![FrameMeta {
            parent: None,
            url: self.doc.final_url.clone(),
            name: String::new(),
            owner: None,
            loaded: true,
        }];
        let mut docs: Vec<Option<&Document>> = vec![Some(&self.doc)];
        let mut i = 0;
        while i < docs.len() {
            if let Some(d) = docs[i] {
                for fr in d.frames() {
                    let name = d
                        .dom
                        .get(fr.owner)
                        .and_then(|n| n.attr("name"))
                        .unwrap_or_default();
                    out.push(FrameMeta {
                        parent: Some(i),
                        url: fr.url().to_string(),
                        name: name.to_string(),
                        owner: Some(fr.owner),
                        loaded: fr.document().is_some(),
                    });
                    docs.push(fr.document());
                }
            }
            i += 1;
        }
        out
    }

    /// The frame (capture order) whose document has context `context` in the page's tree.
    pub(crate) fn frame_of_context(&self, context: u32) -> Option<usize> {
        (0..self.frames_meta().len())
            .find(|&i| self.frame_document(i).is_some_and(|d| d.context == context))
    }

    /// The document of frame `frame`.
    pub(crate) fn frame_document(&self, frame: usize) -> Option<&Document> {
        frame_path(&self.doc, frame).and_then(|p| doc_ref(&self.doc, &p))
    }

    /// The origin of frame `frame`'s content in top-level viewport coordinates.
    pub(crate) fn frame_offset(&self, frame: usize) -> (f64, f64) {
        let metas = self.frames_meta();
        let (mut x, mut y) = (0.0, 0.0);
        let mut cur = frame;
        while let Some(m) = metas.get(cur)
            && let (Some(parent), Some(owner)) = (m.parent, m.owner)
        {
            if let Some(d) = self.frame_document(parent) {
                let (ox, oy) = content_origin(d, owner);
                x += ox;
                y += oy;
            }
            cur = parent;
        }
        (x, y)
    }

    /// Where frame `frame`'s viewport coordinates are in the top-level document (what paint
    /// works in): [`frame_offset`](Self::frame_offset) plus the top-level scroll.
    pub(crate) fn frame_document_offset(&self, frame: usize) -> (f64, f64) {
        let (x, y) = self.frame_offset(frame);
        let (sx, sy) = self.doc.js.scroll.viewport;
        (x + sx, y + sy)
    }

    /// The element at a top-level viewport point, through iframes: its frame, the element
    /// (nothing hit: the root element, as browsers do) and the point in that frame.
    fn hit(&self, x: f64, y: f64) -> (usize, Option<NodeId>, f64, f64) {
        let metas = self.frames_meta();
        let (mut frame, mut x, mut y) = (0, x, y);
        loop {
            let Some(doc) = self.frame_document(frame) else {
                return (frame, None, x, y);
            };
            let hit = doc
                .layout(Viewport::default())
                .element_at(x as f32, y as f32)
                .or_else(|| {
                    let dom = &doc.dom;
                    dom.children(dom.root())
                        .find(|&c| dom.get(c).is_some_and(|n| n.local_name().is_some()))
                });
            let Some(node) = hit else {
                return (frame, None, x, y);
            };
            match metas
                .iter()
                .position(|m| m.parent == Some(frame) && m.owner == Some(node) && m.loaded)
            {
                Some(child) => {
                    let (ox, oy) = content_origin(doc, node);
                    x -= ox;
                    y -= oy;
                    frame = child;
                }
                None => return (frame, Some(node), x, y),
            }
        }
    }

    /// The frame whose focused element took focus last (0 when none holds it).
    fn focused_frame(&self) -> usize {
        (0..self.frames_meta().len())
            .filter_map(|i| {
                let live = self.frame_document(i)?.live.as_ref()?;
                let seq = live.engine.borrow().focus_seq(live.realm);
                (seq > 0).then_some((seq, i))
            })
            .max()
            .map_or(0, |(_, i)| i)
    }

    /// The live state of frame `frame`, if its engine runs.
    pub(crate) fn live_if_started(&self, frame: usize) -> Option<Rc<LiveDoc>> {
        self.frame_document(frame)?.live.clone()
    }

    /// The page's running engines (one per origin).
    fn engines(&self) -> Vec<Rc<std::cell::RefCell<pageon_js::Engine>>> {
        let mut out: Vec<_> = self
            .doc
            .page
            .as_ref()
            .map(|p| p.engines.borrow().values().cloned().collect())
            .unwrap_or_default();
        if let Some(l) = &self.doc.live
            && !out.iter().any(|e| Rc::ptr_eq(e, &l.engine))
        {
            out.push(l.engine.clone());
        }
        out
    }

    /// The live state of frame `frame` (its engine started if needed).
    pub(crate) fn live_in(&mut self, frame: usize) -> Result<Rc<LiveDoc>, ActionError> {
        self.live(frame)
    }

    /// Runs `f` on frame `frame`'s engine (started if needed), then runs what is due now
    /// (microtasks, requests, timers due at this instant) and follows any navigation, as an
    /// action does but without letting time pass (a script call takes no virtual time; for
    /// protocol clients running page script).
    pub(crate) fn script_in<T>(
        &mut self,
        frame: usize,
        label: &str,
        f: impl FnOnce(&mut pageon_js::Engine, usize) -> T,
    ) -> Result<(T, Outcome), ActionError> {
        let net = self.session.net().clone();
        {
            let mut n = net.borrow_mut();
            n.allow_private = self.options.allow_private_network;
            n.robots = self.options.robots;
            n.prefer_markdown = self.options.prefer_markdown;
            n.take_log();
        }
        let mut out = Outcome {
            action: label.to_string(),
            ..Outcome::default()
        };
        let live = self.live(frame)?;
        let errors = self.frame_errors(frame);
        let value = {
            let mut engine = live.engine.borrow_mut();
            f(&mut engine, live.realm)
        };
        let result = self.after_within(frame, &live, errors, &mut out, 0.0);
        out.requests.append(&mut net.borrow_mut().take_log());
        self.redact_outcome(&mut out);
        self.session.seen(self.doc.loaded_at.max(self.now()));
        result.map(|()| (value, out))
    }

    fn now(&self) -> u64 {
        self.doc
            .live
            .as_ref()
            .map_or(0, |l| l.engine.borrow().now_ms())
    }

    fn run(&mut self, action: &Action, out: &mut Outcome) -> Result<(), ActionError> {
        match action {
            Action::Goto(handle) if handle.starts_with("download:") => {
                let d = self
                    .downloads
                    .iter()
                    .find(|d| d.handle == *handle)
                    .cloned()
                    .ok_or_else(|| {
                        let have: Vec<&str> =
                            self.downloads.iter().map(|d| d.handle.as_str()).collect();
                        ActionError::Unsupported(if have.is_empty() {
                            format!("no {handle}: this tab has no downloads")
                        } else {
                            format!("no {handle}: this tab has {}", have.join(", "))
                        })
                    })?;
                let nav = NavRequest {
                    url: d.url.clone(),
                    method: "GET".into(),
                    body: Vec::new(),
                    content_type: None,
                    replace: false,
                };
                let before = self.downloads.len();
                self.navigate_with(None, &nav, History::Push, out, Some(&d))?;
                if self.downloads.len() > before {
                    // Not a type pageon reads: it stays a download (dropped again here).
                    self.downloads.truncate(before);
                    out.downloads.clear();
                    return Err(ActionError::Unsupported(format!(
                        "{handle} ({}) cannot be read as a page; pageon reads HTML, PDF, Markdown and plain text",
                        d.content_type
                    )));
                }
                Ok(())
            }
            Action::Goto(url) => {
                let nav = NavRequest {
                    url: url.clone(),
                    method: "GET".into(),
                    body: Vec::new(),
                    content_type: None,
                    replace: false,
                };
                self.navigate(None, &nav, History::Push, out)
            }
            Action::Back | Action::Forward => {
                let back = *action == Action::Back;
                let step = |i: usize| -> Option<usize> {
                    if back {
                        i.checked_sub(1)
                    } else {
                        Some(i + 1).filter(|&j| j < self.history.len())
                    }
                };
                // Entries the page added without a user activation are skipped, so a page
                // cannot hold the agent (Chrome's history-manipulation intervention).
                let mut next = step(self.index);
                while let Some(i) = next
                    && self.history[i].skippable
                    && let Some(j) = step(i)
                {
                    next = Some(j);
                }
                let Some(i) = next else {
                    return Err(ActionError::Unsupported(format!(
                        "{action} (no {} page in this tab)",
                        if *action == Action::Back {
                            "earlier"
                        } else {
                            "later"
                        }
                    )));
                };
                self.traverse_to(i, true, out)
            }
            Action::Wait(ms) => {
                let live = self.live(0)?;
                let settled = live.engine.borrow_mut().wait(*ms, self.session.env());
                out.settled = Settle {
                    virtual_ms: settled.virtual_ms.max(*ms),
                    pending_timers: settled.pending,
                };
                self.finish(0, &live, out)
            }
            Action::Press(spec) => {
                // Keys go to the frame that holds focus.
                let keys = key_sequence(spec).map_err(ActionError::Unsupported)?;
                let frame = self.focused_frame();
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                {
                    let mut engine = live.engine.borrow_mut();
                    for (mods, key) in &keys {
                        engine.press_chord(live.realm, *mods, key);
                    }
                }
                self.after(frame, &live, errors, out)
            }
            Action::Hold(spec, ms) => {
                let (mods, key) = pageon_js::chord(spec)
                    .ok_or_else(|| ActionError::Unsupported(format!("unknown key {spec:?}")))?;
                let frame = self.focused_frame();
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                live.engine
                    .borrow_mut()
                    .key_hold(live.realm, mods, &key, true, false);
                // Chrome's auto-repeat: after 500 ms, a `keydown` every 33 ms.
                let mut held = 0u64;
                while held < *ms {
                    let step = if held < 500 {
                        (500 - held).min(*ms - held)
                    } else {
                        33.min(*ms - held)
                    };
                    live.engine.borrow_mut().wait(step, self.session.env());
                    held += step;
                    if held >= 500 && held < *ms {
                        live.engine
                            .borrow_mut()
                            .key_hold(live.realm, mods, &key, true, true);
                    }
                }
                live.engine
                    .borrow_mut()
                    .key_hold(live.realm, mods, &key, false, false);
                let r = self.after(frame, &live, errors, out);
                out.settled.virtual_ms += ms;
                r
            }
            Action::Paste(r, text) => {
                let (frame, node) = match r {
                    Some(r) => {
                        let (frame, node, _) = self.resolve(r)?;
                        self.scroll_into_view(frame, node, out)?;
                        let (frame, node, _) = self.resolve(r)?;
                        self.check(frame, node, r, action)?;
                        (frame, Some(node))
                    }
                    None => (self.focused_frame(), None),
                };
                if let (Some(r), Some(n)) = (r, node) {
                    let live = self.live(frame)?;
                    if live.engine.borrow_mut().editable(live.realm, n).is_none() {
                        return Err(ActionError::NotActionable {
                            target: r.clone(),
                            reason: Reason::NotEditable,
                        });
                    }
                }
                self.session.set_clipboard(Some(pageon_js::Clip {
                    text: text.clone(),
                    html: None,
                }));
                self.push_clipboard();
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                let pasted = live.engine.borrow_mut().paste(live.realm, node, text);
                if !pasted {
                    out.unfocused = true;
                }
                self.after(frame, &live, errors, out)
            }
            Action::TypeFocused(text) => {
                let frame = self.focused_frame();
                let value = match text.strip_prefix("secret://") {
                    // The vault's rule as for `type REF`: released for the origin of the frame
                    // that holds focus, and only into an editable element there.
                    Some(name) => {
                        let live = self.live(frame)?;
                        let editable = live.engine.borrow().focused_editable(live.realm);
                        if !editable {
                            return Err(ActionError::Secret(format!(
                                "secret://{name} is typed only into a focused field: nothing editable has focus"
                            )));
                        }
                        let origin = self.frame_origin(frame);
                        let v = self
                            .session
                            .env()
                            .secret(name, &origin)
                            .map_err(|e| ActionError::Secret(e.to_string()))?;
                        self.session.keep_secret(v.clone(), text.clone());
                        v
                    }
                    None => text.clone(),
                };
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                let typed = {
                    let mut engine = live.engine.borrow_mut();
                    match (text.starts_with("secret://"), engine.focused(live.realm)) {
                        (true, Some(f)) => {
                            engine.fill_secret(live.realm, f, &value);
                            true
                        }
                        _ => engine.type_focused(live.realm, &value),
                    }
                };
                if !typed {
                    out.scrolled.clear();
                    out.unfocused = true;
                }
                self.after(frame, &live, errors, out)
            }
            Action::Eval(expr) => self.eval(expr, out),
            Action::HoverAt(x, y) if self.pressed_frame.is_some() => {
                // A move with the button held: a drag step in the frame of the press.
                let (frame, hit, x, y) = self.hit(*x, *y);
                let Some(node) = hit.filter(|_| Some(frame) == self.pressed_frame) else {
                    return Ok(());
                };
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                let intercepted = live.engine.borrow_mut().pointer_move(
                    live.realm,
                    node,
                    (x, y),
                    self.intercept_drags,
                );
                if let Some(items) = intercepted {
                    out.drag = Some(pageon_js::Dragged {
                        html: true,
                        dropped: false,
                        intercepted: Some(items),
                    });
                }
                if !live.engine.borrow().pressed(live.realm) {
                    self.pressed_frame = None;
                }
                self.after(frame, &live, errors, out)
            }
            Action::PointerDown(x, y)
            | Action::PointerUp(x, y)
            | Action::DragEvent { x, y, .. } => {
                let (frame, hit, x, y) = self.hit(*x, *y);
                let frame = match action {
                    Action::PointerDown(..) => frame,
                    _ => self.pressed_frame.unwrap_or(frame),
                };
                let Some(node) = hit else { return Ok(()) };
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                {
                    let mut engine = live.engine.borrow_mut();
                    let dragged = match action {
                        Action::PointerDown(..) => {
                            engine.pointer_down(live.realm, node, (x, y));
                            None
                        }
                        Action::PointerUp(..) => Some(engine.pointer_up(live.realm, node, (x, y))),
                        Action::DragEvent { kind, items, .. } => Some(engine.drag_from_client(
                            live.realm,
                            node,
                            (x, y),
                            *kind,
                            items.clone(),
                        )),
                        _ => None,
                    };
                    out.drag = dragged.filter(|d| d.html);
                    self.pressed_frame = engine.pressed(live.realm).then_some(frame);
                }
                self.after(frame, &live, errors, out)
            }
            Action::Drag(from, to) => {
                // As a person does: bring the element into view, then drag it where the
                // target is.
                let (frame, node, _) = self.resolve(from)?;
                self.scroll_into_view(frame, node, out)?;
                let (frame, node, _) = self.resolve(from)?;
                self.check(frame, node, from, &Action::Hover(from.clone()))?;
                let (tframe, target, _) = self.resolve(to)?;
                if tframe != frame {
                    return Err(ActionError::Unsupported(format!(
                        "dragging {from} into another frame ({to}): drags stay in one document"
                    )));
                }
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                let d = live.engine.borrow_mut().drag(live.realm, node, target);
                out.drag = Some(d);
                self.after(frame, &live, errors, out)
            }
            Action::ClickWith {
                at,
                button,
                count,
                mods,
                nth,
            } => {
                let (frame, node, point) = match at {
                    At::Ref(r) => {
                        let (frame, node, _) = self.resolve(r)?;
                        self.scroll_into_view(frame, node, out)?;
                        let (frame, node, point) = self.resolve(r)?;
                        self.check(frame, node, r, &Action::Click(r.clone()))?;
                        (frame, node, point)
                    }
                    At::Point(x, y) => {
                        out.target = Some(self.describe_at(*x, *y));
                        let (frame, hit, x, y) = self.hit(*x, *y);
                        let Some(node) = hit else { return Ok(()) };
                        (frame, node, Some((x, y)))
                    }
                };
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                live.engine.borrow_mut().click_ex(
                    live.realm,
                    node,
                    point,
                    pageon_js::ClickKind {
                        button: *button,
                        count: *count,
                        mods: *mods,
                        nth: *nth,
                    },
                );
                self.after(frame, &live, errors, out)
            }
            Action::ClickAt(x, y) | Action::HoverAt(x, y) => {
                let (frame, hit, x, y) = self.hit(*x, *y);
                let Some(node) = hit else { return Ok(()) };
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                {
                    let mut engine = live.engine.borrow_mut();
                    if matches!(action, Action::ClickAt(..)) {
                        engine.click(live.realm, node, Some((x, y)));
                    } else {
                        engine.hover(live.realm, node, Some((x, y)));
                    }
                }
                self.after(frame, &live, errors, out)
            }
            Action::Tool {
                name,
                origin,
                input,
            } => self.call_tool(name, origin.as_deref(), input, out),
            Action::Scroll {
                target: Some(r),
                by: ScrollBy::IntoView,
                ..
            } => {
                let (frame, node, _) = self.resolve(r)?;
                if !self.scroll_into_view(frame, node, out)? {
                    out.scrolled.push(format!("{r} is already in view"));
                }
                Ok(())
            }
            Action::Scroll { target, by, point } => {
                let (frame, node, point) = match (target, point) {
                    (Some(r), _) => {
                        let (f, n, _) = self.resolve(r)?;
                        (f, Some(n), None)
                    }
                    // The wheel turns over whatever is at the point, in its frame.
                    (None, Some((x, y))) => {
                        let (f, hit, x, y) = self.hit(*x, *y);
                        (f, hit, Some((x, y)))
                    }
                    (None, None) => (0, None, None),
                };
                let amount = match *by {
                    ScrollBy::Page(d) => {
                        let (x, y) = d.sign();
                        pageon_js::Amount::Pages(x, y)
                    }
                    ScrollBy::Pixels(x, y) => pageon_js::Amount::Pixels(x, y),
                    ScrollBy::IntoView => {
                        return Err(ActionError::Unsupported(
                            "scrolling into view needs a ref: `scroll e12`".into(),
                        ));
                    }
                };
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                let wheeled = live.engine.borrow_mut().wheel(
                    live.realm,
                    point,
                    node.filter(|_| point.is_none()),
                    amount,
                );
                out.scrolled.push(match wheeled {
                    pageon_js::Wheeled::Moved(s, (x, y)) => {
                        let at = format!("{},{}", x.round(), y.round());
                        match (s, target, node) {
                            (pageon_js::Scroller::Element(id), Some(r), Some(n)) if id == n => {
                                format!("scrolled {r} to {at}")
                            }
                            (pageon_js::Scroller::Element(_), ..) => {
                                format!("scrolled a container to {at}")
                            }
                            _ => format!("scrolled to {at}"),
                        }
                    }
                    pageon_js::Wheeled::AtEnd => {
                        let back = matches!(by, ScrollBy::Page(Direction::Up | Direction::Left))
                            || matches!(by, ScrollBy::Pixels(x, y) if *x < 0.0 || *y < 0.0);
                        if back {
                            "already at the start".into()
                        } else {
                            "already at the end".into()
                        }
                    }
                    pageon_js::Wheeled::Prevented => "the page prevented scrolling".into(),
                });
                self.after(frame, &live, errors, out)
            }
            Action::Upload(r, names) => self.upload(r, names, out),
            Action::Click(r) | Action::Hover(r) | Action::Type(r, _) | Action::Select(r, _) => {
                // As a person does: bring it into view first, then act where it is now.
                let (frame, node, _) = self.resolve(r)?;
                self.scroll_into_view(frame, node, out)?;
                let (frame, node, point) = self.resolve(r)?;
                self.check(frame, node, r, action)?;
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                {
                    let mut engine = live.engine.borrow_mut();
                    match action {
                        Action::Click(_) => {
                            engine.click(live.realm, node, point);
                            if engine.take_chooser(live.realm).is_some() {
                                out.file_chooser = Some(r.clone());
                            }
                        }
                        Action::Hover(_) => engine.hover(live.realm, node, point),
                        Action::Type(_, text) => {
                            if engine.editable(live.realm, node).is_none() {
                                return Err(ActionError::NotActionable {
                                    target: r.clone(),
                                    reason: Reason::NotEditable,
                                });
                            }
                            match text.strip_prefix("secret://") {
                                // A secret is filled at once, as a password manager fills it:
                                // no keystrokes, so the page never holds part of it.
                                Some(name) => {
                                    let origin = self.frame_origin(frame);
                                    let v = self
                                        .session
                                        .env()
                                        .secret(name, &origin)
                                        .map_err(|e| ActionError::Secret(e.to_string()))?;
                                    self.session.keep_secret(v.clone(), text.clone());
                                    engine.fill_secret(live.realm, node, &v);
                                }
                                None => engine.type_text(live.realm, node, text),
                            }
                        }
                        Action::Select(_, values) if !engine.is_select(live.realm, node) => {
                            let text = values.join(" ");
                            if !engine.select_text(live.realm, node, &text) {
                                return Err(ActionError::NoSuchText {
                                    target: r.clone(),
                                    starts: engine.text_start(live.realm, node, 60),
                                    text,
                                });
                            }
                        }
                        Action::Select(_, values) => {
                            let missing = engine.select(live.realm, node, values);
                            if !missing.is_empty() {
                                return Err(ActionError::NoSuchOption {
                                    target: r.clone(),
                                    missing,
                                    available: engine.option_labels(live.realm, node),
                                });
                            }
                        }
                        _ => {}
                    }
                }
                self.after(frame, &live, errors, out)
            }
        }
    }

    /// A read with the agent's refs (see [`crate::Read`]), redacted; a ref the episode no
    /// longer knows is a stale or unknown ref.
    pub fn read(&mut self, read: &crate::Read) -> Result<String, ActionError> {
        let capture = self.doc.capture(Viewport::default());
        let text = match read {
            crate::Read::Page {
                filter,
                depth,
                root,
                budget,
            } => self
                .episode
                .read(&capture, *filter, *depth, root.as_deref(), *budget)
                .map_err(|e| {
                    let r = root.clone().unwrap_or_default();
                    match e {
                        RefError::Unknown | RefError::Ambiguous { .. } => {
                            ActionError::UnknownRef(r)
                        }
                        _ => ActionError::StaleRef(r),
                    }
                })?,
            crate::Read::Find { query, limit } => self.episode.find(&capture, query, *limit),
            crate::Read::Line(r) => {
                let line = self
                    .episode
                    .elements(&capture)
                    .into_iter()
                    .find(|e| &e.reference == r)
                    .map(|e| e.line);
                match line {
                    Some(l) => l,
                    None => {
                        self.resolve(r)?;
                        return Err(ActionError::StaleRef(r.clone()));
                    }
                }
            }
            crate::Read::At(x, y) => self.describe_at(*x, *y),
            crate::Read::Markdown(budget) => {
                self.doc
                    .view(crate::Dump::Markdown, Viewport::default(), *budget)
            }
        };
        Ok(self.redact(text))
    }

    /// `eval EXPR`: evaluated in the main document's isolated world `pageon-exec` (its DOM,
    /// cookies and same-origin `fetch`; never the page's script globals), settled like any
    /// action; a promise is awaited within the settle window. The value is page data.
    fn eval(&mut self, expr: &str, out: &mut Outcome) -> Result<(), ActionError> {
        let live = self.live(0)?;
        let errors = self.frame_errors(0);
        let (world, ran) = {
            let mut engine = live.engine.borrow_mut();
            let world = engine
                .world(live.realm, "pageon-exec")
                .map_err(ActionError::Script)?;
            let ran = engine.evaluate(world, expr);
            (world, ran)
        };
        let settled = self.after(0, &live, errors, out);
        let handle = ran.map_err(|t| ActionError::Script(page_text(&t.text)))?;
        settled?;
        if out.navigated.is_some() {
            out.value = Some("undefined".into());
            return Ok(());
        }
        let mut engine = live.engine.borrow_mut();
        let value = match engine.promise_state(world, handle) {
            pageon_js::PromiseState::NotPromise => handle,
            pageon_js::PromiseState::Fulfilled(v) => v,
            pageon_js::PromiseState::Rejected(e) => {
                let why = engine
                    .describe(world, e, false)
                    .ok()
                    .and_then(|d| {
                        d.get("description")
                            .or(d.get("value"))
                            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                    })
                    .unwrap_or_else(|| "rejected".into());
                let why = why.lines().next().unwrap_or_default().to_string();
                return Err(ActionError::Script(format!(
                    "rejected: {}",
                    page_text(&why)
                )));
            }
            pageon_js::PromiseState::Pending => {
                return Err(ActionError::Script(format!(
                    "the promise did not settle within {} ms",
                    self.options.settle_ms
                )));
            }
        };
        let d = engine
            .describe(world, value, true)
            .map_err(ActionError::Script)?;
        let text = match d.get("value") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(v) if d.get("type").and_then(serde_json::Value::as_str) != Some("undefined") => {
                v.to_string()
            }
            _ => d
                .get("description")
                .or(d.get("unserializableValue"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(
                    d.get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("undefined"),
                )
                .to_string(),
        };
        out.value = Some(self.redact(cut_chars(&text, 50_000)));
        Ok(())
    }

    /// What is at a viewport point, as the agent knows it: the line of the element with a ref
    /// that contains what is painted there (`button e12 "Pay now"`), marked when it lies in a
    /// frame of another origin; else the element's tag.
    pub(crate) fn describe_at(&mut self, x: f64, y: f64) -> String {
        let (frame, hit, ..) = self.hit(x, y);
        let Some(node) = hit else {
            return format!("nothing at {x},{y}");
        };
        let capture = self.doc.capture(Viewport::default());
        let elements = self.episode.elements(&capture);
        let owned: Vec<(usize, NodeId, String)> = elements
            .into_iter()
            .filter_map(|e| self.doc.node_for(&e.target).map(|(f, n)| (f, n, e.line)))
            .collect();
        let third = if frame != 0 && self.frame_origin(frame) != self.frame_origin(0) {
            format!(" · third-party frame {}", self.frame_origin(frame))
        } else {
            String::new()
        };
        let Some(doc) = self.frame_document(frame) else {
            return format!("an element at {x},{y}{third}");
        };
        let dom = &doc.dom;
        let mut cur = Some(node);
        while let Some(n) = cur {
            if let Some((_, _, line)) = owned.iter().find(|(f, m, _)| *f == frame && *m == n) {
                return self.redact(format!("{}{third}", line.trim()));
            }
            cur = dom.parent(n);
        }
        let tag = dom
            .get(node)
            .and_then(|n| n.local_name().map(str::to_string))
            .unwrap_or_else(|| "node".into());
        format!("<{tag}> without a ref{third}")
    }

    /// `upload REF NAMES`: a file input is filled directly (it may be hidden, as file inputs
    /// behind styled buttons are); anything else is clicked as a person does and the file
    /// chooser it opens is filled. Files come from the broker (staged, bound to origins) or
    /// from this tab's downloads (to their own origin).
    fn upload(&mut self, r: &str, names: &[String], out: &mut Outcome) -> Result<(), ActionError> {
        let (frame, node, _) = self.resolve(r)?;
        let live = self.live(frame)?;
        let direct = live.engine.borrow().file_input(live.realm, node);
        let errors = self.frame_errors(frame);
        // A drop zone (an element that listens for `drop`): the files are dropped on it.
        if direct.is_none() && live.engine.borrow_mut().drop_zone(live.realm, node) {
            self.scroll_into_view(frame, node, out)?;
            return self.put_files(frame, node, true, r, names, errors, out, true);
        }
        let (target, multiple) = match direct {
            Some(m) => (node, m),
            None => {
                self.scroll_into_view(frame, node, out)?;
                let (frame, node, point) = self.resolve(r)?;
                self.check(frame, node, r, &Action::Click(r.to_string()))?;
                let mut engine = live.engine.borrow_mut();
                engine.click(live.realm, node, point);
                let chooser =
                    engine
                        .take_chooser(live.realm)
                        .ok_or_else(|| ActionError::NotActionable {
                            target: r.to_string(),
                            reason: Reason::NotFileInput,
                        })?;
                let m = engine.file_input(live.realm, chooser).unwrap_or(false);
                (chooser, m)
            }
        };
        self.put_files(frame, target, multiple, r, names, errors, out, false)
    }

    /// Puts files into a file input (`node` in `frame`) whose element the protocol named
    /// (CDP `DOM.setFileInputFiles`, BiDi `input.setFiles`): staged names or paths only.
    pub(crate) fn upload_node(
        &mut self,
        frame: usize,
        node: NodeId,
        names: &[String],
    ) -> Result<Outcome, ActionError> {
        let live = self.live(frame)?;
        let multiple = live
            .engine
            .borrow()
            .file_input(live.realm, node)
            .ok_or_else(|| {
                ActionError::Unsupported(
                    "setting files on an element that is not a file input".into(),
                )
            })?;
        let errors = self.frame_errors(frame);
        let mut out = Outcome::default();
        self.put_files(
            frame,
            node,
            multiple,
            "the element",
            names,
            errors,
            &mut out,
            false,
        )?;
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn put_files(
        &mut self,
        frame: usize,
        target: NodeId,
        multiple: bool,
        r: &str,
        names: &[String],
        errors: usize,
        out: &mut Outcome,
        drop: bool,
    ) -> Result<(), ActionError> {
        let live = self.live(frame)?;
        if names.len() > 1 && !multiple {
            return Err(ActionError::NotActionable {
                target: r.to_string(),
                reason: Reason::OneFile,
            });
        }
        let origin = self.frame_origin(frame);
        let mut files = Vec::new();
        for name in names {
            let (fname, ty, bytes) = if name.starts_with("download:") {
                let d = self
                    .downloads
                    .iter()
                    .find(|d| d.handle == *name)
                    .ok_or_else(|| ActionError::File(format!("no {name} in this tab")))?;
                let from = url::Url::parse(&d.url)
                    .map(|u| u.origin().ascii_serialization())
                    .unwrap_or_default();
                if from != origin {
                    return Err(ActionError::File(format!(
                        "{name} came from {from} and uploads only there (stage it with --file to upload it to {origin})"
                    )));
                }
                (d.filename.clone(), d.content_type.clone(), d.bytes.clone())
            } else {
                let f = self
                    .session
                    .env()
                    .file(name, &origin)
                    .map_err(|e| ActionError::File(e.to_string()))?;
                (f.name, f.content_type, f.bytes)
            };
            let now = self.session.now_ms() as i64;
            files.push((fname, ty, bytes, now));
        }
        if drop {
            let d = live
                .engine
                .borrow_mut()
                .drop_files(live.realm, target, files);
            out.drag = Some(d);
        } else {
            live.engine
                .borrow_mut()
                .set_files(live.realm, target, files);
        }
        self.after(frame, &live, errors, out)
    }

    /// One observation line per page tool: quoted name and description, parameters,
    /// provenance and hints.
    fn tool_lines(&mut self) -> String {
        let q = pageon_agent::quote::quote;
        self.tools()
            .into_iter()
            .map(|(_, t)| {
                let mut line = format!(
                    "tool {} {} ({}) origin {} script {}",
                    q(&t.name),
                    q(&pageon_agent::quote::clip(&t.description, 300)),
                    params(t.input_schema.as_deref()),
                    t.origin,
                    t.script
                );
                for (on, mark) in [
                    (t.third_party, "third-party frame"),
                    (t.consequential, "consequential"),
                    (t.autosubmit, "submits itself"),
                    (t.read_only, "read-only"),
                ] {
                    if on {
                        line.push(' ');
                        line.push_str(mark);
                    }
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The page's tools in every frame (WebMCP): `(frame, tool)`, the top-level
    /// document first.
    pub(crate) fn tools(&mut self) -> Vec<(usize, pageon_js::ToolInfo)> {
        let mut out = Vec::new();
        for frame in 0..self.frames_meta().len() {
            let Ok(live) = self.live(frame) else { continue };
            let tools = live.engine.borrow().tools(live.realm);
            out.extend(tools.into_iter().map(|t| (frame, t)));
        }
        out
    }

    /// Calls a page tool: `execute(input, {signal})`, settled like any action; the result is
    /// JSON text (page data).
    fn call_tool(
        &mut self,
        name: &str,
        origin: Option<&str>,
        input: &serde_json::Value,
        out: &mut Outcome,
    ) -> Result<(), ActionError> {
        if !input.is_object() {
            return Err(ActionError::Tool(format!(
                "the input of {name} must be a JSON object, not {input}"
            )));
        }
        let found = self
            .tools()
            .into_iter()
            .find(|(_, t)| t.name == name && origin.is_none_or(|o| t.origin == o));
        let Some((frame, tool)) = found else {
            return Err(ActionError::Tool(format!(
                "no tool {} on the page{}",
                pageon_agent::quote::quote(name),
                origin.map(|o| format!(" from {o}")).unwrap_or_default()
            )));
        };
        // A form that submits itself for an agent is a write nobody reviewed: consequential.
        let consequential = tool.consequential || tool.autosubmit;
        {
            let net = self.session.net().clone();
            let mut n = net.borrow_mut();
            n.labels = vec![format!("tool:{name}")];
            if consequential {
                n.labels.push("consequential".into());
            }
        }
        out.consequential = consequential;
        if let Some(form) = tool.form {
            out.tool_origin = Some(tool.origin.clone());
            let r = self.call_form_tool(frame, &tool, form, input, out);
            self.session.net().borrow_mut().labels.clear();
            return r;
        }
        out.tool_origin = Some(tool.origin.clone());
        let live = self.live(frame)?;
        let errors = self.frame_errors(frame);
        let called = live.engine.borrow_mut().call_tool(live.realm, name, input);
        let settled = self.after(frame, &live, errors, out);
        self.session.net().borrow_mut().labels.clear();
        let handle = called.map_err(|t| ActionError::Tool(page_text(&t.text)))?;
        settled?;
        if out.navigated.is_some() {
            out.tool_result = Some("null".into());
            return Ok(());
        }
        self.tool_value(&live, handle, name, out)
    }

    /// Calls a form tool (WebMCP's declarative API): fills its fields as typing, selecting
    /// and clicking do, fires `toolactivated`, then submits it if it says `toolautosubmit`.
    /// The result is the page's `respondWith` answer, the JSON-LD of the page the submission
    /// loaded, or, for a form the agent must submit itself, its submit button.
    fn call_form_tool(
        &mut self,
        frame: usize,
        tool: &pageon_js::ToolInfo,
        form: NodeId,
        input: &serde_json::Value,
        out: &mut Outcome,
    ) -> Result<(), ActionError> {
        let name = tool.name.as_str();
        let live = self.live(frame)?;
        let errors = self.frame_errors(frame);
        let params = live.engine.borrow().tool_parameters(live.realm, form);
        let fail = |m: String| ActionError::Tool(format!("{name}: {m}"));
        for (key, value) in input.as_object().into_iter().flatten() {
            let Some(&(_, node)) = params.iter().find(|(n, _)| n == key) else {
                let known: Vec<&str> = params.iter().map(|(n, _)| n.as_str()).collect();
                return Err(fail(format!(
                    "no parameter {key:?} (it has {})",
                    known.join(", ")
                )));
            };
            let (tag, ty) = {
                let dom = live.dom.borrow();
                let n = dom.get(node);
                (
                    n.and_then(|x| x.local_name().map(str::to_string))
                        .unwrap_or_default(),
                    n.and_then(|x| x.attr("type"))
                        .unwrap_or("text")
                        .to_ascii_lowercase(),
                )
            };
            let text = |v: &serde_json::Value| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            match (tag.as_str(), ty.as_str()) {
                ("select", _) => {
                    let values: Vec<String> = match value {
                        serde_json::Value::Array(a) => a.iter().map(text).collect(),
                        v => vec![text(v)],
                    };
                    let missing = live.engine.borrow_mut().select(live.realm, node, &values);
                    if !missing.is_empty() {
                        return Err(fail(format!("{key}: no option {}", missing.join(", "))));
                    }
                }
                ("input", "checkbox") => {
                    let want = value
                        .as_bool()
                        .ok_or_else(|| fail(format!("{key} takes true or false")))?;
                    let mut engine = live.engine.borrow_mut();
                    if engine.checked(live.realm, node) != want {
                        engine.click(live.realm, node, None);
                    }
                }
                ("input", "radio") => {
                    let want = text(value);
                    let radio = {
                        let dom = live.dom.borrow();
                        dom.traverse(form).find_map(|e| match e {
                            pageon_dom::Edge::Enter(r) => dom.get(r).and_then(|x| {
                                (x.is_html("input")
                                    && x.attr("type")
                                        .is_some_and(|t| t.eq_ignore_ascii_case("radio"))
                                    && x.attr("name") == Some(key.as_str())
                                    && x.attr("value").unwrap_or("on") == want)
                                    .then_some(r)
                            }),
                            pageon_dom::Edge::Leave(_) => None,
                        })
                    };
                    let r = radio.ok_or_else(|| fail(format!("{key}: no choice {want:?}")))?;
                    live.engine.borrow_mut().click(live.realm, r, None);
                }
                _ => self.enter_value(frame, &live, node, &text(value))?,
            }
        }
        live.engine.borrow_mut().tool_activated(live.realm, name);
        if !tool.autosubmit {
            self.after(frame, &live, errors, out)?;
            let button = live.engine.borrow_mut().default_button(live.realm, form);
            let submit = button.and_then(|b| {
                self.episode
                    .ref_of(&format!("F{frame}"), crate::capture::backend(frame, b))
            });
            out.tool_result =
                Some(serde_json::json!({"filled": true, "submit": submit}).to_string());
            return Ok(());
        }
        let answer = live.engine.borrow_mut().agent_submit(live.realm, form);
        self.after(frame, &live, errors, out)?;
        if !out.invalid.is_empty() {
            return Ok(());
        }
        match answer {
            Some(h) if out.navigated.is_none() => self.tool_value(&live, h, name, out),
            _ => {
                out.tool_result = Some(if out.navigated.is_some() {
                    self.json_ld().unwrap_or_else(|| "null".into())
                } else {
                    "null".into()
                });
                Ok(())
            }
        }
    }

    /// The page's first `<script type="application/ld+json">`, as compact JSON (the answer of a
    /// form tool whose submission loaded a page).
    fn json_ld(&self) -> Option<String> {
        let dom = self.doc.dom();
        let script = dom.traverse(dom.root()).find_map(|e| match e {
            pageon_dom::Edge::Enter(n) => dom
                .get(n)
                .filter(|x| {
                    x.is_html("script")
                        && x.attr("type")
                            .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json"))
                })
                .map(|_| n),
            pageon_dom::Edge::Leave(_) => None,
        })?;
        let text: String = dom
            .children(script)
            .filter_map(|c| match dom.get(c).map(|x| &x.data) {
                Some(pageon_dom::NodeData::Text(t)) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
        let s = v.to_string();
        const MAX: usize = 32 * 1024;
        Some(if s.len() > MAX {
            let mut cut = MAX;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}… (cut at 32 KB)", &s[..cut])
        } else {
            s
        })
    }

    /// Puts text into a field as the agent's `type` does: a `secret://` handle is filled at
    /// once with the secret for the field's origin; anything else is typed.
    fn enter_value(
        &mut self,
        frame: usize,
        live: &Rc<LiveDoc>,
        node: NodeId,
        text: &str,
    ) -> Result<(), ActionError> {
        match text.strip_prefix("secret://") {
            Some(name) => {
                let origin = self.frame_origin(frame);
                let v = self
                    .session
                    .env()
                    .secret(name, &origin)
                    .map_err(|e| ActionError::Secret(e.to_string()))?;
                self.session.keep_secret(v.clone(), text.to_string());
                live.engine.borrow_mut().fill_secret(live.realm, node, &v);
            }
            None => live.engine.borrow_mut().type_text(live.realm, node, text),
        }
        Ok(())
    }

    /// The settled value of a tool's answer as its result (JSON text, page data, cut at
    /// 32 KB), or why there is none.
    fn tool_value(
        &self,
        live: &Rc<LiveDoc>,
        handle: pageon_js::Handle,
        name: &str,
        out: &mut Outcome,
    ) -> Result<(), ActionError> {
        let mut engine = live.engine.borrow_mut();
        let value = match engine.promise_state(live.realm, handle) {
            pageon_js::PromiseState::NotPromise => handle,
            pageon_js::PromiseState::Fulfilled(v) => v,
            pageon_js::PromiseState::Rejected(e) => {
                let why = engine
                    .describe(live.realm, e, false)
                    .ok()
                    .and_then(|d| {
                        d.get("description")
                            .or(d.get("value"))
                            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                    })
                    .unwrap_or_else(|| "rejected".into());
                // The message, not the stack.
                let why = why.lines().next().unwrap_or_default().to_string();
                return Err(ActionError::Tool(format!(
                    "{name} rejected: {}",
                    page_text(&why)
                )));
            }
            pageon_js::PromiseState::Pending => {
                return Err(ActionError::Tool(format!(
                    "{name} did not finish within {} ms",
                    self.options.settle_ms
                )));
            }
        };
        let json = engine
            .describe(live.realm, value, true)
            .map_err(|e| ActionError::Tool(format!("{name}'s result cannot be serialized: {e}")))?;
        let text = json
            .get("value")
            .map_or_else(|| "null".to_string(), ToString::to_string);
        const MAX: usize = 32 * 1024;
        out.tool_result = Some(if text.len() > MAX {
            let mut cut = MAX;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}… (cut at 32 KB)", &text[..cut])
        } else {
            text
        });
        Ok(())
    }

    /// The origin of frame `frame`'s document (`https://shop.test`), `null` if unknown.
    fn frame_origin(&mut self, frame: usize) -> String {
        frame_path(&self.doc, frame)
            .and_then(|p| doc_at(&mut self.doc, &p).map(|d| crate::frames::origin_of(&d.final_url)))
            .unwrap_or_else(|| "null".into())
    }

    /// Page errors so far in the engine of frame `frame` (shared by its origin's documents).
    fn frame_errors(&mut self, frame: usize) -> usize {
        self.live_if_started(frame)
            .map_or(0, |l| l.engine.borrow().errors().len())
    }

    /// The live document of frame `frame`, starting its engine if it has none yet.
    fn live(&mut self, frame: usize) -> Result<Rc<LiveDoc>, ActionError> {
        let path = frame_path(&self.doc, frame).ok_or_else(|| {
            ActionError::Unsupported("acting in a frame that did not load".into())
        })?;
        let options = self.options.clone();
        let env = self.session.env();
        let net = self.session.net().clone();
        let page = self.doc.page.clone();
        let doc = doc_at(&mut self.doc, &path).ok_or_else(|| {
            ActionError::Unsupported("acting in a frame that did not load".into())
        })?;
        if let Some(l) = &doc.live {
            return Ok(l.clone());
        }
        let live = crate::scripts::start_live(doc, env, &net, options, page.as_deref())
            .map_err(|e| ActionError::Navigation(e.into()))?;
        let live = Rc::new(live);
        doc.live = Some(live.clone());
        Ok(live)
    }

    /// Scrolls `node` (in `frame`) into view as a person would, centring it if it is not fully
    /// visible, through its scroll containers, its frame's document and the documents of the
    /// frames around it. Reports the top-level position in the outcome. Returns whether
    /// anything moved.
    pub(crate) fn scroll_into_view(
        &mut self,
        frame: usize,
        node: NodeId,
        out: &mut Outcome,
    ) -> Result<bool, ActionError> {
        let metas = self.frames_meta();
        let top = self.doc.js.scroll.viewport;
        let (mut frame, mut node) = (frame, node);
        let mut moved = false;
        loop {
            let live = self.live(frame)?;
            moved |= live.engine.borrow_mut().scroll_into_view(live.realm, node);
            match metas.get(frame).and_then(|m| m.parent.zip(m.owner)) {
                Some((parent, owner)) => (frame, node) = (parent, owner),
                None => break,
            }
        }
        refresh(&mut self.doc);
        let now = self.doc.js.scroll.viewport;
        if now != top {
            out.scrolled
                .push(format!("scrolled to {},{}", now.0.round(), now.1.round()));
        }
        Ok(moved)
    }

    /// Resolves a ref against the page as it is now: the frame, the node, and the point to
    /// click when the element's centre is covered but another part of it is not.
    fn resolve(&mut self, r: &str) -> Result<Resolved, ActionError> {
        let capture = self.doc.capture(Viewport::default());
        match self.episode.resolve(r, &capture) {
            Ok(t) => self
                .doc
                .node_for(&t)
                .map(|(f, n)| (f, n, None))
                .ok_or_else(|| ActionError::StaleRef(r.to_string())),
            Err(RefError::Covered { covered_by }) => {
                let covered = || ActionError::NotActionable {
                    target: r.to_string(),
                    reason: Reason::Covered {
                        by: covered_by.clone(),
                    },
                };
                // The ref is valid; find the node, then a visible point of it.
                let key = self.episode.key(r).ok_or_else(covered)?;
                let (frame, node) = self.doc.node_for(&key).ok_or_else(covered)?;
                let path = frame_path(&self.doc, frame).ok_or_else(covered)?;
                let doc = doc_at(&mut self.doc, &path).ok_or_else(covered)?;
                let point = visible_point(doc, node).ok_or_else(covered)?;
                Ok((frame, node, Some(point)))
            }
            Err(RefError::Stale { .. } | RefError::Reset) => {
                Err(ActionError::StaleRef(r.to_string()))
            }
            Err(RefError::Unknown | RefError::Ambiguous { .. }) => {
                Err(ActionError::UnknownRef(r.to_string()))
            }
        }
    }

    /// Actionability: visible, enabled, not inert.
    fn check(
        &mut self,
        frame: usize,
        node: NodeId,
        r: &str,
        action: &Action,
    ) -> Result<(), ActionError> {
        let not = |reason| ActionError::NotActionable {
            target: r.to_string(),
            reason,
        };
        let path = frame_path(&self.doc, frame).ok_or_else(|| ActionError::StaleRef(r.into()))?;
        let doc = doc_at(&mut self.doc, &path).ok_or_else(|| ActionError::StaleRef(r.into()))?;
        let is_option = doc.dom.get(node).is_some_and(|n| n.is_html("option"));
        let layout = doc.layout(Viewport::default());
        if !is_option && layout.visibility(node) != Visibility::Visible {
            return Err(not(Reason::Hidden));
        }
        if !matches!(action, Action::Hover(_)) && disabled(doc, node) {
            return Err(not(Reason::Disabled));
        }
        if let Some(&modal) = doc.js.modals.last()
            && modal != node
            && !doc.dom.ancestors(node).any(|a| a == modal)
        {
            return Err(not(Reason::Inert));
        }
        Ok(())
    }

    /// After input: settle, then collect what the page asked for and follow a navigation.
    fn after(
        &mut self,
        frame: usize,
        live: &Rc<LiveDoc>,
        errors_before: usize,
        out: &mut Outcome,
    ) -> Result<(), ActionError> {
        let window = self.options.settle_ms as f64;
        self.after_within(frame, live, errors_before, out, window)
    }

    /// [`after`](Self::after), settling within `window` ms of virtual time.
    fn after_within(
        &mut self,
        frame: usize,
        live: &Rc<LiveDoc>,
        errors_before: usize,
        out: &mut Outcome,
        window: f64,
    ) -> Result<(), ActionError> {
        let settled = live.engine.borrow_mut().settle(window, self.session.env());
        out.settled = Settle {
            virtual_ms: settled.virtual_ms,
            pending_timers: settled.pending,
        };
        let errors_doc = {
            let engine = live.engine.borrow();
            engine.errors()
        };
        out.errors
            .extend(errors_doc.into_iter().skip(errors_before));
        // Messages the action posted to documents of other origins, and their answers.
        if let Some(page) = self.doc.page.clone() {
            let others: Vec<_> = page
                .engines
                .borrow()
                .values()
                .filter(|e| !Rc::ptr_eq(e, &live.engine))
                .map(|e| (e.clone(), e.borrow().errors().len()))
                .collect();
            page.pump(self.session.env(), window);
            for (e, before) in others {
                out.errors
                    .extend(e.borrow().errors().into_iter().skip(before));
            }
        }
        self.finish(frame, live, out)
    }

    fn finish(
        &mut self,
        frame: usize,
        live: &Rc<LiveDoc>,
        out: &mut Outcome,
    ) -> Result<(), ActionError> {
        let (nav, dialogs, popups) = live.engine.borrow_mut().take_requests(live.realm);
        let blocked = live.engine.borrow_mut().take_blocked(live.realm);
        let (dialogs_more, popups_more) = live.engine.borrow_mut().take_dropped(live.realm);
        out.dialogs_more += dialogs_more;
        out.popups_more += popups_more;
        out.dialogs.extend(dialogs);
        out.popups.extend(popups);
        refresh(&mut self.doc);
        if !blocked.is_empty() {
            self.report_invalid(frame, blocked, out);
        }
        if frame == 0 {
            self.apply_history(out);
        } else {
            // Frames have no session history of their own here: their requests are dropped.
            let _ = live.engine.borrow_mut().take_history(live.realm);
        }
        let Some(nav) = nav else { return Ok(()) };
        if frame != 0 {
            return Err(ActionError::Unsupported(
                "navigating a frame (the frame asked to load another page)".into(),
            ));
        }
        let initiator = self.doc.client.clone();
        let mode = if nav.replace {
            History::Replace
        } else {
            History::Push
        };
        self.navigate(initiator.as_ref(), &nav, mode, out)
    }

    /// Lists the fields whose validation blocked a submission, with the agent's refs and their
    /// accessible names.
    fn report_invalid(&self, frame: usize, blocked: Vec<pageon_js::Blocked>, out: &mut Outcome) {
        let docs = crate::capture::frame_documents(&self.doc);
        let Some(Some(doc)) = docs.get(frame) else {
            return;
        };
        let composed = doc.dom().has_shadow().then(|| doc.dom().composed().dom);
        let dom = composed.as_ref().unwrap_or(doc.dom());
        // Names as the agent sees them: text hidden by CSS is not part of a name.
        let layout = doc.layout(Viewport::default());
        let tree = pageon_ax::Tree::build(dom, layout.error().is_none().then_some(&layout));
        let mut seen = std::collections::HashSet::new();
        let fields: Vec<&pageon_js::Blocked> =
            blocked.iter().filter(|b| seen.insert(b.node)).collect();
        out.invalid_more += fields.len().saturating_sub(MAX_INVALID);
        for b in fields.into_iter().take(MAX_INVALID) {
            // A component is reported by the field the agent types into (its anchor), unless
            // the component itself has a name.
            let own = tree.name(b.node).unwrap_or_default();
            let node = if own.is_empty() { b.focus } else { b.node };
            let backend = crate::capture::backend(frame, node);
            let target = self
                .episode
                .ref_of(&format!("F{frame}"), backend)
                .or_else(|| {
                    self.episode
                        .ref_of(&format!("F{frame}"), crate::capture::backend(frame, b.node))
                });
            let name = tree.name(node).unwrap_or_default().to_string();
            out.invalid.push(InvalidField {
                target,
                name: pageon_agent::quote::clip(&name, 100),
                message: pageon_agent::quote::clip(&b.message, 300),
                page_data: b.page_set,
            });
        }
    }

    /// Loads `nav` in place (the old page goes on success, stays on failure).
    fn navigate(
        &mut self,
        initiator: Option<&pageon_net::Client>,
        nav: &NavRequest,
        mode: History,
        out: &mut Outcome,
    ) -> Result<(), ActionError> {
        self.navigate_with(initiator, nav, mode, out, None)
    }

    /// [`navigate`](Self::navigate), from a download's bytes when `preloaded` is set.
    fn navigate_with(
        &mut self,
        initiator: Option<&pageon_net::Client>,
        nav: &NavRequest,
        mode: History,
        out: &mut Outcome,
        preloaded: Option<&Download>,
    ) -> Result<(), ActionError> {
        // A traversal restores the entry's state for the new document's scripts.
        let restored = (mode == History::Keep)
            .then(|| self.history[self.index].state.clone())
            .flatten();
        let start = Start {
            initiator: initiator.map(|c| (c, nav)),
            keep: true,
            parent: None,
            delegate_tools: false,
            history: restored.as_ref(),
            preloaded,
            ..Default::default()
        };
        let loaded = crate::load::open(
            &nav.url,
            self.session.env(),
            self.session.net(),
            self.options.clone(),
            start,
        )
        .map_err(ActionError::Navigation)?;
        match loaded {
            Loaded::Page(mut doc) => {
                let url = doc.final_url.clone();
                out.dialogs.append(&mut doc.js.dialogs);
                out.popups.append(&mut doc.js.popups);
                out.errors.extend(doc.js.errors.iter().cloned());
                out.settled = Settle {
                    virtual_ms: doc.summary.virtual_ms,
                    pending_timers: doc.summary.pending_timers,
                };
                self.doc_seq += 1;
                let seq = self.doc_seq;
                // A new tab's initial `about:blank` is replaced by its first page (as Chrome).
                let initial = self.history.len() == 1 && self.history[0].url == "about:blank";
                let mode = if initial && mode == History::Push {
                    History::Replace
                } else {
                    mode
                };
                match mode {
                    History::Push => {
                        self.history[self.index].scroll = self.doc.js.scroll.viewport;
                        self.history.truncate(self.index + 1);
                        self.history.push(Entry::new(url.clone(), seq));
                        self.trim_history();
                    }
                    History::Replace => self.history[self.index] = Entry::new(url.clone(), seq),
                    History::Keep => {
                        let e = &mut self.history[self.index];
                        e.url.clone_from(&url);
                        e.doc = seq;
                    }
                }
                self.session.seen(doc.loaded_at);
                let mut doc = *doc;
                doc.set_images(self.images.clone());
                out.refusal.clone_from(&doc.refusal);
                self.close_streams();
                self.doc = doc;
                out.navigated = Some(url);
                out.same_document = false;
                // History changes the new page made while loading.
                self.apply_history(out);
            }
            Loaded::Download(mut d) => {
                d.handle = format!("download:{}", self.downloads.len() + 1);
                if out.downloads.len() < pageon_js::MAX_PER_ACTION {
                    out.downloads.push(d.clone());
                    self.downloads.push(d);
                } else {
                    out.downloads_more += 1;
                }
            }
        }
        Ok(())
    }

    /// Keeps at most [`MAX_ENTRIES`] and points at the last one. Entries a page added without
    /// a user activation go first, oldest first, so a page flooding the history cannot bury
    /// the agent's way back; then the oldest entries.
    fn trim_history(&mut self) {
        let last = self.history.len().saturating_sub(1);
        while self.history.len() > MAX_ENTRIES {
            let drop = (0..last.min(self.history.len() - 1))
                .find(|&i| self.history[i].skippable)
                .unwrap_or(0);
            self.history.remove(drop);
        }
        self.index = self.history.len() - 1;
    }

    /// Tells the current document where it is in the tab's history.
    fn sync_history(&self) {
        if let Some(live) = &self.doc.live {
            let (index, len) = (self.index, self.history.len());
            live.engine.borrow_mut().set_history(
                live.realm,
                u32::try_from(index).unwrap_or(u32::MAX),
                u32::try_from(len).unwrap_or(u32::MAX),
            );
        }
    }

    /// Applies what the main document asked of the history (`pushState`, `replaceState`,
    /// fragment navigations, `back`/`forward`/`go`), in order; in-place URL changes are
    /// reported as `navigated to "…" (same page)` unless the action loaded a document.
    fn apply_history(&mut self, out: &mut Outcome) {
        self.history_depth += 1;
        self.apply_history_ops(out);
        self.history_depth -= 1;
        self.sync_history();
    }

    fn apply_history_ops(&mut self, out: &mut Outcome) {
        // A page answering `popstate` or a load with more history calls is bounded here.
        for _ in 0..16 {
            let Some(live) = self.doc.live.clone() else {
                return;
            };
            let ops = live.engine.borrow_mut().take_history(live.realm);
            if ops.is_empty() {
                break;
            }
            for op in ops {
                match op {
                    pageon_js::HistoryOp::Push {
                        url,
                        state,
                        activated,
                    } => {
                        let current = &mut self.history[self.index];
                        current.scroll = self.doc.js.scroll.viewport;
                        if !activated {
                            current.skippable = true;
                        }
                        self.history.truncate(self.index + 1);
                        let mut e = Entry::new(self.redact(url.clone()), self.doc_seq);
                        e.state = state;
                        self.history.push(e);
                        self.trim_history();
                        self.moved_in_place(url, out);
                    }
                    pageon_js::HistoryOp::Replace { url, state } => {
                        let e = &mut self.history[self.index];
                        e.state = state;
                        if e.url != url {
                            e.url.clone_from(&url);
                            self.moved_in_place(url, out);
                        }
                    }
                    pageon_js::HistoryOp::Traverse(delta) => {
                        let target = i64::try_from(self.index).unwrap_or(0) + i64::from(delta);
                        if self.history_depth <= 3
                            && let Ok(i) = usize::try_from(target)
                            && i < self.history.len()
                        {
                            let _ = self.traverse_to(i, false, out);
                        }
                    }
                }
            }
            self.sync_history();
        }
    }

    /// The document's URL changed in place.
    fn moved_in_place(&mut self, url: String, out: &mut Outcome) {
        let loaded = out.navigated.is_some() && !out.same_document;
        self.doc.final_url.clone_from(&url);
        out.navigated = Some(url);
        out.same_document = !loaded;
    }

    /// Goes to history entry `i`: in place (`popstate`) when it belongs to the current
    /// document, by loading its document (with its state) otherwise.
    fn traverse_to(
        &mut self,
        i: usize,
        settle: bool,
        out: &mut Outcome,
    ) -> Result<(), ActionError> {
        let same = self.history[i].doc == self.doc_seq;
        if same && let Some(live) = self.doc.live.clone() {
            self.history[self.index].scroll = self.doc.js.scroll.viewport;
            self.index = i;
            let e = self.history[i].clone();
            let (_, manual) = live.engine.borrow().history_state(live.realm);
            let errors_before = live.engine.borrow().errors().len();
            live.engine.borrow_mut().restore_history(
                live.realm,
                (&e.url, e.state.clone()),
                (
                    u32::try_from(i).unwrap_or(u32::MAX),
                    u32::try_from(self.history.len()).unwrap_or(u32::MAX),
                ),
                (!manual).then_some(e.scroll),
            );
            self.moved_in_place(e.url, out);
            // The agent's move settles like an action; a page's own move runs within its task.
            return if settle {
                self.after(0, &live, errors_before, out)
            } else {
                Ok(())
            };
        }
        let nav = NavRequest {
            url: self.history[i].url.clone(),
            method: "GET".into(),
            body: Vec::new(),
            content_type: None,
            replace: false,
        };
        let before = self.index;
        self.history[before].scroll = self.doc.js.scroll.viewport;
        self.index = i;
        let r = self.navigate(None, &nav, History::Keep, out);
        if r.is_err() {
            self.index = before;
        }
        r
    }
}

/// Page-authored text in an engine message, quoted.
fn page_text(s: &str) -> String {
    pageon_agent::quote::quote(s)
}

/// A tool's parameters from its `inputSchema`: `from: string, date?: string`.
/// A schema name or type as the agent sees it: a plain identifier as is, anything else quoted
/// and clipped (page data cannot pass as view syntax).
fn schema_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain {
        s.to_string()
    } else {
        pageon_agent::quote::quote(&pageon_agent::quote::clip(s, 40))
    }
}

fn params(schema: Option<&str>) -> String {
    let Some(v) = schema.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) else {
        return String::new();
    };
    let required: Vec<&str> = v["required"]
        .as_array()
        .map(|a| a.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    let Some(props) = v["properties"].as_object() else {
        return String::new();
    };
    // Required parameters first, in their declared order; then the optional ones.
    let mut names: Vec<&str> = required
        .iter()
        .copied()
        .filter(|r| props.contains_key(*r))
        .collect();
    names.extend(
        props
            .keys()
            .map(String::as_str)
            .filter(|k| !required.contains(k)),
    );
    names
        .into_iter()
        .map(|k| {
            let ty = props[k]["type"].as_str().unwrap_or("any");
            let opt = if required.contains(&k) { "" } else { "?" };
            format!("{}{opt}: {}", schema_word(k), schema_word(ty))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `s` cut at `max` characters, saying so.
fn cut_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push_str(&format!(" … [cut at {max} characters]"));
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_parse_and_print() {
        for (text, want) in [
            ("click e5", Action::Click("e5".into())),
            (
                "type e3 hello world",
                Action::Type("e3".into(), "hello world".into()),
            ),
            (
                "type e3 \"quoted text\"",
                Action::Type("e3".into(), "quoted text".into()),
            ),
            ("press Enter", Action::Press("Enter".into())),
            (
                "select e7 New Zealand",
                Action::Select("e7".into(), vec!["New Zealand".into()]),
            ),
            ("hover e2", Action::Hover("e2".into())),
            ("wait 500", Action::Wait(500)),
            (
                "goto https://a.test/",
                Action::Goto("https://a.test/".into()),
            ),
            ("back", Action::Back),
        ] {
            assert_eq!(text.parse::<Action>(), Ok(want.clone()), "{text}");
            assert_eq!(want.to_string().parse::<Action>(), Ok(want));
        }
        assert!("jump e1".parse::<Action>().is_err());
        assert!("click".parse::<Action>().is_err());
    }
}
