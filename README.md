[-> 中文](CHINESE.md)

<div align="center">
<img src="docs/assets/logo/jeti_border_gray_on_amber.svg" alt="Jeti_avatar" width="210">

# The Jeti Programming Language

[**View Project Examples**](#project-examples)

[Overview](#overview) · [Why Jeti?](#why-jeti) · [Project Examples](#project-examples) · [Quick Start](#quick-start) · [Language Features](#language-features) · [New Features](#new-features) · [Compilation & CLI](#compilation--cli) · [Code Examples](#code-examples) · [Design Principles](#design-principles) · [Roadmap](#roadmap) · [FAQ](#faq)

</div>

---

> ⚠️Warning
> **The GitHub Releases page lags behind the source code.** The releases shown
> on the GitHub website are usually *older* than what's pushed to the `main`
> branch — I often forget to cut a new release after pushing new commits. If you
> want the latest features and fixes, **clone the repo and build from source**
> (see [Build](#build)); only use the downloaded releases if you prefer the
> stable, older snapshot.

---

## **Overview**

Jeti is a **purely static** Objective-C dialect (C superset language). Jeti source is transpiled to C99, then compiled to native machine code by Clang. No runtime message forwarding, no GC pauses, no JIT warm-up — all method dispatch, memory management, and polymorphism are resolved at compile time. It currently works — there are games and tools running in it. If you find it interesting, feel free to give it a try.

I don't intend to replace ObjC or Swift. I just miss ObjC's syntax and wanted to let it live again in a statically compiled world. ☺️

---

## Why Jeti?

I like ObjC's message send syntax `[obj message]`, but ObjC's runtime (`objc_msgSend`) is heavy. Jeti keeps the syntax and compiles it statically to plain C.

Jeti is a young language and not yet production-ready. The question it explores: "what happens if you transpile ObjC into plain static C?"

### What it does

- Method calls → compile-time VTable offsets, no objc_msgSend
- Memory management → CFG static analysis, retain/release decided at compile time
- Output → human-readable C99, not compiler IR

### Design Goals

- **C-compatible**: a C superset — legal C is legal Jeti
- **Readable**: generated C is meant to be read by humans
- **Lightweight**: just one small static runtime

### Why the `NP-` Prefix Stays

The language has changed its name, but the Foundation classes keep their `NP-` prefix (`NPObject`, `NPString`, `NPArray`, …). It comes from **"Nupa"** — the project's original working name before it became Jeti.

Keeping it is deliberate:

- **History you can read** — every `NP` class carries the name of the project's origin, even as the language name changed around it.
- **Same role as `NS`/`CF`** — ObjC uses a two-letter class prefix to mark the framework (`NSObject`, `NSString`); `NP` plays exactly that role for Jeti's Foundation. The full naming rules live in `NAMING.md`.
- **Stability over churn** — renaming classes would break every existing `.jeti` file, bridge headers (`jeti_NPString_UTF8String`), and metadata symbols (`JETI_CLASS_$_NPString`) for zero semantic gain. The prefix is not tied to the language name, so language renames don't touch it.

So: language name = Jeti, compiler = `jetic`, but classes stay `NP*` — Nupa's fingerprint in the standard library.

---

## Project Examples

[![examples](https://img.shields.io/badge/examples-000?style=for-the-badge)](examples/)

`examples/`

| Project               | Description                                                                              | Run                            |
| --------------------- | ---------------------------------------------------------------------------------------- | ------------------------------ |
| **`04_soma-kernel/`** | Tiny 32‑bit i386 OS kernel (NASM + C + Jeti), bare‑metal `-ffreestanding` mode                | `./run.sh` or `./run.sh --gui` |
| **`03_LibUI/`**       | GUI app via [libui-ng](https://github.com/libui-ng/libui-ng), all callbacks in pure Jeti | `./run_libui.sh`               |
| **`02_ncurses/`**     | Terminal demos (`ncurses_demo`, `sysmon`) using `Terminal::Ncurses`                      | `make run`                     |
| **`01_JSONEditor/`**  | Multi‑file JSON editor with split‑screen terminal preview                                | `jetic run json_editor.jeti`     |

---

## Quick Start

### Dependencies

- Clang (>= 14)
- Rust (stable, with cargo)
- Git

### Build

```bash
git clone https://github.com/3shine123/jeti-lang.git
cd jeti-lang
cargo build --release
```

### Install

The build automatically drops an `install.sh` (plus headers and `libjeti.a`) next to the `jetic` binary. Install it to your system with:

```bash
# After building from source — the script lives next to the binary
cd target/release        # or target/debug if you ran a plain `cargo build`
./install.sh             # installs to /opt/jeti by default
./install.sh /usr/local  # optional: pick a different prefix
```

This installs:

- **binary** → `<prefix>/bin/jetic`
- **static lib** → `<prefix>/lib/libjeti.a`
- **headers** → `<prefix>/include/`
- **system headers** → `/usr/local/include/{Foundation,jeti}/` (needs write permission; skip with `sudo` or pass a second arg like `./install.sh /opt/jeti ~/include`)

The installer auto-detects your language (中文 / English).

Alternatively, download a prebuilt release archive (`jeti-<platform>.tar.gz` or `.zip`) from the releases page, extract it, and run the `install.sh` inside:

```bash
tar xzf jeti-x86_64-unknown-linux-musl.tar.gz
cd jeti-x86_64-unknown-linux-musl
./install.sh
```

> **Tip:** with `jetic` on your PATH and system headers installed, `<jeti/runtime.h>` and `<Foundation/...>` resolve automatically — no `-I include` needed.

### Compile a Jeti Program

```bash
# Just output C code (auto-derives .jeti → .c)
jetic -rewrite-jeti hello.jeti
jetic hello.jeti -rewrite-jeti               # flag works anywhere
jetic -rewrite-jeti hello.jeti -o out.c      # explicit path also works
# (--rewrite-jeti double-dash form also accepted)

# Compile the transpiled C alone with Clang — two ways:
#   1) compile the runtime source directly
clang -I include -o hello hello.c include/jeti/runtime.c
#   2) link the prebuilt libjeti.a (lives next to the jetic binary)
clang -I include -o hello hello.c -Ltarget/release -ljeti

# Compile to executable
jetic hello.jeti -o hello_bin                # transpile + compile + link

# Multi-TU: extra positional inputs are compiled and linked in; .o/.a as-is
jetic main.jeti lib.jeti -I include -o app     # two TUs, one command (no manual clang)
jetic main.jeti lib.o libfoo.a -o app        # mix jeti sources with prebuilt objects

# Precompiled Foundation library: build once, link in every project
./tools/build-foundation-lib.sh                            # → target/foundation/libjetifoundation.a
jetic app.jeti -I include -L target/foundation -ljetifoundation -o app   # explicit
jetic app.jeti -o app                                        # or: auto-linked when findable (decl-only clients)

# Compile + run
jetic run hello.jeti
jetic run hello.jeti -o hello_bin            # keep binary after run
jetic run hello.jeti                          # auto-clean temp binary

# Show compilation warnings
jetic -v run hello.jeti

# [!] Error: .c output without -rewrite-jeti
jetic hello.jeti -o hello.c   → Error: use -rewrite-jeti to output C code

# [!] Error: no output method specified
jetic hello.jeti              → Error: specify -o or -rewrite-jeti
```

### Foundation: Two Usage Modes

Foundation supports two modes. Both are fully supported; **for real projects we recommend the precompiled-library mode**.

**Self-contained / unity mode** — implementations are inlined via `#import`; no library needed. Good for single files, quick experiments, and legacy builds:

```jeti
// hello.jeti
#import <Foundation/Foundation.jeti>   // declarations + implementations, all inlined

int main() {
    NPLog(@"hello %@", [NPString stringWithUTF8String:"world"]);
    return 0;
}
```

```bash
jetic run hello.jeti
```

**Precompiled Foundation / multi-TU mode (recommended)** — the implementation lives in a static library built once; your TU only compiles your own code:

```jeti
// app.jeti
#import <Foundation/Foundation.hje>   // declarations only — nothing inlined

int main() {
    NPLog(@"hello %@", [NPString stringWithUTF8String:"world"]);
    return 0;
}
```

```bash
./tools/build-foundation-lib.sh   # once → target/foundation/libjetifoundation.a
jetic app.jeti -o app               # the library is found and linked automatically
```

Under the hood: in self-contained mode the inlined implementations are not their TU's main file, so their class metadata is weak (duplicated per TU and merged). In library mode each Foundation `.jeti` is compiled as its own TU, so its `@implementation` owns the metadata and emits it strong — one copy in the archive. Full owner/strong/weak rules: `docs/plans/architecture.md`.

### Shell Completion (Tab autocomplete)

`jetic` ships with generated completion scripts for **zsh**, **bash** and **fish**, built with
[clap_complete](https://crates.io/crates/clap_complete). Regenerate them any time with:

```bash
jetic -gen-completions zsh > _jetic
jetic -gen-completions bash > jetic.bash
jetic -gen-completions fish > jetic.fish
```

The scripts are also copied into the install bundle (`share/jetic/completions/`) by `install.sh`.

**zsh** — add the directory to `fpath` before `compinit` runs:

```zsh
fpath=(/opt/jeti/share/jetic/completions $fpath)
autoload -U compinit && compinit
```

**bash**:

```bash
source /opt/jeti/share/jetic/completions/jetic.bash
```

**fish**:

```fish
source /opt/jeti/share/jetic/completions/jetic.fish
```

After installing a new version, clear the zsh cache with `rm -f ~/.zcompdump*` and open a new terminal.

### Run Tests

```bash
# Run all tests
./test_all.sh -j4

# Run Rust unit tests
cargo test --workspace
```

---

### Editor Support (LSP)

`jeti-lsp` is a stdio language server that reports **syntax, type and async
diagnostics** for the buffer you are editing, plus an **outline**,
**go-to-definition**, **hover** and **completion**. It runs the compiler's own
front end (`Pipeline::analyze_document`), so the findings are exactly what the
CLI reports — never a second, drifting linter — and every position crosses the
protocol boundary through the shared `SourceSpan` model (1-based byte columns →
LSP UTF-16 code units) exactly once.

```bash
cargo build -p jeti-lsp           # → target/debug/jeti-lsp
jeti-lsp --include include        # speaks LSP over stdin/stdout
```

One analysis run per edit feeds everything: diagnostics are published, and the
symbol index behind the outline, definition, hover and completion requests
answers from the *same* AST — a request never re-parses your buffer. Definition
resolves the name under the cursor (a whole selector like `setObject:atIndex:`
counts as one name) and returns every declaration of it, including ones in an
imported header, so `NPString` in your code jumps into
`Foundation/NPString.hje`. Hover shows the declaration's own source text, and
completion offers every name the index knows — classes, methods (including whole
selectors), functions, typedefs, enums and globals — plus the **members of the
receiver** you are sending to: inside `[str ` you get `NPString`'s own methods
and its ancestors', `[Widget ` gives that class's class methods, and `obj.`
works too. Argument positions are not treated as selector positions, which the
AST — not the text — decides. Signature help shows the signature of the call the
cursor is in and highlights the parameter being filled in (`:` triggers it).

A syntax error does not hide the rest of the file. The parse still fails, but the
recovered syntax tree is handed to the later stages, so the editor reports the
syntax mistake **and** the type/ARC errors below it, and the outline, definition,
hover and completion keep answering from that tree. The terminal prints the same
list, ordered by source position (the way clang orders diagnostics): the CLI
refuses to *emit code* for a file that did not parse, it does not refuse to tell
you what else is wrong with it.

Scope today: the lifecycle handshake, incremental document sync
(`didOpen`/`didChange`/`didClose` → `publishDiagnostics`),
`textDocument/documentSymbol`, `textDocument/definition`, `textDocument/hover`,
`textDocument/completion` (global names plus receiver members; protocol members
for `id<P>` and generic argument substitution are not in yet) and
`textDocument/signatureHelp`, with **incremental** document sync (the client
sends changed ranges, not the whole buffer). Rename and ARC-only diagnostics are
not in yet.

### VS Code

The bundled extension (`.vscode/extensions/jeti-lang-support`) adds syntax
highlighting **and** the language client: it spawns `jeti-lsp` and renders the
diagnostics as squiggles in the editor, fills the Outline view and the
breadcrumb bar, answers Go to Definition, shows hovers with the declaration's
source text, completes both global names and receiver members, and shows the
signature of the call you are in. Try it in a throwaway window (nothing is
installed into your own setup):

```bash
./tools/run-vscode-extension.sh                 # this repository
./tools/run-vscode-extension.sh ~/my/project    # any Jeti project
```

The extension looks for the server at `target/debug/jeti-lsp` /
`target/release/jeti-lsp` in the workspace first (so the Jeti repo itself needs
no configuration), then on `PATH`. Settings:

| Setting | Default | Meaning |
| --- | --- | --- |
| `jeti.serverPath` | `jeti-lsp` | Server executable; a path is used as given, a bare name falls back to `PATH`. |
| `jeti.searchPaths` | `["include", "."]` | `--include` dirs for `#import` resolution, relative to the workspace folder. |
| `jeti.arc` | `true` | Analyze in ARC mode (the compiler's default). Turn it **off** for projects built with `-fno-jeti-arc`. |
| `jeti.arguments` | `[]` | Extra server flags in `jetic`'s spelling: `-fno-jeti-arc`, `-fno-checker`, `-eh legacy`. |

#### Project configuration

The settings above are the *defaults*. A project can also state its own flags the
way clangd reads them, and the server finds them by itself:

| File | Scope | Shape |
| --- | --- | --- |
| `compile_commands.json` | per file | an array of `{directory, file, arguments \| command}` — the entry for the document wins, otherwise the closest entry's flags are used (clangd's inference). A relative `-I` resolves against the entry's `directory`. |
| `compile_flags.txt` | the whole tree | one flag per line, applied to every document. |

Both are searched from the document's directory upwards, stopping at the
workspace root; a directory holding both uses `compile_commands.json`.
Recognized flags: `-I DIR` / `-IDIR` / `--include DIR`, `-fno-jeti-arc`,
`-fno-checker`, `-eh checked|legacy|sjlj`. Everything else (`-o`, `-g`, …) is
ignored, because the editor neither links nor runs code. In `compile_flags.txt`
a line starting with `#` is a comment, so a flag can sit there commented out
until it is wanted — a jetic extension: clangd reads the same file and passes
every line to the compiler, so a directory shared with clangd should keep
comments out of it. Project flags *refine*
the settings: directories add, a switch that turns something off stays off, and
an explicit `-eh` in a file replaces the editor's choice.

```json
[
  { "directory": ".", "file": "src/legacy.jeti",
    "arguments": ["jetic", "-I", "include", "-fno-jeti-arc", "src/legacy.jeti"] }
]
```

#### Why the ARC/MRC settings exist

The last two settings exist because the editor has to agree with your build.
By default the server analyzes exactly as `jetic` does — ARC on, checker on — so
legacy manual-retain/release code shows `explicit 'release' not allowed in ARC
mode` **because that is what the compiler reports** (the test suite only builds
those files because it retries with MRC). Tell the editor how you build and the
warnings go away for code that builds.

The client has no npm dependencies (`vscode` + node built-ins only), so the
workspace needs no `npm install`. Diagnostics come from the compiler's front
end, so nothing is reported here that `jetic` would not report on the command
line.

---

## Language Features

### Class System

```jeti
@interface Animal : NPObject {
@public
    NPString *_name;
}
- (instancetype)initWithName:(NPString *)name;
- (void)speak;
@property (readonly) NPString *name;
@end

@implementation Animal
- (instancetype)initWithName:(NPString *)name {
    self = [super init];
    if (self) {
        _name = name;
    }
    return self;
}
- (void)speak {
    printf("...\n");
}
@end
```

### Protocol

```jeti
@protocol Drawable
- (void)draw;
- (BOOL)isVisible;
@end

@interface Shape : NPObject <Drawable>
@end
```

### Properties

```jeti
@interface Person : NPObject
@property NPString *name;
@property int age;
@property (readonly) NPString *identifier;
@end
```

### Category

```jeti
@interface Person (Printing)
- (void)printGreeting;
@end

@implementation Person (Printing)
- (void)printGreeting {
    printf("Hello, my name is %s\n", [self name]);
}
@end
```

### Block

```jeti
int (^square)(int) = ^int(int x) {
    return x * x;
};

void (^logAndCall)(NPString *, void (^)(void)) = ^void(NPString *msg, void (^next)(void)) {
    printf("[LOG] %s\n", msg);
    if (next) next();
};
```

### @autoreleasepool

```jeti
@autoreleasepool {
    NPString *temp = [NPString stringWithUTF8String:"hello"];
    // temp is released when the pool pops
}
```

### @selector

```jeti
SEL sel = @selector(doSomething:);
```

### Full C Compatibility

```jeti
#include <stdio.h>
#include <stdlib.h>

@interface Wrapper : NPObject
- (void)callCFunction;
@end
```

### C Attributes (__attribute__)

Jeti supports `__attribute__((...))` pass-through. You can write C `__attribute__` on global declarations and struct fields, and the compiler preserves them verbatim in the generated C output.

```jeti
__attribute__((packed))
struct Point {
    int x;
    int y;
};

__attribute__((format(printf, 1, 2)))
int my_log(const char *fmt, ...);
```

The compiler ships with a **590-attribute classification table** (scraped from Clang and GCC official docs). The `-backend` option controls which attributes are allowed:

| Option                        | Behavior                                                                            |
| ----------------------------- | ----------------------------------------------------------------------------------- |
| `-backend=clang` (default)    | Allow clang-specific attributes (e.g. `availability`, `diagnose_if`, `objc_direct`); gcc-only attributes error |
| `-backend=portable`           | Only attributes supported by both gcc and clang; others error                        |
| `-backend=gcc`                | Allow gcc-specific attributes (e.g. `strub`, `optimize`, `stack_protect`)            |

Unknown attributes (not in the table) produce a warning and pass through — never a hard error.

### Memory Management

Jeti uses **compile-time static ARC**. The compiler determines each object reference's lifetime through CFG dataflow analysis and inserts retain/release calls automatically. No manual `retain`/`release`/`autorelease` needed.

In MRC mode (`-fno-jeti-arc`):

```jeti
NPObject *obj = [[NPObject alloc] init];
// ... use obj ...
[obj release]; // MRC manual release
```

### C Bridge (`-emit-bridge-header`)

Jeti transpiles to C, but calling Jeti object methods from C normally requires verbose vtable-index and SEL-constant boilerplate. `-emit-bridge-header` generates a header with `static inline` wrappers for every method, so C code can call Jeti objects like ordinary C functions.

**Usage**: transpile a Jeti library to C, then generate the bridge header:

```bash
jetic -rewrite-jeti lib.jeti -o lib.c -emit-bridge-header lib.h
```

Then include the bridge header from C:

```c
#include "lib.h"

int main(void) {
    jeti_metaInit();  // metadata back-fill — see the note below

    // Class method: jeti_<Class>_<method>(params...)
    NPString *s = jeti_NPString_stringWithUTF8String_("Hello");

    // Instance method: jeti_<Class>_<method>(self, params...)
    size_t len = jeti_NPString_length(s);
    const char *cstr = jeti_NPString_UTF8String(s);

    // Nested message send (like Jeti's [[s UTF8String] ...])
    const char *nested = jeti_NPString_UTF8String(
        jeti_NPString_stringWithUTF8String_("nested")
    );

    // Multi-argument message send (like Jeti's [arr replaceObjectAtIndex:0 withObject:obj])
    NPArray *arr = jeti_NPArray_arrayWithObject_(s);
    jeti_NPArray_replaceObjectAtIndex_withObject_(arr, 0, s);

    // Each colon in the selector becomes an underscore in the function name:
    //   [obj foo:arg1 bar:arg2] → jeti_<Class>_foo_bar_(obj, arg1, arg2)
    //   [m replaceCharactersInRange:rng withString:str]
    //   → jeti_NPMutableString_replaceCharactersInRange_withString_(m, rng, str)
}
```

Link against the transpiled `.c` and `runtime.c`:

```bash
clang caller.c lib.c include/jeti/runtime.c -I include -o app
```

⚠️ The bridge header uses `sel_registerName` to resolve selectors at runtime, so it does **not** depend on the codegen-generated `static const` SEL constants (which are file-local and invisible across translation units). Class metadata itself is **statically initialized at load time** by every TU whose main file holds the `@implementation` — which is all `jetic` workflows. `jeti_metaInit()` stays in the examples as a harmless idempotent back-fill; it is only *required* when a build reaches implementations through `#import "*.jeti"` (single-TU umbrella builds), where no TU owns the metadata.

#### Memory Management from C

Jeti's **ARC is compile-time and applies only to `.jeti` source** — it never sees calls coming from C. When C code calls bridge functions, objects are **not** automatically retained or released. Manage them manually, following the ObjC memory-management naming convention:

| Method family                                  | Caller owns?       | What C code must do                                                             |
| ---------------------------------------------- | ------------------ | ------------------------------------------------------------------------------- |
| `alloc`, `new`, `copy`, `mutableCopy`          | ✅ +1               | Must call `jeti_release(obj)` when done                                         |
| `init`                                         | ❌ consumes `alloc` | Nothing                                                                         |
| everything else (e.g. `stringWithUTF8String:`) | ❌ autoreleased     | Nothing, but `jeti_retain(obj)` if it must outlive the current autorelease pool |

```c
#include "lib.h"

int main(void) {
    jeti_metaInit();
    jeti_autoreleasepool_t *pool = jeti_autoreleasepoolPush();

    // +1 (returns autoreleased convenience object); use within this pool only
    NPString *s = jeti_NPString_stringWithUTF8String_("hello");
    printf("%s\n", jeti_NPString_UTF8String(s));

    // If it must outlive the pool: retain now, release later
    NPString *t = jeti_NPString_stringWithUTF8String_("world");
    jeti_retain(t);
    jeti_autoreleasepoolPop(pool);      // t survives (was retained)
    printf("%s\n", jeti_NPString_UTF8String(t));
    jeti_release(t);

    // alloc-family returns +1 → must release
    NPString *u = jeti_NPString_alloc(jeti_NPString_stringWithUTF8String_("x") /* placeholder */);
    // (real usage: jeti_NPString_copy(s) returns +1, release it)
    NPString *copy = jeti_NPString_copy(s);
    jeti_release(copy);
}
```

`jeti_retain`, `jeti_release`, `jeti_autorelease`, `jeti_autoreleasepoolPush`/`jeti_autoreleasepoolPop` are declared in `<jeti/runtime.h>` and work on any Jeti object. This is exactly the manual-retain-count (MRC) model — from the C side you can think of Jeti objects as raw pointers you own or don't own by convention.

---

## New Features

Jeti adds features on top of Objective-C syntax that ObjC itself doesn't have.

**Recent highlights:**

- **Predicates / KVC (`NPPredicate`)** — a runtime format-string parser + evaluation engine living entirely in the Foundation library (`age > 18 AND name BEGINSWITH[c] 'A'`, `ANY tags LIKE '*dev*'`), backed by compile-time KVC accessor tables (`JETI_KVC_$_X`, strong in the owner TU) and a host filtering API (`filteredArrayUsingPredicate:` / `indexOfObjectMatchingPredicate:` / `filterUsingPredicate:`; a `nil` predicate is the identity). The compiler never parses the format string — see `docs/plans/architecture.md` §12.
- **Sets (`NPSet` / `NPMutableSet` / `NPOrderedSet`)** — hash-bucket set containers in the Foundation library (unique elements, `containsObject:` / `anyObject` / `setWithObjects:count:`), with `NPOrderedSet` preserving insertion order; all container methods dispatch through the static vtable, so they are safe across TUs.
- **Native bare-metal support (`-ffreestanding`)** — compiles to self-contained C with no libc, no Foundation, no TLS; `@try/@catch` uses `__builtin_setjmp/longjmp`, and a zero-boilerplate `runtime_freestanding.c` provides the bump allocator, `JETI_CLASS_$_jeti_root`, exception state, and `memcpy`.
- **C superset** — `@protocol` + conformance, `@property` + `@synthesize`, `instancetype`, `@public` ivars, dot syntax, structs + function pointers, inline asm, C-style casts.
- **Typed `@catch`** — catch arms match via `__jeti_eh_isa` (isKindOf: superclass-chain semantics, like ObjC): a parent-class arm catches subclass instances, and the first matching arm consumes the exception so later arms never double-catch.
- **ARC fixes** — scope-stack model no longer releases parent-scope variables at nested scope end; `for`-init object hoisting stops leaks and invalid `for` headers.
- **`@noarc` block** — block-level MRC: in ARC mode, manual `retain`/`release`/`dealloc`/`autorelease` inside `@noarc { }` is allowed; the block-level analogue of `-fno-jeti-arc` and clang's `-fno-objc-arc`.
- **`__attribute__` pass-through + `-backend`** — full support for C `__attribute__((...))` and all `__`-prefixed C predefined identifiers (`__FILE__`, `__LINE__`, `__builtin_*`, `__extension__`, `__typeof__`, `__alignof__`, ...); the `-backend` flag controls which compiler-specific attributes are allowed.

### for-in Enumeration

```jeti
for (NPString *s in arr) {
    printf("%s\n", [s UTF8String]);
}
```

Desugared at parse time into an index loop over `[coll count]` / `[coll objectAtIndex:]` — the collection expression is evaluated once, nil-safe (`[nil count]` is 0), and elements are borrowed (no retain/release). Plain C arrays and `NPArray` both work.

### Protocol Conformance Checking

The checker now verifies that a class declaring `<Proto>` implements every required method (recursively through parent protocols; `@optional` methods are exempt):

```
class 'Circle' does not implement required method 'draw' from protocol 'Drawable'
```

### Protocol Composition (`P & Q`)

Reuse C's `&` operator to require several protocols at once — no new syntax:

```jeti
// ① Intersection type: the receiver must implement both
void render(id<Drawable & Serializable> item);

// ② Composed protocol declaration
@protocol Renderable <Drawable & Serializable>
@end
```

Protocol types stay compile-time constraint labels only — vtable slots are unaffected, so multi-TU layout is unchanged.

### Foundation Type Dispatch (`isKindOfClass:` / `respondsToSelector:` / `isEqual:`)

The official ObjC spellings are now implemented on the root class, enabling idiomatic multi-way dispatch without any new language construct:

```jeti
for (id item in items) {
    if ([item isKindOfClass:[Dog class]]) {
        [(Dog *)item bark];
    } else if ([item respondsToSelector:@selector(draw)]) {
        [(id<Drawable>)item draw];
    }
}
```

`isKindOfClass:` walks the isa chain, `respondsToSelector:` queries the unified vtable, and `isEqual:` defaults to pointer identity on the root class (matching `NSObject`) — while `NPString` and `NPNumber` override it with **value** equality, which is what makes dictionary keys work. The legacy `isKindOf:` remains as a compatibility alias.

### Struct `==` / `!=` Value Comparison

C rejects `a == b` on structs outright; Jeti reuses the existing operators and desugars to a generated field-by-field compare function:

```jeti
struct Point a = {1, 2};
struct Point b = {1, 2};

if (a == b) { ... }   // field-wise compare
if (a != b) { ... }

p == &a               // pointer comparison semantics unchanged
```

### async/await

An async method declares the modifier with its **business return type** — `async int`, `async void`, even `async NPArray<NPString *> *` — and awaits with the bare soft keyword `await` (context-sensitive: a C identifier named `await` still parses as an identifier everywhere else):

```jeti
@interface Fetcher : NPObject
- (async int)compute:(int)n;   // suspends, yields an int
- (async void)runAll;          // async void = the entry method
@end

@implementation Fetcher
- (async int)compute:(int)n {
    int raw = await n;                // suspension point
    return raw * 2;                   // the body returns T, not a task
}

- (async void)runAll {
    int x = await [self compute:21]; // awaiting a call unwraps to T
    NPLog(@"result=%d", x);
}
@end

int main() {
    // v1 rule 3: blockOn: is the only sync→async bridge — a bare async call
    // is an error everywhere (rule 2). The receiver lives inside the block
    // (block thunks capture params + self until the stage-3 spawn work).
    [NPTask blockOn:async ^ void {
        Fetcher *f = [[Fetcher alloc] init];
        await [f runAll];             // rule 2: an async call MUST be awaited
    }];
    return 0;
}
```

Design rules:

- **`async` takes the business type directly** — `async int` / `async void` / `async NPString *`. The legacy `(async NPTask<T>)` spelling is rejected with a migration error; a bare `- (NPTask<int>)load` remains an *ordinary synchronous* method that merely returns a task object.
- **Signatures declare, the checker enforces** — `await` without the modifier → error. A body with no `await` is legal (Async Semantics v1 rule 1: `async` means *may* suspend — cache hits and sync protocol impls are normal; see `docs/plans/async_semantics_v1.md`). **A call to an async method has exactly one form: `await [f x]`** (rule 2) — a bare async call is a checker error in every position (statement, expression, cast-to-void discard, return, and from sync methods): no "bare call creates a task", no statement-position auto-drive, no sync blocking wrapper. The only sync→async bridge is `+[NPTask blockOn:<async block>]` (rule 3); `await` is the only blocking drive.
- **`await` lowers to a state machine** — the body is split at suspension points into a `switch(task->state)` driver over a heap `NPTask`; locals that survive a suspension are lifted into a per-method frame struct.
- **Inside a message send, `await` must be parenthesized** — an argument is `[self wrap:(await [self load:4])]`, a receiver is `[(await [self box:2]) value]`. A bare `[self wrap:await [self load:4]]` is a parse error (with a fix-it): unparenthesized, it reads like Swift's await-over-the-whole-chain, and Jeti deliberately defines no precedence for it. Selector position (`[task await]`) and C identifiers named `await` are unaffected; cast/grouping parens are fine — `(int)(await [self load:3])` drives the task first, then converts.
- **Errors are plain Objective-C-style EH** — a failable async method annotates `throws(ErrType *)` (documentation-level, the same spelling as the sync side); callers handle failures with ordinary `@try/@catch`; an unhandled failure aborts loudly. A mere suspension never fires `@finally` — a failed await does. (Checked EH backend, the default.)
- A cooperative single-thread scheduler (`jeti_sched_run`) and I/O integration are planned as the next milestone.

### Switch Pattern Matching (`case` patterns)

`case` labels accept **patterns**, not just integer constants. Type dispatch stays a method chain in spirit — the patterns desugar to `isKindOfClass:` / `isEqual:` / comparisons — but you write them declaratively:

```jeti
// Object patterns mix freely in one switch:
switch (subject) {
    case NPString *s:                      // type binding → isKindOfClass:
        NPLog(@"string: %s", [s UTF8String]);
        break;
    case NPNumber *n when [n intValue] > 3:  // type binding + `when` guard
        NPLog(@"number: %d", [n intValue]);
        break;
    case @"literal":                       // object literal → isEqual: (value semantics)
        NPLog(@"matched a literal");
        break;
    default:
        break;
}

// Comparison patterns, on a SCALAR subject:
switch (n) {
    case > 100:
        NPLog(@"big");
        break;
    case > 0 && < 100:                     // range
        NPLog(@"small");
        break;
    default:
        break;
}

// Plain multi-value constants stay plain C:
switch (n) {
    case 1, 2, 3:
        NPLog(@"one of 1-3");
        break;
    default:
        break;
}
```

| pattern | lowers to |
|---------|-----------|
| `T *name` | `jeti_isKindOfClass(subject, &JETI_CLASS_$_T)`; inside the arm, `name` is already bound to `(T *)subject` |
| `> 10`, `< 10`, `>= 0`, `<= 9` | `subject > 10` (the subject is spliced into the dangling operand) |
| `> 0 && < 100` | `subject > 0 && subject < 100` |
| `@"lit"`, `@42`, `@YES`, `@'c'`, `@(expr)` | `[subject isEqual:<literal>]` — value semantics, so `@"lit"` matches a *different* NPString with the same contents |
| `T *x when <expr>` | the type test, `&&`-ed with the guard |
| anything else | plain C constant, compared with `==` |

`when` is a **contextual keyword** — `int when = 1;` still compiles. Arms are tested top to bottom and the first match wins; `break` (or falling off the end) leaves the switch. **Fallthrough follows C**: an arm without `break` runs into the next arm's body.

`case` labels that could never be a C constant — `case [obj msg]:`, `case f():`, `case a = b:` — are rejected with `case label is not a constant expression` instead of leaking into the generated C, where the error used to point at generated code with no source correspondence.

M1 limits, all diagnosed rather than silently miscompiled:

- **Constant arms can't mix with pattern arms** — the C path needs the original body, the pattern path discards it. Split them into separate switches.
- **A dangling comparison can't mix with a type-binding / object-literal arm**, and **can't be used on a syntactically-object subject** (`switch (@7) { case > 100: ... }`). Both would make the subject an object, so `subject > 100` becomes a *pointer* compare — always true, silently. Switch on a scalar instead (`switch ([o intValue])`).
- Residual M1 gap: a *variable* subject holding an object (`id o = @7; switch (o) { case > 100: ... }`) still degenerates — deciding it needs type information the parser does not have.
- Nested patterns get no cross-level `case` scoping (an inner `case` belongs to the inner switch, same as C).

Implementation: the parser classifies each label and flattens the whole switch into a single arm list; a new lowering pass rewrites that into a `goto`/`if` chain with C labels before the checker runs — so the generated C stays plain C99 and there is no new IR. Golden: `tests/golden/42_switch_pat/`.

### Boxed Literals (`@(expr)` / `@YES` / `@NO` / `@'c'`)

```jeti
NPNumber *a = @123;          // int
NPNumber *b = @1.5;          // double
NPNumber *c = @YES;          // BOOL → 1
NPNumber *d = @'c';          // char
NPNumber *e = @(i + 1);      // factory chosen by the operand's STATIC type
```

Every form yields a real `NPNumber`: the literal's own type picks the factory (`numberWithInt:` / `numberWithDouble:` / `numberWithChar:`), and `@(expr)` picks by the operand's static type — `double`/`float` → `numberWithDouble:`, `BOOL` → `numberWithBool:`, `char` → `numberWithChar:`, `long`/`long long` → `numberWithLongLong:`, any other integer → `numberWithInt:`. Boxed results are ordinary objects, so they dispatch like anything else: `[@(i * 2) intValue]`.

`@(expr)` is rewritten in the **checker**, not the parser: the parser has no types, and the C99 backend has no `_Generic` to fall back on. The rewrite reuses ordinary message-send nodes, so static dispatch and the nil guard come for free — the same mechanism as object subscripts and struct `==`. Non-arithmetic operands are rejected rather than silently boxed:

```
illegal type 'NPString *' in a boxed expression — '@(...)' accepts arithmetic and BOOL values only
```

### Dictionary Literals (`@{ key: value }`)

```jeti
NPDictionary *d = @{ @"a": @1, @"b": @2, @"c": @3 };
NPLog(@"%d", [[d objectForKey:@"b"] intValue]);   // 2
printf("%lu\n", (unsigned long)[d count]);        // 3

NPMutableDictionary *m = [NPMutableDictionary dictionary];
[m setObject:@10 forKey:@"x"];
[m setObject:@11 forKey:@"x"];   // equal key → replaced, not appended
[m removeObjectForKey:@"x"];

NPDictionary *empty = @{};       // `@{}` is an empty dictionary (`@[]` is the array)
```

Keys compare with `isEqual:`, so `NPString`/`NPNumber` keys have **value** semantics. String literals are **interned** (same contents → the same object, like ObjC constant strings), and value equality remains the semantic guarantee — a lookup with a fresh `@"b"` finds the entry either way. Storage mirrors `NPArray` — two parallel object arrays with a linear scan — and `count` / `objectForKey:` / `allKeys` / `allValues` / `copy` / `description` / content-based `isEqual:` round out the API. Entries must be objects, as in ObjC:

```
illegal type 'int' in a dictionary literal — keys and values must be Objective-C objects
```

### Exception Semantics (`-eh checked` — the default backend)

Jeti's exceptions are **ObjC exceptions by value, without unwinding**. `@try`/`@catch`/`@finally`/`@throw` behave exactly like clang's `-fobjc-arc-exceptions` mode — and a differential test suite (`tests/eh_diff/run_eh_diff.sh`) locks this in by running each case under both jetic and real clang/ObjC, then diffing stderr line by line (7/7 cases pass).

```jeti
@interface Boom : NPObject
- (void)fire;
@end

@implementation Boom
- (void)fire {
    @throw @"negative input";
}
@end

int main() {
    @try {
        Boom *b = [[Boom alloc] init];
        [b fire];                       // execution stops HERE
        NPLog(@"never runs");
    }
    @catch (NPString *e) {
        NPLog(@"caught: %@", e);
    }
    @finally {
        NPLog(@"finally always runs");
    }
    return 0;
}
```

The semantics you get:

- **ARC settles every frame** — an exception that crosses frames releases each frame's owned locals on the way out. There is no `longjmp`, so nothing is skipped and nothing leaks.
- **A throw interrupts immediately** — the rest of the statement never evaluates: `x = [a risky] + [b sideEffect];` never runs `sideEffect`.
- **Typed catch chains match by isa** — an unmatched `@catch` lets the exception continue to the enclosing `@try`; a rethrow inside `@catch` propagates to the outer handler, never re-enters the same one.
- **`@finally` ordering** — inner finally runs before the outer catch; the outer finally runs after the outer catch.
- **Throws inside block literals** propagate to the enclosing `@try` like any other call.
- **Uncaught exceptions abort** with ObjC's wording: `*** Terminating app due to uncaught exception of class 'NPString'`, exit code 1.
- **C callers can't miss an exception** — bridge-header wrappers check the error flag and abort rather than silently returning a zero value.

**`-eh checked` is the default backend** — a plain `jetic run` compiles with it. `-eh legacy` (alias `-eh sjlj`) selects the old zero-overhead setjmp backend and remains a complete rollback; that backend has the classic limitation: a cross-frame throw skips intermediate frames' cleanup (documented below).

### `throws` — Declared Exceptions

`@throw` and `throws` differ by one letter and mean completely different things — the split Java draws between `throw` (a statement) and `throws` (a declaration clause).

| | `@throw` | `throws` |
|--|----------|-----------|
| What | **statement** — raises an exception at runtime | **declaration annotation** — compile-time metadata |
| Where | inside a body | trailing, before the `;` or `{` of a declaration |
| Shape | `@throw expr;` | `throws(T *)` or bare `throws` |
| In generated C | yes (the setjmp/flag machinery) | **never** — no code, no vtable slot |

```jeti
@interface Repo : NPObject
- (NPString *)fetch:(const char *)url throws(NPError *);   // throws NPError *
- (int)parse:(const char *)s throws;                       // throws; type unstated
- (int)count;                                              // never throws
@end

@implementation Repo
- (NPString *)fetch:(const char *)url throws(NPError *) {
    if (!url) {
        @throw [[NPError alloc] init];   // the statement
    }
    return @"ok";
}
@end
```

Apple has occupied exactly this slot — trailing metadata before the `;` — with macros for over a decade (`NS_DESIGNATED_INITIALIZER`, `NS_REQUIRES_NIL_TERMINATION`, `API_AVAILABLE(...)`). Jeti promotes the slot to first-class syntax and lets the checker reconcile it.

**What the checker enforces**

- `throws(T *)` — every `@throw` that escapes the declaration must have a static type compatible with `T` (subclasses allowed):
  `error: '@throw' of type 'AppError *' does not match the declared 'throws(NPString *)'`
- Bare `throws` — the body must really contain an escaping `@throw`:
  `error: 'liar' is marked 'throws' but its body never executes '@throw'`
- No annotation — an escaping `@throw` is an error:
  `error: '@throw' escapes 'bad' without a 'throws' annotation; declare it with 'throws(<type>)' or handle it with a local '@try'`

A `@throw` caught by a `@try` **in the same body** is never an escape, so `main` and locally-guarded helpers need no annotation:

```jeti
static void bad(int n) {                        // error: escapes 'bad'
    if (n < 0) {
        @throw [[AppError alloc] init];
    }
}

static void good(int n) throws(AppError *) {   // declared — fine
    if (n < 0) {
        @throw [[AppError alloc] init];
    }
}

static void guarded(int n) {                    // fine — caught locally
    @try {
        if (n < 0) {
            @throw [[AppError alloc] init];
        }
    }
    @catch (AppError *e) {
    }
}
```

The type check is deliberately conservative: `@"..."` literals, bare C strings, casts, and variables of known type are judged; a message send is not (its class is not knowable from a selector-only registry), so it satisfies any declared type. Annotations are compile-time only — adding or removing `throws` never changes generated C, program output, or ARC behaviour. Misusing the pair is itself an error: `throws` inside a body, or `@throw(...)` on a declaration, each gets a diagnostic naming the other keyword.

### Implicit Root Class (`jeti_root`)

Jeti now supports user-defined root classes. You no longer need to inherit from `NPObject` — an `@interface` without a superclass automatically gets a compiler-injected implicit root class `jeti_root`, while keeping `id` type uniformity and static dispatch.

**Before:**

```jeti
@interface Animal : NPObject   // had to inherit NPObject
```

**After:**

```jeti
@interface Animal              // no superclass → implicit root class
@interface Animal : NPObject   // explicit NPObject still works
```

Both are valid, and `id` can point to any Jeti object.

#### How It Works

When no superclass is specified, the compiler injects `jeti_root`:

```jeti
// User code:
@interface Animal {
    int age;
}
- (void)speak;
@end

// Compiler treats as:
@interface Animal : jeti_root {
    int age;
}
- (void)speak;
@end
```

Generated C code:

```c
// Built-in structures
struct jeti_object_header {
    struct jeti_vtable *vtable;
};

struct jeti_root {
    struct jeti_object_header header;
};

// Animal's struct
struct Animal {
    struct jeti_root __super;  // contains header
    int age;
};
```

#### `id` Type

```c
typedef struct jeti_root *jeti_id_t;
```

`id` is no longer tied to `NPObject` — it only requires the object to start with `jeti_root`. This means:

```jeti
Animal *a = [[Animal alloc] init];
id obj = a;                    // valid: Animal inherits from jeti_root
[obj speak];                   // static dispatch: obj->header.vtable[...]
```

#### Explicit Inheritance Still Works

```jeti
@interface Dog : Animal {
    NPString *breed;
}
@end
```

Generated C:

```c
struct Dog {
    struct Animal __super;     // contains jeti_root → header
    struct NPString *breed;
};
```

#### `NPObject` vs `jeti_root`

| Declaration                 | Means                             | Use Case                        |
| --------------------------- | --------------------------------- | ------------------------------- |
| `@interface Xxx`            | Implicit `jeti_root`, lightweight | Custom layout, kernel, embedded |
| `@interface Xxx : NPObject` | Explicit NPObject, full runtime   | User apps, ARC, retain/release  |

```jeti
// Lightweight root class, no refcounting overhead
@interface KernelTask {
    int pid;
    int priority;
}
- (void)run;
@end

// Full NPObject with automatic memory management
@interface UserModel : NPObject
@property NPString *name;
@end
```

#### Bare-Metal / Freestanding Support (`-ffreestanding`)

Jeti can compile to **self-contained C with no libc, no Foundation, no TLS**, for kernels, MCUs, and bare-metal embedded development.

```bash
jetic -rewrite-jeti -ffreestanding kernel.jeti   # emits self-contained C
```

In `-ffreestanding` mode the transpiled C:

- does **not** `#include <string.h>`; instead `#include <jeti/runtime.h>` (freestanding branch)
- implements `@try/@catch/@finally` with the default `-eh checked` backend — plain flag + guard control flow, **no `setjmp`/`longjmp` and no `jmp_buf` at all**, which is what makes the bare-metal target work. (`-eh legacy` falls back to `__builtin_setjmp/longjmp`, with plain non-`__thread` exception globals.)
- is self-contained for `SEL`/`NPClass`/`NPObject`/`id`
- does **not** bundle the Clang Blocks runtime — block literals reference `__NSConcreteStackBlock`/`_Block_copy`/`_Block_release`; on real bare metal, either link a Blocks runtime port or use `-backend portable`/`-backend gcc` (blocks lower to plain C functions, no ABI symbols)

The user only provides: `JETI_CLASS_$_jeti_root`, the exception globals (if using `@try`), `memcpy` (if using `@try`), and freestanding headers (`stdint.h`/`stddef.h`/`stdbool.h`).

**Bare-metal allocator + `[[Class alloc] init]`** (`include/jeti/runtime_freestanding.c`):

```jeti
@interface HeapCounter {
    int total;
}
+ (id) alloc;
- (id) init;
- (int) add:(int)x;
@end
@implementation HeapCounter
+ (id) alloc  { return jeti_alloc(self); }   // bump allocator
- (id) init   { return self; }
- (int) add:(int)x { total += x; return total; }
@end

void demo(void) {
    HeapCounter *c = [[HeapCounter alloc] init];  // bare-metal heap alloc
    [c add:10];                                    // → 10
}
```

Features verified bare-metal (`examples/04_soma-kernel/` i386 protected-mode kernel + `tests/golden/25_freestanding/`):

- `@namespace` + `@interface` (implicit root class)
- Class / instance method messaging
- `@try/@catch/@finally`
- `@selector`, inline asm, C-style casts
- `[[Class alloc] init]` heap allocation + ARC auto-`jeti_release`

Sample output (soma-kernel under qemu):

```
[jeti] class method [SomaCore::Calculator compute:21] = 43
[jeti] instance methods on C-created obj: add:7 -> 7, add:35 -> 42, value = 42
[jeti] @try/@catch demo:
       try body, throwing...
       caught [e errorCode] = 42
       finally always runs
       after-try continues
[jeti] alloc+init (bump allocator):
       [c add:10]=10 [c add:20]=30 [c value]=30
```

#### Method Dispatch

All Jeti objects dispatch through a unified VTable mechanism:

```c
// [obj doSomething:arg]
obj->header.vtable[INDEX_doSomething](obj, arg);
```

The compiler assigns a fixed global index to each selector. All classes place the function pointer for the same selector at the same VTable position. If a class doesn't implement a method, the slot holds the parent's implementation or NULL.

#### Kernel-Friendly Design

The object header is minimal:

```c
struct jeti_object_header {
    struct jeti_vtable *vtable;
    // no retain count, no flags
};
```

Reference counting is managed by compile-time static ARC analysis, not stored in the object. `jeti_id_t` is a plain C pointer (8 bytes on 64-bit), zero ABI overhead for passing, assigning, and array storage.

#### Status

   Implemented:

- [x] Implicit root class injection (semantic analysis)
- [x] `jeti_root` and `jeti_object_header` C code generation
- [x] `id` → `jeti_id_t` type mapping
- [x] Unified VTable index allocation
- [x] Root/subclass struct generation
- [x] Unit test coverage

### @namespace

`@namespace` organizes classes, functions, and constants, avoiding global name collisions. This is a feature ObjC lacks — traditional ObjC relies on prefix conventions (e.g., `NS`, `UI`) to simulate namespacing.

```jeti
@namespace Game
    @interface Player : NPObject {
        int health;
    }
    - (id)init;
    - (int)getHealth;
    @end

    @implementation Player
    - (id)init {
        self = [super init];
        if (self) health = 100;
        return self;
    }
    - (int)getHealth { return health; }
    @end
@endnamespace

@namespace UI
    @interface HUD : NPObject {}
    - (void)showPlayerHealth:(Game::Player *)player;
    @end
@endnamespace
```

**Encoding rules**: `::` separators are encoded as `__` in C symbols.

| Jeti Symbol                   | Transpiled C Symbol          |
| ----------------------------- | ---------------------------- |
| `Game::Player`                | `Game__Player`               |
| `Game::Entities::Enemy`       | `Game__Entities__Enemy`      |
| Method `-[Game::Player init]` | `Game__Player_init`          |
| VTable                        | `JETI_VTABLE_$_Game__Player` |
| Class metadata                | `JETI_CLASS_$_Game__Player`  |

**Features**:

- Nested namespaces supported (`Game::Entities::Enemy`)
- Cross-namespace references (`Game::Player *player`)
- `@class` forward declarations inside namespaces (`@class Player;`) — codegen emits `struct Game__Player;` + `typedef struct Game__Player Game__Player;` so forward-declared types are usable in method signatures
- Cross-namespace inheritance (`@interface HUD : Engine::Graphics::Renderable`)
- No prefix convention needed — C symbols are encoded automatically
- Classes without namespaces remain backward-compatible

#### @using Import Mechanism

`@using` imports symbols from other namespaces into the current scope, avoiding the need to write fully qualified names each time. Three forms are supported:

**Form 1: Import a fully qualified name**

```jeti
@using Game::Player;
Game::Player *p = [[Game::Player alloc] init];
// After @using, the short name Player can be used instead
Player *p = [[Player alloc] init];
```

**Form 2: Import with an alias**

```jeti
@using GP = Game::Player;
// GP is an alias for Game::Player
GP *p = [[GP alloc] init];
```

**Form 3: Import an entire namespace**

```jeti
@using namespace Game;
// All classes under Game can be accessed by short name
Player *p = [[Player alloc] init];
Enemy *e = [[Enemy alloc] init];
```

**Conflict detection**:

- If a short name conflicts with an existing symbol in the current scope, the compiler reports an error
- If two `@using` entries import the same short name, the compiler reports an ambiguity error
- Aliases and short names are valid within the file scope of the `@using` declaration

### `@noarc` Block — Block-level MRC

In ARC mode, the checker forbids manual memory management:

```jeti
[obj release]; // error: explicit 'release' not allowed in ARC mode
```

`@noarc { }` scopes a block where you manage memory manually — the block-level analogue of `-fno-jeti-arc` (and clang's `-fno-objc-arc`):

```jeti
@noarc {
    [obj retain];
    [obj release];
    [obj autorelease];
}
```

Key points:

- **Block-level scope** — only statements inside `@noarc { }` are exempt. Everything outside still uses static ARC, and manual `retain`/`release`/`dealloc`/`autorelease` outside the block is a compile error.
- **No ARC injection** — the ARC analyzer skips `@noarc` blocks entirely, inserting no retain/release for objects used there.
- **Runtime-method exemption** — the implementations of `retain`/`release`/`dealloc`/`autorelease` themselves may call these methods without `@noarc`.
- **Whole-program analogue** — `-fno-jeti-arc` switches the whole program to MRC; `@noarc` does the same for a single block.
- **Foundation** — the NPString/NPMutableString convenience constructors (`+stringWithUTF8String:`, `+stringWithString:`) wrap their deliberate `autorelease` in `@noarc { }`.

---

### Refcount Trace (`-trace-refcount`)

`-trace-refcount` runs a static reference-count simulator over the AST **after** ARC injection, printing a chronological, color-coded trace of every retained object's count, then exits without codegen or compilation. It is a debug aid for verifying that each object is released exactly once (no leaks, no double-releases).

```bash
jetic -trace-refcount app.jeti                          # color trace
jetic -trace-refcount -trace-no-color -trace-max-iters 2 app.jeti
```

Options:

- **`-trace-no-color`** — disable ANSI colors (for diffs / CI piping).
- **`-trace-max-iters <N>`** — how many loop iterations each loop simulates (default 2); every iteration gets fresh `Cat#N` object identities.
- Colors: **green** = count increased · **blue** = count decreased (still alive) · **cyan** = freed (reached 0) · **red** = double-release/over-release and `possible leak`/`over-released` in the summary · **yellow** = informational untracked-target warnings.
- Object identity = allocation site (`Class#N` per class); creations are `alloc`/`new`/`copy`/`mutableCopy`-prefixed, `init` chains to its receiver, and `@"..."`/`@[...]` literals.
- Objects that leave the traced scope are excluded from the leak summary: `return`/`@throw` results, `static` singletons, `@"..."`/`@[...]` literals, and method parameters. A final `== Summary ==` reports live (`possible leak`), over-released (negative count), and freed objects with their allocation positions.

### `@defer` — Scope-Exit Execution

Go-style deferred cleanup: `@defer { ... }` registers its body with the innermost enclosing block, and the body runs at **every exit** of that block — the natural end, a `return` at any depth, a `break`/`continue` that jumps out of it, and a same-function `@throw` — innermost first (LIFO).

```jeti
- (void)work {
    FILE *f = fopen("cfg.txt", "r");
    @defer { fclose(f); }            // runs at every exit below
    if (!ready) { return; }          // defer runs before returning
    @defer { printf("second\n"); }   // multiple defers: LIFO
    ...
}                                    // block end: "second" first, then fclose
```

Semantics:

- **Loop bodies** re-run their defers **every iteration** — and on `continue`/`break`.
- `break`/`continue` fire only the defers registered between the jump and the innermost loop/switch; defers registered **outside** the loop are not double-fired.
- Variables are read directly — no capture, no copy (plain C scoping, deliberately unlike blocks).
- **ARC order**: user defers run *before* the ARC-injected scope-end release, so objects are still alive inside your defer (`dealloc` prints last).
- `-eh checked` needs no special case: its throws are ordinary returns by the time the defer pass runs.

M1 limits (compile-time enforced): `@defer` must sit directly inside a block; the body must not contain `return`/`break`/`continue`/`@throw` (`error: 'return' inside an '@defer' body is not supported (M1)`). A cross-function `@throw` (sjlj longjmp past intermediate frames) skips those frames' defers — the same documented limitation as ARC's scope-end releases, gone under `-eh checked`.

Implementation: pure desugar (`crates/defer`, pipeline step 3.9 — after the `-eh checked` rewrite, before ARC). Codegen, checker, and the runtime see ordinary statements — zero changes downstream. Golden: `tests/golden/36_defer/`.

### `async T` — Async Method Modifier

Bare `await` alone left one soft spot: a header cannot tell you whether a method suspends. The `async` modifier promotes async-ness to a **signature-level flag**: it sits before the return type and takes the **business type** directly (`async int`), visible in the `.hje`. A bare `NPTask<T>` return means the opposite — a synchronous method that merely returns a task object.

```jeti
@interface Fetcher : NPObject
- (async int)compute:(int)n;           // suspends, yields an int
+ (async void)runAll;                  // entry point
- (NPTask<int>)loadCached;             // bare NPTask<T> = ordinary sync method
- (int)plain:(int)n;                   // unmarked = promises never to suspend
@end
```

The body's awaits decide the truth, and the checker reconciles both directions:

| declaration | body | verdict |
|-------------|------|---------|
| `async T` | has `await` | ✅ |
| `async T` | no `await` | ✅ — legal (rule 1: `async` is a capability, "may suspend"; cache hits and sync protocol impls are normal) |
| unmarked | has `await` | **error** — `'compute:' contains 'await' but is not declared 'async' — a suspending method must declare the async modifier` |
| unmarked | no `await` | ✅ |

- The modifier is part of the signature: `@interface` and `@implementation` must agree — `'async' modifier mismatch on 'compute:': the @interface and @implementation disagree — the modifier is part of the method signature`. Header-only `@interface` methods are exempt (cross-TU safety).
- `async` takes the business type directly (`async int` / `async NPString *` / `async void`); the legacy `(async NPTask<T>)` spelling produces a migration error (see `tests/negative/async_nptask_*.jeti`).
- Task handles are not user-capturable under v1: `NPTask<T> *t = [f compute];` is an error (rule 2 removed "bare call creates a task"); handles will be born only at the explicit `+[NPTask spawn:…]` (stage 3, planned). `await` remains the only way to read a result, and awaiting the same task twice is legal (the result is cached).
- `NPTask` is a reserved class name.
- **One bridge, no entry positions** — `[NPTask blockOn:async ^ void { … }]` pumps an async block to completion at the call site (lowered to `jeti_task_await`); it is the only sync→async path, in `main`, sync methods, and freestanding code alike. The old statement-position entry (`[f runAll];` auto-drive, statement-position `[t start];`) is gone — a statement-position `[t start];` now merely enqueues, and bare task-yielding statements are checker errors. There is still no pump at `main`'s exit — ARC's scope-end release lands later.

Golden: `tests/golden/37_async_modifier/`.

### Object Subscripting (`a[0]` on NPArray)

`recv[i]` and `recv[i] = v` on container objects now work as sugar. The checker rewrites them — type-aware, judged by the **symbol table** (does the class, or a superclass, actually declare the methods?), not by "looks like an object":

| source | rewritten to | condition |
|--------|--------------|-----------|
| `recv[i]` | `[recv objectAtIndex:i]` | receiver's class declares `objectAtIndex:` |
| `recv[i] = v` | `[recv setObject:v atIndex:i]` | class also declares `setObject:atIndex:` |

```jeti
NPArray *a = @[ @"x", @"y", @"z" ];
NPLog(@"%@", a[0]);            // → [a objectAtIndex:0]
NPMutableArray *m = [NPMutableArray array];
[m addObject:@"first"];
m[0] = @"hello";               // → [m setObject:@"hello" atIndex:0] — replaces, not appends
```

Plain C is never touched: `int c[3]; c[1]`, `char *p; p[0]`, and `const char *s; s[2]` all pass through as raw C subscripts (probe-verified, zero false positives). The rewrite lands in the checker (not the parser — the parser has no variable types, and the emit stage has no vtable metadata), so downstream vtable dispatch, nil guards, and SEL constants work with zero special cases.

Dictionary subscripting (`d[@"k"]`) is deliberately **not** part of this rewrite: the mapping is `objectAtIndex:`-only, so `NPDictionary` does not declare `objectForKeyedSubscript:` — that would advertise a spelling which the rewrite would send to the wrong selector. Use `[d objectForKey:@"k"]`.

### Real Generic Checking (monomorphization + element types)

Generic containers **monomorphize and are type-checked**. `NPArray<NPString *>` and `NPDictionary<NPString *, NPNumber *>` generate real specialized C (struct, vtable, class metadata, method copies with substituted types), and the checker substitutes the element types into method signatures — so the element type is enforced, not erased:

#### True generics across translation units

Jeti's generics are deliberately different from Objective-C lightweight generics. Objective-C keeps one runtime class and uses generic arguments mainly as compiler annotations. Jeti keeps the source spelling familiar (`Factory<NPString *>`) but generates a real monomorphized class: a distinct C struct, methods, vtable, metadata, and ABI for each concrete argument list. There is no type-erased fallback for a specialized use.

In a multi-TU build, the compiler first scans all `.jeti` inputs for concrete specializations and forwards that demand to every TU. A TU emits the specialization only when it contains the generic implementation body; declaration-only TUs emit references to the same mangled specialization. This makes the following work without a dummy variable in the library TU:

```jeti
// model.hje — shared declaration
@interface Factory<T> : NPObject
+ (T)make;
@end

// lib.jeti — implementation TU
#import "model.hje"
@implementation Factory
+ (T)make { return nil; }
@end

// main.jeti — client TU
#import "model.hje"
int main(void) {
    NPString *s = [Factory<NPString *> make];
    return s == nil ? 0 : 1;
}
```

Build both inputs together: `jetic main.jeti lib.jeti -I . -o app`. The implementation must be available in one of the inputs (or in the source/module form used to build the library). A precompiled library can provide a fixed set of specializations, but it cannot invent a new method body for an argument type whose implementation was not shipped. If two TUs provide the same specialization, the normal owner/strong-metadata rules reject the duplicate definition instead of silently choosing an ABI.

This is the intended trade-off: Jeti matches Objective-C's call-site style, while its semantics are closer to C++ templates—concrete types are checked and compiled into separate code, and unused specializations do not exist.

```jeti
NPMutableArray<NPString *> *m = [NPMutableArray array];
[m addObject:@"a"];
NPString *s = [m objectAtIndex:0];      // NPString *, not id

[m addObject:@42];                      // ✗ error: NPNumber* into an NPString* container
int bad = [m objectAtIndex:0];          // ✗ error: pointer into scalar
```

`@[...]` and `@{...}` literals **infer** their element types when every element agrees, so `NPArray<NPString *> *a = @[ @"x", @"y" ];` needs no annotation; a mixed array falls back to bare `NPArray`.

Both spellings coexist: bare `NPArray` stays fully supported (zero migration) and simply erases to `id`. Assigning a bare container into a specialized variable is allowed but warns, because the element type is then unverified:

```text
warning: assigning a bare 'NPArray *' to a specialization of it — the bare
container's element type is unchecked; add an explicit cast if the contents are known to match
```

`-Werror` escalates it. `NPArray<A>` and `NPArray<B>` remain mutually assignable without complaint — the same permissiveness as ObjC lightweight generics (you asked for `id` back, you get `id` back).

Note the cost: specialization is compile-time code, not free type safety. The same program using containers generically instead of bare compiles to ~42 KB / +41% more C — all duplicated method bodies and metadata, byte-identical layout, so zero runtime benefit. Golden: `tests/golden/40_nparray_generic/`.

### Generic Protocol Bounds (`T : Proto`)

Type parameters accept class-level constraints — ObjC spelling (`T : id<Summable>`), bare protocol name (`T : Summable`), or a class pointer (`T : NSObject *`); all are stored and diagnosed as the bare name:

```jeti
@protocol Greetable
- (NPString *)greeting;
@end

@interface Box<T : Greetable> : NPObject {
    T _value;
}
- (instancetype)initWithValue:(T)value;
@end

// multi-param: only V is constrained
@interface Pair<K, V : Comparable> : NPObject { ... }

// subclass must RE-DECLARE the inherited bound (explicit spelling, same
// philosophy as full ivar layouts in shared .hje headers) and must not
// weaken it
@interface MutableBox<T : Greetable> : Box<T> { ... }
```

The checker enforces bounds at **explicit specialization points** (`Box<Dog *> *b = ...;`): a violating argument is an error (all violations reported at once). Escape channels — `id`, `instancetype`, nested type-param slots, forward-declared shells, unresolvable bound names — pass silently (a missed report beats a false one, same philosophy as the rest of the checker). Bare spellings (`Box *`) never trigger: erasure compatibility, today's code keeps compiling. Bounds are pure compile-time metadata — **zero codegen**, golden output byte-identical; `-fno-checker` turns the check off. Method-level constraints (`where U : P`) are not supported. Golden: `tests/golden/46_generic_bounds/`.

### Jeti-Syntax Macros (dual-track `#define`)

`#define` bodies containing **jeti syntax** (`[recv msg]`, `@`-literals, `^{}` blocks) used to be passed through verbatim to the C compiler — a syntax error. jetic now parses and expands them at the source level. Plain-C macro bodies pass through unchanged and are expanded by the C compiler as before; behavior is identical there.

```jeti
#define TAG(o)      [o tag]                    // jeti track: expanded by jetic
#define BUMP(o, n)  [o addTo:n times:1]
#define LOG(x)      NPLog(@"tag=%d", x)        // body contains an @literal
#define TWICE(x)    ((x) + (x))                // C track: expanded by clang

int t = TAG(w);                                    // → [w tag]
BUMP(w, 3);
LOG(TAG(w));
```

Expansion rules follow ISO C §6.10.3 (implemented independently in `crates/cpp`, cross-checked line-by-line against `clang -E`): arguments are fully expanded before substitution (`#`/`##` operands use raw text), `#param` stringifies, `a ## b` pastes, `__VA_ARGS__` joins with commas, self-recursive macros freeze (blue-paint), a function-like macro's bare name outside a call does not expand, and `\` continuations join logical lines. Conditional directives (`#if`/`#ifdef`/`#ifndef`/`#elif`/`#else`/`#endif`) are evaluated by jetic too — `defined(X)` operands are exempt from expansion, skipped groups don't define macros, and malformed conditionals error instead of silently swallowing the file.

Limits (clear errors, not silent): a macro invocation must close on one line (use `\` to continue), and macro bodies may not contain `_Pragma`. Golden: `tests/golden/38_macros/`.

### C99 Designated Initializers

All six C99 designated-initializer forms work, including the ones ObjC's C subset never needed:

```jeti
struct Point { int x; int y; };
struct Point p1 = { .x = 1, .y = 2 };      // 1. full designated
struct Point p2 = { .y = 5 };               // 2. partial — omitted fields zero-filled
struct Point p3 = { .x = 1, 7 };           // 3. designated mixed with positional

NPRange r = (NPRange){ .location = 3,      // 4. compound literal + designators
                        .length = 9 };

CGPoint pts[3] = { [0].wx = 1, [2].wy = 6 };  // 5. array elements

struct Outer o = { .in.a = 3, .tag = 9 }; // 6. nested member chains
```

Positional entries continue from the last designated field (form 3 puts `7` in `.y`), unspecified fields are zero-filled, and designator chains like `[1].wx` work too. They pass through as ordinary C initializers — no new IR.

### C99 `_Complex` Passthrough

`float _Complex` / `double _Complex` declarations, typedefs, and parameters pass through untouched, and imaginary literals (`2.0i`, `1e3j`) are emitted **raw** — the imaginary part used to be silently dropped (`2.0i` → `2.0f`).

```jeti
#include <complex.h>
typedef float _Complex cfloat;

double _Complex z = 1.0 + 2.0i;   // mixed real + imaginary
double _Complex w = 2.0i;         // bare imaginary
cfloat f = 1.5;
printf("A=%.1f+%.1fi\n", creal(z), cimag(z));
```

Known limit: the jeti checker has no complex type inference (narrowing between complex widths isn't warned; semantics are enforced by the C compiler). Golden: `tests/golden/39_complex/`.

---

## Compilation & CLI

### Command-Line Options

```bash
jetic [options] <input.jeti>

Modes:
  (none)            Default: transpile + compile to binary (requires -o)
  run               Transpile + compile + run (auto-clean temp binary)

Options:
  -o <file>         Output file (.o produces object file, otherwise executable)
  -I <dir>          Add include search path
  -L <dir>          Add library search path
  -v, --verbose     Show verbose output (including Clang warnings)
  -V, --version     Show version number
  -rewrite-jeti     Output C code only (no compilation)
  -fjeti-arc        Enable ARC (default)
  -fno-jeti-arc     Disable ARC (manual MRC mode)
  -fno-checker      Skip type checking
  -eh <mode>        Exception backend: checked (default) or legacy (alias sjlj)
  -ffreestanding    Bare-metal/freestanding output (no libc, no TLS)
  -backend <mode>   C compiler backend: clang (default), portable, or gcc
  -arch <target>    Build for target architecture (e.g. -arch x86_64)
  -asm <file.s>     Link a real assembly file (repeatable)
  -gen-completions <shell>  Generate shell completion script (zsh|bash|fish)
  -emit-bridge-header <file.h>  Generate a C bridge header for calling Jeti from C

Refcount trace (debug aid):
  -trace-refcount                Print a static reference-count trace of each retained object, in source order
  -trace-max-iters <N>           Loop iterations simulated in the trace (default 2)
  -trace-no-color                Disable colors in the trace
```

### Build System Integration

**cargo**:

```bash
cargo build
cargo test --workspace
```

---

## Code Examples

### Precompiled Foundation Library

Instead of inlining Foundation into every TU (`#import <Foundation/Foundation.jeti>`, the self-contained umbrella), build it once as a static library and link every project against it — faster per-file compiles, one copy of the implementation:

```bash
./tools/build-foundation-lib.sh            # → target/foundation/libjetifoundation.a
```

The script transpiles each Foundation `.jeti` as its **own translation unit** (a generated wrapper prepends the full declaration surface, then inlines the implementation text), compiles, and archives. Because each `@implementation` lands in its TU's main file, R2 ownership automatically emits that class's metadata as STRONG symbols — the script nm-verifies all nine and fails loudly if any come out weak. No `-fstrong-metadata` exists any more: ownership is derived by construction.

Clients then import only the declaration header:

```jeti
#import <Foundation/Foundation.hje>    // declarations only — no implementations inlined

int main() {
    NPString *s = [NPString stringWithUTF8String:"hello"];
    NPLog(@"%@", s);
    return 0;
}
```

```bash
jetic app.jeti -I include -L target/foundation -ljetifoundation -o app   # explicit

# … or let jetic find and link the library itself:
jetic app.jeti -o app
```

Notes:

- **No flags to remember** — a main file holding `@implementation` is strong automatically; declaration-only clients stay weak, which is correct (the library's tables win the link).
- **Auto-link** — jetic links `libjetifoundation.a` automatically when it can find one (next to the binary, `target/foundation`, `/opt/jeti/lib`, `/usr/local/lib/jeti`, or your `-L` dirs). It only fires for **declaration-only clients**: a TU that inlines Foundation implementations (`Foundation.jeti`, directly or through an imported `.hje`) is skipped, so self-contained programs and multi-TU builds never see the library's strong vtables. `-ffreestanding`, shared mode, and an explicit `-ljetifoundation` all suppress the auto link.
- **`jeti_metaInit()`** is only *required* for umbrella builds that reach implementations through `#import "*.jeti"` (single-TU builds where no TU owns the metadata). With the precompiled library — and with every normal `jetic` workflow — metadata is statically initialized at load time and the call is an idempotent no-op.
- Re-implementing a library class in a client is standard C override semantics against the archive (the library's member stays dormant unless referenced) — but slots for methods you do not implement stay NULL, so implement everything you dispatch.

### Hello World

```jeti
#include <stdio.h>
#import <Foundation/Foundation.jeti>

@interface Greeter : NPObject
- (void)greet;
@end

@implementation Greeter
- (void)greet {
    printf("Hello, Jeti!\n");
}
@end

int main() {
    @autoreleasepool {
        Greeter *g = [[Greeter alloc] init];
        [g greet];
    }
    return 0;
}
```

### Polymorphism

```jeti
@interface Animal : NPObject
- (void)speak;
@end

@interface Dog : Animal
@end

@interface Cat : Animal
@end

@implementation Animal
- (void)speak { printf("...\n"); }
@end

@implementation Dog
- (void)speak { printf("Woof!\n"); }
@end

@implementation Cat
- (void)speak { printf("Meow!\n"); }
@end

int main() {
    Animal *animals[2];
    animals[0] = [[Dog alloc] init];
    animals[1] = [[Cat alloc] init];
    for (int i = 0; i < 2; i++)
        [animals[i] speak];  // VTable static dispatch
    return 0;
}
```

### Block + ARC

```jeti
typedef void (^EventHandler)(int code, NPString *msg);

@interface Engine : NPObject
- (void)onEvent:(EventHandler)handler;
@end

int main() {
    @autoreleasepool {
        Engine *e = [[Engine alloc] init];
        int captured = 42;
        [e onEvent:^void(int code, NPString *msg) {
            printf("code=%d msg=%s captured=%d\n", code, msg, captured);
        }];
    }
    return 0;
}
```

### Static Generics

Jeti compiles generics at compile time via **monomorphization** — each `DataPack<QuantumToken *>` becomes a standalone C struct `DataPack_QuantumToken_ptr` with concrete type substitutions. No type erasure, no boxing, no runtime overhead.

```jeti
@interface DataPack<T> : NPObject {
    @public
    int _count;
    T _storage[2];
}
- (void)pushItem:(T)item;
- (T)popItem;
@end

@implementation DataPack
- (void)pushItem:(T)item {
    if (_count < 2) {
        _storage[_count++] = item;
    }
}
- (T)popItem {
    if (_count > 0) {
        _count--;
        T item = _storage[_count];
        _storage[_count] = 0;
        return jeti_autorelease(item);
    }
    return 0;
}
@end

int main() {
    @autoreleasepool {
        // Each instantiation generates specialized C code
        DataPack<QuantumToken *> *tokenPack = [[DataPack<QuantumToken *> alloc] init];
        DataPack<EncryptedMetric *> *metricPack = [[DataPack<EncryptedMetric *> alloc] init];
    }
    return 0;
}
```

**How it works**:

- `DataPack<T>` → `struct DataPack` (generic) is skipped; only specialized structs are emitted
- `DataPack<QuantumToken *>` → `struct DataPack_QuantumToken_ptr` with `QuantumToken * _storage[2]`
- Methods are cloned per instantiation with substituted return/param/body types
- VTable, meta VTable, and class metadata are generated per instantiation
- Type name encoding: `DataPack<QuantumToken *>` → `DataPack_QuantumToken_ptr`

**Status**: ✅ Fully implemented for single type parameter. Multiple parameters (`<K, V>`) in progress.

---

## Design Principles

### 1. Static > Dynamic

ObjC's runtime is powerful, but I don't want to depend on it. Make all decisions at compile time — what you generate is what runs, no surprises.

- Method dispatch → VTable offsets
- Memory management → CFG static analysis
- Protocol conformance → compile-time checks

### 2. Generate Human-Readable C

Jeti's "backend" is **human-readable C99**, not LLVM IR. This means:

- Debug with standard Clang/LLDB tools
- Generated C can be reviewed, modified, embedded in other projects
- No LLVM backend lock-in — wherever Clang runs, Jeti runs

### 3. Incremental

Start from a class system, add things gradually:

- ✅ Class/Protocol/Category/Properties
- ✅ Block / @autoreleasepool
- ✅ Static ARC
- ✅ @selector / VTable polymorphism
- ✅ @namespace
- ✅ Exception handling (`@try`/`@catch`/`@finally`/`@throw`) — **default backend is `-eh checked`** (flag + guard lowering, unwind-safe ARC: a cross-function throw releases every frame's owned locals; no `setjmp`/`longjmp`, so it works on bare metal)
  - `-eh legacy` (alias `-eh sjlj`) selects the old `setjmp`/`longjmp` backend. ⚠️ Its documented limit (verified with ASan): an object owned by an **intermediate frame** leaks on a **cross-function throw** — `longjmp` skips its scope-end `jeti_release`. That limit does not apply to the default backend.
- ⏳ Foundation standard library
- ⏳ Compiler self-hosting

### 4. Readability

Generated C should be as clear as handwritten C:

- `struct` + `->` for ivar access
- `static const SEL` constants
- Consistent and predictable naming
- Explicit temporary variable names

---

## Roadmap

### Phase 1: Infrastructure ✅

- [x] Lexer
- [x] Preprocessor
- [x] Parser
- [x] CST validation & printing

### Phase 2: Semantic Analysis ✅

- [x] Symbol table
- [x] Name binding
- [x] Type checking
- [x] Property elaboration
- [x] Protocol conformance

### Phase 3: VTable + Object Layout ✅

- [x] VTable layout
- [x] Object memory layout
- [x] Class metadata

### Phase 4: Intermediate Representation ✅

- [x] Typed AST
- [x] CST → AST
- [x] CFG construction

### Phase 5: Static ARC ✅

- [x] Ownership inference
- [x] Local + global ARC
- [x] Retain/Release insertion
- [x] ARC verification

### Phase 6: C99 Code Generation ✅

- [x] C99 AST
- [x] AST → C99 conversion
- [x] Header generation
- [x] Compiler options

### Phase 7: Runtime ✅

- [x] Core retain/release/alloc/init
- [x] Autorelease pool

### Phase 8-10: In Progress

- [ ] Foundation standard library

- [x] Block runtime

- [x] Weak references

- [✅] Generics (monomorphization)

- [x] Exception handling (`@try`/`@catch`/`@finally`/`@throw`; `-eh checked` for full unwind-safe ARC)

- [ ] Debug information

- [x] VSCode/IDE support

---

## FAQ

### **Is it production-ready?**

Not yet. But it is **real** - it compiles, it runs, and it is designed with growth in mind. If you find syntax appealing and want to contribute, you are welcome.

### What can Jeti do?

Write small games and tools. The snake game, Flappy Bird, space shooter, tic-tac-toe in this repo are all written in Jeti, running in the terminal.

### What's missing compared to ObjC?

- No `objc_msgSend` — VTable static dispatch
- No runtime Method Swizzling
- No `forwardInvocation:`
- Selectors are compile-time constants, not runtime strings

### Why C99 as output?

Because C99 compiles everywhere. Generate human-readable C, compile with Clang, debug with lldb. No need to bind to any specific backend.

---

## License

Jeti is licensed under the [Apache License, Version 2.0](LICENSE).

Copyright © 2026 3shine123

- Full license text: [`LICENSE`](LICENSE) · notices: [`NOTICE`](NOTICE)
- Compiling code with Jeti places no licensing obligation on the output: the
  C files `jetic` generates, and the binaries you build from them, are yours.
