# 33_keyed_archive — NPKeyedArchiver + NPKeyedUnarchiver

**NPKeyedArchiver** / **NPKeyedUnarchiver**
(`include/Foundation/NPKeyedArchiver.hje/.jeti`,
`NPKeyedUnarchiver.hje/.jeti`) are the `NSKeyedArchiver` / `NSKeyedUnarchiver`
ports. The v1 carrier format is **JSON**, reusing `NPJSONSerialization`: the
archiver writes a keyed tree and `-finishEncoding` turns it into `NPData`.

- `-encodeObject:forKey:` / `-encodeInt:forKey:` / `-encodeDouble:forKey:` /
  `-encodeBool:forKey:` — typed writers; `nil` is a no-op (as in ObjC).
- `-finishEncoding` — JSON bytes; it may only be called once.
- `+archivedDataWithRootObject:` — encode a JSON-representable root directly.
- `-initForReadingWithData:` — parses the archive; a root that is not a keyed
  tree is a bad archive (nil).
- `-decodeObjectForKey:` / `-decodeIntForKey:` / `-decodeDoubleForKey:` /
  `-decodeBoolForKey:` / `-containsValueForKey:` — a missing key or a
  type mismatch yields the zero value (0 / 0.0 / NO / nil) rather than throwing,
  so callers check `-containsValueForKey:` first. This differs from ObjC, which
  raises `NSInvalidUnarchiveOperationException`.

Honest boundaries (see the headers): **not compatible with ObjC's binary
`NSKeyedArchiver` format** (theirs is a binary plist with class names and a
reference table; this is JSON — the two cannot read each other; use
`NPJSONSerialization` for cross-language exchange). Only JSON-representable
values round-trip (`NPString` / `NPNumber` / `NPArray` / `NPDictionary` /
`NPNull`); other objects (`NPDate`, `NPData`, custom classes) must be encoded by
the caller. There is no `NPCoding`-style object graph: `-encodeObject:` does not
walk an object's ivars and there is no reference-sharing or cycle handling.

Verified here:

| case | checked |
|------|---------|
| G1 | typed round-trip: object / int / double / bool |
| G2 | `-finishEncoding` only once |
| G3 | missing key → `NO` and zero values |
| G4 | type mismatch (string read as int) → 0 |
| G5 | root-object archive round-trip |
| G6 | bad archive (root is an array) → nil |

Output is deterministic — GOLDEN-MATCH line by line.
