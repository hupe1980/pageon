//! Chrome's `WebMCP` domain: the page's tools (registered and declared by forms) for automation
//! clients, as Puppeteer's `page.webmcp` reads them. A call runs as the agent's tool call does
//! (an action: the egress policy, labels, guard and recording apply).

use serde_json::{Value as Json, json};

use super::{CdpError, Reply, Session};
use crate::Action;
use pageon_env::Env;

impl<E: Env> Session<'_, E> {
    /// `WebMCP.*`.
    pub(super) fn webmcp_method(&mut self, ti: usize, method: &str, params: &Json) -> Reply {
        match method {
            "WebMCP.enable" => {
                self.targets[ti].webmcp = true;
                self.targets[ti].tools_seen.clear();
                self.webmcp_sync(ti);
                Ok(json!({}))
            }
            "WebMCP.disable" => {
                self.targets[ti].webmcp = false;
                self.targets[ti].tools_seen.clear();
                Ok(json!({}))
            }
            "WebMCP.invokeTool" => self.invoke_tool(ti, params),
            "WebMCP.cancelInvocation" => Err(CdpError::server(
                "No pending invocation with given id (pageon answers every call before replying)",
            )),
            m => Err(super::unknown(m)),
        }
    }

    /// The page's tools as CDP `Tool`s, with pageon's frame index and tool id.
    fn cdp_tools(&mut self, ti: usize) -> Vec<(u64, String, usize, Json)> {
        let tools = self.targets[ti].tab.tools();
        let mut out = Vec::new();
        for (frame, t) in tools {
            let Some(frame_id) = self.targets[ti].frames.get(frame).map(|f| f.id.clone()) else {
                continue;
            };
            let schema = |s: &Option<String>| {
                s.as_deref()
                    .and_then(|s| serde_json::from_str::<Json>(s).ok())
            };
            let mut tool = json!({
                "toolId": t.id.to_string(),
                "name": t.name,
                "description": t.description,
                "annotations": {
                    "readOnly": t.read_only,
                    "untrustedContent": t.untrusted_content,
                    "consequential": t.consequential,
                    "autosubmit": t.autosubmit,
                },
                "frameId": frame_id,
            });
            if let Some(s) = schema(&t.input_schema) {
                tool["inputSchema"] = s;
            }
            if let Some(s) = schema(&t.output_schema) {
                tool["outputSchema"] = s;
            }
            if let Some(form) = t.form {
                tool["backendNodeId"] = json!(self.backend_id(ti, frame, form));
            }
            out.push((t.id, t.name, frame, tool));
        }
        out
    }

    /// Reports tools added and removed since the last report, when the domain is enabled.
    pub(super) fn webmcp_sync(&mut self, ti: usize) {
        if !self.targets[ti].webmcp {
            return;
        }
        let now = self.cdp_tools(ti);
        let seen = std::mem::take(&mut self.targets[ti].tools_seen);
        let removed: Vec<Json> = seen
            .iter()
            .filter(|(id, ..)| !now.iter().any(|(n, ..)| n == id))
            .map(|(id, name, frame)| {
                let frame_id = self.targets[ti]
                    .frames
                    .get(*frame)
                    .map(|f| f.id.clone())
                    .unwrap_or_default();
                json!({"toolId": id.to_string(), "name": name, "frameId": frame_id})
            })
            .collect();
        let added: Vec<Json> = now
            .iter()
            .filter(|(id, ..)| !seen.iter().any(|(s, ..)| s == id))
            .map(|(.., t)| t.clone())
            .collect();
        self.targets[ti].tools_seen = now
            .into_iter()
            .map(|(id, name, frame, _)| (id, name, frame))
            .collect();
        if !removed.is_empty() {
            self.target_event(ti, "WebMCP.toolsRemoved", json!({"tools": removed}));
        }
        if !added.is_empty() {
            self.target_event(ti, "WebMCP.toolsAdded", json!({"tools": added}));
        }
    }

    /// `WebMCP.invokeTool`: answers the invocation id, then `toolInvoked` and `toolResponded`.
    fn invoke_tool(&mut self, ti: usize, params: &Json) -> Reply {
        let by_id = params.get("toolId").and_then(Json::as_str);
        let by_name = params.get("toolName").and_then(Json::as_str);
        let frame_id = params
            .get("frameId")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        if by_id.is_some() == by_name.is_some() {
            return Err(CdpError::params(
                "exactly one of toolId and toolName must be given",
            ));
        }
        let input = params.get("input").cloned().unwrap_or_else(|| json!({}));
        let tools = self.cdp_tools(ti);
        let found = tools.iter().find(|(id, name, frame, _)| {
            let here = self.targets[ti]
                .frames
                .get(*frame)
                .is_some_and(|f| f.id == frame_id);
            here && match (by_id, by_name) {
                (Some(i), _) => id.to_string() == i,
                (_, Some(n)) => name == n,
                _ => false,
            }
        });
        self.next_request += 1;
        let invocation = self.next_request.to_string();
        let (tool_id, tool_name, origin) = match found {
            Some((id, name, frame, _)) => {
                let origin = self.targets[ti]
                    .tab
                    .tools()
                    .into_iter()
                    .find(|(f, t)| f == frame && t.id == *id)
                    .map(|(_, t)| t.origin);
                (Some(id.to_string()), Some(name.clone()), origin)
            }
            None => (None, by_name.map(str::to_string), None),
        };
        let mut invoked = json!({
            "frameId": frame_id,
            "invocationId": invocation,
            "input": input.to_string(),
        });
        if let Some(n) = &tool_name {
            invoked["toolName"] = json!(n);
        }
        if let Some(i) = &tool_id {
            invoked["toolId"] = json!(i);
        }
        let responded = match (&tool_id, &tool_name) {
            (Some(_), Some(name)) => {
                let action = Action::Tool {
                    name: name.clone(),
                    origin,
                    input: input.clone(),
                };
                match self.targets[ti].tab.act(action) {
                    Ok(out) => {
                        self.after_outcome(ti, &out);
                        let output = out
                            .tool_result
                            .as_deref()
                            .map(|r| serde_json::from_str::<Json>(r).unwrap_or_else(|_| json!(r)))
                            .unwrap_or(Json::Null);
                        json!({"invocationId": invocation, "status": "Completed", "output": output})
                    }
                    Err(e) => json!({
                        "invocationId": invocation, "status": "Error", "errorText": e.to_string(),
                    }),
                }
            }
            _ => json!({
                "invocationId": invocation, "status": "Error",
                "errorText": "No tool with given id or name in the given frame",
            }),
        };
        // The reply comes first (Chrome: "Response is sent before tool events").
        let out = std::mem::take(&mut self.out);
        self.target_event(ti, "WebMCP.toolInvoked", invoked);
        self.target_event(ti, "WebMCP.toolResponded", responded);
        let events = std::mem::replace(&mut self.out, out);
        self.after_reply.extend(events);
        Ok(json!({"invocationId": invocation}))
    }
}
