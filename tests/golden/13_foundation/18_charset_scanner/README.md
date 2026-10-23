# 18_charset_scanner — NPCharacterSet / NPMutableCharacterSet / NPScanner

**NPCharacterSet** (`include/Foundation/NPCharacterSet.hje/.jeti`) is the
`NSCharacterSet` port: a 256-entry bitmap (pointer ivar, calloc'd — a
fixed-size array ivar hits the codegen ivar boundary, NPUUID precedent).

**NPScanner** (`include/Foundation/NPScanner.hje/.jeti`) is the `NSScanner`
port: holds a string + scan position; every `scan*` consumes the skip set
first (default: whitespace + newlines, ObjC-aligned; nil disables skipping)
and **does not advance on failure**.

Honest boundaries (in the headers):

- **Byte/ASCII domain** (0..255): `letterCharacterSet` is ASCII letters, NOT
  ObjC's Unicode letter category; unichar/UTF-16 surrogates and locale
  variants are deferred until needed.
- No `scanHex*` / `scanUUID` / locale spellings in v1.

Implementation notes:

- `np_char_range` / `np_addRange:` / `np_removeRange:` — `np_`-prefixed
  internals (bare names colliding with system symbols silently dispatch
  wrong — golden 14_uuid note).
- Scanner private methods are declared in the header so the same-TU checker
  sees them; the digit helper is `np_scanNumber:dbl:` — a `double:` selector
  segment collides with the C type keyword in the parser.
- `scanUpTo*` out-params carry the CONSUMED PREFIX (`substringWithRange:`),
  not the tail — `substringFromIndex:` was the wrong primitive on the first
  draft. `scanUpToString:` consumes to the END and succeeds when the stop
  string never appears (NSScanner's documented behavior).
- `scanDouble:`'s fraction loop must consume the digits AFTER the dot (the
  first draft jumped dot + first digit together — "3.5" scanned as 3.0).

Verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | builtin sets: digit/letter/alnum/whitespace boundaries (newline excluded from whitespace) |
| A2 | `characterSetWithCharactersInString:`, mutable add/remove (range + string), `invert` |
| A3 | `scanInt` / `scanLongLong` / `scanDouble`: skip set, signs, fraction, 64-bit value |
| A4 | `scanUpToCharactersFromSet:` + `scanString:` + `scanUpToString:` consuming prefixes |
| A5 | exact `scanString:` match, position advance, `isAtEnd` |
| A6 | failed scans do NOT advance; `intoString:` out-param set to NULL |

Output deterministic — GOLDEN-MATCH line by line.

Probe: `probes/x13_charset_scanner/`.
