# strong_metadata — metadata-linkage and class-ownership protection tests

Guards the precompiled-library (P3) contract (`doc/stable_slots_plan.md` §8) and
the per-class ownership rule R2 (§9).

```bash
./tests/strong_metadata/run_strong_metadata_test.sh
```

Seven checks, because "it printed the right thing" does not by itself prove the
metadata linkage is correct:

| # | What | Why it matters |
|---|---|---|
| 1 | `nm` shows the library's metadata as **strong** | the real table must outrank a client's stub, by object-file evidence — not by luck of link order |
| 2 | a declaration-only client links against the strong library and runs | the actual P3 end-to-end path |
| 3 | the deleted `-fstrong-metadata` spelling is rejected with `Unknown argument` | plan A removed the manual switch — ownership is by construction (R2), so a legacy script carrying the flag must fail loudly, not half-work |
| 4 | two TUs that both `@implementation` the same class fail with `duplicate symbol` (**no flag involved**) | rule R2: ownership is derived from the main file, so a second owner must never merge silently |
| 5 | a standalone TU — a self-contained `.hje` **or** a `.jeti` holding an `@implementation` — emits **strong** metadata for its owned class **with no flag** (vtable + meta vtable, `nm`-verified) | the flag must never be something to remember by hand: R2 ownership is derived from the main file, so the standalone-TU workflow is automatic |
| 6 | a declaration-only `.hje` stays **weak** with no flag | its NULL-filled stubs are the client side of R2 and must lose to the owner's table |
| 7 | a main file that owns the root classes (`NPObject.jeti` compiled standalone) compiles clean — `jeti_root`/`NPObject` defined **exactly once** in the generated file | stable-slots §9 blocker 1: Section 4's guarded fallbacks used to be re-emitted unguarded by the class-layout section — a hard C redefinition error |

There is no flag left to remember: the old `-fstrong-metadata` was deleted when
plan A landed (per-TU Foundation build, doc/stable_slots_plan.md §11), and
check 3 pins that deletion. Checks 4–6 cover the automatic side of the same
rule: R2 makes a duplicate implementation loud (4), makes standalone TUs
automatically strong (5) and keeps declaration-only clients weak (6). Check 7
pins the §9 struct dedup. The per-TU library build is the only way a library TU
gets strong metadata — implementations reached through `#import` still do not
confer ownership.

Plan-A link semantics worth knowing: a client that re-implements a library
class against the per-TU *archive* is standard C override semantics — the
library's member stays dormant unless something references its symbols, so
nothing collides at link time (the duplicate-symbol loudness of check 4
applies to explicit link inputs, e.g. two TUs in one `jetic main.jeti lib.jeti`
command). The override is partial by construction: slots for methods the
client did not implement stay NULL, so such a client must implement everything
it dispatches.

Out-of-band, like `tests/multi_tu` — it needs several TUs compiled separately
plus a static library, which `test_all.py` (one TU per `.jeti`)
cannot express. The run-time half of the same story lives in
`tests/multi_tu/09_layout_mismatch` (two owners, checked at link *and* run).

## Relationship to R2

Under R2 a class's metadata is emitted **strong** by the TU whose main file holds
that class's `@implementation`, and weak in every other TU. An
`@implementation` reaching a TU through `#import` does **not** confer ownership —
that is what keeps the self-contained mode (every TU inlines Foundation, and the
repeated copies must keep merging weakly) working unchanged.
