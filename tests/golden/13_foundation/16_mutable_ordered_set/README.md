# 16_mutable_ordered_set — NPMutableOrderedSet

**NPMutableOrderedSet** (`include/Foundation/NPMutableOrderedSet.hje/.jeti`)
is the `NSMutableOrderedSet` port: `NPOrderedSet`'s positional querying over
`NPSet`'s storage (`_items`/`_count`/`_capacity`), with mutators.

Order semantics (NSOrderedSet-aligned):

- add keeps **first-insertion** position — re-adding an existing element is a
  no-op on both membership and order;
- removal is a **dense shift** (the tail moves down), indices stay contiguous;
- `insertObject:atIndex:` puts a NEW element at the position pushing the rest
  back; an equal element already in the set is a no-op (its position does not
  move — same rule as re-adding);
- `exchangeObjectsAtIndexes:and:` swaps two positions; out-of-bounds is a
  no-op.

Verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | `initWithArray:` dedupes keeping first-insertion order; positional queries |
| A2 | re-adding an existing element does not move it |
| A3 | `removeObjectAtIndex:` / `removeObject:` shift the tail, indices contiguous |
| A4 | `insertObject:atIndex:` positions a new element, existing element is a no-op |
| A5 | `exchangeObjectsAtIndexes:and:`; out-of-bounds no-op |
| A6 | inherited `count` / `containsObject:` / `allObjects` (NPSet semantics) |

Output deterministic — GOLDEN-MATCH line by line.

Probe: `probes/x11_npmutableorderedset/`.
