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
    /// Select options of a `<select>` by value or label.
    Select(String, Vec<String>),
    /// Move the pointer onto an element.
    Hover(String),
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
            Self::Wait(ms) => write!(f, "wait {ms}"),
            Self::Goto(u) => write!(f, "goto {u}"),
            Self::Back => f.write_str("back"),
            Self::Forward => f.write_str("forward"),
            Self::Scroll { target, by } => {
                f.write_str("scroll")?;
                if let Some(t) = target {
                    write!(f, " {t}")?;
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

    /// `click e5`, `click 120,40` (a viewport point), `type e3 hello world`, `press Enter`,
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
            "click" => match point() {
                Some((x, y)) => Self::ClickAt(x, y),
                None => Self::Click(need("a ref, e.g. `click e5`")?),
            },
            "hover" => match point() {
                Some((x, y)) => Self::HoverAt(x, y),
                None => Self::Hover(need("a ref, e.g. `hover e5`")?),
            },
            "type" => Self::Type(need("a ref and text, e.g. `type e3 hello`")?, unquote(tail)),
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
                need("a ref and an option, e.g. `select e7 Germany`")?,
                vec![unquote(tail)],
            ),
            "press" => Self::Press(need("a key, e.g. `press Enter`")?),
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
                    "unknown action {other:?}: use click, type, upload, press, select, hover, scroll, wait, goto, back, forward or tool"
                ));
            }
        })
    }
}

/// `down`, `down 300`, `by 120,-40`, `e12`, `e5 down [PX]`.
fn parse_scroll(rest: &str) -> Result<Action, String> {
    const HELP: &str = "`scroll` needs a direction, a ref or both: `scroll down`, `scroll down 300`, `scroll by 0,300`, `scroll e12`, `scroll e5 down`";
    let words: Vec<&str> = rest.split_whitespace().collect();
    let is_ref = |w: &str| Direction::parse(w).is_none() && w != "by";
    let (target, words) = match words.split_first() {
        Some((w, more)) if is_ref(w) => (Some((*w).to_string()), more),
        _ => (None, words.as_slice()),
    };
    let px = |w: &str| {
        w.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| format!("`scroll` needs pixels as a number, not {w:?}"))
    };
    let by = match words {
        [] if target.is_some() => ScrollBy::IntoView,
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
    Ok(Action::Scroll { target, by })
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

/// What an action did.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Outcome {
    /// The action, as text (`click e5`).
    pub action: String,
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
        let mut parts = vec![format!("ok {}", self.action)];
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
        if !self.requests.is_empty() {
            let blocked = self
                .requests
                .iter()
                .filter(|r| !matches!(r.outcome, crate::RequestOutcome::Ok))
                .count();
            let mut r = format!("{} request(s)", self.requests.len());
            if blocked > 0 {
                r.push_str(&format!(" ({blocked} blocked or failed)"));
            }
            parts.push(r);
        }
        if self.consequential {
            parts.push("consequential tool".into());
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
    /// The navigation the action started failed.
    Navigation(LoadError),
    /// Not supported yet.
    Unsupported(String),
    /// A `secret://` handle that is unknown, or not for the target's origin.
    Secret(String),
    /// A file that is not staged, not for the target's origin, or cannot be read.
    File(String),
    /// The content process failed (crashed or broke the protocol).
    Engine(String),
    /// A page tool could not be called, or failed; page text is quoted.
    Tool(String),
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
            Self::Navigation(e) => write!(f, "navigation failed: {e}"),
            Self::Unsupported(w) => write!(f, "{w} is not supported yet"),
            Self::Secret(m) => write!(f, "secret refused: {m}"),
            Self::File(m) => write!(f, "upload refused: {m}"),
            Self::Engine(m) => write!(f, "the content process failed: {m}"),
            Self::Tool(m) => write!(f, "tool failed: {m}"),
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

impl<'s, E: Env> Tab<'s, E> {
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
        });
    }

    /// Replaces every secret value typed in this session by its handle.
    pub(crate) fn redact(&self, mut s: String) -> String {
        for (value, handle) in self.session.secrets().iter() {
            if !value.is_empty() && s.contains(value.as_str()) {
                s = s.replace(value.as_str(), handle);
            }
        }
        s
    }

    fn redact_outcome(&self, o: &mut Outcome) {
        if self.session.secrets().is_empty() {
            return;
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
        o.popups = o.popups.drain(..).map(|u| self.redact(u)).collect();
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
        | Action::Type(r, _)
        | Action::Select(r, _)
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
        let result = self.run(&action, &mut out);
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
            Action::Press(key) => {
                // Keys go to the frame that holds focus.
                let frame = self.focused_frame();
                let live = self.live(frame)?;
                let errors = self.frame_errors(frame);
                live.engine.borrow_mut().press(live.realm, key);
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
            } => {
                let (frame, node, _) = self.resolve(r)?;
                if !self.scroll_into_view(frame, node, out)? {
                    out.scrolled.push(format!("{r} is already in view"));
                }
                Ok(())
            }
            Action::Scroll { target, by } => {
                let (frame, node) = match target {
                    Some(r) => {
                        let (f, n, _) = self.resolve(r)?;
                        (f, Some(n))
                    }
                    None => (0, None),
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
                let wheeled = live
                    .engine
                    .borrow_mut()
                    .wheel(live.realm, None, node, amount);
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
                            let value = match text.strip_prefix("secret://") {
                                Some(name) => {
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
                            engine.type_text(live.realm, node, &value);
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

    /// `upload REF NAMES`: a file input is filled directly (it may be hidden, as file inputs
    /// behind styled buttons are); anything else is clicked as a person does and the file
    /// chooser it opens is filled. Files come from the broker (staged, bound to origins) or
    /// from this tab's downloads (to their own origin).
    fn upload(&mut self, r: &str, names: &[String], out: &mut Outcome) -> Result<(), ActionError> {
        let (frame, node, _) = self.resolve(r)?;
        let live = self.live(frame)?;
        let direct = live.engine.borrow().file_input(live.realm, node);
        let errors = self.frame_errors(frame);
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
        self.put_files(frame, target, multiple, r, names, errors, out)
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
        live.engine
            .borrow_mut()
            .set_files(live.realm, target, files);
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
        {
            let net = self.session.net().clone();
            let mut n = net.borrow_mut();
            n.labels = vec![format!("tool:{name}")];
            if tool.consequential {
                n.labels.push("consequential".into());
            }
        }
        out.consequential = tool.consequential;
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
        if matches!(action, Action::Select(..))
            && !doc.dom.get(node).is_some_and(|n| n.is_html("select"))
        {
            return Err(ActionError::Unsupported(format!(
                "select on {r}, which is not a <select> (click its options instead)"
            )));
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
