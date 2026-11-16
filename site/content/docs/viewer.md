+++
title = "The viewer"
description = "Watch an agent's session step by step, pause it, and approve or deny the writes it is about to send, in a page the session serves on loopback."
weight = 33
[extra]
seo_title = "Supervise a browser agent: steps, pause and approvals"
group = "Safety"
nav = "Viewer & approvals"
+++

A session can serve a page for the person supervising it. It shows what the agent did and
read, step by step, and asks before writes leave.

```sh
pageon open https://shop.example --origin https://shop.example --approve
# viewer: http://127.0.0.1:53117/v/4f0c…   open it in your browser

claude mcp add shop -- pageon mcp --origin https://shop.example --approve
# the URL is on the server's stderr
```

`--ui` serves the viewer; `--approve` also asks you in it. Both work with `pageon mcp` and
`pageon open`; `pageon ui` prints a CLI session's URL again.

<figure class="shot"><picture><source media="(prefers-color-scheme: dark)" srcset="../../img/viewer-approval-dark.png"><img src="../../img/viewer-approval-light.png" alt="The viewer asking whether to send a POST to the shop, carrying the secret card, after clicking Pay 129.00 EUR" width="1280" height="800" loading="lazy"></picture><figcaption>A write waits for you: where it goes, what it carries, what caused it.</figcaption></figure>

## Approvals

With `--approve`, each write the policy holds waits for you: a write to an `--origin`, a write
after reading another site with `--cross-site-writes refuse`, or a
[guard](@/docs/security.md#guard)'s `ask`. Reads never ask.

The island at the top opens into the question. It leads with what pageon knows: the method and site, the full URL (secrets as
`secret://name` handles), and in plain words what the request carries: your secret, content of
another site read before it, a page tool marking it consequential, a first request to the site.
Then what caused it, with the element's name marked as coming from the page, since a page names
its own buttons.

| Choice | Key | Does |
| --- | --- | --- |
| **Allow Once** | `a` | Sends this request |
| **Allow for This Session** | `g` | Sends it, and later requests with this method to this site |
| **Don't Allow** | `d`, `Esc` | Refuses it; the agent's outcome says it was not approved |

A write nobody decides within 10 minutes is refused.

## Steps

<figure class="shot"><picture><source media="(prefers-color-scheme: dark)" srcset="../../img/viewer-steps-dark.png"><img src="../../img/viewer-steps-light.png" alt="The viewer's steps: opened the shop, searched, typed the email and the card secret, clicked Pay, with the page after the click" width="1280" height="800" loading="lazy"></picture><figcaption>Every step in words, with the page before and after it, or what the model read.</figcaption></figure>

Every tool call is a step on the timeline, said in words: *Clicked “Pay 129.00 EUR”*, with
names taken from the view the model read. The selected step shows in the agent's browser:
**Before**, **After**, or **What the model read**, word for word (`v` switches). Pictures are
taken only while the viewer is open, so a session nobody watches pays nothing. Each step shows
the tokens of what the model read, and the island the session's total.

The island at the top says what the agent is doing: **Working**, **Needs you** (a write
waits), **Paused** or **Idle**. `Space` pauses: the agent's next call waits until you resume
(a call already running finishes; the CLI says it is paused). `j` and `k` move between steps.

## Safety

The viewer listens on 127.0.0.1 at a URL with a random token. The first browser to open it
gets an HttpOnly, SameSite=Strict cookie and is the only viewer. Pausing and deciding also need
the page's own token and a matching `Origin`, so other local processes and web pages cannot
approve. Page text is shown as text, never as markup, under a CSP without inline script. It
keeps the last 1,000 steps and 100 pictures.
