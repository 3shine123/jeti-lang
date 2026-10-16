# arc_intern — regression guard for the ARC intern-table use-after-free

```bash
./tests/arc_intern/run_arc_intern_test.sh
```

Fixed 2026-10-02. Full analysis: `docs/plans/arc_intern_uaf.md`.

An interned `@"..."` constant assigned straight to an object ivar was freed by
the synthesised ARC dealloc (which releases owned ivars), leaving
`jeti_stringFromCstr`'s intern table pointing at freed memory; the next intern
lookup `strcmp()`d it. `full_syntax_test.jeti` hit this as an intermittent
`SUSPECT` (exit 1 with no output — the abort discards buffered stdout).

The fix: the intern table now takes its **own** reference (`jeti_retain(obj)`)
when it stores an object, instead of aliasing the object's initial `+1`.

## Why this is out-of-band

AddressSanitizer is required — a plain build may crash, may not, and loses
stdout either way, so it cannot be a `test_all` case. It also must not be left
to "ARC failed → retry under MRC", because the MRC retry would pass and hide it.

The runner asserts the *absence* of the defect:

| | |
|---|---|
| `exit 0` | no sanitizer report, exit code 0, output `k=second` |
| `exit 1` | the UAF is back (or the repro stopped exercising it) |
