+++
title = "Agent readiness audit"
description = "pageon audit loads a page as an agent does and scores what it meets: names, actions, page tools, what its view costs in tokens, access and health, with the fixes that matter most first."
weight = 49
[extra]
seo_title = "Agent readiness audit: how agents meet your site"
group = "Reference"
nav = "Agent readiness audit"
+++

Is a site ready for agents? Checklists look at what it publishes. `pageon audit` loads the
page as an agent and reports what the agent meets: controls it cannot name, clicks that land on
an overlay, forms it must fill field by field, and what reading the page costs.

```sh
pageon audit https://shop.example/
```

```text
   81  agent readiness · https://shop.example/

  Names    ██████░░░░  62  3 unnamed · 1 placeholder only · 3 alike
  Actions  ████████░░  77  1 covered · 1 fake button · 1 small
  Tools    ██████████ 100  1 tool · "search"
  Cost     ██████████ 100  137 tokens (49% of the HTML)
  Access   ███████░░░  70  no llms.txt · no Markdown
  Health   ████████░░  80  1 script error

  Fix first
   +6  3 controls without a name: an agent cannot tell what they are for
         link e1
         button e2
         textbox e7
         → Give icon buttons and links an aria-label, images in links an alt, and fields a <label>.
   +3  3 links "Edit" share a name: an agent cannot tell them apart
         3 × link "Edit": e3, e4, e5
         → Name each for what it acts on, e.g. aria-label="Edit <item>", or add visually hidden text.
   +2  1 control covered by another element: a click lands on what covers it
         button e13 "Under" covered
         → Move overlays, banners and sticky bars off controls, or let clicks through (pointer-events: none).
  …
```

Refs are those of `pageon fetch URL --dump ax`, so you can see each element in the agent's view.

## What it checks

| Area | Weight | What counts |
| --- | --- | --- |
| Names | 25 | Controls with an accessible name. A placeholder alone counts half; three or more controls with the same role and name count half |
| Actions | 20 | Controls an agent can act on: not covered by another element, not a clickable `<div>`, and at least 24 × 24 px or with that much room (WCAG 2.5.8; links in text are exempt) |
| Tools | 15 | [Page tools](@/docs/webmcp.md) with a description and described parameters. Not scored on pages without tools; forms that could be tools are suggested |
| Cost | 20 | Tokens (o200k) of the whole agent view: full marks up to the 4,000 an agent sees by default, less beyond; the largest landmarks are named |
| Access | 10 | Not refused or challenged as an honest agent; content without scripts; robots.txt, llms.txt, Markdown on request |
| Health | 10 | The HTTP status and the page's own script errors (not pageon's time or memory limits); a page that shows next to nothing; layout thrashing (more than 40 layouts forced while loading) |

The score is the weighted mean of the areas that apply. Findings are ordered by the points
fixing them would gain.

Besides the page, the audit loads `/robots.txt`, `/llms.txt` and the page with
`Accept: text/markdown`, with scripts off. That last load also measures what agents that run
no scripts read.

## In CI

```sh
pageon audit http://localhost:3000 --private-network allow --fail-under 80
pageon audit https://staging.example/ --json > audit.json
```

`--fail-under N` exits 9 when the score is below N. `--json` prints the report with every area,
finding and token count. Every session option applies (`--origin`, `--identity`,
`--profile`, …), to the site's files too; the audit runs caged like any load.

## On real sites

We audited eight public sites on 2026-11-16. Most of them are well built, and every score is
high. The findings are still concrete:

| Site | Score | What it found |
| --- | --- | --- |
| github.com | 96 | Nine targets under 24 px with neighbours too close; no Markdown |
| gov.uk | 96 | No llms.txt, no Markdown |
| docs.python.org | 96 | Two search boxes under 24 px next to their buttons |
| bbc.com/news | 96 | One small target; no llms.txt, no Markdown |
| rust-lang.org | 95 | No robots.txt |
| Hacker News | 92 | 69 links share 5 names ("upvote", "hide", …); an unnamed search box |
| TodoMVC | 92 | Its only field is named by a placeholder |
| crates.io search | 88 | Agents that run no scripts see 3 % of the page; 21 controls share 3 names |

The audit judges a page as pageon's agent meets it; other agents see similar pages, not
identical ones. It counts only errors the site's own scripts raise, but an error that appears
only in pageon may be a gap in pageon: please [report](https://github.com/hupe1980/pageon/issues) it.
