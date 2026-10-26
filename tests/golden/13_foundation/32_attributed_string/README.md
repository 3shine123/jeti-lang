# 32_attributed_string — NPAttributedString

**NPAttributedString** (`include/Foundation/NPAttributedString.hje/.jeti`) is the
`NSAttributedString` port, **v1 as a pure data layer**: the whole string carries
one attribute dictionary (a single run), and rendering is left to libui.

- `+attributedStringWithString:` / `+attributedStringWithString:attributes:`
  (factories; the `autorelease` sits in a `@noarc { }` block, the project's
  idiom for ARC mode).
- `-initWithString:` / `-initWithString:attributes:` — the text and the
  attribute dictionary are both **copied**, so mutating the originals afterwards
  does not reach into the attributed string.
- `-string` / `-length` / `-attributes` (a copy) / `-attribute:atIndex:` /
  `-attributesAtIndex:` / `-effectiveRangeLengthAtIndex:` / `-isValidIndex:`.

Honest boundaries (see the header): **single run** — `-attributesAtIndex:`
returns the whole-string dictionary for any valid index and
`-effectiveRangeLengthAtIndex:` returns the whole length. Per-range runs (the
same string styled differently in different spans) need a growable run array and
are a follow-up. No `-initWithAttributedString:`, no `-dataFromRange:` archiving,
and no `NSParagraphStyle`-style semantic attribute types; attribute names are
plain `NPString` keys with arbitrary object values, as in ObjC.

Verified here:

| case | checked |
|------|---------|
| F1 | construction and `-length` / `-string` |
| F2 | index validity (`0..len-1` valid, `len` invalid) |
| F3 | attribute lookup: hit, missing name → nil, out-of-range → nil |
| F4 | `-effectiveRangeLengthAtIndex:` = whole string (single run) |
| F5 | no-attributes construction → `-attributesAtIndex:` nil |
| F6 | copy semantics: mutating the source dictionary afterwards has no effect |

Output is deterministic — GOLDEN-MATCH line by line.

While building this class, the probe also surfaced a **static-dispatch
footgun** worth knowing about: a message sent to a class that does not declare
the selector is *not* a compile error as long as **some** class in the
translation unit declares it (selectors resolve globally, not per receiver
type). The call then dispatches through a NULL vtable slot and crashes at
runtime — here, `[immutableDict setObject:forKey:]` (that selector belongs to
`NPMutableDictionary`). Use the mutable class for mutation.
