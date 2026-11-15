# pageon-net

pageon's browser networking. Every request a page makes passes through one place:
- documents, stylesheets, scripts and frames;
- `fetch` and `XMLHttpRequest`;
- the handshakes of `WebSocket` and `EventSource` (cookies, `Origin`, the egress checks).

That place applies the browser's rules over [`pageon-env`](../pageon-env) (`Live`, `Recording`
or `Replay`).

| Area | What it does |
| --- | --- |
| Egress | Private and link-local destinations are **refused by default**, in every IPv4/IPv6 encoding and for `localhost`/`*.localhost`. The check runs on the URL (so it also holds in replay), on every redirect hop, and in `Live` on what DNS answers. One option allows them. Mixed content (`http:` from an `https:` page) is blocked |
| Fetch | Redirects (`follow`, `error`, `manual`), CORS with preflight, response tainting (`basic`, `cors`, `opaque`, `opaqueredirect`), forbidden request headers |
| Headers | Honest browser headers: `Accept` per destination, `Origin`, `Referer` (strict-origin-when-cross-origin), `Sec-Fetch-Site`/`-Mode`/`-Dest`/`-User` |
| Cookies | RFC 6265bis as Chromium ships it: `SameSite=Lax` by default, `None` needs `Secure`, `__Secure-`/`__Host-` prefixes, Public Suffix List rejection, partitioned cookies (CHIPS) keyed by the top-level site, limits. Cookies import and export in Playwright's shape (`storageState`) |
| Storage | `localStorage`/`sessionStorage` areas per origin, 5 MiB each |
| Cache | Fresh `GET` scripts and stylesheets (explicit `max-age`/`Expires`; never `no-store`/`no-cache`), served without a request and marked `(cache)` in the log. No revalidation |
| Log | Each request with its destination, status, outcome and rejected cookies, so callers see what a page fetched and what was refused, and why |

It is deterministic. Time is passed in by the caller (the page's virtual clock), and the only I/O
is the `Env` argument. Loads stay replayable byte for byte.

Evidence:
- the WPT `http-state` cookie cases, 161/161;
- a 23-case fetch and CORS matrix;
- a private-network matrix of 25 destinations, each reached in 7 ways, with 0 escapes;
- live tests for the DNS filter and the proxy;
- 10,000 fuzzed cookies and URLs.

MIT OR Apache-2.0.
