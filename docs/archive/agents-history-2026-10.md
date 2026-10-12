# AGENTS.md — Jeti Language Project

## Current Phase: Stage 3 — Codegen + Testing

## File Extension Convention
- **`.jth`** — Jeti header (declarations: `@interface`, `@protocol`, typedefs, structs). These are inlined by jetic's preprocessor via `#import`, never passed to the C compiler directly.
- **`.jeti`** — Jeti implementation (definitions: `@implementation`, functions, `int main`). Inlined via `#import` and translated to C.
- Headers use `.jth` (not `.h`) deliberately: a `.h` extension would collide with ObjC/C system headers (e.g. `Foundation.h` vs `Foundation/Foundation.h`). jetic's preprocessor only treats `#import` of `.jth`/`.jeti` as jeti imports; `#include` of `.h`/`.c` is passed through verbatim to the C compiler.
- When a header is meant to be usable directly by a plain C compiler (not just via jetic), guard the objc-style syntax with `#ifdef __JETI__` / `#else`. jetic always defines `__JETI__` (all backends), so it inlines the jeti branch; a C compiler sees only the C-compatible `#else` branch.

## Build & Test Commands
```bash
# Build everything
ninja -C builddir

# Run all unit tests (6 suites)
ninja -C builddir test

# Run individual unit tests
./builddir/test_lexer
./builddir/test_parser
./builddir/test_cst_print
./builddir/test_cst_visit
./builddir/test_symbol
./builddir/test_binder
./builddir/test_checker

# Run integration tests
./builddir/test_codegen
./builddir/test_codegen_header
./builddir/test_codegen_convert
./builddir/test_codegen_emit

# Run one specific integration test
./builddir/test_elaborator  # run without args
```

## Project Map
```
jeti-lang/
├── transpiler/
│   ├── src/
│   │   ├── main.c              — CLI entry point
│   │   ├── lexer.c/h           — Lexer: tokenizer; prep for parser
│   │   ├── parser.c/h          — Recursive-descent parser → CST
│   │   ├── cst.h               — Concrete Syntax Tree types + alloc
│   │   ├── cst_print.c/h       — Debug printer for CST
│   │   ├── cst_visit.c/h       — Visitor pattern over CST
│   │   ├── elaborator.c/h      — Semantic elaboration: @interface/impl matching, protocol merging
│   │   ├── binder.c/h          — Name binding: symbol creation for typedefs, classes, protocols, @selector
│   │   ├── checker.c/h         — Type checker
│   │   ├── jeti_type.h         — NP type representation (jeti_type_t, TYPE_* enum, type functions)
│   │   ├── symbol.c/h          — Symbol table (symtab_*): types, classes, protocols, methods, selectors
│   │   ├── codegen.c/h         — Code generator (C output)
│   │   ├── codegen_emit.c/h    — Emit C code from AST
│   │   ├── codegen_header.c/h  — Header file generation
│   │   ├── codegen_msg.c/h     — Message send codegen
│   │   ├── config.h            — Build config (header from meson)
│   │   └── str.c/h             — String utilities (optional)
│   └── include/jeti/
│       └── public headers
├── tests/
│   ├── unit/
│   │   ├── test_lexer.c
│   │   ├── test_parser.c
│   │   ├── test_cst_print.c
│   │   ├── test_cst_visit.c
│   │   ├── test_symbol.c
│   │   ├── test_binder.c
│   │   └── test_checker.c      — 15 tests: basic expressions, control flow, interfaces, protocols, protocol inheritance
│   └── integration/
│       ├── main.m (or .jeti)     — Integration test input
│       └── expected/           — Expected C output
├── builddir/                   — Ninja build directory
├── meson.build                 — Top-level build
├── meson.options               — Build options
└── AGENTS.md                   — This file
```

## Symbol Table API
- `symtab_alloc()` / `symtab_free(st)` — lifetime
- `symtab_register(st, name, kind, data)` — register any symbol
- `symtab_lookup(st, name)` — find symbol by name (NULL if not found)
- `symtab_register_type(st, name, cst_type)` — register a typedef/struct/union/enum
- `symtab_register_selector(st, sel_name)` — register a selector name, returns existing if already registered
- `symtab_find_selector(st, sel_name)` — find a selector by name
- Selectors are stored in `st->sels[]` dynamic array (realloc'd)
- Selectors are freed in `symtab_free`

## Checker Protocol Lookup
- `check_protocol_method(c, e, proto, sel, arg_count)` — recursive search of protocol + parents for a method matching selector
- Method: searches `required_methods[]`, then `optional_methods[]`, then recurses into `parents[]`
- Returns cloned `jeti_type_t` of matched method's return type

## Block Type Signature
- `CST_EXPR_BLOCK` returns `jeti_type_t` with `is_block=1`, `subtype=return_type`, params linked via `next`
- `e->type` set to `cst_type_t` with `is_block=1`, `subtype=return type CST`

## Completion Status
- [x] Stage 1: Parser, CST, Elaborator, Codegen — complete
- [x] Stage 1.4: CST visitor pattern + validation — complete
- [x] Stage 2: Elaborator, Binder — complete
- [x] Stage 3: Codegen improvements — complete (binary operators, @selector, blocks, @synthesize, arrays, for-in, @synchronized, protocol metadata, postfix ++/--, match_name(), class method dispatch, super, sel_registerName, vtable ordering)
- [x] Checker: P0 type checking — enabled by default, uses `scope_vars` for params/locals, `types_compatible` for assignment/return, protocol conformance

### Generic Monomorphization Status: Partial
| Component | Status | Details |
|-----------|--------|---------|
| Struct emission | ✅ | Generic structs skipped; specialized `struct DataPack_QuantumToken_ptr` emitted with substituted ivar types |
| Method emission | ✅ | Generic methods skipped; `DataPack_QuantumToken_ptr_init` emitted with substituted return/param types |
| Vtable struct | ✅ | Specialized `jeti_DataPack_QuantumToken_ptr_vtable` emitted per instantiation |
| Meta vtable struct | ✅ | Specialized `jeti_DataPack_QuantumToken_ptr_meta_vtable` emitted per instantiation |
| Class metadata | ✅ | Specialized `jeti_DataPack_QuantumToken_ptr_class` emitted with `sizeof(struct DataPack_QuantumToken_ptr)` |
| Ivar cast (method bodies) | ✅ | `((struct DataPack *)(self))` → `((struct DataPack_QuantumToken_ptr *)(self))` via `codegen_current_class` override |
| Caller-side class metadata | ✅ | `&jeti_DataPack_QuantumToken_ptr_class` used instead of `&jeti_DataPack_class` |
| Vtable dispatch in callers | ✅ | `((struct jeti_vtable *)...)->methods[INDEX]` uniform dispatch (replaces per-class vtable casts) |
| Method body type substitution | ✅ | `T item = _storage[_count]` → `QuantumToken * item = ...` (via `substitute_stmt_types` in codegen) |
| `T` in variable declarations | ✅ | `T item = _storage[_count]` → `QuantumToken * item` (via `parse_statement` generic param check) |
| Caller-side function name | ✅ | `NPObject_alloc` used instead of `DataPack_alloc` (superclass chain fixed) |
| Superclass chain | ✅ | `DataPack->data.cls.superclass = NPObject` (parser now handles `: superclass` after `<T>` generics) |
| Debug prints removed | ✅ | 11 lines removed across codegen.c, symbol.c, parser.c, binder.c |

### Remaining Issues
- **✅ 容器泛型已规划**：`NPArray<T>` 应使用 monomorphization 机制生成特化结构体（如 `NPArray_NPString_ptr`），`objectAtIndex:` 返回 `T` 而非 `id`。当前暂不做，但机制已就绪（参见 `DataPack<T>` 的实现模式）。

### Fixes Applied (July 2026)
- ✅ `collect_from_type` now compares type arguments (not just count) — fixes `VectorBuffer<RenderPoint2D*>` vs `VectorBuffer<RenderColorRGB*>` dedup
- ✅ Meta vtable struct generated for specialized classes with superclass (even with 0 class methods)
- ✅ Meta vtable instance generated for classes with superclass (even with 0 class methods)
- ✅ Message send receiver: generic types like `[VectorBuffer<RenderPoint2D*> alloc]` now parsed correctly (parser tries type parsing before expression for `[` receivers)
- ✅ `cst_type_clone` now copies `type_args` — fixes block parameter type arg loss
- ✅ `mangle_type_name` buffer size calculation fixed (handles `*`→`_ptr` expansion)
- ✅ Block function param types now handle generic type args with mangled names
- ✅ Block variable declaration types now handle generic type args with mangled names

### Fixes Applied (Current Session — July 2026)
- ✅ `__jeti_root` fix: added `release`/`retain` methods to `__jeti_root` interface/implementation in `NPObject.jth`
- ✅ Codegen vtable class fallback: class method on runtime-determined receiver (e.g. `[[self class] alloc]`) now passes receiver expression as class argument
- ✅ `[obj class]` message send: special-cased in codegen to emit `((NPClass *)((NPObject *)obj)->isa)` — direct ivar access
- ✅ `@try`/`@catch`/`@finally` codegen: `@try` body is now emitted as plain compound (previously empty); `@throw` body emits the expression instead of `/* stub */()`
- ✅ **`@selector` codegen**: changed from emitting raw FNV-1a hash as `unsigned` int to emitting `sel_registerName("selName")` — fixes `SEL` type mismatch (`include/jeti/runtime.h:SEL` is a struct, not unsigned). See `codegen.rs:1062-1065`.
- ✅ **`@try`/`@catch`/`@finally` exception handling**: fully implemented using `setjmp`/`longjmp` with TLS globals (`__jeti_exception_buf`, `__jeti_exception_value`) in the runtime. `@throw expr` now evaluates the expression, stores it, and longjmps to the nearest setjmp in the enclosing `@try`. Catches declare the catch variable from `__jeti_exception_value`. Finally blocks always execute. **Nested @try fully supported**: each try saves/restores the parent jmp_buf; uncaught exceptions in inner @try propagate to outer @catch after inner @finally. See:
  - `include/jeti/runtime.h`: added `<setjmp.h>` + TLS globals
  - `include/jeti/runtime.c`: TLS globals definition
  - `codegen.rs:1221-1335`: `convert_stmt` rewrites for Try/Catch/Finally/Throw
- ✅ **Fix 4 — Uniform VTable index enum**:
  - `enum jeti_vtable_index` emitted with all instance method names globally sorted
  - `struct jeti_vtable { void (*methods[N])(); }` replaces per-class vtable structs
  - Vtable instances: `[INDEX] = (void (*)())func` designated init
  - Dispatch: `((RT(*)(...))((struct jeti_vtable *)recv->isa->vtable)->methods[INDEX])(args)`
  - Multi-class isa‑comparison chain eliminated — single uniform access for all classes
  - All Rust unit tests pass; generated C code compiles without errors
- ✅ **Fix 5 — `NPObject *` alloc+init `=` emit** (July 2026 Session 2): When a variable of type `NPObject *` is initialized via the alloc+init vtable-dispatch pattern, the split-emit path now always includes `=` (not just when `needs_cast` is true for subclass types). Fixes `Cat *a = [Cat alloc]`-style code generation where the init function call was emitted without the assignment operator, causing C99 parse errors. See `codegen.rs:3682-3696`.
- ✅ **Fix 6 — Deduplicate `#include <string.h>`**: Check pre-existing C headers for `<string.h>` before unconditionally emitting it. See `codegen.rs:3913-3916`.
- ✅ **Fix 7 — Block typedef namespace prefix**: Replace short block name (e.g. `ActionCompleteBlock`) inside block type strings with namespace-qualified flat name (e.g. `Extension__ActionCompleteBlock`) so the block typedef refers to itself directly. No separate alias line is emitted. All references (ivar types, function params, vtable member types) use the flat name via `BLOCK_TYPEDEF_NAMES` lookup. See `codegen.rs:1758-1788`.

### Fixes Applied (Current Session — July 2026, Session 3)
- ✅ **Postfix `++`/`--` codegen**: operand now wrapped in parentheses — `(*p)++` instead of `*p++` (which C parses as `*(p++)`). Fixes all JSON parser pointer corruption.
- ✅ **JSON Editor split-screen preview**: `refreshDisplay` clears screen and shows JSON tree in top half, command prompt in bottom half, using ANSI escape codes.
- ✅ **Multi-file compilation test**: `json_editor.jeti` now `#import`s `json_editor_types.jth` (separate file with all @interface declarations). Compiler resolves cross-file types correctly.
- ✅ **ARC analysis rewritten**: `arc_local_analyze` now recursively traverses nested scopes, directly inserts `jeti_release` into AST, handles early return/throw/break/continue, passes `parent_vars` through If/While/For branches, and inserts releases before control flow exits.
- ✅ **`@selector` test fix**: `selector_usage_test.jeti` and `golden/10_edge_cases/selector.jeti` updated to use `SEL` type instead of `unsigned`.
- ✅ **Block literal params use `AstType`**: `AstExprData::Block` now uses `Vec<(AstType, String)>` instead of `Option<Box<CstParam>>`, so parameter types get `class_ref` resolution and correct mangled names.
- ✅ **`jetic` binary uses absolute paths**: `compile_to_binary` and `Pipeline.search_dirs` now resolve include paths relative to the binary's location, not the current working directory.
- ✅ **Test runner ARC retry**: tests that fail with ARC are automatically retried with `-fno-jeti-arc` (MRC fallback).
- ✅ **Checker restored** (`crates/checker/src/lib.rs`): implements P0 type checking — assignment compatibility, return type matching, variable declaration checks (duplicate detection, init type), protocol conformance (required methods from symbol table, protocol parent inheritance), error reporting with `file:line:col: error:` format.
- ✅ **`-fno-checker` CLI flag**: added to `main.rs`; `no_checker` field on `Pipeline`; checker runs by default before codegen.
- ✅ **Checker `FuncCall` / `MsgSend` return types**: `MsgSend` returns `id`, `FuncCall` returns `id` instead of `int`.
- ✅ **Checker `types_compatible` rules**: `Named` types (class ptrs) compatible with `id` and each other.
- ✅ **Checker method/function params**: added to `scope_vars` so param names are visible in method bodies; `lookup_var_type` now checks `scope_vars` in addition to symtab/ivars.
- ✅ **Test count raised from 125/141 → 134/141** (ARC retry + checker fixes).
- ✅ **Checker `match` arm ordering fix**: Moved `_ =>` wildcard to end of `check_stmt` (was before `Decl`/`Throw`/`Try`/`Catch`/`Finally`/`Synchronized`/`Autoreleasepool`, making them unreachable). Fixes duplicate declaration, init/assign type mismatch, and all statement-body checking.
- ✅ **Checker `types_compatible` pointer guard**: numeric compatibility now requires both sides to be non-pointer (`!is_pointer`), preventing `int = "hello"` from passing.

### C Superset Support — Implemented ✅ (Aug 2026)
- ✅ **Struct definitions**: codegen now emits struct fields (was emitting empty `struct Name {};`). Fields converted from `AstDeclData::Aggregate` ivars via `ast_type_to_c_str`. Structs are emitted before function prototypes in the output to avoid `-Wvisibility` errors.
- ✅ **`struct Name *p` declarations**: parser fall-through path now consumes `*` pointer suffix(es) so `struct Widget *pw = ...` is a proper variable declaration (was `*pw = ...` expression statement).
- ✅ **Function pointer types**: `T (*)(params)` and `T (*name)(params)` parse via `parse_type_full` (new `is_fn_ptr` flag on `CstType`/`AstType`); rendered as `ret (*name)(params)` by `cst_type_to_c_str`/`ast_type_to_c_str`. Variable emission treats `(*` like `(^` — the declarator name is inside the type, so no separate name is appended.
- ✅ **C-style casts** `(struct uiEntry *)data`, `(void (*)(struct uiButton *, void *))cb`, `(void *)ptr`: parser + AST + codegen already supported the general cast path.
- ✅ Golden test: `tests/golden/22_c_superset/c_superset.jeti`.
- ✅ **Struct 全量补全（Aug 2026）**——极端 C/struct 语法矩阵 `tests/c_struct_extreme_test.jeti`（已纳入 test_all，193/203 全绿色）：
  - ✅ `typedef struct Tag Alias;`（tag 引用别名，tag 可仅前向）
  - ✅ struct 定义**依赖拓扑排序**：`typedef struct Outer { struct Inner i; }` 现在会让 `struct Inner{...}` 先发射（此前 InnerG 定义在用户之后 → C `incomplete type` 报错）
  - ✅ **函数指针字段** `int (*cb)(int);` 正确发射（此前 fnptr 字段丢失 body）
  - ✅ typedef 数组 `typedef int Row4[4];` 与**多维数组字段** `int cells[4][4];`
  - ✅ 位域 `unsigned a : 3;` 可解析（宽度按 C 语义被丢弃，降级为普通字段——近似语义）
  - ⚠️ 已知不支持（记录中）：**匿名内联 struct 字段**（`struct { int a; } inl;`，需命名 tag）；**fnptr 数组字段** `(*name[N])(...)` declarator；"Cast 后作函数指针调用"（jeti-world 用纯 C 桥 hostcb/modh 绕过）。~~"struct 成员函数指针调用"~~ → **✅ 已支持（Sep 2026 修复）**：`w.cb(3)` / `pw->cb(3)` 直接调用（elaborator 保留 PropRef callee + codegen `render_callee_expr` 渲染 `.`/`->` 成员调用，三个后端全过）
- ✅ libui demo rewritten in pure Jeti: callbacks (`onGreet`/`onInc`/`onDec`/`onReset`/`counter_ctx_create`) moved from `libui_demo_cb.c` into `examples/03_LibUI/libui_demo.jeti`; button-click fn-ptr cast moved from `LibUI_helper.c` into `LibUI.jeti`. `include/LibUI.{nh,np}` + `LibUI_mac.c` now live in `examples/03_LibUI/include/`; `run_libui.sh` compiles only `runtime.c` + `LibUI_mac.c` as C helpers.

### Inline Asm + Real `.s` Linking — Implemented ✅ (Aug 2026)
- ✅ **`asm` / `__asm__` / `__asm__` keyword** (lexer `KeywordKind::Asm`; also `__volatile`/`__volatile__`).
- ✅ **Extended asm** (full GCC/Clang layout): `asm [volatile] [goto] (template : outputs : inputs : clobbers : labels)`.
  - Template: adjacent string literals concatenated; raw escapes preserved (`\n\t` survive into emitted C).
  - Operands: named `[name] "constraint"(expr)` and positional `"constraint"(expr)`; output/input/in-out `+r`.
  - Clobbers: `"cc"`, `"memory"`, register names.
  - `asm goto`: 4th section of labels; emitted as `__asm__ goto (... : ... : labels)`.
- ✅ **Bare top-level `__asm__("...")`** as a declaration (passes through to emitted C verbatim, e.g. `.section` directives).
- ✅ **C label statements** (`ident:` not `::`) now parse via source-slice lookahead — required for `asm goto` targets; emitted as `ident: ;`.
- ✅ **Codegen**: `emit_asm_syntax()` produces `__asm__ [__volatile__] [goto] ("template" : ... : ...)` used by both stmt and decl emitters (`crates/codegen/src/codegen.rs`).
- ✅ **Real `.s` assembly linking**: `jetic -asm <file.s>` / `-S <file.s>` (repeatable) passes the file to clang/ld; verified with `_asm_square`/`_asm_add3` in `tests/asm_link_test.s`. `test_all.py` auto-links a sibling `name.s` next to any `name.jeti`.
- ✅ **`const unsigned char *` codegen fix**: `ast_type_to_c_str` pointer types now recurse into the subtype (like `cst_type_to_c_str`) so base qualifiers (`unsigned`/`signed`/`long`/`const`) survive, e.g. fn-pointer param `const unsigned char *` was emitted as `const char *` (dropping `unsigned`). Fixed in `crates/codegen/src/codegen.rs`.
- ✅ **Extreme fusion test**: `tests/asm_fusion_test.jeti`+`.s` — 12-stage stress fusing inline asm + real ARM64 external asm (CRC32 `0xCBF43926`, `rotl32`, `clz32`, `bitrev32`) with namespaces, class inheritance + VTable dispatch, protocols (`id<IChecksum>`), VProperties + `@synthesize`, Blocks with `__block` capture, `@try/@catch/@finally`, struct + C cast, fn-pointer to asm symbol, `@selector`, `__weak`, ARC `@autoreleasepool`.
- ⚠️ **AArch64 `%w` gotcha**: inline asm operands holding 32-bit values must use the `%w` register modifier (e.g. `%w[s]`, `%w0`). Without it clang maps `"r"` to 64-bit `x` regs, so `ror x0,x0,#27` rotates 64 bits and corrupts `unsigned` vars (first `asm_inline_test.jeti`/`asm_fusion_test.jeti` had it wrong; clang warns "use constraint modifier w").
- ✅ **x86_64 / Rosetta cross-arch**: `jetic -arch <target>` forwards `-arch <target>` to clang. `asm_x64/asm_x86_fusion_test.jeti`+`asm_x86_ext.s` (`square`/`sum3`/`rotl32`/`clz32`, inline `imull`/`addl`) builds an x86_64 Mach-O and runs it through Rosetta on this arm64 M4 (calls `softwareupdate --install-rosetta` and uses native `/usr/bin/arch` — not uutils `arch`). x86_64 inline asm needs no `%w` modifier (32-bit regs are used directly).
- ✅ **Golden dirs**: `tests/golden/23_asm` (inline/references), `24_asm_fusion`, and the x86_64 cross-arch case lives under `asm_x64/` (not in `tests/**`, so it's excluded from the default arm64 suite).
- ✅ **Tests**: `tests/asm_inline_test.jeti` (extended/volatile/named/`+r`/asm goto), `tests/asm_link_test.jeti`+`.s`, golden `tests/golden/23_asm/asm_inline.jeti`+`.out`.
- ⚠️ Operands reuse `parse_expression()` — message-send operands parse but are not recommended (use plain C-ish expressions in asm operands).

### In Progress — for-in 语法落地（desugar 方案，本会话规划）(Sep 2026)
- **目标**：打通 `for (T x in coll)` 遍历 `NPArray`。现状：`in` 已是 lexer 硬关键词（`lexer.rs:69` `KeywordKind::In`）；CST/AST/Cg 的 `ForIn` 变体存在但 parser 无规则（stub 不可达，parser.rs ~2144）、codegen AST→Cg 臂是 no-op（codegen.rs ~2045）、emitter 是坏 stub（codegen.rs ~5445：用不存在的 `jeti_array_count` + C 数组 `typeof(c)[0]` 索引，但集合是 `NPArray*` 对象）；checker/ARC/trace 已有 ForIn 臂。
- **已定方案（关键决策）**：**parser 层直接 desugar**，不把 ForIn 传给下游——emit 阶段拿不到 vtable 元数据无法手写 `[coll count]` 消息派发（nil 守卫等），违背"desugar、不引入新 IR"铁律。先例：`@42`→`[NPNumber numberWithInt:]`、`arrayWithObjects:`→`@[...]`。desugar 形态：
  ```c
  { NPObject *__jeti_fi_coll = <coll>;            /* 只求值一次；[nil count]==0 nil 安全 */
    for (size_t __jeti_fi_i = 0; __jeti_fi_i < [__jeti_fi_coll count]; __jeti_fi_i++) {
        T x = [__jeti_fi_coll objectAtIndex: __jeti_fi_i];
        ...body...
    } }
  ```
  下游（checker/ARC/codegen）全部看到普通 `For`+`Decl`+`MsgSend`，**零改动**；ARC 元素借用语义（不 retain/不 release）天然正确。
- **配套清理**：删除不再被构造的 ForIn 变体及各阶段手臂（`#![deny(dead_code)]` 会报 never constructed）——`CstStmtData::ForIn`/`AstStmtData::ForIn`/`CgStmtData::ForIn` + elaborator/checker/arc/trace/codegen 各处 match 臂 + `trace_loop` 的 for-in 标记。
- **Binary op 编码表**（codegen `op_to_str`，codeGen.rs:738）：非赋值 4=+ 8=< 9=> 10=<= 11=>= 12=== 13=!= 17=&&；赋值 0==。
- **测试计划**：`tests/for_in_test.jeti` 已建（遍历 NPArray/嵌套/空数组）；`cargo build`（debug）→ `jetic run` 端到端 → `test_all.py` 回归。
- **语法形态**：`for (T x in coll)`（ObjC 规范形，`id` 可用；var 是元素别名不拥有，不做 typed bare-name 形式）。

### for-in 语法落地 ✅ (Sep 2026, 本会话)
- ✅ **`for (T x in coll)` 遍历 NPArray 可用**。采用 **parser 层 desugar**（先例：`@42`→`[NPNumber numberWithInt:]`、`arrayWithObjects:`→`@[...]`），不把 ForIn 传给下游——emit 阶段拿不到 vtable 元数据，无法手写 `[coll count]` 消息派发（nil 守卫等），违背"desugar、不引入新 IR"铁律。
- ✅ **desugar 形态**（parser.rs For 分支内，见 `scan_for_in_header` + For-in desugar 块）：
  ```c
  { T *__jeti_fi = <coll>;            /* 只求值一次；借用别名，ARC 零注入 */
    for (size_t __jeti_fi_i = 0; __jeti_fi_i < [__jeti_fi count]; __jeti_fi_i++) {
        T x = [__jeti_fi objectAtIndex:__jeti_fi_i];
        ...body...
    } }
  ```
  下游（checker/ARC/codegen）只看到普通 `For`+`Decl`+`MsgSend`——nil 安全（`[nil count]==0`）、元素借用语义（不 retain/release）天然正确，**零下游改动**。
- ✅ **检测**：`scan_for_in_header()`——source-slice token 扫描（深度计数 + 字符串/char 字面量跳过），depth-0 `in` 在匹配 `)` 前且无 depth-0 `;` 即 for-in 头。不能用投机 parse+回溯：`parse_declaration` 会在 `in` 处 `consume(';')` 记 error 污染状态。
- ✅ **清理**：删除死 stub（parser For-in 占位块、`CstStmtData::ForIn`/`AstStmtData::ForIn`/`CgStmtData::ForIn` 未删——变体仍被 elaborator/checker/arc/trace/codegen 手臂引用，`deny(dead_code)` 因仍有构造路径？实际：ForIn 变体现在**无人构造**，但因 enum 变体不构造不触发 dead_code lint，手臂仍在，留待后续清理或保留兼容）。
- ✅ **测试**：`tests/for_in_test.jeti`（NPArray 遍历/嵌套 for-in/空数组 3 场景）端到端通过；test_all **212/223**（基线 211/222 +1，3 既有失败不变）、cargo test 55/55。
- ⚠️ **ForIn 变体残留**：CST/AST/Cg 的 `ForIn` 变体与 elaborator/checker/arc/trace/codegen 的 match 臂仍在（现不可达）。enum 变体不构造不报 dead_code，故编译安静；后续可整体删除（连带 trace 的 "for-in" loop 标记）。

### Checker 协议一致性检查 — Implemented ✅ (Sep 2026, 本会话)
- ✅ **P0 缺口补齐**：此前 checker 完全不做协议一致性检查（`protocol` 在 checker 内零匹配），类的 `<Proto>` 声明形同虚设。现在缺失必需方法会报错：`class 'X' does not implement required method 'sel' from protocol 'P'`（`file:line:col: error:` 格式）。
- ✅ **AST 带协议列表**：`AstDeclData::Class` 新增 `protocols: Vec<String>` 字段（此前 elaborator 用 `..` 把 CST 的 `protocols` 丢弃了）。elaborator 构造点填 `protocols.clone()`；其余 match 点全用 `..`，不受影响。
- ✅ **检查逻辑**（`checker.rs` `check_protocol_conformance`，在 `check()` 全部 decl 检查后跑，仅无既有错误时执行）：
  - 从符号表快照 `fqn → (required_methods, parents)`（clone 后再报错，避免借用冲突）
  - `class_methods` 只收**带 body** 的方法——binder 的 `propagate_protocol_methods` 会把协议方法**声明**（无 body）克隆进 `@interface` 以稳定跨 TU vtable 布局，那些不能算"已实现"
  - 按类名聚合：协议通常挂在 `@interface` 而非 `@implementation` 上，故用 `has_impl`（该类在 TU 内有 impl）判定，而非当前 decl 的 `is_implementation`
  - 协议名解析：先查原名，再查 `ns::proto` 形式（命名空间内协议）
  - 递归走父协议（`parents`，带 `seen` 去重防环）；`@optional` 方法天然豁免（不进 `required_methods`）
  - 跨 TU 安全：无 `@implementation` 的类（header-only，实现链接自别处）整体跳过
- ✅ **parser 修复（关键前置）**：`@interface X <Proto>` 的 `<Proto>` 此前被**泛型 type-params 块**吃掉（L3627 `match_token(Less)` 先执行），协议列表永远是空的。修法：`<...>` 块内名字若**全部**是已声明类型（`@protocol` 会 `add_type_name`）→ 归 protocols，否则归 type_params（`Box<T>` 不受影响）。`@interface X : Super <Proto>` 的第二个 `<...>` 块（superclass 之后）与前者合并到同一列表。
- ✅ **selector 归一化**：binder 存多段 selector 带尾冒号（`deployShield:`），AST 方法名可能不带，比较时 `trim_end_matches(':')`。不归一化会让 `ultimate_megafusion_test.jeti` 等 15 个测试误报。
- ⚠️ **负例测试目录**：`tests/negative/` 存放"checker 必须拒绝"的故意失败用例（`proto_missing_method.jeti`、`protocol_fail.jeti`——后者原在 `golden/07_protocols/`，是历史遗留的故意失败样本，无 `.out` 快照）。已在 `test_all.py` 的 glob 排除（`"negative" not in p.parts`），否则会被当普通 FAIL。
- ✅ **测试**：`tests/proto_conformance_test.jeti`（必需方法全实现 + `@optional` + 父协议继承链 `Colored <Drawable>`）通过；负例正确报错。回归：test_all **212/223**、cargo test **55/55**、trace golden 7/8（`arc_inject` 既有失败）。

### Golden 目录：两个新特性各自独立 (Sep 2026, 本会话)
- ✅ `tests/golden/30_for_in/` — for-in 遍历语法
  - `for_in_test.jeti` + `.out`（单层遍历 / 嵌套 for-in / 空数组 3 场景）
  - `README.md` — desugar 方案与理由、检测方式、已知残留
- ✅ `tests/golden/31_proto_conformance/` — checker 协议一致性检查
  - `proto_conformance_test.jeti` + `.out`（父协议继承链 + `@optional` 豁免）
  - `README.md` — 报错格式、实现要点、配套 parser 修复
- ✅ 两个 `.out` 已验证**确定性**（重跑一致，且 ARC/MRC 输出相同——两特性都与内存管理模式无关）。
- ⚠️ **故意失败用例统一在 `tests/negative/`**（不在 golden 目录）：`proto_missing_method.jeti`（`Circle <Drawable>` 缺 `draw`）+ `protocol_fail.jeti`（原在 `golden/07_protocols/`，历史遗留的无快照失败样本）。已在 `test_all.py` glob 排除（`"negative" not in p.parts`），验证方式是"编译必须失败且报错清晰"。

### 语言特性路线图（Sep 2026, 本会话定案）
用户已拍板。**换会话后直接按下面"待做"四节的顺序执行，勿再重新论证已否决项。**

#### 通用规则：新关键字的 `@` 前缀判据
> **`@` 标记的是"C 的语法里根本没有这个槽位、且需要编译器/运行时机制"的构造。**

- **词表核对**（`crates/lexer/src/lexer.rs` KW_TABLE 实证）：27 个 `@` 关键字 **100%** 落在三组、无一例外——①命名声明（`@interface`/`@implementation`/`@end`/`@protocol`/`@class`/`@property`/`@synthesize`/`@dynamic`/`@optional`/`@required`/`@defs`/`@namespace`/`@endnamespace`/`@using`）②运行时控制块（`@try`/`@catch`/`@finally`/`@throw`/`@synchronized`/`@autoreleasepool`/`@noarc`）③访问控制（`@public`/`@package`/`@protected`/`@private`）。**C 里全部没有对应物。**
- **`@` 缺席处正是 C 有槽位处**：60 个裸的非 C 词——`nil`/`YES`/`NO`/`self`/`super`（值）、`id`/`Class`/`SEL`/`BOOL`/`instancetype`（**类型名**）、`in`（语句内连接词）、`copy`/`retain`/`nonatomic`（`@property(...)` 括号内）、`__block`/`__weak`（`__` 前缀）。
- **决定性反例：ObjC 自己的 `in` 就是裸的。** `for (id x in collection)` 是 ObjC 2.0 新增的**语句级**特性，前缀是——没有。
- **机制代价表**（判据的量化形式："不用这个特性要手写多少样板"）：
  | 特性 | 手写代价 | 判决 |
  |------|----------|------|
  | `@try/@catch` | 巨大：setjmp/longjmp + 错误 TLS 槽 + 清理顺序 | `@` 合理 |
  | `@synchronized` | 巨大：mutex 创建/加解锁，裸机上根本没有 | `@` 合理 |
  | `@autoreleasepool` | 巨大：pool 栈 push/pop | `@` 合理 |
  | 结构体值比较 | 手写 15 行逐字段循环 | 复用 `==`（C 有槽位） |
  | 协议组合 `P & Q` | 手写两遍断言 | 复用 `&`（C 有槽位） |
  | `@await` | 巨大：task 结构 + 状态机 + 调度器 | **`@`（2026-09-27 定案）**：前缀运算符位是**开放上下文**，裸 `await` 与标识符歧义（`await * 2`、调用名为 `await` 的 C 函数）；`@` 标记状态机机制且让 `await` 保持 100% 合法 C 标识符，先例是表达式位的 `@selector(...)`/`@42` 装箱 |

- **三个前缀家族**（`@` 直觉部分来自"C 用不到前缀"，但 Jeti 实际已用三套约定）：
  | 前缀 | 用于 | 例 |
  |------|------|-----|
  | `@` | 声明 / 运行时块 / 访问控制 | `@interface` `@try` `@private` |
  | `__` | GNU C 风格扩展 | `__block` `__weak` `__typeof__` `__asm__` `__extension__` |
  | `#` | 预处理器指令 | `#import` `#include` `#define` |

  真要给编译器/运行时相关的东西加前缀，`__xxx` 比 `@xxx` 更贴既有约定（GCC 的 `__thread` 就是这么干的）。
- ⚠️ **反面论证存档**（若日后改用另一判据）：`match` 确实是"新语句形式"（ObjC 从没在语句位置用过裸的新关键字，`in` 只是既有 for 的连接词），而 `@try` 也是新语句形式——"新语句 → `@`"确有先例。`@match` 是可辩护的选择，但已被本轮否决（理由见下），勿轻易捡回。

#### 已否决（勿捡回）
| 被否决项 | 理由 |
|----------|------|
| **`if let`** | **零新能力，纯糖**。ObjC 自己就是这个模式（`id o = [d objectForKey:@"k"]; if (o) { ... }`），Swift 只是给它起了名字。Jeti 的 `id` 就是指针、`nil` 就是 0。相比 `if (x)` 只多两件事且都已被覆盖：(a) 受作用域约束的绑定 → `for (T x in coll)` 已提供该形式；(b) 类型收窄 → Jeti 无 optional 类型，checker 对消息发送本就宽松返回 `id`。且它是最破坏 ObjC 一致性的一项（Swift 味），收益为零。 |
| **`match`（任何形式，含 `=>` 箭头语法）** | ①**语法必然非 ObjC**：ObjC 对多路分发**没有语法 construct**，答案就是 `[obj isKindOfClass:[X class]]` + 强转的方法链。`match` 无论怎么设计都无 ObjC 先例——像 C `switch` 就不像（switch 是语言 construct，`isKindOfClass:` 是方法），像 `=>` 就是 Rust（全世界只有 Rust 这么写 match arm）。②**新能力其实只剩 struct 解构**（`Point { x, y }` 绑两字段），而 Jeti 的 struct 就是 C struct，写 `p.x` 本来就一行。③**能力大部分本就该由 `isKindOfClass:` 补**（见待做 #1），为一个省一行的糖造新语句家族 + 新词 + 降级机制 + 穷尽性分析，代价过高。 |
| **`async` 修饰符**（`async - (int)fetch:`） | 抄了 Swift 的词却用了 Swift 根本不用 的位置——Swift 是 `func fetch() async -> Int`（async 在签名内），C++20 **根本没有 async 修饰符**（靠函数体里有 `co_await` 判定协程）。顶在 `-` 前面哪儿都不像。删除，改用函数体内的 `await` 自证。 |
| **`if (v := expr)` 简写** | 与被否决的 `if let` 同类（Swift 味 + 零新能力），且进一步背离 C 语法。 |
| **struct 指定初始化器 `.x = 7` 归类为"直接透传"** | 实测不成立（见下方勘误 2）。**2026-09-29 更新**：声明处指定初始化器已支持（6 形态中 5 通，含部分/混合/数组元素/嵌套路径），仅复合字面量组合（形态 4）残留——详见勘误 2 的支持矩阵。 |

#### 待做（严格按此顺序）—— ✅ 2026-09-27 全部落地（#4 至里程碑 1，见下方各节记录）

##### 1. Foundation 补齐 ObjC 类型分发三件套 —— 最高优先 ✅ 已实现（golden/32）
**实测发现（`grep` 全部 0 命中）**：Jeti 的 Foundation 缺 ObjC 用于多路分发的几乎全部词汇。

| ObjC 词汇 | Jeti 现状 |
|-----------|-----------|
| `isKindOfClass:` | ✗ 缺失（有个**非标准**的 `- (BOOL)isKindOf:(Class)cls;`，`NPObject.jth:14`——命名就不合 ObjC） |
| `respondsToSelector:` | ✗ 缺失 |
| `conformsToProtocol:` | ✗ 缺失 |
| `isEqual:` | ✗ 缺失 |

- **为什么最高优先**：这是 **100% ObjC 一致、零新语法**的一步，且它让 `match` 想解决的 90% 场景直接变得可写。正路是"先补齐 ObjC 现有词汇"，而不是"造一个新语句家族"。补完之后类型分发是纯方法链，且天然 nil 安全。
- **命名纪律**：新方法名一律用 **ObjC 官方拼写**（`isKindOfClass:` 而非 `isKindOf:`）。现有的 `isKindOf:` 属历史遗留非标准命名——可保留作兼容别名，但新代码/文档用官方名。（是否给 `isKindOf:` 加 deprecation 警告待定，勿擅自改签名。）
- **实现提示**：`isKindOfClass:` 走 isa 链比较（`isa` 是 Jeti 已有的直接字段访问，codegen 对 `[obj class]` 已有特判可参考）；`respondsToSelector:` 查 vtable 槽位（统一 VTable 下可按 selector 名映射下标）；`conformsToProtocol:` 查 checker 刚补的协议闭包（`checker.rs` `check_protocol_conformance` 已有遍历逻辑，但运行时需要 metadata 支持——见待定项）。
- ⚠️ **待定**：`conformsToProtocol:` 的实现需要类元数据携带 protocol 列表（现在 `CgClassMeta` 有 `method_names` 等，是否已有 protocols 需先核实）；若没有则要先给元数据加字段（涉跨 TU 布局，需谨慎）。建议先做 `isKindOfClass:` + `respondsToSelector:` + `isEqual:` 三个不需要元数据的，`conformsToProtocol:` 单独评估。
- **测试**：新增 `tests/golden/32_foundation_dispatch/`。

##### 2. struct `==` / `!=` 值比较 —— 复用 C 运算符，无新语法 ✅ 已实现（golden/33）

**先读勘误**：这不是"修静默 bug"（实测是 clang 硬报错），是**补可用性缺口**。

⚠️ **实测结论（旧规划已推翻，勿再引用旧说法）**：`struct Point a,b; a == b` → 生成 C 原样输出 `a == b` → clang `error: invalid operands to binary expression ('struct Point' and 'struct Point')`。所以它**不是静默内存/逻辑错误**，"比 C 还危险""修现存静默 bug"的定级**不成立**。它是响亮的可用性缺口：要做值比较得手写 15 行逐字段循环。
⚠️ **指定初始化器支持矩阵（2026-09-29 实测，取代旧的"不支持"结论；形态 4 当日二次实测修正为 ✅）**：parser 已接指定初始化分支（`parse_designated_init` → `CstExprData::DesignatedInit`，elaborator/checker/codegen 全链透传），**6/6 形态全部可用**：

| 形态 | 例子 | 状态 |
|------|------|------|
| 1 完整指定 | `struct Point p1 = { .x = 1, .y = 2 };` | ✅ |
| 2 部分+零填充 | `struct Point p2 = { .y = 5 };` | ✅（未指定字段零初始化） |
| 3 位置+指定混合 | `struct Point p3 = { .x = 1, 7 };` | ✅ |
| 4 复合字面量+指定 | `NPRange r = (NPRange){ .location = 3, .length = 9 };` | ✅ **（当日二次实测修正）**——"cast 后跟 `{`"路径本就接通：`parse_init_list_or_dict` 已接 designator 分支。首轮探针判其为"未支持"是**误判**，真因是测试文件自身缺陷（引用未定义的 `NPRange` → `is_type_name` 为 false → 不走 cast 分支；另有 `CGPoint` 字段名写成 `x/y` 而非 `wx/wy`）。两处笔误已修，6 形态端到端 exit=0 |
| 5 数组元素指定 | `CGPoint pts[3] = { [0].wx = 1.0f, [2].wy = 6.0f };` | ✅ |
| 6 嵌套成员路径 | `struct Outer o = { .in.a = 3, .tag = 9 };` | ✅ |

位置式 `{1, 2}` 一直可用。测试：`tests/designated_init_test.jeti`（6 形态全过）。
⚠️ **教训（可复用的方法论）**：判定"某语法未实现"必须先跑**最小探针**，不能只看既有测试文件的失败信息。本轮"形态 4 未实现"与上一轮"类型参数被静默擦除"两个判定都被探针推翻/修正过；前者是测试文件自身笔误，后者是探针只量到现象（`NPArray_NPString` 零次出现）却没定位机制。

- **语法**（无新关键字，复用 C 运算符）：
  ```objc
  struct Point { int x; int y; };
  struct Point a = {1, 2};
  struct Point b = {1, 2};

  a == b          // 新：值比较（逐字段）
  a != b          // 新：值不等
  p == &a         // 既有：指针比较语义不变（两侧是指针就不走 struct 路径）
  ```
- **降级**：elaborator（或 parser，`AstExpr`/`CstExpr` 上标一个结构体比较标志）识别两侧**都是 struct 类型**时，降级为调用生成的比较函数：
  ```c
  static bool jeti_struct_eq_Point(struct Point a, struct Point b) {
      return a.x == b.x && a.y == b.y;      // 逐字段
  }
  ```
  - 无指针字段的 POD 可退化为 `memcmp(&a, &b, sizeof a) == 0`；含指针字段则逐字段（`==` 对指针比地址，是 C 的常规做法）。
  - 发射时机仿 `@synthesize` getter 的生成路径（`codegen.rs` 的 `EMITTED_METHODS` 附近），每个 struct 发射一个 eq 函数；**按需发射**避免 C 里 `static` 未使用告警。
- **checker 配套**：`check_conversion` / `check_sign_compare` 必须**跳过**结构体比较（它们只看标量，否则会误报）；`types_compatible` 对 `==` 两侧类型不一致时应报错（现状漏到 clang 才报，Jeti 应自己报）。
- **测试**：新增 `tests/golden/33_struct_eq/`。

##### 3. 协议组合 `P & Q` —— 复用 C 的 `&`，无新语法 ✅ 已实现（golden/31 扩展）
- **语法**：
  ```objc
  // ① 交集类型：接收者必须同时实现 P 和 Q
  void render(id<Drawable & Serializable> item);   // '&' token 已存在，lexer 不用动

  // ② 组合协议声明
  @protocol Renderable <Drawable & Serializable>
  @end
  ```
- **parser**：三处协议列表循环各加一个分支——`Identifier` 后跟 `Ampersand` 时收 `(Vec, "and", Vec)`。三处分别是：类型限定（`parse_type_full`）、`@interface`、`@protocol <parents>`。数据结构可给 `CstType` 加 `protocol_intersection: Option<(Vec<String>, Vec<String>)>`，或统一成 `ProtocolList` 枚举（`All([P,Q])` / `Intersection{left,right}`）。
  - 顺带：组合协议 `@protocol R <P & Q>` 在 binder 里把 P、Q 的 required 并入 R。
- **codegen 零改动**：统一 VTable 下 `[recv foo]` 的槽位索引全局唯一，与接收者写 `Shape *` 还是 `id<P & Q>` 无关——**协议类型只是给 checker 用的编译期约束标签**。这正是 Jeti 比 ObjC 简单的地方：ObjC 靠运行时 `objc_msgSend` 兜底，Jeti 里它就是个编译期断言。
- **checker**：`id<P & Q>` 接收者验证——取声明的静态类，沿 superclass 链 + protocols 闭包，验证 P 和 Q 的 required 方法集都被覆盖。复用 binder 已有的 `protocol_selectors`（递归父协议，已存在）。
  - **前置依赖**：待做 #1 的协议一致性检查（已完成，见上）已经把 `check_protocol_conformance` + `proto_table` 快照写好，本项在其上做增量即可。
- **多 TU**：P & Q **不改变 vtable 槽位集合**（槽位由方法名决定），无新增布局错配风险。
- **测试**：扩 `tests/golden/31_proto_conformance/`（加交集类型正例 + 负例放 `tests/negative/`）。

##### 4. `@await`（async/await）—— `@` 前缀表达式，**无修饰符** ✅ 里程碑 1+2 已实现（golden/34；M3 调度器/I/O 待做）
**已定形态**（删掉 `async - (int)fetch:` 修饰符；声明端与普通 ObjC 方法一字不差，只有函数体里的 `@await` 表明这是 async 方法，即 C++20 `co_await` 的判定风格）：
```objc
@interface Fetcher : NPObject
- (int)compute:(int)n;      // 声明端与普通 ObjC 方法一字不差
+ (void)runAll;              // 入口方法：async 链顶端，编译成 blocking wrapper
@end

@implementation Fetcher
- (int)compute:(int)n {
    int a = @await jeti_io_read();          // 挂起点
    return a * 2;
}

+ (void)runAll {
    int x = @await [self compute:21];        // 调用方也 await → 自己也是 async（链式传染）
    NPLog(@"done %d", x);
}
@end

int main() {                                // main 保持 int 返回值，await 永不直接出现在这里
    [Fetcher runAll];                       // 正常同步 ObjC 调用 → blocking wrapper 内部泵调度器
    return 0;
}
```
- **`await` 走上下文关键词**（`is_contextual_kw_ident()` 机制），不挂 `@`（理由见通用规则）。`int await = 1;` 必须能编过（C 超集铁律）。
- **ownership 正交**（已核实）：Jeti 的 ownership 本来就靠**方法名**推断，不依赖声明处标注（`crates/ownership/src/ownership.rs:28-38` 的 `init`/`alloc`/`new`/`copy`/`mutableCopy` 前缀）。故 `await [self alloc]` 的 +1/+0 判定完全不变，**async 不需要任何新的 ownership 规则或标注**。
- **vtable 布局无影响**（已核实）：槽位由**方法名**决定、派发处统一 cast，故把 async 方法发射成状态机函数（返回 status、收 `task*`）**不改变槽位集合**，不引入跨 TU 布局错配。

- ✅ **已定：同步入口 → 必须报错，不允许静默阻塞**
  `error: cannot call suspending method 'X' from non-async context`
  理由："静默泵调度器"是单线程协作式调度下最大的实际风险——程序卡住但不报错，极难定位。放行方式只有一个：**入口方法**（async 链顶端，如 `+ (void)runAll`）编译成显式 blocking wrapper，内部 `jeti_task_create` + 驱动调度器（`jeti_task_join`）直到完成；单线程下泵动即有进展，不会死锁。
  - async 链内部正常传染（方法 `@await` 另一个 → 自己也变 async），无需任何标注，编译器按"函数体是否含 `@await`"逐层判定。
  - ❗因此"进入 async 需靠 `jeti_task_start`/`jeti_run_all` 手动驱动"的说法**作废**：API 只在入口 wrapper 内部使用，调用方看不见。`main` 保持 `int main()` 不变。
  - 判据与 C++20 一致（`co_await` 只能出现在协程体内，从普通函数调协程要显式走 `get_return_object`/`resume`）；Jeti 简化为"入口方法自动 blocking"，因为静态派发下无需暴露 task 句柄。
- ⚠️ **落地前必须先定的 4 项**（否则 desugar 形态会返工）：
  1. **入口方法识别规则**：约定"非私有、返回 `void`、且被同步上下文调用的 async 方法"；更严格方案需显式标记（已被"删修饰符"否决），故倾向在错误信息里给出修复提示。
  2. **语句位置的 `await`**：`await f();` 算"求值后丢弃"还是解析成 `(await f)()`？
  3. **跨 await 的 `@throw` / `@noarc` / break-continue**：jmp_buf 不能跨挂起点，需检测并报错（同函数内正常）。
  4. **`-emit-bridge-header`**：它按声明签名生成 wrapper，而 async 方法的发射签名不同（返回 status、收 `task*`），需跳过或特判。
- ⚠️ **已知软肋（诚实记录）**：①**签名会撒谎**——头文件里看不出 `compute:` 会挂起（相对显式 `async` 标记的真实可读性损失）；②**传染性不可见**——谁在传染只有编译器知道；③**同一把尺子量 `await` 其实不完全干净**——ObjC 的异步答案是 **blocks + GCD**（`dispatch_async(queue, ^{...})`），Jeti 已有 blocks，所以"ObjC 一致"的话异步该长成 blocks 派发。`await` 仍值得做，因为 Jeti 的静态派发让状态机方案比 blocks 续体拆分干净得多——但这是取舍，不是"它更 ObjC"。真要极致一致，可做 `dispatch_async` + blocks（代价是每个挂起点手写续体）。
- **备选（若日后嫌"签名撒谎"碍眼，本轮不采用）**：用**类型**而非关键字标记——`instancetype`/`id`/`Class`/`SEL` 这些纯 ObjC 概念在 Jeti 里都是**裸类型名**（没挂 `@`），故 `NPAsync<int> r = ...` 同样不破坏约定，且在头文件里可见。**→ 2026-09-29 反转为采用**（用户拍板）：作为**声明侧标记** `NPAsync<T>` 落地（仅返回类型位，变量位禁用），设计见下方"`NPAsync<T>` 声明标记"节。
- **实现体量**：状态机 desugar（新 crate `crates/async`，位置与 `crates/arc` 同构——在 elaborator 之后独立 pass）+ 运行时 API（`include/jeti/runtime.h` 加约 100 行：`jeti_task_create` / `jeti_task_resume` / `jeti_task_join` / `jeti_run_all`）。
  - **局部变量提升**：所有活过 await 点的局部变量提升进 task 结构。（历史先例是 @try 的 TRY_LIFT shadow 机制——已因双重释放删除，见"@try 双重释放 UAF 修复"节；M3 提升进 task frame 的思路相同，但释放结算归 ARC/任务退出汇合点。）
  - **ARC 结算**：任务完成/取消时对提升到 task 结构里的对象局部逐个 `jeti_release`（在"任务退出"这个汇合点统一结算，比在每个 await 点插 release 更简单且正确）。
  - 生成的 C 读起来就是一个大 switch 状态机（与 C `switch` 同构，守住"人能读"原则），portable 后端要求 `clang -pedantic -Werror` 与 `gcc -pedantic -Werror` 双通过。
- **分阶段**：里程碑 1（无 I/O 的 async + 手动 create/resume，先证明 desugar + ARC 结算正确）→ 里程碑 2（`jeti_run_all` 轮询调度器 + 任务间 yield）→ 里程碑 3（线程池/事件循环/I/O，纯运行时的事）。
- **测试**：新增 `tests/golden/34_async/`，含负例（同步调 async 不报错应失败 → 放 `tests/negative/`）。

### `@defer` 作用域退出执行 —— 已实现 ✅ (Sep 2026, 本会话)

> 状态：**已实现**（落地记录见节尾；设计保留作存档依据，勿重新论证）。动机：资源清理现在全靠手记（`@try/@finally` 或逐出口 `fclose`），defer 是"编译器替你记出口"的机制化。

**语法**（`@` 判据自洽：C 里没有 defer 的语法槽位，不用它就得在**每一处出口**手写清理——判据表"巨大"档；块形式与 `@autoreleasepool` 同属②运行时控制块组）：

```objc
- (void)work {
    FILE *f = fopen("x.txt", "r");
    @defer { fclose(f); }          // 作用域退出时执行
    if (!ready) { return; }         // 提前 return 也会执行 defer
    @defer { printf("second\n"); }  // 多个 defer：LIFO
    ...
}                                    // 函数体出口：先 second 后 fclose
```

**语义（Go/Swift 惯例，ObjC 化命名）**：

| 规则 | 内容 |
|------|------|
| 触发点 | defer 所属**最内层复合语句块**的每一处退出：块尾自然出口、块内任意深度的 `return`、**跳出该块**的 `break`/`continue`、块内 `@throw` |
| 顺序 | 同一出口点，从**最内层** defer 所属块到最外层依次执行（即同块多 defer 为 LIFO） |
| 逐次执行 | 循环体块里的 defer **每轮迭代结束**执行（作用域 = 循环体块，块每轮退出一次） |
| 变量可见性 | defer 体按普通嵌套块语义直接读写外层局部变量——**无捕获、无拷贝**（C 栈变量天然可见，与 blocks 的捕获语义刻意不同） |

**desugar 形态（铁律：不引入新 IR，codegen 零改动）**——`@defer` 不对应任何新 CgStmt/CgDecl。新增独立 pass（新 crate `crates/defer`，位置与 `crates/arc` 同构；pipeline **Step 3.9**，即 eh desugar(3.85) 之后、ARC(Step 4) 之前），把 defer 体作为普通语句**复制插入**到所属块的每个出口点：

- 块尾自然出口：插在块内末尾（该块全部 defer 按 LIFO 展开）。
- 块内任意深度的 `return` 前：沿"该 return 所在块 → defer 所属块"的链逐层展开（内层 defer 先执行）。
- 跳出 defer 所属块的 `break`/`continue` 前：同上（不跳出该块的内层 break/continue **不触发**）。
- `@throw` 前：同上（语句在 longjmp 之前，同函数 throw 时 defer 会执行）。
- **实现要点**：插桩以"defer 所属块"为单位——块 A 的 defer 挂在 A 的每个出口上，与 return 写在哪层嵌套无关。

**与 ARC 的顺序保证（分工对齐）**：defer pass 先跑、ARC 后跑 → ARC 的 scope-end release 注入点在 defer 插入语句**之后**（两者挂在同一批位置：块尾/return/throw）→ 实际执行序 = 用户 defer（对象还活着）→ ARC release。**defer 本质是"用户可编程版"的 ARC scope-end 插桩，复用同一插桩点，语义天然对齐**——这也是它必须排在 ARC 之前的原因。

**与 `-eh checked` 的顺序**：eh desugar 把 `@throw` 改写成旗标+early return；defer 排在它**之后**（3.85 → 3.9），checked 模式的 throw 路径已变成普通 `return`，自动被 defer 插桩覆盖，零特判。

**checker 配套**：
- defer 体按普通 compound 做类型检查（复用既有路径，零新检查逻辑）。
- M1 禁止 defer 体内的 `return`/`break`/`continue` **逃出 defer 块**（报 error 并指向正确写法；Go 语义的 return-in-defer 留 M2）。
- defer 体自身的对象局部由 ARC 正常结算（插入的块就是普通块，ARC 看得见）。

**M1 限制（如实）**：
- 块形式 only（`@defer { ... }`）；单表达式糖 `@defer fclose(f);` 留 M2。
- defer 体内不允许 `return`/`break`/`continue`（M2 再议 Go 语义）。
- 跨函数 `@throw`（sjlj longjmp 掠过中间帧）会跳过中间帧的 defer——**与 ARC release 注入同一已知限制**（README 异常章节既有记载），非 defer 新增缺陷；`-eh checked` 下无此问题（checked 不跳帧）。

**实现触点清单**：lexer（`AtDefer` + KW_TABLE）；parser（语句位 `@defer` → `CstStmtKind::Defer(body)`；函数体/方法体/任意块内可写）；CST/AST（`CstStmtData::Defer(Box<CstStmt>)`，elaborator 直转；checker/trace/async/arc 的 match 臂补透传或处理）；`crates/defer` 新 crate（插桩 pass + 单测）；pipeline（Step 3.9 挂载）；**codegen 零改动**；golden `tests/golden/36_defer/`（LIFO 顺序 / 提前 return / 循环逐轮 / 同函数 throw / defer-then-ARC-release 顺序 / 嵌套 defer / 与 -eh checked 组合），负例 `tests/negative/defer_return.jeti`。

#### 落地记录 ✅ (Sep 2026, 本会话) — 全链打通，golden/36

五触点全部落地（**codegen 零改动**——defer 在 codegen 看到时已 splice 成普通语句）：

- **lexer**：`KeywordKind::AtDefer` + KW_TABLE 表项 + 三处关键字排除列表。
- **parser/elaborator**：语句位 `@defer { ... }`（仿 `@autoreleasepool`）→ `CstStmtData::Defer(Box)` → `AstStmtData::Defer(Box)` 直转。
- **新 crate `crates/defer`**（`jeti_defer::desugar_unit`，pipeline **Step 3.9**：eh desugar(3.85) 之后、ARC(4) 之前）：两套跟踪集——
  - `pending`（最内层优先）：全部已注册 defer，在 `return`/同函数 `@throw`（函数级出口）触发；
  - `jump`：`break`/`continue` 触发的子集——**仅"此处与最内层 loop/switch 之间"注册的 defer**。进入 loop/switch 体时 `jump` 清空，普通块累加（实测：循环内 break 不触发外层 defer，outer 仅打印一次）。
  - defer 体**克隆**插入各出口；块尾自然出口 LIFO 展开；defer 体内 block literal 以全新作用域重跑 desugar（独立函数）。
- **上下游适配（实测偏离设计清单，如实记录）**：checker/trace/async/arc 全部运行在 Step 3.9 **之后**、永远见不到 Defer 节点（catch-all 已覆盖，零改动）；真正需要适配的是 **binder**（先于 defer 运行，补绑定臂）与 **eh**（defer 体内 `@throw` 由 `validate_defer_body` M1 拒绝——eh desugar 在 defer 之前，checked 模式的 defer 体 throw 永不会被改写、会漏进裸 sjlj 混后端）。
- **实现期修掉的两个真 bug（探针实证后修，勿重蹈）**：
  1. **break 误触发外层 defer**：首版单一 `pending` + `escapes` 布尔——循环内 break 会把函数级外层 defer 也触发（outer 打印两次）。修法：`pending`/`jump` 双集拆分。
  2. **`@namespace` 内函数漏 splice**：`desugar_decl` 初版只递归 Function/Method/Class，namespace 函数的 defer 直达 codegen `_ => {}` 被**静默丢弃**（清理永不执行）。补 `Namespace(members)` 递归臂。（对照探针确认 `NS::fn(7)` 调用形态报 parse 错是既有 parser 限制，与 defer 无关。）
- **M1 限制（强制执行）**：defer 体内禁 `return`/`break`/`continue`/`@throw`（负例 `tests/negative/defer_return.jeti`，文案 `'return' inside an '@defer' body is not supported (M1)`）；`@defer` 必须直接位于块内（如 `if` 的唯一 body 会报错提示包 `{ }`）。
- **诊断**：defer errors 走 `translate_lines`（SourceMap）行号翻译（负例验证：报第 7 行即真实文件行）。
- **测试**：`tests/golden/36_defer/`（7 场景：LIFO+提前 return / 循环体 defer 每轮+continue/break 触发 / 外层 defer 仅触发一次 / 嵌套块 / 同函数 `@throw` / ARC 顺序（defer 先于 scope-end release，`tracker dealloc` 最后打印）/ block literal 独立作用域）——`.out` 验证确定性（重跑一致 + ARC/MRC 一致，MRC 下仅 dealloc 行消失属设计）；`-eh checked` 下输出逐字节一致。README 记语义与实现要点。
- **回归**：test_all **259/267**（基线 257/265 + golden/36 + golden/37 各新增 1 项，2 个 `-F` 既有失败不变）、cargo test 102/102。

### `NPAsync<T>` 声明标记 —— 已实现 ✅ (Sep 2026, 本会话)

> 状态：**已实现**（落地记录见节尾）。动机：`@await` M1/M2 的"签名会撒谎"软肋——头文件里看不出方法会挂起；`NPAsync<T>` 把 async-ness 扶正为**头文件可见的标记**（用户原话："标记防止忘记"）。这是把路线图否决过的"备选"反转为采用：否决理由（备选不如 `@await` 灵活）依然成立，但两者**不互斥**——`@await` 是语言机制，`NPAsync<T>` 是它的声明侧标签。

**语法**（标记挂在**返回类型位**，`<...>` 复用既有泛型 type_args 解析——`NPAsync` 不是真泛型类，是保留标记名）：

```objc
@interface Fetcher : NPObject
- (NPAsync<int>)compute:(int)n;     // 标记：会挂起，完成后给 int
+ (NPAsync<void>)runAll;            // void 也标（入口方法）
- (int)synchronousWork;              // 不标 = 承诺不挂起
@end
```

**语义**：

| 规则 | 内容 |
|------|------|
| 标记即解包 | parser 在方法/函数**返回类型位**遇到 `NPAsync<T>`（恰好 1 个 type_arg）：解包为 `T` + 方法置 `async_marker` 标志。AST/CST 记 flag，**下游（vtable/driver/return 类型检查）只见 `T`** |
| 纯编译期 | **codegen/runtime/ARC 零改动**：生成 C 签名就是 `T`（M2 driver 本就返回 `T`）。跨 TU：`.jth` 与 `.jeti` 两侧都擦成 `T`，vtable/链接零影响 |
| 非值类型 | `NPAsync<T>` 出现在**变量/参数/ivar 位** → checker error：`'NPAsync<T>' is a declaration marker, not a value type — '@await' the call instead`（调用方看到的返回类型就是 `T`，标记不参与调用点类型） |
| 保留名 | 用户声明名为 `NPAsync` 的类 → error（`'NPAsync' is reserved for the async marker`） |
| interface/impl 一致 | 标记是签名的一部分：`@interface` 标了而 `@implementation` 没标（或反之）→ error（对齐既有"签名匹配"检查哲学） |

**一致性对账（挂 `jeti_async::check_unit`——它独占"体内是否含 @await"的分析，且跑在 desugar 之前能见到原始 AST；checker 在 Step 4.8 desugar 之后已看不到 @await）**：

| 声明 | 体内 | 判定 |
|------|------|------|
| `NPAsync<T>` | 有 `@await` | ✅ |
| `NPAsync<T>` | 无 `@await` | error：`'X' is marked 'NPAsync<T>' but its body never suspends` — 去标记或补 `@await`（防标记撒谎，与裸 `@throws` 的"必须真抛"同一哲学） |
| 裸 `T` | 有 `@await` | **warning**：`'X' contains '@await' but its return type is not marked 'NPAsync<T>' — mark it so callers can see it suspends`（**"防止忘记"的本体**；`-Werror` 可升级拦截） |
| 裸 `T` | 无 `@await` | ✅ 现状 |

- `check_unit` 需新增 **warnings 出口**（现在只产 errors），pipeline 照 checker warnings 的紫色样式打印。
- **入口规则不变**：`+ (NPAsync<void>)` 是否 blocking wrapper 仍由 M1 既有判据（void + 体内 await + 被同步上下文调用）决定，标记不参与派发决策——标记只对**人**和**对账**说话，不改变任何运行时行为。

**实现触点清单**：parser（`is_type_name`/内置类型名表加保留名 `NPAsync`——照 `size_t` 系先例，否则返回类型位解析不认；方法/函数返回类型位识别 `NPAsync<单type_arg>` → 解包 + 置 flag，零/多 type_arg 报 error；interface 与 implementation 两处方法声明 + 顶层函数声明共用一个 helper）；CST/AST（`CstDeclData::Method`/`Function` 加 `async_marker: bool`——照 `has_variadic`/`throws` 先例，elaborator 透传，`crates/async` 的 `entry_decl` 等既有构造点补 `false`，cargo test 构造点同步）；`jeti_async::check_unit`（对账表 4 行 + interface/impl 匹配 + warnings 出口）；checker（变量/参数位拒绝 NPAsync + 保留名类声明检查）；**codegen/runtime/ARC 零改动**；无新 CLI flag。

**M1 限制（如实）**：
- 标记只活在 jeti 源头：生成 C、桥接头不出现 NPAsync（擦除设计）。桥接头"非 void async 跳过"的现状不变——未来可用标记静态可知签名发 wrapper，列 M3 候选。
- 协议 required 方法的标记一致性不做（`propagate_protocol_methods` 克隆会带上 flag，但对账只覆盖 class 的 interface/impl，协议侧留待需要时）。
- `NPAsync<NPAsync<int>>` 不禁止（解包一层得 `NPAsync<int>`，落到变量位仍会被拒绝；语义怪但无害）。

**测试计划**：golden `tests/golden/37_async_marker/`（标记+await 正常跑通 / `NPAsync<void>` 入口 / 体内 `return` 按 `T` 检查 / 跨 TU `.jth` 标记 + `.jeti` 实现两侧一致）；负例 `tests/negative/`：`async_marker_no_await.jeti`（标记无 await）、`async_marker_var.jeti`（变量位用 NPAsync）、`async_marker_mismatch.jeti`（interface/impl 不一致）；warning 用例（await 无标记默认编译过、`-Werror` 拦截）端到端探针验证。

#### 落地记录 ✅ (Sep 2026, 本会话) — 全链打通，golden/37

- **parser**：保留名 `NPAsync` 双注册——内置类型名表（照 `size_t` 系先例）+ **`generic_class_names`**（`<...>` 直接路由 type_args 路径，免走协议试探回滚）。`unwrap_async_marker` helper：恰好 1 个 type_arg → 解包为 `T` + flag；零/多 type_arg 报 error（`'NPAsync' marker requires exactly one type argument`）。挂 `parse_function_decl_or_definition` 与 `parse_method` 两个返回类型位——interface/impl/protocol/顶层函数全走这两处，零第三挂载点。
- **CST/AST**：`CstDeclData::{Function,Method}` + `AstDeclData::{Function,Method}` 加 `async_marker: bool`（照 `throws` 先例）；elaborator 透传；4 处既有构造点补 `false`（trace 1 + checker 2 + async `entry_decl` 1）。
- **`jeti_async::check_unit`**：对账表 4 行（`reconcile_marker`：标记+await ✅ / 标记无 await error / await 无标记 warning / 都无 ✅）+ interface/impl 标记一致性（`iface_markers` HashMap，键 `cls_sym::sel`，双向都报）。⚠️ **只对有 body 的实现对账**——初版用 `method_is_async`（对无 body 方法恒 false）会把每个带标记的接口方法误报"标记撒谎"；改为 header-only interface 豁免（跨 TU 安全，与协议一致性检查同规则）。
- **pipeline**：`AsyncDiagnostics` 加 `warnings` 出口；紫色 `[async] warning:` 打印 + `-Werror` 升级（`Async check failed (-Werror):`）。
- **checker（设计触点清单内）**：`reject_npasync_type` helper 拒绝全部值位——变量（含逗号链后续声明符）/参数（`add_params_to_scope`；CstParam 无位置信息，报声明行）/ivar/property；保留类名 `NPAsync` 报 error（interface+impl 各报一次，可接受）。文案统一 `'NPAsync<T>' is a declaration marker, not a value type (<ctx>) — '@await' the async call instead`。
- **诊断统一出口（连带修复）**：async/defer/eh 三处诊断全部接 `translate_lines`（SourceMap）——探针暴露 async warning 打内联缓冲区行号 `911:1`（文件仅 15 行）；修后报真实文件行。连带修掉 filename 双前缀（`m3.jeti: m3.jeti:9` → `m3.jeti:9`）。
- **擦除实证**：生成 C 中 `NPAsync` 出现 **0 次**（`grep -c` 实证），签名 `int F_compute_(NPObject *, SEL, int)` 干净——vtable/跨 TU/桥接头零影响，codegen/runtime/ARC 零改动达成。
- **测试**：golden/37 正例（`NPAsync<int>` compute + `NPAsync<void>` 类方法入口 + 跨链 `@await` + 未标记 `plain:`；`.out` 重跑一致 + ARC/MRC 一致）；负例 4 个文件全部实测 exit=1（`async_marker_mismatch` 双向 / `async_marker_no_await` / `async_marker_value_pos` / `async_marker_reserved`）；warning 四态探针全过（含 `-Werror` 升级 exit=1）。
- **回归**：test_all **259/267**（基线 257/265 + golden/36 + golden/37 各新增 1 项，2 个 `-F` 既有失败不变）、cargo test **102/102**。

### `@throws` 声明标注（已实现 ✅ Sep 2026, 本会话）+ `@throw` / `@throws` 用法区分 (Sep 2026)

> **为什么单独立一节**：`@throw` 与 `@throws` 一字之差、语义完全不同——这是使用者（包括项目作者本人）最容易搞混的一对词。本节把区别钉死，实现与用户文档一律以此为准。

#### 一句话区分

- **`@throw`（已实现）**：**语句**——函数/方法体内**运行时抛出异常**的动作。`@throw @"boom";`
- **`@throws`（已实现）**：**声明标注**——挂在方法/函数声明的 `;` 前，**编译期**声明"本函数会抛"及异常类型，不执行任何动作。

| | `@throw` | `@throws` |
|--|----------|-----------|
| 词性 | 语句（动作） | 声明标注（元数据） |
| 位置 | 体内，语句位 | 声明尾部，`;` 前（尾置） |
| 时机 | 运行时 | 编译期（checker 对账） |
| 形态 | `@throw expr;`（带表达式） | `@throws(T *)` 或裸 `@throws`（不带运行时值） |
| ObjC 先例 | 有（`@throw` 关键字） | 无关键字（苹果用尾置宏占位，见下） |
| Java 同构 | `throw`（语句） | `throws`（声明子句）——**一词之差同款约定** |

#### 正确用法

```objc
@interface Repo : NPObject
- (NSData *)fetch:(NPURL *)url @throws(NPError *);  // 声明：会抛 NPError*
- (int)parse:(const char *)s @throws;               // 声明：会抛，类型不注明
- (int)count;                                       // 声明：不抛
@end

- (NSData *)fetch:(NPURL *)url {
    if (!url) {
        @throw [[NPError alloc] initWithCode:404];   // 语句：抛出
    }
    ...
}
```

#### 用错的两种方向（实现后 checker 必须报清楚）

| 写错 | 应报 |
|------|------|
| 在体内写 `@throws @"x";` | `'@throws' is a declaration annotation, not a statement; use '@throw' inside a method body` |
| 在声明的 `;` 前写 `@throw(...)` | `'@throw' is a statement, not a declaration annotation; use '@throws(...)' on the declaration` |

#### 苹果头文件出处（尾置元数据的正统性依据）

实测摘自本机 SDK（`xcrun --show-sdk-path`，Foundation 头文件）——苹果自己就在**声明的 `;` 前这个槽位**挂元数据，裸/带括号两形态都有十几年先例：

| jeti 形态 | Apple 同位先例（SDK 原文） |
|-----------|---------------------------|
| 裸 `@throws` | `NS_DESIGNATED_INITIALIZER`（NSXMLDTDNode.h:81）、`NS_REQUIRES_NIL_TERMINATION`（NSArray.h:760） |
| `@throws(NPError *)` | `API_AVAILABLE(macos(10.9), ios(3.0))`（NSPredicate.h:50/58）——尾置 + 括号参数 |

差别只在强制力：苹果的宏展开成 `__attribute__((sentinel))`/availability 交给 clang；jeti 把同一槽位扶正为一等语法、checker 直接对账——正统性继承自 ObjC 生态惯例，强制力是其升级。

#### checker 行为契约（实现时照此）

| 声明形态 | checker 对账规则 |
|----------|------------------|
| `@throws(NPError *)` | 体内每个 `@throw` 的静态类型必须与声明一致（含子类），不符报 error |
| 裸 `@throws` | 体内必须确有 `@throw`（别报假料），不查类型 |
| 不写 | 体内不得有直接 `@throw`（报 error） |

裸形态定位：**渐进采用**——先标事实、日后补类型收紧（对应 C++ `noexcept` 只标事实能活下来的教训）。

#### 实现注记（勿返工）

- 纯 checker 侧特性，**无 desugar、无运行时**：标注信息挂在 CST/AST 的方法/函数声明节点上，checker 读它对账，codegen 不发射任何东西。
- 解析点：`parse_class_interface` / `parse_class_implementation` 方法声明的 `;` 消费处 + 顶层函数声明同位置；lexer 加 `@throws` 关键字（`@` 判据自洽：声明位已有 `@property` 等先例，属①命名声明组）。
- 跨 TU：`@throws` 不影响 vtable 布局（槽位由方法名决定），无多 TU 错配风险。
- README/CHINESE 等用户文档**等实现落地后**再写（文档先行会让使用者对着没实现的语法踩空）；本节是唯一权威出处。

#### 落地记录 ✅ (Sep 2026, 本会话)

五触点全部打通（lexer / parser / CST+AST / elaborator / checker）——**无 desugar、无 codegen 改动、无运行时**：

- **lexer**：`KeywordKind::AtThrows` + KW_TABLE `@throws` 表项。
- **parser**：`Function` / `Method` 声明尾部（`;` 或 `{` 之前）消费 `@throws` / `@throws(T)`（`parser.rs:2755-2776`，方法同款块）；语句位 `@throws`、声明位 `@throw(...)` 双向报错并指向正确关键词；三处关键字排除列表补 `AtThrows`。
- **CST/AST**：`CstDeclData::{Function,Method}.throws: Option<Box<CstType>>` → `AstDeclData::{Function,Method}.throws: Option<Box<AstType>>`，elaborator 透传；`crates/async` 的 `entry_decl` 补 `None`。
- **裸 vs 未标注编码**：`None` = 未标注；`Some(Void 非指针)` = 裸 `@throws`；`Some(T)` = 带类型。checker 侧 `throws_state()` 归一为 `Option<Option<AstType>>`（`Some(None)` 即裸形态）。
- **checker**（`reconcile_throws()`，声明体检查完毕后调用）：
  - `@throws(T *)`：逃逸的 `@throw` 静态类型须与 `T` 相容（子类可）→ 否则 `'@throw' of type 'X' does not match the declared '@throws(Y)'`
  - 裸 `@throws`：体内必须确有 `@throw` → 否则 `'X' is marked '@throws' but its body never executes '@throw'`（`-eh checked` 下 throw 已被重写，此判定跳过——`eh_checked` 守卫）
  - 不写：逃逸的 `@throw` → `'@throw' escapes 'X' without a '@throws' annotation; declare it with '@throws(<type>)' or handle it with a local '@try'`；**被本体内 `@try` 捕获的不算逃逸**（`try_depth` 只对**带 `@catch`** 的 `@try` 计数）
  - 类型判定保守（`throw_expr_type`）：只判 `@"..."`(id)、裸 C 串(char*)、Cast 目标类型、已知类型变量；**MsgSend 一律不判**（selector-only 注册表无从得知类）→ 对任意声明类型放行
- **既有 6 处过期声明补标注**（新规则命中，全是"helper 抛、调用方 catch"的 ObjC 惯用法；标注纯编译期，`.out` 逐字节不变）：
  - `tests/TOMLEditor/toml_editor.jeti`：`static void THROW(const char *msg) @throws(ParseError *)`
  - `examples/01_JSONEditor/json_editor.jeti`：`static void THROW(const char *msg) @throws(Error *)`
  - `tests/golden/15_exceptions/try_catch.jeti`：`divide` → `@throws(MathError *)`
  - `tests/golden/24_asm_fusion/asm_fusion.jeti`：`- (void)corruptAndThrow @throws`（interface + impl 两处）
  - `tests/try_catch_basic_test.jeti`：`- (void)throwIfNegative:(int)val @throws`（interface + impl）
  - `tests/try_catch_comprehensive_test.jeti`：`+ (int)divide:(int)a by:(int)b @throws`（interface + impl）
- **探针实测**（临时文件，不入库）：带类型自由函数 + 裸标注方法 + 带类型方法 = exit 0；本地 `@try` 包裹的未标注 `@throw` = exit 0（豁免生效）；逃逸未标注 / 声明类型不符 / 裸标注撒谎 = exit 1 且文案精确。
- **回归**：`cargo test --workspace` **59/59**（顺带修了 3 处测试构造缺 `throws` 字段——`cargo build` 不编测试代码，故此前被掩盖）；`python3 test_all.py -j8` 规则命中归零、仅剩 2 个既有 `-F` 失败。
- **用户文档**：README.md `### \`@throws\` — Declared Exceptions` + CHINESE.md `### \`@throws\` —— 声明式异常`（本节自此作为设计依据存档；面向使用者的说法以那两节为准）。

### 四大特性落地记录 ✅ (Sep 2026, 本会话) — 路线图待做 #1/#2/#3 + #4-M1

> 回归基线：cargo 55/55，test_all **212/227**（3 既有失败 `-F` 不变）。用户拍板的设计决策在各节内标注。

#### #1 Foundation 分发三件套 ✅（golden/32）
- **`isKindOfClass:`**：`jeti_root` 声明 + runtime `jeti_isKindOfClass`（isa 链复用 `jeti_isKindOf`）。旧 `isKindOf:` 保留作兼容别名。
- **`respondsToSelector:`**：**编译器伪方法**（不声明不占槽）——codegen MsgSend 特判 → `jeti_resp_<member>(recv)`：`__o && ((struct jeti_vtable *)__o->isa->vtable)->member != 0`。NULL 槽位（协议存根/声明未实现）→ NO；**未知 selector → 常量 0**（不是本 TU vtable 成员，无从引用）；nil 接收者短路；helper 按需发射。⚠️ 坑：`convert_expr` 跑在 `METHOD_METADATA.set` 之前，selector 存在性必须查 `class_infos`。
- **`isEqual:`**：默认指针相等；子类可重写（`NPString` 用既有 `isEqualToString:`；字面量 interning 落地后同内容字面量是同一对象，值语义仍是键查找的语义保证）。
- **`conformsToProtocol:` 推迟**：`NPClass.protocols/protocol_count` 字段在但 codegen 恒写 0；填充需把协议表导进 AST/CgClassMeta（`ast_to_cg_unit` 无 symtab），涉跨 TU 布局。

#### #2 struct `==`/`!=` 值比较 ✅（golden/33）
- checker `maybe_rewrite_struct_eq`：Binary(op 12/13) 两侧同 tag 值 struct（`is_struct && !is_pointer`，类型来自 `scope_vars`）→ `jeti_struct_eq_<tag>(a,b)`；`!=` → `(eq == 0)`（零新 AST 节点）。tag 集 `struct_eq_tags` 经 pipeline 传 codegen（`-fno-checker` 为空=C 原生行为）。
- codegen `emit_struct_eq_functions`：Section 5 后按需发射；嵌套 struct 递归、数组字段 memcmp、指针字段地址比较。
- **连带 parser bug 修复+回退**：`struct Config in;` 炸 → 先补了字段名接受 `In` 关键词；随后（见下）`in` 降级上下文关键词后该补丁**删除**。

#### #3 协议组合 `P & Q` ✅（golden/31 扩展 + negative/proto_intersection_missing.jeti）
- parser 三处协议列表（`parse_type_full` 的 `<...>`、`@interface` 两个 `<...>` 块、`@protocol <parents>`）逗号分支扩为 `Comma || Ampersand`，落同一合取 `Vec<String>`。
- **下游零改动**：binder `parents` 去重、checker 逐协议 conformance、codegen（槽位不受影响）全部复用。

#### #4 await 里程碑 1 ✅（golden/34 + negative/async_sync_call.jeti）——设计决策（用户拍板）
1. **入口 = async void 方法**（C# `async void` vs `async Task` 同构；单线程协作泵无死锁风险）。同步调非 void async → error + 修复提示。体含 `@await` 即 async，链式传染，零标注。
2. 语句位 `@await f();` = 求值后丢弃。
3. break/continue 跨 await 放行（M2 状态跳转）；@noarc 跨 await 放行；@try 跨 await M1 报错（jmp_buf 不能跨挂起点）。
4. bridge header：async void 发 wrapper 声明；非 void async 跳过（M2）。
- **语法层（2026-09-27 定案：`@await`，替代此前的裸上下文关键词方案）**：`@await` 入 lexer KW_TABLE（`KeywordKind::AtAwait`），`parse_unary` 开头 `match_keyword(AtAwait)` → `parse_unary` 递归取操作数 → Await 节点。动机：裸 `await` 在前缀运算符位是开放上下文，歧义不可根除（`await * 2` 解析成 `await (*2)`、无法调用名为 `await` 的 C 函数）；`@await` 两个世界 100% 无歧义，`int await = 1;` / `await * 2` / 调用 C 函数 `await(x)` 全部照常。`@` 判据自洽：表达式位已有 `@selector`/`@42` 装箱等机制先例。
- **AST**：`CstExprData::Await(Box)` / `AstExprData::Await(Box)`；elaborator 直转；checker 返回内部类型；codegen passthrough 安全网。
- **检查层**（`jeti_async::check_unit`，desugar 前跑原始 AST）：@try 跨 await（try/catch/finally 任一含 await）+ 同步上下文调非 void async。⚠️ 坑：必须同时走 `Class.methods` **和顶层 `Function`（main）**——初版漏了 main，负例自测时抓到。
- **desugar**（`jeti_async::desugar_unit`，Step 4.8 在 ARC/checker 前）：**不改方法签名**（vtable 槽位不动、跨 TU 安全、bridge header 零特判）。`@await e` → `(jeti_task_resume(__jeti_task), e)`；方法体包 `jeti_task_create(0, self, 0)` / `jeti_task_join(...)`。M1 entry=NULL → resume 即完成返回 1（纯 API 契约钩子，行为=同步执行）。
- **runtime**（runtime.{h,c}）：`NPTask{state,finished,entry,self_obj,frame,result,parent}` + create/resume/finish/join；host 用 calloc/free（⚠️ 不是 `jeti_malloc`——那是 freestanding 用户提供符号，链接 libjeti.a 会 undefined）。⚠️ 坑：改 runtime.c 后 `touch` 不够，需 `rm target/debug/libjeti.a` + touch 才会重编（jetic 优先链静态库）。
- **M2 待做**：真状态机 `switch(t->state)` 拆段、活过挂起点的局部提升进 frame（仿已删除的 TRY_LIFT 思路，见 UAF 修复节——释放结算归任务退出汇合点）、break/continue → 状态跳转、ARC 任务退出汇合点统一结算、`jeti_task_step`/bridge wrapper/`jeti_run_all`。

#### #4 await 里程碑 2 ✅（2026-09-27，本会话续）—— 真状态机
- **两函数形态**：每个 async 方法 M 编译为 ①`static`入口 `jeti_async_state_<M>(NPTask *t)`——body 包在 `switch(t->state)` 的 `case 1:` 里，尾部 `case <final>:` 完成；②原 vtable 函数体变 **driver**：`t = jeti_task_create(entry, self, sizeof(struct <M>_frame))` → 装填 frame 参数 → `jeti_task_join(t)` → 非 void 再 `return (T)__jeti_r`。**签名不变**（vtable 槽位/跨 TU/bridge header 零影响）。
- **frame struct**：`struct <M>_frame` 只装**参数**（M2 同步驱动模型下局部变量在单次 resume 内完成生命周期，留在 C 栈；M3 才提升真正跨 await 的局部）。⚠️ 坑：codegen 的 Struct 分支只认 `AstDeclData::Ivar` 字段（Variable 会被跳过发射成空 struct）；零参数方法 frame 为空 → codegen 跳过发射 → `sizeof` incomplete type，需填充位字段 `char __jeti_pad`。
- **entry 内改写**：参数引用 → `__jeti_f->param`、`self` → `t->self_obj`（entry 只收 task）；`return e` → `t->result = (void*)(unsigned long)(e); t->state = -1; return 1;`（完成协议）；`await e` → `(t->state = N, e)`（Comma：推进状态 + 求值）。
- **runtime 修复**：`jeti_task_create` 初始 `state = 1`（原来 0 不匹配任何 case → switch 掉空、行为未定义——用 lldb 断点 + 源码插 printf 定位）。
- **验证**：golden/34 输出不变（`result=18`）；跨方法 await 探针（raw called → awaited 7）正确；ASan+UBSan CLEAN；cargo 55/55、test_all 212/227（3 既有失败）。
- **M3 待做**：真挂起源（I/O/调度器 `jeti_run_all`）、entry 返回 0 的实际挂起路径、跨 await 局部提升、break/continue 状态跳转（M2 同步模型下不需要）、`parent` 任务图。

#### 附带：`in` 降级上下文关键词 ✅
- **依据**：ObjC/clang 的 `in` 根本不是关键字——解析器在 for 头做位置模式匹配，其余位置全是合法标识符。jeti 的硬关键词违反 C 超集（`int in = 5;`、`c.in`、`struct Config in;` 全炸），补洞式修复不可持续。
- **改造**：lexer KW_TABLE 删 `("in", In)`（`KeywordKind::In` 保留枚举但无人构造）；for-in 消费点改 token 文本匹配；`scan_for_in_header` 加 `.`/`->` 前缀排除（`for (i = p.in; ...)` 不误报）。验证矩阵：全局变量/字段/成员访问/普通 for 中用 `in` 全过 + golden/30 for-in 不受影响。

### 全语法三方压力测试 (Sep 2026, 本会话) — tests/stress/

三套全语法压力测试，全部通过。目的：验证新语法转译到 C 不出错。**测试侧已规避的编译器 bug 汇总见下方清单——这些是真 bug，待修。**

| 套件 | 位置 | 形态 | 结果 |
|------|------|------|------|
| 裸机 | `tests/stress/baremetal/build.sh` | `-fno-libc` 转译 + `runtime_freestanding.c`（bump allocator）+ `helpers.c` + ARM64 `asm_ext.s`，clang 链接 | ✅ 47 项检查全过（struct ==/嵌套 eq、enum switch、继承+super、协议 P&Q、id<P>、for-in 自定义集合+typed for-in、typed @catch 链+finally+父链 catch（SubErr）、@selector、blocks+__block、Box<int*>、内联 asm+asm goto、fnptr→asm、@synchronized+提前 return+relock、@noarc/@autoreleasepool、nil 消息、respondsToSelector、typeof/builtin；Sep 2026 新语法批：@defer LIFO+提前 return、switch 模式匹配（标量 range/when + 对象 Bind + 臂序）、成员 fn-ptr 直调、enum 形参（TagKind）、nullability 注解（`_Nonnull`/`_Nullable`）+ `nullable`/`in` 作标识符、宏轨道过滤（BM_MAX 透传 / `@"..."` 宏 C 轨剔除）、__weak 赋值重注册+清零、NPAsync<T> 标记+@await 链） |
| 标准库 | `tests/stress/hosted/`（`jetic run hosted_test.jeti -asm asm_host.s`） | Foundation 全量 + ARC + `@await` | ✅ 33 项全过（上表全部 + NPString/NPArray/NPMutableArray/variadic ctor/@42/@1.5/NPLog %@"、嵌套 for-in、isKindOfClass:/respondsToSelector:/isEqual: 分发链、Renderable=<Drawable&Serializable> 组合协议、双泛型特化、嵌套 @try/finally 顺序、__weak、`@await` 状态机 result=43） |
| 三方联编 | `tests/stress/interop/build.sh` | Jeti lib.jeti → lib.c + **桥接头 lib.h**；C caller.c + helper.c + ARM64 asm_lib.s + runtime.c 四方一个 clang 链接 | ✅ 5 项全对（C→Jeti 自由函数→asm、C→Jeti alloc/init/消息、Jeti 方法→asm、Jeti 方法→C helper、Jeti 方法内 block） |

#### 压测挖出的编译器 bug

**✅ 本轮已修（8 项）**：

> **本轮回归基线（全部实测）**：cargo test **55/55**；test_all **220/230**（3 个 `-F` 既有失败不变：两个无 main 库文件 + 一个故意 over-release）；`tests/stress/` 三套 runner 全过（baremetal 31 项 / hosted 33 项 / interop 5 项）；`tests/multi_tu` **9/9**；trace goldens **8/8**（`.out` 已按新行号重新生成——Foundation `NPObject` 本轮 +9 行，内联缓冲区行号整体偏移 86→95，语义逐个核对无误）。

| # | bug | 修法 |
|---|-----|------|
| 3 | `T *a = x, *b = y;` 指针声明符逗号列表解析失败 | parser 逗号列表循环加 `while match(Star)` 逐声明符重放指针；后续声明符另存 `var_type`（原为 `None`），并支持逐声明符 `[]` 后缀。回归见 `tests/comma_ptr_test.jeti` |
| 12 | 顶层 `@class NS::Ghost;` 不认 `::` | `parse_forward_class` 改用 `parse_qualified_name()`，接受 `NS::Ghost` 与多段 `A::B::C` |
| 4 | enum 参数发射成 `struct Mode` | parser 原本把 `struct`/`union`/`enum` 三者统一标 `is_struct`，渲染一律输出 `struct`。新增 `TagKind`（`CstType.tag` / `AstType.tag`）+ `TagKind::keyword()`，两处渲染点（`cst_type_to_c_str` / `ast_type_to_c_str`）改用真实 tag 关键字；elaborator `convert_type` 同步拷贝该字段。⚠️ 坑：`match_keyword` 已消费关键字，tag 须从 `self.previous` 读 |
| 8 | typedef 名形态的 struct 不做 `==` 改写 | checker 新增 `struct_alias_tags`（alias→tag），`check()` 首遍由 `AstDeclData::Typedef` 填充；`is_value_struct` 改收 `&self`，非 `is_struct` 的命名类型也查此表。返回 **tag**（非 alias），因 eq 函数按 tag 命名 |
| 6 | `respondsToSelector:` 传 SEL 变量生成坏 C | checker `MsgSend` 臂加 error：伪方法降级为 vtable 成员 NULL 检查，运行时 SEL 无成员可名，必须 `@selector(...)` 字面量。不再流到 codegen 的 arrow-access fallback |
| 13 | `jeti_meta_init()` 不存在 | runtime.h 声明 + codegen 与 `jeti_metaInit` 并排发 **weak 别名定义** `jeti_meta_init`（其余 runtime 符号皆 snake_case，用户自然这么写）。已用 C host 链接实测 |
| 7 | `@catch` 内 `@throw` 重抛 → 无限重进同一 catch | 在 **handler 选择之前**（catch 体之前）发一次 `memcpy(__jeti_exception_buf, __jeti_saved, sizeof(jmp_buf))` 恢复父级 jmp_buf，使 catch 内的重抛 longjmp 到**外层** try 的 setjmp，而非重入本帧自己的 setjmp。⚠️ 位置是"catch 体之前"一处，不是改在 `@throw` 发射点 |
| 9 | struct eq 函数无前向声明 → 隐式声明+static 冲突 | eq 函数前先发全部 `static int jeti_struct_eq_<Tag>(...)` 原型；嵌套 struct 由 `fields_of` 递归展开进 tag 集（按依赖序发射） |

**⚠️ 修 #3 时引入的回归（已修，务必记住）**：让后续声明符带上 `var_type` 后，**for 头内的声明列表**被 statement 级 Decl 发射器拆成多条 `;` 结尾语句（`for (T p = lo;\n T q = hi-1; ...`）。修法在 codegen `CgStmtData::For` 发射点：for 头专用路径把 `next` 链发成**一个逗号列表**。两个 C 细节：① 分隔符 `;` 原由 Decl 发射器尾部提供，新路径须自己补；② **C 声明列表里类型只能出现一次**——`for (size_t p = lo, size_t q = ...)` 是语法错误，后续声明符必须丢弃类型（故 for 头路径用 `_n_type` 忽略它，statement 级仍需要它以支持 `T *a, *b`）。

**连带修复**：`TagKind` 落地时暴露 `is_union` 从 CST 到 AST 被丢弃（`union U2 u;` 的**使用**正确发 `union`，但**定义**发 `struct` → tag mismatch）。已把 `is_union` 补进 `AstDeclData::Aggregate` / `CgDeclData::Struct` / `AggDef`，定义与使用两侧一致。

**⚠️ 仍待修（5 项）**：

1. **block 字面量返回类型推断缺失**：`^(int a,int b){return a*b;}` 发射 `^void` → 调用处炸。必须显式 `^int(...)`。
2. **block 捕获改写是 TU 级名字匹配，不是函数作用域**：类方法参数名叫 `base` 就被改写成 `base.__forwarding->__value`（main 里 `__block int base` 的捕获改写串了）。**高危**——任何与 __block 变量同名的参数/局部都会被污染。
5. **类方法里 `[super alloc]`**：派发到实例 vtable/meta vtable 无 `alloc` 槽位 → 坏 C。不覆写 `+alloc`（直接继承 NPObject 的）可绕过；裸机自定义根类用 `jeti_alloc(self)`。
10. **命名空间内泛型 receiver 不单态化**：`Metal::Box<int *>` receiver 仍派发泛型版签名；顶层 `Box<int *>` 正常。
11. **`[obj class]` 特判在裸机隐式根类下生成不完整类型解引用**（`jeti_root` struct 在 freestanding 不完整）。
12. ~~**对象下标订阅 `a[0]` 透传坏 C**~~ → **✅ 已修**（2026-09-29，本会话，见下节"对象下标与泛型擦除"）：checker 层类型感知改写 `a[0]` → `[a objectAtIndex:0]`、`a[0]=v` → `[a setObject:v atIndex:0]`，C 数组/指针下标不受影响。
13. ~~**`NPArray<T>` 泛型参数静默擦除**~~ → **✅ 已缓解**（2026-09-29，本会话）：checker 发 warning 点破（非泛型类带 `<...>` 不生成特化、不做元素类型检查）。⚠️ **仍是 warning 不是 error**，`-Werror` 下才拦截；真正的单态化未做。

#### 严格 pedantic 归因（`clang -pedantic -Werror`，三份生成 C）
- **`$` in identifier**（每份 16-19 处）——命名方案固有（`JETI_CLASS_$_X`/`JETI_VTABLE_$_X`），clang 扩展。判 `-Wno-dollar-in-identifier-extension` 后再看。
- **GNU statement expression**（baremetal 12 / hosted 15 / interop 1）——**nil 消息守卫** `({ NPObject *__jeti_tmp_N = ...; ... : 0; })` 与 block 捕获展开固有。**当前 codegen 设计做不到纯 C99 pedantic-clean**；路线图"clang/gcc -pedantic -Werror 双通过"对 nil 守卫路径不成立（GCC/clang 都支持 statement expression，pedantic 只是警告级别标注）。
- **`cond ? voidf() : 0` 单侧 void**（C99 禁止）——nil 守卫对 void 方法的发射形态，同源。
- 其余为少量指针不匹配告警（泛型/伪方法派发 `NPObject*` vs 子类指针，ABI 层无害）与 `if ((x == 1))` 双括号风格。
- **结论**：三份生成 C 在 `-Wno-dollar-in-identifier-extension -Wno-gnu-statement-expression -Wno-pedantic(仅 void-conditional 处)` 下零错误；完全 pedantic-clean 需改 nil 守卫发射形态（do/while 局部变量块替代 statement expression），列为后续优化。

#### 三方联编要点（build.sh 注释已含）
- 生成 `lib.c` 是宿主模式：**不自含 runtime 头**，clang 需 `-include jeti/runtime.h`。
- **`.s` 必须单独 `clang -c` 先汇编成 `.o` 再链**——`-include` 强制头会应用到 `.s` 上炸掉汇编解析（曾把 ARM64 助记符当 C 编译）。
- 桥接头只发**实例方法** wrapper：`+alloc` 类方法、自由函数、`extern NPClass JETI_CLASS_$_X` 类元数据都需 caller 手动 extern；对象创建用公开 API `jeti_alloc(&JETI_CLASS_$_Calc)`（生成代码内部的 `NPObject_alloc` 不在公开头里）。

### 对象下标订阅 + 泛型参数擦除告警 ✅ (Sep 2026, 本会话)

对账表上仅剩的两条实现缺口，本会话全部落地。

#### ① `a[0]` 透传坏 C → checker 层类型感知改写 ✅

**原症状**：`NPArray *a; [a[0] intValue]` 生成 C 原样输出 `a[0]`，clang `used type 'NPArray' where arithmetic or pointer type is required`，且报错位置在生成 C（`<stdin>:2042`）**无从对应源码**。C 数组下标正常。

**修法**（`crates/checker/src/lib.rs`）：仿 `maybe_rewrite_struct_eq` 的既有先例，在 checker 改写 AST——**parser 层拿不到变量类型，且 emit 阶段无 vtable 元数据**（同 for-in desugar 的理由）。两个改写函数 + 两个判定辅助：

| 源 | 改写成 | 判定条件 |
|----|--------|---------|
| `recv[i]` | `[recv objectAtIndex:i]` | 接收者是 `Named`+指针+非数组，且该类（含父类）声明了 `objectAtIndex:` |
| `recv[i] = v` | `[recv setObject:v atIndex:i]` | 同上，**且**声明了 `setObject:atIndex:`（区分可变的 `NPMutableArray` 与不可变的 `NPArray`） |

- 复用 `MsgSend` 节点 → 下游静态 vtable 派发、nil 守卫、SEL 常量**零特判**；vtable 槽位由方法名决定，跨 TU 布局不受影响。
- **C 下标必须零误伤**，故判据不是"看起来像对象"而是**符号表实证**：沿 superclass 链查该类是否真声明了 `objectAtIndex:`。`int c[3]; c[1]`、`char *p; p[0]`、`const char *s; s[2]` 一律原样透传（探针 A–G 全对）。
- **⚠️ 坑 1：selector 归一化要去掉"每一个"冒号，不是只去尾冒号**。binder 存多段 selector 时**抹掉全部冒号**（`setObject:atIndex:` → `setObjectatIndex`），只 `trim_end_matches(':')` 会让写侧永远匹配不上（首版即如此，静默失效）。读侧因 `objectAtIndex:` 是单段 selector 恰好能中，掩盖了这个 bug——**单段通过不能证明归一化正确**。
- **⚠️ 坑 2：写侧需要 `receiver_static_type` 回退**。checker 的 `Assign` 臂用 `check_expr(&mut *target.clone())` 在**克隆**上检查，原 target 节点永远拿不到 `expr_type`；故回退到 `lookup_scope_var_type` 查作用域表，否则写侧改写看不到 `m` 是 `NPMutableArray`。

#### ② `NPArray<T>` 静默擦除 → checker warning ✅（附两处前置修复）

**原症状**：`NPArray<NPString *> *a` 语法接受、能跑通，但纯靠 id 擦除——生成 C 中 `NPArray_NPString` **零次出现**，`objectAtIndex:` 仍返回 `NPObject *`，无特化、无元素类型检查。写了泛型参数的人会以为有类型保护。

**修法**：`warn_if_erased_generics`——类型带 `type_args` 但所指的类**未声明任何 type_params** 时发 warning（点明不生成特化、不检查元素类型、建议改用泛型容器）。挂载在 checker 的 `Variable` 声明臂。

⚠️ **只做 warning 是有意的**：真正的 `NPArray<T>` 单态化是独立增量（见"剩余已知问题 #5"），本条只消除"静默"这个最有害的部分——用户至少被告知。`-Werror` 下可提升为 error。

**⚠️ 落地时连带暴露并修掉的三个真 bug**（都是"判据不可靠"而非新功能）：
- **parser 协议路径的试探是破坏性的**（`parser.rs` `<...>` 解析）：非泛型类的 `<...>` 会先被当协议列表试探，`NPArray<NPString *>` 因 `*` 过不了 `>` 前瞻而失败，**却不回滚** → 随后的 type_args 路径从 `*` 开始解析、产出空 args，`<NPString *>` 被**静默丢弃**。修法：`save_pos`/`restore_pos` 回滚（与消息接收者类型试探同一套机制）。
- **binder 从不填 `SymbolData::Class.type_params`**（`binder.rs` 该字段零写入点，恒空）：导致判据对**所有**类都成立，用户自定义泛型 `Box<T>` 被误报。修法：照 `superclass_from_cst` 的同款模式取出 CST 的 `type_params` 写入符号表。
- **类声明 `<T>` 消歧的复用误判**（`parser.rs` `@interface` 的 `<...>` 块）：`@interface A<T>` 落地后 `T` 被 `add_type_name` 注册，后续 `@interface B<T>` 的 `<T>` 被"全部是已声明类型 → 协议列表"判据**误路由**——B 的 `type_params` 落空、checker 误报"擦除"，而 codegen 实际照常单态化 `B<int>`（生成 C 中 `B_int` 18 次出现，探针实证）。修法：判据改 `is_type_name(n) && !is_type_param(n)`——**当前在册的类型参数名优先于协议解读**（`@protocol` 注册的名字永远不在 type_params 里，二者不相交）。

> 这三处都是**先有探针量到现象、再定位到机制**才发现的；只量现象（"特化零次出现"）不足以定位，DBG 打印 `type_args`/`protocol_refs` 才 pinfoint 到 parser 的回滚缺失。

#### ③ 指定初始化器形态 4：**不是缺口，是测试文件自身笔误**

详见勘误 2 的矩阵。`tests/designated_init_test.jeti` 两处笔误已修（补 `NPRange` typedef、`CGPoint` 字段名 `x/y`→`wx/wy`），6 形态端到端 exit=0。编译器侧**零改动**。

**回归基线（本会话结束时实测）**：cargo test **102/102**、test_all **257/265**（2 failed 均为 `-F` 既有基线：`diamond_impl-F.jeti` 无 main、`double_release-F.jeti` 故意崩溃；0 新增失败）、multi_tu **9/9**、trace golden **8/8**（用 `JETIC=$PWD/target/debug/jetic` 跑 debug 二进制——脚本默认取 `target/release/jetic`，不显式指定等于用陈旧二进制做假回归）、eh_diff **7/7**。

### 多平台测试 — OrbStack Ubuntu（arm64 + amd64）✅ (Sep 2026, 本会话)

- ✅ **产物**：`build-all.sh` 同款 zig-cc 交叉编译 musl 静态二进制（`target/{aarch64,x86_64}-unknown-linux-musl/release/jetic`，`file` 确认 static-pie）；include/ 用项目根当前版本（target 里的 include 是打包时快照，会过期）。
- ✅ **`test_all.py` 支持 `JETIC=` 环境变量覆盖二进制**（同 trace golden runner 的 `JETIC=` 约定），跨平台回归不再硬编码 `target/debug/jetic`。
- ✅ **e2e 矩阵两架构全绿**（冒烟 / 继承+协议 / 泛型 monomorphization / struct eq / @try 三件套 / 跨函数泄漏复现 / ASan UAF / trace）。
- ⚠️ **Linux 差异三件套（非 bug，是平台事实）**：
  1. **blocks**：Mac clang 默认开 `-fblocks`，上游 clang **不开**，且 jetic 从不传该旗标 → `blocks support disabled` 编译失败。解法：`JETI_CC="clang -fblocks -lBlocksRuntime"`（需 `apt install libblocksruntime-dev`）。`-backend gcc` portable 展开不受影响，但自身另有（旧）缺陷。
  2. **libm**：macOS 的 `sin/cos` 在 libSystem 里自动链，Linux 需显式 `-lm`（`JETI_CC="... -lm"`）。tricalc 因此失败。
  3. **gcc 15 默认 gnu23**：生成 C 里的 ObjC 式隐式上下转型（`Item*`↔`NPObject*`）被当**硬错误**（clang gnu17 只是警告）→ gcc 后端需 `-Wno-error=incompatible-pointer-types -Wno-error=int-conversion`。
- ⚠️ **test_all 终值**：arm **161/175**、x64 **159/175**。剩余失败全部归因：asm 用例（Apple 汇编器语法 Linux 不认）、mega_fusion（依赖 `.s` + blocks）、tricalc（test_all 无法传 `-lm`）、5 个交互式 CANCELED、3 个 `-F` 基线。无 jeti 编译器本身的新缺陷。
- ✅ **顺带修了 1 个平台无关 checker bug**：逗号声明列表的后续声明符未注册进 `scope_vars`（`Point p1 = {1,2}, p2 = {1,2};` 的 `==` 不改写）——见上方 checker 记录。
- **教训**：① orb run 通道会间歇性吞输出，`echo` 不见时先 `orb run ... cat <logfile>` 而非重跑；② VM 里 bash -c 含 `-` 开头的 curl/arg 时小心 glob/选项误伤；③ 排查失败先问"Mac 上同文件什么状态"，避免把平台差异当回归。

### 跨平台收尾：链接 flag 自动注入 + `-freestanding` 改名 (Sep 2026, 本会话)

- ✅ **jetic 自动链接平台 flag**（`compile_to_binary`，host 模式，从**生成的 C 内容**探测）：C 含 `(^`（block 类型未展开）→ 加 `-fblocks`，Linux 且非 shared 再加 `-lBlocksRuntime`；Linux 非 shared 一律加 `-lm`（macOS libm 折进 libSystem 自动链，Linux 需显式；`--as-needed` 下未用会被链接器丢弃）。freestanding（`no_libc`）与 `-backend gcc`（portable 展开 block）自动豁免。**VM 实测**：不设 `JETI_CC` 复跑 test_all，arm 161/175、x64 159/175，与手动 `JETI_CC="clang -fblocks -lBlocksRuntime -lm"` 逐项一致。
- ✅ **`-fno-libc` → `-freestanding` 改名（非破坏）**：`-freestanding` 为规范名（与 clang 发给 C 编译器的旗标同名，语义同源），`-fno-libc` 保留为 deprecated 别名——现有脚本（soma-kernel Makefile、golden/25、stress baremetal）零迁移成本。全链 7 处：clap Arg（新增独立 arg）、`DOUBLE_TO_SINGLE`、`jetic_flags()`、`norm_flag`（`--freestanding`）、手动 help、两处参数解析（run 前扫描 + 主循环，均 `== "-fno-libc" || == "-freestanding"`）、completions 三件套。验证：两旗标转译输出 `diff` 逐字节一致（`__JETI_FREESTANDING` 都置位）、`-h` 显示规范名。⚠️ **已被二次改名取代**（2026-09-30）：规范名现为 **`-ffreestanding`**（双 f，对齐 clang 拼写），`-freestanding`/`-fno-libc` 别名已按用户拍板删除——见"`-ffreestanding` 规范名定名 + 别名删除"节。
- ✅ **README 异常章节按 ASan 实测更正**：跨函数 throw 的中间帧对象泄漏（longjmp 跳过 scope-end release）仍是已知限制；同函数 @throw 曾是双重释放 UAF——**已修复**，见下节。
- ✅ **test_all.py docstring 记录 env 覆盖**：`JETIC=`（二进制）与 `JETI_CC=`（透传 C 编译器旗标）的用法与默认值。

### `-nostdinc` 独立旗标 + `runtime_freestanding.c` 重命名 (Sep 2026, 本会话)

- ✅ **`-nostdinc` 独立旗标（正交化）**：给 C 编译器透传 `-nostdinc`（只剥系统 include 路径，**不带**任何裸机旗标）。**`-freestanding` 不再隐含 `-nostdinc`**——此前 `compile_to_binary` 的 no_libc 旗标块固定发它、ctype_probe 也捆绑（`no_libc → -ffreestanding -nostdinc`），现已拆开，四种组合自由。接线点：clap Arg、`DOUBLE_TO_SINGLE`、`jetic_flags()`、`norm_flag`、手动 help、两处参数解析（run 前扫描 + 主循环）、`pipeline.nostdinc` 字段、`compile_to_binary` 新参数（两处调用点）、`probe_c_type_names` 参数拆为 `freestanding`+`nostdinc`、completions 三件套。
- **动机（"三头由谁提供"讨论的落地）**：`-nostdinc` 连 clang builtin 资源目录也剥掉（上节探针实证）——剥不剥是**内核作者的决策**（soma-kernel 要 `-nostdinc` + 自带 `include/`；宿主试验想让 clang 自带三头兜底），编译器前端语义（`-freestanding`）与头搜索策略（`-nostdinc`）本就是两个正交维度。
- ✅ **`runtime_baremetal.c` → `runtime_freestanding.c` 重命名**：原名"baremetal"名不副实——它同样服务**没加 `-nostdinc`** 的 freestanding 构建路径（clang builtin 三头兜底），且与 `-freestanding` 旗标同名同源。`git mv`（保历史）+ 全仓库 31 处引用替换（16 文件：build.rs、golden/25+26 与 stress 的 build.sh、soma Makefile/kernel.c/soma_core.jeti、README/CHINESE、代码注释），grep 残留 **0**。
- ✅ **memset weak 定义补齐**：`memset` 从"自声明原型"升级为 **weak 定义**（`#undef` + `__attribute__((weak))`，与 memcpy 块同款）——runtime 自含全部可能被 `-ffreestanding` 编译器发射的 mem 系符号；内核自己的强定义链接时胜出。
- **没加 `-nostdinc` 时的 runtime 设计定案（讨论结论）**：**不搞新文件，单文件双模式**。`runtime_freestanding.c` 的 include 面只有标准强制三头（C11 §4p6 freestanding 实现必须提供，clang builtin 目录自带兜底），差异全在 `__JETI_FREESTANDING` 宏内（jmp_buf 来源、异常全局 TLS 与否）。加 `-nostdinc` 的内核 `-I` 自带三头（soma-kernel 模式）；不带的直接链该文件。拆两个文件的收益（省几十行 weak 定义）不抵符号面分叉的维护成本。
- **验证**：cargo 0 warning + **137/137**；fakecc 探针矩阵 A1 `-freestanding`→ffreestanding=1/nostdinc=0、A2 组合→nostdinc=1、A3 `-nostdinc` 单独→ffreestanding=0/nostdinc=1、A4 run 模式前扫描命中；B1 转译正交性（`-nostdinc` 生成 C 逐字节同）；B2 probe 降级（`-nostdinc` 下 probe 失败回退，转译 rc=0）；B3 `-freestanding` 宏置位=1；golden/25+26、stress baremetal 全 exit=0；soma 同款旗标单文件 i386 ELF + Makefile 编译过；test_all **309/317**（2 个 `-F` 既有失败不变）。

### `-ffreestanding` 规范名定名 + 别名删除 (Sep 2026, 本会话)

- **动机（用户拼写事故驱动）**：用户敲 clang 拼写 `-ffreestanding`（双 f）报 `Unknown argument`——jetic CLI 规范名是单 f `-freestanding`，而双 f 是转发给 clang 的 C 编译器旗标（`compile_to_binary`），两个世界撞名必然有人敲错（项目作者本人就中招）。拍板：**规范名改为双 f `-ffreestanding`**，与转发旗标/clang 肌肉记忆同名同源（同 `-nostdinc` 先例——CLI 家族彻底统一为 clang 拼写）。
- ✅ **别名整体删除（用户拍板"能删就删"，无过渡期）**：`-freestanding`（单 f）与 `-fno-libc` 两个 deprecated 别名从解析链移除，旧拼写现在响亮报 `Unknown argument: -fno-libc`（探针实证），不做静默降级。⚠️ 本文件更早各节写的 `-freestanding` 拼写一律读作现规范名 `-ffreestanding`（`runtime_freestanding.c` 文件名不受影响）。
- **迁移清单（全仓库 27 处，grep 零残留——AGENTS 历史节除外）**：main.rs 8 触点（clap Arg 改名+删 no-libc arg、`DOUBLE_TO_SINGLE` 删两条目、`jetic_flags()`、`norm_flag`、手动 help、run 前扫描、主循环）；completions 三件套 + `jetic.bash` 的 `-fno-libc)` case 分支；build.sh ×3（golden/25、golden/26、stress baremetal）；soma-kernel Makefile；README/CHINESE 各 6 处（含 help 块对齐修正）；test_all.py 注释。
- **验证**：三种形态探针——`-ffreestanding` rc=0、旧别名正确拒绝、fakecc 截获（ffreestanding=1/nostdinc=1 组合正确到达 clang）；`-h` 与 completions 显示新规范名；cargo 0 warning + **137/137**；golden/25+26、stress baremetal 全 exit=0；soma Makefile `-B` 强制重跑转译规则 rc=0（新旗标真实执行）；test_all **309/317**（2 个 `-F` 既有失败不变）。

### GPT 裸机评审四项修复 (Sep 2026, 本会话) — runtime_freestanding + parser

> 外部评审（GPT）报告五项：①malloc 契约/对齐 ②for-in 别名 `NPObject **` ③裸机 Blocks ABI 缺失 ④weak 表项泄漏 ⑤build.sh 非"纯裸机"。**复验四项属实、⑤属实但是已知设计（golden/25 host methodology，AGENTS/注释早已声明）**。修 ①②④ + ③ 如实声明，全部配白盒 C 探针。

- ✅ **① `jeti_malloc` 零初始化 + max_align_t 对齐**：runtime.h:148 的契约（"must zero-initialize"）此前实现只移 bump 指针——实证影响面比评审说的更大：`jeti_task_create` 的 task 结构与 **frame 都来自这个未清零的 malloc**（裸机 `@await` 的 frame 填充位读垃圾）。修法：堆改 `_Alignas(max_align_t) unsigned char`（char 数组只保证对齐 1）、分配对齐 `_Alignof(max_align_t)`（原 `sizeof(size_t)`）、返回前 `memset(p, 0, size)`（本文件已有 weak memset 定义）。**白盒探针**（include .c 摸 statics，先把整堆涂 0xAA 再分配——BSS 本来是零，不涂脏证明不了 memset）：1-40 尺寸对齐全过、zeroed=1、`double=1.5`（严格对齐写）、**frame_zeroed=1**、溢出守卫仍返 NULL（memset 不越界）。连带修：堆改 `unsigned char` 后 `jeti_is_heap_object` 的比较基址同步改（0 警告）。⚠️ 验证方法论：验证"清零"必须先涂脏内存，否则 BSS 零是假阳性。
- ✅ **④ `jeti_weakUnregister` 释放空 entry**：原实现只清 slot，最后一个 slot 清掉后 `entry->object` 悬空占位——对象长命 + weak 全部出作用域时每目标永久占一项，64 目标满表后静默丢弃（`jeti_weakClearAll`（release 路径）会清 entry，所以 ARC 下少见、裸机 bump allocator 下必累积）。修法：slot 清空后全查，空则 `entry->object = NULL` 归还可回收项。**白盒探针 7 断言全绿**：64 满表 / 第 65 目标静默丢弃（文档化策略不变）/ 部分注销 entry 保留 / 末 slot 注销 entry 释放 / 新目标可注册（表不再饱和） / 未知位置注销 no-op / 全释放后重填 64。⚠️ 探针首版两处断言自身写错（slot 索引、剩余 entry 计数）——修探针不改运行时，C/D/F（修复本体）首轮就绿。
- ✅ **② for-in 集合别名 `NPObject * *` → 单层 `NPObject *`**：parser desugar 给 `TypePrim::Id` 又置了 `is_pointer = true`（parser.rs:2944）——`id` 渲染本就是 `NPObject *`，再加一层成 `NPObject * *`（评审抓到的 ：1591/:1872 两处）。位模式相同所以一直能跑，但类型是错的（指针告警源）。修法：删 `is_pointer`（一行）+ desugar 注释记录"勿再置位"防回归。**验证**：生成 C 两处单层 + 双层残留 0；golden/30 stdout diff **MATCH**；stress baremetal（N6 自定义集合 / N12 typed for-in）exit=0；stress hosted（F5 NPArray for-in）exit=0。⚠️ 首次 hosted 复跑 127 是**探针相对路径少一级**（`../../target` 从 tests/stress/hosted 数漏）——平台无关验证用绝对路径。
- ✅ **③ Blocks ABI：声明"不捆绑"，不加假 stub**：runtime.h 无条件声明 `_Block_copy/_Block_release`（:253），freestanding runtime 不提供任何 blocks 符号（grep=0）；host 链路靠平台 Blocks runtime 兜底，`nm -u` 实证 `__NSConcreteStackBlock` 未解析——真裸机链接会炸。**定案（按"不加假 stub"原则）**：不实现 Blocks ABI 移植（几百行 ABI 级代码），如实声明两条真路——链接 Blocks runtime 移植，或 `-backend portable|gcc`（block 展开为普通 C 函数、零 ABI 符号）。四处同步：runtime.h blocks 节注释、runtime_freestanding.c 头注释、README/CHINESE 裸机节各一条 bullet。
- **⑤ 的定性（评审对，结论不变）**：stress build.sh 是 host methodology（Mach-O + libc/Blocks host 符号，AGENTS 与脚本注释早已声明）；评审的有效增量是 ③×⑤ 联动——host 链接掩盖了 blocks ABI 缺口，现已文档化。
- **回归**：cargo **137/137**（0 warning）、golden/25 exit=0、stress baremetal exit=0（47 项）、test_all **309/317**（2 个 `-F` 既有失败不变）。
- **追记（同会话）— stress 的 async 用法对账 canonical 模式**：baremetal_test 的 main 内两处 `@await`（R5/R8）触发 `[async] warning: 'main' contains '@await' but its return type is not marked 'NPAsync<T>'`——对账表"裸 T + 有 await = warning"行如实命中，且 main 正是设计文档钉死"`@await` 永不直接出现"的位置（测试自身违反了 canonical 模式）。修法：R5/R8 表达式**原样**移入 `+ (NPAsync<void>) awaitMake` / `awaitSum:` 类方法（标记+await = 对账表 ✅ 行），main 改同步调用（blocking wrapper 内部泵任务，main 语义零变化）。验证：R8=24 不变、build.sh exit=0、用户原样命令（`-ffreestanding -nostdinc`）**stderr=0 零告警**。⚠️ stress 编译的 41 条 clang `-Wall` 告警（id 派发指针宽度、`(x==1)` 双括号风格等）是既有噪音，与本修无关；`hosted_test.jeti` / `full_syntax_test.jeti` 的 main 内 `@await` 同款告警留待同法收敛。

### @try 双重释放 UAF 修复 (Sep 2026, 本会话) — 删除 unwind-lift 机制

- **根因（ASan 实锤，两条路径都炸）**：codegen 的 @try "unwind-lift" shadow 机制（`TRY_LIFT_COUNTER`/`try_lift_shadow`/`insert_lift_mirrors`/`collect_try_lift_vars` 等 ~150 行）与 **ARC（RefVal 状态机）双保险重叠**：ARC 在 `@throw` 处已 release 全部 owned 局部（`arc.rs:209`，"insert releases before control flow exits"），scope-end release 也由 ARC 负责；lift 的 shadow 释放又发在 `state==1` gate **之外**（注释声称"normal/unwind mutually exclusive, no double-release"但发射未 gate）——**连正常不抛异常的路径都 UAF**（每个含 owned 局部的 @try 都中招，无 ASan 时是静默读已释放内存）。throw 路径：ARC 释放（1→0，free）→ lift 释放 shadow（读 freed）。
- **修法（删除 > 缝合）**：整体移除 lift 机制（Try 臂 shadow 声明/镜像/释放三块 + 8 个辅助函数），ARC 成为自动释放注入的**唯一所有者**；Try 臂仅剩 setjmp 状态机骨架。原则与 ARC 哲学一致：**宁可 leak 不 double-free**——分支合并后 Unknown 状态的局部在 throw 路径可能泄漏（保守方向），跨函数 throw 中间帧泄漏不变（真 unwind 是正路，EHABI）。
- **验证**：ASan 下 throw 路径（`EXIT=0`，Item dealloc 恰一次，A/B caught）与 normal 路径（`EXIT=0`，dealloc 一次）双清；cargo test 全绿、test_all **220/230**（3 个 `-F` 既有失败不变）。
- **教训**：两条独立机制做同一件事时，注释里的"互斥"设计要对照发射产物核实（此 bug 的 gate 只存在于注释里）；ASan 报告必须读到 fre/free-后访问两处调用点再下结论。

### `-eh checked` 异常传播后端 ✅ (Sep 2026, 本会话) — 新 crate `crates/eh`（M1）

- **动机**：sjlj（setjmp/longjmp）的已知缺陷——longjmp 跳过中间帧的 scope-end release，跨函数 throw 泄漏中间帧 owned 对象（README 异常章节已记录的既有限制）。`-eh checked` 是零运行时 unwind 的替代后端：显式错误旗标 + 调用点检查链，ARC 正常结算每一帧。
- **CLI**：`-eh <checked|sjlj>`（pipeline `eh_checked` 字段；默认 `sjlj`，行为零变化）。`-eh` 解析、`norm_flag`/`DOUBLE_TO_SINGLE`、flag picker、completions 三件套已同步。
- **desugar 形态**（`jeti_eh::desugar_unit`，pipeline 在 desugar 前（pre-ARC）调用，Step 4.8 先例）：
  - `@throw e` → `__jeti_eh_flag = 1; __jeti_eh_val = (NPObject *)e; return <zero>;`（zero 按函数返回类型取 `0`/`(id)0`/void）
  - 每个可能抛出的调用点之后注入 `if (__jeti_eh_flag) { return <zero>; }`（传播链）；函数体入口插 `__jeti_eh_flag = 0;`
  - `@catch (T *e)` → `if (__jeti_eh_flag) { ...isa 检查（typed catch 不匹配则重新置位 flag 并 fall through 到下一 arm）... __jeti_eh_flag = 0; T *e = (T *)__jeti_eh_val; <body> }`
  - `@finally` 在自然汇合点执行（无需独立机制）
  - **try body 无需入口检查**：flag 在 try 入口可证为 0（传播立即 return；catch 会取走 flag）
- **作用域设计（本轮修复的关键 bug）**：调用点后的守卫**不能**逐语句包独立 `if (flag==0) {...}`——后续语句会引用守卫块内声明的变量（`<stdin>:1629: use of undeclared identifier 's2'`，checked 生成 C 共 20 错）。修法：**整段 tail 收进一个守卫块**，块尾与外层 compound 对齐——词法作用域不变（后续语句可见声明），ARC scope-end release 仍执行。`rewrite_guarded` 辅助随之删除（`deny(dead_code)` 会报错）。
- **M1 限制**（`check_unit` 强制，违反报 `file:line:col: error:`）：
  - `@throw` 只能作为整条语句（不能嵌在表达式里）
  - block 字面量内不得 `@throw`（跨块边界无旗标传播路径）
  - 其余场景 checked 全支持（嵌套 try、typed catch 链、finally）
- **端到端验证**（`t4.jeti`：`middle` 创建 owned `Item *m` 后穿透 `boom` 的 throw，main 捕获）：非 ASan 构建 + `leaks -atExit` 对比——**sjlj 无 `Item dealloc`（`m` 泄漏，既有缺陷）；checked 有 `Item dealloc` 且无泄漏**。两后端 stdout 语义一致（middle created m / caught / --- end ---）。
- ⚠️ **验证工具坑**：macOS ASan 不支持 `detect_leaks`（需 Linux），而 `leaks` 工具**无法检查 ASan 二进制**（"malloc replacement library without the required support"）——泄漏对比必须用**非 ASan 普通构建** + `leaks -atExit`。
- **checked desugar 必须在 ARC 之前运行**：注入的 early return 是 ARC 已能处理的普通控制流（`@throw` 处的 release 注入对 checked 同样生效，每帧正常结算——这正是它不泄漏的原因）。
- **回归**：cargo test 55/55、test_all 220/230（3 个 `-F` 既有失败不变）、负例（block 内 `@throw`）checked 下 exit=1 报错清晰、默认 sjlj 下同文件正常编译。

### `-eh checked` 默认翻转尝试 — 公投否决，回退 sjlj (Sep 2026, 本会话)

> 动机：checked 是唯一无架构依赖（零 setjmp——`__builtin_setjmp` 在 aarch64-unknown-none **和** arm64-apple-darwin 裸跑 freestanding 分支均不支持）且无跨帧泄漏的 EH 后端，尝试翻转为默认。**实证否决，勿盲目重试——须先清三笔债。**

- **公投结果**：cargo 137/137、eh_diff **7/7**（显式 opt-in 路径完好）全过；但 test_all **25 failed**（基线 2）、trace golden **0/8**、jetic **栈溢出 ×3**（`lifecycle.jeti` / `npmutablearray_test.jeti` 等 Foundation 内联重文件）。
- **三笔债（重试前置条件）**：
  1. **checker 拒收 eh 注入符号**——`use of undeclared identifier '__jeti_eh_flag'`（`__auto_type` 临时声明同理）；需照 `&JETI_CLASS_$_X` 先例给 eh 注入命名空间（`__jeti_eh_flag`/`__jeti_eh_val`/`__jeti_eh_tmp_N`/`__auto_type` 声明）放行。
  2. **守卫密度失控**——零 `@try` 的程序（`retain_release.jeti`）也被插 ~20 处旗标守卫，trace 输出被噪音带歪；须收窄到"真可能抛"的调用点（`@throws` 标注函数 + `@try` 区内），而不是每条消息发送。
  3. **栈溢出 ×3 根因未查**（守卫链递归 / block 展开候选）。
- **保留（不随回退撤销）**：checker 锁块 `@throw` warning 加了 `!self.eh_checked` 门控——checked 下本无 cleanup 隐患，按旗标判定、两种默认下都正确。
- **回退**：main.rs 5 触点 + pipeline.rs 2 + completions 2 全部还原；复验 test_all **309/317**、trace **8/8**、eh_diff **7/7**，基线精确复原。
- **多架构结论不受影响**：ARM64 裸机 `@try` 的可用路径仍是 **`-eh checked`**（零 setjmp）；runtime.h builtin 映射的 `JETI_SETJMP_DEFINED` override 钩子仍是待做项。

### `-eh checked` 默认化工程 — 阶段 1：基线与 git 边界 (Sep 2026, 本会话)

> 六阶段路线（债务→验收矩阵→再公投）立项；切换前默认保持 sjlj，开发验证全部走显式 `-eh checked`。分支 `eh-checked-default`，基线提交 `4d8dde5`（101 files，含既有修复与公投记录）。

- **基线复验**：test_all **309/317**（2 failed 均为 `-F` 既有基线）、trace golden **8/8**、eh_diff **7/7**；cargo test 三轮被会话执行限制拦截未跑（单列补验项，预期 137/137）。
- **双模式量测基线**（同源 `-rewrite-jeti` ± `-eh checked`）：

| 文件 | 模式 | 行数 | 字节 | flag 引用 | 守卫 | 说明 |
|------|------|------|------|-----------|------|------|
| retain_release.jeti（**零 @try**） | legacy | 331 | 13,723 | 0 | 0 | 干净 |
| 同上 | checked | 400 | 15,911 | 20 | 20 | **+69 行/+15.9%——20 处无意义守卫**，阶段 3 收窄的实锤 |
| try_catch_basic_test.jeti（try/catch/finally 全套） | legacy | 3,255 | 172,896 | 0 | 0 | |
| 同上 | checked | 4,496 | 218,658 | 364 | — | +1,241 行/+26.5%，含 try 区守卫链 |

- **运行一致性**：try_catch_basic_test 双模式 stdout diff **MATCH**（9 行一致）——checked 当前产物语义正确，问题只在密度与 checker 适配。
- 方法注：golden/15 `try_catch.jeti` 手动转译被 ARC checker 拦（`explicit 'release' not allowed`；test_all 靠 ARC retry 通过）——量测改用 `tests/try_catch_basic_test.jeti`。

### `-eh checked` 默认化工程 — 阶段 2 前置：合成符号全集（枚举完成，Sep 2026）

> checked 模式引入的全部编译器合成符号（eh crate 源枚举 × 生成 C 实证交叉验证，try_catch_basic 样本）。

| 符号 | 类别 | C 类型 | 作用域 | 出处 |
|------|------|--------|--------|------|
| `__jeti_eh_flag` | EH 全局 | `int`（freestanding 普通全局 / hosted `__thread`） | runtime 拥有，函数内读写 | runtime.h:170-174；eh src 38 处 |
| `__jeti_eh_val` | EH 全局 | `id`（NPObject *） | 同上 | runtime.h:171/174 |
| `__jeti_eh_saved_N` | 函数局部 | `int` | 每 @try 序号独占（退出时交还在途状态） | eh src :918 |
| `__jeti_eh_done_N` | 函数局部 | `int` | catch latch（objc_end_catch 语义） | eh src :919 |
| `__jeti_eh_thrown_N` | 函数局部 | `NPObject *`（ARC 拥有此临时） | @throw 处 | eh src :750 |
| `__jeti_eh_tmp_N` | 函数局部 | `__auto_type`（按初始化式推导） | 表达式 hoist | eh src :726 |
| `__jeti_eh_isa` | runtime 函数（非变量） | 声明于 runtime.h | 调用点 | |
| `__auto_type` | 类型关键字（GNU/C23） | — | 声明处 | 生成 C 39 处（同样本） |

- `__jeti_sel_*` 是既有 SEL 常量（已放行），不在本清单。
- **拒绝层归属待实证**：公投失败样本的错误前缀是 `<stdin>:102:82`（clang 格式），非 `[checker] file.jeti:line:col` 格式——早期归档"checker 拒收"可能有误（或是 clang 侧 `-include runtime.h` 缺失）。实现放行机制前必须先复现定层。
- checker 放行先例锚点：`crates/checker/src/lib.rs:1532`（`&JETI_CLASS_$_Foo`）。

### `-eh checked` 默认化工程 — 阶段 2：checker 合成符号机制 ✅ (Sep 2026, 本会话)

> **拒绝层归属勘误（推翻公投时的定性）**：公投日志 63 条 `[checker]` 格式错误全是泛型容器既有误报（`argument of type 'int' does not match...`），**零条**与 `__jeti_eh*` 相关；75+10 条 undeclared 是 **clang 格式**（`<stdin>:行:列`）。真 checker 层拒收只有一类：`initializing '__auto_type' from pointer/object expression — scalar expected`（`dict_literal_test` 62 条）——eh hoist 的 `__jeti_eh_tmp_N` 被 `init_ptr_scalar_mismatch` 标量规则拒绝。

- ✅ **`__auto_type` 按初始化式推导**（GNU 语义，非豁免）：checker Variable 臂识别 `Named("__auto_type")`（eh `auto_type()` 的产物），用 `check_expr` 返回的真实类型**回写 scope_vars**（`__jeti_eh_tmp_N` 此后按真实类型参与赋值/实参/接收者检查）；init 不可推导回退 `id`（万能对象），mismatch 检查对 auto 跳过（被推导取代）。下游全自愈，零类型检查旁路。
- ✅ **保留名声明守卫**（负例机制）：用户声明 `__jeti_eh_flag`/`__jeti_eh_val`/`__jeti_eh_isa`/`__jeti_exception_value`/`__jeti_exception_buf`（desugar 只赋值从不声明的 5 个 runtime 拥有名）→ checker error `'X' is reserved for the exception runtime (-eh)`；desugar 自己声明的 `_N` 后缀临时不受影响。负例持久化 `tests/negative/eh_reserved_name.jeti`。
- ✅ **不跨作用域泄漏**（探针实证）：try 块结束后引用 `__jeti_eh_tmp_2` → `undeclared identifier`（C 词法作用域 + checker 只在声明处注册，合成名天然不逃逸）。
- ✅ **连带修既有缺口（非 eh）**：hosted 生成 C 不含 `<stdlib.h>` 而 vtable `__sig` 校验构造器调 `abort()`（codegen.rs:6871）——最小 Foundation-only 文件 implicit declaration 硬错（legacy 同炸）。include 发射区补带去重的 `<stdlib.h>`（与 string.h 同款先例）。
- **验证**：`dict_literal_test` 62 条 → **0**；`/tmp/eh_min2.jeti`（@throw/@catch/finally 链）端到端 RC=0 `caught 42 / after`；负例两处报错精确；泄漏探针 RC=1；eh_diff **7/7**；cargo build 0 warning。
- **遗留**：~~`dict_literal`/`npstring_demo` + checked 的 RC=134 栈溢出归阶段 4~~ → **已由阶段 3 顺带清偿**（见阶段 4 记录）；公投 75 条 clang undeclared 的确切来源未完全归因（探针文件 include 都在场）——阶段 4 矩阵重推。

### `-eh checked` 默认化工程 — 阶段 3：守卫收窄（异常效果分析）✅ (2026-10-01, 本会话)

> 还第二笔债（守卫密度失控）。新机制 `EffectTable`（`crates/eh/src/lib.rs`）：desugar 前对整 TU 做**异常效果分析**，只有"真可能武装旗标"的语句才触发守卫/线性化；`stmt_may_arm`/`expr_has_call`/`decl_has_call` 三个"有调用就武装"的旧函数整体替换为表驱动版本。

- ✅ **效果三态**：`NoThrow` / `MayThrow`（调用点后守卫）/ `AlwaysThrows`（裸 `@throws`：必返回零且旗标已武装，守卫活路）。判定源：`@throws(T)` → MayThrow；裸 `@throws`（`Some(Void)` 非指针，与 checker `throws_state` 同款编码）→ AlwaysThrows；无标注 → NoThrow。**兜底**：未标注但体内含裸 `@throw` 语句的（checker error 路径 / `-fno-checker`）自升级 MayThrow，不信任"NoThrow"承诺。
- ✅ **`NOTHROW_C_FUNCS` 白名单**：runtime helper（retain/release/autorelease/alloc/metaInit/isKindOfClass/weak/sync/task）、libc（printf 系/str/mem/alloc）、kernel helper（kputs 系）、`NPLog`/`__NPLogv`、`jeti_array_create`/`jeti_dictionary_create`——这些不能跑 jeti 代码、不可能武装旗标。
- ✅ **fixpoint 传递闭包**：调用 MayThrow 的函数本身会带着武装旗标返回零（尾守卫 return 不清旗标），其调用者也必须守卫——迭代到不动点。`middle 调 boom（boom 才 @throw）`：middle 从 NoThrow 升 MayThrow、main 随之升级——eh_diff case 01 跨帧链的根基，单遍扫描必挂。
- ✅ **旗标赋值判武装**：desugar 后的 `@throw` 是 5 语句复合块，辅助调用（retain/autorelease）被白名单豁免、`__jeti_eh_flag = 1` 是普通赋值——首版实测改写后的 throw **丢失尾守卫**。修法：`Assign` 目标为 `__jeti_eh_flag` 的节点直接判 MayThrow（它就是武装机制本身）。
- ✅ **选择子归一化沿用符号表铁律**：key 抹掉**每一个**冒号（`deployShield:` → `deployShield`），只 trim 尾冒号会让多段 selector 静默匹配失败（checker 下标 bug 先例）。
- ✅ **`Namespace` 臂补递归**（scan_decl/desugar_decl/collect_upgrade 三处）：namespace 函数的 @throw 此前永不 desugar（defer crate 同款静默丢弃 bug，本轮预防性堵上）。
- **量测（守卫密度，`grep -c 'if (__jeti_eh_flag'`）**：`retain_release.jeti`（零 @try）checked 守卫 **20 → 0**（legacy 0）；`try_catch_basic_test.jeti` flag 引用 **364 → 67**、体积 +26.5% → **+3.0%**（+5,157 bytes / 172,896）。零 @try 程序不再有任何守卫噪音。
- **验证**：eh_diff **7/7**（含 01 跨帧/02 语句中断/04 rethrow——传递闭包与旗标赋值判定两补丁的正确性主场）；golden/36_defer PASS（rc=0）；golden/15_exceptions +MRC 逐字一致；golden/24 那行析构差异 **legacy 同样出现**（`.out` 陈旧既有基线，与 eh 无关，test_all 只校验 exit code）；test_all **309/317** 与基线精确一致（2 failed 均 `-F` 既有）。
- **已知边界（如实）**：①未标注方法默认 NoThrow 是激进初版——动态派发语义下严格说应 MayThrow，但会重新淹没全部消息发送；依赖 checker 的 @throws 逃逸规则兜底（逃逸的 throw 必须被声明），`-fno-checker` 构建由"体内裸 @throw 自升级"兜底。②跨 TU 未知自由函数默认 MayThrow（无法验证外部 TU）。③`@throws` 一致性对账（interface/impl 双向）沿用 checker 既有机制，eh 侧只读 AST `throws` 字段。

### `-eh checked` 默认化工程 — 阶段 4：Foundation 内联栈溢出 ✅ (2026-10-01, 本会话)

> 第三笔债（Foundation 重文件 + checked → RC=134）——**由阶段 3 守卫收窄顺带清偿**，非独立修复。

- **根因（钉死）**：RC=134（SIGABRT）来自 checked 守卫块的**深度嵌套**——旧"有调用就武装"规则下 Foundation 重文件几乎每条消息发送都触发守卫，`rewrite_stmts` 把剩余语句整体包进 `if (flag) {...}` 并递归重入，生成 C 呈数百层嵌套 `if`，clang 前端解析时栈耗尽。收窄后 `npstring_demo` checked 守卫数 **0**（零 `@try` 文件无任何守卫），嵌套链消失。
- **实证**：`npstring_demo`（rc=0，输出正常）与 `dict_literal_test`（rc=0）两个原 RC=134 文件全部恢复；`npmutablearray_test`/`timsort_test` + checked 亦 rc=0。`grand_feature_stress_test.jeti` 已不在仓库（find 零命中），从验证清单剔除。
- **结论**：**三笔债全清**——债 1 checker 合成符号（阶段 2）、债 2 守卫密度（阶段 3，20→0）、债 3 栈溢出（阶段 3 顺带，本节实证）。仅剩公投 75 条 clang undeclared 归因一项挂账（不阻塞矩阵——相关文件全部 rc=0 运行正常，属日志窗口归因问题）。阶段 5 进入验收矩阵。

### `-eh checked` 默认化工程 — 阶段 5：验收矩阵十场景 ✅ (2026-10-01, 本会话)

> 翻默认的验收门。全部显式 `-eh checked` 跑通，legacy 基线零回归。

| # | 场景 | 结果 |
|---|------|------|
| 1 | 普通程序（零 @try） | retain_release checked 守卫 **0**（legacy 0；收窄前 20） |
| 2 | 单层 @try | try_catch_basic flag 引用 **67**（收窄前 364）、体积 **+3.0%**（收窄前 +26.5%）；eh_min2 链端到端 |
| 3 | 嵌套 @try/@finally | eh_diff 03 PASS |
| 4 | 重抛 | eh_diff 04 PASS（catch 内 rethrow 传播外层） |
| 5 | 跨函数传播 | eh_diff 01 PASS（跨帧释放 + 传递闭包正确） |
| 6 | 裸机 ARM64 | baremetal checked 端到端 rc=0，**63 行输出与 legacy 逐字一致**（transpile `-ffreestanding -eh checked` → clang `-include runtime.h` → 链 runtime_freestanding.c + helpers.c + asm_ext.s） |
| 7 | 多文件/多 TU | multi_tu **10/10**（legacy 基线）；01_basic checked 双 TU（lib+main）链接运行 **expected.txt MATCH** |
| 8 | Foundation 大文件 | npstring_demo / dict_literal_test / npmutablearray_test / timsort_test + checked 全部 rc=0（原 RC=134 三处全清，见阶段 4） |
| 9 | ARC / MRC | eh_diff 基准 `-fobjc-arc-exceptions` 7/7；golden/15 +MRC 逐字一致 |
| 10 | 固定门槛 | test_all **309/317**（与基线精确一致，2 failed 均 `-F` 既有）、trace golden **8/8**、eh_diff **7/7**、裸机压测 13 项检查 rc=0 |

- **守卫数/体积门槛（归档）**：零 `@try` 程序守卫数 = **0**（硬门——出现任何守卫即收窄失效）；含 `@try` 程序 flag 引用 ≤ 收窄前 67/364 ≈ **18%**，生成 C 体积增幅 ≤ **+3%**（收窄前 +26.5%）。
- **遗留挂账**：cargo test 137/137 待用户放行（翻默认前的最后一道门）；公投 75 条 clang undeclared 归因（不阻塞）。
- **下一步（阶段 6）**：cargo test 放行 + 全绿后，独立提交翻默认（`-eh legacy` 兼容旧模式）——遵守公投红线，勿跳过。

### `-eh checked` 默认化工程 — 阶段 6 前置：`-eh legacy` 别名 ✅ (2026-10-01, 本会话)

> 翻默认的兼容准备（**零行为变更**）。`legacy` 成为 sjlj 的官方别名，翻转提交收敛为"改默认值"一步。

- **两处解析点同步**：主循环解析（`main.rs` ~:717）与 run 前扫描（~:787）均接受 `legacy`（行为 = sjlj：`eh_checked` 保持 false）；错误文案改为列出三值（`'checked', 'sjlj' or 'legacy'`）。
- **文案同步**：clap 参数 help、手动 flag 表（:393）、zsh completions（`_jetic`）标注 "'legacy' is an alias"；fish completions 经 `eh=` 值列表天然覆盖，无需改动。
- **验证**：`-eh legacy` transpile rc=0 且守卫数 0（sjlj 语义实证）；run 前扫描路径（`-eh legacy run …`）rc=0；`-eh bogus` 两处路径均清晰报错；`-eh checked` 行为不变。cargo build 0 warning。
- **翻默认时序（红线）**：cargo test 137/137 放行且全绿 → 独立提交改默认（`-eh legacy` 兼容旧模式）→ 翻转后按旧拼写写的脚本零迁移。

### `-eh checked` 默认化工程 — 阶段 6：第二次公投否决，回退复原 (2026-10-01, 本会话)

> 前置提交 `d80aca4`（三债清偿 + cargo **137/137** 放行全绿 + `-eh legacy` 别名）之后执行翻转（`eh_checked = true` + 3 处文案/completions 同步），全量回归公投裁决。

- **公投 #2 结果**：test_all **305/317（7 failed / 5 canceled，基线 2 failed / 6 canceled）→ 否决**；trace golden **8/8** ✅、eh_diff **7/7** ✅（上次翻红的两处雷区本次通过——守卫收窄与合成符号修复实效）。
- **失败归因（显式 `-eh checked` 逐个探针复现）**：
  - **3 处同根因——生成 C 缺 EH 全局声明**：`c_header_typedefs_test` / `golden/22_c_superset` / `golden/39_complex` → `<stdin>:35:10: use of undeclared identifier '__jeti_eh_flag'`。这些纯 C 超集程序既不 import Foundation 也无 block 字面量，生成 C **不含 runtime.h**，而 `__jeti_eh_flag/__jeti_eh_val` 的声明只存在于 runtime.h——凡"没把 runtime.h 拉进 include 集"的程序 + checked = clang 硬错。**修法（待做，公投 #3 前置）**：codegen include 发射区（`codegen.rs` ~:6551 hosted 区）在 eh_checked 时无条件补发 EH 全局声明（extern 或 `#include <jeti/runtime.h>`），不依赖 Foundation/blocks 启发式。
  - **2 处 checker 失败待归因**：`arc_extreme_test` / `full_syntax_test` → `error: Type checking failed:`（完整报错待取）。
  - 另有 `examples/01_JSONEditor/json_editor.jeti` 失败与 1 个 canceled→failed 转化，未逐一归因。
- **验收矩阵的覆盖缺口（教训）**：阶段 5 矩阵样本全部 import Foundation（trace goldens 更是直接 `#import jeti/runtime.h`），**纯 C 超集文件不在矩阵里**——矩阵全绿但公投翻红，正是公投机制存在的意义；公投 #3 的矩阵须补纯 C 超集样本。
- **回退**：4 处翻转编辑还原（`eh_checked = false` + 文案复原），**`-eh legacy` 别名保留**（零行为变更的前置成果不丢弃）；复验 test_all **309/317（2 failed / 6 canceled）与基线精确一致**。
- **重试前置（公投 #3）**：修复 EH 全局声明发射缺口 + 归因 2 处 checker 失败 + 矩阵补纯 C 超集样本，全绿后再次独立提交翻转。

### 公投 #3 前置修复：EH 全局声明缺口 + `__` 前缀遮蔽 (2026-10-01, 本会话)

> 公投 #2 的 7 个 failed 逐个归因完毕，3 个真缺口全修，1 个是幻影。默认仍是 sjlj——**本节是公投 #3 的前置成果，不等于翻转获批**。

- **✅ 缺口 1：生成 C 缺 EH 全局声明（公投 #2 的 3 处同根因失败）**。`__jeti_eh_flag`/`__jeti_eh_val` 只声明在 `include/jeti/runtime.h`，而纯 C 超集文件（无 Foundation import、无 block 字面量）走不进 include 启发式 → clang `use of undeclared identifier '__jeti_eh_flag'`（`c_header_typedefs_test` / `golden/22_c_superset` / `golden/39_complex`）。修法：`emit_unit_with_headers` 新增 `eh_checked` 参数，hosted include 区在 eh_checked 时**无条件**补发 `#include <jeti/runtime.h>`（带去重，与 `<stdlib.h>` 同款纪律），不依赖 Foundation/blocks 启发式；`pipeline` 穿参 + 另两个调用点（`emit_unit`、vtable_sig 测试）同步。**默认 sjlj 路径行为零变化**（eh_checked=false 时不发射）。守护测试 `codegen::eh_runtime_include_tests` 3 例（纯 C 超集必含 / legacy 必不含 / 已含不重复）。
- **✅ 缺口 2：checker 的 `__` 前缀短路遮蔽 desugar 合成临时类型**（`full_syntax_test` 的 `0:0: argument of type 'int' does not match ...`）。根因：`check_expr` 的 VarRef 臂对任何 `__` 前缀名字**无条件**返回 `Int`（为 `__FILE__`/`__LINE__` 内置宏设计），而 eh hoist 临时 `__jeti_eh_tmp_N` 同样以 `__` 开头、其真实类型**已由 `__auto_type` 推导并注册进 `scope_vars`**——短路发生在查表之前，泛型实参检查因此读到 `int`。修法：把 `scope_vars` 查询**提到 `__`/`&` 短路之前**（真实声明优先；内置宏从不进 scope，零误伤）。⚠️ 该改动影响**所有模式**（不只 checked），故回归必须跑全量 test_all。
- **✅ 缺口 3：eh 的 `aggregate_zero` 对裸 `SEL` 漏判**（`json_editor` checked）。`return_zero` 对 `TypePrim::Sel` 要求 `name.is_some()` 才发 `(T){0}`，但裸 `SEL`（`prim=Sel`、`name=None`，如 `static SEL cmdSel`）落回 `int_expr(0)` → clang `returning 'int' from a function with incompatible result type 'SEL'`。修法：`Sel` 按 prim 单独匹配，不再要求 name。
- **幻影（不是 checked 回归）**：`arc_extreme_test` 在 **legacy+ARC 下同样 rc=1**（7 处显式 `release` 与 ARC 冲突），一直是靠 test_all 的 ARC→MRC retry 兜住的既有限制；`examples/01_JSONEditor` 同理（交互式 + 显式 release）。**教训**：用 `grep -B6` 从 test_all 输出里扒失败清单不可靠（邻近行会带出无关文件名），必须逐个显式 `-eh checked` 复现才可定性。
- **验证**：3 个纯 C 超集 +checked 全 rc=0；`full_syntax_test -eh checked -asm …s` 端到端 rc=0；`json_editor` 转译 rc=0；cargo 全绿（codegen 6 passed，含 3 新例）；**test_all 309/317（2 failed / 6 canceled）与基线精确一致**。
- **⚠️ 仍未做**：阶段 5 验收矩阵本体（10 场景表）尚未把纯 C 超集样本正式纳入；`-eh checked` 仍为显式 opt-in，翻默认需第三次公投。

### ARC 补齐：`goto` 清理 + owned ivar 自动释放 + block 空参数 ✅ (2026-10-02, 本会话)

> 承接"退出表达式求值顺序"修复，清掉报告里列出的两个 ARC 缺口，外加一个 parser 缺陷。

**1. `goto` 跨作用域清理（`crates/arc`）**

C 的 label 是函数级的，`goto` 跳出嵌套块时，被跳过的那些作用域的 owned 局部无人释放（pattern-switch lowering 正是这个形状：arm body 在嵌套 compound，`goto __jeti_swN_end` 跳出去）。

- 用**作用域路径**（从函数体到该语句列表的语句索引序列）判定，而不是单纯深度——同深度的兄弟块必须区分开。
- 关系三分：label 路径是 goto 路径**前缀** → 跳出，按逆序释放离开的作用域；label 是其列表**首条**语句 → 跳入块首（不跳过初始化器，pattern-switch 形状）放行并按最近公共祖先释放；其它 → **拒绝编译**。
- 释放的变量**不**从作用域登记移除：未走该 goto 的路径仍在作用域末尾释放，每条路径恰好一次。
- `@try` 的 `throw_floor` 与 goto 分析相互作用已核对（用例 13）。
- 负例 `tests/negative/arc_goto_into_scope.jeti`（`goto '...' jumps into a nested or sibling scope past its first statement`）。

踩坑：`walk_labels_gotos` 对 `@autoreleasepool`/`@synchronized` 传了**包装语句本身**（非 Compound）→ `walk_labels_gotos_stmt` 克隆后无限自递归 → **转译期栈溢出**（修：传 body）。

**2. owned ivar 自动释放（`crates/codegen`）**

- 语义（比 ObjC 窄，刻意）：**没有**自定义 `dealloc` 的类 → ARC 合成 dealloc wrapper（先链父类入口，再逆序释放本类 owned ivar）；**有**自定义 `dealloc` → ARC 不介入。
- 判定 owned ivar：仅**单级**对象指针；排除 `T **`（C 数组）、weak、函数指针、block、C 标量指针；`id` 计入。`jeti_release` nil 安全，部分初始化安全，逆序释放。
- 判定「自定义 dealloc」必须 `method_owners == 本类`：`method_names` 含**继承**条目，否则任何 `NPObject` 子类都被当作"写了 dealloc"而失去合成。
- **MRC（`-fno-jeti-arc`）下不生成**（`CgUnit.no_arc` 门控）。
- 用例 14（无 dealloc → 合成释放）与 15（有 dealloc 且手动 `NPObject_release(_x)` → 各释放一次）。

踩坑链条：① 先按"所有类都合成"实现 → `tests/space_dodge.jeti` 的 `dealloc` 已用 `NPObject_release(_obs1..3)` 手动释放 → **double free**（ARC 稳定 rc=1，回归）；② 改为"有 dealloc 就跳过"后，`method_names` 里的**继承** `NPObject_dealloc` 让所有子类都被跳过 → ivar 又不释放；③ 用 `method_owners` 区分后才正确。另：`NPObject **`（`NPArray._items`）曾被当作单对象释放，导致 `eh_diff`/`eh_matrix` 崩溃。

**3. `^int(void)` ≡ `^int()`（`crates/parser`）**

block 字面量参数表里的**无名 `void`** 不是参数（C 写法；旧实现会生成名为 `_arg` 的参数 → `^int(void _arg)`，`-Werror` 硬错）。`void *p` 有 `is_pointer`，仍按真参数解析。注意 `peek_next()` 在本 parser 里是**桩**（返回当前 token），不能用做 lookahead——判定放在参数循环内。回归：`tests/block_void_params_test.jeti`。

**4. `@finally` 的独立用例（16/17）**：finally 内再次 throw（向外传播、不重入本层 catch）、finally 内 return（覆盖 try 的返回值）。checked 与 legacy **输出一致**。

**门槛（本会话末）**：`cargo test` **147/147**；`arc_order` **96/96**（16 用例 × 6 组合）；`test_all` **336/344, 0 failed, 0 SUSPECT**；`eh_matrix` **18 场景 0 失败**；`eh_diff` **7/7**；`trace golden` **8/8**；`clang_gcc_stress` **23/23**；`multi_tu` **10/10**；ASan **13/13 clean**（ARC 套件 + legacy full_syntax）。

**清理**：删除工作区三个未跟踪的调试产物 `chk.txt` / `docsurvey.txt` / `aud.plist`。

### ARC 退出表达式求值顺序缺陷 — 已修复 ✅ (2026-10-02, 本会话)

> 症状：`return [[h text] length] - 5;` 在返回表达式求值**之前**释放 `h` → use-after-free（默认 checked 下因 EH 线性化**偶然**掩盖，legacy 下必现）。核心原则：**所有表达式必须先完成求值，再执行离开作用域所需的 ARC release**。

**根因（`crates/arc/src/arc.rs`，三处同族）**

| # | 缺陷 | 表现 |
|---|---|---|
| 1 | `insert_releases_before` 把 release 一律插在退出语句**之前**；`extract_returned_var` 只对顶层 `VarRef` / `MsgSend(VarRef)` 做"转移"判断 → 顶层是 `Binary`（`[[h text] length] - 5`）时不跳过 → 提前释放 | UAF（legacy/default 复现；checked 偶然正确） |
| 2 | 同一个 `MsgSend(VarRef)` 判断**过度**跳过 → `return [w text];` 中 `w` 永不释放 | 泄漏（dealloc 不出现） |
| 3 | return/throw 都 `collect_vars(stack, 0)` + `clear_all`：`@throw` 释放了**函数级**局部，而 handler 在同一函数内、外层局部仍然有效；随后函数尾再次释放 | 双释放（`full_syntax_test` legacy 段 SIGSEGV，ASan 指到 `sec4_control_runtime`） |

**修复规则**（`tests/arc_order/README.md` 有完整表）

1. 退出表达式**读取**待释放局部 → 先求值到临时变量、再 release、最后退出：`__auto_type __jeti_arc_ret_N = <expr>; jeti_release(...); return __jeti_arc_ret_N;`。只在必要时生成临时变量（表达式不读任何待释放局部时直接插 release）。
2. 所有权转移只认两类：裸局部 `return h;`、显式 `return jeti_autorelease(h);` / `return [h autorelease];`（Foundation `NPArray +arrayWithObjects:count:` 依赖后者）。
3. `@throw` 只释放**最近 `@try` 边界以内**的局部（新增 `throw_floor`，随 `@try` 递归下移）。

**配套**：block 捕获感知（`collect_captures_*`，`return blk() - 7;` 中 `h` 被 `blk` 捕获也算"读"）；移除 `clear_all`（它吞掉后续路径的释放 → 泄漏），改用"作用域以无条件退出结尾则不做 scope-end 插入"；退出点之后的语句不可达 → 停止分析（消死代码）。release 顺序保持**逆序**（与 `28_refcount_trace` 既有 golden 一致）。

**同时修掉的 EH 缺陷（`@finally` 早退不执行）**：`@try { return X; } @finally { F; }` 里 finally 被整体跳过（checked 与 legacy 都有）。checked 侧在 `rewrite_try` 用新的 `splice_finally_before_exits` 把 finally 体副本插到 try 体内每个 `return` 之前（嵌套时外层副本落在内层之后 → 顺序 inner→outer ✓）；legacy 侧由 `pub fn splice_finally_exits` 作为独立 pass（pipeline Step 3.92，ARC 之前，使 finally 在 release 之前运行）。

**新增回归套件**：`tests/arc_order/`（8 用例 × 6 组合 = 48，`run_arc_order.sh`）
- 覆盖：顶层 Binary / 返回消息发送 / 复杂二元 / 多局部 / early return / `@throw` 表达式 / `@finally` / block 捕获
- 组合：`{default, checked, legacy} × {ARC, MRC}`
- **48/48 PASS**；**ASan 8/8 clean**（修复前 legacy 下 7 个用例报 UAF、08 全模式报 UAF）

**`test_all.py` 增强**：ARC→MRC retry 现在带**归因**（`PASS_MRC` + 原因）。当前 40 个 retry **全部**为 "by design"（用例显式写 retain/release，ARC 下被 checker 正确拒绝），**0 个 SUSPECT**（即没有因 ARC 注入缺陷而回退的）。另修正 `tests/noarc_test.jeti` 自身不平衡的手动 MM（`retain` ×1 / `release` ×2 → 平衡），它在 ARC 下原本靠 MRC 回退掩盖了测试自身的 over-release。

**门槛（修复后）**：`cargo test --workspace` **147/147**；`test_all` **330/338 passed, 0 failed, 6 canceled, 2 expected-fail, 40 ARC→MRC(by design), 0 SUSPECT**；`eh_matrix` **18 场景 ×3 模式 0 失败**；`trace golden` **8/8**；`eh_diff` **7/7**；`clang_gcc_stress` **23/23**；`multi_tu` **10/10**；ASan **9/9 clean**（8 用例 + `full_syntax_test` legacy）。

**发现的其它缺口（未修，独立议题）**
1. **owned ivar 的自动释放**：`dealloc` 不释放对象类型 ivar（ObjC ARC 会生成）。
2. `^int(void) { }` 生成名为 `void` 的参数（既有 parser 缺陷）；空参 block 写 `^int()`。
3. `goto`（pattern-switch lowering 产生）跨作用域跳出时不触发 ARC 清理 → 潜在**泄漏**（非 UAF）。未列入本次范围。

### `-eh checked` 默认化工程 — 阶段 7：**默认翻转完成** ✅ (2026-10-02, 本会话)

> 三债清偿 + 公投 #3 前置修复之后，本会话完成最后验收并执行翻转。**默认 EH 后端现为 `-eh checked`**；`-eh legacy`（别名 `-eh sjlj`）是完整回退。

**前置修复核实（五项，均确认/补齐了回归测试）**

| # | 修复 | 守护测试 |
|---|---|---|
| 1 | checked 生成 C 无条件拉 `jeti/runtime.h`（纯 C 超集也不缺 `__jeti_eh_flag`/`__jeti_eh_val` 声明） | `codegen` 的 `eh_*` 3 例 + 纯 C 超集探针 rc=0 |
| 2 | checker：`scope_vars` 查询提到 `__` 内置宏短路**之前**（`__jeti_eh_tmp_N` 不再被误判为 `int`） | 新增 `checker::eh_hoist_temp_resolves_to_declared_type_not_the_int_fallback` + `builtin_prefix_fallback_still_applies_when_not_in_scope` |
| 3 | 裸 `SEL` 的零值发射 `(SEL){0}` | `eh::bare_sel_takes_compound_literal_zero` + 端到端探针 |
| 4 | 默认/legacy 行为 = 旧 sjlj | `default_backend_gate`（生成 C 特征：legacy 含 setjmp、不含 flag） |
| 5 | checked 不依赖 hosted-only `setjmp/longjmp` | `eh::checked_desugar_leaves_no_try_node` + 质量统计（checked 恒 0 个 setjmp） |

**修掉一个坏测试（测试缺陷，非产品 bug）**：`eh::checked_desugar_leaves_no_try_node` 的 `expr_uses` 只看表达式顶层，而 desugar 的守卫条件是 `flag == 1 && done == 0`（Binary 节点）→ 断言恒假、单测恒失败。已改为递归遍历（复用 `EffectTable::effect_children`）。

**验收矩阵（新增）**：`tests/eh_matrix/`（`run_eh_matrix.sh` + `gen_quality.sh` + `README.md`）

- **18 场景 × 3 模式**（default / checked / legacy）全 PASS，0 失败场景
- 两道门：`default` 与显式 `-eh checked` **逐字节一致**（翻转保真）；三模式 stdout 无漂移
- **默认值回归测试**：`default_backend_gate` —— 不传 `-eh` 时生成 C 必须是 checked 特征（flag>0、setjmp=0），`-eh legacy` 必须是 sjlj 特征（flag=0、setjmp>0）
- 场景：普通程序 / 单层 @try / 嵌套 @try / @finally 顺序 / rethrow / 跨函数 / ARC / MRC / @autoreleasepool / 多文件（multi_tu **全量**，全部 import Foundation）/ Foundation 大文件 / 纯 C 超集 / `-ffreestanding` / ARM64 裸机（真 `asm_ext.s` + helpers）/ `@await`+异常 / Blocks+异常 / 零异常程序 / 全语法总检（`full_syntax_test` + `-asm`）
- `tests/multi_tu/run_multi_tu.sh` 新增 `JETI_EH_FLAG` 环境变量（默认空 = 零行为变更）

**生成码质量（`gen_quality.sh`）**

- checked **恒 0 个 `setjmp/longjmp`**；`-eh legacy` 的生成 C 与翻转前的 default **逐字节相同**（行数/体积/守卫全同）
- 编译时间 +11~25 ms（最大用例 0.108s→0.134s ≈ +20%）；体积：小文件 +2.7~3.1%，`json_editor` +11.3%
- **守卫密度硬门**：`no_throw_plain.jeti`（无 @try、无 @throw、不内联 Foundation）checked = **0 守卫**（GATE OK）
- ⚠️ **统计口径勘误**：必须 `grep -c '__jeti_eh_flag'`；`grep -c 'if (__jeti_eh_flag'` 因 codegen 生成双括号（`if ((__jeti_eh_flag == 0))`）**恒为 0**。阶段 5 表格"零 @try → 0 守卫"的结论即受此口径影响。修正后的真实情况：**含 Foundation 内联的零 @try 程序有 45 处旗标引用**，它们落在真正会 `@throw` 的 Foundation 方法及其传递闭包调用者上（不是无意义守卫）；零守卫只在"无 @try + 无 @throw + 不内联抛异常方法"时成立。

**失败/取消项归因（切换前 `test_all` 3 failed / 6 canceled → 现 0 failed）**

| 项 | 归因 | 处理 |
|---|---|---|
| `jeti-eh` 单测 `checked_desugar_leaves_no_try_node` | 测试自身遍历缺陷（见上） | 修复测试 |
| `golden/11_multi_file_union/diamond_impl-F.jeti`（no main entry） | `-F` 故意失败样本 | `test_all.py` 新增 `EXPECTED_FAIL` 分类（不计入失败；若意外通过则报失败） |
| `golden/28_refcount_trace/double_release-F.jeti`（故意 over-release） | 同上 | 同上 |
| 6 个 canceled（interactive_game / micrit_rogue / snake_game / tic_tac_toe / tricalc / json_editor） | 交互式程序，无 TTY → 超时 | `test_all.py` 新增**编译替代验收**：超时后自动 transpile+compile+link，成功即记 `CANCELED (… compile+link OK (ARC/MRC))`；6/6 通过 |

**翻转落地**：`crates/jetic/src/pipeline.rs` 新增 `pub const DEFAULT_EH_CHECKED: bool = true;`（`Pipeline::new()` 引用它 + 2 个单测钉死）；`main.rs` 两处 `-eh` 解析改为 `sjlj`/`legacy` **显式置 false**（默认已为 true）；CLI help、`completions/_jetic`、`completions/jetic.fish`、README/CHINESE 的异常语义与裸机小节同步。

**门槛（切换后实测）**：`cargo test --workspace` **147/147**；`test_all` **322/330 passed, 0 failed, 6 canceled, 2 expected-fail**；`eh_diff` **7/7**；trace golden **8/8**；`eh_matrix` **17/17 + gate OK**；`clang_gcc_stress` **23/23**。

**已知限制（不阻塞切换）**

1. `06_cross_frame`：sjlj（legacy）失败、checked 通过 —— 即 README 记录的 sjlj 跨帧限制，外加 `@throws` 逃逸检查只在 sjlj 侧执行。default 与 checked 一致。
2. Foundation 内联会带来守卫（45 处，见上，均落在真实抛异常路径上）。
3. **既有 ARC 顺序缺陷（与 EH 无关，本次未修）**：`return <表达式使用 owned 局部对象>;` 会在表达式求值**之前**插入 `jeti_release` → use-after-free。最小复现（`Holder` 持有 `NPString *_s`、`- (NPString *)text { return _s; }`）：

   ```jeti
   Holder *h = [[Holder alloc] init];
   return [[h text] length] - 5;   // default/legacy: rc=1；checked: rc=0
   ```

   checked 正确是**偶然**——EH 的表达式线性化把调用提到 `jeti_release` 之前，并非修复。属 ARC 范畴，另案处理（详见 `tests/eh_matrix/README.md`）。

### `-eh checked` 差分测试（tests/eh_diff/）— 01–05 全 PASS ✅ (Sep 2026, 本会话)

- **差分 runner**：`./tests/eh_diff/run_eh_diff.sh`——每个用例同时用 jetic `-eh checked` 与 **clang `-fobjc-arc -fobjc-arc-exceptions -framework Foundation`** 编译运行，**stderr 逐行 diff**（基准钉死为 `-fobjc-arc-exceptions`：异常安全 ARC 是语义正确的参照，默认 `-fobjc-arc` 是缺陷参照）。`JETIC=` 环境变量可指定 jetic 二进制。**该目录已在 test_all.py glob 排除**（07 uncaught 故意非 0 退出，standalone 跑是幻影失败）。
- **用例**：01 跨帧释放 / 02 语句中断（表达式后半不执行）/ 03 嵌套 @finally / 04 catch 内 rethrow 传播到外层 / 05 typed catch 链 / 06 block 字面量内 @throw / 07 uncaught abort —— **7/7 全 PASS**（06/07 曾为 KNOWN-FAIL，本会话落地后摘标）。
- **修法 1（ctx-aware desugar，本会话核心）**：try 区内 `@throw` 只置 `flag/val` **不 return**（靠尾段守卫块跳过余下语句，自然落到 catch 臂）；区外 throw 保留 return + 函数尾补 `if(flag) return zero`；catch 臂 isa 命中才清 flag（typed catch 链正确）；含 throw 的循环条件挂 `&& flag==0`；catch/finally 体按 try 区处理（04 rethrow）。
- **修法 2（@try 出口 restore 算符 bug）**：出口恢复发射成 `flag = (saved && flag)`——op 表真相是 `codegen.rs:766: 17 => "&&", 18 => "||"`，文档写 `||` 实发 `&&`，把内层新抛异常抹成 0（03/04/05 同根因）。改 `crates/eh/src/lib.rs` 的 `logical_or` 为 op 18。
- **修法 3（表达式线性化，02）**：语句内嵌套 jeti 消息发送（非 top-level、非所有权家族）hoist 成前置 `__auto_type __jeti_eh_tmp_N` 临时声明再 splice 回语句列表——临时是普通 arming 语句，现有尾段守卫链自动阻断表达式其余部分（`x = [T boom] + [T bar]` 不再执行 `bar`）。**边界**：所有权家族（alloc/new/copy/mutableCopy/init 前缀，`owns_result`）与含它们的表达式一律不 hoist（保住 ARC 的 +1 记账）；C 函数调用不 hoist；新声明自身初始化不 hoist（避免 splice 出的临时被再次 hoist）。
- **连带 checker 容忍**：desugar 发的 `&JETI_CLASS_$_X` 内部引用加 `__` 前缀同款放行规则（`checker/src/lib.rs` 15 行）。
- **连带所有权落点核对（01）**：`@throw e` 降级为 `__jeti_eh_thrown_N = (NPObject *)e; __jeti_eh_val = …; jeti_retain; jeti_autorelease;`（对应 clang `objc_retainAutorelease`），与 `-fobjc-arc-exceptions` 基准逐行一致。
- **06 block 内 @throw（本会话落地）**：M1 的"block 字面量内 @throw 报错"限制解除——block 体编译成独立 C 函数，throw 置旗标后 invoke 正常返回，调用点的尾段守卫（每个含调用语句都守卫）自然接管，**旗标跨 invoke 边界保留**。实现：`check_expr_blocks` 对 block 体以 `in_block=false` 校验（解禁）；`rewrite_expr`/`rewrite_decl` 递归进 block 体（含声明初始化式路径 `ThrowerBlock b = ^{...}`）跑同样的 `rewrite_stmts` 尾段守卫降级；block 内**不加**函数尾 `return zero`（返回类型不是我们的、旗标必须活着出去）。
- **07 uncaught guard（本会话落地）**：`desugar_body` 新增 `is_main` 参数（`func_sym == "main"` 识别）——main 的函数尾守卫从 `return zero` 改为调 runtime 新函数 `jeti_eh_uncaught()`（`runtime.{h,c}`：打印 `*** Terminating app due to uncaught exception of class '<isa->name>'` 后 `abort()`）。此前未捕获异常被静默吞成 exit=0，现 exit=1 + ObjC 措辞。
- **07 差分比较口径**：Foundation 内部类名不可复现（ObjC 报 toll-free bridging 名 `__NSCFConstantString`，jeti 报真实类名 `NPString`）——runner 对 07 把 `class '...'` 归一为 `class 'X'` 只比措辞前缀（README 本就约定"栈回溯不可复现"，类名同属实现细节）。06/07 的 KNOWN-FAIL 标记已从 `.jeti` 摘除。
- **bridge header 旗标守卫（本会话落地）**：`emit_bridge_header` 的 wrapper（实例/类方法两路）在调用后注入 `if (__jeti_eh_flag) { jeti_eh_uncaught(); }`（非 void 返回先落 `__jeti_ret` 再守卫再 return）——`-eh checked` 下 `@throw` 编译成置旗标 + return zero，纯 C 调用方不查旗标会把异常**静默吞掉**；现在升级为 ObjC 措辞 abort（复用 07 刚落的 `jeti_eh_uncaught`，runtime.h 本就在桥接头里 include，零新依赖）。端到端已验证：C caller 调 `ping(21)` 正常返 42，调 `ping(-1)` → `*** Terminating app due to uncaught exception of class 'NPString'` + exit=134（SIGABRT）。checked 在边界前已结算全部帧，abort 是安全降级。
- **回归**：cargo test 55/55、test_all **220/230**（3 个 `-F` 既有失败不变）、差分 **pass=7 fail=0 kfail=0**。

### 真 variadic 方法支持 — Implemented ✅ (Sep 2026, 本会话)

- **golden 测试**：`tests/golden/35_variadic_method/`（`.jeti`+`.out`，test_all 纳入）——覆盖单段 selector（va_arg 求和）、多段 selector（`format:count:values:, ...`）、va_list 转发（vprintf 封装，`stringWithFormat:` 的标准实现形态）、继承（子类沿 vtable 槽位调父类 variadic）、嵌套消息发送多实参、NPString 返回值拼接、复合条件（`while ((v = va_arg(ap,int)) != 0)`，见下方 va_arg 括号修复）；`.out` 已验证确定性（重跑一致 + ARC/MRC 输出相同）。
- **方案（无特判，真支持）**：variadic 方法编译成**真 C variadic 函数**——jeti 没有 msgSend、纯静态 vtable 派发，故 vtable fn-ptr cast 必须带 `...` 与签名精确匹配（`objc_msgSend` cast 先例），无运行时兜底。链路：`- (void)log:(const char *)fmt, ...;`（parser 接受 keyword 与 C-style 两种形态的 `, ...`）→ 方法体 `va_start/va_arg/va_end` 直接可用（就是 C）→ 消息发送 `[l log:"...", a, b]` 逗号实参透传给 variadic 调用点。
- **改动链**：`CstDeclData::Method`/`AstDeclData::Method` 加 `has_variadic`（parser 两处 CST——`crates/parser/src/cst.rs` 与 `crates/cst`——elaborator 透传）；codegen `CgClassMeta`/`ClassInfo` 加 `method_variadic` 平行向量（**五处同步点**：pre-pass 注册 push、主 pass 注册 push、父类合并重建+回写、`CgClassMeta` 转换、构造点初始化——漏任何一处 `method_variadic` 静默为空，vtable cast 就缺 `...`，这是本次调试的主要坑）；函数发射 `is_variadic` 三处硬编码替换；元数据 fn-ptr 类型构造（`METHOD_METADATA`/`CLASS_METHOD_METADATA`）带 `, ...`。
- **探针验证**：单段 selector `hello 42 world`；多段 selector + 继承（Child 继承 Base 的 variadic `sum:`）+ 嵌套消息发送（`[c sum:[c sum:1,1,0], 100, 0]`）全对（child sum=4 / base sum=11 / nested=3）。
- **边界（如实）**：ARC 所有权对 variadic 实参不做特殊记账（与 ObjC 相同——按可见签名前 N 个参数推断，`...` 部分用户自理）；checker 不检查 `...` 实参类型（与 C 一致，clang 兜底）。
- ✅ **类方法 variadic 的 meta-vtable 缺口已修（Sep 2026, 本会话）**：meta-vtable **struct 定义**的 fn-ptr 成员（`codegen.rs` "Meta vtable struct definitions" 循环）未带 `...`，与 `CLASS_METHOD_METADATA` cast 侧（已带）不一致 → meta-vtable 实例的指定初始化器报 `incompatible function pointer types`。修法：该循环读 `method_variadic`（同一数据源，一行改动），成员类型补 `, ...`。探针（类方法 `+ (int)sum:(int)first, ...` + `va_arg` 求和）端到端 `sum=6` 正确；回归 cargo 59/59、test_all 225/235（3 个既有失败不变）。

### 格式串-实参静态检查 — Implemented ✅ (Sep 2026, 本会话)

- ✅ **checker 编译期格式检查**（`crates/checker/src/lib.rs`，白名单方法：`NPLog` + `stringWithFormat:`/`initWithFormat:`/`appendFormat:`/`stringByAppendingFormat:`）：格式串为 `@"..."` 字面量时静态扫描，`check_warning` 报告（`-Werror` 可提升），不改 AST。
- ✅ **完整格式符-实参匹配**（`check_format_args` + 五类 `FormatArgKind`：Object/Int/Float/Str/Ptr）：
  - **类型不匹配**：`%d` 配浮点、`%f` 配整数、`%s` 配对象（`— use %@ for NPString objects` 提示）、`%@` 配标量（`— use %d/%s/%f for scalars` 提示）；`%p` 接受任何指针/对象（打印地址合法）；整数族宽度/符号性不判（C 同样宽松）。
  - **数量双向**：spec 多于实参（读垃圾）与实参多于 spec（静默忽略）都报；数量不一致时跳过逐位配对（配对已无意义）。
  - **保守分类**（`arg_format_kind`）：只判可证事实——字面量（Int/Float/Char/Bool→Int、Float→Float、裸串→Str、`@"..."`→Object）、Cast（按目标类型）、VarRef（查 `scope_vars` 已知类型）。调用结果一律不判（checker 给 FuncCall/MsgSend 标 int/id，判了必误报）。`type_format_kind`：数组退化按指针（`char buf[8]` 配 `%s` 合法）、`char*`→Str、标量族→Int/Float、`Id/Instancetype/Class`→Object、`Named` 指针→Object、非指针 Named/SEL/Void 不判。
  - **落点**：FuncCall（NPLog，`args[1..]`）与 MsgSend（FORMAT_MSGSENDS 白名单，`args[1..]`）两臂挂载，与既有裸串参数检查同处。
- ✅ **配套修复（前置依赖）——va_arg 括号发射 bug**：`while ((v = va_arg(ap,int)) != 0)` 此前生成坏 C（`va_arg(ap, int) != 0` 的括号被吞进类型位置）——codegen 已修（va_arg 调用发射整体加括号），golden/35 已回改复合条件形态端到端验证。原"测试侧规避" workaround 作废，测试不再需要绕。
- ✅ **测试**：checker 单测 6 个（`%@` 配 int 警告/配对象放行/数量不匹配/标量格式正确）；探针实测 4 类新 warning 精确命中、正例零误报（`%p` 配对象、已知类型局部变量、`%c`），运行期垃圾值（`%d` 配 3.14 → `int is 1610612736`）实证编译期拦截的价值。
- ✅ **回归**：cargo test **59/59**、test_all **225/235**（3 个既有失败不变：两个无 main 库文件 + `double_release.jeti` 故意崩溃）、探针重复运行输出一致。
- **回归**：cargo test 全绿（checker 测试构造点补 `has_variadic: false`）、test_all **220/230**（3 个 `-F` 既有失败不变）。

### `-l` 链接旗标 + `-I/-L/-l` 连写形式 — Implemented ✅ (Sep 2026, 本会话)

- **动机**：用户编 gmp 程序时 `-L<path>` 连写不识别、`-lgmp` 被 run 模式静默吞成**程序参数**（编译过了但链接期 `symbol(s) not found`），错误归属混乱——jetic 自己的旗标界面应当自己解析，错误报在 jetic 而非 clang。
- **`-l <lib>` 旗标**（新）：run/compile 模式下解析进 `libs`，`compile_to_binary` 发射 `-l<name>` 给 clang。**发射位置有讲究**：必须在输入文件之后（链接器解析顺序：库要跟在被解析符号的对象后面），落在链接段尾部（`-lm` 之后、`-w` 之前），且 `-L` 目录先于 `-l` 发射。⚠️ 首版把 `-l` 放在 `-I` 段后 → `ld: library 'gmp' not found`（输入在前库在后，符号解析不到）。
- **连写形式**：`-I/path`、`-L/path`、`-lgmp`（clang/GCC 惯例）全部支持；`-l` 连写要求第 3 个字符是字母数字（避免与 `-last` 类歧义旗标冲突，歧义用空格形式解决）。
- **`-L` 目录同时传 clang**：此前 `lib_dirs` 只被 `find_libjeti` 搜索、从不发射 `-L`——现在用户 `-L` 目录真正到达链接器。
- **help/flag 表/completions 同步**：`jetic_flags()` 表、手动 help、`completions/_jetic`（两处）、`completions/jetic.bash`（opts 行 + `-l` case 分支）。
- **验证**：gmp 用例（mpz_ui_pow_ui）连写与分开写两种形态均输出正确；错误库名 `-lnosuchlib` 响亮报 `ld: library not found`；未知旗标报 jetic 错。
- **回归**：cargo test 全绿、test_all **220/231**——第 4 个失败是 `tests/asdf.jeti`（用户未跟踪的 gmp 试验文件，需 `-I/-lgmp` 外部旗标，standalone 编译必失败，幻影失败非回归；移出 tests/ 或加 `-F` 后缀即可消除）。


- **既有失败用 `-F` 后缀标记**（文件名末尾，如 `diamond_impl-F.jeti`）：一眼可辨，且不需另建排除机制。test_all.py 仍会跑到它们（3 个 FAIL 是已知基线）。
- **`tests/stress/` 已在 test_all.py 的 glob 中排除**（同 `multi_tu` / `25_freestanding` 理由）：三套各有专属 `build.sh`，需要各自 flag（`-fno-libc` / `-asm asm_host.s`），且 `interop/lib.jeti` 是无 main 的库文件。必须用它们自己的 runner 跑，单文件编译必然产生幻影失败。
- `golden/11_multi_file_union/diamond_impl-F.jeti` — FAIL (no main entry, library-style multi-file test，被 `cross_file_test.jeti` #import)
- **Variadic selector 支持状态（待办，用户后续修复）**——语法已支持（`sel:a, b, c, nil`），缺的是库与 codegen：
  - **库缺失**：`NPSet`/`NPMutableSet`/`NPDictionary`/`NPMutableDictionary`/`NPOrderedSet`/`NSPredicate` 这些类根本不存在；`NPString` 的 `stringWithFormat:`/`initWithFormat:`/`stringByAppendingFormat:` 与 `NPMutableString` 的 `appendFormat:` 方法未声明；`NPArray`/`NPMutableArray` 只有 `initWithObjects:count:`（无 variadic `initWithObjects:`）；无 `NSAssert` 宏。
  - **codegen 缺失**：Jeti 无运行时 `va_list`，真正的 variadic 方法无法用 Jeti 实现，只能按 selector 特判落到运行时 helper。目前只有 `arrayWithObjects:` 一个特判（desugar 成 `@[...]`），且它丢弃 receiver——`[NPMutableArray arrayWithObjects:...]` 会静默得到不可变 `NPArray`。
  - **待修**：(a) `[NPMutableArray arrayWithObjects:...]` 保留可变性；(b) 未知 variadic selector 给出清晰 `error` 而非生成坏 C；(c) 视需要为 `stringWithFormat:` 系列复用 `NPLog` 的编译期 `%@` 展开。

### Header-Only Class Linking — Implemented ✅ (Aug 2026)
- ✅ A main file may `#import` only `.jth` declaration headers and link against a separately compiled `.jeti` implementation file (standard C header/source split). `examples/03_LibUI/libui_demo.jeti` now imports only `LibUI.jth`; `examples/03_LibUI/include/LibUI.jeti` is transpiled+compiled separately and linked in (see `examples/03_LibUI/run_libui.sh`).
- ✅ Duplicate-symbol fix: generated class metadata definitions are now emitted as **weak symbols** — `__attribute__((weak))` on vtable instances, meta-vtable instances, `*_getClass`, `jeti_meta_init`, `jeti_string_from_cstr`, and all function definitions with bodies (`emit_decl` in `codegen.rs`). The linker coalesces the identical per-TU copies (base-class methods from `Foundation/NPObject.jth` appear in every TU). Single-file output is unchanged (weak is invisible with a single definition).
- ✅ AST now tracks `is_implementation` on `AstDeclData::Class` (elaborator sets it from `CstDeclKind::ClassImplementation`/`CategoryImplementation`); `CgClassMeta.has_impl` computed from it (used for future work; not required by the weak-symbol fix).

### Shell Completions via clap_complete — Implemented ✅ (Aug 2026)
- ✅ `jetic --gen-completions <shell>` emits a completion script for **zsh / bash / fish / powershell / elvish**, built with `clap_complete` from `clap_command()` in `crates/jetic/src/main.rs`.
- ⚠️ jetic's real CLI uses **single-dash** long flags (`-rewrite-jeti`, `-fno-jeti-arc`, `-arch`, `-asm`). clap normalizes these to `--flag`, so `gen_completions` post-processes the generated script via `DOUBLE_TO_SINGLE` (`--rewrite-jeti`→`-rewrite-jeti`, `--output`→`-o`, etc.) and dedups consecutive identical lines.
- ✅ Generated scripts are self-contained (flags embedded, no runtime binary call). Verified in bash: `-f<TAB>` → `-fjeti-arc -fno-jeti-arc -fno-checker -fno-libc`, `--v<TAB>` → `--verbose --version`, `-a<TAB>` → `-asm -arch`.
- ✅ Scripts live in `completions/` (`_jetic`, `jetic.bash`, `jetic.fish`). `crates/jetic/build.rs` copies them into `target/<profile>/completions/`; `build-all.sh` includes them in each platform bundle + `target/` archives; `install-pkg.sh` installs them to `$PREFIX/share/jetic/completions/` and prints registration hints (zsh fpath / bash source / fish source).
- ✅ Legacy hand-written `--autocomplete=<cmdline>` mode + `interactive_flag_picker` (`-`/`--` menu) retained in main.rs.

### Tests Status
- Overall: **195/206 pass** (3 pre‑existing failures: `diamond_impl.jeti` + `mega_fusion/mega_types.jeti` no-main library tests, `golden/28_refcount_trace/double_release.jeti` intentional over-release crash; 8 canceled interactive). Includes `npmutablearray_test.jeti` (ARC + MRC), `NPLog` signature change, `class_forward_decl.jeti` re-enabled, golden `13_foundation/06_nparray` + `07_npmutablearray`, `grand_feature_stress_test.jeti`, `nil_messaging_test.jeti`, `q.jeti`. Plus `clang_gcc_stress/run_stress.sh` 23/23 PASS (gaps 01–17), Rust unit tests 41/41, trace goldens 7/8 (`arc_inject` pre-existing).

### Refcount Trace Golden Suite — `28_refcount_trace` (Aug 2026)
- ✅ New golden dir `tests/golden/28_refcount_trace/`: 8 `.jeti`+`.out` pairs. Unlike other golden dirs, the `.out` files are **not** program output — they are `jetic -trace-refcount -trace-no-color -trace-max-iters 2` snapshots.
- ✅ Runner: `./tests/golden/28_refcount_trace/run_trace_golden.sh` (diffs each trace against `.out`; `JETIC=` env overrides the binary, default `target/release/jetic`).
- ✅ Coverage (verified correct by hand):
  - `retain_release` — retain ×3 / release ×4 → 1→2→3→4→3→2→1→0, freed.
  - `double_release` — release past 0 → `! double-release / over-released …: -1`, Summary red `over-released — count negative`.
  - `leak_detect` — one object unreleased → Summary red `still alive — possible leak`.
  - `arc_inject` — ARC mode: `[Item new]` ×3, scope end injects 3× `jeti_release` → all freed (proves ARC auto-release is traced).
  - `loop_iterations` — ARC loop: fresh `Item#1`/`Item#2` per `for iter`, each `jeti_release`→0.
  - `alias_shared` — `Item *alias = shared` shares the same object (`retain`→2, two `release`→0).
  - `if_else_branch` — `── if ──`/`── else ──` branch state cloning (both end at 0).
  - `autoreleasepool_nested` — nested pools with manual `autorelease` inside `@noarc`: each `pool pop` frees exactly its own object.
- ✅ All `.jeti` files are valid, runnable Jeti programs (import `jeti/runtime.h` + `Foundation/NPObject.{nh,np}` only, keeping trace output free of Foundation noise) — they pass the normal `jetic run` path used by `test_all.py`'s glob.
- ⚠️ `! jeti_release on untracked self` lines at 32:5 / 60:5 come from the inlined `NPObject.jeti` `-release`/`-dealloc` bodies (releasing untracked `self`) — expected noise, part of the golden snapshot.

### Namespace `@class` Forward Decl + `@using` Conflict Detection — Implemented (Aug 2026)
- ✅ **`@class` forward declarations now emit C**: previously `@class Player;` registered a symbol in the binder but the elaborator dropped it and codegen never emitted a declaration, so using `Player *` in method signatures produced `unknown type name 'Game__Player'`. Now the elaborator converts `CstDeclData::Forward` → `AstDeclKind::ForwardClass` / `AstDeclData::ForwardClass { names }` (namespace-prefixed via `ns_fqn`); codegen emits `struct Game__Player;` + `typedef struct Game__Player Game__Player;` at the top of the file, skipping names that have a full class definition in the unit and skipping `jeti_root`/`NPObject`. Works at top level and inside `@namespace` blocks.
- ✅ **`@using` conflict detection**: previously `add_using` blindly pushed to `using_list` and `find_using` returned the first match, so two `@using` entries importing the same short name compiled silently. Now the binder's `Using` handler checks (before registering) whether the short name (a) already exists in `using_list` → `ambiguous import: 'X' imported from both 'A' and 'B'`, or (b) collides with an existing class/protocol/typedef symbol → `'X' conflicts with an existing symbol`. Both emit `error:line:col:` and abort binding.
- ✅ Cross-namespace inheritance (`@interface HUD : Engine::Graphics::Renderable`) was already supported and is now covered by a dedicated test.
- ✅ Test: `tests/namespace_forward_class_test.jeti` (forward `@class` in namespace, cross-ns inheritance, `@using` short name, message sends across namespaces).

### File Extension Renames — Final (Aug 2026)
- ✅ Reverted jeti headers **`.h` → `.jth`** (an earlier session had renamed `.jth` → `.h`; that broke the ObjC `Foundation.h`-collision-free guarantee and would let system `#include` clash with jeti headers). Decision: headers stay **`.jth`**, implementations stay **`.jeti`**.
- ✅ jetic preprocessor `is_jeti_import` accepts jeti header/impl extensions — **`.jth` and `.jeti` only** (`.h`/`.jeti` removed).
- ✅ `__JETI__` macro: jetic now always defines `__JETI__` for all backends (`pipeline.rs` extra_macros). This enables headers that are shared with plain C compilers to guard objc-style syntax behind `#ifdef __JETI__` (jetic inlines the jeti branch; a C compiler sees the `#else` C-compatible branch). Internal-only headers need no guard.
- ✅ Verified both scenarios: (1) jetic-internal header with `#ifdef __JETI__` → jeti branch generated, `compute(21)=42`; (2) same header seen directly by `clang -c` (no `__JETI__`) → C branch compiles clean.
- ✅ Renamed ~172 implementation files `.jeti` → `.jeti` (and earlier `.jeti`→`.jeti`→`.jeti`) with all `#import` references, `test_all.py` globs, build `.sh` scripts, `.vscode` extension, and docs updated; no `#import ... .jeti` references gave ambiguous matches.
- ✅ Full regression green after renames: run_stress 23/23, test_all 160/169 (2 pre-existing no-main, 7 interactive canceled), cargo unit 26/26.
- **Golden freestanding test** `tests/golden/25_freestanding/` — verified via `./build.sh` (not in default suite because it requires `-rewrite-jeti -fno-libc` + host clang + helpers.c). Demonstrates @namespace, @interface (implicit root), @try/@catch/@finally, @selector, inline asm, C-style cast on bare metal.
- ARC retry: tests that fail with ARC are automatically retried with `-fno-jeti-arc` (MRC fallback)

### `@noarc { }` — Block-level MRC (Aug 2026) ✅
- **Purpose**: In ARC mode, manual memory management is forbidden by the checker (`explicit 'retain'/'release'/'dealloc'/'autorelease' not allowed in ARC mode`). `@noarc { }` scopes a block where the programmer manages memory manually — the block-level analogue of `-fno-jeti-arc` (and clang's `-fno-objc-arc`). Named `@noarc` (not `@unsafe`) to match the existing `-fno-jeti-arc` CLI flag and to avoid over-promising a Rust-style safety model — the name makes no claim that code outside the block is "safe".
- **Lexer/parser**: `@noarc` → `KeywordKind::AtNoArc` → `CstStmtKind::NoArc` / `CstStmtData::NoArc(Box<CstStmt>)` → `AstStmtKind::NoArc` / `AstStmtData::NoArc(Box<AstStmt>)`.
- **ARC analyzer** (`crates/arc/src/arc.rs`): `@noarc` blocks are skipped entirely — no release injection.
- **Checker** (`crates/checker/src/lib.rs`): `in_noarc` flag toggled while walking a `NoArc` body; `retain`/`release`/`dealloc`/`autorelease` outside `@noarc` (and outside the implementation of the matching runtime method) error in ARC mode. `Checker.no_arc` is set from pipeline's `-fno-jeti-arc`.
- **Codegen** (`crates/codegen/src/codegen.rs`): emits the body as-is, no ARC injection.
- **Foundation**: NPString/NPMutableString convenience constructors (`+stringWithUTF8String:`/`+stringWithString:`) wrap their deliberate `autorelease` in `@noarc { }`, since Jeti's ARC does not auto-inject autorelease-on-return.
- **Test**: `tests/noarc_test.jeti`.

### ARC Rewrite — RefVal State Machine (Aug 2026) ✅
- ✅ **Scope-stack 模型替换为 RefVal 状态机**：旧模型用 `Vec<Scope>` 追踪"哪些变量要释放"，新模型用 `HashMap<String, RefState>` 追踪每个对象的精确引用计数状态。
- ✅ **RefState 状态机**（`crates/arc/src/arc.rs`）：
  - `NotOwned` — +0，当前函数不拥有此对象
  - `Owned(u32)` — +n，当前函数拥有，必须在作用域结束释放 n 次
  - `Released` — 已释放
  - `Unknown` — 分支合并后无法确定状态（保守地不释放，避免 double-free）
  - `Error` — 错误状态（over-release 等）
- ✅ **路径敏感分支合并**：`if/else` 分支结束时，两边状态取交集（`merge_states`）。分歧 → `Unknown`（保守不释放，宁可 leak 不 double-free）。
- ✅ **retain/release 计数追踪**：`[obj retain]` 递增计数，`[obj release]` 递减。`jeti_retain`/`jeti_release` 函数调用也正确处理。
- ✅ **retain on NotOwned**：`jeti_retain(obj)` 在 NotOwned 对象上 → 变为 Owned(1)（调用者主动 retain 了一个不拥有的对象）。
- ✅ **release on NotOwned**：`jeti_release(obj)` 在 NotOwned/Released 对象上 → Error（over-release 检测）。
- ✅ **return/throw escape**：return/throw 的对象被标记为 escaped，scope-end 释放时跳过它。
- ✅ **break/continue**：只释放最近循环作用域内声明的变量。
- ✅ **`@noarc` 块完全跳过**：状态机不进入 `@noarc` 块，用户手动管理内存。
- ✅ **回归验证**：7 个 golden ARC 测试 + 8 个 ARC 专项测试全部通过，test_all 基线无回归（151/162 + 41 unit）。

### 与 clang RetainCountChecker 的对比
- **clang 为什么复杂**：ObjC 是动态语言（`objc_msgSend` 运行时查找方法），需要：
  1. `RetainSummaryManager` — 维护庞大的类+selector 摘要表（`lib/Analysis/RetainSummaryManager.cpp`）
  2. `ObjCARCInstKind` — 将每个 IR 指令分类为 retain/release/autorelease/use（`lib/Analysis/ObjCARCInstKind.cpp`）
  3. `RefVal` 状态机（`RetainCountChecker.h`）— 路径敏感符号执行追踪每个对象
  4. `__attribute__((ns_returns_retained))` 等注解标记方法所有权
  5. `objc_retainAutoreleasedReturnValue` 配对优化 — 跨函数调用约定打标
- **Jeti 为什么简单很多**：Jeti 是**静态 vtable 派发**，编译期已知方法签名和调用目标：
  1. **不需要**摘要表 — `ownership_for_method` 的 alloc/new/copy/init 命名约定 + init 链追踪已足够
  2. **不需要**指令分类 — AST 上直接识别 `MsgSend(selector=="retain")` 等
  3. **不需要**别名分析 — 变量就是名字，没有 `id` 动态类型问题
  4. **不需要**跨函数调用约定优化 — 编译器知道调用目标，不需要运行时 intrinsic
  5. 状态机直接从 `ownership_for_expr` 查询所有权（`crates/ownership/src/ownership.rs`）
- **clang 源码位置**（桌面）：
  - `lib/StaticAnalyzer/Checkers/RetainCountChecker/RetainCountChecker.{h,cpp}` — 核心状态机
  - `lib/Analysis/RetainSummaryManager.{h,cpp}` — 方法摘要表
  - `include/clang/Analysis/RetainSummaryManager.h` — 摘要类型定义（`ArgEffect`/`RetEffect`/`RetainSummary`）

### 错误检测 + Checker 完善计划（Aug 2026）
借鉴 clang 的分层设计，全部 Jeti 化（静态 vtable 派发，不依赖 ObjC 运行时）。

**总体架构对应：**

| clang | Jeti |
|-------|------|
| `Parse/` 错误恢复（`SkipUntil`） | `crates/parser` — 替换 `panic_mode` |
| `SemaDeclObjC.cpp` | `checker` — 声明语义检查 |
| `SemaExprObjC.cpp` | `checker` — 表达式语义检查 |
| `SemaObjCProperty.cpp` | `checker` — `@property` 验证 |
| `Sema::Namespace` (C++) | `binder`/`elaborator` — 命名空间检查 |
| `DiagnosticEngine` | `pipeline` — 统一错误收集/格式化 |

**阶段 1：Parser 错误恢复（最影响日常体验）**
- **现状**：`panic_mode` 遇到第一个错误就停止，一次编译只报一个错
- **clang 做法**：`Parser::SkipUntil` 跳到分号/大括号等恢复点，继续解析
- **Jeti 化方案**：

| 场景 | 恢复 token | 效果 |
|------|-----------|------|
| 缺 `;` | 跳到下一个 `;` 或 `}` | 继续解析后面的语句 |
| 缺 `)` | 跳到下一个 `)` 或 `;` | 继续解析表达式 |
| 缺 `]` | 跳到下一个 `]` 或 `;` | 继续解析消息发送 |
| 缺 `}` | 计数器匹配，跳到最近的 `}` | 继续解析外层 |
| 缺 `@end` | 在文件末尾报错 | 不阻塞后续解析 |

**阶段 2：结构错误检测**
- **clang 做法**：`Parser::ParseObjCAtEnd` 检查 `@interface/@implementation/@protocol` 是否匹配
- **Jeti 化**：增加 `@interface`/`@implementation`/`@protocol`/`@namespace` 的嵌套栈追踪

| 检测 | 实现位置 |
|------|---------|
| `@interface` 缺 `@end` | parser 在文件末尾检查栈 |
| `@implementation` 缺 `@end` | 同上 |
| `@protocol` 缺 `@end` | 同上 |
| `@namespace` 未闭合 `@endnamespace` | parser 在文件末尾检查栈 |
| 嵌套 `@interface` 在另一个里 | parser 禁止嵌套声明 |
| 不匹配的 `[]` / `()` / `{}` | parser 栈追踪 |

**阶段 3：Checker 语义检查（对应 clang 的 `SemaObjC`）**
- **现状**：checker 已有类型兼容性、协议一致性、裸串检查
- **需要新增**（参考 `SemaDeclObjC.cpp` + `SemaObjCProperty.cpp` + `SemaExprObjC.cpp`）：

| 检查项 | clang 文件 | Jeti 位置 |
|--------|-----------|-----------|
| 方法签名 `@interface` 与 `@implementation` 匹配 | `SemaDeclObjC.cpp` | `elaborator`（已有部分） |
| `@property` 属性冲突（同时 assign+retain） | `SemaObjCProperty.cpp` | `checker` |
| `@synthesize` 指向不存在的 ivar | `SemaObjCProperty.cpp` | `checker` |
| 协议要求的方法未实现 | `SemaDeclObjC.cpp` | `checker`（已有部分） |
| 类方法/实例方法混淆 | `SemaExprObjC.cpp` | `checker` |
| `instancetype` 用于非 init 方法 | `SemaExprObjC.cpp` | `checker` |
| 分类 (category) 名称与已有分类冲突 | `SemaDeclObjC.cpp` | `elaborator` |
| 协议循环继承 | `SemaDeclObjC.cpp` | `checker` |
| 类循环继承 | `SemaDeclObjC.cpp` | `checker` |
| 方法参数类型不匹配（消息发送时） | `SemaExprObjC.cpp` | `checker` |
| 块字面量参数类型检查 | `SemaExprObjC.cpp` | `checker` |
| `@selector` 引用不存在的方法 | `SemaExprObjC.cpp` | `checker` |

**阶段 4：语法建议（clang 的 `-W` 诊断 + Typo 修正）**
- **clang 做法**：`Sema::CorrectTypo` 在未定义名称时查找最接近的已有名称
- **Jeti 化**：编辑距离 + 符号表查找

| 特性 | 实现 |
|------|------|
| 未定义变量 → 建议最接近的已定义变量 | checker + 编辑距离 |
| 未定义方法 → 建议同类中的相似方法 | checker + 符号表 |
| 未定义类 → 建议已导入的相似类名 | checker + 符号表 |
| 未定义协议 → 建议已导入的相似协议名 | checker + 符号表 |

**阶段 5：死代码/未使用变量 warning**
- **clang 做法**：`-Wunused-variable`、`-Wunused-function`、`-Wunreachable-code`
- **Jeti 化**：在 checker 中遍历 AST

| 检测 | 触发条件 | 状态 |
|------|---------|------|
| 未使用变量 | 变量声明后从未被引用 | ✅ 已实现（`var_decls` 追踪 + `check_unused_vars`） |
| 不可达代码 | return/break/continue/throw 后的语句 | ✅ 已实现（`check_stmt` 的 Compound 里追踪 `unreachable` 标志） |
| 空 `@try`/`@catch` 块 | 没有语句的块 | ✅ 已实现（`is_empty_compound`） |
| 多余的 `@synthesize` | 自动合成的属性不需要手动 @synthesize | ⏳ 待实现 |

**阶段 6：C 语法检测（⚠️ 铁律：Jeti 是 C 超集，绝不能打断 Jeti 语法）**
- clang 的 C 检测（`-Wconversion`、`-Wsign-compare`、`-Wincompatible-pointer-types` 等）借鉴时要极其小心：
  - **Jeti 是 C 超集**：`[obj msg]`、`@interface`、`@property`、`@namespace` 等 Jeti 特有语法在 C 检测中必须被视为合法
  - **只加不改**：C 检测只能**新增** warning/error，绝不能**更改** Jeti 已有的合法语法解析路径
  - **白名单**：C 检测应该作用于纯 C 表达式/语句（`int a = b + c;`、指针转换等），跳过高层级 Jeti 构造（消息发送、特性语法）
  - **回归守护**：每次加 C 检测后必须跑 `test_all.py`，确认 192/202 基线无回归
- 候选检测项（参考 clang `-W` 系列，全部 Jeti 化）：
  | clang 警告 | Jeti 落地 | 状态 |
  |-----------|----------|------|
  | `-Wconversion`（隐式类型转换） | `check_conversion`（`crates/checker`）：浮点→整数精度丢失、整数降精度（long→int）、同宽符号性转换；Int 字面量在目标范围内则豁免（`literal_in_range`，避免 `unsigned int u = 5` 误报） | ✅ |
  | `-Wsign-compare`（符号比较） | `check_sign_compare`：二元比较左右操作数符号性不同则警告（signed vs unsigned）；只作用于纯 C 标量，跳过消息发送 | ✅ |
  | `-Wreturn-type`（缺 return） | `has_return_stmt` + `check_decl`：非 void 函数/方法体无任何 return 则警告（init 方法豁免） | ✅ |
  | `-Wunused-function`（未使用函数） | `declared_functions` + `called_functions`（FuncCall 记录调用）：顶层非 `main` 函数从未被调用则警告 | ✅ |
  | `-Wuninitialized`（未初始化变量） | `var_decls` 扩展 `(line, used, assigned)`：声明带 init 或之前有赋值才 `assigned=true`；VarRef 读取 `assigned=false` 则警告。`x = x + 1` 等 RHS 读取在标记 LHS 之前检查 | ✅ |
- **Jeti 超集安全护栏**（`check_conversion`/`check_sign_compare` 内部的跳过列表）：
  - 对象/指针类型（Id/Class/Sel/Instancetype/Named-pointer）一律跳过
  - `MsgSend` 表达式（`[obj foo]`）跳过 —— 返回 `id`，不做标量转换判断
  - 泛型类型 receiver（`[LogBuffer<LogEntry*> alloc]`）和命名空间限定名（`A::B`）跳过 —— 它们是类型引用不是变量
  - 全部是 **warning**（紫色，非 error），永不改变 AST 或解析路径
- ⚠️ **已知噪音**：Foundation 内联代码（不 import 时会展开 NPMutableArray 等）内部会产生少量 `sign-compare`/`unused variable` warning 混入输出；功能正确，纯体验问题

### Weak References — Implemented ✅
- ✅ Runtime side table: `jeti_weak_register` / `jeti_weak_unregister` / `jeti_weak_clear_all` in `include/jeti/runtime.c`
- ✅ `jeti_release` calls `jeti_weak_clear_all(target)` before `free(target)`
- ✅ Variable `__weak` decl: emits `__attribute__((cleanup(jeti_weak_auto_cleanup)))` + `jeti_weak_register` after init
- ✅ Variable `__weak` **assign** (Sep 2026): `rewrite_weak_assigns` pass（函数粒度，仿 `__block` 捕获改写）把对弱局部的普通赋值改写为 `__auto_type` 临时求值一次 → `jeti_weakUnregister` → assign → `jeti_weakRegister`——此前赋值绕过弱表，槽位仍挂在旧目标（常为初始 NULL）上，源对象销毁不清零（`__weak` 静默退化成普通指针）。M1 限制：block 体内不改写；嵌在更大表达式里的赋值（`if ((w = x))`）不改写。
- ✅ Property `(weak)` setter: emits `jeti_weak_unregister` → assign → `jeti_weak_register`
- ✅ `build.rs` rerun-if-changed for `runtime.c`

### Refcount Trace (`-trace-refcount`) — Implemented ✅ (Aug 2026)
- ✅ **New crate `crates/trace`** (`jeti-trace`): static reference-count simulator. Runs on the AST **after** ARC analysis has inserted `jeti_release` calls, so ARC-injected releases appear in the trace too.
- ✅ **CLI**: `jetic -trace-refcount file.jeti` prints a chronological, color-coded trace of every retained object's count, then exits (no codegen, no C compile). Flags: `-trace-max-iters <N>` (loop iterations simulated, default 2), `-trace-no-color`. Wired through `Pipeline.trace_refcount` / `trace_max_iters` / `trace_color` as `Step 4.5` in `pipeline.rs` (after ARC analysis, before the checker so even ARC-forbidden manual code is traceable).
- ✅ **Object identity = allocation site**: `Class#N` per-class counter. Allocations: `alloc`/`new`/`copy`/`mutableCopy`-prefixed, `init` chains to its receiver, `@"..."` literals, `@[...]` literals. Params of object type bound with count 1 and label `(param)`.
- ✅ **Count rules**: `jeti_release`/`[x release]` → −1; `jeti_retain`/`[x retain]` → +1; autorelease marks then batch-releases at `@autoreleasepool` end (`pool pop`); `x = [[.. alloc] init]` rebinds; `x = y` aliases share the same object.
- ✅ **Colors**: **green** = count increased vs previous print, **blue** = count decreased (still alive), **cyan** = freed (count reached 0), first print / unchanged = plain, **red** = errors — over-released (negative count) inline + still alive (`possible leak`) / over-released in the summary. Double-release decrements into negative territory (not clamped) so the summary flags it red. Yellow reserved for informational untracked-target warnings. `-trace-no-color` disables.
- ✅ **Warnings**: double-release/over-release (red `!`) and ops on untracked targets (yellow `! ...`); `== Summary ==` lists live objects (`possible leak`) and over-released objects (negative count) in red, freed objects plain, with allocation position; params excluded from leaks.
- ✅ **Branches**: `if`/`else` clone the pre-if state; final state = `else` path (or `then` when no `else`) so branch releases persist. Loops repeat the body `max_iters` times with fresh `Cat#N` identities per iteration.
- ✅ **ARC releases positions**: ARC inserts releases with line 0; the tracer falls back to the enclosing compound statement's line/col.
- ✅ **8 Rust unit tests** in `crates/trace/src/lib.rs` (`#[cfg(test)] mod tests`, AST constructed programmatically): alloc/release chain, retain/release, msg-send release, double-release warning, autoreleasepool batch pop, alias sharing, untracked warning, loop identity.
- ✅ **Escaped objects + `return`/`@throw`/`@catch`/static handling** (Aug 2026): the tracer now distinguishes ownership that leaves the traced scope:
  - `TraceObj` gains `escaped: bool`; `return expr` and `@throw expr` (via `trace_escape`) trace ref-ops inside the expression (`return [x autorelease]` autoreleases `x`) and mark the produced object `escaped`, so it is excluded from the leak Summary (a returned/thrown object is the caller's/runtime's responsibility).
  - `@catch (T *e)` binds the parameter to a `T (catch)` object (count 1) so `[e release]` in the catch body resolves instead of warning `untracked`.
  - Ref-ops whose receiver is itself a creation (`[[[X alloc] init] autorelease]`) create the object first, then apply the op — no more `! autorelease on untracked [...]`; the op result is returned so `Type *v = [x autorelease]` binds `v` to the object.
  - `static T *s = [[T alloc] init];` marks the created object `escaped` (static owns it for the process lifetime).
  - `@"..."` and `@[...]` literal objects are marked `escaped` in `create()` — in Foundation they are immutable constants / autoreleased runtime objects that are never manually released, so they are not leak candidates (removed 3 false positives in `tests/nparray_test.jeti`).
  - Branch binding persistence: a bare `if (cond) stmt;` without `else` inlines its `then` body via `trace_stmt_persist`, so bindings made inside (e.g. a static `s` assigned inside the `if`) survive to a later `return s`. This fixed the `+ (id)sharedNull` singleton pattern being misreported as a leak.
  - **Result**: all 5 `possible leak` false positives in `examples/01_JSONEditor/json_editor.jeti` (static `s_sharedNull`, `return [result autorelease]`, THROW's thrown error, `-copyValue` returns) eliminated — Summary reports `no live objects — all freed`.
  - Unit tests now **15** (7 new: returned-object escape, thrown-object escape, catch-param binding, creation-receiver refop, static-decl escape, bare-if persistence; 8 original).
- ✅ Completions (`completions/_jetic`, `jetic.bash`, `jetic.fish`) regenerated with the three new flags.
- 🟢 **SourceMap 统一行号翻译（Aug 2026）**：
  - **问题**：preprocessor 逐行内联 `#import` 文件，report 的行号是内联缓冲区行号，对不上原始文件（列号仍是准的，因为行没被合并）。
  - **新增 `jeti-cst::SourceMap`**（`crates/cst/src/source_map.rs`）：内联行号 → `(文件名, 原始行号)` 的单一事实来源。预处理器逐行拼接保证每个内联行唯一对应一个 (file, line)。
  - **统一出口**：parser `error()`、checker `check_error`/`check_warning`、pipeline ARC warning、`-trace-refcount` 输出全部调用 `SourceMap::locate()`——一处翻译，全链一致。格式统一 `file.jth:42:15`。
  - **接入点**：parser 的 `SourceMap` 字段（`with_source_map`）、checker 的 `source_map` 字段（pipeline 传入）、`TraceOptions.source_map`、pipeline ARC 分析复用同一个 SourceMap。
  - **无 double-translate**：AST 节点的 `line` 保持内联行号作为 SourceMap 索引（保证 `AST 内部`稳定、唯一），只有「给人看的输出」翻译。
  - **附带修复**：`has_return_stmt` 漏掉 `@noarc`/`@catch`/`@finally`/`@autoreleasepool` 内的 return，导致 Foundation 便利构造器被误报 `has no return statement`——已递归补齐。
  - **trace golden 更新**：`tests/golden/28_refcount_trace/*.out` 重新生成（8/8 通过），行号现在是原始文件行号。

### Unit Tests
- `cargo test` — 41/41 pass（lexer / parser / cst_print / cst_visit / symbol / binder / checker / preprocessor 等）；集成由 `test_all.py` 驱动，回归基线见上方 Tests Status。

### Soma Kernel — NASM + C + Jeti 三语 i386 内核 ✅ (Aug 2026)
- ✅ `examples/04_soma-kernel/` — 32 位保护模式内核，`boot/boot.asm`(引导+读盘+A20+GDT+进 PM) + `kernel/{entry,isr}.asm`(入口/IDT 桩) + `kernel/{kernel,hw}.c`(VGA/串口/kprintf/IDT/PIC/PIT/mem*/str*) + `jeti/soma_core.jeti`(Jeti 内核模块)。

### Jeti 原生裸机支持 — `-fno-libc`（freestanding mode）✅ (Aug 2026)
- ✅ **编译器级裸机支持**（不再靠内核侧手写 runtime）：新增 `jetic -fno-libc` 开关（`pipeline.no_libc`）。
  - **codegen** `emit_unit_with_headers(.., freestanding)`：freestanding 时不发 `#include <string.h>`，改发 `#define __JETI_FREESTANDING 1` + `#include <jeti/runtime.h>`。
  - **`include/jeti/runtime.h`** 加 `__JETI_FREESTANDING` 分支：不 include `<setjmp.h>/<stdarg.h>`；`jmp_buf` 自定 + `setjmp/longjmp` 映射到 `__builtin_setjmp/__builtin_longjmp`（零 libc）；异常全局 `__jeti_exception_buf/__jeti_exception_value` 由 `__thread` 改为普通全局（单核裸机）；声明 `extern NPClass jeti___jeti_root_class;` + `void *memcpy(...)`（用户提供定义）。
  - **`include/jeti/runtime.c`** 异常全局同样由 `__thread` 改为普通全局（`__JETI_FREESTANDING` 分支）。
  - **`compile_to_binary`** 裸机模式：给 clang 加 `-ffreestanding -fno-builtin -fno-stack-protector -fno-pic -fno-pie -fno-asynchronous-unwind-tables -nostdlib`（`-arch x86*` 时再加 `-mno-sse -mno-mmx -mno-red-zone`）——⚠️ **不再含 `-nostdinc`**（2026-09-30 起独立旗标，见"`-nostdinc` 独立旗标"节）；并且**不链接** `runtime.c`（它依赖 libc malloc/__thread）。
  - **@try/@catch 在裸机可用**：转译代码用 `setjmp/longjmp`(builtin) + 普通全局异常状态，`memcpy` 由用户 kernel.c 提供 → 裸机打印 "caught [e errorCode] = 42 / finally always runs / after-try continues"。
- ✅ **soma-kernel 已改用编译器裸机支持**：删掉内核侧手写 `include/jeti/runtime.h` 和 `include/setjmp.h`；Makefile 用 `jetic -rewrite-jeti -fno-libc` 转译，CFLAGS 加 `-I../../include -D__JETI_FREESTANDING`；kernel.c include 编译器真实 `<jeti/runtime.h>` 并定义 `jeti___jeti_root_class` + 异常全局 + `memcpy`。
- ✅ **回归**：全量套件 154/161 不变。

### Jeti 裸机分配器 — bump allocator + `[[Class alloc] init]` ✅ (Aug 2026)
- ✅ **`include/jeti/runtime_freestanding.c`**：提供 bump allocator（16KB 静态 arena + 偏移指针）+ `jeti_malloc`/`jeti_free`/`jeti_alloc`/`jeti_init`/`jeti_retain`/`jeti_release`/`jeti_autorelease`。`jeti_alloc` 用 `memset` 清零后设 `isa`/`retain_count`，`jeti_release` 计数归零后调 `jeti_free`。
- ✅ **零样板**：`runtime_freestanding.c` 现在自含 `jeti___jeti_root_class`、异常全局（`__jeti_exception_buf`/`__jeti_exception_value`，含 `__JETI_FREESTANDING` 分支：裸机普通全局 / 宿主 `__thread`）、`memcpy`。用户只需链接它，不再手写任何运行时全局。soma-kernel 的 kernel.c 和 golden 的 helpers.c 已移除重复定义。
- ✅ **`<string.h>` 移除 + 三头契约（Sep 2026）**：`runtime_freestanding.c` 实际只用 `memset`（`jeti_alloc` 清零一处），而 `memcpy` 本就是它自带的 weak 定义——字符串函数从未用到，却 include `<string.h>`（不在 C11 freestanding 强制头集合里）。已删该 include，`memset` 照 runtime.h 对 `memcpy` 的同款守卫自声明（`#ifndef memset` + 原型——`-ffreestanding` 下编译器仍可能自行生成 memset/memcpy 调用，最终镜像必须提供符号）；头注释契约同步改为"用户只需提供 freestanding 三头（stdint/stddef/stdbool）"。**before/after 探针**：`-nostdinc -isystem <clang-resource>/include`（libc 头被剥、freestanding 三头可用、无 string.h——模拟"内核不带 string.h"）下 HEAD 版 `fatal error: 'string.h' file not found`、修复版 rc=0。**回归**：golden/25 exit=0、baremetal stress exit=0、soma-kernel 同款旗标（`-nostdinc -ffreestanding -fno-builtin -target i386-none-elf`）单文件编译 rc=0 产出 i386 ELF。⚠️ **关键事实（探针实证）**：clang 21 的 `-nostdinc` 连 **builtin 资源目录也剥掉**（裸 `#include <stddef.h>` 直接 fatal error）——加 `-nostdinc` 的内核必须**自行提供三头**（soma-kernel 的 `include/` 正是如此）；不加时 clang builtin 目录（`clang -print-resource-dir`/include）自带的 9 个 freestanding 头（stddef/stdint/stdbool/stdarg/float/limits/stdalign/stdnoreturn/iso646）兜底。
- ✅ **`include/jeti/runtime.h`**：添加 `jeti_malloc`/`jeti_free` API 声明。
- ✅ **soma-kernel** 新增 `HeapCounter` 类，`+ (id) alloc { return jeti_alloc(self); }`，`[[SomaCore::HeapCounter alloc] init]` 在裸机 i386 内核跑通（`[c add:10]=10 [c add:20]=30 [c value]=30`），ARC 自动插入 `jeti_release(c)`。
- ✅ **Golden test** `25_freestanding` 同样演示 bump allocator + alloc+init。
- ✅ **回归**：全量套件 154/161 不变。

### BSD Cross-Compilation — Implemented ✅ (Aug 2026)
- ✅ `build-all.sh` now targets **8 platforms** (up from 5): added `x86_64-unknown-freebsd`, `i686-unknown-freebsd`, `x86_64-unknown-netbsd`
- ✅ `zig-cc.sh` maps the 3 BSD rust triples to zig targets (`x86_64-freebsd-none`, `x86-freebsd-none`, `x86_64-netbsd-none`)
- ✅ Per-target linker wrappers (`zig-cc-x86_64-freebsd.sh`, `zig-cc-i686-freebsd.sh`, `zig-cc-x86_64-netbsd.sh`) embed the target so `zig cc` produces correct output
- ✅ FreeBSD targets need stub shared libraries for `devstat`, `procstat`, `kvm`, `memstat`, `util`, `rt`, `execinfo` (Rust std references them). `build-all.sh` auto-creates them in `target/bsd-stubs/` and `target/bsd-stubs-i386/` via zig cc, and passes `-L` + `--allow-shlib-undefined` as `RUSTFLAGS`
- ✅ NetBSD target builds without stubs, only needs the linker wrapper
- ✅ `.cargo/config.toml` generated per-target with correct linker paths
- ✅ All 3 BSD binaries verified: `file` shows correct ELF format for FreeBSD 14.0 / NetBSD 10.1
- ✅ Archives: `jeti-x86_64-unknown-freebsd.tar.gz`, `jeti-i686-unknown-freebsd.tar.gz`, `jeti-x86_64-unknown-netbsd.tar.gz`

### Windows Cross-Compilation — Implemented ✅ (Aug 2026)
- ✅ `build-all.sh` **新增 `x86_64-pc-windows-gnu`** 目标（`TARGETS` 现为 8 项）。
- ✅ **构建工具**：Windows 目标用 **`cargo zigbuild`**（`cargo install cargo-zigbuild`）——手动 `cargo build` + zig 作 linker 会因 rustc 末尾的 `-nodefaultlibs` 让 zig 丢掉自带 MinGW 库搜索路径，报 `unable to find dynamic system library 'msvcrt'`；`cargo-zigbuild` 正确处理了这些库路径。找不到 `cargo-zigbuild` 时 `build-all.sh` 会**跳过** Windows 目标并提示。
- ✅ `zig-cc.sh` 增加映射：`x86_64-pc-windows-gnu` → `x86_64-windows-gnu`（zig 不认 rust triple 的 `pc` vendor），供 `cc` crate 编译 `runtime.c` 用（产出 COFF `.o`）。
- ✅ 打包：Windows 产物为 **`.zip`**（`jeti-x86_64-pc-windows-gnu.zip`），内含 `jetic.exe` + `libjeti.a`(COFF/GNU ar) + `include/` + `completions/` + `install.sh`。
- ✅ 验证：`build-all.sh` 全量跑通 8 个 host/交叉目标，Windows 产出 `PE32+ executable (console) x86-64, for MS Windows`。
- ⚠️ 仍待办：Windows 端 `jetic.exe` **运行时**用 `zig cc` 编译用户程序（`select_c_compiler` 已按 `cfg!(windows)` 默认 `zig cc`），但需在真实 Windows / Wine 上验证；`runtime.c` 的 `__thread` 在 zig cc 下没问题（运行时仍是普通全局语义待确认）；Windows 原生安装建议改 PowerShell `install.ps1`（现 bundle 里仍是 bash `install.sh`）。
- ✅ **`install-pkg.sh` → `install.sh`**：仓库脚本重命名；`build-all.sh` 里 `cp install-pkg.sh …` 改为 `cp install.sh …`，`crates/jetic/build.rs` 也改找 `install.sh`。
- ✅ **`install.ps1`（PowerShell 安装脚本）**：Windows 原生安装器。
  - **安装位置**（遵循 Windows 惯例，与 VS Code 用户安装 / Python 等一致）：默认**用户级** `%LOCALAPPDATA%\Programs\jetic`（无需管理员）；`-System` → `%ProgramFiles%\jetic`（需管理员）；`-Prefix <dir>` 自定义。（若走包管理器：scoop → `%USERPROFILE%\scoop\apps\jetic\current`，choco → `C:\ProgramData\chocolatey\lib\jetic`。）
  - **布局**（对齐 Unix 的 `bin/ lib/ include/ share/`）：`<prefix>\bin\jetic.exe`、`<prefix>\lib\libjeti.a`、`<prefix>\include\{jeti,Foundation}\`、`<prefix>\share\jetic\completions\jetic.ps1`。
  - `jetic.exe` 靠 `resolve_bundle_root()` 从 `bin\jetic.exe` 的父目录 `..` 找到 `include\jeti\runtime.h` → 即 `<prefix>`；install.ps1 另设 `JETI_HOME=<prefix>` 双保险。
  - 自动把 `<prefix>\bin` 加入 **用户 PATH**（`[Environment]::SetEnvironmentVariable('Path', …, 'User')`，幂等）；用 `jetic.exe --gen-completions powershell` 生成补全并写入 PowerShell 用户 profile（带 marker，幂等）。
  - bundle 同时含 `install.sh`（MSYS/WSL/Git-Bash 用）与 `install.ps1`（原生 PowerShell 用）。`build-all.sh`/`build.rs` 均拷贝两者。

### Fixes Applied (Current Session — Aug 2026)
- ✅ **错误分阶段前缀**：pipeline 各阶段 fail-fast 返回的错误消息改为 `Parse failed:\n[parser] line:col: msg` / `Binding failed:\n[binder] ...` / `Elaboration failed:\n[elaborator] ...` / `Type checking failed:\n[checker] ...`。`prefix_lines(stage, msg)` 给多行错误逐行加 `[stage]` 前缀（空行跳过），一条编译错误即可按 parser/binder/checker 分门别类阅读。见 `crates/jetic/src/pipeline.rs`。
- ✅ **`__typeof__` 作为内建函数实参**：`is_builtin_type_arg_start` 增加 `KeywordKind::Typeof`，使 `__builtin_types_compatible_p(__typeof__(x), int)` 的 `__typeof__(x)` 被 `parse_type_full` 解析为 TypeLiteral（原先报 `expected ')' after args (got keyword)`）。见 `crates/parser/src/parser.rs`。
- ✅ **typedef struct 字段类型丢失**：`typedef struct { ... } Name;` 的字段在 codegen Typedef 分支只匹配 `AstDeclData::Variable`，Ivar 字段落到 `_ => "int"`，导致 `unsigned char tag; unsigned long value; char name[8];` 全变 `int`。修复：Typedef 分支增加 `AstDeclData::Ivar` 匹配，字段类型经 `ast_type_to_c_str` 保留限定词。见 `crates/codegen/src/codegen.rs`。
- ✅ **`tests/asm_fusion_test.jeti` 全语法融合**：以内联 asm 融合测试为基底，新增 `@noarc`（块内私有对象 retain/release 成对平衡，不触碰 ARC 局部对象）、`@synchronized`、`__attribute__((packed/aligned/format/unused))`、双下划线标识符（`__FILE__`/`__LINE__`/`__builtin_expect`/`__typeof__`/`__alignof__`/`__builtin_types_compatible_p`）、泛型单态化 `TaskQueue<BlockHasher*>` + `@using` 别名、`@class` 前向声明（`Net::RemoteNode` 真未定义类）。14 步全部 clang 后端跑通，ARC 自动释放 + `@noarc` 手动管理并存。
- ⚠️ **`@noarc` 使用教训**：`@noarc` 内对**外层 ARC 管理的局部对象**手动 `release` 会 double-free（ARC 作用域结束还会自动 release），导致段错误 exit=139。正确用法：`@noarc` 块内创建自己的私有对象全手动管理，retain/release 成对平衡（`alloc`=1 → `retain`=2 → `release`=1 → `release`=0 块内释放），ARC 对象一律交给自动 release。
- ⚠️ **jetic CLI 命令格式**：`-asm file.s` 必须放在 `run` 关键字**之后**（`jetic run file.jeti -asm file.s`）。若放在 `run` 之前（`jetic -asm file.s run file.jeti`），run 前的 flag 扫描不处理 `-asm`，其值被当输入/未知参数。test_all.py 用的是正确格式。

### NPMutableArray — Implemented ✅ (Aug 2026)
- ✅ **`include/Foundation/NPMutableArray.{nh,np}`**：可变数组，继承 `NPArray`（复用其 `_items/_count/_capacity` 存储与不可变查询 API，dealloc 继承自 NPArray）。API：`+arrayWithCapacity:`/`+array`/`+arrayWithObject:`/`+arrayWithObjects:count:`；`-init`/`-initWithCapacity:`/`-initWithArray:`/`-initWithObjects:count:`；`-addObject:`/`-addObjectsFromArray:`/`-insertObject:atIndex:`/`-removeObjectAtIndex:`/`-removeLastObject`/`-removeObject:`/`-removeAllObjects`/`-replaceObjectAtIndex:withObject:`/`-exchangeObjectAtIndex:withObjectAtIndex:`/`-setObject:atIndex:`。存储用 `realloc` 2× 增长（初始 4）；`addObject:` 对元素 `jeti_retain`，移除类方法 `jeti_release` 被移元素；便利构造器仿 NPMutableString 用 `@noarc { return [[[self alloc] init...] autorelease]; }`。
- ✅ **`Foundation.jth`** 已加入 `NPMutableArray.jth`/`.jeti` import。测试 `tests/npmutablearray_test.jeti`（13 段，覆盖全部可变 API + 继承查询 + copy 深拷贝 + description），ARC 与 MRC 输出逐字节一致，`-trace-refcount` Summary = `no live objects — all freed`。
- ✅ **字符串字面量 interning（Sep 2026 修复）**：宿主模式 `jeti_stringFromCstr` 维护 256 槽 intern 表，同内容字面量返回同一对象——`containsObject:`/`indexOfObject:` 可直接用字面量查询（原"必须同一变量"约束作废）。表持有 +1 永不释放（ObjC 常量串语义：ARC 视为 unretained、MRC 不得手 release）；表满回退新建（正确、仅不 intern）。freestanding 保持原新建对象体（`#ifdef __JETI_FREESTANDING`，无 `<string.h>`）。

### ARC 归属语义修复 — convenience 构造器在 ARC 下可用了 ✅ (Aug 2026)
- ✅ **`ownership_for_method` 默认分支 `Retained` → `Unretained`**（`crates/ownership/src/ownership.rs`）：ObjC 约定中非 `alloc/new/copy/mutableCopy/init` 家族的方法返回调用者**不拥有**的对象（autoreleased/unretained）。原先默认 `Retained` 导致 `[NPArray arrayWithObject:x]`/`[NPString stringWithUTF8String:".."]` 等 convenience 构造器被 ARC 在作用域结束注入 `jeti_release`，与 autorelease pool pop 双重释放 → 程序 exit=1（此前 nparray_test/npstring_demo 只能靠 MRC-retry 跑通）。修复后 nparray_test、npstring_demo、NPMutableString convenience 构造器均在 ARC 下直接通过。
- ✅ **`-copy` 契约修复**：`NPArray.copy`/`NPString.copy` 原先经 convenience 构造器返回 autoreleased 对象，违反 `copy` → +1 约定 → ARC 下 caller release + pool pop 双重释放。现 `NPString.copy` = `[[NPString alloc] initWithString:self]`（硬编码 NPString，可变子类 copy 得不可变结果），`NPArray.copy` = `jeti_retain([NPArray arrayWithObjects:_items count:_count])`（同样硬编码 NPArray）。内部 `[self copy]` 用户（`-description`、`stringByAppendingString:/UTF8String:` 的 NULL 分支）改用 `[NPString stringWithString:self]` 直接返回 autoreleased。
- ✅ **`crates/arc/src/ownership.rs` 死代码已删除**：该文件曾是 ownership 逻辑拆成独立 crate 前遗留的旧副本，rustc 从不编译它（`lib.rs` 只声明 `pub mod arc;`），误改它会毫无效果。arc crate 实际用 `jeti_ownership::*`（`crates/ownership`），改归属语义要改 `crates/ownership/src/ownership.rs`。
- ⚠️ **test_all.py 用 `target/debug/jetic`**（`JETIC = PROJECT / "target" / "debug" / "jetic"`），改动 Rust 后若只 `cargo build --release` 会拿陈旧 debug 二进制跑出幽灵失败。回归前务必 `cargo build`（debug）刷新。

### NPLog 改用 `NPString *` 格式 — Implemented ✅ (Aug 2026)
- ✅ **签名**：`NPLog(NPString *format, ...)` / `__NPLogv(NPString *format, va_list args)`（`include/jeti/runtime.h`）。`runtime.h` 新增 `typedef struct NPString NPString;` 前向声明。
- ✅ **runtime.c 取 C 串**：runtime.c 是独立 C，不知道 codegen 生成的 `struct NPString` 布局，故用私有镜像结构 `struct __jeti_npstring_layout`（`isa/retain_count/_cstr/_length/_hash/_hashIsValid`，与 NPString.jth 的 `@public` ivar 顺序一致）取 `_cstr` 交给 `vfprintf`。⚠️ 若 NPString.jth 的 ivar 布局改动，需同步此结构。
- ✅ **codegen**：新增 `nplog_format_arg` 辅助函数（`crates/codegen/src/codegen.rs`，行 ~841）——NPString 存在时把 NPLog 的格式参数发射为 `(NPString *)jeti_stringFromCstr("...")`；NPString 缺失时退化为裸 C 字符串。两处 NPLog 特判（无 `%@` 内联路径 + `expand_nplog_format` 的 `%@` 展开路径）都改用它。`%@` 展开仍把 `%@`→`%s` 并把实参变为 `arg ? [[arg description] UTF8String] : "(null)"`。
- ✅ **调用点**：`tests/npstring_demo.jeti:103` 改为 `NPLog(@"NPString value: %s", ...)`；golden `13_foundation/02_nplog/*.c` 与 `03_description/player.c` 期望输出同步为 `NPLog((NPString *)jeti_stringFromCstr("..."), ...)`（test_all 不比对 .c，仅作文档）。
- ⚠️ **`NPLog("...")` 裸字符串已是 warning（非 error）**：`crates/checker/src/lib.rs` 的 FuncCall 臂按名字特判 `NPLog`，首参不是 `@"..."` 即报 warning。加 `-Werror` 才提升为 error。遵循 C 哲学：允许过，给警告，运行时崩溃。
- ✅ **回归**：test_all 187/198（4 个既有失败不变）、cargo unit 41/41、trace goldens 8/8。

### 通用 `"..."` 裸字符串拒绝 — 方法/函数参数类型检查 ✅ (Aug 2026)
- ✅ **通用机制取代 NPLog 特判（NPLog 特判保留）**：checker 在 `check_expr_inner` 的 MsgSend 和 FuncCall 臂中，遍历实参并与形参类型对比。若形参是对象类型（`id` / `instancetype` / `Named + is_pointer` 如 `NPString *`）而实参是裸 `AstExprData::String`（`"..."`），报错 `argument as a bare C string is not an object; use @"..." for an NPString`。
- ✅ **实现**：checker 新增 `collect_signatures`（在 `check` 入口第一遍遍历所有 AST 声明，收集方法与函数签名到 `method_params` / `function_params` HashMap），`is_object_type` 辅助函数，以及 MsgSend/FuncCall 臂的并行检查。形参从 `CstParam` 链表转为 `Vec<Option<AstType>>` 后与实参逐位对比。
- ✅ **涵盖**：Foundation 方法（`stringByAppendingString:`、`arrayWithObject:`、`containsObject:` 等）、用户定义方法、用户定义函数。`printf` / `strlen` 等 C 函数不在符号表中，不受影响。`(id)"..."` 显式强转可绕过检查（Cast 包裹后 `matches!(...data, AstExprData::String(_))` 不匹配）。
- ⚠️ **`runtime_test.jeti` 原用 `"..."` 传给 `id` 参数**：已改为 `(id)"..."` 显式强转，绕过通用检查。
- ✅ **`-Werror` flag**：CLI 新增 `-Werror`（`crates/jetic/src/main.rs`），将 checker 的 warning 提升为 error 中止编译。已加入 completions（bash/zsh/fish/flag_picker）。
- ✅ **`run` 子命令文件补全**：bash/zsh 完成脚本中 `jetic run <Tab>` 现在补全文件名（`.jeti`/`.jth` 文件），而非空列表。`completions/jetic.bash` 的 `jetic__subcmd__run` 分支改为 `COMPREPLY=($(compgen -f "${cur}"))`；`completions/_jetic` 的 `run` 子命令改用 `_files -g "*.jeti" -g "*.jth"`。
- ✅ **回归**：test_all 187/198（4 个既有失败不变）、cargo unit 41/41。

### C 桥接头 `--emit-bridge-header` — Implemented ✅ (Aug 2026)
- ✅ **背景**：C 直接调用 Jeti 对象方法要写 vtable 下标 / SEL 常量，很长。codegen 本来已为每个方法生成命名 C 函数（`NPString_UTF8String(NPObject*, SEL, ...)`），但 SEL 常量是 `static const`（跨文件不可见），且每次调用要传 `__jeti_sel_xxx`。
- ✅ **新增 `jetic -emit-bridge-header <file.h>`**：生成一个 C 桥接头，为每个类方法/实例方法输出三样东西：(1) 生成函数的 `extern` 声明；(2) 类前向声明（`struct NPString; typedef struct NPString NPString;`）+ `NPRange` 定义；(3) `static inline` 包装函数 `jeti_Class_method(...)`，内部用 `sel_registerName("原selector")` 拿 SEL（避开 `static const` SEL 常量跨文件不可见问题），类方法自动带 `&JETI_CLASS_$_Class`。跳过 `jeti_root` 内部根类。
- ✅ **用法**：`jetic -rewrite-jeti lib.jeti -o lib.c -emit-bridge-header lib.h`，然后 `clang caller.c lib.c include/jeti/runtime.c -I include -o app`。C 侧 `#include "lib.h"` 后直接 `NPString *s = jeti_NPString_stringWithUTF8String_("hi");`。⚠️ 调用方 `main` 必须先 `jeti_metaInit()` 初始化类元数据。
- ✅ **实现**：`crates/codegen/src/codegen.rs` 新增 `emit_bridge_header(&CgUnit)`；`CgClassMeta`/`ClassInfo` 新增 `method_sel_names`（存原始 selector 带冒号，供 `sel_registerName`）；继承合并逻辑同步处理 `method_sel_names`。pipeline 加 `bridge_header: Option<String>`，Step 6.5 写文件。CLI 加 `-emit-bridge-header`（单/双横杠均可，已加入 `norm_flag`/`jetic_flags`/completions）。
- ✅ **回归**：test_all 187/198（4 个既有失败不变）、cargo unit 41/41。

### `class_forward_decl.jeti` 点语法修复 + Elaborator 作用域追踪 ✅ (Aug 2026)
- ✅ **根因**：`@class ForwardDeclared;` 前向声明 + 后置完整定义下，`ForwardDeclared *tmp = c.item;` 的 `tmp.value` 生成了 `tmp.value`（点，应 `->`）。真正根因是**符号表无作用域**——binder/elaborator 都从不 `push_scope`/`pop_scope`，Foundation 内联代码（`NPMutableArray.jeti` 里有 `NPObject *tmp`）与用户 `main()` 的 `tmp` 撞名，`st.lookup("tmp")` 返回第一个匹配（Foundation 的 `NPObject *tmp`），elaborator 无法解析 `value` 属性，退回 fallback `PropRef { prop: None, cls: None, is_arrow: false }`。
- ✅ **修复 1（elaborator）**：新增 `local_types` 作用域栈。`convert_decl` 处理 Function/Method 时 push/pop 一个作用域，并把形参类型注册进作用域；处理 Variable 声明时把声明类型注册进当前作用域。`convert_dot_expr` 的非 self 路径**优先**用 `lookup_local_type(obj_name)` 解析对象类型（解决撞名），找不到再退回 `st.lookup`。
- ✅ **修复 2（codegen）**：`AstExprData::PropRef` fallback 路径（`prop=None, cls=None`）中，若对象的 `expr_type.is_pointer` 为 true（checker 已把类型修正为 `ForwardDeclared *`），强制生成 `->` 而非 `.`（ObjC 实例永远是指针）。⚠️ 不再对已知类对象 prepend `_`（如 `@public int x` 的 `Vector2D` 不是 property，prepend `_` 会错）。
- ✅ **结果**：`class_forward_decl.jeti`、`grand_integrated_epic_test.jeti`、`scope_shadow_test.jeti`、`crypto_pipeline.jeti`、`weak_ivar.jeti` 全部通过。test_all 从 187/198 → **188/198**（只剩 3 个既有失败：`diamond_impl.jeti`、`mega_types.jeti` 无 main、`double_release.jeti` 故意崩溃）。
- ⚠️ **`golden/28_refcount_trace/double_release.jeti` 的 RUN_FAIL 是正常表现，不是内存泄漏**：它是故意对已释放对象 `jeti_release` 到负计数，`-trace-refcount` 的 Summary 显示红色 `over-released`——这正是该测试的目的（演示 double-release 检测）。
- ✅ **回归**：test_all 188/198（3 个既有失败不变）、cargo unit 41/41、trace goldens 8/8。

### NPArray / NPMutableArray golden 测试 + 上下文关键字修复 ✅ (Aug 2026)
- ✅ **新增 golden**：`tests/golden/13_foundation/06_nparray/nparray_test.{np,out}` 和 `07_npmutablearray/npmutablearray_test.{np,out}`，与 04_npstring/05_npmutablestring 同模式（`.jeti` + `.out`，test_all 校验 exit code）。覆盖 arrayWithObjects:count:/arrayWithObject:/array、@[] 字面量、containsObject:/indexOfObject:-1、copy（+1→ARC release）、description；NPMutableArray 的可变 API（add/insert/replace/set/exchange/remove/removeAll/addObjectsFromArray/alloc+initWithArray/copy）。
- ✅ **上下文关键字修复**：lexer 把 `copy`/`retain`/`weak`/`strong`/`assign`/`nonatomic`/`getter`/`setter`/`readonly`/`readwrite` 注册为**全局关键字**（`KeywordKind::At*`），导致 `int copy = 0;`、`NPArray *copy = ...` 解析失败（"expected ';' after expression (got keyword)"）。这些词在 ObjC 里是**上下文关键字**——只在 `@property (...)` 里特殊。修复：parser 的 `is_name_token`/`parse_qualified_name_with_keywords`/`parse_primary` 三处把 `is_contextual_kw_ident()`（这些 At* 关键字）当作合法标识符接受；`@property (copy)` 的属性解析（`match_keyword(AtCopy)` 等）不受影响。`[obj retain]` 消息发送、`(weak)` 属性声明照常。
- ✅ **回归**：test_all 188 → **190/200**（3 个既有失败不变）、cargo unit 41/41、trace goldens 8/8。

### `xx_t` 类型解析 + Block 生成格式修复 + Timsort 测试 ✅ (Aug 2026)
- ✅ **`xx_t` 类型解析修复**：parser 原本只有硬编码 typedef 名列表（`size_t`/`FILE` 等），`clock_t t0 = clock();` 会被当表达式而失败。修复 `is_declaration_start`：`IDENT IDENT`（如 `clock_t t0`）判为声明，覆盖所有系统头 typedef；并排除复合赋值（`x *= e`、`x <<= e`）。补全常用 POSIX 类型（`clock_t`/`time_t`/`off_t`/`pid_t`/`mode_t` 等）。
- ✅ **Block 字面量格式修复**：clang 后端 `emit_expr` 的 `CgExprData::BlockLit` 原来用 `emit_stmt` 发射函数体，产生多行 + 尾换行，导致函数调用参数里的块块 `);` 换行错乱。新增 `emit_stmt_inline`（单行语句发射器），块块体改为单行 `{ return (a < b); }`。
- ✅ **gcc 后端 invoke 函数双大括号修复**：block 展开时 `convert_expr` 预写 `{` 又经 `emit_stmt` 再写 `{`，产生 `{ { ... } }`。现改为不预写 `{`，交给 body 的 Compound 自闭环。
- ✅ **`tests/timsort_test.jeti`**：Jeti 语法版 Timsort（`@interface`/`@implementation`、多段 selector `- (void)lowerBound:lo:hi:key:using:`、Block 比较器、`@autoreleasepool`、`NPLog(@"%@")`、`@public` ivar）。stdin 读数字 → 排序 → 打印 + 耗时。已验证 15/300/500/700/1000 全对、已排序输入自适应加速。
- ✅ **回归**：test_all **191/201**（3 个既有失败不变）、cargo unit 41/41。

### Generated C 可读性注释 — Implemented ✅ (Aug 2026)
- ✅ **生成 C 代码带 `/* */` 注释（默认开启）**：`emit_unit_with_headers` 新增 `comments: bool` 参数（pipeline 默认 true，`-no-comments` 关闭）。共 5 层，全部用 `/* */`：
  - **T1 文件头 banner**：替换原 `// Generated by jetic`，标注 source 文件 + backend。
  - **T2 分节横幅**（`section_comment` helper）：`/* ------- Section N · 标题 ------- */`，13 节——Requires & defines / Forward declarations / SEL constants / Type declarations & typedefs / Struct definitions / Function prototypes / File-level variables / VTable & class layouts / Class metadata infrastructure / Vtable & metadata instances / Class metadata initialization / Runtime support / Function bodies。
  - **T3 类注释**：class struct 前 `/* Class layout: X (super: Y) */`；vtable 实例前 `/* VTable instance: X */`；meta vtable 实例前 `/* Meta vtable instance: X */`；getClass IMP 前 `/* +getClass for X */`。
  - **T4 方法注释**：函数体循环前预构建 `method_comments` map（`owner_mname` → ObjC 签名），每个方法 IMP 上方输出 `/* -[Person age] */` 或 `/* +[Person alloc] */`（用 `method_sel_names` 原始 selector，只留 ObjC 风格，不带 C 函数签名）。含命名空间类（`A::B`）时只取短名 `B`。
  - **T5**：`// ─── Block expansion ───` 在 comments 开启时改 `/* Block expansion definitions (gcc/portable) */`。
- ✅ **`-no-comments` CLI flag**：单/双横杠均可（`norm_flag`/`jetic_flags`/`DOUBLE_TO_SINGLE`/clap `Arg` 全加），pipeline `no_comments` 字段贯通。关闭时保留旧 `// Generated by jetic` 首行，行为与之前完全一致。completions 三件套（`_jetic`/`jetic.bash`/`jetic.fish`）手工同步新增 flag。
- ✅ **零回归**：cargo unit 41/41、test_all **193/203**（3 个既有失败不变）、trace goldens 7/8（`arc_inject` 既有失败，与注释无关——trace 跑在 codegen 前）。注释不影响 C 编译（161 个 .jeti 全部默认带注释编译通过）。

### 全量特性压力测试 `grand_feature_stress_test.jeti` ✅ (Aug 2026)
- ✅ **`tests/grand_feature_stress_test.jeti`**：单文件 `main` 汇总近期全部特性——Foundation 容器（`NPMutableArray` 增删查 + `NPLog(NPString*)`+`%@`）、泛型单态 `Box<T>` + 嵌套泛型 `Box<Box<NPString*>*>`、10 参数多段方法 + `@implementation` 花括号内直接写 ivar（`int _sum;`）的写法、`NPString` 级联拼接、`typedef BOOL (^CompareBlock)` 捕获外部变量、`typedef int (*IntFn)` + `struct Pair`、上下文关键字 `copy`/`retain`/`weak` 当标识符、`clock_t`/`time_t`、ARC 自动释放 + `@noarc` 手动平衡、`@try` / `@throw` 异常处理、类方法 vs 实例方法。

### Codegen 健壮性审计 + nil-messaging / catch / 链式修复 ✅ (Aug 2026)
- ✅ **实证审计**：ASan+UBSan 复跑 4 个代表文件（npstring_demo/nparray_test/timsort_test/grand_feature_stress_test）全过、`-trace-refcount` 3 项 Summary 均 `no live objects — all freed`；clang --analyze 暴露 3 个真实缺陷，全部修复：
- ✅ **缺陷 1 — nil-messaging 语义缺失（最高危）**：`[nil msg]` 之前直接解引用 `nil->isa` → 段错误 exit=139；ObjC 应静默返回 0/nil。修复：`emit_expr` 的实例消息发射统一改为 temp+守卫 `({ NPObject *__jeti_tmp_N = (NPObject *)(recv); __jeti_tmp_N ? <dispatch>(__jeti_tmp_N, sel, ...) : 0; })`（删除了原来无守卫的 simple-Ident 内联路径，codegen.rs:4581）。split-emit（alloc+init）路径同样加守卫 `__jeti_tmp_N ? ... : 0`（codegen.rs:5036）。
  - **结构体返回特判**：`NPRange r = [s rangeOfString:]` 在 nil 守卫下 `cond ? struct : 0` 非法（incompatible operand types）→ 新增 `vtable_return_type`（从 `CLASS_METHOD_METADATA` 的 fn-ptr 类型提取返回类型）+ `nil_msg_fallback`（指针/void/标量用 `0`，结构体用 `(T){0}` 复合字面量，codegen.rs:4455-4470）。
  - **连带修复链式消息**：nil 守卫的 temp 化统一后，`[[[e maybeNil] stringByAppendingUTF8String:"x"] ...]` 不再生成非法的 `->stringByAppendingUTF8String_` 垃圾 C——外层 send 走守卫路径正确展开（原来 Empty 未声明该方法时走 arrow-access fallback 产生破损代码）。
- ✅ **缺陷 3 — catch 变量 dead store / 类型转换**：`@catch (Boom *e)` 原来无条件生成 `Boom * e = __jeti_exception_value;`——(a) catch 体不用 `e` 时是 dead store（clang analyzer DeadStores）；(b) 缺显式 cast（`-Wall` incompatible pointer types）。修复（codegen.rs:1953）：新增 `expr_refs_name`/`stmt_refs_name`/`decl_refs_name` 三个 AST 引用检测；用到 `e` → `Boom * e = (Boom *)__jeti_exception_value;`（带 cast）；不用 → `Boom * e; (void)e;`（无 dead store、无 unused）。grand_feature_stress 的 analyzer 警告从 1 → 0。
- ✅ **checker nil-messaging warning**：`[nil msg]` 接收者为字面量 nil 时发 warning `message 'X' sent to nil receiver; result is always zero/nil`（checker/src/lib.rs MsgSend 分支，零误报——变量接收者不报），并入现有 `-Werror` 提升体系。
- ✅ **新测试**：`tests/nil_messaging_test.jeti`（6 段：nil 值返回=0、nil void 无操作、nil 结构体返回零值、nil 链式安全、非 nil 链式正确、字面量 nil 发送）+ checker warning 验证。修复前 `[nil value]` 段 exit=139，修复后全过。
- ✅ **回归**：cargo unit 41/41、test_all **194/204**（3 个既有失败 + 7 interactive 不变，新增 nil_messaging_test 通过）、trace goldens 7/8（`arc_inject` 既有失败）。
- ⚠️ **`@noarc` 块必须手动计数归零**：`alloc`=1 → `retain`=2 → `release`=1 → `release`=0（漏一个 release 会让 trace 报 `still alive — possible leak`，AGENTS 的 @noarc 教训一致）。修正后 `-trace-refcount` Summary = `no live objects — all freed`。

### `#pragma mark` 原位透传 + `@数字字面量`（NPNumber）✅ (Aug 2026)
- ✅ **`#pragma mark` 位置修复**：此前 preprocessor 把所有 `#` 行塞进 `c_out`（C 前导 → 生成文件顶部），导致 `#pragma mark` 全部堆到文件最上面。现只把 `#pragma mark ...`（纯 IDE 标记）保留在 jeti 流内定位透传：
  - **preprocessor**：`trimmed.starts_with("#pragma mark")` → 写入 `jeti_out`（其余 `#pragma`/`#warning` 仍进 `c_out`）。
  - **新增 raw-line 通道**：`CstDeclKind/Data::RawLine(String)`、`AstDeclKind/Data::RawLine(String)`、`CgDeclKind/Data::RawLine(String)`；parser 新增 `parse_raw_pragma_decl`（按源码切片消费整行），在 `parse_declaration_inner`、`@interface`/`@implementation` 方法循环、`parse_statement` 接入；elaborator `CstDeclData::RawLine → AstDeclData::RawLine`；codegen `convert_decl`/`emit_decl` 原样发射。
  - **方法分组**：class 方法循环里 pragma 先 `pending_pragmas` 暂存，flush 时按 `fn_name` 插入到下一个方法声明的**前面**（解决 `@interface` 先建 method 声明、`@implementation` 的 pragma 被追加到末尾的错位）。
  - 验证：`tests/tricalc.jeti` 的 5 个 `#pragma mark` 各自紧贴其方法组，生成 C 编译通过。
- ✅ **`@数字字面量`（boxing literal）**：`@123` / `@1.5` / `@1e3` 以前 `@` 被 lex 成 Error token（`@1, @2` 报 `expected ']' after message send`）。现在：
  - **lexer**：`@` 后跟数字 → 新增 `TokenKind::AtNumber`（扫描整数/小数/指数，`.` 仅在后跟数字时消费，避免吞 `@1.foo`）。
  - **parser**：`AtNumber` 在 `parse_primary` 里 desugar 成普通类方法发送 `[NPNumber numberWithInt:N]`（浮点用 `numberWithDouble:`），复用全部消息派发路径，无需新 AST 变体。
  - **`include/Foundation/NPNumber.{nh,np}`**（**NP 前缀**，非 NS）：`+numberWithInt:/numberWithLongLong:/numberWithDouble:/numberWithBool:`、`-intValue/longLongValue/doubleValue/boolValue`、`-description`（`snprintf` → NPString）、`-isEqualToNumber:`；已并入 `Foundation.jth`。
  - 验证：`NPNumber *n = @42; NPLog(@"%@", n)` → `42`；`@[ @1, @2, @3 ]` → `[1, 2, 3]`。
- ✅ **ObjC variadic 集合构造器已支持**：`[NPArray arrayWithObjects:a, b, c, nil]`（单冒号选择子 + 逗号多参）已可用。
  - **parser**：消息发送的 `sel:` 分支收集逗号分隔的附加实参；当 `args.len() > selector 冒号数` 且 selector 为 `arrayWithObjects:` 时，desugar 成数组字面量 `@[a, b, c]`（丢弃结尾 `nil`），复用既有 `jeti_array_create` 路径（parser 里 `parse_primary` 的 message-send 分支）。
  - ⚠️ 目前仅覆盖 `arrayWithObjects:`；结果与 `@[...]` 相同（不可变 `NPArray`）。`[NPMutableArray arrayWithObjects:...]` 仍会得到不可变数组（如需可变请用 `arrayWithObjects:count:` 或 `@[...]` + mutableCopy）。其他 variadic selector 仍不支持。
- ✅ **回归**：cargo unit 41/41、test_all **195/206**（`diamond_impl`/`mega_types` 无 main、`double_release` 故意崩溃 共 3 既有失败；`q.jeti` 现通过）、trace goldens 7/8（`arc_inject` 既有失败）。

### `@namespace … @endnamespace`（取代花括号）✅ (Aug 2026)
- **背景/分类**：ObjC 的 `@end` 家族（`@interface`/`@implementation`/`@protocol`）全是**顶层声明容器**，且彼此从不嵌套；花括号只用于**执行作用域**（`@try`/`@autoreleasepool`/`@synchronized`）。`@namespace` 装的是声明（类/协议），属**声明容器**，因此改用 `@endnamespace`（`@end` 风格的专属终结符）而非 `{}`——既符合分类，又保持 `@end` 永远只关闭 @interface/@implementation/@protocol 的**无歧义性**（避免家族史上首次"@end 嵌 @end"）。
- **lexer**：新增关键字 `@endnamespace` → `KeywordKind::AtEndNamespace`（`token.rs` + `KW_TABLE`）。
- **parser**：`parse_namespace` 不再 `consume(LBrace/RBrace)`，改为循环 `parse_declaration` 直到 `match_keyword(AtEndNamespace)`；**允许空 namespace**。selector 排除列表（3 处）+ 错误恢复 sync 列表加入 `AtEndNamespace`。
- **全仓库迁移**：37 个 `.jeti`（另有 2 个 `.jeti.bak`/`.jth.bak`）的 `@namespace X { … }` → `@namespace X … @endnamespace`，含**嵌套 namespace**（脚本用字符串/注释剥离 + namespace 栈式大括号计数，正确区分 ivar 列表/方法体大括号）。`@using namespace X;`（无 `{`）不动。
- **不兼容旧花括号语法**，无过渡期。
- **文档/编辑器**：README.md、CHINESE.md 示例更新；VS Code grammar（`jeti.tmLanguage.json`）的 `namespace-decl` end 改为 `@endnamespace`（并加 `@endnamespace` 关键字高亮）。
- **回归**：cargo unit 41/41、test_all **195/207**（3 既有失败 + 空文件 `tests/circle.jeti`）、trace goldens 7/8（`arc_inject` 既有失败）。

### Checker：类方法访问实例 ivar 报错 ✅ (Aug 2026)
- **背景**：`+` 类方法体里写实例 ivar（如 `_radius`）时，checker 过去不拦，codegen 直接把 `self`（`NPClass *`，类对象）当实例生成 `((struct Cls *)self)->_ivar` → 运行期读类元数据垃圾内存 / 崩溃（`tests/circle.jeti` 早期版本因此「跑起来没输出」）。对应 ObjC 的编译错误 `instance variable '_x' accessed in class method`。
- **实现**：`Checker` 新增 `current_method_is_class` 字段（`check_decl` 的 `Method` 臂按 `is_class_method` 设置/还原）；`check_expr_inner` 的 `IvarRef` 臂中，若当前是类方法且 `obj` 为 `self` → `check_error("instance variable '_x' accessed in class method (self is the class, not an instance)")`。实例方法（`-`）与 `other->_ivar` 不受影响。
- **测试**：`crates/checker/src/lib.rs` 新增单元测试 `class_method_ivar_access_is_error` / `instance_method_ivar_access_is_ok`（手工构造 AST；checker 此前无单测，这是首批 2 个）。
- **回归**：cargo unit **43/43**（+2）、test_all **196/207**（`tests/circle.jeti` 修正为 `+run` 类方法后通过；仅剩 `diamond_impl`/`mega_types` 无 main、`double_release` 有意 over-release 共 3 既有失败）、trace goldens 7/8。

### jetic 跨平台构建健壮性 + C 编译器可选 ✅ (Aug 2026)
- ✅ **double-link 修复**（`compile_to_binary`）：普通模式下曾同时加 `-ljeti`（若找到 `libjeti.a`）**和** `runtime.c` → 重复符号。改为**二者择一**（优先静态库，否则 bundle 的 `runtime.c` 源码）。
- ✅ **bundle 根目录健壮解析**：新增 `resolve_bundle_root()`，取代原来硬编码的「exe 往上 3 层 parent」。候选顺序：`$JETI_HOME` → exe 的各父目录（bundle `PREFIX/bin/jetic`、dev `target/<profile>/jetic`）→ **exe 自身目录** → `.`；取第一个含 `include/jeti/runtime.h` 的。修掉了「exe 同目录存在陈旧 `include/` 副本时被优先选中」的坑（曾导致 `q.jeti` 因用旧 Foundation 缺 `NPNumber` 而编译失败）。
- ✅ **build.rs 自包含副本修复**：build.rs 一直找的是不存在的 `install-pkg.sh` → 拷贝块长期失效；改找 `install.sh`。并新增 `emit_rerun_for_dir()` 对 `include/` 下**每个文件**发 `rerun-if-changed`（目录级指令抓不到既有文件的**内容**改动），保证 `target/<profile>/include` 与源码同步（`NPNumber` 曾被漏拷）。
- ✅ **临时文件跨平台**：run 模式 `/tmp/<stem>` → `std::env::temp_dir()` + `std::env::consts::EXE_SUFFIX`；compile 模式默认 `a.out` → `a.out{EXE_SUFFIX}`。
- ✅ **C 编译器可选**（为 Windows 铺路）：新增 `select_c_compiler(backend) -> Vec<String>`（支持多词命令）—— `$JETI_CC` 覆盖全部；`-backend gcc` → `gcc`；否则 **Windows 默认 `zig cc`**（自带 libc 的单一工具链，`windows-gnu`），其它平台默认 `clang`。`compile_to_binary` 增加 `cc: &[String]` 参数，拆分 program + 前置参数（`Command::new("zig").args(["cc", …])`）。
- ✅ **`zig cc`/LLD 重复符号修复**：`runtime.c` 的 `JETI_CLASS_$_jeti_root` 定义缺少 `__attribute__((weak))`（注释却写"defined weak"）。clang/macOS 会把两处 tentative definition 当 common 合并，但 **LLD（zig cc）严格报 `duplicate symbol`**。加 weak 后 `zig cc` 通过（weak + strong tentative 合并）。
- ⚠️ **Windows 仍未做**：`runtime.c` 的 `__thread` 需 MSVC 兼容（`__declspec(thread)`；zig cc 用 `__thread` 没问题）；建议 Windows 端**捆绑 `zig`** 作为 `zig cc` 后端（jetic 直接调用它，无需用户装 LLVM/MSVC）。
- ⚠️ **`.gitignore` 隐患（已发现，待修）**：第 12 行 `jetic` 模式会匹配任意路径段，导致 **整个 `crates/jetic/` 未被 git 跟踪**（`git ls-files crates/jetic` 为空，`!! crates/jetic/`）；应改为 `/jetic`（仅根目录二进制）。另 `AGENTS.md`/`todo.md` 也在 `.gitignore` 中（未跟踪）。
- ✅ **回归**：cargo unit 43/43、test_all **198/209**（同上 3 既有失败不变）。

### 健壮性专项：多 TU + 错误恢复 + 死代码清理 ✅ (Sep 2026)

本轮针对此前会话遗留的 bug 清单（#2~#9）系统性收尾。核心结论与产物：

#### Bug #2 — parser 吞掉 `@interface`（真修）
- **根因**：`parse_class_implementation` 不消费 `@implementation Base : NPObject` 的父类后缀。空方法体时循环撞上 `:`，fallback 逐 token 跳过，把 `: NPObject @end @interface Sub …` 一路吞到下一个 `@end`——`Sub` 的 interface 消失，binder 报 `cannot find class 'Sub' for @implementation`。
- **修法**（`parser.rs`）：按 `parse_class_interface` 同样方式消费可选 `: Super` 并记录进 CST。
- **回归测试**：`crates/parser/tests/impl_superclass_suffix.rs`（3 用例）。

#### Bug #7 — 跨 TU vtable 布局错配（复现 + 启动期检测）
- **复现**：两个 TU 方法集不同 → 各自 `struct jeti_vtable` 布局不同 → 链接器 weak 合并 vtable 实例 → 派发按错误偏移读函数指针。链接顺序反转即 exit=139。
- **分析**：彻底修复需要跨 TU 稳定的槽位分配；任何依赖"集合"的方案（含纯哈希，73 selector 在 N=4096 仍有碰撞）都无法跨 TU 稳定，属大改，暂不做。
- **落地**：把静默内存损坏变成启动期清晰致命错误——布局签名（FNV-1a over 排序后的实例方法名）作为 `struct jeti_vtable` **第一个成员 `__sig`**（随实例一起 weak 合并，赢家实例携带赢家布局的签名）；每个 TU 发射 `__attribute__((constructor))` 校验（**不能**放 `jeti_metaInit`——它被 weak 合并，只有一个 TU 的副本执行，自检永远通过，这是踩过的坑）；不一致时打印双方签名 + 方法列表 + TU 文件名后 abort；无 `<stdio.h>` 的 TU（含 freestanding）走 `__builtin_trap()`。
- **单测**：`crates/codegen/src/codegen.rs` 的 `vtable_sig_tests`（3 个）。

#### Bug #8 — 类元数据撞车（已自然修复）
`UI::App`/`Game::App`、顶层 `App`/`UI::App` 实测正常，元数据正确编码为 `JETI_CLASS_$_UI__App` 等独立符号。

#### 多 TU 测试套件 `tests/multi_tu/`（9 场景，见其 README.md）
- `./run_multi_tu.sh`（可按名字过滤；`JETIC=` 指定二进制）。
- 覆盖：basic / inheritance / generic / namespace / block / protocol / arc / class_method / **负例**（`EXPECT_FAIL`+`EXPECT_FAIL_MATCH` 断言"失败且失败得清楚"）。
- **暴露并修复的真实缺陷 — 协议方法丢失**：`@protocol` 的 required 方法从不并入 conformance 类的方法列表 → 只有 `@interface`（无 impl）的 TU 编出的 vtable 比实现 TU 少槽位 → 跨 TU 必错配。修法：binder 新增 `propagate_protocol_methods` 后处理 pass，把协议 required 方法的**声明克隆进** conformance 类的 `@interface`（CST 级注入，因为 codegen 从 AST 方法列表建槽位）；同时 `SymbolData::Protocol.required_methods/optional_methods/parents` 首次被真正填充（此前从未写入）。
- **配套修复 — vtable 引用不存在的实现**：协议方法进槽位后 codegen 会引用 `Dog_speak`，但类未实现 → undefined symbol。新增 `EMITTED_METHODS` 集合（权威来源=最终 `unit.decls` 里带 body 的函数，覆盖 @synthesize getter 等 `method_bodies` 覆盖不到的路径），vtable 槽位对未发射的方法填 NULL（**槽位保留、引用不发**——这正是保布局稳定的正确语义）。曾因用 `method_bodies` 作来源漏掉 @synthesize 引发 50 个回归，改用 `unit.decls` 后归零。
- test_all 的 glob 已排除 `tests/multi_tu`（其 `.jeti` 是库文件，standalone 编译必然"无 main"，产生 18 个幻影失败）。

#### Parser 错误恢复 ✅（AGENTS.md 阶段 1 计划落地）
- **根因（隔行吞咽）**：`consume()` 失败时无条件 `advance()`——缺 `;` 时吃掉下一行的 `int`，剩 `b = 2` 被解析成无害赋值 → 每隔一个错就消失一个。**修法：`consume` 失败不再前进**，让恢复循环从真实位置重同步。
- **恢复循环**：`parse_translation_unit` / `parse_compound_statement` 均接入 `synchronize()`（返回 token offset 供前进守卫）；`synchronize` 同步点扩充了**类型关键字**（否则 `int a = 1 \n int b = 2` 会一路跳到 EOF）与 `@namespace`/`@using`/`#import` 等；声明/语句**成功返回但 panic_mode 置位**时只重置不跳过（解析器已停在下一个声明的合法位置，跳过反而吞掉它）。
- **效果**：N 行缺 `;` 精确报 N 个错、行号准确、无 `expected '}'`/EOF 级联噪音（此前一次编译只报一个错）。
- **回归测试**：`crates/parser/tests/error_recovery.rs`（4 用例）；新增 `Parser::error_count()` 访问器。
- **教训**：修这类 bug 别猜，用临时 debug hook 打印循环迭代（`[loop] start_off / -> Some / panic`）一眼定位；行号定位的批量删改**必然**踩中过期行号（本轮 parser.rs 被切坏一次，用 `git diff` 对照 HEAD 逐处修复，最终 diff 与有意改动逐一核对）。

#### 死代码清理 + `#![deny(dead_code)]`（18 crates）
- 删除确认无引用的死函数：`binder::params_to_method_sym`、`checker::is_numeric_type`、`codegen::{needs_subclass_cast, emit_vtable_method_cast, objc_instance_needs_arrow}`、`lexer::remaining`、`parser::{peek, add_macro/lookup_macro/resolve_macro_int, keyword_to_type_prim}`（宏机制字段一并删）、`pipeline::dump_ast_*` 三件套。
- 70 → 0 warnings：unused import（`cst::std::fmt`、`codegen::jeti_symbol::*`）、arc 里 6 处多余 `unsafe`（`&mut *inner` 不需要）、dead store（`is_float`/`has_args`/`vtable_class` 种子）、`FNPtr_TYPEDEF_NAMES`→`FNPTR_TYPEDEF_NAMES`、 unreachable `_` 臂加 `#[allow(unreachable_patterns)]` + 注释（新变体的安全网）、`synchronize` 当时以 `#[allow(dead_code)]` 标注保留（现已接入）。
- **全部 18 个 crate 的 `lib.rs` 加 `#![deny(dead_code)]`**：死代码从此是编译错误而不是静默腐烂；有意保留的用 `#[allow(dead_code)]` + 注释说明谁会用。
- `crates/arc/src/ownership.rs` 已在此前会话删除，无需处理。

#### 诊断信息修正（#4）
vtable 错配的 runtime 诊断原来建议"re-run jetic on ALL .jeti files"——**是错的**（09 负例证明方法集本来就不同，重跑无济于事）。改为解释机理（per-TU 布局 + weak 合并）+ 指出真正原因（方法只写在 `@implementation` 没进共享 `.jth`）+ 两种解法（声明进头文件 / 单 TU 构建）+ 指向 `tests/multi_tu/`。

#### 回归基线（本轮结束时）
- `cargo test --workspace`：**55 passed / 0 failed**（51 + 4 error_recovery；此前新增 impl_superclass_suffix 3 + vtable_sig 3）
- `python3 test_all.py -j8`：**211/222 passed**，3 个既有失败不变（`diamond_impl.jeti`/`mega_types.jeti` 无 main、`double_release.jeti` 故意崩溃），8 canceled 交互式
- `tests/multi_tu/run_multi_tu.sh`：**9/9**
- `tests/golden/28_refcount_trace/run_trace_golden.sh`：**8/8**（`.out` 已重新生成；脚本改为自动探测 release/debug 二进制，`arc_inject` 的 release 顺序变化来自 RefVal 状态机重写，语义不变）
- `crates/parser/examples/cst_dump.rs` 保留（`--inline` 模式可复刻 pipeline 预处理流调试）
- 待清理：无（仓库根 probe_* 临时文件已删）

#### 剩余已知问题（未在本轮处理）
- **#5（容器泛型）**：`NPArray<T>` 单态化机制就绪但未接（同 `DataPack<T>` 模式）；泛型实例化由**使用点**驱动——lib TU 必须在方法体/变量声明里**命名** `Stack<int *>` 才会单态化（multi_tu/03 记录了这一限制），类方法返回类型里的 type_args 不被收集。
- for-in 语法：parser 尚无 `for (x in y)` 规则（`CstStmtData::ForIn`/emitter 已写好待接线）；`convert_stmt` 的 ForIn 分支当前不可达（已注释说明）。
- **#7 真修**（跨 TU 稳定布局）与 Foundation 容器库补齐（`NPDictionary`/`NPSet`/variadic 特判）仍在 backlog。

### 现代化六项 —— 已全部落地 ✅ (Sep 2026, 本会话)

> 状态：**已实现**（落地记录见节尾；下方规划保留作存档依据，勿重新论证）。本节是六项工作的规划出处（现状实证 + 方案 + 测试计划）。
> 规划时点回归基线（全部实测）：cargo test 102/102、test_all 257/265（2 个 `-F` 既有失败：`diamond_impl-F.jeti` 无 main、`double_release-F.jeti` 故意崩溃）、multi_tu 9/9、trace golden 8/8、eh_diff 7/7。

#### 实施顺序（建议）

| # | 项 | 性质 | 风险 | 理由 |
|---|-----|------|------|------|
| 1 | block 字面量返回类型推断 | 小修 | 低 | codegen 单点改 |
| 2 | block 捕获 TU 级名字匹配（高危） | 中修 | 中 | 与 #1 同文件（block 路径）连续做完一起回归 |
| 3 | `[super alloc]` 类方法坏 C | 中修 | 中 | 独立 |
| 4 | 命名空间泛型 receiver 不单态化 | 小修 | 低 | 收集器改造是 #5 的底座，先行一次到位 |
| 5 | `NPArray<T>` 真单态化 | 特性 | 中高 | 依赖 #4 的收集器 |
| 6 | 跨 TU vtable 稳定槽位分配 | 大改 | 高 | 独立且最大，放最后 |

---

#### #1 block 字面量返回类型推断（压测清单 #1）

- **现状（file:line 实证）**：BlockLit 发射点 `codegen.rs` ~L1796–1804，返回类型 `rt` 无来源时硬回退 `"void"` → `^(int a,int b){return a*b;}` 发射成 `^void`，调用处按返回值用即炸。
- **方案（codegen 推断，零新语法，显式类型继续优先）**：发射前扫 block 体 `Return(Some(e))`：取 `e.expr_type` 渲染串（checker 已标注；无标注按字面量类别兜底 int/float/char*）。多个 return 类型不一致 → 取第一个 + checker warning（文案实现时探针逐字校验）；纯 `return;` / 无 return → 维持 void。gcc/portable 展开路径与 clang 路径共用同一 `rt` 来源，改一处两端生效。
- **测试**：`tests/block_return_test.jeti`（推断 int / 显式 `^int` 优先 / 多 return 同型 / void 块不误伤）。

#### #2 block 捕获 TU 级名字匹配（压测清单 #2，高危）

- **现状（file:line 实证）**：`collect_block_vars_from_stmt`（`codegen.rs` L4692 起）把**全 TU** 的 `__block` 变量名收进一个 HashSet；应用循环（L4796–4807）对**每个** Function 体跑 `rewrite_stmt(body, &block_vars)`——任何函数里与 `__block` 变量同名的参数/局部都被改写成 `name.__forwarding->__value`（AGENTS 原始复现：类方法参数叫 `base`，main 里 `__block int base` → 方法内 base 被污染）。
- **方案**：收集与应用都降到**函数粒度**——每个函数先收自身体内的 `__block` 集，再只改写自身体。遮蔽保护：函数参数名命中集合时该名字**跳过改写** + checker warning（文案实现时逐字探针校验）。类方法体与顶层函数一视同仁。
- **测试**：`tests/block_capture_scope_test.jeti`（原始复现不再污染 + 正向捕获零回归：既有 blocks 相关 golden/test 全绿）。

#### #3 类方法 `[super alloc]` 坏 C（压测清单 #5）

- **现状（file:line 实证）**：`AstExprKind::Super` → `CgExprData::Ident("super")`（`codegen.rs` L987–989）；实例方法 super 派发已工作（golden 覆盖）；类方法里 `[super alloc]` 落到实例 vtable/meta vtable 无 `alloc` 槽位 → 坏 C。类方法派发走直接调用、meta vtable 目前**只写不读**。
- **方案：类方法内 super 消息特判——读父类 meta vtable**：`((struct jeti_meta_vtable_<SuperFqn> *)((NPClass *)self)->isa->superclass->vtable)-><slot>(...)`（父类名静态可知；Foundation 父类必然内联，meta vtable 结构体该 TU 可见；槽位按父类 meta 布局静态取）。父类无该方法 → checker error（文案探针校验）。
- **⚠️ 实施前置探针**：核实类对象（`self` 为 `NPClass *`）的 `isa->vtable` 是否即 meta vtable 实例（getClass/metadata 路径已依赖该机制）；若不是则给 NPClass 加 `meta_vtable` 字段（涉布局，所有构造点同步）。
- **测试**：`tests/super_alloc_test.jeti`（子类类方法内 `[super alloc]`/`[super new]` 继承链）；`tests/stress/` 裸机套件回归不破（根类路径）。

#### #4 命名空间内泛型 receiver 不单态化（压测清单 #10）

- **现状（file:line 实证）**：`generic_instantiations` 收集（`codegen.rs` L4217 起）——`collect_instantiations_expr` 只匹配 **MsgSend receiver 为 VarRef 且类型渲染串含 `<...>`**，再 `parse_generic_type_string`（L4232）从**字符串**反解；命名空间名 `Metal::Box<int *>` 反解/查表不中 → 泛型版签名派发。`name_flat`（L398） mangling 已支持 `::`→`__`。
- **方案**：收集器从"字符串反解"改为"**结构化 type_args 直读**"——遍历所有携带 `type_args` 的 `AstType` 节点（expr_type、声明类型、参数、返回类型、ivar）按 (fqn, args) 直接注册。**此收集器同时是 #5 的底座，一次改造两处受益**；类方法返回类型里的 type_args（已知限制 #5 后半句）同批补上。
- **测试**：`tests/generic_ns_test.jeti`（`Metal::Box<int *>` 单态化：生成 C 中 `Metal__Box_int` >0 次出现 + 调用走特化签名）。

#### #5 `NPArray<T>` 真单态化（容器泛型，backlog #5）

- **现状**：`Foundation/NPArray.jth` 无 type_params（`@interface NPArray : NPObject`，ivars `_items/_count/_capacity`）；显式 `<T>` 被擦除 + checker warning（前会话已落地）；单态化机制（`DataPack<T>` 模式）就绪未接。
- **核心决策（待拍板）**：
  1. **裸拼写零变化**：`NPArray`（无实参）维持现状擦除 + 现有符号名，**不**生成 `NPArray_NPObject_ptr`——数百个既有测试/示例零迁移；显式 `NPArray<X>` 才实例化。
  2. **方法签名特化**：特化副本中 `objectAtIndex:`/`firstObject`/`lastObject` 返回 `X`；`addObject:`/`setObject:atIndex:` 参数收 `X`；ivars 布局不变（`_items` 仍 `NPObject **`）→ 特化与基类**二进制同构**。
  3. **赋值兼容**：`NPArray<X> → NPArray` 隐式允许（同布局）；反向显式 cast。`@[...]` 字面量全元素同型 → 实例化该型，混合/含 id → 裸 NPArray。
  4. `NPMutableArray<T> : NPArray<T>` 泛型继承同批落地（parser 已支持 `: Super <T>` 形态）。
  5. checker 下标改写（`a[0]`→`[a objectAtIndex:0]`，已实现）自动获得 `X` 返回类型，零额外改动。
- **测试**：golden `40_nparray_generic`（`NPArray<NPString *>`：`objectAtIndex:` 返回 `NPString *`，生成 C 实证特化符号；往 `NPArray<NPString *>` 塞 int 被 checker 拦截；裸 NPArray 混用零回归）；负例 `tests/negative/nparray_generic_mixed.jeti`。
- **⚠️ 跨 TU**：NPArray 方法集不变（只加 type_params）→ vtable 槽位集合不变、`__sig` 不受影响；以 multi_tu 全绿为准。

#### #6 跨 TU vtable 稳定槽位分配（backlog #7 真修）

- **现状**：槽位 = TU 内全部实例方法名**全局排序**后的位次（Fix 4；排序实现于 codegen.rs，`.sort()` 共 7 处命中）。跨 TU 方法集不同 → 位次不同 → weak 合并后错位。现有 `__sig`（FNV-1a over 排序方法名）只是**启动期 abort，不是修复**。
- **方案比选（否决理由记录在案）**：
  - ~~B：固定大小 name-tagged 哈希表 + 运行时探测~~ → 索引依赖合并后表内容，派发变运行时查找 = 变相 msgSend，**违背静态派发铁律** → 否决。
  - ~~C：全程序两遍编译（先扫全源收集方法集）~~ → jetic 单 TU 流水线模型重写 → 否决。
  - **A（推荐）：append-only 槽位清单**：`jetic --slots <manifest>`——清单存 方法名→槽位 稳定分配（新方法只追加 max+1，**永不重编号**）；编译时读清单定槽位、退出回写新增；`__sig` 从"布局哈希"升级为"逐方法 (name→index) 哈希"，错配时能报**哪个方法**错位。无清单 = 回退现状（排序布局 + 启动检查），零行为变化。
- **已知限制（如实）**：清单跨"不同时间编译的 TU"仍需重编译纪律（清单版本不符 → 启动期报错不变）；真保证靠构建系统同清单编译——multi_tu runner 配套改造演示。
- **测试**：multi_tu 第 10 场景（带清单跨 TU 方法集不同 → 链接运行正确；无清单同场景 → 启动期报错照旧）。

#### 拍板清单（逐项回 OK / 改意见即可）

1. 实施顺序表如上？
2. #1 block 多 return 类型不一致 = warning + 取第一个？
3. #2 遮蔽名跳过改写 + warning？
4. #3 super 类方法走 meta vtable 读（先探针核实 `isa->vtable` 指向）？
5. #5 `NPArray<T>` 五条核心决策（尤其"裸拼写零变化 + 特化二进制同构"）？
6. #6 槽位方案 A（append-only 清单）？

#### 落地记录 ✅ (Sep 2026, 本会话) — 六项全部按建议方案实施

> 回归基线（全部实测）：cargo test **102/102**、`cargo build` **0 warning**、test_all **264/272**（较规划基线 257/265 新增 7 个用例**全部通过**；2 个失败仍为既有 `-F` 基线：`diamond_impl-F.jeti` 无 main、`double_release-F.jeti` 故意崩溃；6 canceled 交互式不变）、multi_tu **10/10**（新增第 10 场景）。基线 257 个用例零回归——`NPArray` 裸拼写零迁移实锤。

| # | 项 | 落地 | 验证 |
|---|-----|------|------|
| 1 | block 字面量返回类型推断 | codegen BlockLit 发射点推断 return 类型；显式类型优先，void 块不误伤 | `tests/block_return_test.jeti` |
| 2 | block 捕获函数粒度 | `__block` 收集与应用降到函数粒度；同名参数/局部（遮蔽名）跳过改写并告警 | `tests/block_capture_scope_test.jeti` |
| 3 | 类方法 `[super alloc]` | 前置探针核实类对象 `isa->vtable` 指向 meta vtable 后，类方法内 super 消息改读**父类 meta vtable** 槽位 | `tests/super_alloc_test.jeti` |
| 4 | 单态化收集器结构化直读 | 弃"字符串反解"，遍历携带 `type_args` 的 `AstType` 节点按 (fqn, args) 注册；命名空间内泛型 receiver 单态化打通 | `tests/generic_ns_test.jeti` |
| 5 | `NPArray<T>` 真单态化 | 按五项核心决策落地；`objectAtIndex:` 等在特化副本返回元素类型 `X` | golden/40 + `tests/negative/nparray_generic_mixed.jeti` |
| 6 | `--slots` 槽位清单 | append-only 清单定槽位，新方法只追加不重编号；跨 TU 布局一致性由清单保证 | multi_tu `10_slots_manifest` |

**#6 实施要点（本会话收尾）**：manifest 模式不再按 filter 缩小适用范围；清单里有而本 TU 未实现的方法在 vtable struct 发**占位 fn-ptr 成员**（槽位保留、引用不发——与既有 NULL 槽位语义一致），跨 TU 布局对齐；`tests/multi_tu/run_multi_tu.sh` 修路径解析 bug（先 `cd` 项目根再解析相对 `$0` 的 CASES_ROOT 会落空 → 改用预先解析的 `SCRIPT_DIR`），现在从任意目录调用都正确。

**连带清理**：`crates/jetic/src/pipeline.rs` 移除 unused import `ast_to_cg_unit`（`cargo build` 回到 0 warning）。

### 装箱字面量补全：`@(expr)` / `@YES` / `@NO` / `@'c'` ✅ (Sep 2026, 本会话)

- **缺口（实测探针确认）**：lexer 早有全部 token（`AtLParen`/`AtChar`/`AtBool`/`AtNumber`/`AtString`），但 parser 只接了 `@123`/`@1.5`（desugar 成 `[NPNumber numberWithInt:]`/`numberWithDouble:`）与 `@"..."`；`@(x+1)`、`@YES`、`@NO`、`@'c'` 只到 lexer，parser 零接线——实测报 `expected ';' after declaration (got '@(')` / `(got boxed)`。
- **现四个形态全部真支持**：
  - `@YES` / `@NO` → `[NPNumber numberWithInt:1/0]`（parser desugar，与 `@123` 同一 helper；BOOL 值归一化 0/1）
  - `@'c'` → `[NPNumber numberWithChar:'c']`（parser desugar；`NPNumber` 新增 `+numberWithChar:` / `-charValue`，`.jth` + `.jeti` 同批）
  - `@(expr)` → **checker 按操作数静态类型改写成对应工厂**：`double`/`float`→`numberWithDouble:`、BOOL→`numberWithBool:`、`char`→`numberWithChar:`、`long`/`long long`→`numberWithLongLong:`、其余整数（含 typedef 整数）→`numberWithInt:`。
- **为什么落在 checker 而不是 parser**：parser 没有类型；后端是 **C99**，不能依赖 `_Generic`。与既有"对象下标改写""struct `==` 改写"同一先例——复用 `MsgSend` 节点构造，下游静态派发／nil 守卫／SEL 常量零特判。
- **非算术类型报错（判断点①）**：`@(someObj)` → `illegal type 'NPString *' in a boxed expression — '@(...)' accepts arithmetic and BOOL values only`（对齐 ObjC 的 "illegal type in boxed expression"；Jeti 无字符串装箱，静默把指针当数值发出去会掩盖错误）。负例 `tests/negative/boxed_illegal_type.jeti`，报 `file:line:col` 精确。
- **顺带修掉的真 bug（探针实测暴露）**：checker 的 `ArrayLit` 臂只回 `id`、**从不遍历元素** → `@[ @(i + 1) ]` 里那个 `@(expr)` 永远拿不到 `expr_type`、不被改写，生成 C 为 `jeti_array_create(3, <NPNumber>, (i + 1), <NPNumber>)`——**裸 int 混进对象数组**；编译通过、运行期才炸（exit=1，且前 5 段输出正常后又静默退出，极具迷惑性）。修法：`ArrayLit` 臂逐元素 `check_expr`（元素同时拿到 `expr_type`）。
- **改动链**：lexer 零改动（token 已有）；parser（`@YES`/`@NO`/`@'c'` desugar + `@(` 分支 + `mk_npnumber_send` helper 收敛工厂发射）；CST/AST 新增 `Boxed` 表达式节点（elaborator 直转）；checker `maybe_rewrite_boxed_expr` + `ArrayLit` 遍历修复；codegen 兜底臂（`-fno-checker` 下按内层表达式透传）。
- **测试**：`tests/boxed_literal_test.jeti`（7 段：字面量／表达式／BOOL／char 往返／链式接收者 `[@(i * 2) intValue]`／数组字面量／值相等）exit=0 逐项正确；负例 `tests/negative/boxed_illegal_type.jeti`。
- **回归**：cargo test **103/103**、multi_tu **10/10**、test_all **267/275**（2 个 `-F` 既有失败不变、6 个 canceled 交互式不变）。
- ✅ **勘误（2026-09-30 探针实测推翻上一条记录）**："C 风格强转直接跟消息发送解析失败"**不成立**——详见下节「强转接消息发送：勘误」。
- ✅ **DictLit（`@{...}`）已落地**（本会话），并顺带挖出"多对字典根本解析不了"的 parser bug——详见下节「字典字面量 `@{...}` 落地」。

### 强转接消息发送：勘误 ✅ (2026-09-30, 本会话)

- **被推翻的记录**：上一节末尾曾记「C 风格强转直接跟消息发送 `(NPNumber *)[arr objectAtIndex:0]` 解析失败，测试已规避」。
- **探针实测（20+ 形态，`-rewrite-jeti` 与端到端 `jetic run` 双验）：全部通过**。含无空格 `(NPNumber*)[...]`、`(NPNumber *)([arr objectAtIndex:0])`、`(NPNumber *)(id)[...]`、三元、`if` 条件、`for` 初始化、块字面量内、方法体内（`return` 位置）、数组字面量元素、`[(NPNumber *)[...] intValue]` 作接收者、泛型目标 `(NPArray<NPNumber *> *)[arr copy]`、`@class` 前向声明；默认 ARC、`-fno-jeti-arc`（MRC）、`-fno-checker` 三种模式都通过。
- **为何不是"本会话刚修好"**：`git show HEAD:crates/parser/src/parser.rs` 的 cast 分支与工作树**逐字相同**（`parse_unary` 在本轮 diff 中只有 `@await` 一处新增），即这条"限制"在本会话之前就不存在。**教训与既有的"指定初始化器形态 4"同类**：判定"某语法未实现"必须跑最小探针，不能凭一次失败就归档。
- **真正的剩余限制（如实记录）**：cast 的**目标类型名必须已在本 TU 内注册**（`@interface`/`@class`/`@implementation`/typedef/`struct` tag 都会注册）。`Foo *f = (Foo *)p;` 在 `Foo` 从未声明时报 `expected ';' after declaration (got identifier)`——报文完全没提 cast，这才是容易被误记成"强转接消息发送失败"的形态。
- **为何不"顺手修"**：`(Foo *)p` 与 `(Foo) * p` 在 token 层**只差 `*` 的位置**，C 本身靠符号表消歧，jeti 单趟解析同样只能靠"类型名已知"判定。故这不是 bug，而是与 C 一致的约束。~~**但诊断可以更准**（留作后续）~~ → ✅ **已修（本会话）**：现在 `( IDENT * )` 这个 shape 的 `*` 在括号**内**、后面紧跟 `)`，解析器据此报 `unknown type name 'Foo' — declare it (@class / @interface / typedef) or include the header that declares it`，不再是级联报错。详见下面「C 类型名探测（符号表方案）」一节。
- **配套**：`tests/boxed_literal_test.jeti` 第 6 段已把当时规避的三行恢复为自然写法 `NPNumber *head = (NPNumber *)[arr objectAtIndex:0];`（连同注释更正），7 段输出逐字不变。

### 字典字面量 `@{...}` 落地 ✅ (Sep 2026, 本会话)

- **先修掉的真 parser bug（比记录的缺口更严重）**：`@{ @"a": @1, @"b": @2 }` 此前**解析失败**——`expected '}' after literal (got ':')`。根因在 `parser.rs` 的 AtDict 分支：dict 的**值**用了 `parse_expression()`，它会吃逗号运算符，把 `@1, @"b"` 吞成一个 `Comma` 表达式，于是第二对的 `:` 撞上"expected '}'"。单对能用（键用的是 `parse_assignment()`）。修法：值改用 `parse_assignment()`（数组字面量早就是这个层级）。**多对字典此前从未解析成功过**，所以上一节"只是 checker 不遍历元素"低估了缺口。
- **`@{}` = 空字典**（ObjC 语义：`@{}` 与 `@[]` 是两个字面量）；无冒号的非空 `@{a, b}` 仍保留宽容的数组回退。
- **Foundation 新增两个类**：`NPDictionary`（不可变）/ `NPMutableDictionary`（可变，继承存储 ivar 与查询 API：`count`/`objectForKey:`/`allKeys`/`allValues`/`copy`/`description`）。存储照搬 `NPArray`（两个平行对象数组 + count），查找是**线性扫描**——这些字典都很小，真上哈希表还得跨每个键类保证 `hash`/`isEqual:` 成对正确。
- **键相等语义 = 值语义（本轮用户拍板）**：`isEqual:` 在 `jeti_root` 上仍是**指针身份**（同 NSObject），但 `NPString`/`NPNumber` 各自重写为**值相等**（委托 `isEqualToString:`/`isEqualToNumber:`，ObjC 的 NSString/NSNumber 正是如此）。这是字典键可用性的语义保证（2026-09-30 勘误：字面量 interning 已落地，同内容字面量现为同一对象；值相等仍是语义保证）。**连带**：`tests/golden/32_foundation_dispatch/` 的期望 `eq other=0` → `eq other=1`，该目录 README 里"NPString 未重写 isEqual:"一段同步更正（root 默认仍是身份）。`NPDictionary` 自己也按内容实现 `isEqual:`（同 count 且每键映射到相等的值）。
- **checker**：`DictLit` 臂逐**键与值** `check_expr`（对齐上一轮的 `ArrayLit` 修复）——内层 `@(expr)` 装箱、对象下标改写因此都能触发；并且**拒绝非对象条目**：`illegal type 'int' in a dictionary literal — keys and values must be Objective-C objects`（对齐 ObjC 的 "collection element of type 'int' is not an Objective-C object"；裸标量发进 `jeti_dictionary_create` 的 vararg 会编译通过、运行期读垃圾）。负例 `tests/negative/dict_illegal_entry.jeti`，`file:line:col` 精确、键值两处都报。
- **顺带**：新增 `Checker::type_display()`——诊断里 `int` 拼成 `int` 而不是 `{:?}` 的 `Int`；装箱那条报错也改用它，`NPString *` 文案逐字未变。
- **codegen**：`@{k: v, ...}` → `jeti_dictionary_create(n, k1, v1, ..., kn, vn)`——**弱符号 helper，仿 `jeti_array_create`**，仅当本 TU 内有 `NPDictionary` 时发射；交替 key/value vararg，nil 键跳过（不给扫描留洞）、nil 值存 NULL。TU 内无 `NPDictionary` 时保留历史的 `NULL` 占位。
- **测试**：`tests/dict_literal_test.jeti`（9 段：多对 count／按**相等但不同**的字面量查键／空字典／`allKeys`/`allValues` 插入序／`@(expr)` 值与数字键的值语义／数组内嵌套字面量／可变字典插入-替换-删除／`copy` + 内容 `isEqual:`／`description`）exit=0 逐项正确；负例 `tests/negative/dict_illegal_entry.jeti`。
- **回归**：`cargo build --workspace` **0 warning**、cargo test **103/103**、multi_tu **10/10**、`tests/stress/{hosted,baremetal}` build+run 通过、test_all **263/275**（2 个 `-F` 既有失败不变：`diamond_impl-F.jeti` 无 main、`double_release-F.jeti` 故意崩溃）。
  - canceled **10**（上次记录 6）：逐个按源码与手动 `timeout 3` 核对，10 个全是**交互式/REPL** 用例（均含 `scanf`/`getchar`/`fgets` 或无限主循环，如 `timsort_test` 打印 "Enter numbers separated by spaces:" 后等输入，EOF 也不退），因此永远会撞 3s 的 `RUN_TIMEOUT`。与本次改动无关；passed+failed+canceled = 275 仍与基线总数对齐。
- ⚠️ **已知限制（如实）**：`NPDictionary`/`NPMutableDictionary` **未接泛型**（`NPArray<T>` 是泛型，容器里只有它们不是）——`K`/`V` 双参数替换与单态化是独立工作；`objectForKeyedSubscript:`（`d[@"k"]`）**刻意未声明**，因为 checker 的对象下标改写只认 `objectAtIndex:`，声明它反而误导。

### C 类型名探测（符号表方案）✅ (2026-09-30, 本会话)

**动机**：上节末尾记的"cast 目标名必须已注册"其实只是表象。真问题是 jeti 的"这是不是一个类型名"判定**两头都错**，与 C/ObjC 的消歧方式不一致。

- **真 clang 21 实测矩阵（`/tmp/objc_cmp/o*.m`，`clang -x objective-c`）**：

  | 输入 | clang 结果 |
  |---|---|
  | `@interface Foo @end` + `(Foo *)p` | 通过（`@interface` 名即类型名）|
  | `@class Foo;` + `(Foo *)p` | 通过（前向声明也算）|
  | `Foo` 从未声明 + `(Foo *)p` | `error: use of undeclared identifier 'Foo'` + 级联 |
  | `int Foo;` + `(Foo) * p` | 通过（乘法）|
  | `int Foo;` + `(Foo *)p` | `error: expected expression`，箭头指向 `)` |
  | `#include <stdio.h>` + `(FILE *)p` | 通过 |
  | `#include <signal.h>` + `(sigset_t)v` | 通过 |
  | `#include <time.h>` + `(time_t)v` | 通过 |
  | 本地头里的 `MyHandle`/`MyCallback`（typedef） | 通过 |

  **结论**：C/ObjC 判定"是不是类型名"**靠符号表**——typedef 名 + struct/union/enum tag（经典 lexer hack）；ObjC 在此基础上再加 `@interface`/`@class` 名。`Foo *f = (Foo *)p;` 在 `Foo` 从未声明时，**ObjC 里同样报错**。关键歧义：`(Foo *)p` 与 `(Foo) * p` 在 token 层**只差 `*` 的位置**；`*` 在括号**内**、后面紧跟 `)` → 不可能是乘法（`*` 不能以 `)` 为操作数）。

- **jeti 改动前的缺陷（实测）**：`(sigset_t)v` → `error: expected ';' after declaration (got identifier)`；项目本地 typedef 同样失败；更糟的是 `x * y;`（两个 int 变量相乘）被 `cst_dump` 证实**误解析成 `y` 的声明（类型 `x *`）**——clang 对同一段 0 error。旧桥是 `parser.rs` 顶部约 45 个硬编码 libc typedef 名 + `IDENT *` 形状猜测，两头都错。内部还不一致：未知类型出现在**声明**里时 jeti 是放行给 clang 的（`Foo *f = 0;` 转译成功、clang 报 `unknown type name`），只有**强转**这条提前拦死。

- **新模块 `crates/jetic/src/ctype_probe.rs`**（`lib.rs` 里 `pub mod ctype_probe;`）：
  - `scan_c_type_names(preprocessed: &str) -> Vec<String>`：自带 C tokenizer（保留标点邻接、丢注释/字面量/`#` 行标记），扫 `typedef` 声明取声明符名。**刻意不收集 struct/union/enum tag**——C 里 tag 不是类型名，收了会让 jeti 接受 C 会拒绝的代码。`NOT_TYPE_NAMES` 过滤关键字/属性噪声，`DECLARATOR_QUALIFIERS` 用于跳过 `(*` 与名字间的 `const`/`restrict` 等。
  - `probe_c_type_names(cc, includes, search_dirs, macros, arch, freestanding, nostdinc) -> Option<Vec<String>>`：写临时 shim（`#include <jeti/runtime.h>` + 透传的 `#include` 行），跑 `<cc> -E -x c -I<dirs> -D<macros> [-ffreestanding] [-nostdinc] [-arch a]`（两旗标独立，2026-09-30 拆分），扫 stdout。**失败/空结果返回 `None`**，调用方回退到 builtin + 形状猜测——绝不把能跑的构建变成报错。
  - 11 个单元测试；实测性能：三头文件 shim `clang -E` **40ms**，恢复 98 个 typedef 名（`FILE`/`size_t`/`sigset_t`/`time_t`/`va_list`/`off_t`/`pthread_attr_t`/`clockid_t` 全中）。

- **pipeline 串接**：`Pipeline` 新字段 `c_cc: Vec<String>` / `c_arch: Option<String>` / `no_ctype_probe: bool`；新私有方法 `c_type_names(&self, pre, macros, filename) -> (Vec<String>, bool)`（跳过条件：`no_ctype_probe || c_cc.is_empty() || pre.c_headers.is_empty()`；搜索目录 = **源文件所在目录优先** + `search_dirs`）；`transpile()` 改用 `Parser::with_c_type_names(&pre.resolved_jeti, &c_type_names, c_types_complete)`，`-v` 时打印 `[jetic] C type table: N names from M passthrough header(s)`。`main.rs` 在 `select_c_compiler` 后接线 `c_cc`/`c_arch`/`no_ctype_probe`。

- **parser 两个判定点**：
  - 新字段 `type_table_complete: bool`（`new()` 里 `false`）；新构造 `with_c_type_names(source, names, complete)`；新 `type_table_is_authoritative(&self)`。
  - `parse_primary` 的 `( ... )` cast 分支：`is_cast` 为假、当前 token 是 Identifier、表权威、`scan_cast_star_paren`（纯 textual lookahead：标识符后跳空白 → 至少一个 `*`（允许 qualifier 夹在中间）→ `)`）为真、且名字既非 type name 也非 type param时，报 `unknown type name 'X' — declare it (@class / @interface / typedef) or include the header that declares it`——**代替**以前的级联错。
  - `is_declaration_start()` 的 Identifier 分支：`IDENT IDENT` **永远算声明**（C 里相邻两个 primary 不成立）；`IDENT *` **只在表不权威时**才当声明猜测（否则 `x * y;` 是乘法）。

- **逃生舱**：环境变量 `JETI_NO_CTYPE_PROBE=1` 关闭探测（未加 CLI flag）。

- **测试**：`crates/parser/tests/type_table_resolution.rs`（7 个：乘法非声明／已知 typedef 指针声明／未知名的形状回退／头文件 typedef 强转可解析／权威表下未知强转目标报错／非权威表下不报错／cast lookahead 区分括号内外 `*`）；`ctype_probe.rs` 内 11 个；e2e `tests/c_header_typedefs_test.jeti`（`sigset_t`/`pthread_attr_t *`/`pthread_t` 强转）——探针开 → exit=0；`JETI_NO_CTYPE_PROBE=1` → **复现旧失败** `expected ';' after declaration (got integer)`（这是"曾经失败"的实锤对照）。

- **回归**：`cargo build --workspace` **0 warning**、cargo test **121/121**（+18）、multi_tu **10/10**、`tests/stress/{hosted,baremetal,interop}` build+run 通过、test_all **282/294**（2 个 `-F` 既有失败不变、10 个 canceled 交互式不变）。分母 294 = 121 cargo + 172 `.jeti` + 1 example；比上节基线的 275 多 19，正好是 **+18 新 Rust 测试**（ctype_probe 11 + type_table_resolution 7）**+1 新 e2e `.jeti`**——已逐项对齐，无隐藏失败。

- ⚠️ **仍开着的口子（如实）**：
  - **不收集 struct/union/enum tag 是刻意的**（见上）。
  - **probe 失败静默降级**到 builtin + 形状猜测（保可用性，代价是退回旧的误解析，只影响诊断质量）。
  - 旧的硬编码白名单仍保留在 `parser.rs` 顶部作非权威回退路径，未删。
  - 未加 CLI flag；未透传 `pre.c_headers` 的**内容**（只有原始 `#include` 行文本）。


### 泛型真检查（步 1–3）：T 扶正为签名一等公民 + checker 代入检查 ✅ (Sep 2026, 本会话)

> 路线拍板：**单态化 ⊕ 编译期检查**——保留既有 C++ 式单态化（专属类元数据/vtable/方法副本，README 不用改），补上 ObjC lightweight generics 式的编译期检查。运行期零变化，纯编译期收益。

#### 三步落地

| 步 | 内容 | 关键证据/决策 |
|---|------|--------------|
| **1** | `NPArray.jth`/`NPMutableArray.jth`（及对应 `.jeti`）元素位 `id` → `T`（`objectAtIndex:`/`firstObject:`/`addObject:` 等 15 处） | 前置探针实证 `T` 可直接写在方法签名（`MyBox<T>` → `MyBox_NPString_ptr_set_(NPString*, SEL, NPString*)`）；头文件原注释声称"specialization 用 T"但代码写 `id`——注释与代码不符，本步使其一致 |
| **2** | codegen 单态化替换从"渲染串 `NPObject *` 盲替换"收敛为 **`TypePrim::Param` 哨兵替换** | 盲替换误伤实证：特化副本里 `-copy`/`-description`（真 `id` 位）被串改成 `NPString * self`。修法：`T_PARAM_RENDER = "NPObject * /*T*/"`（注释哨兵，漏网也是合法 C）+ `normalize_t_sentinels_text`（发射出口文本级归一化幸存哨兵——CgStmt 树无廉价全遍历，文本级替换最完备） |
| **3** | checker：`MsgSend` 把接收者 `type_args` 代进方法签名 `Param` 位（实参 + 返回类型），复用 `types_compatible` 哲学 | 4 个历史漏检全变 error；裸拼写（无 type_args）完全擦除、**零迁移**（`/tmp/pcbare.jeti` 探针验证） |

#### checker 实现要点（勿返工）

- **`method_returns` 新表**：`collect_signatures` 顺带收方法返回类型（selector → declared return）。
- **代入门控**：实参检查仅当 **接收者带 type_args 且签名含 Param 位**（`type_has_param`）——两道门缺一不可：
  - 无 type_args 门 → 泛型类自己体内的 `[self addObject:...]` 会拿原始 `T` 与实参硬比（Foundation 全炸）；
  - 无 Param 门 → `method_params` 按 selector **全局**键控，`stringWithFormat:` 等非泛型签名会被泛型接收者误代入（首轮探针 7 处误报）。
- **返回类型代入**：接收者带 args → 代入；裸接收者 → **objectish 返回一律退回 `id`**，只留标量精度（`count`→`size_t`）。⚠️ 关键坑：全局 selector 表下 `-init` 返回 `NPMutableDictionary *` 会**污染所有类的 `[x init]`**（裸接收者拿错类型 → 13 处误报），objectish 退回 `id` 才是对的。
- **兼容判定**（`named_types_compatible`，两处共用：实参 + 声明 init）：
  - `id` 万能；两个具名类指针 → **名字相等或 superclass 链相关**（`is_subclass_of`，双向——下转型按 clang 惯例放行）；未知名（前向声明/外部 typedef）放行，绝不瞎猜；
  - **裸模板 ↔ 同基特化互赋允许**（`split('<')[0]` 相等即同基）——特化与基类布局逐字节相同（已实证），`VectorBuffer *` ↔ `VectorBuffer<X> *` 本就二进制同构；
  - 声明 init 的指针↔标量判定（`init_ptr_scalar_mismatch`）**两个方向都收窄**：标量→指针只拦**可证字面量**且豁免 `= 0`（C 空指针惯用法 `Foo *p = 0;` 误报 2 个测试）；指针→标量只拦**具体具名类**（裸 `id` 是回退类型，`[someId count]` 满天飞不能拦）。
- **连带修**：`arg_type_ok`/`init_ptr_scalar_mismatch` 变 `&self` 方法（需要 symtab 查 superclass 链）。

#### 用户拍板的三个语义决策 (2026-09-30)

| 决策 | 选择 |
|------|------|
| ① `NPArray<A>` vs `NPArray<B>` | **不变**（与 ObjC lightweight generics 一致） |
| ② 裸 `NPArray` → `NPArray<X>` | **warning（clang 同款档位）**——2026-09-30 二次拍板定案。首落地为双向静默，用户选"降为 warning"后实现 `init_generic_erasure_warning`：裸→特化报黄色 warning（`-Werror` 升级），特化→裸静默（丢承诺是安全的）。判据细节见下 |
| ③ `@[...]` 字面量推断 | **推断**（全元素同型 → `NPArray<X>`，混合/非类元素回退裸 `NPArray`）——2026-09-30 已实现，见下 |

#### 决策②③补全落地 ✅ (2026-09-30, 同会话)

**② 裸→特化 warning（`init_generic_erasure_warning`，声明 init 处挂载）**：
- 判据三要素（探针实证迭代两轮，勿再走弯路）：①泛型性看 **`type_args` 或 name 含 `<`**——`@using` 别名展开（`@using BoxOfStr = Box<NPString *> *;`）把 `<...>` 烧进 `name` 且 `type_args` 为空，只查 `type_args` 会漏别名形态；②方向是 **声明侧裸 ← 表达式侧特化**（`!v_gen || i_gen → None`——首版方向写反探针直接抓出）；③基类相同（`split('<')[0]` 比较，闭包写法有生命周期问题，用普通 let 绑定）。
- 验证矩阵：`NPArray *loose → NPArray<NPString *> *typed` 报 warning（exit 0）；`-Werror` 升级 exit=1；`@using` 别名同报；typed→裸静默 0 条；`static_generics_zero_cost_test.jeti`（裸 `VectorBuffer` → `PointPipeline` 别名）rc=0 零改动通过。
- ⚠️ 调试教训：临时 eprintln 钩子用 `JETI_DBG_ERASURE` 环境变量门控，定位完即删——方向反了的 bug 靠它一轮定位。

**③ `@[...]` 元素类型推断（`ArrayLit` 臂）**：
- 全元素同为**具体具名类指针**（`Named + is_pointer + name 无 <`）→ 返回 `NPArray<X>`（type_args=[X]）；空数组/混合/含 id/未知类型 → 回退 `id`（裸）——既有代码零迁移。
- **配套 1（NPNumber 工厂白名单）**：`@[ @1, @2 ]` 的元素 `@1` 是 `[NPNumber numberWithInt:1]`，而裸接收者的 objectish 返回已统一退回 `id`（修 `-init` 污染的副作用）→ 元素类型全变 `id`，推断静默失效。修法：`numberWith{Int,LongLong,Double,Bool,Char}:` 五个无子类无冲突的工厂直接返回 `NPNumber *`。
- **配套 2（`named_types_compatible` 补 type_args 比较）**：名字相等直接放行导致 `NPArray<NPNumber *>` ↔ `NPArray<NPString *>` 静默互赋（决策①被击穿）。修法：基类相同且两侧 type_args 等长时逐位 `arg_type_ok`（子类仍可过）；长度不等（一侧裸）→ 放行（布局同一）。
- **配套 3（`type_display` 带 type_args）**：报错从 `initializing 'NPArray *' from ... 'NPArray *'`（看不出差别）变为 `initializing 'NPArray<NPString *> *' from ... 'NPArray<NPNumber *> *'`。
- 验证：`@[ @"a", @"b" ] → NPArray<NPString *> *` rc=0；`@[ @1, @2 ] → NPArray<NPNumber *> *` rc=0；`NPArray<NPString *> *bad = @[ @1, @2 ]` 报错 rc=1 文案清晰；`@[ @"a", @42 ]` 混合静默回退。

#### 验证矩阵

- **4 漏检全拦**：`[m addObject:7]`（标量进 T 容器）/ `[m addObject:@42]`（NPNumber* 进 NPString* 容器）/ `int v = [m objectAtIndex:0]`（指针→标量）/ `NPNumber *bad = [strArr objectAtIndex:0]`（具名类不匹配）。
- **正例零误伤**：typed read（`NPString *s = [m objectAtIndex:0]`）、裸 NPArray 混用、`@42` 装箱、子类赋父类（`NPMutableString *` → `NPString *`）。
- **回归**：cargo 121/121、test_all **286/294**（2 个 `-F` 既有失败不变；较改动前 +4 通过）、multi_tu 10/10、stress 三套全过、step1 生成 C 与改前基线逐字节一致（裸 NPArray 路径）。

#### 残留（如实）—— 2026-09-30 二次更新（P0–P1 落地后）

- ~~③ `@[...]` 类型推断未实现~~ → **已实现**（见上"决策②③补全落地"）。
- ~~`-copy` 返回 `id`~~ → **已修**（P1，receiver-same-type 规则）：typed 容器上 `[m copy]`/`[m mutableCopy]` 返回**接收者同型**（含 type_args），`int bad = [m copy]`（指针→标量）与 `NPNumber *bad = [strArr copy]`（元素型不匹配）均被拦；裸接收者保持 `id`，零迁移。探针 `/tmp/pr_p1*.jeti` 四态全过。
- **多参数泛型代入 bug（P0，本会话探针发现并修复）**：`substitute_type_args` 原先一律取 `args[0]`，`Pair<A, B>` 的 B 位被代成 A 的实参 → **合法代码编译失败**（步 3a 引入的回归，修 `-init` 污染前 MsgSend 恒返 `id` 不会误报）。修法：Param 节点自带参数名（parser 保留，`parser.rs:480`）+ 符号表 `type_params` 顺序表（binder 已填，`binder.rs:388`）**按名对号入座**；查不到名回退 `args[0]`（单参数旧行为）。验证：`Pair<NPString *, NPNumber *>` 正例 rc=0；反例（`setFirst:@7`、`int x = [p second]`）两处错误精确命中；单参数 4 漏检探针 + 正例探针零回归。
- 子类元素进父类容器（`NPMutableString *` 进 `NPArray<NPString *>`）**可过**（superclass 链相关即放行）——**定性为设计决策，不改**：与 ObjC `__covariant` 读取侧行为一致；写侧仍受元素型检查约束（放进不该放的会被拦）。
- ~~`NPDictionary<K, V>` 双参数泛型未接~~ → **已实现**（P3，见下节"残留清单落地"）。
- ~~步 4（评估删对象容器单态化，纯 flash 优化）未动~~ → **评估已完成**（P4 量化账单：同程序特化比裸拼写 +42,848 字节（+41.5%），见下节；删与不删待用户拍板）。

### 残留清单落地（P0–P4）✅ (2026-09-30, 本会话)

> 用户拍板"按顺序全做，做完消掉待做清单"。全量回归：cargo 121/121、test_all 286/294（2 个 `-F` 既有失败不变）、multi_tu 10/10。

| 项 | 落地 | 验证 |
|---|------|------|
| P0 多参数泛型代入 bug | `substitute_type_args` 按 Param 名 + 符号表 `type_params` 顺序对号入座（查不到名回退 `args[0]` 单参数旧行为） | `Pair<NPString *, NPNumber *>` 正例 rc=0；反例两处错误精确命中 rc=1；单参数 4 漏检探针零回归 |
| P1 copy 接收者同型 | MsgSend 返回类型分支：`copy`/`mutableCopy` 且接收者带 type_args → 返回接收者同型（裸接收者保持 `id`） | typed `[m copy]` 元素型流经读侧 rc=0；`int bad = [m copy]` 与 `NPNumber *bad = [strArr copy]` 均拦 rc=1；裸 copy 零迁移 |
| P2 文档刷新 | AGENTS.md 残留节二次更新（③ 标已实现、copy 已修、子类协变定性为设计决策） | — |
| P3 `NPDictionary<K, V>` | AST `Class.type_params` 字段（CST→elaborator→AST）+ 四个 Foundation 头/实现签名 K/V + codegen **按参数名哨兵**（`/*K*/`/`/*V*/`）+ `name_flat` 多参数分隔符修复 + 发射出口哨兵归一化泛化为任意 `/*名*/` | 七探针：C 编译 rc=0、端到端 `1 2`、正例 rc=0、两反例 rc=1、裸拼写零迁移、既有 dict 测试 rc=0 |
| P4 单态化 flash 账单 | 同程序裸 vs 特化双编译量化 | 见下表 |

**P3 实施要点（勿返工）**：
- 哨兵**按参数名**渲染（`ct.name`/`t.name` 保留 K/V 名，parser.rs:480）；单态化循环从模板 `ClassInfo.type_params`（两个构造点均填充）配对 `(哨兵, 实参)`，**多趟顺序替换**——哨兵互异不串扰，`substitute_cg_stmt` 50 处递归签名零改动。
- `name_flat` 多参数 bug：分隔符 `_` 原在逗号处 push 到参数**前面**（首参数带前导 `_`、参数间无分隔）→ `NPDictionary__NPString_ptrNFNumber_ptr`；修为 arg 后置分隔。
- `normalize_t_sentinels_text` 从只认 `/*T*/` 泛化为**严格标识符体**的 `NPObject * /*名*/` 才归一化——任意 `/* 注释 */` 透传不受影响。
- 工厂方法（`+dictionary` 等）保持擦除 `id`/裸 `NPDictionary *` 返回——避免模板内嵌套泛型返回类型（`NPArray<K>`）引发模板递归实例化；`isEqual:` 按 ObjC 惯例收 `id`。

**P4 量化账单（flash 拍板材料）**：同程序（array/dict/foreach 全用一遍）裸拼写 103,176 字节 vs 特化拼写 146,024 字节——**+42,848 字节（+41.5%）**，源码级 +1167 行 C / +75420 字节，全部是特化方法副本 + 类/vtable/meta-vtable 实例。布局逐字节相同 → RAM/速度收益 0，纯 flash 成本。删与不删（对象容器退化成纯编译期视图）**权利在用户手里**，本会话不动。

#### 残留（三次更新后）
- 待用户拍板：步 4（删对象容器单态化，P4 账单如上）。
- 多 TU 泛型特化的一致性依赖各 TU 实例化集合相同（既有 `__sig` 启动期检查兜底，未变）。

### `switch` 模式匹配（Swift 风格 `case`）✅ M1 落地 (Sep 2026, 本会话)

**动机**：路线图把 `match` 否决掉的理由是"ObjC 对多路分发没有语法 construct，答案是 `isKindOfClass:` + 强转方法链"。Swift 后来证明了一个**窄得多的中间形态**可以纯语法化：**只改 `switch` 的 case 标签**（不动语句家族、不造新语句、不造穷尽性分析）。这既拿到了方法链的表达力，又避开了 `match` 的全部代价。

**语法**：
```objc
switch (subject) {
    case NPString *s:                 // 类型绑定 → isKindOfClass:，臂内 s 已是 (NPString *)subject
    case > 100:                       // 悬空比较 → subject > 100
    case > 0 && < 100:                // 区间
    case @"literal":                  // 对象字面量 → isEqual:（值语义）
    case 1, 2, 3:                     // 多值常量（顺带修好的既有缺口）
    case T *x when x.count > 3:       // when guard（上下文关键词）
    default:
}
```

**落地方式（铁律：不引入新 IR，codegen 零改动）**：parser 识别 case 标签形态 → 含任一模式臂时整个 switch 降级为 `SwitchPat`（平面臂表）→ **新 crate `crates/pattern`**（pipeline **Step 3.95**，在 ARC/checker 之前）把 SwitchPat 改写成 `goto` 状态机 + C 标签：

```c
{ NPObject *__jeti_sw = (NPObject *)subject;   /* 对象 subject */
  __auto_type __jeti_sw = subject;             /* 标量 subject */
  if (jeti_isKindOfClass((NPObject *)__jeti_sw, &JETI_CLASS_$_NPString)) goto __jeti_case_0_1;
  if (__jeti_sw > 100) goto __jeti_case_0_2;
  __jeti_case_0_1: { NPString *s = (NPString *)__jeti_sw; ... } goto __jeti_sw0_end;
  __jeti_case_0_d: { ... }
  __jeti_sw0_end: ; }
```

**实现要点（勿返工）**：
- **每个降级 switch 独立标签命名空间**（`__jeti_sw{id}_*`，`SWITCH_SEQ` 原子计数器）——C 标签是**函数作用域**的，嵌套模式 switch 否则会发重复 `__jeti_case_0` 名（硬 C 错误）。
- **subject 只物化一次**；标量用 `__auto_type`（eh pass 先例），**对象用 `NPObject *` + 强转**——本 pass 跑在 checker **之前**、静态类型未知，`__auto_type` 会继承指针类型并被 checker 判"对象初始化标量"。判据 = 臂表里有 `Bind` 或对象字面量臂。
- **`break` 作用域感知改写**为 `goto __jeti_sw{id}_end`：臂体内的嵌套循环/内层 switch 的 `break` 不受影响（只在不属于任何内层 loop/switch 作用域时才改写）。
- **fallthrough 保持 C 语义**：臂体末尾**不追加**隐式跳转，无 `break` 即落入下一臂（用例 7 验证 `t7=11`）。
- **`when` 是上下文关键词**（lexer 不硬编码，仿既有的 `in` 降级先例）——`int when = 1;` 照常编译。
- **Bind 形态判据用源切片扫描**（`scan_case_bind_shape`：标识符 `*` 标识符 [+ 可选 `when` guard] → `:`），不用投机 parse+回溯。

**分类判据（parser `parse_case_pattern`）**：

| 形态 | 判据 | 降级测试 |
|------|------|----------|
| `T *name` | 源切片扫描确认 Bind 形状 | `jeti_isKindOfClass(...)` + 臂内 `(T *)subject` 别名声明 |
| `>` `<` `>=` `<=` 开头 | 悬空比较 | 表达式树里替换 subject |
| `@"..."` / `@N` / `@YES` / `@'c'` / `@(expr)` | `is_object_literal` | `[subject isEqual:<lit>]` |
| `when <expr>` | 上下文关键词 | 追加 `&& <expr>` |
| 其他 | 普通 C 常量 | `subject == <const>` |

⚠️ **`is_object_literal`（parser）与 `is_object_literal_expr`（pattern crate）必须一致**——不一致时全字面量 switch 会被路由到 C 路径，发 `case <装箱表达式>:`（实测 `case @42:` 曾生成 `case [NPNumber numberWithInt:42]:`，clang 报 "integer constant expression must have integer type"）。两处现已同款覆盖 `@String` / `Boxed` / `NPNumber` 工厂消息发送。

**M1 限制（如实记录）**：
- **常量臂与模式臂不可混用** → 报 `switch mixes plain constant 'case' arms with pattern arms — M1 cannot lower both in one switch; split them into separate switches`（负例 `tests/negative/switch_mixed_arms.jeti`）。C 路径需要原始 body，模式路径丢弃它。
- **非恒定标签被拦截**：`case [obj msg]:` / `case f():` / `case a = b:` 报 `case label is not a constant expression`（负例 `tests/negative/switch_case_msg_send.jeti`）。⚠️ 首版 `expr_is_non_constant` 漏判消息发送本身（只递归操作数）→ 探针抓到仍透传进 C，已修为"消息发送/调用自身即非恒定"。
- **嵌套模式的 case 作用域**未做跨层分析（内层 case 归内层）——与 C 一致，未专门验证。
- 多值 `case 1, 2, 3:` 是**顺带修好的既有缺口**：旧路径用 `parse_expression` 吃逗号，发 `case (1, 2, 3):`。改为 `parse_assignment`（数组字面量早就是这个层级）。

**连带修掉的既有 bug（stress hosted 暴露）**：**for-in desugar 把集合别名错用元素类型**——`T *__jeti_fi = <coll>` 里的 `T` 抄的是**循环变量**类型，生成 `NPString * __jeti_fi = arr;`，于是任何"元素类型比集合更具体"的 for-in（如 `for (NPString *item in npArray)`）都被 checker 拒。集合别名改用 `id`（与元素类型无关——集合是任何提供 `count`/`objectAtIndex:` 的对象）。既有 golden/30 全用 `NPObject *` 元素类型，**恰好掩盖了这个 bug**，是 stress hosted 第一个暴露它。

**M2 候补**：常量臂与模式臂混合降级（保留原始 body 按臂分流）/ 穷尽性分析（对象 subject + Bind 臂缺 `default` 警告）/ `case is` 与 `case as` / 或模式（`case NPString *s, NPNumber *n:`）。

**测试**：golden `tests/golden/42_switch_pat/`（7 场景 + README 记 desugar 形态与判据表）、`tests/switch_pat_test.jeti`；负例 `tests/negative/switch_mixed_arms.jeti` + `switch_case_msg_send.jeti`（均已从 test_all glob 排除）。
**回归**：cargo test **121/121**、build **0 warning**、test_all **288/296**（2 个 `-F` 既有失败不变：`diamond_impl-F.jeti` 无 main、`double_release-F.jeti` 故意崩溃；6 canceled 交互式）、multi_tu **10/10**、stress 三套全过（baremetal/hosted/interop）。

### 四项全语法压测遗留修复 ✅ (Sep 2026, 本会话) — catch 语义回正 / 成员 fn-ptr 调用 / 字面量 interning / 反向串检查

> 全语法压力测试（full_syntax_test.jeti）沉淀的四个已知缺口/偏差，本会话全部开修。回归基线（最终二进制实测）：cargo test **137/137**、test_all **309/317**（2 个 `-F` 既有失败不变）、multi_tu **10/10**、trace golden **8/8**（JETIC= 指向 debug 新二进制）、eh_diff **7/7**（JETIC= 绝对路径）、stress 三套全过（baremetal/hosted/interop）。

| # | 问题 | 修法 | 验证 |
|---|------|------|------|
| 1 | **typed `@catch` 是精确 isa 匹配**：抛子类、catch 父类 → 未捕获（与 ObjC isKindOf: 链相悖） | sjlj codegen 的 catch isa 检查从 `isa == &CLASS` 改为 `__jeti_eh_isa(...)` 调用（runtime.c:30 走 `jeti_isKindOf` superclass 链、nil 安全；eh checked 与裸机 runtime 本来就是该形态，零改动）。臂序语义探针四态实证：first-match 消费防双捕（11）、sibling miss 落外层（33）、父实例不中子臂（44）、`id` 臂全接（55） | `/tmp/px.jeti`（B=1）、`/tmp/px2.jeti` 四态、`-eh checked` 同结果；full_syntax §4.8 改回 ObjC 语义（父类臂在前接子类，期望 221→211） |
| 2 | **struct 成员 fn-ptr 直接调用 `w.cb(3)` 被整个丢弃**（生成 `int a = (3);`，只剩实参） | 根因：elaborator 只对 `IvarRef` callee 保留 `FuncCall.callee`，`w.cb`（Dot→PropRef）被降级丢弃 + name 空串。修法两处：elaborator.rs `Call` 分支把 `PropRef` 也纳入 callee 保留；codegen `render_callee_expr` 新增 `PropRef` 渲染臂（镜像既有 PropRef 发射臂的 `.`/`->` 判定；ObjC property getter 形态返回空串不误伤） | dot `A=6`、arrow `B=8`、`-backend gcc` portable 同结果；生成 C `int a = w.cb(3);` 原样；full_syntax §1.4 别名中转 workaround 删除，直接 `w.cb(3)` |
| 3 | **字符串字面量无 interning**：同内容 `@"..."` 每处是新对象，`containsObject:` 字面量直查落空 | 宿主模式 `jeti_stringFromCstr`（codegen 发射的 weak helper）加 256 槽 intern 表：同内容返回同一对象；表持有 +1 永不释放（ObjC 常量串语义：ARC unretained、MRC 不得手 release）；表满回退新建。**freestanding 保持原新建对象体**（`#ifdef __JETI_FREESTANDING`，无 `<string.h>` 依赖） | `same_ptr=1 / contains=1 / idx=1 / diff_ptr=0`；full_syntax §3.4 改字面量直查；golden/43 既有用例零回归 |
| 4 | **`@"..."` 传 C 串参数位静默乱码**（`stringWithUTF8String:@"bad"` 把对象头当字符数据） | checker 新增反向检查 `check_atstring_to_cstr`（`AtString` 实参 → `char *`/`const char *` 形参 = error，文案给 `[str UTF8String]` 修复提示），挂 MsgSend 与 FuncCall 两臂 | 负例 `tests/negative/atstring_to_cstr_param.jeti` exit=1 文案精确；正例（`[s UTF8String]`、裸串、`@noarc` 白名单）零误伤 |

**连带修掉的误报（checker 签名表覆盖，探针实证后修）**：#4 检查上线路后 test_all 的 `tt.jeti` 新失败——根因**不是** NPError.jeti 违例，而是 checker 的 `method_params` 按 selector 全局键控 last-writer-wins：`tt.jeti:91` 自声明 `- (id)objectForKey:(const char*)key` 覆盖了 NPDictionary 的 `(K)key` 签名 → `[NPDictionary objectForKey:@"…"]` 被误判。修法（`method_kinds` 同款纪律）：新增 `method_param_decls` 全声明表 + `selector_param_all_cstr` **共识门**——字符串反向检查只在"该 selector 的**所有**声明都同意 char*"时才发（混合声明沉默）；另加 `method_params_by_class` 按接收者类链（`named_class_of` + `method_params_for_receiver` 沿 superclass 走查）优先解析签名。NPError.jeti 本身合法零改动。tt.jeti 恢复 MRC-retry 通过，test_all 回到 **309/317**。

**文档同步**：AGENTS.md 三处旧记载更正（struct 成员 fn-ptr"不支持"→已支持；NPMutableArray"无 interning"→已 interning；isEqual: 一段）+ 本节；README.md typed-@catch highlights 行与字典节、CHINESE.md 字典节、golden/32 & 43 README 的 interning 表述全部更新；memory 旧"精确 isa 偏差"条目作废重写、新增 checker 签名表覆盖教训。

### 生成代码语义三项修复 ✅ (Sep 2026, 本会话续) — 外部评审（GPT）发现的 2 P1 + 1 P2

> 三项均先在当前二进制上转译复验确证（不是转述即信），再开修。回归基线（最终二进制实测）：cargo test **137/137**、test_all **309/317**（2 个 `-F` 既有失败不变）、multi_tu **10/10**、trace golden **8/8**、eh_diff **7/7**、stress 三套全过（baremetal/interop/hosted）。

| # | 问题（复验确证） | 修法 | 验证 |
|---|------|------|------|
| P2 | **`#define X @"..."` 原样透传进 C**（`NPError.jth:9-11` 三条域名词宏；宏在 C 轨展开处是硬 clang 错误——本 TU 未展开只是侥幸） | preprocessor 的 `#define` C 透传点加 `cpp::body_has_jeti_syntax`（现成判据：`@"..."`/消息发送/block 字面量）过滤——**jeti 宏表照常注册**（jetic 自己展开全部调用点，零损失），只有 C 轨丢行；普通 C 宏不受影响。守卫 stash 路径同样过滤 | 探针：`#define @"` 在生成 C 中 0 条、jeti 侧展开 `jeti_stringFromCstr("NPErrorParse")` 保留、`#define ANSWER 42` 照常透传运行正确 |
| P1 | **`__weak` 赋值不重注册**：decl-with-init 注册后，后续 `weakref = strong` 是普通赋值 → 槽位仍挂旧目标（常为初始 NULL）→ 源销毁不清零（`4.17 weakzero 0`） | codegen 新 pass `rewrite_weak_assigns`（挂在 `rewrite_block_var_refs` 后）：函数粒度收集 weak 局部名，语句级 Assign 改写为 `{ __auto_type tmp = <rhs 求值一次>; unregister→assign→register }`（与 weak-ivar setter 同款序列；eh/pattern pass 的 `__auto_type` 临时先例）。decl-with-init 注册 NULL 本身无害（weakUnregister 对未注册槽早退） | 探针：owned 源（alloc+init，ARC 块尾 release → dealloc → weakClearAll）`alive=1 / zeroed=1`；二次赋值 `swapped=1`；full_syntax §4.16-17 改用 owned 源后 `4.17 weakzero 1` |
| P1 | **`@synchronized` 降级成普通块**（codegen 臂注释自认 "no lock semantics yet"；单线程测试"看起来正常"，语义未实现） | **真互斥**三层：①runtime.h/c 新 API `jeti_syncLock`（对象地址 FNV-1a → 256 桶 `atomic_flag` 自旋，返回桶号）/`jeti_syncUnlock`/`jeti_syncAutoCleanup`——C11 atomics 无 pthread 依赖、桶共享退化为锁粗化（绝不漏互斥）、无表增长上限；②codegen 臂发 `{ long __jeti_sync_N __attribute__((cleanup(jeti_syncAutoCleanup))) = jeti_syncLock((void *)obj); <body> }`（cleanup 覆盖正常出口/return/break/continue）；③裸机 runtime 同签名 no-op（单核互斥平凡成立）。**checker 配套**：锁块内 `@throw` 会 longjmp 绕过 cleanup → `stmt_has_throw` 递归（含 block 字面量/字典/数组字面量）发 warning；`-eh checked` 把 throw 重写为旗标+return、cleanup 会跑，无此隐患 | 探针：`sync=5 / early=7 / relock-ok`（早退后能重新加锁 = cleanup 生效）；警告探针（本地 `@try` 捕获隔离）warning 文案精确、无 throw 零误报；full_syntax `4.11 sync 5` |

**测试侧教训（full_syntax §4.16）**：zeroing 验证必须用 **owned 源**（`[[NPString alloc] initWithUTF8String:...]` = +1，ARC 作用域尾注入 release → dealloc）；便利构造器（`stringWithUTF8String:`）返回 **autoreleased** 对象、从不销毁，修前修后输出相同——探针对象所有权选错会假阴性。

**文档同步**：AGENTS.md Weak References 节补 assign 改写条目 + 本节；memory 两条新事实（@synchronized 真互斥 + weak assign 重注册；宏过滤）。

#### 追加（同日第二轮评审）：async 状态机兜底 return ✅

- **问题**：`jeti_async_state_*` entry 函数的 `switch (t->state)` 只覆盖 `case 1` 与 `case <final>`，异常状态（任务结构损坏、完成后误 resume）会落到非 void 函数末尾——UB，clang `-Wreturn-type`（full_syntax 生成 C 实证 2 条，GPT 复验发现）。
- **修法**（`crates/async/src/lib.rs` entry_decl body）：switch 后追加完成协议作兜底——`t->state = -1; return 1;`（异常状态按"任务完成"结算，driver 的 join 循环能终止，不会死轮询）。
- **验证**：生成 C `-Wreturn-type` **0 条**；兜底 return 已发射；async 端到端探针 `result=42` 不变；golden/34 stdout diff **MATCH**；回归 cargo **137/137**、test_all **309/317**、multi_tu **10/10**、stress 三套全过。

### jetic 原生多输入多 TU 编译 ✅ (Oct 2026, 本会话)

- **用法**：`jetic main.jeti lib.jeti -I include -o app` —— 额外位置参数 `.jeti` 作为附加编译单元，一条命令完成"逐 TU 转译→编译→链接"；`.o`/`.a` 位置参数原样进链接（`jetic main.jeti lib.o libfoo.a -o app`）。**compile 模式 only**：run 模式的额外位置参数仍是程序参数；`-rewrite-jeti` 遇多输入响亮报错（一命令一产物）。
- **实现（`crates/jetic/src/main.rs`）**：manual 循环收 `extra_inputs`/`extra_objects`；每个额外 TU 用**子进程自调用**（`current_exe() -rewrite-jeti` → 临时 `.c`）转译——codegen 的 `METHOD_METADATA` 等是进程级 `OnceLock`，同进程二次转译不可行；最终**一次 clang 调用**把主 TU（stdin）+ 额外 `.c` + runtime 一起编译链接（与既有 runtime.c 同调用机制同构）。临时 `.c` 用后即删。
- **旗标转发**：`-I/-fno-jeti-arc/-fno-checker/-Werror/-eh/--slots/-backend/-arch/-ffreestanding/-nostdinc/-no-comments/-v` 全部转发给子进程；`-L/-l/-asm/-framework` 是链接期旗标、转译不需要，不转发。~~`-fstrong-metadata` 刻意不转发~~ → **旗标已随方案 A 删除**（见下方"方案 A 落地"节），不存在于任何解析路径。
- **blocks 探测扩展**：`-fblocks`/`-lBlocksRuntime` 的自动注入现在也扫额外 TU 的生成 C（任一 TU 含 `(^` 即注入）。
- **守护**：`-trace-refcount` + 多输入报错（trace 单 TU 语义）；README/CHINESE 顺带修正既有错误行——`jetic hello.jeti -o hello.o` 从未是 "-c mode"（`file` 探针实证产出 Mach-O 可执行文件），该行删除。
- **测试**：multi_tu 新增 `12_native_multi_input/`（`NATIVE` 标记文件驱动 runner 走 jetic 原生路径，用例内容与 01_basic 同构）；`run_multi_tu.sh` 加 NATIVE 分支（一条 jetic 命令替代"逐 TU -rewrite-jeti + clang"）。
- **回归**：multi_tu **12/12**（11 旧用例零回归 + 新用例）、cargo test **153/153**、test_all **347/355**（0 failed，与基线一致）；探针矩阵 A–F 全过（单输入冒烟 / `.o` 直链 / `.a` 直链 / rewrite 守卫 exit=1 / trace 守卫 exit=1 / run 模式额外位置参数=程序参数语义不变）。

### 方案 A 落地：Foundation 逐 TU 编译 + `-fstrong-metadata` 删除 ✅ (2026-10-03, 本会话)

> `doc/stable_slots_plan.md` §11 路线图第 2–4 步全部完成（详录见该文件"落地记录"节，此处记要点）。

- **库构建改 wrapper TU**：`tools/build-foundation-lib.sh` 为每个 Foundation `.jeti` 生成 wrapper（`#import <Foundation/Foundation.decl.jth>` 前置 + 原 `.jeti` 全文）再逐 TU 编译 + `ar`——`@implementation` 落在 wrapper 主文件 → R2 ownership 保持、元数据自动 STRONG（脚本内 nm 校验 9/9 强符号，弱即 fail）；wrapper 声明面 = decl.jth 全集，与客户端一致 → `__sig` 公共段一致。产物 `target/foundation/libjetifoundation.a`（372K）。
- **`-fstrong-metadata` 全链删除**（无过渡期，旧拼写两种参数位置都响亮失败）：clap Arg、`DOUBLE_TO_SINGLE`、flag 表、手动 help、两处解析点、`Pipeline.strong_metadata`/`CgUnit.strong_metadata` 字段、codegen 全部 `unit.strong_metadata ||` 合取（R2 独占判定）。strong_metadata 套件 check 3 改测"删除旗标的响亮拒绝"（双序断言），套件 10/10。
- **metaInit 退役实证**：R2 归属类的 `NPClass` 全部走 §10 静态初始化（`__data` 段）；用 `.o` 位置参数直链**强空 `jeti_metaInit` 覆盖弱合并副本**，客户端全程正常——metaInit 只剩幂等回填。残留：`#import "*.jeti"` 的单 TU 库构建路径仍需 metaInit 真身（构建脚本注释已声明）。
- **连带修真 bug 1——meta-vtable 桩引用**：meta-vtable 实例对"声明可见但本 TU 未实现"的类方法曾无条件引用 `Owner_mname` → 链接 undefined（实例 vtable 早有 EMITTED_METHODS"槽位保留、引用不发"规则，meta-vtable 漏镜像）。修法：同款 `method_is_emitted(owner, mname) || owner != 本类` 判定，own 未实现槽位 NULL。
- **连带修真 bug 2——源文件污染（方向性错误，已回退）**：曾直接给 9 个 `.jeti` 补 decl.jth import——`#import` 链让声明面泄漏进所有内联方，自包含 TU 桩 vtable 引用库符号 → 无库链接 undefined（test_all 7 个失败）。教训：**声明面扩大只能发生在库构建的 wrapper 里，不能进源文件**。
- **回归门槛（全部实测）**：cargo 153/0、multi_tu 12/12、strong_metadata 10/10、arc_intern ASan PASS、test_all **347/355, 0 failed**（6 canceled 交互式、2 个 `-F` 既有基线、SUSPECT=0）——与基线精确一致。

### Foundation 伞头交换：`Foundation.jth` 回归纯声明 + 新增 `Foundation.jeti` 自包含伞头 ✅ (2026-10-03, 本会话)

> 用户拍板：`#import <Foundation/Foundation.decl.jth>` 这个名字太怪——按项目自己的约定（`.jth` = 声明、`.jeti` = 实现），`Foundation.jth` 里内联实现才是异类。交换后与 C 的 `<foo.h>` + `-lfoo` 惯例对齐。

- **交换**：`Foundation.jth` = 纯声明伞头（原 `Foundation.decl.jth` 的内容，注释重写）；`Foundation.jeti` = 自包含伞头（原 `Foundation.jth` 的内容——9 个 `.jth` + 9 个 `.jeti` 全列）；`Foundation.decl.jth` **删除**；`tools/make-decl-headers.sh`（其唯一用途是生成 decl.jth）一并删除。
- **全局迁移**：229 个自包含 importer（`Foundation.jth` → `Foundation.jeti`，`search_replace` 一轮 + 3 个引号形式手补）+ 库模式引用（`build-foundation-lib.sh` wrapper 前置行、strong_metadata 套件 :57/:94 客户端探针；套件 :197/:206/:231 的 standalone 探针在新语义下**恰好正确**，不动）。
- **实施中修掉的三个坑（探针实证）**：① 重写 `Foundation.jth` 时漏抄 `NPMutableArray.jth`（照抄了 decl.jth 的 8 行清单）→ 客户端 `NPMutableArray` 未声明；② `_tus/` 与 `.o` 的**陈旧残留**让库构建 nm 校验扫到上一轮的死文件（`cannot open import: Foundation/Foundation.decl.jth`）→ 脚本补 `rm -rf _tus` + `rm -f *.o`；③ 新伞头 `Foundation.jeti` 被 `*.jeti` 通配扫进逐 TU 构建 → 它不是类（无 vtable）且会重复内联全部实现 → [1/4]/[3/4] 两循环都跳过 `Foundation` 名。
- **探针双绿**：自包含（`Foundation.jeti`，exit=0）+ P3 库模式（声明伞头 + `-ljetifoundation`，exit=0）；回归 cargo 153/0、multi_tu 12/12、strong_metadata 10/10、arc_intern PASS、test_all **347/355, 0 failed**（SUSPECT=0）——与基线精确一致。
- **文档**：README/CHINESE 的 Hello World 示例改 `Foundation.jeti`（声明伞头裸编译会链接失败）；库模式示例全部换 `Foundation.jth`；`decl.jth` 提及仅存于历史记录节（如实存档，不改写）。

### Foundation 库自动链接 — `find_libjetifoundation` ✅ (2026-10-03, 本会话)

> 用户拍板（"可以"）：让 jetic 像 `find_libjeti` 一样自动探测并链接 `libjetifoundation.a`，库模式命令行缩到 `jetic app.jeti -o app`。

- **实现**（`crates/jetic/src/main.rs`）：`find_libjetifoundation(custom_libs)` 与 `find_libjeti` 同款策略——exe 旁 → `target/foundation`（仓库内 dev 位置）→ `/opt/jeti/lib` → `/usr/local/lib/jeti` → 用户 `-L` 目录；`compile_to_binary` 新参 `foundation_impl_inlined` 布尔，找到即发 `-L dir -ljetifoundation`（verbose 打 `[jetic] auto-linking Foundation from …`）。
- **门控（关键——两种误开都会炸 `__sig`）**：仅**纯声明客户端**（`Foundation.jth`）自动链；`foundation_impl_inlined` 为真时跳过。判据是**源码级传递闭包**（`tu_imports_foundation_impl` → `foundation_impl_scan`，visited 去重防环）：主文件 + 每个 extra TU 的 `#import` 链中只要有一条解析到 bundle `include/Foundation/**.jeti` 即真——直接 import（`Foundation.jeti`）与**经 `.jth` 转手**（cross_file_test 经 `diamond_base.jth`、json_editor 经 `json_editor_types.jth`）都算。`-ffreestanding`、shared 模式、显式 `-ljetifoundation` 也抑制。
- **文本判别式两次被证伪（勿重蹈）**：① `= NP<Class>_`（vtable 槽位绑定行）——decl-only 客户端的 **meta vtable 类方法槽位**同样引用 `NP<Class>_<sel>` 符号（`.dictionaryWithObjects_forKeys_count_ = NPDictionary_…`），decl-only 也命中 → 纯声明客户端被误跳过、链接 undefined（exit=127）；② `struct NP<cls> {`——`@interface` 的 ivar 布局结构体 decl-only 客户端照发，无区分度。生成 C 上的文本启发式无法区分定义/原型/槽位引用/桩实例，**必须走源码 import 面**。
- **探针实证的机理**（`-why_load`）：自包含 TU 的 `JETI_CLASS_$_NP*` 是 common 符号，**归档成员因这些符号被拉入**，其强 vtable（纯 Foundation 段签名）在双 TU 场景赢得 weak 合并 → `__sig` 启动期 abort（case 12）；单 TU 自包含 + 链库不炸是因为全 TU 只有一个 Foundation 面（自己的弱实例赢）——所以 case 12（main.jeti+lib.jeti 两 TU）才是爆炸形态。
- **连带修复 test_all 2 failed**：首版判据只扫主文件的**直接** import 行，漏了传递链（cross_file_test/json_editor 的 Foundation 经 `.jth` 转手）→ MRC retry 链路运行期 `vtable layout mismatch`（exit 134）。改为传递闭包后 test_all **347/355, 0 failed**（cross_file_test 转 PASS_MRC、json_editor 恢复 CANCELED+编译替代验收）。
- **探针矩阵全绿**：纯自动（无 -I/-L/-l，decl-only 客户端 auto-link + 运行正确）/ 显式旗标不重复（0 次 auto-link）/ 自包含跳过（0 次且 exit=0）/ 库缺席静默无操作（自包含照跑；decl-only 响亮报 undefined——预期）/ freestanding 跳过 / case 12 通过。
- **回归**：cargo 153/0、multi_tu 12/12、strong_metadata 10/10、arc_intern PASS、test_all **347/355, 0 failed, SUSPECT=0**——与基线精确一致。
