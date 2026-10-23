# 20_json — NPJSONSerialization

**NPJSONSerialization** (`include/Foundation/NPJSONSerialization.hje/.jeti`)
is the `NSJSONSerialization` subset: JSON ↔ Foundation objects
(`NPDictionary`/`NPArray`/`NPString`/`NPNumber`/`NPNull`), any top-level
value (RFC 8259). Parser is a recursive-descent C scanner; serialization is
compact form with keys in insertion order (NPDictionary preserves it).

Honest boundaries (in the header):

- Numbers: integers take the exact `long long` path; `.`/`e`/`E` forms take
  the double path (ObjC also reads doubles as double).
- Errors are **nil, no NSError out-param** in v1.
- Serialization rejects unsupported object-graph types with nil (ObjC throws).
- `dataWithJSONObject:`-style output is compact; non-ASCII bytes pass
  through as UTF-8 (ObjC default too).
- No `NSJSONReading/Writing` option bits.

Implementation notes:

- String escapes: `\" \\ \/ \b \f \n \r \t \uXXXX` incl. **surrogate-pair
  synthesis** to UTF-8 (`\ud83d\ude04` → 😄); lone surrogates are errors.
- `true`/`false` round-trip faithfully: `NPNumber` gained an `_isBool` ivar
  (set by `initWithBool:`, zeroed in the other init paths) — without it
  `true` came back as `1`, while ObjC preserves the bool kind.
- `np_json_*` file-scope helpers take **no block parameters** (the
  file-level emitter drops them — golden 17_array_sort note); scalar-only
  state travels in the parser struct.
- Trailing garbage after the top-level value → nil (`ps.pos != ps.len`).

Verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | scalars: int / double / true / false / null / string |
| A2 | containers: array, object, nested mixed (`[true,null]` inside an object) |
| A3 | escapes: quote/backslash/solidus/`\n`/`\t`/`\u0041`/`\u4e2d`, surrogate pair |
| A4 | error paths: truncated, trailing garbage, bad escape, bare identifier, missing colon |
| A5 | serialize: compact, keys in insertion order, bool fidelity, control-char escaping |
| A6 | round-trip: parse → serialize → parse, values and kinds intact |
| A7 | depth cap (>64 → nil) and the `JSONObjectWithData:` entry (NUL-safe copy) |

Output deterministic — GOLDEN-MATCH line by line.

Probe: `probes/x15_json/`.
