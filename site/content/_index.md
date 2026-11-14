+++
title = "pageon: the browser engine built for AI agents"
description = "pageon is an open-source browser engine in Rust, built for AI agents: compact page views with stable refs, typed actions, sandboxed pages, replay and fork. MCP, Python, Puppeteer, Playwright and WebDriver BiDi."
template = "index.html"

[extra]
lead = "Its own engine in Rust, no Chromium. pageon shows a model the page in a fraction of the tokens, acts on stable refs with typed outcomes, keeps the page in a sandbox, and lets agents replay, rewind and look ahead."

works_with = ["Claude Code", "Claude Desktop", "Cursor", "any MCP host", "Python", "Puppeteer", "Playwright", "WebDriver BiDi", "BrowserGym"]

why_title = "Everything an agent needs from a browser, and nothing it does not"
why_lead = "Agents drive Chromium through tools built for testing and debugging. pageon is a browser engine whose output is made for a model, and whose safety does not depend on the model behaving."

numbers_lead = "Observation formats compared on MiniWoB++ with the same model, prompt, budget and actions; engine figures from the test suite. Every cell is reported, including the ones pageon loses."

safety_lead = "Prompt injection is a when, not an if. pageon enforces safety in the engine and the broker, outside the model's reach."

safety = [
  "Pages run in a **sandboxed process**: no files, no network, no programs. JavaScript runs.",
  "**Origin sets** decide where requests may go; writes can wait for a person or a guard program.",
  "**Secrets** are typed as `secret://name`, never shown in views, screenshots or recordings, and refused at egress for any origin they are not bound to.",
  "All page text is **quoted**; hidden text, input traps and third-party frames are **marked**.",
  "pageon is **honest about itself**: `navigator.webdriver` is true, robots.txt is honoured, and it never solves CAPTCHAs.",
]

[[extra.features]]
icon = "🔍"
title = "Views made for models"
text = "A compact outline of what an agent can read and act on, cut to a token budget that names its tokenizer. On real pages it uses a quarter of the tokens of Playwright MCP's snapshot, with at least the same task success."
link = "@/docs/views.md"

[[extra.features]]
icon = "🎯"
title = "Refs that stay put"
text = "`e12` names the same element across steps. Refs are never written into the page, and when the page replaces an element its ref is relocated and the relocation reported."
link = "@/docs/views.md"

[[extra.features]]
icon = "⚡"
title = "Actions that tell the truth"
text = "`click e12` checks actionability against real layout, settles on virtual time, and returns what happened: navigations, dialogs, downloads, requests, errors. Failures say what to do next."
link = "@/docs/actions.md"

[[extra.features]]
icon = "🛡️"
title = "Sandboxed by default"
text = "Every page runs in a caged content process. The broker owns the network, the origin policy, the secrets vault and an optional guard program in any language."
link = "@/docs/security.md"

[[extra.features]]
icon = "⏪"
title = "Replay, rewind, look ahead"
text = "Sessions record and replay byte for byte. Agents go back to any step without re-sending anything, and try actions on a fork of the page before committing."
link = "@/docs/replay.md"

[[extra.features]]
icon = "🧩"
title = "Workflows and WebMCP"
text = "A solved task compiles into a workflow that reruns without a model and stops where a site drifted. Tools a page declares with WebMCP are listed first, with their provenance."
link = "@/docs/workflows.md"

[[extra.connect]]
name = "MCP"
text = "Add pageon to Claude Code, Claude Desktop, Cursor or any MCP host. Eighteen `browser_*` tools; every action answers with its outcome, what changed and its step number."
link = "@/docs/mcp.md"
code = '''
```sh
claude mcp add pageon -- pageon mcp
claude mcp add shop -- pageon mcp --origin https://shop.example --secret shop@https://shop.example
```
'''

