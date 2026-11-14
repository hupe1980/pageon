//! `input.performActions` / `releaseActions`: WebDriver action sequences, tick by tick, as
//! trusted CDP input events (pointer, key, wheel, pauses on the page's clock).

use pageon_env::Env;
use serde_json::{Value as Json, json};

use super::{Answer, BidiError, Session, parse_shared_id};

/// A top-level context's input state.
#[derive(Debug, Default, Clone)]
pub(super) struct State {
    keys: Vec<String>,
    buttons: Vec<u64>,
    x: f64,
    y: f64,
}

impl State {
    fn modifiers(&self) -> u64 {
        self.keys.iter().fold(0, |m, k| {
            m | match k.as_str() {
                "Alt" => 1,
                "Control" => 2,
                "Meta" => 4,
                "Shift" => 8,
                _ => 0,
            }
        })
    }
}

/// The key a WebDriver key value names (a grapheme, or a code point of the `` table).
fn key_name(v: &str) -> Result<String, BidiError> {
    let mut chars = v.chars();
    let (Some(c), None) = (chars.next(), chars.clone().next()) else {
        // A grapheme cluster of several code points is typed as itself.
        return if v.is_empty() {
            Err(BidiError::invalid("a key value is one grapheme"))
        } else {
            Ok(v.to_string())
        };
    };
    let named = match c as u32 {
        0xE000 => "Unidentified",
        0xE001 => "Cancel",
        0xE002 => "Help",
        0xE003 => "Backspace",
        0xE004 => "Tab",
        0xE005 => "Clear",
        0xE006 | 0xE007 => "Enter",
        0xE008 | 0xE050 => "Shift",
        0xE009 | 0xE051 => "Control",
        0xE00A | 0xE052 => "Alt",
        0xE00B => "Pause",
        0xE00C => "Escape",
        0xE00D => " ",
        0xE00E | 0xE054 => "PageUp",
        0xE00F | 0xE055 => "PageDown",
        0xE010 | 0xE056 => "End",
        0xE011 | 0xE057 => "Home",
        0xE012 | 0xE058 => "ArrowLeft",
        0xE013 | 0xE059 => "ArrowUp",
        0xE014 | 0xE05A => "ArrowRight",
        0xE015 | 0xE05B => "ArrowDown",
        0xE016 | 0xE05C => "Insert",
        0xE017 | 0xE05D => "Delete",
        0xE018 => ";",
        0xE019 => "=",
        n @ 0xE01A..=0xE023 => return Ok(char::from(b'0' + (n - 0xE01A) as u8).to_string()),
        0xE024 => "*",
        0xE025 => "+",
        0xE026 => ",",
        0xE027 => "-",
        0xE028 => ".",
        0xE029 => "/",
        n @ 0xE031..=0xE03C => return Ok(format!("F{}", n - 0xE030)),
        0xE03D | 0xE053 => "Meta",
        0xE040 => "ZenkakuHankaku",
        n if (0xE000..=0xF8FF).contains(&n) => {
            return Err(BidiError::invalid(format!(
                "key value U+{n:04X} is not in the WebDriver key table"
            )));
        }
        _ => return Ok(c.to_string()),
    };
    Ok(named.into())
}

fn button_name(b: u64) -> &'static str {
    match b {
        0 => "left",
        1 => "middle",
        2 => "right",
        3 => "back",
        4 => "forward",
        _ => "none",
    }
}

