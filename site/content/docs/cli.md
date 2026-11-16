+++
title = "Command line"
description = "The pageon CLI: browse in named sessions from the shell, fetch and act on pages, take screenshots, record and replay, serve MCP, CDP and WebDriver BiDi, and run workflows."
weight = 10
[extra]
group = "Interfaces"
nav = "Command line"
+++

One binary does everything. `pageon <command> --help` prints every option with examples.

| Command | What it does |
| --- | --- |
| `pageon open URL`, `click`, `type`, `view`, … | Browse in a named session that stays open between commands |
| `pageon fetch URL` | Load a page, optionally act on it, and print a view or take a screenshot |
| `pageon mcp` | Serve a browser to an agent over MCP (stdio; `--http` for Streamable HTTP) |
| `pageon cdp` (alias `pageon bidi`) | Serve Puppeteer, Playwright and WebDriver BiDi clients on one port |
| `pageon replay FILE` | Re-run a recorded session and check every step reaches the same page |
| `pageon workflow compile/run` | Turn a recorded session into a workflow; run it without a model |
| `pageon identity new/directory` | Create and publish a Web Bot Auth identity |

## Sessions

For coding agents and scripts that work from a shell. `open` starts a session in the background
on first use; each later command acts on it and prints what happened and what changed.

```sh
pageon open https://shop.example --origin https://shop.example
pageon type e1 ada
pageon click e2          # ok click e2 · navigated to … then the diff of the view
pageon find "report"     # elements anywhere on the page
pageon view markdown     # the main content
pageon screenshot page.png --marks
pageon close
```

| Command | Does |
| --- | --- |
| `open URL` | Load a page; starts the session with the [shared options](#options-shared-by-fetch-open-replay-mcp-cdp-and-workflow-run) given here, plus `--idle` (default 30 minutes, `0` never), `--detail`, `--ui` or `--approve` for the [viewer](@/docs/viewer.md), and `--sign-in FILE` ([sign-ins that renew](@/docs/workflows.md#sign-ins-that-renew)) |
| `click`, `type REF TEXT [--submit]`, `press`, `select`, `paste`, `hover`, `drag`, `scroll`, `wait`, `back`, `forward`, `upload` | [Actions](@/docs/actions.md) on refs |
| `observe [--full]` · `view [ax\|markdown\|text\|requests\|errors]` · `find WORDS` | Read the page |
| `screenshot FILE [--marks] [--ref REF]` | Pixels to a file; notes on stdout |
| `tabs`, `reset [STEP]`, `lookahead ACTION…`, `call NAME JSON`, `takeover REASON` | Tabs, [reset and lookahead](@/docs/lookahead.md), [page tools](@/docs/webmcp.md), a person's help |
| `sessions` · `close [--all]` | List sessions with their tabs; end one or all |
| `ui` | Print the session's [viewer](@/docs/viewer.md) URL (`open --ui` or `--approve`) |
| `skill [--install [DIR]]` | Print the agent skill, or write `DIR/pageon/SKILL.md` (default `.claude/skills`) |

`-s NAME` (or `PAGEON_SESSION`) picks the session; each is a separate browser with its own
cookies and storage. Each command is the [MCP tool](@/docs/mcp.md) of the same name, with the same
reply. `--save-state` and `--record` are written when the session closes, by `close` or after
`--idle`.

For a local dev server, keep the session on it:
`pageon open http://localhost:3000 --private-network allow --origin http://localhost:3000`.

Sessions live in `$PAGEON_SESSIONS`, else `$XDG_RUNTIME_DIR/pageon`, else `~/.pageon/sessions`:
a directory only you can open (pageon refuses one others can), holding each session's socket,
options and log. Secret values reach the session through its environment, never a file. Unix only.

## fetch

```sh
pageon fetch https://example.com                         # readable text
pageon fetch https://example.com --dump ax --budget 2000 # the agent view, 2,000 tokens
pageon fetch https://example.com --act "click e3" --act "type e5 hello" --dump ax
pageon fetch https://shop.example --act "click e9" --expect 'see "Thanks"'   # exit 8 if not
pageon fetch https://example.com --screenshot page.png --full-page
pageon fetch ./page.html                                 # local files work too
pageon fetch https://example.com/paper.pdf --dump ax      # PDFs, Markdown and text read as pages
pageon fetch https://blog.example/post --prefer-markdown  # the site's Markdown, for crawls
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

| Command | Its own options |
| --- | --- |
| `fetch` | `--dump`, `--act`, `--expect`, `--viewport WxH`, `--prefer-markdown`, `--approve`, screenshot options |
| `replay` | `--to N`, `--live`, `--approve`, `--trace FILE`, `--report` (what each step cost) |
| `mcp` | `--http [PORT]`, `--allow-origin`, `--otlp URL`, `--detail full\|compact`, `--allow-javascript-exec`, `--ui`, `--approve`, `--sign-in FILE` |
| `cdp` | `--port`, `--allow-origin`, `--discovery` |

## Options shared by fetch, open, replay, mcp, cdp and workflow run

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
| 2 | Usage or environment error (bad option, unreadable file, busy port, no such session) |
| 3 | The load, an action or the sandbox failed; the message says what to do |
| 4 | The server answered with an error status (its page is printed) |
| 5 | `replay`: a step diverged from the recording |
| 6 | The site refused the agent (blocked, challenge, rate limited) |
| 7 | `workflow run`: the site drifted from the workflow |
| 8 | An [expectation](@/docs/actions.md#expectations) (`--expect`) did not hold |

Logs and errors go to stderr; stdout carries only the view, the reply, the legend or the protocol.
