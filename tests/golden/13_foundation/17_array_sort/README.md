# 17_array_sort — comparator sort APIs

**`sortedArrayUsingComparator:`** (`NPArray`) and **`sortUsingComparator:`**
(`NPMutableArray`) — the `NSComparator`-shaped sorts (`NSComparator` shape:
the block returns `<0 / 0 / >0`), plus **`NPNumber -compare:`** (the
three-way compare NSNumber has, missing until now).

Implementation notes:

- **Stable bottom-up merge sort**, iterative, entirely inside the method
  body. A file-scope C helper was tried first and its BLOCK parameter is
  dropped by the file-level function emitter (the block-ivar drop's
  sibling) — method bodies call block parameters fine (NPTimer precedent).
- Left-first merge on ties keeps equal elements in insertion order.
- Nil comparator: `sortedArrayUsingComparator:` returns an unsorted copy;
  `sortUsingComparator:` leaves the array unchanged (the container API's
  nil-tolerant stance).
- `NPNumber -compare:` splits on the same precision line as
  `isEqualToNumber:`: exact 64-bit ordering for integer-vs-integer, double
  path otherwise.
- Comparators are block literals at call sites (a plain C function is not a
  block — the vtable parameter rejects it).

Verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | numeric sort via `compare:` ascending, duplicates kept |
| A2 | string sort, lexicographic |
| A3 | stability: equal-key elements keep insertion order (two-field objects) |
| A4 | nil comparator: unsorted copy / untouched mutable array |
| A5 | in-place `sortUsingComparator:` sorts the receiver |
| A6 | empty array and single element: no-op, no crash |

Output deterministic — GOLDEN-MATCH line by line.

Probe: `probes/x12_array_sort/`.
