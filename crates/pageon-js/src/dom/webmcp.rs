//! WebMCP: `document.modelContext.registerTool`. Each tool keeps provenance the
//! engine records at registration (origin, registering script, third-party frame), which the
//! page cannot change; the agent side reads the registry through [`crate::Engine::tools`].

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{
    Iface, ModelContextRegisterToolOptions, ModelContextTool, ToolActivatedEventInit,
    ToolCancelEventInit,
};
use crate::realm::{EventData, Slot, Target};

/// A registered tool.
#[derive(Debug, Clone)]
pub struct Tool {
    pub(crate) info: ToolInfo,
    pub(crate) execute: Value,
    pub(crate) signal: Option<u32>,
}

/// What the agent sees of a tool (no function).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolInfo {
    /// A new id per registration (a tool registered again under its name gets another).
    #[serde(default)]
    pub id: u64,
    /// The form of a declarative tool (`<form toolname>`).
    #[serde(skip)]
    pub form: Option<pageon_dom::NodeId>,
    /// A declarative tool whose form submits itself when called (`toolautosubmit`).
    #[serde(default)]
    pub autosubmit: bool,
    /// Name.
    pub name: String,
    /// Title, if given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description (page text).
    pub description: String,
    /// `inputSchema` as JSON text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<String>,
    /// `outputSchema` as JSON text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<String>,
    /// `readOnlyHint`.
    #[serde(default)]
    pub read_only: bool,
    /// `consequentialHint`.
    #[serde(default)]
    pub consequential: bool,
    /// `untrustedContentHint`.
    #[serde(default)]
    pub untrusted_content: bool,
    /// The registering document's origin (engine fact).
    pub origin: String,
    /// The registering script: its URL, `inline`, or `callback` (engine fact).
    pub script: String,
    /// Registered by a cross-origin frame (engine fact).
    #[serde(default)]
    pub third_party: bool,
}

fn valid_name(n: &str) -> bool {
    (1..=128).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn stringify(cx: &mut Cx<'_, '_, '_>, v: &Option<Value>) -> R<Option<String>> {
    let Some(v) = v else { return Ok(None) };
    let l = cx.local(v);
    if l.is_null_or_undefined() {
        return Ok(None);
    }
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    match v8::json::stringify(tc, l) {
        Some(s) => Ok(Some(s.to_rust_string_lossy(tc))),
        None => match tc.exception() {
            Some(e) => Err(Error::Thrown(v8::Global::new(tc, e))),
            None => Err(Error::Type("the schema cannot be serialized".into())),
        },
    }
}

/// The registering script, as the engine knows it.
fn script(cx: &mut Cx<'_, '_, '_>) -> String {
    let st = cx.st();
    let Some(node) = st.current_script else {
        return "callback".into();
    };
    let base = url::Url::parse(&st.url).ok();
    drop(st);
    let src = cx
        .realm
        .dom
        .borrow()
        .get(node)
        .and_then(|n| n.attr("src").map(str::to_string));
    match src {
        Some(s) => base
            .and_then(|b| b.join(s.trim()).ok())
            .map_or(s, |u| u.to_string()),
        None => "inline".into(),
    }
}

fn next_id(cx: &Cx<'_, '_, '_>) -> u64 {
    let mut sh = cx.realm.shared.borrow_mut();
    sh.tool_seq += 1;
    sh.tool_seq
}

/// Most form tools listed per document.
pub(crate) const MAX_FORM_TOOLS: usize = 64;
/// Most properties of one form tool.
const MAX_PROPERTIES: usize = 100;

/// The form tools of a realm's document (WebMCP's declarative API): every form with a valid
/// `toolname` and a `tooldescription`, under `registerTool`'s rules (secure context, the
/// `tools` permissions-policy feature); a name an imperative tool holds is skipped. Each keeps
/// its id while its form keeps its name.
pub(crate) fn declared(realm: &crate::realm::Realm) -> Vec<ToolInfo> {
    let (allowed, origin, third_party) = {
        let st = realm.st.borrow();
        let secure = pageon_net::site::trustworthy(&st.client.url);
        (
            secure && st.client.tools,
            st.origin.clone(),
            st.client.third_party(),
        )
    };
    if !allowed {
        return Vec::new();
    }
    let dom = realm.dom.borrow();
    let mut taken: Vec<String> = realm
        .st
        .borrow()
        .tools
        .iter()
        .map(|t| t.info.name.clone())
        .collect();
    let mut out = Vec::new();
    let forms: Vec<pageon_dom::NodeId> = dom
        .traverse(dom.root())
        .filter_map(|e| match e {
            pageon_dom::Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("form")) => Some(n),
            _ => None,
        })
        .collect();
    for form in forms {
        if out.len() >= MAX_FORM_TOOLS {
            break;
        }
        let Some(node) = dom.get(form) else { continue };
        let (Some(name), Some(description)) = (node.attr("toolname"), node.attr("tooldescription"))
        else {
            continue;
        };
        let (name, description) = (name.trim().to_string(), description.trim().to_string());
        if !valid_name(&name) || description.is_empty() || taken.contains(&name) {
            continue;
        }
        taken.push(name.clone());
        let autosubmit = node.attr("toolautosubmit").is_some();
        let id = {
            let mut st = realm.st.borrow_mut();
            match st
                .declared
                .iter()
                .find(|(f, n, _)| *f == form && *n == name)
            {
                Some(&(_, _, id)) => id,
                None => {
                    let id = {
                        let mut sh = realm.shared.borrow_mut();
                        sh.tool_seq += 1;
                        sh.tool_seq
                    };
                    st.declared.push((form, name.clone(), id));
                    id
                }
            }
        };
        out.push(ToolInfo {
            id,
            form: Some(form),
            autosubmit,
            name,
            title: None,
            description,
            input_schema: Some(schema(&dom, form).to_string()),
            output_schema: None,
            read_only: false,
            consequential: false,
            untrusted_content: false,
            origin: origin.clone(),
            script: "form".into(),
            third_party,
        });
    }
    // Forget the ids of forms that are no longer tools under that name.
    let live: Vec<(pageon_dom::NodeId, String)> = out
        .iter()
        .filter_map(|t| t.form.map(|f| (f, t.name.clone())))
        .collect();
    realm
        .st
        .borrow_mut()
        .declared
        .retain(|(f, n, _)| live.iter().any(|(lf, ln)| lf == f && ln == n));
    out
}