[[extra.connect]]
name = "Python"
text = "`pageon.Browser` drives the engine as a child process. Typed outcomes, `ActionError` with guidance, screenshots, reset and lookahead."
link = "@/docs/python.md"
code = '''
```python
from pageon import Browser

with Browser(origins=["https://shop.example"]) as browser:
    tab = browser.open("https://shop.example/login")
    print(tab.observe())
    out = tab.type("e4", "secret://shop", submit=True)
    print(out.observation)
```
'''

[[extra.connect]]
name = "Puppeteer & Playwright"
text = "`pageon cdp` is a DevTools endpoint that existing Puppeteer and Playwright code connects to unchanged, with the page still in the sandbox."
link = "@/docs/cdp.md"
code = '''
```js
const browser = await puppeteer.connect({ browserURL: 'http://127.0.0.1:9222' });
const pw = await chromium.connectOverCDP('http://127.0.0.1:9222');
```
'''

[[extra.connect]]
name = "WebDriver BiDi"
text = "The W3C protocol on the same port, at `/session`. Puppeteer's BiDi mode and plain BiDi clients work; `pageon:observe` and `pageon:act` add the agent view."
link = "@/docs/bidi.md"
code = '''
```js
const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222/session',
  protocol: 'webDriverBiDi',
});
```
'''

[[extra.connect]]
name = "Rust"
text = "Embed the engine: sessions, tabs, typed actions and outcomes, recordings, the sandbox and the policy as a library."
link = "@/docs/rust.md"
code = '''
```rust
let session = Session::new(Live::default());
let mut tab = session.open("https://example.com", LoadOptions::default())?;
println!("{}", tab.observe());
println!("{}", tab.act("click e1".parse()?)?);
```
'''

[[extra.stats]]
value = "0.27×"
label = "the tokens of Playwright MCP on real pages"

[[extra.stats]]
value = "+18.7"
label = "points of task success over browser-use 1.0"

[[extra.stats]]
value = "2.2 ms"
label = "a click in the sandbox, end to end"

[[extra.stats]]
value = "1,787"
label = "of 1,792 html5lib parser tests"

[[extra.bench]]
vs = "Playwright MCP snapshot"
diff = "+3.2 points"
ci = "[−3.2, +9.9]"
tokens = "0.27×"

[[extra.bench]]
vs = "BrowserGym AXTree"
diff = "+8.3 points"
ci = "[+1.9, +14.9]"
tokens = "0.16×"

[[extra.bench]]
vs = "browser-use 1.0"
diff = "+18.7 points"
ci = "[+12.8, +24.8]"
tokens = "1.02×"

[[extra.faq]]
q = "Is pageon a wrapper around Chrome?"
a = "No. pageon has its own engine: html5ever parses, V8 runs JavaScript through bindings generated from the WebIDL standards, Stylo and Taffy style and lay out, and pageon owns the DOM, the event loop, networking, the accessibility tree and the agent views. No Chromium is involved."

[[extra.faq]]
q = "Which agents and frameworks can use it?"
a = "Any MCP host (Claude Code, Claude Desktop, Cursor and others), Python, existing Puppeteer and Playwright code over CDP, WebDriver BiDi clients, BrowserGym and AgentLab, and Rust programs."

[[extra.faq]]
q = "How does pageon keep a prompt-injected agent from doing damage?"
a = "Pages run in a sandboxed process. The broker enforces origin sets, holds writes for an approver or a guard program, keeps secrets out of every view and refuses them at egress outside their origin, and quotes all page text so a page cannot forge the view."

[[extra.faq]]
q = "Does pageon solve CAPTCHAs or hide that it is automated?"
a = "No. By default pageon says it is an agent, honours robots.txt and can sign every request with Web Bot Auth. When a site refuses it, the outcome says so; a person can take over through a live view."

[[extra.faq]]
q = "What does it cost?"
a = "Nothing. pageon is open source under MIT OR Apache-2.0, and the benchmark runs on a local model at no cost."
+++
