# 15_indexset — NPIndexSet / NPMutableIndexSet

**NPIndexSet** (`include/Foundation/NPIndexSet.hje/.jeti`) is the
`NSIndexSet` / `NSMutableIndexSet` port: a sorted table of disjoint
`[start, start+len)` ranges (two parallel `size_t` arrays), ObjC-aligned.

- `+indexSet`, `+indexSetWithIndex:`, `+indexSetWithIndexesInRange:` —
  ranges are `NPRange` (location + length, half-open), same shape as
  `NSRange`.
- `-count` is the **element** count (sum of range lengths), ObjC-aligned —
  not the range count.
- `-containsIndex:`, `-firstIndex`, `-lastIndex` (`NPNotFound` when empty),
  `-intersectsIndexesInRange:`.
- `-enumerateUsingBlock:` — ascending enumeration (v1 has no `stop`
  parameter; reverse / ranged enumeration deferred until needed).
- Mutable side: `-addIndex:`, `-addIndexesInRange:` (adjacent ranges merge
  automatically), `-removeIndex:`, `-removeIndexesInRange:` (splits ranges
  spanning the removal), `-removeAllIndexes`.
- `-isEqual:` compares the range table (works across
  `NPIndexSet`/`NPMutableIndexSet`).

Verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | empty set `NPNotFound`; single index; range construction; count = elements |
| A2 | `containsIndex:` hit / boundary / miss |
| A3 | adjacent ranges merge into one segment; disjoint add starts a new segment |
| A4 | removing a middle range splits a segment; partial-overlap removal |
| A5 | ascending block enumeration, strictly increasing |
| A6 | `isEqual:` same-shape tables equal across mutable/immutable; different shapes unequal |
| A7 | `intersectsIndexesInRange:` intersecting / adjacent-but-disjoint / empty range |

Output is deterministic — GOLDEN-MATCH line by line.

Probe: `probes/x9_npindexset/`. Note: `__block`-captured C arrays are not
subscriptable inside a block closure yet (capture-cell limitation) — the
golden asserts via scalars instead.
