# AGENTS.md — Jeti Language Project

> **Read first:** `docs/plans/architecture.md` is the single source of truth for the current
> architecture (pipeline, `.hje`/`.jeti` semantics, Foundation modes, owner/strong/weak
> metadata, vtable rules, EH backends). This file covers layout, commands, and the
> rules code changes must follow.
> **Source locations / debugging / LSP plan:** `docs/plans/source_locations_debug_lsp_plan.md`
> describes SourceSpan, generated-C `#line`, DWARF-backed debugging, versioned source
> maps, and LSP/debugger reuse. It is a plan, not implemented behavior; check
> `docs/plans/architecture.md` and `ROADMAP.md` for current status.
>
> **History:** detailed implementation logs and decision records live in
> `docs/plans/stable_slots_plan.md`, `docs/plans/arc_intern_uaf.md`, and
> `docs/archive/agents-history-2026-10.md` (the pre-2026-10 version of this file,
> including the legacy C-era project map). Do not re-derive them here.

## Current Phase

Maintenance + incremental features on the Rust implementation. The legacy C
version (`transpiler/`) is **gone** — the compiler is entirely Rust under `crates/`.

## Project Layout

```text
jeti-lang/
├── crates/                  # Rust workspace — the compiler
│   ├── lexer/ parser/ cst/ cst_visit/ elaborator/ binder/   # front end
│   ├── checker/             # type checker (default on)
│   ├── codegen/             # AST → C99
│   ├── arc/ eh/ async/ defer/ pattern/ trace/               # lowering passes (pipeline order)
│   ├── ownership/ preprocessor/ cpp/ symbol/ ast/ attrs/ cfg/ layout/
│   ├── lsp/                 # jeti-lsp: LSP server (stdio JSON-RPC, diagnostics)
│   └── jetic/               # CLI driver + pipeline (main.rs, pipeline.rs)
├── .vscode/extensions/jeti-lang-support/   # VS Code extension: grammar + LSP client
│                            #   (extension.js glue, lsp-client.js transport, test/ node harness)
├── include/
│   ├── jeti/                # runtime.h / runtime.c / runtime_freestanding.c
│   └── Foundation/          # 9 classes, each as a .hje (decl) + .jeti (impl) pair,
│                            #   plus Foundation.hje (decl umbrella) and
│                            #   Foundation.jeti (self-contained umbrella)
├── tests/
│   ├── multi_tu/            # 12 cross-TU scenarios (vtable/metadata rules)
│   ├── eh_diff/ eh_matrix/ arc_order/ strong_metadata/ arc_intern/
│   ├── golden/              # .jeti + expected .out pairs
│   ├── stress/              # baremetal / hosted / interop (own runners)
│   └── *.jeti                 # individual integration programs
├── compile_flags.txt        # editor flags (jeti-lsp, clangd-style); shipped fully
│                            #   commented — see its header before touching it
├── tools/build-foundation-lib.sh   # → target/foundation/libjetifoundation.a
├── test_all.py              # full integration suite (test_all.sh = parallel wrapper)
├── docs/                    # plans/ (architecture.md — start here), archive/ (history)
└── ROADMAP.md               # what is actually not done yet
```

## File Extension Convention

- **`.hje`** — Jeti header: declarations (`@interface`, `@protocol`, typedefs, structs).
  Inlined by the preprocessor via `#import`, never compiled directly.
- **`.jeti`** — Jeti implementation: `@implementation`, functions, `int main`.
  Two uses: compiled as its own translation unit (multi-TU mode), or inlined via
  `#import` (self-contained mode).
- `#import` of `.hje`/`.jeti` is a jeti import; `#include` of `.h`/`.c` passes through to
  the C compiler verbatim. `.hje` (not `.h`) avoids colliding with C/ObjC system headers.
- jetic always defines `__JETI__`; headers shared with plain C compilers guard
  jeti-only syntax with `#ifdef __JETI__` / `#else`.
