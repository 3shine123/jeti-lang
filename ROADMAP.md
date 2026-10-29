# ROADMAP

> What is **actually not done yet**. Completed features are not listed here.
> For the current architecture see `docs/plans/architecture.md`; historical decision
> records live in `docs/plans/stable_slots_plan.md` and `docs/plans/arc_intern_uaf.md`.

## Language / compiler gaps

- [ ] **Source locations, debug info, and LSP foundation** — shared `SourceSpan` (CST/AST nodes carry 1-based line + byte columns + half-open end), UTF-16 conversion, `#line` emission, `-g`/`-O0` debug builds and the versioned `.jeti.map` sidecar are in. **The LSP serves diagnostics, an outline, go-to-definition, hover, completion and signature help** (`crates/lsp`, binary `jeti-lsp`; VS Code client wired): one `Pipeline::analyze_document` run per edit feeds `publishDiagnostics` plus a symbol index that answers `textDocument/documentSymbol`, `definition`, `hover`, `completion` and `signatureHelp` (no request re-parses the buffer), with every position projected through `SourceSpan::to_lsp_range`. Completion covers global names *and* the receiver's members (through the class chain, `[Worker ` for class methods, `obj.` too). Still open: `id<P>` protocol members and generic argument substitution in completion, rename, caching/coalescing of analyses (projects can already state their own analysis flags through `compile_commands.json` / `compile_flags.txt`) (incremental sync is in, but every edit still re-runs the front end), workspace folders, ARC-only diagnostics, and the debugger adapter that consumes `.jeti.map` for Jeti breakpoints/stack frames. Plan: `docs/plans/source_locations_debug_lsp_plan.md`.
- [x] **Generic class-method return instantiation (single TU)** — `[Box<NPString *> defaultValue]`
  substitutes T in the checker's result type and in the specialized vtable fn-ptr casts;
  ARC ownership on the returned T is correct (no extra release at the call site).
  Evidence: `probes/genret/` g1 (multi-instantiation), g3 (function pointers), g4 (ARC).
- [x] **Generic class-method return across TUs** — a multi-input build now collects
  concrete specializations from all `.jeti` inputs and forwards the demand to the TU
  containing the generic implementation, so `[Factory<NPString *> make]` no longer
  needs a dummy variable in the library TU. This remains true monomorphization: the
  generated class, methods, vtable, metadata, and ABI are specialized per argument
  list. A separately precompiled library can only provide the specializations it
  shipped; it cannot invent a new method body for an unseen type. See `probes/genret/g2/`
  and the cross-TU generic section in `README.md` / `CHINESE.md`.
- [x] **Categories across TUs** — implemented (weak-definition/strong-extern vtable slots, ownership excluded for category impls); see `docs/plans/categories_protocol_plan.md` §8 and `tests/multi_tu/14`/`15`.
- [x] **Protocol inheritance across TUs** — implemented: weak-coalesced `NPProtocol` identity, static metadata + `conformsToProtocol:` runtime query. Category-only conformances are merged by a constructor registration path (`tests/multi_tu/16_category_protocol`).
- [ ] **Pattern switch M2** — mixing plain constant arms with pattern arms in one `switch` (currently rejected with a clear error); exhaustiveness analysis.

## Foundation library gaps

- [x] **Container classes** — `NPSet` / `NPMutableSet` / `NPOrderedSet` exist
  (isEqual: value semantics, insertion order preserved by NPOrderedSet;
  mutator slots live on NPSet so the subclass vtables resolve — see the
  notes in `NPSet.hje`).
- [x] **`NPPredicate`** (the `NSPredicate` model) — shipped: a runtime
  format-string parser + evaluation engine (`NPPredicate.jeti`), compile-time KVC
  accessor tables (`JETI_KVC_$_X`, see `docs/plans/architecture.md` §12) and the host
  filtering API (`filteredArrayUsingPredicate:` /
  `indexOfObjectMatchingPredicate:` / `filterUsingPredicate:`; a `nil`
  predicate is the identity). Covered by `tests/predicate_filter_test.jeti` and
  `tests/multi_tu/13_kvc_predicate`. Block-based filtering
  (`indexesOfObjectsPassingTest:`) stays a valid alternative for one-off tests.
- [ ] **Variadic convenience constructors** — only `arrayWithObjects:count:`-style signatures exist; the variadic `initWithObjects:..., nil` form is not declared. Related known quirk: `[NPMutableArray arrayWithObjects:...]` desugars to the immutable array path.
- [x] **`NPAssert`** assertion macro (fixed-arity: `NPAssert(cond, "desc")`,
  throws NPError; variadic format-string form waits on preprocessor
  `__VA_ARGS__` support).
- [x] **Object subscripting on dictionaries** — `d[@"k"]` reads via
  `objectForKey:` and `md[@"k"] = v` writes via `setObject:forKey:`; the
  checker's subscript rewrite picks the selector from what the receiver class
  actually declares (indexed arrays keep `objectAtIndex:` / `setObject:atIndex:`).
- [x] **`conformsToProtocol:` runtime** — class metadata now carries a populated protocol table and the runtime query walks class + protocol parent chains (`docs/plans/categories_protocol_plan.md` §8).
- [ ] **Shared (dynamic) Foundation library** — *accepted as a future direction, gated on a metadata-interposition design.* The current default is **static linking by design** (the weak-merge model depends on it; see `docs/plans/architecture.md` §7.2). A prototype exists (`tools/build-foundation-lib.sh` with `FOUNDATION_SHARED=1`) but it breaks KVC-driven code: declaration-only clients emit non-owned classes' `NPClass` as zeroed tentative definitions with a NULL `.kvc_entries`, and without cross-module weak-merge those stubs win. Prerequisite: a codegen mode (`-flink-foundation=shared`) that emits pure `extern` declarations for manifest-listed classes instead of tentative definitions. Not scheduled — the trigger is a real "multiple Jeti apps sharing one Foundation install" need.

## EH known limitations (accepted, not scheduled)

- **`-eh legacy` (sjlj)**: a cross-function `@throw` skips scope-end releases in intermediate frames (leak). The default `checked` backend does not have this issue.
- **Bare generics are a warning, not an error**: bare container spellings (`NPArray` without type args) keep erased-compat behavior; use explicit type args for element-type checking.

## Checker known limitations (accepted, not scheduled)

- **Undeclared selector is a compile-time error on the static paths** (no longer a warning). A statically typed receiver whose class chain does not declare the selector is rejected — that send would dispatch through a NULL vtable slot and segfault; ObjC rejects the same shape. Three shape-based escapes keep established idiom alive: selectors that never dispatch through the receiver slot (`respondsToSelector:`, `conformsToProtocol:`, root methods), `[obj ivar]` (C field access via the bracket form, never a send), and senders typed as an ancestor where some subclass in the TU declares the selector (`NPObject *o; [o speak]` with `Dog : NPObject` — the guarded `respondsToSelector:` idiom and `NPObject<P> *` receivers). `id` / `instancetype` receivers have no class chain, so they only reject a selector declared *nowhere* in the unit. Private cross-TU methods are caught by the same rule: a method absent from the shared `.hje` is invisible in the client TU.

## Tooling

- [ ] **Windows native validation** — `jetic.exe` builds and cross-compiles, but runtime behavior on real Windows is untested (zig cc backend, `__thread` semantics).
