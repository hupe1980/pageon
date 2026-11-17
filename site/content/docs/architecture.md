+++
title = "Architecture"
description = "How pageon is built: its own DOM, V8 with WebIDL bindings, Stylo and Taffy layout, a sandboxed page process and one seam for nondeterminism."
weight = 51
[extra]
seo_title = "Architecture of an agent-first browser engine"
group = "Reference"
+++

pageon is a browser engine written in Rust. It owns the parts that decide what an agent sees
and what a page can do, and reuses proven components for the rest.

## Owned and reused

| pageon owns | pageon reuses |
| --- | --- |
| The DOM (an arena with handles that never alias) | html5ever (HTML parsing) |
| The bindings, generated from the WebIDL standards | V8 (JavaScript) |
| The event loop, on a virtual clock | Stylo, Taffy, Parley (style, layout, text) |
| Networking semantics: Fetch, CORS, cookies, storage | rustls, hyper (transport) |
| The box tree, its layout and paint, the accessibility tree and the agent views | vello_cpu (pixels) |
| The process model, policy and vault | procage (the sandbox) |

## The crates

| Crate | Role |
| --- | --- |
| `pageon` | Sessions and tabs, actions, the sandbox broker, policy and vault, the MCP, CDP and BiDi servers, the CLI |
| `pageon-js` | V8, bindings generated from pinned WebIDL, the HTML event loop and script model |
| `pageon-style` | Stylo on pageon's own document: computed styles kept beside the nodes, updated from the document's change log |
| `pageon-layout` | pageon's box tree laid out with Taffy and Parley on bundled fonts, visibility, cover, hit-testing, paint |
| `pageon-ax` | Roles (HTML-AAM, ARIA), names (AccName 1.2), states |
| `pageon-agent` | The `ax` view: refs, quoting, marks, budgets, diffs |
| `pageon-net` | Fetch, CORS, cookies (RFC 6265bis, PSL, CHIPS), storage, the egress policy, the request log |
| `pageon-dom` | The document model, parsing and serialization |
| `pageon-env` | The single seam for network, clock and randomness: live, recorded, replayed |

## Design principles

- **An honest platform.** A Web API is implemented with its specified semantics or left out, so
  feature detection works. No stubs that pretend.
- **One seam for nondeterminism.** Network, time and randomness come only through `Env`, and a
  lint enforces it. That is what makes byte-identical replay, reset and offline tests possible.
- **Single-threaded content process.** One V8 platform thread, inline styling, no async runtime
  inside the page process: simple to sandbox, and safe to fork for lookahead.
- **Safety in code, not in prompts.** Policy, secrets and approvals live in the broker, outside
  the page process and outside the model.
- **Bindings from standards.** The DOM surface is generated from the specifications' WebIDL;
  hand-written members are listed and checked at compile time.

## What pages can use

The [web platform](@/docs/platform.md) page lists what pages get and what is absent.