- A `.hje` consumed in multi-TU mode must keep **full struct layouts** (ivar section) —
  Jeti is a C superset, so `@public` ivars, struct fields, and member function pointers
  require clients to see the layout. Do not treat opaque structs as the default rule.

## Build & Test Commands

```bash
cargo build                      # debug jetic → target/debug/jetic (test_all uses this)
cargo build --release
cargo test --workspace           # Rust unit tests

./test_all.sh -j4                # full integration suite (wraps test_all.py; JETIC= overrides binary)
./tests/multi_tu/run_multi_tu.sh
./tests/eh_matrix/run_eh_matrix.sh
./tests/eh_diff/run_eh_diff.sh           # JETIC= must be an absolute path
./tests/strong_metadata/run_strong_metadata_test.sh
./tests/arc_intern/run_arc_intern_test.sh

cargo build -p jeti-lsp          # → target/debug/jeti-lsp (LSP server over stdio)
cargo test -p jeti-lsp           # server unit tests + a real subprocess stdio session

node .vscode/extensions/jeti-lang-support/test/client.test.js     # extension transport ↔ jeti-lsp
node .vscode/extensions/jeti-lang-support/test/extension.test.js  # VS Code glue, stubbed `vscode`
./tools/run-vscode-extension.sh  # Extension Development Host with the extension loaded

./tools/build-foundation-lib.sh  # build target/foundation/libjetifoundation.a once
```

## Naming Conventions (used across all docs)

- Project: **Jeti**; compiler binary: **jetic**; runtime lib: **libjeti.a**;
  Foundation library: **libjetifoundation.a**.
- Classes keep the NP prefix: `NPObject`, `NPString`, `NPArray`, …
  The prefix comes from **"Nupa"**, the project's original working name before it
  became Jeti. It is deliberately decoupled from the language name — language
  renames must NOT touch `NP*` class names (breaking `.jeti` files, bridge headers,
  `JETI_CLASS_$_NP*` symbols for zero gain). Same for lib names: `libjeti.a` /
  `libjetifoundation.a` (not libjetia*, a legacy spelling that was removed).
- Mode names: **self-contained / unity mode** (`#import <Foundation/Foundation.jeti>`) and
  **precompiled Foundation / multi-TU mode** (`#import <Foundation/Foundation.hje>` +
  auto-linked `libjetifoundation.a`) — the recommended mode for real projects.
