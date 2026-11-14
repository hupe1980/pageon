//! WebMCP: `document.modelContext.registerTool`. Each tool keeps provenance the
//! engine records at registration (origin, registering script, third-party frame), which the
//! page cannot change; the agent side reads the registry through [`crate::Engine::tools`].

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{Iface, ModelContextRegisterToolOptions, ModelContextTool};
use crate::realm::{Slot, Target};

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
        cx.st().tools.push(Tool {
            info: ToolInfo {
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
