# ARC over-released an interned string (heap-use-after-free)

> Status: **fixed** (2026-10-02). Regression guard:
> `tests/arc_intern/` (`run_arc_intern_test.sh`).
>
> Found while validating the stable-slot / `-fstrong-metadata` work, but it is
> unrelated to it and reproduced on a clean `HEAD`. It was the single `SUSPECT`
> in `test_all`'s ARC→MRC retry accounting; that is now zero.

## Symptom

`tests/full_syntax_test.jeti` ran fine most of the time but intermittently died
with **no stdout at all** and a signal exit — `jetic run` reported exit 1
because `.code()` is `None` for a signal; the raw binary exited 139. It looked
like a startup crash.

The "no output" was a red herring: stdout is block-buffered and the abort
discards the buffer, so the program had in fact run partway. Under ASan it got
far enough to print, and the real defect surfaced.

## Root cause

`jeti_stringFromCstr` interns `@"..."` literals into a static table. Before the
fix it **aliased** the object's initial `+1` instead of taking a reference of
its own, despite the comment claiming *"The table owns its +1 forever"*:

```c
NPObject *obj = jeti_alloc(&JETI_CLASS_$_NPString);   /* refcount 1 */
...
if (jeti_intern_count < 256) {
    jeti_intern_table[jeti_intern_count].cstr = str->_cstr;
    jeti_intern_table[jeti_intern_count].obj  = obj;   /* <-- no retain */
    jeti_intern_count++;
}
```

Now consider a **direct ivar assignment** of a literal:

```jeti
sp->_tag = @"t";          /* emitted: sp->_tag = jeti_stringFromCstr("t"); */
```

ARC does **not** retain here (the RHS is treated as borrowed/`+0`), but the
synthesised ARC dealloc **does** release every owned object ivar:

```c
static void FsSprite__jeti_arc_dealloc(NPObject *self, SEL _cmd) {
    NPObject_dealloc(self, _cmd);
    jeti_release(((struct FsSprite *)self)->_tag);     /* 1 -> 0 */
    jeti_release(((struct FsSprite *)self)->_label);
}
```

So the release consumed the *intern table's* reference, `NPString_dealloc` freed
`_cstr`, and the table was left holding a dangling `const char *`. The next
interning lookup walked that table and `strcmp()`d freed memory:

```c
for (int i = 0; i < jeti_intern_count; i++)
    if (jeti_intern_table[i].cstr == cstr || strcmp(jeti_intern_table[i].cstr, cstr) == 0)
```

A second, broader reading: the same imbalance would over-release *any* `+0`
object whose only owner is an ivar, but ordinary objects usually have another
owner so the count never reaches zero. Interned constants are the case where
the ivar is the sole reference, which is why they were where it detonated.

## ASan report (from the reproducer)

```text
==ERROR: AddressSanitizer: heap-use-after-free ... READ of size 2
    #0 strcmp
    #1 jeti_stringFromCstr
    #2 main
freed by:
    #1 NPString_dealloc
    #2 jeti_release
    #3 Holder__jeti_arc_dealloc
    #4 jeti_release
    #5 main
previously allocated by:
    #1 jeti_stringFromCstr
    #2 main
SUMMARY: AddressSanitizer: heap-use-after-free in jeti_stringFromCstr
```

`tests/full_syntax_test.jeti` showed the identical stack with `sec2_objects` /
`sec3_expressions` in place of `main`.

## Fix

`crates/codegen/src/codegen.rs`, the hosted branch of `jeti_stringFromCstr`,
immediately before the table store:

```c
jeti_retain(obj);   /* the table owns its +1 forever */
```

## Verification

* `tests/full_syntax_test.jeti` under ASan (`JETI_CC="clang -fsanitize=address -g -O0"`):
  runs to `ALL SECTIONS DONE`, exit 0, **zero** AddressSanitizer reports
  (previously: heap-use-after-free, exit 134).
* `jetic run tests/full_syntax_test.jeti -asm tests/full_syntax_test.s -I tests`:
  exit 0 with all 86 lines (previously exit 1 and zero output).
* `test_all` ARC→MRC `SUSPECT` count: **1 → 0**.
* `tests/arc_intern/run_arc_intern_test.sh` asserts the defect stays away.

## Considered alternative

Make ARC retain on assignment to an owned object ivar when the RHS is not `+1`
(proper ARC balancing). That is the more general fix, but broader and riskier;
the one-line table fix restores the documented invariant and has been shown
sufficient here.