- TU roles: **owner TU** (its main file holds the class's `@implementation`) and
  **declaration-only client** (imports `.hje` only). Historical synonyms
  (`decl-only`, `pure header`, `P3 header`) are deprecated.

## Multi-TU / Metadata / Vtable Constraints (iron laws)

1. **Owner rule (R2):** the TU whose **main file** holds `@implementation X` emits
   X's metadata (`JETI_VTABLE_$_X`, `JETI_CLASS_$_X`, getClass, …) as **strong**
   symbols; declaration-only clients emit **weak stubs**. Two main-file
   implementations of the same class fail at link time with a duplicate symbol —
   by design. `-fstrong-metadata` **does not exist**; ownership is derived.
2. **vtable layout (R1):** `[public segment]` = methods declared in the expanded
   `#import` set (shared `.hje`), sorted; `[private segment]` = methods only in this
   TU's main file, appended at the end. Public-segment offsets are identical in all
   TUs. **Private methods only dispatch inside their owner TU** — cross-TU methods
   must be declared in the shared `.hje`.
3. **`__sig` (R3):** the startup layout-signature check covers the **public segment
   only**; private tail differences are legal. A mismatch aborts loudly (never
   silently misdispatches).
4. **`jeti_metaInit`** is an idempotent compat backfill for self-contained umbrella
   builds only; owner/multi-TU paths rely on statically initialized `NPClass`
   metadata and do **not** call it.
5. **Static dispatch:** there is no `objc_msgSend`. All message sends compile to
   static vtable indexing; selector symbols are stored with **every colon stripped**
   (`setObject:atIndex:` → `setObjectatIndex`) — any code that looks up a selector
   by name must strip all colons, not just a trailing one.
6. **Variadic methods:** the vtable fn-ptr cast must include `, ...` (signature-exact).
7. **File-level RawLine vs structs:** emitted in source order (Section 5 walks source
   order); otherwise `#pragma mark` / `_Pragma` placement silently breaks.
8. **EH default is `checked`** (flag + call-site guards, zero setjmp); `-eh legacy`
   (alias `-eh sjlj`) is the old setjmp/longjmp backend, kept as an explicit fallback.
9. **Generics:** compile-time monomorphization is the primary model; bare container
   spellings keep erased-compat behavior (warning, not error).

## Rules for Code Changes

- **C superset iron law:** Jeti must remain a C superset. Never break legal C or
  existing jeti syntax while adding checks; C-level warnings only add diagnostics,
  never change parsing paths.
- **Verify before claiming done:** judge "feature works / not implemented" with a
  minimal probe first (past "not implemented" verdicts were disproven by probes);
  before reporting success run the regression baselines (`cargo test --workspace`,
  `./test_all.sh`, `multi_tu`, plus the affected specialized suite). Never mark an
  unverified fix complete.
- **Debug via generated C:** first step for miscompiles is
  `jetic -rewrite-jeti file.jeti -o file.c` and reading the C. `-asm` must come after
  `run` on the command line.
- **Parser discipline:** a failed `consume()` must not advance (it would eat the next
  declaration's first token); avoid positional line-number surgery when editing
  `parser.rs` (stale line numbers cut files — match content or diff against git).
  A `[` after an expression is a **subscript** unless two adjacent words sit inside
  it — that shape is a message send (`[recv sel]`), which is what lets a missing
  `;` on the previous line report *that* mistake instead of a subscript cascade
  (`bracket_opens_a_message_send`).
- **Do not resurrect deleted things:** `Foundation.decl.hje`, `make-decl-headers.sh`,
  and `-fstrong-metadata` are gone on purpose. `Foundation.hje` = declarations only,
  `Foundation.jeti` = self-contained umbrella.
- **auto-link gating:** jetic auto-links `libjetifoundation.a` only for
  declaration-only clients (source-level transitive `#import` scan decides); a TU
  that inlines Foundation implementations is skipped on purpose — do not "simplify"
  this to a generated-C text heuristic (two such heuristics were already falsified).
- **Source location preservation:** when adding or changing AST/HIR nodes or lowering
  passes, preserve their source origin. New synthesized nodes should be marked or
  documented as synthetic and retain the originating Jeti span when one exists.
  Do not add a parallel line-number/source-map mechanism: follow
  `docs/plans/source_locations_debug_lsp_plan.md` and keep diagnostics, generated-C mappings,
  LSP, and debugger support on the shared SourceSpan/SourceMap model.
- **One position projection, at the boundary:** internal positions are 1-based
  lines with **byte** columns; LSP needs 0-based UTF-16 code units. The crossing
  happens exactly once, in `SourceSpan::to_lsp_range` (fed by
  `Diagnostic::to_source_span` / `SourceMap::span_from_node`). Never re-derive a
  column in a consumer, never mix the two unit systems, and never invent a range
  for a node without a source origin (`end == 0` → zero-width, at the start).
- **LSP requests come from the compiler front end:** `Pipeline::analyze_document` is
  the single entry — `crates/lsp` must not grow its own parser/checker chain, and a
  request must never re-parse the buffer. It runs the same stage order as
  `transpile`, sharing `Pipeline::prepare`, and every request (diagnostics, outline,
  definition) answers from that one run's AST and `SourceMap` (walking the AST it
  was handed is the consumer's job, as it is for codegen). New front-end
  stages/fatality rules must be mirrored there, not duplicated — the one deliberate
  difference is a **parse error**: the editor hands the recovered CST to the later
  stages (fix one error, see them all, and the AST still answers outline/definition),
  while `transpile` refuses to emit code at all. It runs the same analysis first, so
  the terminal prints the same list the editor publishes (ordered by source position,
  not by stage).
- **Destructive git ops need user approval.**
