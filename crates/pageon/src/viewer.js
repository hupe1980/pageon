// The viewer page. Everything a page wrote (text, titles, URLs, target lines) is set with
// textContent, never as markup.
"use strict";

const meta = (name) => document.querySelector(`meta[name="${name}"]`).content;
const base = meta("pageon-base");
const token = meta("pageon-token");
const $ = (id) => document.getElementById(id);

const steps = [];          // as recorded
let selected = -1;
let following = true;      // keep the newest step selected
let view = "after";        // before | after | read
let paused = false;
let lastStepAt = 0;
const waiting = new Map(); // id -> approval, oldest first
let focusBefore = null;    // where focus returns when the alert closes

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

async function post(path, body) {
  const r = await fetch(base + path, {
    method: "POST",
    headers: { "x-pageon-token": token, "content-type": "application/json" },
    body: body ? JSON.stringify(body) : "",
  });
  if (!r.ok) $("state").textContent = await r.text();
}

// ---- Steps as sentences -------------------------------------------------------------------

const q = (s) => `“${s}”`;

// What the model was shown for a ref: the latest view line naming it, e.g. `button e3 "Pay"`.
function named(ref, upTo) {
  if (!ref) return "";
  const line = new RegExp(`(?:^|\\n)[~+ ]*[a-z]+ ${ref} "((?:[^"\\\\]|\\\\.)*)"`);
  for (let i = upTo; i >= 0; i--) {
    const m = line.exec(steps[i].text || "");
    if (m && m[1]) return q(m[1].replace(/\\"/g, '"'));
  }
  return ref;
}

function hostPath(url) {
  try {
    const u = new URL(url);
    return u.host + (u.pathname === "/" ? "" : u.pathname);
  } catch {
    return url || "a page";
  }
}

function sentence(i) {
  const base = action(i);
  const first = (steps[i].text || "").split("\n", 1)[0];
  return /^signed out of .*; signed in again/.test(first) ? `${base}, after signing in again` : base;
}

function action(i) {
  const s = steps[i];
  const a = s.input || {};
  const t = (r) => named(r, i - 1);
  const tool = s.tool.replace(/^browser_/, "");
  switch (tool) {
    case "open": return `Opened ${hostPath(a.url)}`;
    case "click": return `Clicked ${t(a.ref)}`;
    case "type": {
      const text = String(a.text || "");
      const what = text.startsWith("secret://") ? `secret ${q(text.slice(9))}` : q(text);
      return `Typed ${what} into ${t(a.ref)}${a.submit ? " and pressed Return" : ""}`;
    }
    case "press": return `Pressed ${a.key}`;
    case "select": return `Chose ${(a.values || []).map(q).join(", ")} in ${t(a.ref)}`;
    case "paste": return a.ref ? `Pasted into ${t(a.ref)}` : "Pasted";
    case "hover": return `Pointed at ${t(a.ref)}`;
    case "drag": return `Dragged ${t(a.ref)} onto ${t(a.to)}`;
    case "scroll": return a.ref && !a.direction ? `Scrolled to ${t(a.ref)}` : `Scrolled ${a.direction || "down"}`;
    case "wait": return `Waited ${a.ms} ms`;
    case "back": return "Went back";
    case "forward": return "Went forward";
    case "upload": return `Uploaded ${(a.files || []).map(q).join(", ")} to ${t(a.ref)}`;
    case "observe": return "Looked at the page";
    case "view": return `Read the page as ${a.kind}`;
    case "find": return `Searched for ${q(a.query || "")}`;
    case "screenshot": return "Took a picture of the page";
    case "tabs": return a.action === "new" ? `Opened ${hostPath(a.url)} in a new tab` : `Tabs: ${a.action}`;
    case "reset": return a.step === undefined ? "Undid the last step" : `Went back to step ${a.step}`;
    case "lookahead": return `Tried ${(a.actions || []).join(", ")} on a copy`;
    case "call_tool": return `Used the page’s tool ${q(a.name || "")}`;
    case "takeover": return "Asked you to take over";
    case "workflow_run": return "Ran a saved workflow";
    case "workflow_save": return "Saved the session as a workflow";
    default: return tool;
  }
}

function took(ms) {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

function detailLine(s) {
  const parts = [];
  if (!s.ok) parts.push("Failed");
  if (s.step !== null && s.step !== undefined) parts.push(`Step ${s.step}`);
  parts.push(took(s.ms));
  if (s.tokens) parts.push(`${s.tokens.toLocaleString("en")} tokens`);
  return parts.join(" · ");
}

function addStep(s) {
  steps.push(s);
  lastStepAt = Date.now();
  const i = steps.length - 1;
  const li = el("li", s.ok ? "" : "failed");
  const b = el("button");
  b.type = "button";
  b.append(el("span", "what", sentence(i)), el("span", "when", detailLine(s)));
  b.addEventListener("click", () => select(i, false));
  li.append(b);
  $("steps").append(li);
  $("welcome").hidden = true;
  if (following) select(i, true);
  island();
}

function setUrl(url) {
  const box = $("winUrl");
  box.replaceChildren();
  if (!url) return;
  try {
    const u = new URL(url);
    box.append(el("b", "", u.host), u.pathname === "/" && !u.search ? "" : u.pathname + u.search);
  } catch {
    box.append(url);
  }
}

// The step whose picture shows the page before step i: the latest earlier one with a picture.
function pictured(i) {
  if (view === "after") return steps[i].image ? steps[i] : null;
  for (let k = i - 1; k >= 0; k--) if (steps[k].image) return steps[k];
  return null;
}

function paint() {
  if (selected < 0) return;
  const s = steps[selected];
  const img = $("shot");
  const read = view === "read";
  $("text").hidden = !read;
  $("text").textContent = s.text;
  const p = read ? null : pictured(selected);
  if (p) {
    img.src = `${base}/shot/${p.n}.jpg`;
    img.alt = view === "before" ? "The page before this step" : "The page after this step";
  } else {
    img.removeAttribute("src");
  }
  img.hidden = !p;
  $("noshot").hidden = read || !!p;
  const url = view === "before" ? steps[selected - 1]?.url : s.url;
  setUrl(url || s.url);
  for (const [id, v] of [["viewBefore", "before"], ["viewAfter", "after"], ["viewRead", "read"]]) {
    $(id).setAttribute("aria-selected", String(view === v));
  }
}

function select(i, follow) {
  if (i < 0 || i >= steps.length) return;
  selected = i;
  following = follow || i === steps.length - 1;
  const items = $("steps").children;
  for (let k = 0; k < items.length; k++) {
    items[k].firstChild.setAttribute("aria-current", String(k === i));
  }
  items[i].firstChild.scrollIntoView({ block: "nearest" });
  const s = steps[i];
  $("detail").hidden = false;
  $("sentence").textContent = sentence(i);
  const m = $("meta");
  m.replaceChildren();
  if (!s.ok) m.append(el("span", "bad", "Failed"), " · ");
  m.append(detailLine({ ...s, ok: true }) + (s.url ? ` · ${hostPath(s.url)}` : ""));
  const lines = (s.text || "").split("\n");
  const passed = lines.filter((l) => l.startsWith("✓ ")).length;
  const failed = lines.filter((l) => l.startsWith("✗ ")).length;
  if (passed + failed) {
    m.append(" · ");
    m.append(el("span", failed ? "bad" : "good", failed ? `${failed} of ${passed + failed} checks failed` : `${passed} check${passed === 1 ? "" : "s"} passed`));
  }
  paint();
}

function setView(v) {
  view = v;
  paint();
}

// ---- The island and its alert -------------------------------------------------------------

function plain(label) {
  if (label === "consequential") return ["The page’s own tool marks this as consequential.", true];
  if (label === "first-contact") return ["The first request to this site in the session.", false];
  if (label.startsWith("secret:")) return [`Carries your secret ${q(label.slice(7))}.`, true];
  if (label.startsWith("read-from:")) return [`The agent read ${label.slice(10)} before this.`, true];
  if (label.startsWith("tool:")) return [`Sent by the page’s tool ${q(label.slice(5))}.`, false];
  return [label, false];
}

// "clicking “Pay”" from the action (`click e3`) and its target line (`button e3 "Pay"`).
function cause(c) {
  const verbs = { click: "clicking", type: "typing into", press: "pressing", select: "choosing in",
    tool: "using the page’s tool", drag: "dragging", upload: "uploading to", paste: "pasting into" };
  const [act, arg] = c.action.split(" ");
  const m = /"((?:[^"\\]|\\.)*)"/.exec(c.target || "");
  return [verbs[act] || act, m ? q(m[1].replace(/\\"/g, '"')) : (arg || "")];
}

function showAlert() {
  const first = waiting.values().next().value;
  const alert = $("alert");
  const open = !!first;
  const wasOpen = !alert.hidden;
  alert.hidden = !open;
  $("scrim").hidden = !open;
  $("island").classList.toggle("open", open);
  document.title = open ? `(${waiting.size}) pageon` : "pageon";
  if (!open) {
    if (wasOpen) {
      if (focusBefore && focusBefore !== document.body) focusBefore.focus({ preventScroll: true });
      else document.activeElement.blur();
      focusBefore = null;
    }
    island();
    return;
  }
  const e = first.effect;
  let host = e.origin;
  try { host = new URL(e.origin).host; } catch { /* keep the origin */ }
  $("alertTitle").textContent = `Send a ${e.method} to ${host}?`;
  $("alertUrl").textContent = e.url;
  const facts = $("alertFacts");
  facts.replaceChildren();
  for (const l of e.labels || []) {
    const [text, strong] = plain(l);
    facts.append(el("li", strong ? "strong" : "", text));
  }
  if (!facts.children.length) facts.append(el("li", "", "It may change something on the site."));
  const c = $("alertCause");
  c.replaceChildren();
  if (e.cause) {
    const [verb, name] = cause(e.cause);
    c.append(`After ${verb} `, el("b", "", name));
    if (e.cause.target) c.append(el("small", "", "The name comes from the page."));
  } else {
    c.append("The page sent it on its own.");
  }
  $("session").title = `Later ${e.method} requests to ${host} go without asking until the session ends.`;
  $("queue").textContent = waiting.size > 1 ? `1 of ${waiting.size} waiting` : "";
  if (!wasOpen) {
    focusBefore = document.activeElement;
    $("once").focus();
  }
  island();
}

function decide(verdict) {
  const first = waiting.values().next().value;
  if (first) post("/decide", { id: first.id, verdict });
}

function island() {
  let state = "Idle", cls = "";
  if (waiting.size) { state = "Needs you"; cls = "needs"; }
  else if (paused) { state = "Paused"; cls = "paused"; }
  else if (Date.now() - lastStepAt < 4000) { state = "Working"; cls = "working"; }
  $("island").className = `island ${cls}${waiting.size ? " open" : ""}`;
  $("state").textContent = state;
  const tokens = steps.reduce((n, s) => n + (s.tokens || 0), 0);
  $("count").textContent = steps.length
    ? `${steps.length} step${steps.length === 1 ? "" : "s"} · ${tokens.toLocaleString("en")} tokens`
    : "";
}
setInterval(island, 1000);

function onStatus(s) {
  paused = s.paused;
  $("iconPause").hidden = paused;
  $("iconPlay").hidden = !paused;
  $("pause").setAttribute("aria-label", paused ? "Resume the agent" : "Pause the agent");
  $("pause").title = paused ? "Resume (Space)" : "Pause (Space)";
  island();
}

function onApproval(a) {
  if (a.state === "waiting") waiting.set(a.id, a);
  else waiting.delete(a.id);
  showAlert();
}

function connect() {
  const es = new EventSource(base + "/events");
  es.addEventListener("step", (m) => addStep(JSON.parse(m.data)));
  es.addEventListener("approval", (m) => onApproval(JSON.parse(m.data)));
  es.addEventListener("status", (m) => onStatus(JSON.parse(m.data)));
  es.onerror = () => {
    $("island").className = "island";
    $("state").textContent = "Session ended";
    es.close();
  };
}

$("once").addEventListener("click", () => decide("once"));
$("session").addEventListener("click", () => decide("session"));
$("deny").addEventListener("click", () => decide("deny"));
$("pause").addEventListener("click", () => post(paused ? "/resume" : "/pause"));
$("viewBefore").addEventListener("click", () => setView("before"));
$("viewAfter").addEventListener("click", () => setView("after"));
$("viewRead").addEventListener("click", () => setView("read"));

document.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  const deciding = !$("alert").hidden;
  switch (e.key) {
    case "a": if (deciding) decide("once"); break;
    case "g": if (deciding) decide("session"); break;
    case "d": case "Escape": if (deciding) decide("deny"); break;
    case "j": case "ArrowDown": if (!deciding) select(selected + 1, false); break;
    case "k": case "ArrowUp": if (!deciding) select(selected - 1, false); break;
    case "v": if (!deciding) setView({ before: "after", after: "read", read: "before" }[view]); break;
    case " ":
      if (document.activeElement && document.activeElement.tagName === "BUTTON") return;
      post(paused ? "/resume" : "/pause");
      break;
    default: return;
  }
  e.preventDefault();
});

connect();
