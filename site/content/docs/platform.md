+++
title = "Web platform"
description = "What pages can use on pageon: scripts and modules, the DOM and CSS, forms, web components, frames, networking, streams, Web Crypto, files and drag and drop."
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
| DOM | The DOM standard's tree, events (capture, bubble, handlers), selectors, `innerHTML`, live collections, `MutationObserver`, `IntersectionObserver`, `ResizeObserver`, `TreeWalker`, `NodeIterator`, `DOMParser` and `createHTMLDocument` (HTML, inert: nothing in them runs or loads) |
| CSS and layout | The cascade (Stylo), block, flex and grid (Taffy), text (Parley, with dictionary line breaking for Chinese, Japanese, Thai, Lao, Khmer and Burmese) and bundled fonts, computed style, geometry (`getBoundingClientRect`), `matchMedia`, `CSS.supports`, `CSS.escape`, scrolling of the page and of containers |
| Editing | `Selection` and live `Range`s, `contenteditable` with the browser's edits through `beforeinput`/`input` (typing, Enter, Backspace and Delete by character or word, paste, cut), caret keys, text-field selection (`selectionStart`, `setSelectionRange`, `setRangeText`), `caretPositionFromPoint` and range rectangles from layout, `execCommand` for the editing commands (`insertText`, `delete`, `insertParagraph`, …; formatting commands return `false`). Editors such as ProseMirror, Lexical and Quill work |
| Forms | Every control as form state, constraint validation with Chrome's messages, `:invalid`/`:user-invalid`, form-associated custom elements, `FormData`, multipart submission |
| Web components | Shadow DOM (open, closed, declarative), slots, custom elements, `ElementInternals`, adopted stylesheets |
| Frames and messaging | Same- and cross-origin iframes, `postMessage`, `contentWindow`/`contentDocument` across same-origin frames, `MessageChannel` within a document (ports are not transferable) |
| Networking | `fetch`, `XMLHttpRequest`, `AbortController`, CORS, cookies (`SameSite`, CHIPS), `document.cookie` |
| Storage | `localStorage`, `sessionStorage` and IndexedDB, kept per origin for the session across loads and tabs (IndexedDB: 50 MiB per origin) |
| Streams | `ReadableStream`, `WritableStream`, `TransformStream`, `TextDecoderStream`, `TextEncoderStream`, `Response.body` (a body arrives whole, in one chunk), `arrayBuffer()`, `bytes()` |
| Web Authentication | `navigator.credentials.create()`/`get()` for passkeys (ES256, `none` attestation) with keys held by the broker, `PublicKeyCredential` and its JSON helpers; see [passkeys](@/docs/secrets.md#passkeys) |
| Web Crypto | `crypto.subtle` on secure pages: SHA digests; HMAC, ECDSA P-256 and Ed25519 keys, signatures and verification; AES-GCM. Keys come from the session's randomness and ECDSA signs deterministically, so replays match. Other algorithms reject with `NotSupportedError` |
| Realtime | `WebSocket` and `EventSource`, held by the broker, under the same policy as requests |
| Files | `Blob`, `File`, `FileList`, `FileReader`, file inputs and drop zones (with staged files only), downloads |
| Drag and drop | Trusted pointer drags, HTML drag and drop, `DataTransfer`, `DragEvent` |
| Clipboard | One clipboard per session, never the host's: copy, cut and paste events with `clipboardData`, `navigator.clipboard.writeText` after a click or key; `readText` is refused, so pages read only what the agent pastes |
| History | `history.pushState`, `popstate`, fragments, `back`/`forward` with saved state |
| Other | Timers and `requestAnimationFrame` on the virtual clock, `crypto.getRandomValues`/`randomUUID`, `TextEncoder`/`TextDecoder`, `URL`, user timing (`performance.mark`/`measure`, `PerformanceObserver`), `screen`, `document.visibilityState`, `InputEvent`, `reportError`, Canvas 2D drawing, SVG elements, `<dialog>` with a modal top layer, WebMCP (`document.modelContext`) |

## Not supported

`BroadcastChannel`, iframes a script inserts after load, `blob:` URLs, workers and
service workers, IME composition, undo (`historyUndo`) and formatting commands, `designMode`,
`ClipboardItem`, `PasswordCredential` and `FederatedCredential`, the Navigation API, Web Animations (`Element.animate`), `document.write`, XML
parsing (`DOMParser` takes HTML only), canvas text and readback, the Permissions and
Notifications APIs, WebRTC and WebTransport, HTTP/3.
For automation clients: WebDriver classic and request interception (use origin sets or a
recorded session instead).

## Honest identity

`navigator.webdriver` is `true`, the user agent names pageon, and robots.txt is honoured. See
[identity](@/docs/identity.md).