/// The controls of a form that are tool parameters, in tree order: (name, node).
pub(crate) fn parameters(
    dom: &pageon_dom::Dom,
    form: pageon_dom::NodeId,
) -> Vec<(String, pageon_dom::NodeId)> {
    let form_id = dom.get(form).and_then(|n| n.attr("id")).map(str::to_string);
    let mut out: Vec<(String, pageon_dom::NodeId)> = Vec::new();
    for e in dom.traverse(dom.root()) {
        let pageon_dom::Edge::Enter(n) = e else {
            continue;
        };
        let Some(node) = dom.get(n) else { continue };
        if !(node.is_html("input") || node.is_html("select") || node.is_html("textarea")) {
            continue;
        }
        // Its form: the `form` attribute, else the nearest form ancestor.
        let owner = match node.attr("form") {
            Some(f) => form_id.as_deref() == Some(f),
            None => {
                dom.ancestors(n)
                    .find(|&a| dom.get(a).is_some_and(|x| x.is_html("form")))
                    == Some(form)
            }
        };
        let Some(name) = node.attr("name").filter(|s| !s.is_empty()) else {
            continue;
        };
        let ty = node.attr("type").unwrap_or("text").to_ascii_lowercase();
        let excluded = node.is_html("input")
            && matches!(
                ty.as_str(),
                "submit" | "reset" | "button" | "image" | "hidden" | "file"
            );
        if !owner || excluded || node.attr("disabled").is_some() {
            continue;
        }
        // A radio group is one parameter.
        if out.iter().any(|(m, _)| m == name) {
            continue;
        }
        out.push((name.to_string(), n));
        if out.len() >= MAX_PROPERTIES {
            break;
        }
    }
    out
}

