# pageon-js

pageon's JavaScript: V8 (single-threaded), DOM bindings generated from the standards' WebIDL, and
the HTML event loop on pageon's virtual clock.

The `pageon` loader drives it, so a plain `pageon::load` already runs a page's scripts the way
HTML does:
- classic scripts at their parse position;
- then `defer` scripts and module scripts in document order (modules with static and dynamic
  `import`, import maps, `import.meta`, top-level `await`, JSON modules; `nomodule` scripts are
  skipped);
- scripts inserted by DOM methods (script loaders, webpack chunks, tag snippets), with
  `load`/`error`;
- then `DOMContentLoaded` and `load`;
- then settling on virtual time.

Pages without scripts never start the engine.

## The surface is generated, and honest

`idl/` holds pinned `@webref/idl` files and `allowlist.toml`. `cargo xtask idl` generates
`src/generated.rs`: V8 templates, inheritance, brand checks and WebIDL argument conversions.
Without hand-written code it also generates:
- every `[Reflect]` attribute (strings, booleans, numbers, URLs);
- every `on…` event-handler attribute;
- the constants.

The members implemented by hand are listed in the allowlist and live in `src/dom/`. A missing
implementation is a compile error. **Anything else is absent**, never stubbed, so feature
detection works and failures name the missing API. CI checks that the generated file is fresh.

The surface covers what real pages and the MiniWoB++ and corpus pages need:
- the DOM tree, attributes and `attributes`, `classList`, `style` and computed style;
- selectors (Stylo's parser), `innerHTML` and events (capture, target, bubble, handlers);
- live collections and `dataset`;
- geometry readers from pageon-layout;
- form controls (as form state, not attributes), `<dialog>` with a modal top layer, iframes;
- timers, `requestAnimationFrame`, `console`, `location` (read), `navigator`, `performance`;
- `MutationObserver`, `IntersectionObserver`, `TreeWalker`, `XMLSerializer`;
- WebMCP: `document.modelContext.registerTool`, with the origin and script of every tool;
- a Canvas 2D drawing subset rasterized with tiny-skia;
- SVG elements and named access on `window`;
- networking: `fetch` with `Headers`, `Request` and `Response`, async `XMLHttpRequest`,
  `AbortController` and `AbortSignal` (including `timeout` and `any`), `URL`,
  `URLSearchParams`, `FormData` (string entries), `document.cookie`, `localStorage` and
  `sessionStorage`.

Input comes from the engine: `Engine::click`, `type_text`, `press`, `select` and `hover` dispatch
trusted pointer, mouse, focus, keyboard and input events, at the topmost element under the point,
followed by the browser's default actions (links, form submission, labels, `<details>`).
Navigations, dialogs (`alert`/`confirm`/`prompt`, answered at once by a policy) and popups are
recorded for the caller. The isolate is locked only while in use, so pages can stay live
in tabs and be dropped in any order.

Requests go through [`pageon-net`](../pageon-net), which applies cookies, CORS and the egress
policy. Responses arrive as tasks on the virtual clock, in issue order, while the page settles.
A blocked or CORS-refused request is a plain `TypeError: Failed to fetch` to the page, as in
browsers, and the reason is in the request log. Unhandled promise rejections are page errors.

## Deterministic

`Date`, `performance.now()`, `Math.random()` and every timer read the clock and RNG seeded
from `Env`. Time is virtual: `setTimeout(f, 300)` runs `f` 300 ms later on the page's clock, at
once in wall time. A recorded load replays to the same DOM, byte for byte.

## Limits

| Limit | Default | When hit |
| --- | --- | --- |
| Time per task | 2 s wall | `time-limit` error; the task ends; later scripts run |
| Heap | 256 MB | `memory-limit` error; scripting stops for the page |
| Tasks per settle | 10,000 | settling stops; pending timers are counted |
| Settle window | 1,000 ms virtual | timers due later stay pending |

The process stays single-threaded at rest: the watchdog thread exists only while a task runs.
A bug in a binding becomes a JavaScript `TypeError` and a reported page error; it never aborts
the process.

## Not yet

These are absent, and reported when a page needs them:
- `document.write`;
- `ElementInternals` (form-associated custom elements), constructable stylesheets
  (`adoptedStyleSheets`) and customized built-ins (`is=`);
- `iframe.contentWindow`, `postMessage` and other cross-frame scripting;
- form validation (`checkValidity`, `validity`);
- `Blob`, `File`, `FileReader`, streams;
- `WebSocket`, `EventSource`, `IndexedDB`;
- Selection and `Range`, the Navigation API, `crypto.subtle`;
- workers;
- canvas text, images and readback;
- SVG geometry;
- CSS module scripts and subresource integrity.

MIT OR Apache-2.0.
