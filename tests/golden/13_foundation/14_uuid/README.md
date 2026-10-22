# 14_uuid — NPUUID

**NPUUID** (`include/Foundation/NPUUID.hje/.jeti`) is the `NSUUID` port:
RFC 4122 v4 random UUIDs, 128 bits stored as two `uint64_t` halves (no
array ivars — they hit a codegen boundary).

- `+UUID` / `-init` — new random v4 UUID. Hosted entropy is `/dev/urandom`;
  `-ffreestanding` has no entropy source and falls back to a predictable
  counter+xorshift mix (shape-valid, version/variant bits stamped) — see the
  honest boundary in `NPUUID.hje`.
- `-initWithUUIDString:` — parses the canonical 36-char form
  (8-4-4-4-12, dashes required, hex case-insensitive); invalid input
  returns nil, ObjC-aligned.
- `-UUIDString` / `-description` — uppercase canonical form.
- `-isEqual:` — value equality over the 128 bits.

Verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | shape: 36 chars, dashes at 8/13/18/23, all hex, version `4`, variant in `[89AB]` |
| A2 | two `+UUID` calls differ |
| A3 | roundtrip `UUIDString → parse → UUIDString` is identity; lowercase input accepted, output stays uppercase |
| A4 | `-isEqual:` value equality across objects; distinct UUIDs unequal |
| A5 | invalid inputs → nil: empty / short / misplaced dash / bad hex / too long |

Output is deterministic (checks, not the random bytes themselves) —
GOLDEN-MATCH line by line.

Naming note: the parse helper is `np_uuid_parse_canonical` — a bare
`uuid_parse` collides with a system symbol and silently dispatches to the
wrong implementation (no diagnostic; renamed and it worked). Foundation
`.jeti` file-local C helpers are prefixed `np_<class>_…`.
