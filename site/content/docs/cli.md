+++
title = "Command line"
description = "The pageon CLI: fetch and act on pages, take screenshots, record and replay, serve MCP, CDP and WebDriver BiDi, and run workflows."
weight = 10
[extra]
group = "Interfaces"
nav = "Command line"
+++

One binary does everything. `pageon <command> --help` prints every option with examples.

| Command | What it does |
| --- | --- |
| `pageon fetch URL` | Load a page, optionally act on it, and print a view or take a screenshot |
| `pageon mcp` | Serve a browser to an agent over MCP (stdio; `--http` for Streamable HTTP) |
| `pageon cdp` (alias `pageon bidi`) | Serve Puppeteer, Playwright and WebDriver BiDi clients on one port |
| `pageon replay FILE` | Re-run a recorded session and check every step reaches the same page |
| `pageon workflow compile/run` | Turn a recorded session into a workflow; run it without a model |
| `pageon identity new/directory` | Create and publish a Web Bot Auth identity |

## fetch

```sh
pageon fetch https://example.com                         # readable text
pageon fetch https://example.com --dump ax --budget 2000 # the agent view, 2,000 tokens
pageon fetch https://example.com --act "click e3" --act "type e5 hello" --dump ax
pageon fetch https://example.com --screenshot page.png --full-page
pageon fetch ./page.html                                 # local files work too
```

| `--dump` | Prints |
| --- | --- |
| `text` | Readable text (default) |
| `ax` | [The agent view](@/docs/views.md), with refs |
| `markdown` | The main content as Markdown; links carry refs |
| `html` | The serialized DOM after scripts ran |
| `tree` · `boxes` | The element outline; with boxes and hidden/offscreen/covered flags |
| `errors` | Script errors of the page and its frames |
| `requests` | Every request, with blocked ones and the reason |

Screenshot options: `--screenshot FILE` (`.png`, `.jpg`), `--full-page`, `--marks`
(set-of-marks, legend on stdout), `--crop REF` (one element, with provenance), `--scale`,
`--mask`. See [screenshots](@/docs/screenshots.md).

## Options shared by every command

| Option | |
| --- | --- |
| `--budget TOKENS` | Token budget (o200k) of `ax` and `markdown` views. Default 4,000 |
| `--no-js` | Do not run the page's scripts |
| `--no-sandbox` | Run the page in-process (it then runs with your rights) |
| `--origin ORIGIN` | An origin the task may read and write; requests elsewhere are refused. Repeatable; `https://*.example.com` for subdomains. The scheme is required |
| `--read-origin ORIGIN` | An origin the task may only read |
| `--approve` | Ask on the terminal before every write (`fetch`, `replay --live`) |
| `--guard COMMAND` | Ask a program about requests ([guard](@/docs/security.md#guard)) |
| `--secret NAME@ORIGIN` | A [secret](@/docs/secrets.md) from `PAGEON_SECRET_<NAME>`, bound to an origin; `NAME:totp@ORIGIN` for TOTP |
| `--file NAME@ORIGIN=PATH` | A file the agent may upload by name, only to that origin |
| `--state FILE` · `--save-state FILE` | Start from / save cookies and localStorage (Playwright `storageState`) |
| `--record FILE` · `--replay FILE` | Record the session; serve it again offline |
| `--private-network allow` | Let pages reach localhost and private ranges (refused by default) |
| `--allow-files` | Let agents open `file:` pages (`mcp`, `cdp`, `bidi` refuse them by default) |
| `--proxy URL` | Send requests through a proxy (`HTTP(S)_PROXY` is honoured otherwise) |
| `--identity FILE` · `--signature-tag TAG` | Sign every request, with a tag ([identity](@/docs/identity.md)) |
| `--profile chrome` · `--platform windows` | One coherent Chrome, on this machine's platform or another |
| `--passkeys FILE` | Sign in with [passkeys](@/docs/secrets.md#passkeys) held by the broker |
| `--cross-site-writes refuse` | Refuse writes to a site after reading another, unless approved ([cross-site writes](@/docs/security.md#cross-site-writes)) |
| `--robots ignore` | Skip robots.txt, for sessions a person directs |
| `--timeout SECS` · `--max-bytes BYTES` | Network limits |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Done |
| 2 | Usage or environment error (bad option, unreadable file, busy port) |
| 3 | The load, an action or the sandbox failed; the message says what to do |
| 4 | The server answered with an error status (its page is printed) |
| 5 | `replay`: a step diverged from the recording |
| 6 | The site refused the agent (blocked, challenge, rate limited) |
| 7 | `workflow run`: the site drifted from the workflow |

Logs and errors go to stderr; stdout carries only the view, the legend or the protocol.
