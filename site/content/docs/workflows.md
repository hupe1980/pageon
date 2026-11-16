+++
title = "Workflows"
description = "Compile a solved task into a workflow that reruns without a model, survives cosmetic changes, stops where a site drifted, and is repaired by hand."
weight = 42
[extra]
seo_title = "Workflows: rerun solved tasks without a model"
group = "Reproducible sessions"
+++

Once an agent has solved a task, it should not need a model to do it again. pageon compiles a
recorded session into a workflow: readable JSON that names elements the way the agent saw them.

```sh
pageon fetch https://shop.example/login --act "type e3 ada" --act "type e4 secret://shop" \
  --act "click e5" --act "click e9" --record ep.json
pageon workflow compile ep.json -o report.flow.json
pageon workflow run report.flow.json --param user=bob --secret shop@https://shop.example
```

{% raw %}
```json
{
  "workflow": 1,
  "start": "https://shop.example/login",
  "params": [{ "name": "user", "default": "ada" }],
  "steps": [
    { "do": "type", "target": { "role": "textbox", "name": "User" }, "text": "{{user}}" },
    { "do": "type", "target": { "role": "textbox", "name": "Password" }, "text": "secret://shop" },
    { "do": "click", "target": { "role": "button", "name": "Sign in" },
      "expect": "https://shop.example/account" },
    { "do": "click", "target": { "role": "link", "name": "Reports" } }
  ]
}
```
{% endraw %}

- **Locators are what the agent saw**: role, name and index, never CSS, XPath or refs.
- **Typed values become parameters**; secrets stay `secret://` handles.
- **Navigating steps say where they should land.**
- A run takes about 1 ms per step, through the same sandbox, policy and vault as any session.

## When the site changes

| Change | What the run does |
| --- | --- |
| Refs shifted, fields reordered | Nothing to notice: locators are by role and name |
| A label was renamed ("Sign in" → "Sign in now") | Follows it and reports **soft drift** |
| A target is gone, or a step lands elsewhere | Stops at that step: **drifted** (exit 7), with what the page offers instead |

## Repair

After a drift the tab stays where the run stopped. Finish the task by hand (or let the agent),
then save: the session compiles into the repaired workflow. Over MCP that is
`browser_workflow_run` then `browser_workflow_save`; in Python `tab.workflow_run(...)` and
`tab.workflow_save()`. Workflows travel as JSON in tool arguments; the server never reads or
writes files a model names.

## Sign-ins that renew

Sign-ins expire mid-task. Record the sign-in once and name it with `--sign-in`:

```sh
PAGEON_SECRET_PORTAL=… pageon fetch https://portal.example/login --secret portal@https://portal.example \
  --act "type e1 ada" --act "type e2 secret://portal" --act "click e3" --record login.rec.json
pageon workflow compile login.rec.json -o login.flow.json

pageon mcp --origin https://portal.example --secret portal@https://portal.example --sign-in login.flow.json
```

The workflow's start page is the site's sign-in page. When an agent's action is redirected from
another page of that site to the sign-in page, or answered 401, pageon runs the workflow (the
password from the vault, a TOTP code if the secret is `:totp`) and opens the page again:

```text
signed out of portal.example (redirected to the sign-in page); signed in again with login.flow.json (3 steps), reopened /orders
ok open https://portal.example/orders · …
```

- Only engine facts count (where a page request went, its status), never page text.
- A write is never sent again: after a POST met 401, the reply says `the POST was not sent
  again: do it again`.
- The sign-in runs as the agent's own actions: origins, guard, approver and recording apply.
- Three renewals of a site in a row that do not hold, and pageon stops (`not signing in
  again`), so a wrong password cannot lock the account.
- The reply carries `signed_in` (origin, workflow, why) in its structured content.

`--sign-in` works on `pageon mcp` and `pageon open` (repeatable, one per site); Python:
`Browser(sign_ins=["login.flow.json"])`.
