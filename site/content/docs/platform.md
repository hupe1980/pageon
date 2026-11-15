+++
title = "Web platform"
description = "What pages can use on pageon: scripts and modules, the DOM and CSS, forms, web components, frames, networking, WebSockets, files and drag and drop."
weight = 53
[extra]
group = "Reference"
seo_title = "Web platform support: what pages can use"
+++

pageon implements the web platform pages need, with the semantics the standards give it, and
leaves the rest out. A missing API is absent rather than stubbed, so feature detection works
and `pageon fetch URL --dump errors` names what a page tried to use.

## Supported

| Area | What pages get |
| --- | --- |
| Scripts | Classic and module scripts, `defer`/`async`, import maps, dynamic `import()`, top-level `await`, JSON modules, script loaders and bundler chunks (Vite, esbuild, webpack) |
| DOM | The DOM standard's tree, events (capture, bubble, handlers), selectors, `innerHTML`, live collections, `MutationObserver`, `IntersectionObserver`, `ResizeObserver`, `TreeWalker` |
| CSS and layout | The cascade (Stylo), block, flex and grid (Taffy), text (Parley) with bundled fonts, computed style, geometry (`getBoundingClientRect`), `matchMedia`, scrolling of the page and of containers |
| Forms | Every control as form state, constraint validation with Chrome's messages, `:invalid`/`:user-invalid`, form-associated custom elements, `FormData`, multipart submission |
| Web components | Shadow DOM (open, closed, declarative), slots, custom elements, `ElementInternals`, adopted stylesheets |
| Frames | Same- and cross-origin iframes, `postMessage`, `contentWindow`/`contentDocument` across same-origin frames |
| Networking | `fetch`, `XMLHttpRequest`, `AbortController`, CORS, cookies (`SameSite`, CHIPS), `localStorage`, `sessionStorage`, `document.cookie` |
| Realtime | `WebSocket` and `EventSource`, held by the broker, under the same policy as requests |
| Files | `Blob`, `File`, `FileList`, `FileReader`, file inputs and drop zones (with staged files only), downloads |
| Drag and drop | Trusted pointer drags, HTML drag and drop, `DataTransfer`, `DragEvent` |
| History | `history.pushState`, `popstate`, fragments, `back`/`forward` with saved state |
| Other | Timers and `requestAnimationFrame` on the virtual clock, `crypto.getRandomValues`/`randomUUID`, `TextEncoder`/`TextDecoder`, `URL`, Canvas 2D drawing, SVG elements, `<dialog>` with a modal top layer, WebMCP (`document.modelContext`) |

## Not supported

`MessageChannel` and `BroadcastChannel`, iframes a script inserts after load, `blob:` URLs,
`IndexedDB`, workers and service workers, Selection and `Range`, the Navigation API,
`crypto.subtle`, `document.write`, canvas text and readback, WebRTC and WebTransport, HTTP/3.
For automation clients: WebDriver classic and request interception (use origin sets or a
recorded session instead).

## Honest identity

`navigator.webdriver` is `true`, the user agent names pageon, and robots.txt is honoured. See
[identity](@/docs/identity.md).
