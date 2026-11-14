# pageon-env

pageon's single seam for nondeterminism. Engine code gets network responses, the current time
and random numbers only through the `Env` trait, and a clippy lint enforces it. There are three
implementations:

| Implementation | What it does |
| --- | --- |
| `Live` | HTTP/1.1 and HTTP/2 over rustls, with an honest pageon user agent; redirects are not followed, so the engine records every hop. Refuses the private network unless a request allows it: URL literals, and every DNS answer (no rebinding). Proxies from `HTTP(S)_PROXY`/`NO_PROXY` or `Live::with_proxy` |
| `Recording` | Wraps `Live` and writes everything it served to a readable JSON file |
| `Replay` | Serves only the recording, byte-identically and offline. A request that was not recorded is an error, never a network call |

Requests carry their method, headers and body, and recordings keep request bodies.

`Live` speaks for one of two identities:
- **`agent`** (default): an honest pageon user agent; with an `Identity`, every request is signed
  with Web Bot Auth (RFC 9421, Ed25519) so sites can verify who is calling;
- **`browser-compatible`** (feature `chrome`): one pinned Chrome, coherent across the TLS
  ClientHello, HTTP/2 settings, headers and client hints.

`Env` is also the place to intercept or mock requests: wrap any implementation and answer, block
or rewrite what it is asked for. [`pageon-net`](../pageon-net) applies the browser's rules
(cookies, CORS, egress) on top.

MIT OR Apache-2.0.
