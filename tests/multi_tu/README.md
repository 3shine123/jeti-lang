# Cross-TU link consistency suite

Run: `./run_multi_tu.sh` (filter with `./run_multi_tu.sh 05_block`) · `JETIC=path/to/jetic` to pick a binary.

## Why this suite exists

Jeti's uniform vtable is synthesised **per translation unit**: `struct jeti_vtable`
gets one member per instance method that TU happens to see. Plain C never hits
this because every struct is spelled out in the source, so C's type system
protects you. Jeti synthesises the layout, so nothing does.

When two TUs see different method sets they compile different layouts, while the
linker weak-merges the vtable *instances* into a single allocation. Dispatch
through the losing layout then reads the wrong slot — silent garbage, or a
segfault at a line that has nothing to do with the real cause.

This suite pins down, per language feature, whether it survives a cross-TU link.
The point is to turn "I discovered this three weeks later as a mystery crash"
into "the test told me this feature cannot cross a TU boundary."

## The rule

**Every instance method a class exposes publicly must be visible to every TU
that links against it** — declare them in the shared `.hje`. Since R3, a method
that exists only in an `@implementation` (TU-local private) no longer breaks the
link: R1 gives the private tail slots no other TU indexes, R2 keeps its dispatch
inside the owning TU, and the `__sig` fingerprint covers the shared segment only.

Codegen stamps an FNV-1a signature of that shared segment into the vtable struct
itself (`__sig`, first member) so it weak-merges together with the instance that
won, and a per-TU `__attribute__((constructor))` verifies it. A mismatch aborts
at load time with both signatures and this TU's method list — a loud failure
rather than a wrong-offset call. (`jeti_metaInit` is the wrong place for that
check: it is weak-merged, so only one TU's copy ever runs and would only ever
compare its own layout against itself.)

### Ownership (rule R2)

Metadata for a class is emitted **strong** by the TU whose **main file** holds
that class's `@implementation`, and weak in every other TU (a `@implementation`
reaching a TU through `#import` does not confer ownership — that is the
self-contained mode, where every TU inlines Foundation and the copies must keep
merging). Consequences:

* the owner's real table can never be displaced by another TU's stub, and
* two TUs that both `@implementation` the same class in their own main files
  are a **duplicate symbol at link time**.

`09_layout_mismatch` is the case that exercises this: it used to be caught by
the runtime `__sig` abort, and is now rejected earlier, at link. That is a
louder diagnostic, not a weaker one, so `EXPECT_FAIL_MATCH` looks for
`duplicate symbol`.

## Cases

| Case | What it pins down |
|---|---|
| `01_basic` | Plain cross-TU method dispatch through a shared `.hje`. |
| `02_inheritance` | Subclass allocated in the lib TU, driven through a base-class pointer — every send is a real cross-TU vtable dispatch, and the subclass's struct must physically embed the parent's ivars. |
| `03_generic` | Monomorphised generic. **Requires the lib TU to name the instantiation** (`Stack<int *>`) inside a method body or variable declaration — instantiation is collected by *usage*, so an `@implementation` that never mentions it emits no specialised struct or vtable. |
| `04_namespace` | `::` → `__` C symbol encoding must round-trip: the client names `Geo::Origin`, the lib defines it. |
| `05_block` | A Jeti block handed to a lib-TU method, stored, and fired later. The block must be `_Block_copy`d (libui has no unregister API; a stack block would dangle) and any captured-and-mutated variable must be `__block`. |
| `06_protocol` | A class conforming to a protocol gets vtable slots for the protocol's required methods **even when the class does not redeclare them**. This needed a fix: the required methods used to be dropped after binding, so a header-only TU compiled a layout with fewer slots than the implementation TU. |
| `07_arc` | Compile-time ARC insertion stays correct when the halves were transpiled separately — including a convenience-style `+1`-free return from the lib TU. |
| `08_class_method` | `+` methods dispatch through `JETI_META_VTABLE_$_X`, a *second* per-class layout that must agree across the link independently of the instance vtable. |
| `09_layout_mismatch` | **Negative case.** Both TUs `@implementation` the same class, so each owns it and the linker rejects the duplicate metadata (rule R2). Must fail loudly and mention the diagnosis — the contract is "fails legibly", not merely "fails", so a case that crashed for an unrelated reason does not pass by accident. Marked with `EXPECT_FAIL` + `EXPECT_FAIL_MATCH` (checked at link *and* at run). |
| `10_slots_manifest` | `--slots` manifest mode: the manifest defines the complete layout (methods unknown to it appended at the end, never renumbered), so TUs compiled with the same manifest link correctly even with different method sets. The manifest is also the `__sig` segment. |
| `11_vtable_private_slots` | The legal split R3 exists for: the lib implements `Widget` plus a TU-local private helper, the client sees only the shared `.hje` and defines its own `App`. Under R1+R2+R3 the shared segments agree (the `__sig` guard stays silent) and each TU dispatches its own privates — the case runs to `run=42`. |
| `12_native_multi_input` | The compiler's own multi-input mode (`jetic main.jeti lib.jeti -o app`): one command transpiles each TU (its own jetic subprocess), compiles, and links — no manual clang. Same cross-TU dispatch as `01_basic`; marked with a `NATIVE` file so the runner drives the native path. |
| `13_kvc_predicate` | KVC accessor tables ride the *same* metadata merge: the lib TU owns `Widget` and emits the strong `JETI_KVC_$_Widget` table while the client sees only the `.hje`. If the client's weak metadata won, `size` / `label` would resolve to nothing and the predicates would quietly print `0` instead of the expected hits. |
| `14_category_present` / `15_category_absent` | Category methods across TUs: the category TU contributes method bodies without claiming class ownership; present links and absent fails clearly. |
| `16_category_protocol` | A category TU adds a protocol conformance to an owner class. The category constructor merges its protocol list into the owner's static metadata, and protocol symbols are weak-coalesced across TUs so `@protocol(P)` has one link-unit identity. |

## Writing a case

```
NN_name/
  model.hje      shared declarations — the only channel between the TUs
  lib.jeti        "library" TU: defines the classes
  main.jeti       "client" TU: sees only model.hje, has main()
  expected.txt  exact stdout the program must print (optional)
```

Notes that cost time when the cases were first written:

- Ivars belong on the `@interface`. An `@implementation` ivar block is not merged
  into the class layout, so the methods referencing them fail to compile.
- `id` is a lexer keyword — `- (int)id;` does not parse. Name it `widgetId`.
- ARC rejects a bare `[x autorelease]`. Use `@noarc { }`, or have the factory
  method return `+1` and let the caller release.
- Most cases need `#import <Foundation/Foundation.hje>`: the generated C gets
  `NPObject` / `SEL` / `JETI_CLASS_$_*` from the C headers Foundation inlines.

## Not covered

- Category-added protocols that are not declared in a shared category interface are only visible to the category TU; put the category method declaration in the shared `.hje` to keep the public vtable segment identical.
- Generic monomorphisation triggered only from a class-method return type.
