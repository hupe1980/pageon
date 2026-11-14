# Baseline fidelity

60/60 adapter outputs identical to their reference (100.0 %; bar ≥ 95 %).

Method: Baseline fidelity: each adapter equals its reference implementation's output.  The adapters run the references' own code paths — Playwright's public ``page.aria_snapshot(mode="ai")`` (what Playwright MCP calls), BrowserGym's ``flatten_axtree_to_str``, browser-use 1.0's ``AxHelpers.state()`` — on inputs captured from the live page. This check re-runs each reference on the stored inputs and compares it with the adapter's untruncated output, over up to 20 corpus pages.

| Page | Format | Result |
| --- | --- | --- |
| checkout | playwright_mcp | identical |
| checkout | browsergym | identical |
| checkout | browser_use | identical |
| engine-vocabulary | playwright_mcp | identical |
| engine-vocabulary | browsergym | identical |
| engine-vocabulary | browser_use | identical |
| form-states | playwright_mcp | identical |
| form-states | browsergym | identical |
| form-states | browser_use | identical |
| hidden-fields | playwright_mcp | identical |
| hidden-fields | browsergym | identical |
| hidden-fields | browser_use | identical |
| i18n | playwright_mcp | identical |
| i18n | browsergym | identical |
| i18n | browser_use | identical |
| landmarks | playwright_mcp | identical |
| landmarks | browsergym | identical |
| landmarks | browser_use | identical |
| long-list | playwright_mcp | identical |
| long-list | browsergym | identical |
| long-list | browser_use | identical |
| modal-cover | playwright_mcp | identical |
| modal-cover | browsergym | identical |
| modal-cover | browser_use | identical |
| nested-frames | playwright_mcp | identical |
| nested-frames | browsergym | identical |
| nested-frames | browser_use | identical |
| real-bbc-news | playwright_mcp | identical |
| real-bbc-news | browsergym | identical |
| real-bbc-news | browser_use | identical |
| real-crates-search | playwright_mcp | identical |
| real-crates-search | browsergym | identical |
| real-crates-search | browser_use | identical |
| real-form | playwright_mcp | identical |
| real-form | browsergym | identical |
| real-form | browser_use | identical |
| real-github | playwright_mcp | identical |
| real-github | browsergym | identical |
| real-github | browser_use | identical |
| real-govuk | playwright_mcp | identical |
| real-govuk | browsergym | identical |
| real-govuk | browser_use | identical |
| real-hackernews | playwright_mcp | identical |
| real-hackernews | browsergym | identical |
| real-hackernews | browser_use | identical |
| real-mdn | playwright_mcp | identical |
| real-mdn | browsergym | identical |
| real-mdn | browser_use | identical |
| real-python-docs | playwright_mcp | identical |
| real-python-docs | browsergym | identical |
| real-python-docs | browser_use | identical |
| real-rust-lang | playwright_mcp | identical |
| real-rust-lang | browsergym | identical |
| real-rust-lang | browser_use | identical |
| real-shop | playwright_mcp | identical |
| real-shop | browsergym | identical |
| real-shop | browser_use | identical |
| real-todomvc | playwright_mcp | identical |
| real-todomvc | browsergym | identical |
| real-todomvc | browser_use | identical |