fn text_of(dom: &pageon_dom::Dom, n: pageon_dom::NodeId) -> String {
    let mut s = String::new();
    for e in dom.traverse(n) {
        if let pageon_dom::Edge::Enter(t) = e
            && let Some(pageon_dom::NodeData::Text(x)) = dom.get(t).map(|x| &x.data)
        {
            s.push_str(x);
            s.push(' ');
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A control's description: `toolparamdescription`, else its label's text.
fn described(dom: &pageon_dom::Dom, n: pageon_dom::NodeId) -> Option<String> {
    let node = dom.get(n)?;
    if let Some(d) = node.attr("toolparamdescription") {
        return Some(d.trim().to_string()).filter(|d| !d.is_empty());
    }
    let label = node
        .attr("id")
        .and_then(|id| {
            dom.traverse(dom.root()).find_map(|e| match e {
                pageon_dom::Edge::Enter(l)
                    if dom
                        .get(l)
                        .is_some_and(|x| x.is_html("label") && x.attr("for") == Some(id)) =>
                {
                    Some(l)
                }
                _ => None,
            })
        })
        .or_else(|| {
            dom.ancestors(n)
                .find(|&a| dom.get(a).is_some_and(|x| x.is_html("label")))
        })?;
    Some(text_of(dom, label)).filter(|t| !t.is_empty())
}

/// The options of a `<select>` or a radio group: (value, visible text).
fn choices(
    dom: &pageon_dom::Dom,
    form: pageon_dom::NodeId,
    n: pageon_dom::NodeId,
) -> Vec<(String, String)> {
    let Some(node) = dom.get(n) else {
        return Vec::new();
    };
    if node.is_html("select") {
        return dom
            .traverse(n)
            .filter_map(|e| match e {
                pageon_dom::Edge::Enter(o) if dom.get(o).is_some_and(|x| x.is_html("option")) => {
                    let text = text_of(dom, o);
                    let value = dom
                        .get(o)?
                        .attr("value")
                        .map_or_else(|| text.clone(), str::to_string);
                    Some((value, text))
                }
                _ => None,
            })
            .collect();
    }
    let name = node.attr("name").unwrap_or_default().to_string();
    dom.traverse(form)
        .filter_map(|e| match e {
            pageon_dom::Edge::Enter(r) => {
                let x = dom.get(r)?;
                let radio = x.is_html("input")
                    && x.attr("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("radio"))
                    && x.attr("name") == Some(name.as_str());
                radio.then(|| {
                    let value = x.attr("value").unwrap_or("on").to_string();
                    (value.clone(), described(dom, r).unwrap_or(value))
                })
            }
            _ => None,
        })
        .collect()
}

/// The JSON Schema of a form tool's input, from its controls.
pub(crate) fn schema(dom: &pageon_dom::Dom, form: pageon_dom::NodeId) -> serde_json::Value {
    use serde_json::{Value as J, json};
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, n) in parameters(dom, form) {
        let Some(node) = dom.get(n) else { continue };
        let ty = node.attr("type").unwrap_or("text").to_ascii_lowercase();
        let num = |a: &str| node.attr(a).and_then(|v| v.trim().parse::<f64>().ok());
        let mut p = if node.is_html("select") || (node.is_html("input") && ty == "radio") {
            let opts = choices(dom, form, n);
            let one = json!({
                "type": "string",
                "enum": opts.iter().map(|(v, _)| J::from(v.as_str())).collect::<Vec<_>>(),
                "anyOf": opts.iter().map(|(v, t)| json!({"const": v, "title": t})).collect::<Vec<_>>(),
            });
            if node.is_html("select") && node.attr("multiple").is_some() {
                json!({"type": "array", "items": one})
            } else {
                one
            }
        } else if node.is_html("input") && ty == "checkbox" {
            json!({"type": "boolean"})
        } else if node.is_html("input") && matches!(ty.as_str(), "number" | "range") {
            let mut p = json!({"type": "number"});
            if let Some(v) = num("min") {
                p["minimum"] = json!(v);
            }
            if let Some(v) = num("max") {
                p["maximum"] = json!(v);
            }
            p
        } else {
            json!({"type": "string"})
        };
        if let Some(d) = described(dom, n) {
            p["description"] = json!(d);
        }
        if node.attr("required").is_some() {
            required.push(J::from(name.as_str()));
        }
        properties.insert(name, p);
    }
    let mut s = json!({"type": "object", "properties": properties});
    if !required.is_empty() {
        s["required"] = J::Array(required);
    }
    s
}

/// `document.modelContext`.
pub(crate) fn model_context_of(cx: &mut Cx<'_, '_, '_>) -> Obj {
    super::window_same(cx, "modelContext", Iface::ModelContext, Slot::ModelContext)
}

fn changed(cx: &mut Cx<'_, '_, '_>) -> R<()> {
    let Obj::Slab(id) = model_context_of(cx) else {
        return Ok(());
    };
    super::event_target::fire(cx, Target::Object(id), "toolchange", false, false).map(|_| ())
}

/// Unregisters the tools a signal guarded (spec: aborting the signal unregisters).
pub(crate) fn signal_aborted(cx: &mut Cx<'_, '_, '_>, signal: u32) -> R<()> {
    let removed = {
        let mut st = cx.st();
        let before = st.tools.len();
        st.tools.retain(|t| t.signal != Some(signal));
        before != st.tools.len()
    };
    if removed {
        changed(cx)?;
    }
    Ok(())
}

/// `ModelContext`.
pub mod model_context {
    use super::*;

    pub fn register_tool(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        tool: ModelContextTool,
        options: ModelContextRegisterToolOptions,
    ) -> R<Value> {
        let r = register(cx, tool, options);
        super::super::net::settled_unit(cx, r)
    }

    fn register(
        cx: &mut Cx<'_, '_, '_>,
        tool: ModelContextTool,
        options: ModelContextRegisterToolOptions,
    ) -> R<()> {
        if !cx.st().client.tools {
            return Err(Error::Dom(
                "NotAllowedError",
                "the \"tools\" permissions-policy feature is not allowed in this document".into(),
            ));
        }
        let name = tool
            .name
            .ok_or_else(|| Error::Type("registerTool: the tool needs a name".into()))?;
        let description = tool
            .description
            .ok_or_else(|| Error::Type("registerTool: the tool needs a description".into()))?;
        let execute = tool
            .execute
            .ok_or_else(|| Error::Type("registerTool: the tool needs execute".into()))?;
        if !cx.local(&execute).is_function() {
            return Err(Error::Type(
                "registerTool: execute is not a function".into(),
            ));
        }
        if !valid_name(&name) {
            return Err(Error::Dom(
                "InvalidStateError",
                format!("{name:?} is not a valid tool name (1–128 of A–Z, a–z, 0–9, _ - .)"),
            ));
        }
        if description.is_empty() {
            return Err(Error::Dom(
                "InvalidStateError",
                "the description is empty".into(),
            ));
        }
        for o in options.exposed_to.iter().flatten() {
            let ok = url::Url::parse(o).is_ok_and(|u| pageon_net::site::trustworthy(&u));
            if !ok {
                return Err(Error::Dom(
                    "SecurityError",
                    format!("{o:?} is not a potentially trustworthy origin"),
                ));
            }
        }
        let input_schema = stringify(cx, &tool.input_schema)?;
        let output_schema = stringify(cx, &tool.output_schema)?;
        let signal = match options.signal {
            Some(Obj::Slab(id)) => Some(id),
            _ => None,
        };
        if let Some(id) = signal
            && matches!(cx.realm.st.borrow().slot(id), Some(Slot::Signal(d)) if d.is_aborted())
        {
            return Err(Error::Dom("AbortError", "the signal is aborted".into()));
        }
        // Re-checked after serialization: a schema's `toJSON` may have registered the name.
        if cx.st().tools.iter().any(|t| t.info.name == name) {
            return Err(Error::Dom(
                "InvalidStateError",
                format!("a tool named {name:?} is already registered"),
            ));
        }
        let a = tool.annotations.unwrap_or_default();
        let st = cx.st();
        let origin = st.origin.clone();
        let third_party = st.client.third_party();
        drop(st);
        let script = script(cx);
        let id = next_id(cx);
        cx.st().tools.push(Tool {
            info: ToolInfo {
                id,
                form: None,
                autosubmit: false,
                name,
                title: tool.title,
                description,
                input_schema,
                output_schema,
                read_only: a.read_only_hint,
                consequential: a.consequential_hint,
                untrusted_content: a.untrusted_content_hint,
                origin,
                script,
                third_party,
            },
            execute,
            signal,
        });
        changed(cx)
    }
}

/// Fires `toolactivated` at `document.modelContext`: a form tool's fields were filled.
pub(crate) fn activated(cx: &mut Cx<'_, '_, '_>, name: &str) -> R<()> {
    let Obj::Slab(id) = model_context_of(cx) else {
        return Ok(());
    };
    let data = EventData {
        ty: "toolactivated".into(),
        initialized: true,
        trusted: true,
        time: super::events::now(cx),
        tool: name.to_string(),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::ToolActivatedEvent, Slot::Event(Box::new(data)));
    super::event_target::dispatch(cx, Target::Object(id), e).map(|_| ())
}

fn tool_event(
    cx: &mut Cx<'_, '_, '_>,
    iface: Iface,
    ty: String,
    init: (bool, bool, bool, String),
) -> Obj {
    let (bubbles, cancelable, composed, tool) = init;
    let data = EventData {
        ty,
        bubbles,
        cancelable,
        composed,
        initialized: true,
        time: super::events::now(cx),
        tool,
        ..EventData::default()
    };
    cx.alloc(iface, Slot::Event(Box::new(data)))
}

fn tool_name(cx: &Cx<'_, '_, '_>, this: Obj) -> R<String> {
    let id = this.slab()?;
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::Event(e)) => Ok(e.tool.clone()),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// `ToolActivatedEvent`.
pub mod tool_activated_event {
    use super::*;

    pub fn constructor(
        cx: &mut Cx<'_, '_, '_>,
        ty: String,
        init: ToolActivatedEventInit,
    ) -> R<Obj> {
        Ok(tool_event(
            cx,
            Iface::ToolActivatedEvent,
            ty,
            (init.bubbles, init.cancelable, init.composed, init.tool_name),
        ))
    }

    pub fn get_tool_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        tool_name(cx, this)
    }
}

/// `ToolCancelEvent`.
pub mod tool_cancel_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: ToolCancelEventInit) -> R<Obj> {
        Ok(tool_event(
            cx,
            Iface::ToolCancelEvent,
            ty,
            (init.bubbles, init.cancelable, init.composed, init.tool_name),
        ))
    }

    pub fn get_tool_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        tool_name(cx, this)
    }
}
