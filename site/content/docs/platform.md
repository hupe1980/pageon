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
| DOM | The DOM standard's tree, events (capture, bubble, handlers in the order they were set; `new EventTarget()` and classes that extend it), `error` and `unhandledrejection` at the window (errors of other origins' scripts muted, as in browsers), selectors, `innerHTML`, live collections, `MutationObserver`, `IntersectionObserver` and `ResizeObserver` (once per frame of the virtual clock, when browsers render), `TreeWalker`, `NodeIterator`, `DOMParser` and `createHTMLDocument` (HTML, inert: nothing in them runs or loads) |
| CSS and layout | The cascade (Stylo), block, flex and grid (Taffy), text (Parley, with dictionary line breaking for Chinese, Japanese, Thai, Lao, Khmer and Burmese) and bundled fonts, computed style (kept up to date from the document's changes, no layout for values that are not sizes), form and state pseudo-classes (`:invalid`, `:user-invalid`, `:required`, `:placeholder-shown`, `:read-write`, `:open`, `:modal`, `:defined`, `:state()`, `:popover-open`), geometry (`getBoundingClientRect`), `matchMedia`, `CSS.supports`, `CSS.escape`, scrolling of the page and of containers |
| Editing | `Selection` and live `Range`s, `contenteditable` with the browser's edits through `beforeinput`/`input` (typing, Enter, Backspace and Delete by character or word, paste, cut), caret keys, text-field selection (`selectionStart`, `setSelectionRange`, `setRangeText`), `caretPositionFromPoint` and range rectangles from layout, `execCommand` for the editing commands (`insertText`, `delete`, `insertParagraph`, …; formatting commands return `false`). Editors such as ProseMirror, Lexical and Quill work |
| Forms | Every control as form state, named access (`form.email`, `form.elements.plan.value` for radio groups, `form[0]`), `action`/`method`/`enctype`, `valueAsNumber`, constraint validation with Chrome's messages, `:invalid`/`:user-invalid`, form-associated custom elements, `FormData`, multipart submission |
| Popovers | The Popover API (`popover`, `showPopover`/`hidePopover`/`togglePopover`, `beforetoggle` and `toggle`), `popovertarget` buttons, Escape and outside clicks close open popovers, `:popover-open`; views show only open ones |
| Canvas | 2D drawing and text (`font`, `textAlign`, `textBaseline`, `measureText`, `fillText`, `strokeText`) with the bundled fonts, so metrics are the same everywhere; canvases appear in screenshots |
| Stylesheets | `document.styleSheets`, `style.sheet` and `link.sheet`, `cssRules`, `insertRule`/`deleteRule`: rules a script inserts (styled-components, emotion) reach layout. Inserted `<link>` stylesheets, `preload` and `modulepreload` load and fire `load` or `error`. A `<link>`'s rules are not readable |
| Web components | Shadow DOM (open, closed, declarative), slots, custom elements, `ElementInternals`, adopted stylesheets |
| Frames and messaging | Same- and cross-origin iframes, also those scripts create (an `about:blank` window at once, then the `src` loads after the task; a sandbox without `allow-same-origin` gives an opaque, cross-origin window), a page's `load` after its frames, `postMessage`, `contentWindow`/`contentDocument` across same-origin frames, `<iframe sandbox>` (`allow-same-origin`, `-scripts`, `-forms`, `-popups`, `-modals`), `MessageChannel` within a document (ports are not transferable) |
| Networking | `fetch`, `XMLHttpRequest`, `AbortController`, CORS, cookies (`SameSite`, CHIPS), `document.cookie` |
| Storage | `localStorage`, `sessionStorage` and IndexedDB, kept per origin for the session across loads and tabs (IndexedDB: 50 MiB per origin) |
| Streams | `ReadableStream`, `WritableStream`, `TransformStream`, `TextDecoderStream`, `TextEncoderStream`, `Response.body` (a body arrives whole, in one chunk), `arrayBuffer()`, `bytes()` |
| Web Authentication | `navigator.credentials.create()`/`get()` for passkeys (ES256, `none` attestation) with keys held by the broker, `PublicKeyCredential` and its JSON helpers; see [passkeys](@/docs/secrets.md#passkeys) |
| Web Crypto | `crypto.subtle` on secure pages: SHA digests; HMAC, ECDSA P-256 and Ed25519 keys, signatures and verification; AES-GCM. Keys come from the session's randomness and ECDSA signs deterministically, so replays match. Other algorithms reject with `NotSupportedError` |
| Workers | Dedicated workers, classic and module, on the page's virtual clock: `postMessage` with `ArrayBuffer` transfer, `importScripts` (of the URLs a worker names as string literals, and `blob:`/`data:` URLs), timers, `fetch`, IndexedDB and the rest of the worker global; `blob:` worker scripts from `URL.createObjectURL`; at most 16 per page |
| Realtime | `WebSocket` and `EventSource`, held by the broker, under the same policy as requests |
| Files | `Blob`, `File`, `FileList`, `FileReader`, file inputs and drop zones (with staged files only), downloads |
| Drag and drop | Trusted pointer drags, HTML drag and drop, `DataTransfer`, `DragEvent` |
| Clipboard | One clipboard per session, never the host's: copy, cut and paste events with `clipboardData`, `navigator.clipboard.writeText` after a click or key; `readText` is refused, so pages read only what the agent pastes |
| History | `history.pushState`, `popstate`, fragments, `back`/`forward` with saved state |
| Animation and scheduling | Web Animations (`Element.animate`, `getAnimations`, `Animation`, `document.timeline`) and `scheduler.postTask`/`yield` with `TaskController`, on the virtual clock: a 300 ms exit animation finishes without waiting. Animated styles are not interpolated between keyframes; `commitStyles` writes the end state |
| Permissions | `navigator.permissions.query` and `Notification.permission`: nothing is ever granted (`prompt`, or `denied` after a request), except `clipboard-write` for the focused page as in browsers |
| Navigator and screen | `userActivation`, `sendBeacon` (a POST under the session's network policy), `hardwareConcurrency` (8), `pdfViewerEnabled` with the PDF viewer plugin list the HTML standard prescribes, `screen.orientation`, `screenX`/`screenY`. Pages see UTC |
| Other | Timers and `requestAnimationFrame` on the virtual clock, `crypto.getRandomValues`/`randomUUID`, `TextEncoder`/`TextDecoder`, `URL`, user timing (`performance.mark`/`measure`, `PerformanceObserver`), `screen`, `document.visibilityState`, `InputEvent`, `reportError`, `document.write` while the page parses, SVG elements, `<dialog>` with a modal top layer, `select.add`/`remove`, `DOMRect`, `document.fonts` (the bundled fonts, always loaded), WebMCP (`document.modelContext` and form tools), `SubmitEvent` with `submitter` |

## Not supported

`BroadcastChannel`, frames of frames that scripts create, a frame's own navigations, `blob:` URLs outside worker scripts,
shared, service and nested workers, transferring anything but `ArrayBuffer`s, IME composition, undo (`historyUndo`) and formatting commands, `designMode`,
`ClipboardItem`, `PasswordCredential` and `FederatedCredential`, the Navigation API (routers fall
back to `history`), `document.write` after the page has loaded (it throws), `FontFace` (`document.fonts` reports
the bundled fonts as loaded), `DOMMatrix`,
image sizes before painting (`naturalWidth`, `decode`), media playback, XML parsing (`DOMParser`
takes HTML only), canvas images, gradients and readback, WebGL,
Web Audio, showing notifications, WebRTC and WebTransport, HTTP/3.
For automation clients: WebDriver classic and request interception (use origin sets or a
recorded session instead).

## Honest identity

By default `navigator.webdriver` is `true`, the user agent names pageon, and robots.txt is
honoured. See
[identity](@/docs/identity.md).
