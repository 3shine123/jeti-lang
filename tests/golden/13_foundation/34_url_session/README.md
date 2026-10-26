# 34_url_session — NPURLSession

**NPURLSession** (`include/Foundation/NPURLSession.hje/.jeti`) is the
`NSURLSession` port, v1 as a **synchronous GET** over POSIX sockets (hosted).

- `+sharedSession` — one shared session; `-setTimeoutSeconds:` / `-timeoutSeconds`
  (default 30; a non-positive value falls back to the default).
- `-dataWithContentsOfURL:` — performs `GET <path>[?query] HTTP/1.0` with
  `Host:` and `Connection: close`, resolves the host with `getaddrinfo`,
  connects, and returns the response body as `NPData`. Failures (unsupported
  scheme, resolve/connect/send/receive failure) return nil.
- `-stringWithContentsOfURL:` — the same, decoded as UTF-8.
- Response bodies are delimited by `Content-Length`, `Transfer-Encoding: chunked`
  (decoded), or — when neither is present — by connection close.

Honest boundaries (see the header): **`http://` only** — there is no TLS, and
`https://` is rejected outright rather than silently downgraded to cleartext;
**synchronous, no task model** (no `NSURLSessionTask`, delegate, or callback —
in the cooperative single-thread runtime the call blocks the whole event pump);
**no redirects, cookies, auth, or proxy** (3xx is returned as-is); and it is
unavailable under `-ffreestanding` (no sockets).

Verified here (deterministic, offline — the golden must not depend on a
network):

| case | checked |
|------|---------|
| H1 | `+sharedSession` identity; timeout default 30, round-trip, non-positive → default |
| H2 | `https://` rejected (no silent downgrade) |
| H3 | unsupported scheme (`ftp://`) → nil |
| H4 | nil URL → nil |

The real network path is verified by
`probes/p3_urlsession/probe_urlsession.jeti` against a local
`python3 -m http.server`: a 200 response body comes back byte-exact, and a
refused connection returns nil.

Output is deterministic — GOLDEN-MATCH line by line.
