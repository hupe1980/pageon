+++
title = "WebDriver BiDi"
description = "pageon serves the W3C WebDriver BiDi protocol: Puppeteer's BiDi mode and plain BiDi clients drive a sandboxed page, with pageon:observe and pageon:act for agents."
weight = 14
[extra]
group = "Interfaces"
nav = "WebDriver BiDi"
+++

WebDriver BiDi is the W3C standard for browser automation. `pageon cdp` (or its alias
`pageon bidi`) serves it on the same port as CDP, at `/session`.

```js
const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222/session',
  protocol: 'webDriverBiDi',
});
```

Puppeteer's own test scripts run over BiDi unchanged, in the sandbox and in-process.

## Plain BiDi

Any WebSocket client can speak it:

```json
{"id": 1, "method": "session.new", "params": {"capabilities": {}}}
{"id": 2, "method": "browsingContext.getTree", "params": {}}
{"id": 3, "method": "browsingContext.navigate", "params": {"context": "…", "url": "https://example.com", "wait": "complete"}}
{"id": 4, "method": "script.evaluate", "params": {"expression": "document.title", "target": {"context": "…"}, "awaitPromise": false}}
```

| Module | Commands |
| --- | --- |
| `session` | `status`, `new`, `end`, `subscribe`, `unsubscribe` |
| `browser` | `close`, `getUserContexts`, `getClientWindows` |
| `browsingContext` | `getTree`, `create`, `close`, `activate`, `navigate`, `reload`, `traverseHistory`, `captureScreenshot`, `setViewport`, `locateNodes` (css, context) |
| `script` | `evaluate`, `callFunction`, `disown`, `getRealms`, `addPreloadScript` (with `contexts` and `sandbox`), `removePreloadScript`, with the spec's value serialization |
| `input` | `performActions`, `releaseActions` |
| `storage` | `getCookies`, `setCookie`, `deleteCookies` |
| `pageon:` | `pageon:observe {context}`, `pageon:act {context, action}` |

Events (`browsingContext.*`, `script.realm*`, `log.entryAdded`, `network.*`) reach subscribers
only. Commands outside this list return `unsupported operation` with what to use instead, for
example network interception (use the egress policy or `--guard`).

## Sandboxes

`script.evaluate` and `script.callFunction` with `target: {context, sandbox}` run in an isolated
world of the page: same document, separate JavaScript, so a page cannot tamper with code a client
runs there. A node handle from another realm is resolved by its `sharedId` in the target realm.

## Honest capabilities

`session.new` answers `browserName: "pageon"` and pageon's real user agent. Asking for Chrome or
Firefox, or for `acceptInsecureCerts`, fails with `session not created` and the reason. pageon
answers dialogs itself, so `handleUserPrompt` reports `no such alert`.
