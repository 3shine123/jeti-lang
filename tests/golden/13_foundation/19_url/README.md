# 19_url — NPURL (parse-only subset)

**NPURL** (`include/Foundation/NPURL.hje/.jeti`) is the **pure-parsing
subset** of `NSURL`: `scheme://host[:port]/path?query#fragment` split and
rebuilt as strings. No network, no percent-decoding — those are the honest
boundaries (below).

Honest boundaries (in the header):

- Requires the `scheme://authority` shape; scheme-less inputs
  (`mailto:x`, relative refs) → nil from `+URLWithString:`.
- **No percent-decoding/encoding**: `path`/`query` are raw byte segments
  (ObjC's `path` decodes `%41` → `A`).
- **userinfo is dropped** (`user:pass@host`); **no IPv6 literals**
  (`[::1]` mis-parses the host — unsupported in v1).
- No relative-URL resolution, bookmarks, or file-reference URLs; no
  network semantics (that is NPURLSession, gap-plan P3).

Implementation notes (two real bugs the probe caught):

- **Port over-read**: `initWithUTF8String:` takes `strlen` — passing the
  bare `colon + 1` pointer into the middle of the input swallowed the rest
  of the URL into the port string (`"8080/a/b?…"`). The port is now copied
  out against the `auth_end` boundary first. (`A4` passed by luck — its
  port sat at the end of the string; `A1`'s mid-string port exposed it.)
- **Component ivar ownership**: components are built with
  `alloc initWithUTF8String:` (not the autorelease `stringWithUTF8String:`)
  and released in `dealloc` — matching NPScanner's ivar-ownership pattern.
- `absoluteString` rebuilds via `stringByAppendingUTF8String:` chains
  (snprintf pointer arithmetic proved unreliable in generated C); C string
  literals must go through the `*UTF8String:` variant —
  `stringByAppendingString:` takes an `NPString *` and a literal there
  segfaults at runtime.

Verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | full component parse (scheme/host/port/path/query/fragment) + `absoluteString` rebuild |
| A2 | missing components → nil; `absoluteString` minimal form |
| A3 | invalid inputs → nil: no `://`, empty host, bare scheme |
| A4 | port: digit spelling → `NPNumber`; explicit empty port (`host:`) → nil |
| A5 | userinfo dropped |
| A6 | separators: path/query/fragment cut points |

Output deterministic — GOLDEN-MATCH line by line.

Probe: `probes/x14_npurl/`.