impl<E: Env> Session<'_, E> {
    pub(super) fn perform_actions(&mut self, p: &Json) -> Answer {
        let context = p["context"]
            .as_str()
            .ok_or_else(|| BidiError::invalid("context: a browsing context id is required"))?;
        let top = self
            .top_of(context)
            .ok_or_else(|| BidiError::no_frame(context))?;
        let session = self.context_of(&top)?.session.clone();
        let sources = p["actions"]
            .as_array()
            .ok_or_else(|| BidiError::invalid("actions: a list of input sources"))?
            .clone();
        let ticks = sources
            .iter()
            .map(|s| s["actions"].as_array().map_or(0, Vec::len))
            .max()
            .unwrap_or(0);
        for t in 0..ticks {
            let mut pause = 0u64;
            for s in &sources {
                let Some(a) = s["actions"].get(t) else {
                    continue;
                };
                let kind = s["type"].as_str().unwrap_or_default();
                pause = pause.max(self.action(&top, &session, kind, a)?);
            }
            if pause > 0 {
                let out = self.cdp.idle(pause);
                self.take_cdp(out, None);
            }
        }
        Ok(json!({}))
    }

    /// One action of one source; returns how long the tick should last (ms).
    fn action(&mut self, top: &str, session: &str, kind: &str, a: &Json) -> Result<u64, BidiError> {
        let ty = a["type"].as_str().unwrap_or_default();
        let duration = a["duration"].as_u64().unwrap_or(0);
        if ty == "pause" {
            return Ok(duration);
        }
        let mut st = self.input.get(top).cloned().unwrap_or_default();
        match (kind, ty) {
            ("key", "keyDown" | "keyUp") => {
                let key = key_name(a["value"].as_str().unwrap_or_default())?;
                let down = ty == "keyDown";
                if down {
                    st.keys.push(key.clone());
                } else {
                    st.keys.retain(|k| *k != key);
                }
                let text = if down && key.chars().count() == 1 {
                    key.clone()
                } else {
                    String::new()
                };
                self.call(
                    "Input.dispatchKeyEvent",
                    json!({"type": if down { "keyDown" } else { "keyUp" }, "key": key, "text": text, "modifiers": st.modifiers()}),
                    Some(session),
                )?;
            }
            ("pointer", "pointerMove") => {
                let (x, y) = self.point(a, &st)?;
                st.x = x;
                st.y = y;
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({"type": "mouseMoved", "x": x, "y": y, "modifiers": st.modifiers(), "button": "none"}),
                    Some(session),
                )?;
            }
            ("pointer", "pointerDown" | "pointerUp") => {
                let b = a["button"].as_u64().unwrap_or(0);
                let down = ty == "pointerDown";
                if down {
                    st.buttons.push(b);
                } else {
                    st.buttons.retain(|x| *x != b);
                }
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": if down { "mousePressed" } else { "mouseReleased" },
                        "x": st.x, "y": st.y, "button": button_name(b), "clickCount": 1,
                        "modifiers": st.modifiers(),
                    }),
                    Some(session),
                )?;
            }
            ("wheel", "scroll") => {
                let (x, y) = self.point(a, &st)?;
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": a["deltaX"], "deltaY": a["deltaY"]}),
                    Some(session),
                )?;
            }
            (k, t) => {
                return Err(BidiError::invalid(format!(
                    "{t:?} is not an action of a {k:?} input source"
                )));
            }
        }
        self.input.insert(top.to_string(), st);
        Ok(duration)
    }

    /// A pointer target: viewport coordinates, relative to the pointer, or to an element's
    /// center.
    fn point(&mut self, a: &Json, st: &State) -> Result<(f64, f64), BidiError> {
        let (dx, dy) = (
            a["x"].as_f64().unwrap_or(0.0),
            a["y"].as_f64().unwrap_or(0.0),
        );
        let origin = &a["origin"];
        if origin.is_null() || origin == "viewport" {
            return Ok((dx, dy));
        }
        if origin == "pointer" {
            return Ok((st.x + dx, st.y + dy));
        }
        if origin["type"] == "element" {
            let s = origin["element"]["sharedId"]
                .as_str()
                .ok_or_else(|| BidiError::invalid("origin.element: a node with a sharedId"))?;
            let (frame, _, backend) = parse_shared_id(s)
                .ok_or_else(|| BidiError::new("no such node", format!("no node {s:?}")))?;
            let session = self.context_of(frame)?.session.clone();
            let q = self.call(
                "DOM.getContentQuads",
                json!({"backendNodeId": backend}),
                Some(&session),
            )?;
            let quad = q["quads"][0].as_array().cloned().unwrap_or_default();
            let vals: Vec<f64> = quad.iter().filter_map(Json::as_f64).collect();
            if vals.len() < 8 {
                return Err(BidiError::new(
                    "move target out of bounds",
                    "the element has no box (not rendered)",
                ));
            }
            let cx = (vals[0] + vals[2] + vals[4] + vals[6]) / 4.0;
            let cy = (vals[1] + vals[3] + vals[5] + vals[7]) / 4.0;
            return Ok((cx + dx, cy + dy));
        }
        Err(BidiError::invalid(format!("origin {origin}")))
    }

    /// `input.setFiles`: staged files (by path or name) into a file input.
    pub(super) fn set_files(&mut self, p: &Json) -> Answer {
        let s = p["element"]["sharedId"]
            .as_str()
            .ok_or_else(|| BidiError::invalid("element: a node with a sharedId"))?;
        let (frame, _, backend) = parse_shared_id(s)
            .ok_or_else(|| BidiError::new("no such node", format!("no node {s:?}")))?;
        let session = self.context_of(frame)?.session.clone();
        let files = p["files"].clone();
        self.call(
            "DOM.setFileInputFiles",
            json!({"backendNodeId": backend, "files": files}),
            Some(&session),
        )
        .map_err(|e| BidiError::new("unable to set file input", e.message))?;
        Ok(json!({}))
    }

    pub(super) fn release_actions(&mut self, p: &Json) -> Answer {
        let context = p["context"]
            .as_str()
            .ok_or_else(|| BidiError::invalid("context: a browsing context id is required"))?;
        let top = self
            .top_of(context)
            .ok_or_else(|| BidiError::no_frame(context))?;
        let session = self.context_of(&top)?.session.clone();
        let st = self.input.remove(&top).unwrap_or_default();
        for b in st.buttons.iter().rev() {
            self.call(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseReleased", "x": st.x, "y": st.y, "button": button_name(*b)}),
                Some(&session),
            )?;
        }
        for k in st.keys.iter().rev() {
            self.call(
                "Input.dispatchKeyEvent",
                json!({"type": "keyUp", "key": k}),
                Some(&session),
            )?;
        }
        Ok(json!({}))
    }
}
