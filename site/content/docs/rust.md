+++
title = "Rust API"
description = "Embed pageon in Rust: sessions and tabs, typed actions and outcomes, recordings, the sandbox, the egress policy and the secrets vault."
weight = 15
[extra]
seo_title = "Rust API: embed the browser engine"
group = "Interfaces"
nav = "Rust API"
+++

The `pageon` crate is the engine as a library:

```toml
[dependencies]
pageon = { git = "https://github.com/hupe1980/pageon" }
```

## Sessions and tabs

```rust
use pageon::{Live, LoadOptions, Session};

let session = Session::new(Live::default());
let mut tab = session.open("https://example.com/login", LoadOptions::default())?;
println!("{}", tab.observe());                         // the agent view
let out = tab.act("type e3 ada".parse()?)?;            // Outcome
println!("{out}");                                     // ok type e3 "ada" · settled idle after 0 ms
match tab.act("click e99".parse()?) {
    Ok(out) => println!("{out}"),
    Err(e) => eprintln!("{e}: {}", e.guidance()),      // ActionError, with what to do next
}
```

A session holds cookies and storage across its tabs. `tab.screenshot(...)`, `tab.view(...)` and
`tab.observe_full()` give pixels and other views.

Web pages never load `file:` URLs. Top-level `file:` loads are allowed by default, for your own
code; when an agent picks the URLs, set `LoadOptions { allow_files: false, .. }` as `pageon mcp`
does. Origins that come from users are checked with `pageon::check_origin`.

## The environment

Every request, clock reading and random draw goes through one `Env`:

| `Env` | |
| --- | --- |
| `Live` | The network, with pageon's honest headers |
| `Recording::new(Live::default())` | The same, recorded to a JSON file |
| `Replay::load("ep.rec.json")` | Serves only the recording: offline and byte for byte |
| `Policy::new(env).read_write([...])` | Origin sets, an approver and a guard |
| `Secrets::new(env).with("shop", Secret::value(...).origins([...]))` | The vault |

They compose: `Session::new(&Secrets::new(Policy::new(Live::default())...))`.

## Embedding a configured browser

`pageon::Config` is the session the `pageon` binary assembles from its flags: origin sets,
secrets, staged files, a guard, a recording or replay, the identity and profile. A program that
embeds pageon gets exactly the same stack, and can pass its users' flags through verbatim:

```rust
let config = pageon::Config::new()
    .origin("https://shop.example")
    .secret_spec("shop@https://shop.example")       // PAGEON_SECRET_SHOP holds the value
    .file_spec("cv@https://jobs.example=./cv.pdf")
    .record("session.rec.json");
for warning in config.check()? {
    eprintln!("warning: {warning}");
}
let stdin = std::io::stdin();
pageon::mcp::serve_with(&config, Default::default(), stdin.lock(), std::io::stdout())?;
```

`check()` reports what is wrong and how to fix it; `run(|env| …)` runs any job over the stack
and saves the recording with every secret redacted. [mrun](https://github.com/hupe1980/mrun), a
runner for local models, embeds pageon this way and serves it as `mrun mcp serve browser`.

What pageon defends against, and every known gap, is in
[`pageon::threat_model`](https://docs.rs/pageon/latest/pageon/threat_model/).

## The sandbox

`Sandbox::new(env, SandboxOptions::default())` starts a caged content process; its tabs
(`RemoteTab`) have the same API, and the broker keeps the environment. `lookahead` forks the page
to try actions.

`cargo doc --open -p pageon` builds the API reference.
