+++
title = "Workflows"
description = "Compile a solved task into a workflow that reruns without a model in about 1 ms per step, survives cosmetic site changes, stops precisely where a site drifted, and is repaired by acting by hand."
weight = 42
[extra]
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
