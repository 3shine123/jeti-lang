use std::fmt::Write;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::collections::HashMap;
use jeti_ast::*;
use jeti_cst::{TypePrim, CstParam, TagKind};
use jeti_cst::source_map::SourceMap;
use attrs::Backend;

/// `#line` emission context (plan 阶段 2): when enabled, `emit_stmt` emits a
/// `#line <src> "<file>"` directive whenever the statement's mapped source
/// position (via the preprocessor's SourceMap) differs from the last one
/// emitted, so clang diagnostics point back at the `.jeti`/`.hje` the user
/// wrote. Regions with no single source line (generated metadata, macro
/// expansion past the invocation site) map to the virtual file
/// `<jeti-generated>`; every function body entry resets to it first so user
/// positions never leak into generated code below.
struct LineCtx {
    sm: SourceMap,
    cur_file: String,
    cur_line: u32,
}

thread_local! {
    static LINE_CTX: RefCell<Option<LineCtx>> = const { RefCell::new(None) };
    /// Suppresses directive emission inside C constructs that cannot tolerate
    /// an intervening line (for/while headers, else-branches): the next
    /// statement on its own line re-establishes the position.
    static LINE_SUPPRESS: RefCell<bool> = const { RefCell::new(false) };
}

fn escape_line_path(p: &str) -> String {
    p.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) const SYNTHETIC_FILE: &str = "<jeti-generated>";

/// Emit a `#line` directive (at column 0) if the statement's source position
/// differs from the last emitted one. No-op when line directives are off.
fn maybe_emit_line(flat_line: usize, out: &mut String) {
    if LINE_SUPPRESS.with(|s| *s.borrow()) {
        return;
    }
    LINE_CTX.with(|c| {
        let mut c = c.borrow_mut();
        let Some(ctx) = c.as_mut() else { return };
        let (file, line) = ctx.sm.locate(flat_line);
        let (target_file, target_line) = if file.is_empty() {
            (SYNTHETIC_FILE.to_string(), 1)
        } else {
            (file, line)
        };
        if target_file == ctx.cur_file && target_line == ctx.cur_line {
            return;
        }
        // The directive must sit at the start of its own line. Mid-line calls
        // (for-init, else-branch, inline bodies) must SKIP — forcing a newline
        // there would split the surrounding C statement (observed: `for (#line 21`
        // → clang syntax error). The position is picked up by the next
        // statement emitted after a newline instead.
        if !out.is_empty() && !out.ends_with('\n') {
            return;
        }
        let _ = write!(out, "#line {} \"{}\"\n", target_line, escape_line_path(&target_file));
        ctx.cur_file = target_file;
        ctx.cur_line = target_line;
    });
}

/// Run `f` with `#line` directive emission suppressed (for/while headers,
/// else-branches — constructs where an intervening line would be invalid C).
fn with_line_suppressed<R>(f: impl FnOnce() -> R) -> R {
    let prev = LINE_SUPPRESS.with(|s| std::mem::replace(&mut *s.borrow_mut(), true));
    let r = f();
    LINE_SUPPRESS.with(|s| *s.borrow_mut() = prev);
    r
}

/// Reset the `#line` state to the synthetic virtual file. Called at every
/// function-body entry so generated code between user bodies is never
/// misattributed to the previous user file.
fn reset_line_synthetic(out: &mut String) {
    LINE_CTX.with(|c| {
        let mut c = c.borrow_mut();
        let Some(ctx) = c.as_mut() else { return };
        if ctx.cur_file == SYNTHETIC_FILE && ctx.cur_line == 1 {
            return;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        let _ = write!(out, "#line 1 \"{}\"\n", SYNTHETIC_FILE);
        ctx.cur_file = SYNTHETIC_FILE.to_string();
        ctx.cur_line = 1;
    });
}

// ─── Temp variable counter ─────────────────────────────────────────────────
static TEMP_VAR_COUNTER: AtomicUsize = AtomicUsize::new(0);

// ─── Block-variable names (populated during convert_decl) ────────────────────
static BLOCK_VARS: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

fn block_vars() -> std::sync::MutexGuard<'static, Option<std::collections::HashSet<String>>> {
    BLOCK_VARS.lock().unwrap()
}

fn is_block_var(name: &str) -> bool {
    block_vars().as_ref().map_or(false, |s| s.contains(name))
}

// ─── respondsToSelector: helpers (populated during convert_expr) ─────────
// Each `[recv respondsToSelector:@selector(s)]` maps the selector to the
// uniform vtable member name and needs one static BOOL helper whose body is
// emitted later in emit_unit_with_headers (after the vtable struct exists).
// The set holds just the member names (e.g. "speak"); declaration happens at
// the MsgSend special-case site, body emission happens at emit time.
static RESP_HELPERS: std::sync::Mutex<Option<std::collections::BTreeSet<String>>> =
    std::sync::Mutex::new(None);

fn resp_helpers() -> std::sync::MutexGuard<'static, Option<std::collections::BTreeSet<String>>> {
    RESP_HELPERS.lock().unwrap()
}

// ─── Emitted method bodies (`Owner_method` symbols that really exist) ────────
// A vtable slot can exist without an implementation: a method a class declares
// by conforming to a protocol, or one it declares but implements in another
// translation unit. Those slots must initialise to NULL instead of naming a
// function that is never emitted (which would be an undefined symbol at link
// time). Populated while the class metadata is assembled.
static EMITTED_METHODS: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

fn emitted_methods() -> std::sync::MutexGuard<'static, Option<std::collections::HashSet<String>>> {
    EMITTED_METHODS.lock().unwrap()
}

fn method_is_emitted(owner: &str, mname: &str) -> bool {
    emitted_methods()
        .as_ref()
        .map_or(true, |s| s.contains(&format!("{}_{}", owner, mname)))
}

// ─── Block-expansion definitions (gcc/portable) ──────────────────────────────
static BLOCK_DEFS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn block_defs() -> std::sync::MutexGuard<'static, String> {
    BLOCK_DEFS.lock().unwrap()
}

// ─── Backend selection (set during ast_to_cg_unit) ──────────────────────────
static CURRENT_BACKEND: AtomicU8 = AtomicU8::new(0);

fn meta_symbol(kind: &str, flat: &str) -> String {
    // clang/gcc use `$_` (ObjC-style), portable uses `_` (plain C).
    let sep = match CURRENT_BACKEND.load(Ordering::Relaxed) {
        1 | 2 => "$_",  // clang=1, gcc=2
        _ => "",        // portable=0
    };
    format!("JETI_{}{}{}", kind, sep, flat)
}

/// FNV-1a fingerprint of the uniform vtable layout, i.e. of the (sorted) set of
/// instance-method names this translation unit compiled a `struct jeti_vtable`
/// for. Two units agree iff they saw the same method set; the value is stamped
/// into every vtable instance and verified at load time so a cross-TU layout
/// mismatch aborts with a clear message instead of dispatching garbage.
fn vtable_layout_sig(method_names: &[String]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for mname in method_names {
        for b in mname.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// FNV-1a over an arbitrary signature text — same function and offsets as
/// [`vtable_layout_sig`], kept separate because that one hashes a method list.
fn text_layout_sig(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Canonical text for one class's ivar section — the payload of the cross-TU
/// `__layout_sig` check (R4, the layout twin of R3's `__sig`).
///
/// A `.hje` and the TU that owns the class must agree on the ivar section: the
/// emitted `struct <Class>` is a flat C struct with every ancestor's ivars
/// inlined in declaration order, so a stale header silently reinterprets every
/// field past the divergence and the bug surfaces as garbage reads instead of a
/// diagnostic. Both sides are spelled here in source order — superclass name
/// first, then each `(type, name)` pair — which is exactly what the struct
/// emitter consumes, and the per-class fingerprint travels in that class's
/// vtable instance (see the `__layout_sig` member), so the two TUs compare only
/// head-to-head instead of over a global class set.
///
/// Listing the class's OWN ivars is enough: an ancestor's ivars are compared
/// through the ancestor's own entry, and a class visible in two TUs can only
/// name a superclass that both of them import. A class that no other TU
/// links (a header-only data class, a TU-local helper) has nothing to compare
/// against and therefore cannot produce a false alarm.
fn class_layout_text(cm: &CgClassMeta) -> String {
    let mut s = String::new();
    s.push_str(&name_flat(&cm.class_name));
    match cm.super_name {
        Some(ref sup) => {
            s.push_str(" < ");
            s.push_str(&name_flat(sup));
        }
        None => s.push_str(" < (root)"),
    }
    for (ty, name) in cm.ivar_types.iter().zip(cm.ivar_names.iter()) {
        s.push_str(" | ");
        s.push_str(ty);
        s.push(' ');
        s.push_str(name);
    }
    s
}

/// C string literal for a generated diagnostic payload (quotes and backslashes
/// are the only characters a class/type spelling can plausibly carry).
fn c_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// R3: the `__sig` fingerprint must cover only the segment that has to agree
/// across TUs — the public segment (R1 keeps its slot indices identical
/// everywhere and R2 keeps private dispatch local to the owning TU). A
/// TU-local private method must NOT make the signatures differ — that is
/// exactly the legal library+client split
/// `tests/multi_tu/11_vtable_private_slots` pins. With no public information,
/// every method is shared and the fingerprint covers all of them (the pre-R3
/// behavior).
///
/// Manifest mode: the manifest may carry the creator TU's private tail (the
/// creator compiled manifest-less — the file did not exist yet — and wrote its
/// full R1 assignment back). The creator therefore fingerprints its public
/// segment, so the follower must fingerprint the manifest's public entries to
/// match; filtering the manifest through the same public set makes the two
/// agree exactly. With no public information the manifest itself is the
/// segment (pre-R3 semantics for that mode).
fn vtable_sig_segment(
    slots_manifest: Option<&[String]>,
    public_methods: Option<&std::collections::HashSet<String>>,
    global_instance_method_names: &[String],
) -> Vec<String> {
    match slots_manifest {
        Some(manifest) => match public_methods {
            Some(public) => manifest
                .iter()
                .filter(|m| public.contains(*m))
                .cloned()
                .collect(),
            None => manifest.to_vec(),
        },
        None => match public_methods {
            Some(public) => global_instance_method_names
                .iter()
                .filter(|m| public.contains(*m))
                .cloned()
                .collect(),
            None => global_instance_method_names.to_vec(),
        },
    }
}

/// §8-④ (`docs/plans/async_soft_keywords_plan.md`): the shared-segment
/// fingerprint must encode the async part of a method's signature, because the
/// generated C signature does not show it — every async method is emitted with
/// the uniform `NPTask *` return, so `(async int) fetch` and `(async long)
/// fetch` (and a plain synchronous `fetch`) are byte-identical to the `__sig`
/// guard. A client that disagrees with the owner would pass the startup check
/// and then mis-dispatch the awaited value — exactly what R3 exists to catch.
///
/// The marker is derived from the *declaration* AST, which every TU sees (the
/// owner from its `@implementation`, a client from the `#import`ed `.hje`), so
/// both sides compute the same string without any extra metadata.
///
/// Format: `<name>|async[:<business C spelling>]` — the type part is the
/// business type T of `async T`. Plain methods keep their bare name, so their
/// fingerprints are unchanged. (The old `!` failable mark died with the
/// second-batch try-await design, plan §9.5.)
///
/// Every declaration of a selector contributes a mark (`None` = declared
/// synchronously), and the caller only stamps the fingerprint when all of
/// them agree. A private-tail `- (async void)run` next to Foundation's
/// synchronous `-run` is a *different method* that happens to share the slot
/// name; letting either stamp the other's fingerprint makes two TUs that
/// agree on the shared segment disagree on `__sig` (that was the NPRunLoop
/// `-run` R3 abort). On disagreement the name goes in bare — a missed guard,
/// never a false report.
fn async_sig_marks(ast: &AstUnit) -> HashMap<String, Vec<Option<String>>> {
    let mut marks: HashMap<String, Vec<Option<String>>> = HashMap::new();
    for d in &ast.decls {
        if let AstDeclData::Class { methods, .. } = &d.data {
            for m in methods {
                if let (AstDeclData::Method { async_marker, return_type, .. }, Some(sel)) =
                    (&m.data, &m.name)
                {
                    let mark = if *async_marker {
                        let mut mk = String::from("|async");
                        if let Some(t) = return_type.as_deref().and_then(business_return_type) {
                            mk.push(':');
                            mk.push_str(&ast_type_to_c_str(t));
                        }
                        Some(mk)
                    } else {
                        None
                    };
                    marks.entry(sanitize_sel_name(sel)).or_default().push(mark);
                }
            }
        }
    }
    marks
}

/// The business return type of an async method: the pipeline stores the
/// signature as the task handle `NPTask<T> *`, so T is the first type argument;
/// anything else (a non-async declaration, or an already-normalized client)
/// is its own business type.
fn business_return_type(t: &AstType) -> Option<&AstType> {
    if t.is_pointer && t.name.as_deref() == Some("NPTask") {
        t.type_args.first()
    } else {
        Some(t)
    }
}

// ─── Global vtable method metadata (set during ast_to_cg_unit) ────────────
static METHOD_METADATA: OnceLock<HashMap<String, (usize, String)>> = OnceLock::new();
// Per-class method signature: class_flat -> (method_name -> (index, ptr_type)).
// Two classes may share a selector (e.g. get:) with DIFFERENT signatures, so
// dispatch must cast the vtable member using the receiver's static class.
static CLASS_METHOD_METADATA: OnceLock<HashMap<String, HashMap<String, (usize, String)>>> = OnceLock::new();
/// Per-class signature for **class** methods: class_flat → method name →
/// declared parameter type list (self/SEL first, then the real parameters).
///
/// `CLASS_METHOD_METADATA` is deliberately instance-only — it feeds the
/// uniform-vtable fn-ptr cast, which has no class-method slots. Class-method
/// sends therefore had *no* parameter types at all, so a direct call emitted
/// its arguments uncast and clang reported `incompatible pointer types` for
/// perfectly legal sends (`[Maker makeWith:someNPString]`, `[C make:subclassObj]`,
/// `[C makeWith:@"literal"]`). This table gives them the same treatment the
/// instance path already had.
static CLASS_METHOD_PARAMS: OnceLock<HashMap<String, HashMap<String, Vec<String>>>> = OnceLock::new();

/// Local variable declarations seen during body conversion: name → flat class
/// name (only class-typed locals are recorded). The dispatch cast must use the
/// RECEIVER's static class, but lowering passes (async state machines, block
/// thunks) rebuild receiver expressions and drop the checker-stamped
/// `expr_type` — without this table an ordinary `Svc *s` fell through to the
/// any-class-with-this-selector guess, which let an unrelated class's
/// signature (e.g. NPRunLoop's `- (void)run`) be cast onto a send whose
/// receiver class declares `- (async void)run`. Last-writer-wins on shadowing;
/// a stale entry is harmless because the lookup still walks class_infos and
/// falls back when the recorded class never declared the selector.
static LOCAL_VAR_CLASS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn local_var_class() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
    LOCAL_VAR_CLASS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap()
}

fn get_vtable_param_type(method_name: &str, param_index: usize) -> Option<String> {
    METHOD_METADATA.get()
        .and_then(|meta| meta.get(method_name))
        .and_then(|(_, ptr_type)| parse_vtable_param_type(ptr_type, param_index))
}

fn get_vtable_param_type_for_class(class_flat: &str, method_name: &str, param_index: usize) -> Option<String> {
    CLASS_METHOD_METADATA.get()
        .and_then(|classes| classes.get(class_flat))
        .and_then(|methods| methods.get(method_name))
        .and_then(|(_, ptr_type)| parse_vtable_param_type(ptr_type, param_index))
        .or_else(|| get_class_method_param_type(class_flat, method_name, param_index))
        .or_else(|| get_vtable_param_type(method_name, param_index))
}

/// Declared parameter type of a CLASS method (`+foo:`), by the same
/// `param_index` convention the instance path uses: 0 = self, 1 = SEL,
/// 2 = the first real parameter.
fn get_class_method_param_type(class_flat: &str, method_name: &str, param_index: usize) -> Option<String> {
    CLASS_METHOD_PARAMS.get()
        .and_then(|classes| classes.get(class_flat))
        .and_then(|methods| methods.get(method_name))
        .and_then(|params| params.get(param_index))
        .cloned()
}

fn parse_vtable_param_type(ptr_type: &str, param_index: usize) -> Option<String> {
    let paren_start = ptr_type.find("(*)(")?;
    let inner = &ptr_type[paren_start + 4..];
    let paren_end = inner.rfind(')')?;
    let params_str = &inner[..paren_end];
    let mut params: Vec<String> = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    for (i, ch) in params_str.char_indices() {
        match ch {
            '(' | '<' => depth += 1,
            ')' | '>' => depth -= 1,
            ',' if depth == 0 => {
                params.push(params_str[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = params_str[start..].trim();
    if !last.is_empty() {
        params.push(last.to_string());
    }
    // Generic sentinel markers ride along on class-pointer params
    // (`NPObject * /*V*/` in `setObject:forKey:`) — they are compile-time
    // type-args hints for dispatch, not C. Strip them (plus the leftover
    // spaces) or the non-`*` tail defeats `ends_with('*')` at every
    // coercion site.
    let cleaned: Vec<String> = params.iter()
        .map(|p| strip_c_comment_markers(p).trim().to_string())
        .collect();
    cleaned.get(param_index).cloned()
}

/// Remove `/*V*/`-style C comment markers from a parameter spelling.
fn strip_c_comment_markers(s: &str) -> String {
    if !s.contains("/*") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            Some(j) => rest = &rest[i + 2 + j + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}
static BLOCK_TYPEDEF_NAMES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

// Declarator typedefs that are C function pointers (`typedef int (*IntFn)(int)`)
// rather than blocks. They share `BLOCK_TYPEDEF_NAMES` for flat-name resolution
// (fields/ivars/vars) but must NOT be treated as block variables: a plain fnptr
// call has to stay `fn(4,5)`, never `((...)->invoke)(fn,4,5)` on gcc/portable.
static FNPTR_TYPEDEF_NAMES: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

// ─── Current function return type (for covariant return cast) ────────────────
static CURRENT_RETURN_TYPE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

// Declared plain-C function signatures (name → C param type spellings) so
// synthesized `Call` args can be cast where jeti's ObjC semantics allow an
// implicit `id <-> class pointer` join and plain C would flag
// -Wincompatible-pointer-types. Populated at Function-decl conversion
// (prototypes included); read back when a body emitted later calls it.
// jeti requires declaration-before-use, so earlier decls are enough.
static FUNC_PARAM_TYPES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn register_func_sig(name: &str, params: &[(String, String)]) {
    if name.is_empty() {
        return;
    }
    FUNC_PARAM_TYPES
        .lock()
        .unwrap()
        .insert(name.to_string(), params.iter().map(|(t, _)| t.clone()).collect());
}

/// Explicit cast spelling for an implicit ObjC-style pointer join, or None
/// when none is needed. Only `id`/`NPObject *` acts as the wildcard — a
/// specific-class→specific-class conversion is NOT implicit in ObjC either,
/// so that mismatch must keep warning. Builtin C pointers (`char *`,
/// `void *`, `const …`) never join.
fn obj_wildcard_cast(dst: &str, src: &str) -> Option<String> {
    let d = dst.trim();
    let s = src.trim();
    if d == s || !s.ends_with(" *") || !d.ends_with(" *") {
        return None;
    }
    for t in [d, s] {
        if t.contains("char") || t.contains("void") || t.contains("const ") {
            return None;
        }
    }
    let is_id = |t: &str| t == "NPObject *" || t == "id" || t == "instancetype";
    if is_id(s) || is_id(d) {
        Some(d.to_string())
    } else {
        None
    }
}

/// Wrap an object-pointer expression in `(NPObject *)` unless it already is
/// one (or is not an object pointer at all — builtin C pointers pass through).
fn upcast_to_npobject(e: CgExpr) -> CgExpr {
    let Some(t) = e.type_str.as_deref().map(str::trim) else { return e; };
    if t == "NPObject *" || t == "id" {
        return e;
    }
    if obj_wildcard_cast("NPObject *", t).is_none() {
        return e;
    }
    CgExpr {
        kind: CgExprKind::Cast, type_str: Some("NPObject *".into()), line: e.line, col: e.col,
        data: CgExprData::Cast {
            target_type: "NPObject *".into(),
            expr: Box::new(e),
        },
    }
}

/// Cast args of a plain C call per the registered declared signature.
fn coerce_call_args_by_sig(name: &str, args: &mut [CgExpr]) {
    let sig = FUNC_PARAM_TYPES.lock().unwrap().get(name).cloned();
    let Some(pts) = sig else { return };
    for (i, arg) in args.iter_mut().enumerate() {
        let Some(pt) = pts.get(i) else { break };
        let Some(at) = arg.type_str.clone() else { continue };
        let Some(cast) = obj_wildcard_cast(pt, &at) else { continue };
        let inner = std::mem::replace(arg, CgExpr {
            kind: CgExprKind::Int, type_str: None, line: arg.line, col: arg.col,
            data: CgExprData::Int(0),
        });
        *arg = CgExpr {
            kind: CgExprKind::Cast, type_str: Some(cast.clone()), line: inner.line, col: inner.col,
            data: CgExprData::Cast {
                target_type: cast,
                expr: Box::new(inner),
            },
        };
    }
}

fn next_temp_id() -> usize {
    TEMP_VAR_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Backend=clang emits native `^` blocks (compiled by clang -fblocks).
/// gcc/portable expand blocks to struct+invoke form, but gcc/portable were
/// long emitting `^` too, which real GCC rejects. This flag gates the branch.
fn is_clang_backend() -> bool {
    CURRENT_BACKEND.load(Ordering::Relaxed) == 1
}

/// Block-typed value in C. clang: `RT (^)(params)`. gcc/portable: an opaque
/// pointer to the shared `struct __jeti_block_header` (all literal structs
/// start with that header; call via `->invoke`).
fn block_type_c_str(ret: &str, _params: &str) -> String {
    if is_clang_backend() {
        format!("{} (^)({})", ret, _params)
    } else {
        "struct __jeti_block_header *".to_string()
    }
}

// ─── Block literal data ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BlockLiteralData {
    pub return_type: String,
    pub params: Vec<(String, String)>,  // (type, name)
    pub body: Option<Box<CgStmt>>,
    pub func_name: String,
}

fn fnv1a_hash(s: &str) -> u32 {
    let mut hash: u32 = 0x811C9DC5;
    for b in s.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

fn sanitize_sel_name(sel: &str) -> String {
    sel.replace(':', "_")
}

fn sel_const_name(sel: &str) -> String {
    format!("__jeti_sel_{}", sanitize_sel_name(sel))
}

// ─── C99 AST types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum CgExprKind {
    Int, Float, String, Char, Ident, Sizeof,
    Unary, Binary, Assign, Cast,
    Call, Comma, Paren, Member, Arrow, Index, Ternary,
    InitList, DesignatedInit, BlockLit, TypeLiteral,
    /// Pre-rendered C text emitted verbatim — statement expressions `({ ... })`,
    /// whose body statements are converted and rendered ahead of time.
    Raw,
}

#[derive(Debug, Clone)]
pub struct CgExpr {
    pub kind: CgExprKind,
    pub type_str: Option<String>,
    pub line: usize, pub col: usize,
    pub data: CgExprData,
}

#[derive(Debug, Clone)]
pub enum CgExprData {
    Int(i64), Float(f64), FloatRaw(String), String(String), Char(u8), Ident(String),
    Unary { op_str: String, operand: Box<CgExpr>, is_postfix: bool },
    Binary { op_str: String, left: Box<CgExpr>, right: Box<CgExpr> },
    Assign { target: Box<CgExpr>, value: Box<CgExpr> },
    Cast { target_type: String, expr: Box<CgExpr> },
    Call {
        name: String, args: Vec<CgExpr>,
        vtable_class: Option<String>,
        alt_vtable_classes: Vec<String>,
        is_class_method: bool, is_super: bool,
        sel_const_name: Option<String>,
        method_index: Option<usize>,
        /// True when the receiver's STATIC type is a generic instantiation
        /// (`Box<int> *`): the call goes straight to the monomorphized
        /// clone's function — the base template's vtable slot carries the
        /// erased `NPObject *` ABI, which cannot carry a value-type `T`.
        /// C++-template semantics: static type decides, no runtime lookup.
        direct_specialized: bool,
    },
    Comma(Vec<CgExpr>),
    /// `(expr)` — user-written grouping, re-emitted verbatim (see AstExprData::Paren).
    Paren(Box<CgExpr>),
    Member { obj: Box<CgExpr>, field: String },
    Arrow { obj: Box<CgExpr>, field: String },
    Index { arr: Box<CgExpr>, index: Box<CgExpr> },
    Ternary { cond: Box<CgExpr>, then: Box<CgExpr>, else_: Box<CgExpr> },
    InitList(Vec<CgExpr>),
    /// One C99 designated-initializer entry: `.field = v` / `[i] = v` / chain.
    DesignatedInit { designators: Vec<CgDesignator>, expr: Box<CgExpr> },
    BlockLit(BlockLiteralData),
    Sizeof { type_str: String, is_alignof: bool },
    TypeLiteral(String),
    /// Verbatim C text (statement-expression bodies).
    Raw(String),
}

/// One link of a C99 designated-initializer designator chain: `.field` / `[index]`.
#[derive(Debug, Clone)]
pub enum CgDesignator {
    Member(String),
    Index(Box<CgExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgStmtKind {
    Empty, Expr, Compound, If, Switch, Case, Default,
    While, Do, For, ForIn,
    Break, Continue, Return, Goto, Label, Decl, Asm,
}

#[derive(Debug, Clone)]
pub struct CgStmt {
    pub kind: CgStmtKind,
    pub line: usize, pub col: usize,
    pub data: CgStmtData,
}

#[derive(Debug, Clone)]
pub struct CgAsmOperand {
    pub name: Option<String>,
    pub constraint: String,
    pub expr: CgExpr,
}

#[derive(Debug, Clone)]
pub enum CgStmtData {
    Expr(CgExpr),
    Compound(Vec<CgStmt>),
    If { cond: Box<CgExpr>, then: Box<CgStmt>, else_: Option<Box<CgStmt>> },
    Switch { expr: Box<CgExpr>, body: Box<CgStmt> },
    Case { value: Box<CgExpr>, body: Box<CgStmt> },
    Default(Box<CgStmt>),
    While { cond: Box<CgExpr>, body: Box<CgStmt> },
    Do { body: Box<CgStmt>, cond: Box<CgExpr> },
    For { init: Option<Box<CgStmt>>, cond: Option<Box<CgExpr>>, incr: Option<Box<CgExpr>>, body: Box<CgStmt> },
    ForIn { var_name: String, collection: Box<CgExpr>, body: Box<CgStmt> },
    Return(Option<Box<CgExpr>>),
    Goto(String),
    Label(String),
    Decl { decl_type: String, name: String, init: Option<Box<CgExpr>>, array_suffix: Option<String>, is_static: bool, is_weak: bool, is_block: bool, next: Vec<(String, String, Option<Box<CgExpr>>)>, attributes: Vec<String> },
    Asm {
        is_volatile: bool,
        is_goto: bool,
        template: String,
        outputs: Vec<CgAsmOperand>,
        inputs: Vec<CgAsmOperand>,
        clobbers: Vec<String>,
        labels: Vec<String>,
    },
    Break,
    Continue,
    Empty,
    /// Whole-source-line pass-through (`_Pragma("...")`, `#pragma mark`):
    /// re-emitted verbatim at its original statement position.
    RawLine(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgDeclKind {
    Function, Variable, Typedef, Struct, ExternFunc, Enum, Asm, ForwardClass, RawLine,
}

#[derive(Debug, Clone)]
pub struct CgDecl {
    pub kind: CgDeclKind,
    pub name: String,
    pub data: CgDeclData,
    /// User `__attribute__((...))` spellings to emit before the declaration.
    pub attributes: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum CgDeclData {
    Function {
        return_type: String,
        params: Vec<(String, String)>,
        is_variadic: bool,
        is_objc_class: bool,
        body: Option<Box<CgStmt>>,
    },
    Variable {
        var_type: String,
        init: Option<Box<CgExpr>>,
        is_static: bool,
        /// `extern` storage class. Dropping it turned a file-level
        /// `extern char **environ;` into a *definition* — a zero-initialized
        /// tentative definition that shadowed libc's `environ` and made the
        /// whole environment table read as empty.
        is_extern: bool,
        is_const: bool,
        is_weak: bool,
        is_block: bool,
        next: Vec<(String, String, Option<Box<CgExpr>>)>,
    },
    Typedef {
        alias: String,
        type_str: String,
        struct_fields: Vec<(String, String, Vec<String>)>,
    },
    Struct { fields: Vec<(String, String, Vec<String>)>, is_union: bool },
    ExternFunc {
        return_type: String,
        params: Vec<(String, String)>,
        is_variadic: bool,
    },
    Asm {
        is_volatile: bool,
        is_goto: bool,
        template: String,
        outputs: Vec<CgAsmOperand>,
        inputs: Vec<CgAsmOperand>,
        clobbers: Vec<String>,
        labels: Vec<String>,
    },
    Enum { members: Vec<(String, String)> },
    ForwardClass { names: Vec<String> },
    /// A raw C line passed through verbatim (e.g. `#pragma mark - Foo`).
    RawLine(String),
}

#[derive(Debug, Clone)]
pub struct CgUnit {
    pub decls: Vec<CgDecl>,
    pub filename: String,
    pub c_headers: Vec<String>,
    pub selectors: Vec<String>,
    pub classes: Vec<CgClassMeta>,
    pub global_instance_method_names: Vec<String>,
    /// Struct tags whose `==`/`!=` the checker rewrote to `jeti_struct_eq_<tag>`
    /// calls. One field-wise comparison function is emitted per tag, on demand.
    pub struct_eq_tags: Vec<String>,
    /// `-fno-jeti-arc`: manual retain/release. Object ivars are then the
    /// programmer's to release, so no ARC dealloc wrapper is generated.
    pub no_arc: bool,
    /// Classes whose `@implementation` sits in **this** TU's main file (not in
    /// an imported header) — the translation unit that owns them, per rule R2
    /// of `docs/plans/stable_slots_plan.md`.
    ///
    /// An owner's metadata is emitted **strong**, so another TU's weak stub can
    /// never displace it; and if a second TU also implements the class in its
    /// own main file, the link fails with a `duplicate symbol` instead of
    /// silently picking one of two tables. A class whose `@implementation`
    /// arrives through an `#import` stays weak — that is what keeps the
    /// self-contained mode (every TU inlines Foundation) merging as before.
    pub owned_classes: std::collections::HashSet<String>,
    /// `--instantiate-only`: this TU exists to materialize SPECIALIZATIONS out
    /// of a template source shipped by a precompiled library. The bare template
    /// class stays the archive's (weak here), but a specialization it emits is
    /// the only definition in the link — so it must be STRONG, or the client's
    /// empty weak stub could win the weak-merge and dispatch into NULL slots.
    pub instantiate_only: bool,
    /// The instance-method names whose vtable layout must agree across all
    /// linked TUs — the segment the `__sig` fingerprint is computed over
    /// (rule R3 of `docs/plans/stable_slots_plan.md`).
    ///
    /// Under R1 the public segment (methods declared in imported
    /// `@interface`s) occupies the same slot indices everywhere, and R2 keeps
    /// each private method's dispatch inside its owning TU — so two TUs only
    /// need to agree on the shared segment, and a TU-local private method must
    /// NOT make the signatures differ (that is exactly the legal split case 11
    /// pins). With a `--slots` manifest the shared portion is the manifest
    /// itself; with neither, every method is shared (the pre-R3 behavior).
    pub vtable_sig_names: Vec<String>,
    /// True when this TU can see the NPPredicate declaration (pipeline: a
    /// transitive jeti `#import` of NPPredicate). Gates KVC accessor-table
    /// emission (JETI_KVC_$_<Class>): a TU that never touches predicates
    /// pays nothing. See docs/plans/nppredicate_plan.md §2.
    pub kvc: bool,
    /// Protocol declarations visible in this unit: (name, parents, required,
    /// optional). Emitted as static `NPProtocol` instances (D5.2,
    /// docs/plans/categories_protocol_plan.md) so `conformsToProtocol:` can answer
    /// from metadata. Owner-only emission (strong) follows the R2 rule.
    pub protocols: Vec<(String, Vec<String>, Vec<String>, Vec<String>)>,
    /// Every generic specialization this unit asked for, as a rendered
    /// `Base<Args>` spelling — the manifest a multi-TU driver feeds back as
    /// `--generic-instantiation`. It holds the **derived** demands too: cloning
    /// `ChainBox<NPString *>` discovers `ChainArray<T>` inside its body and
    /// demands `ChainArray<NPString *>`, which may be owned by a sibling TU.
    /// Driving these to a fixed point across TUs is what closes the transitive
    /// instantiation chain in multi-TU builds (Cfront-style manifest
    /// iteration); single-TU builds close it inside the worklist below.
    pub generic_demands: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CgClassMeta {
    pub class_name: String,
    pub super_name: Option<String>,
    pub method_names: Vec<String>,
    pub method_sel_names: Vec<String>,  // original selectors (with colons), for bridge header
    pub is_class_methods: Vec<bool>,
    pub method_return_types: Vec<String>,
    pub method_params_list: Vec<Vec<(String, String)>>,
    pub method_variadic: Vec<bool>, // parallel to method_names: true → C `...` method
    pub method_owners: Vec<String>,
    /// Names of category-introduced methods. Keyed by NAME: `method_names`
    /// is re-merged with inherited methods between unit construction and
    /// vtable emission, so any index-parallel array goes stale in between
    /// (seen live: built with 2 names, emitted with 11).
    pub category_method_names: std::collections::HashSet<String>,
    /// This TU contains a category implementation for the class. Used to
    /// publish category-only protocol conformances across TUs.
    pub has_category_impl: bool,
    pub vtable_indices: Vec<i32>,
    pub ivar_types: Vec<String>,
    pub ivar_names: Vec<String>,
    pub ivar_weak: Vec<bool>,
    pub properties: Vec<String>,
    pub has_impl: bool,
    /// Protocol conformances for the static metadata table (D5.2).
    pub protocols: Vec<String>,
}

// ─── AST Type → C string ─────────────────────────────────────────────────────

/// Does this ivar type denote an object ARC owns?
///
/// Only pointer-typed, non-weak, non-scalar-pointer, non-function-pointer
/// ivars qualify: `char *`/`void *` are plain C storage, a function pointer is
/// not an object, and block layouts are managed by the Blocks runtime.
/// Build the comma sequence that writes an object slot and keeps the
/// *referent's* weak list in sync:
///
/// ```c
/// (jeti_weakUnregister((NPObject **)&target),
///  target = value,
///  jeti_weakRegister((NPObject **)&target, (NPObject *)value))
/// ```
///
/// Used for both explicit `self->_weakIvar = v` writes and weak property
/// assignment written with dot syntax (`self.weakProp = v`). ObjC dot syntax
/// *is* a setter call, so it has to register exactly like the synthesized
/// setter does — a raw field write would leave the slot out of the referent's
/// weak list, so the slot is never nil'd and the program keeps a dangling
/// pointer where the language guarantees nil.
fn build_weak_write(
    target: CgExpr,
    value: CgExpr,
    line: usize,
    col: usize,
    type_str: Option<String>,
) -> CgExpr {
    let addr = CgExpr {
        kind: CgExprKind::Unary, type_str: None, line, col,
        data: CgExprData::Unary { op_str: "&".into(), operand: Box::new(target.clone()), is_postfix: false },
    };
    let cast_addr = CgExpr {
        kind: CgExprKind::Cast, type_str: None, line, col,
        data: CgExprData::Cast { target_type: "NPObject **".into(), expr: Box::new(addr) },
    };
    let cast_value = CgExpr {
        kind: CgExprKind::Cast, type_str: None, line, col,
        data: CgExprData::Cast { target_type: "NPObject *".into(), expr: Box::new(value.clone()) },
    };
    let call = |name: &str, args: Vec<CgExpr>| CgExpr {
        kind: CgExprKind::Call, type_str: Some("void".into()), line, col,
        data: CgExprData::Call {
            name: name.into(), args,
            vtable_class: None, alt_vtable_classes: vec![],
            is_class_method: false, is_super: false,
            sel_const_name: None, method_index: None, direct_specialized: false,
        },
    };
    CgExpr {
        kind: CgExprKind::Comma, type_str, line, col,
        data: CgExprData::Comma(vec![
            call("jeti_weakUnregister", vec![cast_addr.clone()]),
            CgExpr {
                kind: CgExprKind::Assign, type_str: None, line, col,
                data: CgExprData::Assign { target: Box::new(target), value: Box::new(value) },
            },
            call("jeti_weakRegister", vec![cast_addr, cast_value]),
        ]),
    }
}

fn is_owned_object_ivar_type(ty: &str) -> bool {
    let t = ty.trim();
    // `id` (and the runtime's `jeti_id_t`) are object pointers spelled without
    // a `*`, so they never reach the pointer checks below.
    if t == "id" || t == "jeti_id_t" { return true; }
    if t.contains("(*") { return false; }                  // function pointer
    if t.contains("struct __jeti_block") { return false; }  // block layout
    if !t.ends_with('*') { return false; }
    // Exactly ONE level of indirection. `NPObject **` is a C array of objects
    // (Foundation's NPArray/NPDictionary back `_items`/`_keys`/`_values` with
    // one) — releasing it would free the array, not an element.
    if t.matches('*').count() != 1 { return false; }
    let base = t.trim_end_matches('*').trim();
    !matches!(base,
        "char" | "signed char" | "unsigned char" | "short" | "unsigned short"
        | "int" | "unsigned int" | "unsigned" | "long" | "unsigned long"
        | "long long" | "unsigned long long" | "float" | "double" | "long double"
        | "bool" | "_Bool" | "void")
}

fn owned_ivars_of(cm: &CgClassMeta) -> Vec<String> {
    cm.ivar_names
        .iter()
        .zip(cm.ivar_types.iter())
        .zip(cm.ivar_weak.iter())
        .filter(|((_, ty), weak)| !**weak && is_owned_object_ivar_type(ty))
        .map(|((n, _), _)| n.clone())
        .collect()
}

/// Synthesize ivar release for classes that do NOT declare their own `dealloc`.
///
/// OWNERSHIP RULE (deliberate, and narrower than ObjC's):
///   * NO user `dealloc` → ARC releases the class's owned object ivars. This is
///     the leak this pass exists to close: such a class previously destroyed
///     its instance without ever releasing the objects it owned.
///   * a user `dealloc` EXISTS → ARC stays out of the way. The body commonly
///     releases ivars itself (handwritten `NPObject_release(_x)`, `[_x release]`,
///     or an OO cascade), and a synthesized release on top of that is a double
///     free. A class that declares `dealloc` has declared its ivar policy.
///
/// Releasing only `jeti_release`-style is nil-safe, so a half-initialized
/// object is safe to destroy, and order is REVERSE declaration (stack order).
/// The wrapper never rewrites the user's body; it *is* the class's `dealloc`
/// entry in the metadata table.
fn emit_arc_dealloc_wrappers(classes: &[CgClassMeta]) -> (std::collections::HashMap<String, String>, String) {
    use std::collections::HashMap;
    // Only a dealloc the class DEFINED ITSELF counts. `method_names` also
    // carries inherited entries (with their real owner recorded in
    // `method_owners`), and treating an inherited NPObject_dealloc as "the user
    // wrote one" would switch the synthesis off for every subclass.
    let has_user_dealloc = |c: &CgClassMeta| {
        let flat = name_flat(&c.class_name);
        c.method_names.iter().enumerate().any(|(p, n)| {
            n == "dealloc"
                && !c.is_class_methods[p]
                && c.method_owners.get(p).map_or(false, |o| name_flat(o) == flat)
        })
    };
    let owned: HashMap<String, Vec<String>> = classes
        .iter()
        .map(|c| (name_flat(&c.class_name), owned_ivars_of(c)))
        .collect();
    let mut names: HashMap<String, String> = HashMap::new();
    for c in classes {
        let flat = name_flat(&c.class_name);
        if owned.get(&flat).map_or(true, |v| v.is_empty()) { continue; }
        if has_user_dealloc(c) { continue; }
        names.insert(flat.clone(), format!("{}__jeti_arc_dealloc", flat));
    }

    let mut defs = String::new();
    for c in classes {
        let flat = name_flat(&c.class_name);
        let ivars = match owned.get(&flat) { Some(v) if !v.is_empty() => v, _ => continue };
        if has_user_dealloc(c) { continue; }

        // Chain to whatever the superclass's dealloc entry is, so inherited
        // ivars are released too.
        let entry = match c.super_name.as_deref().map(name_flat) {
            Some(sup) => {
                if names.contains_key(&sup) {
                    format!("    {}__jeti_arc_dealloc(self, _cmd);\n", sup)
                } else {
                    let sup_user = classes
                        .iter()
                        .find(|x| name_flat(&x.class_name) == sup)
                        .and_then(|x| {
                            x.method_names.iter().position(|n| n == "dealloc")
                                .filter(|&p| !x.is_class_methods[p])
                                .map(|p| x.method_owners.get(p).cloned().unwrap_or_else(|| sup.clone()))
                        });
                    match sup_user {
                        Some(o) => format!("    {}_dealloc(self, _cmd);\n", o),
                        None => String::new(),
                    }
                }
            }
            None => String::new(),
        };

        let releases: String = ivars
            .iter()
            .rev()
            .map(|n| format!("    jeti_release((NPObject *)(((struct {} *)self)->{}));\n", flat, n))
            .collect();

        defs.push_str(&format!(
            "/* ARC: release '{}'s owned ivars (no user dealloc) */\n\
             static void {}__jeti_arc_dealloc(NPObject * self, SEL _cmd) {{\n{}{}}}\n\n",
            c.class_name,
            flat,
            entry,
            releases,
        ));
    }
    (names, defs)
}

/// Flatten a class reference (`Name<Arg *>`) into the symbol suffix codegen
/// uses for its metadata (`Name_Arg_ptr`). Exposed so the multi-TU driver can
/// tell whether a demanded specialization is already defined inside a
/// precompiled library without re-deriving the mangling.
pub fn symbol_flat_name(fqn: &str) -> String {
    name_flat(fqn)
}

fn name_flat(fqn: &str) -> String {
    // First mangle generic type arguments: `Name<T1, T2*>` → `Name_T1_T2_ptr`
    // so that e.g. `DataPack<QuantumToken*>` becomes `DataPack_QuantumToken_ptr`.
    // This is used for vtable/class metadata symbols emitted per instantiation.
    let mut out = String::new();
    let mut depth = 0; // inside <...>?
    let mut cur_arg = String::new();
    let mut base = String::new();
    let mut in_args = false;
    for ch in fqn.chars() {
        if !in_args {
            if ch == '<' {
                in_args = true;
                depth = 1;
            } else {
                base.push(ch);
            }
            continue;
        }
        // inside <...>
        if ch == '<' { depth += 1; cur_arg.push(ch); continue; }
        if ch == '>' {
            depth -= 1;
            if depth == 0 {
                // close this arg group
                let mangled = mangle_one_arg(&cur_arg);
                out.push_str(&mangled);
                cur_arg.clear();
                in_args = false;
                continue;
            }
            cur_arg.push(ch);
            continue;
        }
        if ch == ',' && depth == 1 {
            // Flush this arg; the '_' separator goes AFTER it (the base↔args
            // separator comes from `full.push('_')` below). Pushing '_' before
            // the mangled arg gave the first arg a leading underscore and
            // dropped the inter-arg separator for multi-arg generics
            // (`NPDictionary__NPString_ptrNFNumber_ptr`).
            let mangled = mangle_one_arg(&cur_arg);
            out.push_str(&mangled);
            out.push('_');
            cur_arg.clear();
        } else {
            cur_arg.push(ch);
        }
    }
    // Split trailing `*` from base so `Node*` → `Node_ptr`, `Node **` → `Node_ptr_ptr`.
    // Anything after the first `*` (including more `*` and whitespace) is pointer levels.
    let (base_ident, base_ptr_levels) = split_trailing_ptrs(&base);
    // Build final: base + "_" + args if any args were rendered
    if out.is_empty() {
        // No generic args — just :: replacement + trailing _ptr
        let mut result = base_ident.replace("::", "__");
        for _ in 0..base_ptr_levels {
            result.push_str("_ptr");
        }
        return result;
    }
    let mut full = base_ident.replace("::", "__");
    full.push('_');
    full.push_str(&out);
    for _ in 0..base_ptr_levels {
        full.push_str("_ptr");
    }
    full
}

/// Split a base string into (identifier_part, trailing_*_count).
/// `Node*` → (`Node*`, 0)  — wait, we want to split *before* the first `*`.
/// Actually: `Node` + count=1; `Node **` → `Node` + 2; `Box` → `Box` + 0.
fn split_trailing_ptrs(base: &str) -> (String, usize) {
    // Find the first `*` position — everything from there is trailing pointers.
    if let Some(pos) = base.find('*') {
        let ident = base[..pos].trim_end().to_string();
        let mut count = 0;
        for ch in base[pos..].chars() {
            if ch == '*' { count += 1; }
        }
        (ident, count)
    } else {
        (base.to_string(), 0)
    }
}

// Mangle a single generic type argument like `QuantumToken *` → `QuantumToken_ptr`.
// Handles nested generics: `Box<Node*>` → `Box_Node_ptr`,
// `Box<Box<Node*>>` → `Box_Box_Node_ptr_ptr`.
/// Are every type argument in the spelling `Base<a1, a2>` one of the
/// template's declared type parameters? Param-keyed spellings (`ChainArray<T>`
/// inside ChainBox's body) are the erased template itself, not an
/// instantiation. Bare identifiers only: a concrete specialization
/// (`Box<NPString *>`, `Box<Array<int> *>`) never matches.
fn spelling_args_are_template_params(name: &str, params: &[String]) -> bool {
    let Some(lt) = name.find('<') else { return false };
    let Some(gt) = name.rfind('>') else { return false };
    if gt < lt { return false; }
    let args_str = &name[lt + 1..gt];
    let mut depth = 0usize;
    let mut cur = String::new();
    let mut saw_any = false;
    let check = |arg: &str| -> bool {
        let a = arg.trim().trim_end_matches('*').trim();
        !a.is_empty() && params.iter().any(|p| p == a)
    };
    for ch in args_str.chars() {
        match ch {
            '<' => { depth += 1; cur.push(ch); }
            '>' => { if depth == 0 { return false; } depth -= 1; cur.push(ch); }
            ',' if depth == 0 => {
                saw_any = true;
                if !check(&cur) { return false; }
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    // Last (or only) argument — a single-arg spelling never sees a comma.
    check(&cur)
}

/// AstType-level twin of [`spelling_args_are_template_params`]: the collector
/// carries structured args (`TypePrim::Param`, or `Named("T")` from the raw
/// spelling parse), whose RENDERED form (`NPObject * /*T*/`) can never match
/// a parameter name textually — decide on the semantics instead.
fn ast_args_are_template_params(args: &[AstType], params: &[String]) -> bool {
    if args.is_empty() { return false; }
    let name_matches = |n: &str| params.iter().any(|p| p == n);
    args.iter().all(|a| {
        if (a.prim == TypePrim::Param || a.prim == TypePrim::Named)
            && a.name.as_deref().map(&name_matches).unwrap_or(false)
        {
            return true;
        }
        if let Some(ref s) = a.subtype {
            if s.prim == TypePrim::Param && s.name.as_deref().map(&name_matches).unwrap_or(false) {
                return true;
            }
        }
        false
    })
}

/// R2 ownership for a class spelling, cloning included.
///
/// `owned_classes` holds the names the TU's MAIN file implements
/// (`@implementation ChainBox`), so a clone spelled `ChainBox<int *>` is never
/// in it — yet its metadata has exactly one authoritative home: the TU that
/// owns the template. Falling back to the base name makes the owner emit strong
/// clone metadata, which is what keeps the weak copies in client TUs from
/// winning the linker merge (a client's vtable slots are NULL — that dispatch
/// ends at address 0).
fn class_is_owned(unit: &CgUnit, class_name: &str) -> bool {
    // A template-instantiation TU owns every SPECIALIZATION it materializes
    // (nothing else in the link can define one) while the bare template class
    // stays the archive's. A strong specialization table is what stops the
    // client's empty weak stub from winning the weak-merge.
    if unit.instantiate_only && class_name.contains('<') {
        return true;
    }
    unit.owned_classes.contains(class_name)
        || class_name
            .split('<')
            .next()
            .map_or(false, |base| base != class_name && unit.owned_classes.contains(base))
}


/// Render a type argument the way the elaborator bakes it into class_ref
/// spellings (`ast_type_render_args` twin): a type parameter renders as its
/// bare name (`T`), a pointer gains ` *`, nested generics recurse. The
/// closure machinery keys marker flats on this — the collection side and
/// the clone-time rewrite must produce IDENTICAL spellings.
fn ast_type_to_argname(t: &AstType) -> String {
    let base = t.name.clone().unwrap_or_else(|| match t.prim {
        TypePrim::Void => "void".into(),
        TypePrim::Char => "char".into(),
        TypePrim::Int => "int".into(),
        TypePrim::Long => "long".into(),
        TypePrim::LongLong => "long long".into(),
        TypePrim::Float => "float".into(),
        TypePrim::Double => "double".into(),
        TypePrim::Bool => "BOOL".into(),
        TypePrim::Unsigned => "unsigned".into(),
        _ => "id".into(),
    });
    let inner_args = if t.type_args.is_empty() {
        String::new()
    } else {
        let rendered = t.type_args.iter().map(ast_type_to_argname).collect::<Vec<_>>().join(", ");
        format!("<{}>", rendered)
    };
    let stars = if t.is_pointer { " *" } else { "" };
    format!("{}{}{}", base, inner_args, stars)
}

fn mangle_one_arg(arg: &str) -> String {
    let trimmed = arg.trim();
    let mut s = String::new();
    // strip qualifiers we don't want in mangled name
    let cleaned = trimmed.replace("const ", "").replace("volatile ", "");
    let mut iter = cleaned.chars().peekable();
    let mut name = String::new();
    let mut ptr_count = 0;
    // collect identifier name (may include ::)
    while let Some(&c) = iter.peek() {
        if c.is_alphanumeric() || c == '_' || c == ':' {
            name.push(c);
            iter.next();
        } else {
            break;
        }
    }
    // Nested generic args: `Name<...>` → `Name_` + mangled inner args
    if iter.peek() == Some(&'<') {
        iter.next(); // consume '<'
        let flat_base = name.replace("::", "__");
        s.push_str(&flat_base);
        s.push('_');
        // collect inner until matching '>'
        let mut depth = 1;
        let mut inner = String::new();
        while let Some(&c) = iter.peek() {
            if c == '<' { depth += 1; inner.push(c); iter.next(); }
            else if c == '>' {
                depth -= 1;
                if depth == 0 { iter.next(); break; }
                inner.push(c);
                iter.next();
            } else {
                inner.push(c);
                iter.next();
            }
        }
        // inner is like `Node *` or `Box<Node *>` — mangle via name_flat
        s.push_str(&name_flat(&inner));
        // After the closing '>', there may be trailing `*` for pointer levels
        // (already accounted for in the inner name_flat for nested generics,
        // but for a simple `Node *` the trailing `*` is part of inner).
    } else {
        // Simple identifier — render with trailing `*` → `_ptr`
        let flat = name.replace("::", "__");
        s.push_str(&flat);
    }
    // count trailing `*` that are NOT part of the generic args
    while let Some(&c) = iter.peek() {
        if c == '*' { ptr_count += 1; iter.next(); }
        else if c.is_whitespace() { iter.next(); }
        else { break; }
    }
    for _ in 0..ptr_count { s.push_str("_ptr"); }
    s
}

fn cst_type_to_c_str(ct: &jeti_cst::CstType) -> String {
    // Anonymous inline struct/union — see `ast_type_to_c_str` above.
    if let Some(ref body) = ct.anon_struct_body {
        let tag = match ct.tag {
            TagKind::Union => "union",
            TagKind::Enum => "enum",
            _ => "struct",
        };
        return format!("{} {}", tag, body);
    }
    if ct.is_fn_ptr {
        let ret = ct.subtype.as_ref().map(|s| cst_type_to_c_str(s)).unwrap_or_else(|| "void".into());
        let mut params = String::new();
        let mut bp = ct.block_params.as_ref();
        while let Some(b) = bp {
            if !params.is_empty() { params.push_str(", "); }
            params.push_str(&cst_type_to_c_str(b));
            bp = b.next.as_ref();
        }
        if params.is_empty() { params.push_str("void"); }
        // `int (*fp)(int, ...)`: the ellipsis is part of the TYPE, and the cast
        // must stay signature-exact (AGENTS.md iron law 6).
        if ct.is_variadic {
            params = if ct.block_params.is_none() { "...".into() } else { format!("{params}, ...") };
        }
        if let Some(ref name) = ct.block_name {
            return format!("{} (*{})({})", ret, name, params);
        }
        return format!("{} (*)({})", ret, params);
    }
    if ct.is_block {
        let ret = ct.subtype.as_ref().map(|s| cst_type_to_c_str(s)).unwrap_or_else(|| "void".into());
        let mut params = String::new();
        let mut bp = ct.block_params.as_ref();
        while let Some(b) = bp {
            if !params.is_empty() { params.push_str(", "); }
            params.push_str(&cst_type_to_c_str(b));
            bp = b.next.as_ref();
        }
        if params.is_empty() { params.push_str("void"); }
        // Block types: clang keeps `^`; gcc/portable use the shared header ptr.
        // (pointer and non-pointer block forms both reduce to the header ptr).
        return block_type_c_str(&ret, &params);
    }
    if ct.is_array {
        let base = ct.subtype.as_ref().map(|s| cst_type_to_c_str(s)).unwrap_or_else(|| "int".into());
        if ct.array_size > 0 { return format!("{}[{}]", base, ct.array_size); }
        return format!("{}[]", base);
    }
    // Recurse through pointer chain to build correct type with all * levels
    if ct.is_pointer {
        let base = cst_type_to_c_str(ct.subtype.as_ref().unwrap());
        if ct.is_const {
            return format!("{} * const", base);
        }
        return format!("{} *", base);
    }
    let mut s = String::new();
    if ct.is_const { s.push_str("const "); }
    if ct.is_unsigned && ct.prim != TypePrim::Unsigned { s.push_str("unsigned "); }
    match ct.prim {
        TypePrim::Void => s.push_str("void"),
        TypePrim::Char => s.push_str("char"),
        TypePrim::Short => s.push_str("short"),
        TypePrim::Int => s.push_str("int"),
        TypePrim::Long => s.push_str("long"),
        TypePrim::LongLong => s.push_str("long long"),
        TypePrim::Float => {
            s.push_str("float");
            if ct.is_complex { s.push_str(" _Complex"); }
        }
        TypePrim::Double => {
            s.push_str("double");
            if ct.is_complex { s.push_str(" _Complex"); }
        }
        TypePrim::Bool => s.push_str("_Bool"),
        TypePrim::Signed => s.push_str("signed"),
        TypePrim::Unsigned => s.push_str("unsigned"),
        TypePrim::Id => s.push_str("NPObject *"),
        TypePrim::Class => s.push_str("NPClass *"),
        TypePrim::Sel => s.push_str("SEL"),
        TypePrim::Instancetype => s.push_str("NPObject *"),
        // Type parameter (T in `@interface X<T>`). Rendered with a comment
        // sentinel so monomorphization can replace it by position instead of
        // blindly replacing every `NPObject *` (which would also hit real `id`).
        // Even if a sentinel leaks into the output it is legal C, semantically
        // identical to the bare type.
        TypePrim::Param => match ct.name.as_deref() {
            // Per-name sentinel: the monomorphization pass pairs each param
            // name with its own type argument (`/*K*/`→args[0], `/*V*/`→args[1]).
            // A nameless Param falls back to the generic `/*T*/` marker.
            Some(n) if !n.is_empty() => s.push_str(&format!("NPObject * /*{}*/", n)),
            _ => s.push_str(T_PARAM_RENDER),
        },
        TypePrim::Named => {
            if let Some(ref name) = ct.name {
                // NPTask<T> is the task HANDLE, not a monomorphized class:
                // render as NPTask * (docs/plans/async_nptask_plan.md, stage D) —
                // there is no NPTask_int typedef; the handle type is uniform.
                if name == "NPTask" && !ct.type_args.is_empty() {
                    s.push_str("NPTask *");
                    return s;
                }
                let flat = if ct.type_args.is_empty() {
                    name_flat(name)
                } else {
                    let args_str = ct.type_args.iter()
                        .map(cst_type_to_c_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    name_flat(&format!("{}<{}>", name, args_str))
                };
                if ct.is_struct { s.push_str(&format!("{} {}", ct.tag.keyword(), flat)); }
                else { s.push_str(&flat); }
            } else { s.push_str("int"); }
        }
    }
    s
}

pub fn ast_type_to_c_str(t: &AstType) -> String {
    // An anonymous inline struct/union (`static struct { … } t[N];`): there is
    // no tag to name, so re-emit the body verbatim. Without this the type
    // rendered as `int` and every field access on it failed to compile.
    if let Some(ref body) = t.anon_struct_body {
        let tag = match t.tag {
            TagKind::Union => "union",
            TagKind::Enum => "enum",
            _ => "struct",
        };
        return format!("{} {}", tag, body);
    }
    if t.is_fn_ptr {
        let ret = t.subtype.as_ref().map(|s| ast_type_to_c_str(s)).unwrap_or_else(|| "void".into());
        let mut params = String::new();
        let mut bp = t.block_params.as_ref();
        while let Some(b) = bp {
            if !params.is_empty() { params.push_str(", "); }
            params.push_str(&ast_type_to_c_str(b));
            bp = b.next.as_ref();
        }
        if params.is_empty() { params.push_str("void"); }
        // `int (*fp)(int, ...)` — see the CST arm above.
        if t.is_variadic {
            params = if t.block_params.is_none() { "...".into() } else { format!("{params}, ...") };
        }
        if let Some(ref name) = t.block_name {
            // Function pointer array: `int (*row[4])(int)`
            if t.is_array {
                let size = if t.array_size > 0 {
                    format!("[{}]", t.array_size)
                } else if let Some(ref s) = t.array_size_name {
                    format!("[{}]", s)
                } else {
                    "[]".to_string()
                };
                return format!("{} (*{}{})({})", ret, name, size, params);
            }
            return format!("{} (*{})({})", ret, name, params);
        }
        return format!("{} (*)({})", ret, params);
    }
    if t.is_block {
        let ret = t.subtype.as_ref().map(|s| ast_type_to_c_str(s)).unwrap_or_else(|| "void".into());
        let mut params = String::new();
        let mut bp = t.block_params.as_ref();
        while let Some(b) = bp {
            if !params.is_empty() { params.push_str(", "); }
            params.push_str(&ast_type_to_c_str(b));
            bp = b.next.as_ref();
        }
        if params.is_empty() { params.push_str("void"); }
        if let Some(ref bn) = t.block_name {
            if is_clang_backend() {
                return format!("{} (^{})({})", ret, bn, params);
            }
            return format!("struct __jeti_block_header *{}", bn);
        }
        return block_type_c_str(&ret, &params);
    }
    if t.is_array {
        let base = t.subtype.as_ref().map(|s| ast_type_to_c_str(s)).unwrap_or_else(|| "int".into());
        if t.array_size > 0 { return format!("{}[{}]", base, t.array_size); }
        // Symbolic size (e.g. `MAX_CHILDREN`) — emit the named constant
        // so the field is a fixed-size array, NOT a flexible array member
        // (C forbids FAMs outside the trailing field).
        if let Some(ref name) = t.array_size_name {
            return format!("{}[{}]", base, name);
        }
        return format!("{}[]", base);
    }
    let mut s = String::new();

    // For pointer types, const on the pointed-to type (subtype) goes BEFORE the base type
    if t.is_pointer {
        if t.subtype.as_ref().map(|st| st.is_const).unwrap_or(false) {
            s.push_str("const ");
        }
    } else {
        // For non-pointer types, const applies to the type itself
        if t.is_const { s.push_str("const "); }
    }

    if t.is_unsigned && t.prim != TypePrim::Unsigned { s.push_str("unsigned "); }

    match t.prim {
        TypePrim::Void => s.push_str("void"),
        TypePrim::Char => s.push_str("char"),
        TypePrim::Short => s.push_str("short"),
        TypePrim::Int => s.push_str("int"),
        TypePrim::Long => s.push_str("long"),
        TypePrim::LongLong => s.push_str("long long"),
        TypePrim::Float => {
            s.push_str("float");
            if t.is_complex { s.push_str(" _Complex"); }
        }
        TypePrim::Double => {
            s.push_str("double");
            if t.is_complex { s.push_str(" _Complex"); }
        }
        TypePrim::Bool => s.push_str("_Bool"),
        TypePrim::Signed => s.push_str("signed"),
        TypePrim::Unsigned => s.push_str("unsigned"),
        TypePrim::Id => s.push_str("NPObject *"),
        TypePrim::Class => s.push_str("NPClass *"),
        TypePrim::Sel => s.push_str("SEL"),
        TypePrim::Instancetype => s.push_str("NPObject *"),
        TypePrim::Param => match t.name.as_deref() {
            // Per-name sentinel — see the cst_type_to_c_str Param arm.
            Some(n) if !n.is_empty() => s.push_str(&format!("NPObject * /*{}*/", n)),
            _ => s.push_str(T_PARAM_RENDER),
        },
        TypePrim::Named => {
            if let Some(ref name) = t.name {
                // Use class_ref if available (resolved by elaborator), otherwise use name
                let type_name = if let Some(ref cr) = t.class_ref {
                    cr
                } else {
                    name
                };
                // For generic instantiations (e.g. `Box<Node*>`), the flat
                // name must include the mangled type arguments so that
                // `Box<Node*>` → `Box_Node_ptr` (matches the specialized
                // struct/vtable symbols). When there are no type args,
                // `name_flat` is a passthrough. BUT: a type arg that is
                // itself a type parameter (`+ (NPArray<T>)wrap:` inside the
                // generic template) renders as the `NPObject * /*T*/`
                // sentinel, and mangling that would fabricate a typedef
                // that does not exist (`NPArray_NPObject_ptr`) in BOTH the
                // template and the per-instantiation clone. Fall back to
                // the erased flat base name — legal C in both; the clone's
                // signature substitution rewrites return/param strings
                // separately (param_pairs replace on the whole type string).
                // NPTask<T> → the task HANDLE `NPTask *` (uniform C type; no
                // per-instantiation typedef exists). Same rule as the CST
                // path (docs/plans/async_nptask_plan.md, stage D).
                if type_name == "NPTask" && !t.type_args.is_empty() {
                    s.push_str("NPTask *");
                    return s;
                }
                let contains_param_arg = t.type_args.iter()
                    .any(|a| a.prim == TypePrim::Param
                        || a.subtype.as_ref().map(|s| s.prim == TypePrim::Param).unwrap_or(false));
                let flat = if t.type_args.is_empty() || contains_param_arg {
                    // `type_name` may be the class_ref spelling with the args
                    // baked in (`ChainArray<T>`); a Param argument means the
                    // ERASED template — mangle the base only, never the
                    // parameter name (`ChainArray_T` fabricates a symbol).
                    name_flat(type_name.split('<').next().unwrap_or(type_name))
                } else {
                    let args_str = t.type_args.iter()
                        .map(ast_type_to_c_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    name_flat(&format!("{}<{}>", type_name, args_str))
                };
                if t.is_struct {
                    s.push_str(&format!("{} {}", t.tag.keyword(), flat));
                } else {
                    s.push_str(&flat);
                }
            } else {
                s.push_str("int");
            }
        }
    }

    if t.is_pointer {
        // Recurse into the pointed-to type so ALL qualifiers on the base
        // type survive (e.g. `const unsigned char *` → `const unsigned
        // char *`, not `const char *`). Recursion also naturally handles
        // multi-level pointers (`int **` → `int * *`) and const pointers
        // (`char * const`).
        if let Some(ref sub) = t.subtype {
            s = ast_type_to_c_str(sub);
        }
        s.push_str(" *");
        // const on the pointer itself goes AFTER the *
        if t.is_const {
            s.push_str(" const");
        }
        return s;
    }

    // Generic type args
    if !t.type_args.is_empty() {
        // Just use the base name for generics
    }

    s
}

// ─── AST → CG conversion ─────────────────────────────────────────────────────

fn op_to_str(op: i32, is_assign: bool) -> &'static str {
    if is_assign {
        match op {
            0 => "=", 1 => "+=", 2 => "-=", 3 => "*=", 4 => "/=", 5 => "%=",
            6 => "&=", 7 => "|=", 8 => "^=", 9 => "<<=", 10 => ">>=",
            _ => "=",
        }
    } else {
        match op {
            1 => "*", 2 => "/", 3 => "%", 4 => "+", 5 => "-",
            6 => "<<", 7 => ">>", 8 => "<", 9 => ">", 10 => "<=", 11 => ">=",
            12 => "==", 13 => "!=", 14 => "&", 15 => "^", 16 => "|",
            17 => "&&", 18 => "||",
            // Compound assignment operators (used in AstExprData::Binary when is_assign is not tracked)
            100 => "+=", 101 => "-=", 102 => "*=", 103 => "/=", 104 => "%=",
            105 => "&=", 106 => "|=", 107 => "^=", 108 => "<<=", 109 => ">>=",
            _ => "?",
        }
    }
}

#[derive(Debug, Clone)]
struct ClassInfo {
    class_name: String,
    flat: String,
    super_name: Option<String>,
    /// Declared generic type params in order (`NPDictionary<K, V>` →
    /// ["K", "V"]). Pairs each instantiation's type_args with the right
    /// per-name Param sentinel during monomorphization.
    type_params: Vec<String>,
    method_names: Vec<String>,
    method_sel_names: Vec<String>,  // original selectors (with colons)
    is_class_methods: Vec<bool>,
    method_bodies: Vec<Option<Box<CgStmt>>>,
    method_return_types: Vec<String>,
    method_params_list: Vec<Vec<(String, String)>>,
    method_variadic: Vec<bool>, // parallel to method_names: true → C `...` method
    method_owners: Vec<String>,
    /// Names of methods introduced by a category declaration (`@interface
    /// Dog (Tricks)`) in this unit. Keyed by NAME, not a parallel array:
    /// method_names gets appended out of order (inherited/global methods
    /// merge later), so index-parallel tracking silently misaligns.
    category_method_names: std::collections::HashSet<String>,
    has_category_impl: bool,
    /// Protocol names this class conforms to, resolved to FQN by the
    /// elaborator. Emitted into the class's static protocol table (D5.2,
    /// docs/plans/categories_protocol_plan.md §7): `protocol_count` stops being
    /// hardcoded 0 and the runtime query can answer conformances.
    protocols: Vec<String>,
    ivar_types: Vec<String>,
    ivar_names: Vec<String>,
    ivar_weak: Vec<bool>,
    has_impl_decl: bool,
}

// Render an AstExpr callee to a C expression string (used for block invocation on ivars)
fn render_callee_expr(ae: &AstExpr) -> String {
    match &ae.data {
        AstExprData::IvarRef { obj, ivar, cls, .. } => {
            let obj_str = render_callee_expr(obj);
            if let Some(ref iv) = ivar {
                if let Some(ref cls_name) = cls {
                    if obj_str == "self" {
                        let flat = name_flat(cls_name);
                        format!("((struct {} *)self)->{}", flat, iv)
                    } else {
                        format!("{}->{}", obj_str, iv)
                    }
                } else {
                    format!("{}->{}", obj_str, iv)
                }
            } else {
                obj_str
            }
        }
        AstExprData::VarRef { name, .. } => {
            if ae.kind == AstExprKind::Self_ || ae.kind == AstExprKind::Super {
                "self".to_string()
            } else {
                name.clone()
            }
        }
        AstExprData::PropRef { obj, name, is_arrow, prop, cls, .. } => {
            // Plain C struct field as call target (`w.cb(3)` where cb is a
            // fn-ptr member — C-superset member call, mirroring the PropRef
            // emission arm's `.`/`->` decision). ObjC property getters
            // (prop+cls Some) are vtable dispatches, not renderable callees.
            if prop.is_some() && cls.is_some() {
                return String::new();
            }
            let obj_str = render_callee_expr(obj);
            if obj_str.is_empty() {
                return String::new();
            }
            let obj_is_ptr = obj.expr_type.as_ref().map_or(false, |t| t.is_pointer);
            if *is_arrow || obj_is_ptr {
                format!("{}->{}", obj_str, name)
            } else {
                format!("{}.{}", obj_str, name)
            }
        }
        // A parenthesized receiver: `(v).m`, `(v)->m`. The Paren wraps the
        // object, so without this arm the object rendered empty and the whole
        // call collapsed to a bare `(args)` — silent miscompile.
        AstExprData::Paren(inner) => format!("({})", render_callee_expr(inner)),
        // A prefix-operator receiver: `(&v)->m`, `(*p).m`, …. Parenthesized so
        // it composes as an object (`(&v)->m`) and as a call target
        // (`(&f)(x)` — `&` binds looser than the call, so the parens matter).
        AstExprData::Unary { op, operand, .. } => {
            let op_str = match op {
                1 => "++", 2 => "--", 3 => "*", 4 => "&", 5 => "-", 6 => "+", 7 => "~", 8 => "!",
                _ => "?",
            };
            format!("({}{})", op_str, render_callee_expr(operand))
        }
        // A cast receiver: `((struct V *)p)->m`. Parenthesized for the same
        // reason — as a call target it must read `((T)x->m)(args)`, never
        // `(T)x->m(args)` (which casts the call result instead).
        AstExprData::Cast { target_type, expr } => {
            format!("(({}){})", ast_type_to_c_str(target_type), render_callee_expr(expr))
        }
        // An indexed receiver: `fns[i](args)`.
        AstExprData::Subscript { object, key } => {
            format!("{}[{}]", render_callee_expr(object), render_callee_expr(key))
        }
        _ => String::new(),
    }
}

fn expand_nplog_format(fmt: &str, args: &[AstExpr], class_infos: &std::collections::BTreeMap<String, ClassInfo>, line: usize, col: usize, type_str: Option<String>) -> CgExpr {
    let has_npstring = class_infos.contains_key("NPString");
    let mut new_fmt = String::with_capacity(fmt.len() + 16);
    let mut expanded_args: Vec<CgExpr> = Vec::with_capacity(args.len() + 1);
    let mut arg_idx = 0usize;
    let mut chars = fmt.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch == '%' {
            if let Some(&(_, '%')) = chars.peek() {
                new_fmt.push('%'); new_fmt.push('%');
                chars.next();
            } else if let Some(&(_, '@')) = chars.peek() {
                chars.next();
                if has_npstring {
                    // %@ → [[arg description] UTF8String] guarded by NULL ternary
                    new_fmt.push('%'); new_fmt.push('s');
                    if let Some(arg_ast) = args.get(arg_idx) {
                        let arg_cg = convert_expr(arg_ast, class_infos);
                        let desc_sel = sel_const_name("description");
                        let utf8_sel = sel_const_name("UTF8String");
                        let desc_call = CgExpr {
                            kind: CgExprKind::Call, type_str: Some("NPObject *".into()), line, col,
                            data: CgExprData::Call {
                                name: "description".into(),
                                args: vec![arg_cg.clone()],
                                vtable_class: Some("NPString".into()),
                                alt_vtable_classes: vec![],
                                is_class_method: false, is_super: false,
                                sel_const_name: Some(desc_sel),
                                method_index: None, direct_specialized: false,
                            },
                        };
                        let utf8_call = CgExpr {
                            kind: CgExprKind::Call, type_str: Some("const char *".into()), line, col,
                            data: CgExprData::Call {
                                name: "UTF8String".into(),
                                args: vec![desc_call],
                                vtable_class: Some("NPString".into()),
                                alt_vtable_classes: vec![],
                                is_class_method: false, is_super: false,
                                sel_const_name: Some(utf8_sel),
                                method_index: None, direct_specialized: false,
                            },
                        };
                        let null_str = CgExpr { kind: CgExprKind::String, type_str: None, line, col, data: CgExprData::String("(null)".into()) };
                        expanded_args.push(CgExpr {
                            kind: CgExprKind::Ternary, type_str: Some("const char *".into()), line, col,
                            data: CgExprData::Ternary {
                                cond: Box::new(arg_cg),
                                then: Box::new(utf8_call),
                                else_: Box::new(null_str),
                            },
                        });
                    } else {
                        expanded_args.push(CgExpr { kind: CgExprKind::String, type_str: None, line, col, data: CgExprData::String("(null)".into()) });
                    }
                } else {
                    // NPString not available — fall back to %p (print pointer)
                    new_fmt.push('%'); new_fmt.push('p');
                    if let Some(arg_ast) = args.get(arg_idx) {
                        expanded_args.push(convert_expr(arg_ast, class_infos));
                    } else {
                        expanded_args.push(CgExpr { kind: CgExprKind::Int, type_str: None, line, col, data: CgExprData::Int(0) });
                    }
                }
                arg_idx += 1;
            } else {
                // Other % specifier (e.g. %d, %s, %zu, %p, %f, %x, %c, %ld):
                // pass through the format char and consume one arg.
                new_fmt.push('%');
                if let Some(&(_, nc)) = chars.peek() {
                    new_fmt.push(nc);
                    chars.next();
                }
                if let Some(arg_ast) = args.get(arg_idx) {
                    expanded_args.push(convert_expr(arg_ast, class_infos));
                } else {
                    expanded_args.push(CgExpr { kind: CgExprKind::Int, type_str: None, line, col, data: CgExprData::Int(0) });
                }
                arg_idx += 1;
            }
        } else {
            new_fmt.push(ch);
        }
    }
    let mut all_args = vec![nplog_format_arg(new_fmt, has_npstring, 0, 0)];
    all_args.extend(expanded_args);
    CgExpr {
        kind: CgExprKind::Call, type_str, line, col,
        data: CgExprData::Call {
            name: "NPLog".into(), args: all_args,
            vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
            sel_const_name: None, method_index: None, direct_specialized: false,
        },
    }
}

/// Build the format argument for NPLog: when NPString is available, emit
/// `(NPString *)jeti_stringFromCstr("...")` so the format is passed as an NPString.
/// When NPString is absent, fall back to a raw C string (graceful degradation).
fn nplog_format_arg(fmt: String, has_npstring: bool, line: usize, col: usize) -> CgExpr {
    if has_npstring {
        CgExpr {
            kind: CgExprKind::Cast, type_str: Some("NPString *".into()), line, col,
            data: CgExprData::Cast {
                target_type: "NPString *".into(),
                expr: Box::new(CgExpr {
                    kind: CgExprKind::Call, type_str: Some("NPObject *".into()), line, col,
                    data: CgExprData::Call {
                        name: "jeti_stringFromCstr".into(),
                        args: vec![CgExpr { kind: CgExprKind::String, type_str: None, line, col, data: CgExprData::String(fmt) }],
                        vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }),
            },
        }
    } else {
        CgExpr { kind: CgExprKind::String, type_str: None, line, col, data: CgExprData::String(fmt) }
    }
}

fn convert_expr(ae: &AstExpr, class_infos: &std::collections::BTreeMap<String, ClassInfo>) -> CgExpr {
    let line = ae.line; let col = ae.col;
    let type_str = ae.expr_type.as_ref().map(|t| ast_type_to_c_str(t));
    // Handle kind-based matching for variants not in AstExprData
    if ae.kind == AstExprKind::Nil {
        return CgExpr { kind: CgExprKind::Ident, type_str, line, col, data: CgExprData::Ident("NULL".into()) };
    }
    if ae.kind == AstExprKind::Null {
        return CgExpr { kind: CgExprKind::Ident, type_str, line, col, data: CgExprData::Ident("NULL".into()) };
    }
    if ae.kind == AstExprKind::Self_ {
        return CgExpr { kind: CgExprKind::Ident, type_str: Some("NPObject *".into()), line, col, data: CgExprData::Ident("self".into()) };
    }
    if ae.kind == AstExprKind::Super {
        return CgExpr { kind: CgExprKind::Ident, type_str: Some("NPObject *".into()), line, col, data: CgExprData::Ident("super".into()) };
    }
    if ae.kind == AstExprKind::BlockLit {
        // BlockLit is handled in the match below via AstExprData::Block
    }
    match &ae.data {
        // `await e`: if the async desugar pass ran, no Await node survives to
        // codegen. This arm is the passthrough safety net (e.g. -fno-async or
        // an await outside any method): treat as its inner expression.
        AstExprData::Await { inner, .. } => convert_expr(inner, class_infos),
        // `@(expr)`: the checker rewrites this into the `NPNumber` factory
        // matching the operand's static type — only the checker has types, and
        // the C99 backend has no `_Generic` to fall back on. This arm is the
        // passthrough safety net (`-fno-checker`): treat it as its inner
        // expression, which is what a boxing-free reading of the source means.
        AstExprData::Boxed(inner) => convert_expr(inner, class_infos),
        // `({ ... })` statement expression: CgExpr has no statement slot, so
        // the body statements are converted and rendered to C text up front.
        AstExprData::StmtExpr(body) => {
            let mut inner = String::new();
            for s in body {
                let cs = convert_stmt(s, class_infos);
                emit_stmt_inline(&cs, &mut inner);
                inner.push(' ');
            }
            CgExpr { kind: CgExprKind::Raw, type_str, line, col, data: CgExprData::Raw(format!("({{ {} }})", inner.trim_end())) }
        }
        AstExprData::Int(val) => CgExpr { kind: CgExprKind::Int, type_str, line, col, data: CgExprData::Int(*val) },
        AstExprData::Float(val) => CgExpr { kind: CgExprKind::Float, type_str, line, col, data: CgExprData::Float(*val) },
        AstExprData::FloatRaw(raw) => CgExpr { kind: CgExprKind::Float, type_str, line, col, data: CgExprData::FloatRaw(raw.clone()) },
        AstExprData::String(s) => CgExpr { kind: CgExprKind::String, type_str, line, col, data: CgExprData::String(s.clone()) },
        AstExprData::AtString(s) => {
            let has_npstring = class_infos.contains_key("NPString");
            if has_npstring {
                CgExpr {
                    kind: CgExprKind::Call, type_str: Some("NPObject *".into()), line, col,
                    data: CgExprData::Call {
                        name: "jeti_stringFromCstr".into(),
                        args: vec![CgExpr { kind: CgExprKind::String, type_str: None, line, col, data: CgExprData::String(s.clone()) }],
                        vtable_class: None, alt_vtable_classes: vec![],
                        is_class_method: false, is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }
            } else {
                CgExpr { kind: CgExprKind::String, type_str, line, col, data: CgExprData::String(s.clone()) }
            }
        },
        AstExprData::Char(val) => CgExpr { kind: CgExprKind::Char, type_str, line, col, data: CgExprData::Char(*val) },
        AstExprData::Bool(val) => CgExpr { kind: CgExprKind::Int, type_str, line, col, data: CgExprData::Int(if *val { 1 } else { 0 }) },
        AstExprData::VarRef { name, .. } => {
            CgExpr { kind: CgExprKind::Ident, type_str, line, col, data: CgExprData::Ident(name.clone()) }
        },
        AstExprData::IvarRef { ivar, obj, cls, .. } => {
            let field = ivar.clone().unwrap_or_default();
            let obj_cg = convert_expr(obj, &class_infos);
            let ivar_type_str = cls.as_ref().and_then(|cls_name| {
                let flat = name_flat(cls_name);
                class_infos.get(&flat).and_then(|info| {
                    info.ivar_names.iter().position(|n| n == &field).map(|idx| info.ivar_types[idx].clone())
                })
            });
            let type_str = ivar_type_str.or(type_str);
            if let CgExprData::Ident(ref name) = obj_cg.data {
                if name == "self" || name == "_self" {
                    if let Some(ref cls_name) = cls {
                        let flat = name_flat(cls_name);
                        // Inline cast: ((struct Cls *)self)->ivar avoids stale _self
                        // when self is reassigned (e.g. self = [super init]).
                        return CgExpr { kind: CgExprKind::Arrow, type_str, line, col,
                            data: CgExprData::Arrow {
                                obj: Box::new(CgExpr {
                                    kind: CgExprKind::Cast, type_str: None, line, col,
                                    data: CgExprData::Cast {
                                        target_type: format!("struct {} *", flat),
                                        expr: Box::new(CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident("self".into()) }),
                                    },
                                }),
                                field,
                            },
                        };
                    }
                }
            }
            CgExpr { kind: CgExprKind::Arrow, type_str, line, col, data: CgExprData::Arrow { obj: Box::new(obj_cg), field } }
        }
        AstExprData::MsgSend { receiver, selector, args, is_class_method, is_super, super_name, .. } => {
            // Special case: [receiver class] -> ((NPClass *)((jeti_root *)receiver)->isa)
            // The "class" method is auto-generated on every meta vtable but NOT registered
            // in class_infos, so normal vtable dispatch can't find it. Emit the direct
            // ivar access which is semantically equivalent for all ObjC objects.
            // When the receiver is a class name (e.g. `[Array class]`), emit
            // `&jeti_<flat>_class` directly instead.
            if selector == "class" && !*is_super && args.is_empty() {
                if let AstExprData::VarRef { ref name, .. } = receiver.data {
                    let flat = name_flat(name);
                    if class_infos.values().any(|ci| ci.flat == flat || ci.class_name == *name) {
                        return CgExpr {
                            kind: CgExprKind::Unary, type_str, line, col,
                            data: CgExprData::Unary {
                                op_str: "&".into(),
                                operand: Box::new(CgExpr {
                                    kind: CgExprKind::Ident, type_str: None, line, col,
                                    data: CgExprData::Ident(meta_symbol("CLASS_", &flat)),
                                }),
                                is_postfix: false,
                            },
                        };
                    }
                }
                let obj_cg = convert_expr(receiver, &class_infos);
                let isa_access = CgExpr {
                    kind: CgExprKind::Arrow, type_str: None, line, col,
                    data: CgExprData::Arrow {
                        obj: Box::new(CgExpr {
                            kind: CgExprKind::Cast, type_str: None, line, col,
                            data: CgExprData::Cast {
                                target_type: "jeti_root *".to_string(),
                                expr: Box::new(obj_cg),
                            },
                        }),
                        field: "isa".to_string(),
                    },
                };
                return CgExpr {
                    kind: CgExprKind::Cast, type_str, line, col,
                    data: CgExprData::Cast {
                        target_type: "NPClass *".to_string(),
                        expr: Box::new(isa_access),
                    },
                };
            }
            let mut call_args = Vec::new();
            let mut effective_is_class = *is_class_method;

            // Special case: [receiver respondsToSelector:@selector(sel)] ->
            // a NULL-slot check on the uniform vtable (plus a nil receiver
            // check). The "respondsToSelector:" pseudo-method is NOT declared
            // in any @interface — the compiler implements it directly.
            //   - Slot NULL  = method not implemented by this class chain
            //     (declared in an @interface without @implementation, or
            //     protocol stub from propagate_protocol_methods).
            //   - The uniform vtable makes this a compile-time-known member
            //     name: selectors map 1:1 to sanitized member names.
            // Emits: jeti_resp_<member>(recv_expr) — the helper is declared as
            // a static CgDecl::Function here and emitted after the vtable
            // struct definition in emit_unit_with_headers.
            if selector == "respondsToSelector:" && !*is_super && args.len() == 1 {
                if let AstExprData::Selector(s) = &args[0].data {
                    let member = sanitize_sel_name(s);
                    // Only meaningful for instance methods; class methods live
                    // on the meta vtable which has no uniform layout.
                    // NOTE: METHOD_METADATA is not yet populated during the decl
                    // conversion pass, so check class_infos (built earlier) —
                    // the sanitized names there ARE the vtable member names.
                    let known_instance = class_infos.values().any(|ci| {
                        ci.method_names.iter().zip(ci.is_class_methods.iter())
                            .any(|(n, ic)| n == &member && !*ic)
                    });                    if known_instance {
                        // Register the member; the static helper's prototype
                        // and body are emitted in emit_unit_with_headers.
                        resp_helpers().get_or_insert_with(Default::default).insert(member.clone());
                        return CgExpr {
                            kind: CgExprKind::Call, type_str, line, col,
                            data: CgExprData::Call {
                                name: format!("jeti_resp_{}", member),
                                args: vec![upcast_to_npobject(convert_expr(receiver, &class_infos))],
                                vtable_class: None,
                                alt_vtable_classes: Vec::new(),
                                is_class_method: false,
                                is_super: false,
                                sel_const_name: None,
                                method_index: None, direct_specialized: false,
                            },
                        };
                    } else {
                        // The selector names no instance method in this TU, so
                        // it is not a member of the uniform vtable struct — the
                        // static answer is definitively NO (ObjC's runtime
                        // would also return NO for a selector no class
                        // implements). Emit a constant; no helper is possible
                        // (the member does not exist in the struct).
                        return CgExpr {
                            kind: CgExprKind::Int, type_str, line, col,
                            data: CgExprData::Int(0),
                        };
                    }
                }
            }
            // Special case: [receiver conformsToProtocol:@protocol(P)] ->
            // jeti_class_conformsToProtocol(recv->isa, &JETI_PROTO_$_P) —
            // metadata-based runtime query (D5.2). The protocol table must
            // be declared in this TU (a @protocol decl or a conforming
            // class brings the static instance in); otherwise the static
            // answer is NO, mirroring the respondsToSelector: fallback.
            if selector == "conformsToProtocol:" && !*is_super && args.len() == 1 {
                if let AstExprData::Selector(s) = &args[0].data {
                    // @protocol(P) arrives as selector text "P" (parser maps
                    // the @protocol(...) expression onto a Selector expr).
                    let pname = s.trim_start_matches("@protocol").trim().to_string();
                    let pname = pname.trim_end_matches(':').to_string();
                    if !pname.is_empty() {
                        let recv_c = convert_expr(receiver, &class_infos);
                        // The runtime query wants the NPClass: receiver -> isa
                        // (NPObject's isa field), not the receiver itself.
                        let recv_isa = CgExpr {
                            kind: CgExprKind::Arrow, type_str: None, line, col,
                            data: CgExprData::Arrow { obj: Box::new(recv_c), field: "isa".into() },
                        };
                        return CgExpr {
                            kind: CgExprKind::Call, type_str, line, col,
                            data: CgExprData::Call {
                                name: "jeti_class_conformsToProtocol".into(),
                                args: vec![
                                    recv_isa,
                                    CgExpr {
                                        kind: CgExprKind::Ident, type_str: Some("struct NPProtocol *".to_string()), line, col,
                                        data: CgExprData::Ident(format!("&JETI_PROTO_$_{}", name_flat(&pname))),
                                    },
                                ],
                                vtable_class: None,
                                alt_vtable_classes: Vec::new(),
                                is_class_method: false,
                                is_super: false,
                                sel_const_name: None,
                                method_index: None, direct_specialized: false,
                            },
                        };
                    }
                }
            }
            // Both arms below assign before reading, so there is no useful seed.
            let mut vtable_class: Option<String> = None;
            let mut alt_vtable_classes: Vec<String> = Vec::new();
            if *is_super {
                // For super calls, start with the direct superclass name.
                // Walk the superclass chain to find the nearest ancestor that
                // actually declares the method (the direct superclass may inherit
                // it — e.g. NPLayer4 doesn't have dealloc, only NPObject does).
                vtable_class = super_name.clone();
                // A super send inside a CLASS method (e.g. `[super alloc]`) is
                // not marked is_class_method in the AST — resolve it: if the
                // selector is a class method (+) of the ancestor chain, dispatch
                // through the parent's META vtable.
                if let Some(vc) = vtable_class.clone() {
                    let sel = sanitize_sel_name(selector);
                    let mut current = vc;
                    loop {
                        let flat = name_flat(&current);
                        if let Some(info) = class_infos.get(&flat) {
                            if let Some(idx) = info.method_names.iter().position(|n| n == &sel) {
                                if info.is_class_methods[idx] {
                                    effective_is_class = true;
                                }
                                break;
                            }
                            match info.super_name.clone() {
                                Some(sup) => current = sup,
                                None => break,
                            }
                        } else { break; }
                    }
                }
                if let Some(vc) = vtable_class.clone() {
                    let sel = sanitize_sel_name(selector);
                    let mut current = vc;
                    loop {
                        let flat = name_flat(&current);
                        let has = class_infos.get(&flat).map_or(false, |info| {
                            info.method_names.iter().any(|n| n == &sel)
                        });
                        if has { vtable_class = Some(current); break; }
                        match class_infos.get(&flat).and_then(|info| info.super_name.clone()) {
                            Some(sup) => current = sup,
                            None => break,
                        }
                    }
                }
                if vtable_class.is_none() {
                    // super_name not set — fallback: find any class that has the method
                    let sel = sanitize_sel_name(selector);
                    let mut best = None;
                    for (_, info) in class_infos.iter() {
                        if let Some(idx) = info.method_names.iter().position(|n| n == &sel) {
                            if let Some(owner) = info.method_owners.get(idx) {
                                if let Some(owner_info) = class_infos.get(owner) {
                                    if let Some(ref sup) = owner_info.super_name {
                                        if sup != "NPObject" {
                                            best = Some(sup.clone());
                                            break;
                                        }
                                        best = Some(sup.clone());
                                    }
                                }
                            }
                        }
                    }
                    vtable_class = best;
                }
            } else {
                // Try to resolve vtable class from the receiver's type first.
                let sel = sanitize_sel_name(selector);
                // True monomorphization: a class_ref that NAMES an
                // instantiation (`Box<int>`) resolves to the mangled clone's
                // class (`Box_int`), whose substituted signatures are the
                // ABI-correct ones. Only when that spelling does not exist
                // (or lacks the method) does the walk fall back to the
                // erased base template.
                if let Some(cr) = receiver.expr_type.as_ref().and_then(|t| t.class_ref.clone()) {
                    if cr.contains('<') {
                        // The clone itself is not in class_infos yet (clones
                        // are inserted after expression conversion), so
                        // resolve the method against the BASE template — and
                        // go direct ONLY for the one shape the uniform vtable
                        // cannot carry: a VALUE-type `T` return. The template's
                        // return-type metadata marks the T position with the
                        // `/*T*/` sentinel; when the return spells it, the
                        // erased `NPObject *` slot would truncate the clone's
                        // value — the clone's substituted signature is the only
                        // ABI-correct call. Pointer-shaped returns (the
                        // wildcard-join family) stay on the uniform vtable,
                        // which also resolves inherited methods correctly.
                        let base = cr.split('<').next().unwrap_or(&cr);
                        if let Some(info) = class_infos.get(&name_flat(base)) {
                            if let Some(idx) = info.method_names.iter().position(|n| n == &sel) {
                                if !info.is_class_methods[idx] {
                                    let ret = info.method_return_types.get(idx)
                                        .map(|r| r.as_str())
                                        .unwrap_or("");
                                    let ret_has_t = ret.contains("/*T*/")
                                        || ret.contains("/*K*/") || ret.contains("/*V*/");
                                    let self_implements = info.method_bodies.get(idx)
                                        .map_or(false, |b| b.is_some());
                                    if ret_has_t && self_implements {
                                        vtable_class = Some(cr);
                                        effective_is_class = info.is_class_methods[idx];
                                    }
                                }
                            }
                        }
                    }
                }
                vtable_class = vtable_class.or_else(|| {
                    receiver.expr_type.as_ref()
                    .and_then(|t| {
                        // The receiver's static class: prefer the
                        // elaborator-resolved `class_ref` (generic-aware), then
                        // the plain named-pointer spelling the checker stamps
                        // on every typed receiver (`Svc *s`). Without the
                        // second source only generic receivers resolved, and
                        // an ordinary `Svc *s` fell through to the
                        // any-class-with-this-selector guess — letting an
                        // unrelated class's signature (e.g. NPRunLoop's
                        // `- (void)run`) be cast onto a send whose receiver
                        // class declares `- (async void)run`.
                        if let Some(cr) = t.class_ref.as_ref() {
                            return Some(cr.clone());
                        }
                        if t.is_pointer && t.prim == TypePrim::Named {
                            return t.name.as_deref()
                                .map(|n| n.split('<').next().unwrap_or(n).to_string());
                        }
                        None
                    })
                    .and_then(|cr| {
                        let flat = name_flat(&cr);
                        class_infos.get(&flat).and_then(|info| {
                            if info.method_names.iter().any(|n| n == &sel) {
                                Some(info.class_name.clone())
                            } else {
                                // Walk superclass chain to find the method
                                let mut cur = cr.clone();
                                loop {
                                    let cflat = name_flat(&cur);
                                    if let Some(ci) = class_infos.get(&cflat) {
                                        if ci.method_names.iter().any(|n| n == &sel) {
                                            return Some(ci.class_name.clone());
                                        }
                                        match ci.super_name.clone() {
                                            Some(sup) => cur = sup,
                                            None => break,
                                        }
                                    } else {
                                        break;
                                    }
                                }
                                None
                            }
                        })
                    })
                });
                // Typed-self receivers from the async desugar: `self` inside an
                // async method body is rewritten to
                // `((struct Cls *)__jeti_task->self_obj)` — the cast target IS
                // the receiver's static class. Without this arm the send fell
                // through to the any-class-with-this-selector guess, which
                // could pick a DIFFERENT class sharing the selector name
                // (`[self run]` in Probe hitting AsyncA's `- (async int)run`)
                // and cast the call to that class's signature.
                if vtable_class.is_none() {
                    if let AstExprData::Cast { target_type, .. } = &receiver.data {
                        // `((struct Cls *)…)` from the async desugar spells as a
                        // pointer to a struct-tagged Named — the class name sits
                        // on the subtype (`struct_ptr_type`).
                        let cast_cls = target_type.name.as_deref().map(|s| s.to_string())
                            .or_else(|| {
                                target_type.subtype.as_deref().and_then(|s| {
                                    let is_struct_ptr = target_type.is_pointer
                                        && s.is_struct && s.prim == TypePrim::Named;
                                    if is_struct_ptr { s.name.clone() } else { None }
                                })
                            });
                        if let Some(cls) = cast_cls {
                            if let Some(info) = class_infos.get(&name_flat(&cls)) {
                                let mut cur = info.class_name.clone();
                                loop {
                                    let cflat = name_flat(&cur);
                                    if let Some(ci) = class_infos.get(&cflat) {
                                        if let Some(idx) = ci.method_names.iter().position(|n| n == &sel) {
                                            vtable_class = Some(ci.class_name.clone());
                                            effective_is_class = ci.is_class_methods[idx];
                                            break;
                                        }
                                        match ci.super_name.clone() {
                                            Some(sup) => cur = sup,
                                            None => break,
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                // If the receiver is a concrete class variable, use its type.
                if vtable_class.is_none() {
                    if let AstExprData::VarRef { name, .. } = &receiver.data {
                        // A local variable naming a class is a class receiver
                        // (`[NPString string]`); anything else may be a local
                        // object recorded by the Decl arm above — its declared
                        // class is the receiver's static class.
                        let lookup = if class_infos.contains_key(&name_flat(name)) {
                            name.clone()
                        } else {
                            LOCAL_VAR_CLASS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().get(name).cloned()
                                .unwrap_or_else(|| name.clone())
                        };
                        let flat = name_flat(&lookup);
                        if let Some(info) = class_infos.get(&flat) {
                            // Check if this class (or its superclass chain) has the method
                            let mut cur = info.class_name.clone();
                            loop {
                                let cflat = name_flat(&cur);
                                if let Some(ci) = class_infos.get(&cflat) {
                                    if let Some(idx) = ci.method_names.iter().position(|n| n == &sel) {
                                        vtable_class = Some(ci.class_name.clone());
                                        effective_is_class = ci.is_class_methods[idx];
                                        break;
                                    }
                                    match ci.super_name.clone() {
                                        Some(sup) => cur = sup,
                                        None => break,
                                    }
                                } else {
                                    break;
                                }
                            }
                        } else if name.contains('<') {
                            // Specialized class-name receiver (`[Box<NPString *> defaultValue]`):
                            // the specialized clone is inserted into class_infos AFTER
                            // expression conversion, so the lookup above necessarily missed
                            // it. The base template's presence guarantees the clone will
                            // exist (generic_instantiations collects this same spelling via
                            // parse_generic_type_string), so dispatch to the specialized
                            // function directly — its emitted signature carries the
                            // substituted return/param types, while the base function's
                            // stay erased at the `NPObject * /*T*/` sentinel (an ABI
                            // mismatch for non-pointer T).
                            let base = name.split('<').next().unwrap_or(name);
                            if let Some(info) = class_infos.get(&name_flat(base)) {
                                if let Some(idx) = info.method_names.iter().position(|n| n == &sel) {
                                    // Param-keyed spellings (`ChainArray<T>` inside
                                    // ChainBox's body) dispatch through the MARKER
                                    // flat (`ChainArray_T`): a per-spelling symbol
                                    // that closure cloning rewrites to the real
                                    // specialization (`ChainArray_NPString_ptr`) in
                                    // the clone body; template-mode calls land on
                                    // forwarders delegating to the base template.
                                    // The marker string is unique to param-keyed
                                    // sites, so the clone-time rewrite is
                                    // unambiguous (bare erased uses keep the base
                                    // flat; concrete inner instantiations mangle
                                    // their own args).
                                    vtable_class = Some(name.clone());
                                    effective_is_class = info.is_class_methods[idx];
                                }
                            }
                        }
                    }
                }
                if vtable_class.is_none() {
                    vtable_class = None;
                    for (_, info) in class_infos.iter() {
                        if let Some(idx) = info.method_names.iter().position(|n| n == &sel) {
                            let cls_name = info.class_name.clone();
                            if vtable_class.is_none() {
                                vtable_class = Some(cls_name);
                                effective_is_class = info.is_class_methods[idx];
                            } else {
                                alt_vtable_classes.push(cls_name);
                            }
                        }
                    }
                }
            }

            if effective_is_class {
                if let Some(ref _vc) = vtable_class {
                    let receiver_class = match &receiver.data {
                        AstExprData::VarRef { name, .. } => Some(name.clone()),
                        _ => None,
                    };
                    if let Some(rc) = receiver_class {
                        // For self/super class method calls, pass self directly (it's already a class pointer)
                        // NOTE: a super class-method send (`[super alloc]`) must pass
                        // `self`, NOT the identifier `super` (which is not a C
                        // identifier and only makes sense to the jeti parser).
                        let cls_addr = if rc == "self" || (rc == "super" && *is_super) {
                            // `super` as receiver → pass `self` (in a class method
                            // self IS the NPClass*; `super` is not a C identifier).
                            let recv_ident = if rc == "super" { "self" } else { rc.as_str() };
                            CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line, col,
                                data: CgExprData::Ident(recv_ident.into()),
                            }
                        } else {
                            // Resolve short name to fully-qualified class name via class_infos
                            let fq_rc: &str = {
                                let flat_rc = name_flat(&rc);
                                let mut resolved: Option<&str> = None;
                                if let Some(info) = class_infos.get(&flat_rc) {
                                    resolved = Some(info.class_name.as_str());
                                } else if rc.contains('<') {
                                    // Param-keyed spelling (`ChainArray<T>` inside
                                    // ChainBox's body): the cloner skips param-keyed
                                    // demands, so no `ChainArray_T` clone exists —
                                    // the class address must reference the base
                                    // template (erasure semantics).
                                    let base = rc.split('<').next().unwrap_or(&rc);
                                    if let Some(info) = class_infos.get(&name_flat(base)) {
                                        if spelling_args_are_template_params(&rc, &info.type_params) {
                                            resolved = Some(info.class_name.as_str());
                                        }
                                    }
                                }
                                if resolved.is_none() {
                                    let search_suffix = format!("::{}", rc);
                                    for (_, info) in class_infos.iter() {
                                        if info.class_name == rc
                                            || info.class_name.ends_with(&search_suffix)
                                            || info.flat == rc
                                            || info.flat.ends_with(&flat_rc)
                                        {
                                            resolved = Some(info.class_name.as_str());
                                            break;
                                        }
                                    }
                                }
                                resolved.unwrap_or(rc.as_str())
                            };
                            CgExpr {
                                kind: CgExprKind::Unary, type_str: None, line, col,
                                data: CgExprData::Unary {
                                    op_str: "&".into(),
                                    operand: Box::new(CgExpr {
                                        kind: CgExprKind::Ident, type_str: None, line, col,
                                        data: CgExprData::Ident(meta_symbol("CLASS_", &name_flat(fq_rc))),
                                    }),
                                    is_postfix: false,
                                },
                            }
                        };
                        call_args.push(cls_addr);
                    } else {
                        // Class method on expression receiver (e.g. [[self class] alloc]):
                        // receiver expression evaluates to an NPClass*, pass it directly
                        call_args.push(convert_expr(receiver, &class_infos));
                    }
                } else {
                    // Class method on runtime-determined receiver (e.g. [[self class] alloc]):
                    // receiver itself is the class pointer, pass it directly
                    call_args.push(convert_expr(receiver, &class_infos));
                }
            } else {
                // Instance method: receiver is self for super calls
                let receiver_expr = if *is_super {
                    CgExpr { kind: CgExprKind::Ident, type_str: None, line: 0, col: 0, data: CgExprData::Ident("self".into()) }
                } else {
                    convert_expr(receiver, &class_infos)
                };
                call_args.push(receiver_expr);
            }
            for a in args {
                call_args.push(convert_expr(a, &class_infos));
            }

            let sel_const = sel_const_name(selector);

            // True monomorphization (C++-template semantics): when the
            // receiver's STATIC type is a generic instantiation (`Box<int> *`),
            // the call goes straight to the monomorphized clone's function —
            // the base template's vtable slot carries the erased
            // `NPObject * /*T*/` ABI, which cannot carry a value-type `T`.
            // The clone is emitted with the substituted signature
            // (`Freestanding__Box_int_value` → `int`), so the static call is
            // both type-correct and ABI-correct. Static dispatch means the
            // runtime never looks at isa here — exactly what "真泛型" buys.
            //
            // The flag mirrors the resolve chain above: vtable_class carrying
            // the MANGLED spelling (`FsBox<NPMutableString *>`) is the marker
            // that the resolve chose the clone — and only because the
            // template itself implements the method there. Class-name
            // receivers with a literal specialization
            // (`[Box<int> defaultValue]`) keep their own direct path via the
            // class-method arm.
            let direct_specialized = !*is_super
                && vtable_class.as_deref().map_or(false, |vc| vc.contains('<'));

            CgExpr {
                kind: CgExprKind::Call, type_str, line, col,
                data: CgExprData::Call {
                    name: sanitize_sel_name(selector),
                    args: call_args,
                    vtable_class,
                    alt_vtable_classes,
                    is_class_method: effective_is_class,
                    is_super: *is_super,
                    sel_const_name: Some(sel_const), method_index: None, direct_specialized,
                },
            }
        }
        AstExprData::FuncCall { name, args, callee, .. } => {
            // NPLog(@"...%@...", arg1, arg2): resolve `%@` at COMPILE TIME per
            // the jeti static-dispatch model. No runtime reflection is allowed.
            // Each `%@` arg becomes  arg ? [[arg description] UTF8String] : "(null)"
            // and the format's `%@` is rewritten to `%s`.
            if name == "NPLog" && callee.is_none() && !args.is_empty() {
                if let Some(fmt_str) = match &args[0].data {
                    AstExprData::AtString(s) => Some(s.clone()),
                    AstExprData::String(s) => Some(s.clone()),
                    _ => None,
                } {
                    if fmt_str.contains("%@") {
                        return expand_nplog_format(&fmt_str, &args[1..], class_infos, line, col, type_str);
                    } else {
                        // No %@ — the format is passed as an NPString.
                        let has_npstring = class_infos.contains_key("NPString");
                        let mut cg_args = vec![nplog_format_arg(fmt_str, has_npstring, line, col)];
                        for a in &args[1..] {
                            cg_args.push(convert_expr(a, class_infos));
                        }
                        return CgExpr {
                            kind: CgExprKind::Call, type_str, line, col,
                            data: CgExprData::Call {
                                name: "NPLog".into(), args: cg_args,
                                vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                                sel_const_name: None, method_index: None, direct_specialized: false,
                            },
                        };
                    }
                }
            }
            // Immediately-invoked block literal (IIFE): `^{ … }(args)` /
            // `^int(void){ … }(3)`. The callee IS the block — convert it (this
            // also registers the gcc invoke static into the block_defs buffer)
            // and call it in place, instead of rendering an empty callee name.
            if let Some(ref ce) = callee {
                // `(block)(args)` — a parenthesized block callee is wrapped in
                // Paren; unwrap it so the parenthesized IIFE takes this arm.
                // Without this the callee renders as an empty function name
                // and the call degrades to `(args)` (probe-verified silent
                // miscompile with args, `()` with none).
                let blk_src = match &ce.data {
                    AstExprData::Block { .. } => Some(&**ce),
                    AstExprData::Paren(inner) => match &inner.data {
                        AstExprData::Block { .. } => Some(&**inner),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(blk_src) = blk_src {
                    let blk = convert_expr(blk_src, class_infos);
                    let lit = match blk.data {
                        CgExprData::BlockLit(d) => d,
                        _ => unreachable!("block callee must convert to BlockLit"),
                    };
                    let call_name = if is_clang_backend() {
                        // clang: emit the literal inline and invoke it —
                        // `^rt(params){ … }(args)` is valid blocks syntax.
                        let mut s = String::new();
                        s.push('^');
                        s.push_str(&lit.return_type);
                        s.push('(');
                        for (i, (pt, pn)) in lit.params.iter().enumerate() {
                            if i > 0 { s.push_str(", "); }
                            s.push_str(pt); s.push(' '); s.push_str(pn);
                        }
                        s.push(')');
                        if let Some(ref b) = lit.body {
                            s.push(' ');
                            emit_stmt_inline(b, &mut s);
                        } else {
                            s.push_str(" {}");
                        }
                        s
                    } else {
                        // gcc/portable: call the invoke static directly; plain
                        // blocks carry no captures, so a null cself is fine.
                        lit.func_name.clone()
                    };
                    let mut cg_args: Vec<CgExpr> = args.iter().map(|a| convert_expr(a, class_infos)).collect();
                    if !is_clang_backend() {
                        cg_args.insert(0, CgExpr { kind: CgExprKind::Int, type_str: None, line, col, data: CgExprData::Int(0) });
                    }
                    return CgExpr {
                        kind: CgExprKind::Call, type_str, line, col,
                        data: CgExprData::Call {
                            name: call_name, args: cg_args,
                            vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                            sel_const_name: None, method_index: None, direct_specialized: false,
                        },
                    };
                }
            }
            // When `callee` is Some (block invocation on ivar), use it as
            // the call target instead of a bare function name.
            let call_name = if let Some(ref ce) = callee {
                render_callee_expr(ce)
            } else {
                // Block variable call (e.g. `b(3,4)` where `b` is a block).
                // For gcc/portable: emit `((RT(*)(void*,...))b->invoke)(b, args...)`.
                if !is_clang_backend() && is_block_var(name) {
                    let rt = type_str.as_ref().map(|t| t.as_str()).unwrap_or("void");
                    let invoke_name = format!("(({} (*)(void *, ...)){}->invoke)", rt, name);
                    let mut cg_args: Vec<CgExpr> = args.iter().map(|a| convert_expr(a, &class_infos)).collect();
                    cg_args.insert(0, CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident(name.clone()) });
                    return CgExpr {
                        kind: CgExprKind::Call, type_str, line, col,
                        data: CgExprData::Call {
                            name: invoke_name, args: cg_args,
                            vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                            sel_const_name: None, method_index: None, direct_specialized: false,
                        },
                    };
                }
                name.clone()
            };
            let mut auto_sel = None;
            let mut auto_is_class = false;
            if callee.is_none() {
                // Detect direct calls to NPObject method functions like
                // `NPObject_release(obj)`, `NPObject_retain(obj)`, `NPObject_dealloc(obj)`.
                // Convert these to vtable dispatch so the normal vtable path handles
                // arg casting (e.g. (NPObject *)(child)) instead of a bare C call.
                if let Some(method) = call_name.strip_prefix("NPObject_") {
                    if !method.is_empty() && args.len() == 1 {
                        auto_sel = Some(sel_const_name(method));
                        // Determine if class method (alloc/new) or instance method.
                        auto_is_class = class_infos.get("NPObject")
                            .map(|info| {
                                info.method_names.iter()
                                    .zip(info.is_class_methods.iter())
                                    .any(|(nm, is_cm)| nm == method && *is_cm)
                            })
                            .unwrap_or(false);
                    }
                }
            }
            let mut cg_args: Vec<CgExpr> = args.iter().map(|a| convert_expr(a, &class_infos)).collect();
            // Plain C call: cast args per the registered declared signature
            // (implicit id↔class joins), unless this call turned into a
            // vtable dispatch (which casts args itself) or a block invoke.
            if auto_sel.is_none() && callee.is_none() {
                coerce_call_args_by_sig(&call_name, &mut cg_args);
            }
            if let Some(sel) = auto_sel {
                // Convert NPObject_* calls to vtable dispatch by setting vtable_class,
                // sel_const_name, and using the method name (without NPObject_ prefix).
                let method = call_name.strip_prefix("NPObject_").unwrap();
                CgExpr {
                    kind: CgExprKind::Call, type_str, line, col,
                    data: CgExprData::Call {
                        name: method.to_string(),
                        args: cg_args, // no SEL — emitted from sel_const_name
                        vtable_class: Some("NPObject".to_string()),
                        alt_vtable_classes: vec![],
                        is_class_method: auto_is_class,
                        is_super: false,
                        sel_const_name: Some(sel),
                        method_index: None, direct_specialized: false,
                    },
                }
            } else {
                CgExpr {
                    kind: CgExprKind::Call, type_str, line, col,
                    data: CgExprData::Call {
                        name: call_name, args: cg_args,
                        vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }
            }
        }
        AstExprData::Unary { op, operand, is_postfix } => {
            let op_str = match op {
                1 => "++", 2 => "--", 3 => "*", 4 => "&", 5 => "-", 6 => "+", 7 => "~", 8 => "!",
                _ => "?",
            };
            CgExpr { kind: CgExprKind::Unary, type_str, line, col, data: CgExprData::Unary { op_str: op_str.into(), operand: Box::new(convert_expr(operand, &class_infos)), is_postfix: *is_postfix } }
        }
        AstExprData::Binary { op, left, right } => {
            if (100..=109).contains(op) {
                // Compound assignment (`+=`, `-=`, etc.) on ObjC property:
                // obj.prop += value  →  [obj setProp:([obj prop] + value)]
                if let AstExprData::PropRef { obj, name, prop, cls, .. } = &left.data {
                    if prop.is_some() {
                        let regular_op = match *op {
                            100 => 4, 101 => 5, 102 => 1, 103 => 2, 104 => 3,
                            105 => 14, 106 => 16, 107 => 15, 108 => 6, 109 => 7,
                            _ => 4,
                        };
                        let regular_op_str = op_to_str(regular_op, false);
                        let obj_cg = convert_expr(obj, &class_infos);
                        let getter_cg = convert_expr(left, &class_infos);
                        let value_cg = convert_expr(right, &class_infos);
                        let sum_cg = CgExpr {
                            kind: CgExprKind::Binary, type_str: None, line, col,
                            data: CgExprData::Binary {
                                op_str: regular_op_str.into(),
                                left: Box::new(getter_cg),
                                right: Box::new(value_cg),
                            },
                        };
                        let setter_sel = format!("set{}{}:", &name[..1].to_uppercase(), &name[1..]);
                        let vtable_class = cls.clone();
                        let sel_const = sel_const_name(&setter_sel);
                        CgExpr {
                            kind: CgExprKind::Call, type_str, line, col,
                            data: CgExprData::Call {
                                name: setter_sel.replace(':', "_"),
                                args: vec![obj_cg, sum_cg],
                                vtable_class,
                                alt_vtable_classes: vec![],
                                is_class_method: false,
                                is_super: false,
                                sel_const_name: Some(sel_const), method_index: None, direct_specialized: false,
                            }
                        }
                    } else {
                        let op_str = op_to_str(*op, false);
                        CgExpr { kind: CgExprKind::Binary, type_str, line, col, data: CgExprData::Binary { op_str: op_str.into(), left: Box::new(convert_expr(left, &class_infos)), right: Box::new(convert_expr(right, &class_infos)) } }
                    }
                } else {
                    let op_str = op_to_str(*op, false);
                    CgExpr { kind: CgExprKind::Binary, type_str, line, col, data: CgExprData::Binary { op_str: op_str.into(), left: Box::new(convert_expr(left, &class_infos)), right: Box::new(convert_expr(right, &class_infos)) } }
                }
            } else {
                let op_str = op_to_str(*op, false);
                CgExpr { kind: CgExprKind::Binary, type_str, line, col, data: CgExprData::Binary { op_str: op_str.into(), left: Box::new(convert_expr(left, &class_infos)), right: Box::new(convert_expr(right, &class_infos)) } }
            }
        }
        AstExprData::Assign { target, value } => {
            if let AstExprData::PropRef { obj, name, is_arrow, prop, cls, .. } = &target.data {
                // Only prepend `_` for ObjC property access (prop is Some). For plain
                // C struct field access (`struct.field = val`), use the name as-is.
                let field_name = if prop.is_some() { format!("_{}", name) } else { name.clone() };
                let obj_cg = convert_expr(obj, &class_infos);
                // The ivar's declared type (the `cls` @interface records it)
                // rides on the target so emit-side assignment casts can spell
                // the implicit id↔class-pointer join for the value.
                let ivar_type = if prop.is_some() {
                    cls.as_ref().and_then(|c| class_infos.get(c)).and_then(|info| {
                        info.ivar_names.iter().position(|n| n == &field_name)
                            .map(|idx| info.ivar_types[idx].clone())
                    })
                } else {
                    None
                };
                let arrow_type = ivar_type.or_else(|| obj_cg.type_str.clone());
                let target_cg = if *is_arrow || obj.expr_type.as_ref().map_or(false, |t| t.is_pointer) {
                    CgExpr { kind: CgExprKind::Arrow, type_str: arrow_type, line, col,
                             data: CgExprData::Arrow { obj: Box::new(obj_cg), field: field_name.clone() } }
                } else {
                    CgExpr { kind: CgExprKind::Member, type_str: arrow_type, line, col,
                             data: CgExprData::Member { obj: Box::new(obj_cg), field: field_name.clone() } }
                };
                let value_cg = convert_expr(value, &class_infos);
                // ObjC dot syntax is a setter call, not a raw field write. A weak
                // property has to be registered against the object it points at,
                // or the slot is never nil'd when that object dies — leaving a
                // dangling pointer where the language promises nil. (Reads
                // already dispatch through the getter; this makes writes
                // symmetric instead of silently bypassing the setter.)
                let weak_prop = prop.is_some()
                    && cls.as_ref()
                        .and_then(|c| class_infos.get(c))
                        .map(|info| {
                            info.ivar_names.iter().position(|n| n == &field_name)
                                .and_then(|idx| info.ivar_weak.get(idx).copied())
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                if weak_prop {
                    build_weak_write(target_cg, value_cg, line, col, type_str)
                } else {
                    CgExpr {
                        kind: CgExprKind::Assign, type_str, line, col,
                        data: CgExprData::Assign {
                            target: Box::new(target_cg),
                            value: Box::new(value_cg),
                        },
                    }
                }
            } else if let AstExprData::IvarRef { ivar: ivar_name, cls: ivar_cls, .. } = &target.data {
                let is_weak = ivar_cls.as_ref().and_then(|c| {
                    class_infos.get(c).map(|info| {
                        ivar_name.as_ref().and_then(|ivn| {
                            info.ivar_names.iter().position(|n| n == ivn)
                                .and_then(|idx| info.ivar_weak.get(idx).copied())
                        }).unwrap_or(false)
                    })
                }).unwrap_or(false);
                if is_weak {
                    let ivar_cg = convert_expr(target, &class_infos);
                    let value_cg = convert_expr(value, &class_infos);
                    build_weak_write(ivar_cg, value_cg, line, col, type_str)
                } else {
                    CgExpr { kind: CgExprKind::Assign, type_str, line, col, data: CgExprData::Assign { target: Box::new(convert_expr(target, &class_infos)), value: Box::new(convert_expr(value, &class_infos)) } }
                }
            } else {
                CgExpr { kind: CgExprKind::Assign, type_str, line, col, data: CgExprData::Assign { target: Box::new(convert_expr(target, &class_infos)), value: Box::new(convert_expr(value, &class_infos)) } }
            }
        }
        AstExprData::Cast { target_type, expr } => {
            let ct = ast_type_to_c_str(target_type);
            CgExpr { kind: CgExprKind::Cast, type_str, line, col, data: CgExprData::Cast { target_type: ct, expr: Box::new(convert_expr(expr, &class_infos)) } }
        }
        AstExprData::Comma(exprs) => {
            CgExpr { kind: CgExprKind::Comma, type_str, line, col, data: CgExprData::Comma(exprs.iter().map(|e| convert_expr(e, &class_infos)).collect()) }
        }
        AstExprData::Paren(e) => {
            CgExpr { kind: CgExprKind::Paren, type_str, line, col, data: CgExprData::Paren(Box::new(convert_expr(e, &class_infos))) }
        }
        AstExprData::Subscript { object, key } => {
            // If the object is a PropRef, use ivar access directly instead of getter call
            if let AstExprData::PropRef { obj, name, is_arrow, prop, cls, .. } = &object.data {
                // Only prepend `_` for ObjC property access (prop is Some).
                let field_name = if prop.is_some() { format!("_{}", name) } else { name.clone() };
                let obj_cg = convert_expr(obj, &class_infos);
                let key_cg = convert_expr(key, &class_infos);
                // Inline cast self to avoid stale _self when self is reassigned
                let arr_obj = if let CgExprData::Ident(ref name) = obj_cg.data {
                    if name == "self" {
                        if let Some(ref cls_name) = cls {
                            let flat = name_flat(cls_name);
                            Box::new(CgExpr {
                                kind: CgExprKind::Cast, type_str: None, line, col,
                                data: CgExprData::Cast {
                                    target_type: format!("struct {} *", flat),
                                    expr: Box::new(CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident("self".into()) }),
                                },
                            })
                        } else {
                            Box::new(obj_cg)
                        }
                    } else {
                        Box::new(obj_cg)
                    }
                } else {
                    Box::new(obj_cg)
                };
                // Respect `.`/`->` from source: non-ObjC struct field access emits `.`
                // (Member) rather than `->` (Arrow).
                let field_cg = if *is_arrow || obj.expr_type.as_ref().map_or(false, |t| t.is_pointer) {
                    CgExpr { kind: CgExprKind::Arrow, type_str: None, line, col,
                             data: CgExprData::Arrow { obj: arr_obj, field: field_name.clone() } }
                } else {
                    CgExpr { kind: CgExprKind::Member, type_str: None, line, col,
                             data: CgExprData::Member { obj: arr_obj, field: field_name } }
                };
                CgExpr { kind: CgExprKind::Index, type_str, line, col,
                    data: CgExprData::Index {
                        arr: Box::new(field_cg),
                        index: Box::new(key_cg),
                    },
                }
            } else {
                CgExpr { kind: CgExprKind::Index, type_str, line, col, data: CgExprData::Index { arr: Box::new(convert_expr(object, &class_infos)), index: Box::new(convert_expr(key, &class_infos)) } }
            }
        }
        AstExprData::Ternary { cond, then, else_ } => {
            CgExpr { kind: CgExprKind::Ternary, type_str, line, col, data: CgExprData::Ternary { cond: Box::new(convert_expr(cond, &class_infos)), then: Box::new(convert_expr(then, &class_infos)), else_: Box::new(convert_expr(else_, &class_infos)) } }
        }
        AstExprData::InitList(elements) => {
            CgExpr { kind: CgExprKind::InitList, type_str, line, col, data: CgExprData::InitList(elements.iter().map(|e| convert_expr(e, &class_infos)).collect()) }
        }
        AstExprData::DesignatedInit { designators, expr } => {
            let conv_d = |d: &AstDesignator| -> CgDesignator {
                match d {
                    AstDesignator::Member(n) => CgDesignator::Member(n.clone()),
                    AstDesignator::Index(ix) => CgDesignator::Index(Box::new(convert_expr(ix, &class_infos))),
                }
            };
            CgExpr { kind: CgExprKind::DesignatedInit, type_str, line, col, data: CgExprData::DesignatedInit { designators: designators.iter().map(conv_d).collect(), expr: Box::new(convert_expr(expr, &class_infos)) } }
        }
        AstExprData::ArrayLit(elements) => {
            if class_infos.contains_key("NPArray") {
                // @[a, b, c] → jeti_array_create(3, a, b, c)
                let mut cg_args = Vec::with_capacity(elements.len() + 1);
                cg_args.push(CgExpr { kind: CgExprKind::Int, type_str: None, line, col, data: CgExprData::Int(elements.len() as i64) });
                for el in elements {
                    cg_args.push(convert_expr(el, &class_infos));
                }
                CgExpr { kind: CgExprKind::Call, type_str: Some("NPObject *".into()), line, col,
                    data: CgExprData::Call {
                        name: "jeti_array_create".into(), args: cg_args,
                        vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }
            } else {
                CgExpr { kind: CgExprKind::InitList, type_str, line, col, data: CgExprData::InitList(elements.iter().map(|e| convert_expr(e, &class_infos)).collect()) }
            }
        }
        AstExprData::Selector(s) => {
            CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident(sel_const_name(s)) }
        }
        AstExprData::DictLit { keys, values } => {
            if class_infos.contains_key("NPDictionary") {
                // @{k: v, ...} → jeti_dictionary_create(n, k1, v1, ..., kn, vn)
                let stored = keys.len().min(values.len());
                let mut cg_args = Vec::with_capacity(stored * 2 + 1);
                cg_args.push(CgExpr { kind: CgExprKind::Int, type_str: None, line, col, data: CgExprData::Int(stored as i64) });
                for i in 0..stored {
                    cg_args.push(convert_expr(&keys[i], &class_infos));
                    cg_args.push(convert_expr(&values[i], &class_infos));
                }
                CgExpr { kind: CgExprKind::Call, type_str: Some("NPObject *".into()), line, col,
                    data: CgExprData::Call {
                        name: "jeti_dictionary_create".into(), args: cg_args,
                        vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }
            } else {
                // No NPDictionary in this TU (Foundation not imported): keep the
                // historical `NULL` placeholder rather than emit a call to a
                // helper that was never emitted.
                CgExpr { kind: CgExprKind::Ident, type_str, line, col, data: CgExprData::Ident("NULL".into()) }
            }
        }
        AstExprData::PropRef { obj, name, is_arrow, prop, cls, .. } => {
            // ObjC property access (prop is Some, cls is Some): dispatch via vtable getter.
            // Everything else (non-ObjC struct field access like `raw.c_lflag`, or
            // ivar access through a non-self object) is plain C field access —
            // emit `.` (Member) or `->` (Arrow) per `is_arrow`, NOT a vtable call.
            if prop.is_some() && cls.is_some() {
                let sel = name.replace(':', "_");
                let recv_cg = convert_expr(obj, &class_infos);

                let mut vtable_class = None;
                for (_, info) in class_infos.iter() {
                    if info.method_names.iter().position(|n| n == &sanitize_sel_name(&sel)).is_some() {
                        vtable_class = Some(info.class_name.clone());
                        break;
                    }
                }

                let sel_const = sel_const_name(&name);

                CgExpr {
                    kind: CgExprKind::Call, type_str, line, col,
                    data: CgExprData::Call {
                        name: sel, args: vec![recv_cg],
                        vtable_class, alt_vtable_classes: vec![], is_class_method: false, is_super: false,
                        sel_const_name: Some(sel_const), method_index: None, direct_specialized: false,
                    },
                }
            } else {
                // Plain C struct/union field access — respect `.`/`->` from source.
                // If the object's type is a pointer (the checker resolved it), an
                // ObjC instance is always a pointer — force `->` regardless of
                // how the elaborator set is_arrow.
                let recv_cg = convert_expr(obj, &class_infos);
                let obj_is_ptr = obj.expr_type.as_ref().map_or(false, |t| t.is_pointer);
                if *is_arrow || obj_is_ptr {
                    CgExpr { kind: CgExprKind::Arrow, type_str, line, col,
                             data: CgExprData::Arrow { obj: Box::new(recv_cg), field: name.clone() } }
                } else {
                    CgExpr { kind: CgExprKind::Member, type_str, line, col,
                             data: CgExprData::Member { obj: Box::new(recv_cg), field: name.clone() } }
                }
            }
        }
        AstExprData::Sizeof { type_expr, expr } => {
            if let Some(e) = expr {
                CgExpr { kind: CgExprKind::Unary, type_str, line, col, data: CgExprData::Unary { op_str: "sizeof".into(), operand: Box::new(convert_expr(e, &class_infos)), is_postfix: false } }
            } else {
                let type_str_val = ast_type_to_c_str(&type_expr);
                CgExpr { kind: CgExprKind::Sizeof, type_str, line, col, data: CgExprData::Sizeof { type_str: type_str_val, is_alignof: false } }
            }
        }
        AstExprData::Alignof(ty) => {
            let type_str_val = ast_type_to_c_str(&ty);
            CgExpr { kind: CgExprKind::Sizeof, type_str, line, col, data: CgExprData::Sizeof { type_str: type_str_val, is_alignof: true } }
        }
        AstExprData::TypeLiteral(ty) => {
            CgExpr { kind: CgExprKind::TypeLiteral, type_str, line, col, data: CgExprData::TypeLiteral(ast_type_to_c_str(&ty)) }
        }
        // `is_async` ignored at codegen for now: the async desugar pass (§8-3)
        // owns async block lowering; plain block emission is the fallback path.
        AstExprData::Block { params, return_type, body, .. } => {
            let tid = next_temp_id();
            let func_name = format!("__jeti_block_{}", tid);
            let rt = return_type.as_ref().map(|t| ast_type_to_c_str(t))
                .unwrap_or_else(|| infer_block_return_type(body.as_deref()));
            let mut cg_params = Vec::new();
            for (pt, pn) in params {
                let pt_str = ast_type_to_c_str(pt);
                let pn_str = if pn.is_empty() { "_arg".into() } else { pn.clone() };
                cg_params.push((pt_str, pn_str));
            }
            let cg_body = body.as_ref().map(|b| Box::new(convert_stmt(b, &class_infos)));
            // For gcc/portable, pre-generate the struct+invoke definitions into the
            // module-level buffer so they are available when emit_unit_with_headers runs.
            if !is_clang_backend() {
                let mut defs = block_defs();
                let layout_name = format!("__jeti_block_layout_{}", tid);
                let mut params_sig = String::new();
                for (i, (pt, pn)) in cg_params.iter().enumerate() {
                    if i > 0 { params_sig.push_str(", "); }
                    params_sig.push_str(pt); params_sig.push(' '); params_sig.push_str(pn);
                }
                // Struct definition. A zero-param invoke is `(struct L *)` —
                // appending `, void` after a real parameter is illegal C.
                let invoke_params = if params_sig.is_empty() {
                    format!("struct {} *", layout_name)
                } else {
                    format!("struct {} *, {}", layout_name, params_sig)
                };
                defs.push_str(&format!("struct {} {{\n    void *isa;\n    int flags;\n    int reserved;\n    {} (*invoke)({});\n}};\n",
                    layout_name, rt, invoke_params));
                // Invoke function (emit body now since we have the CgStmt)
                // NOTE: no `{` here — the body's Compound statement emits its own braces.
                defs.push_str(&format!("static {} {}(struct {} *cself{}) ", rt, func_name, layout_name,
                    if params_sig.is_empty() { String::new() } else { format!(", {}", params_sig) }));
                if let Some(ref body) = cg_body {
                    // Emit body to a temporary string and append to defs
                    let mut body_out = String::new();
                    emit_stmt(body, &mut body_out, 0);
                    defs.push_str(&body_out);
                } else {
                    defs.push_str("{}");
                }
                defs.push_str("\n\n");
            }
            CgExpr {
                kind: CgExprKind::BlockLit, type_str, line, col,
                data: CgExprData::BlockLit(BlockLiteralData {
                    return_type: rt,
                    params: cg_params,
                    body: cg_body,
                    func_name,
                }),
            }
        }
    }
}

/// Infer a block literal's return type from its body when no explicit
/// `^T(...)` return type was written. Uses the checker-annotated `expr_type`
/// on each `return e;` expression; falls back to literal classification
/// (int / float / char*) when unchecked. Nested block literals are skipped —
/// a `return` inside an inner block belongs to that block, not this one.
fn infer_block_return_type(body: Option<&AstStmt>) -> String {
    let mut found: Option<String> = None;
    fn scan(stmt: &AstStmt, found: &mut Option<String>) {
        if found.is_some() { return; }
        match &stmt.data {
            AstStmtData::Return(Some(e)) => {
                let t = e.expr_type.as_ref().map(|t| ast_type_to_c_str(t))
                    .unwrap_or_else(|| literal_type_str(e));
                *found = Some(t);
            }
            AstStmtData::Compound(stmts) => {
                for s in stmts { scan(s, found); if found.is_some() { return; } }
            }
            AstStmtData::Autoreleasepool(body) => scan(body, found),
            AstStmtData::If { then, else_, .. } => {
                scan(then, found);
                if let (None, Some(e)) = (found.as_ref(), else_) { scan(e, found); }
            }
            AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
            | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. }
            | AstStmtData::Synchronized { body, .. } | AstStmtData::NoArc(body)
            | AstStmtData::Default(body) => scan(body, found),
            AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. } => scan(body, found),
            AstStmtData::Try { try_block, catches, finally_block } => {
                scan(try_block, found);
                if found.is_some() { return; }
                for c in catches {
                    if let AstStmtData::Catch { body, .. } = &c.data { scan(body, found); }
                    if found.is_some() { return; }
                }
                if let Some(f) = finally_block { scan(f, found); }
            }
            // Note: block literals and nested stmt bodies of Decl are skipped —
            // a return inside an inner block literal belongs to that block.
            _ => {}
        }
    }
    // Literal-classification fallback for unchecked expressions. Recurses
    // through parens, casts, and binary/unary operands so `return (a * b);`
    // (a Binary node with no checker expr_type) still classifies as int.
    fn literal_type_str(e: &AstExpr) -> String {
        match &e.data {
            AstExprData::Int(_) | AstExprData::Bool(_) | AstExprData::Char(_) => "int".into(),
            AstExprData::Float(_) | AstExprData::FloatRaw(_) => "double".into(),
            AstExprData::String(_) | AstExprData::AtString(_) => "char *".into(),
            AstExprData::Paren(inner) | AstExprData::Unary { operand: inner, .. } => literal_type_str(inner),
            AstExprData::Cast { expr: inner, .. } => literal_type_str(inner),
            AstExprData::Binary { left, right, .. } => {
                let lt = literal_type_str(left);
                if lt != "void" { lt } else { literal_type_str(right) }
            }
            _ => "void".into(),
        }
    }
    if let Some(b) = body { scan(b, &mut found); }
    found.unwrap_or_else(|| "void".into())
}

/// Whether `expr` references the identifier `name` (transitively).
/// Used to decide if a @catch parameter variable is used by the catch body,
/// so we can avoid emitting a dead-initialized declaration when it isn't.
fn expr_refs_name(expr: &AstExpr, name: &str) -> bool {
    match &expr.data {
        AstExprData::VarRef { name: n, .. } => n == name,
        AstExprData::IvarRef { obj, .. } => expr_refs_name(obj, name),
        AstExprData::PropRef { obj, .. } => expr_refs_name(obj, name),
        AstExprData::MsgSend { receiver, args, .. } => {
            expr_refs_name(receiver, name) || args.iter().any(|a| expr_refs_name(a, name))
        }
        AstExprData::FuncCall { callee, args, .. } => {
            callee.as_ref().map_or(false, |c| expr_refs_name(c, name)) || args.iter().any(|a| expr_refs_name(a, name))
        }
        AstExprData::Unary { operand, .. } => expr_refs_name(operand, name),
        AstExprData::Binary { left, right, .. } => expr_refs_name(left, name) || expr_refs_name(right, name),
        AstExprData::Assign { target, value } => expr_refs_name(target, name) || expr_refs_name(value, name),
        AstExprData::Cast { expr, .. } => expr_refs_name(expr, name),
        AstExprData::ArrayLit(items) => items.iter().any(|i| expr_refs_name(i, name)),
        AstExprData::InitList(items) => items.iter().any(|i| expr_refs_name(i, name)),
        AstExprData::DictLit { keys, values } => keys.iter().chain(values.iter()).any(|i| expr_refs_name(i, name)),
        AstExprData::Comma(items) => items.iter().any(|i| expr_refs_name(i, name)),
        AstExprData::Subscript { object, key } => expr_refs_name(object, name) || expr_refs_name(key, name),
        AstExprData::Sizeof { expr, .. } => expr.as_ref().map_or(false, |e| expr_refs_name(e, name)),
        AstExprData::Block { params, body, .. } => {
            params.iter().any(|(_, p)| p == name) || body.as_ref().map_or(false, |b| stmt_refs_name(b, name))
        }
        AstExprData::Ternary { cond, then, else_ } => {
            expr_refs_name(cond, name) || expr_refs_name(then, name) || expr_refs_name(else_, name)
        }
        _ => false,
    }
}

/// Whether `stmt` references the identifier `name` (transitively).
fn stmt_refs_name(stmt: &AstStmt, name: &str) -> bool {
    match &stmt.data {
        AstStmtData::Expr(e) => expr_refs_name(e, name),
        AstStmtData::Compound(stmts) => stmts.iter().any(|s| stmt_refs_name(s, name)),
        AstStmtData::If { cond, then, else_ } => {
            expr_refs_name(cond, name)
                || stmt_refs_name(then, name)
                || else_.as_ref().map_or(false, |e| stmt_refs_name(e, name))
        }
        AstStmtData::Switch { expr, body } => expr_refs_name(expr, name) || stmt_refs_name(body, name),
        AstStmtData::Case { value, body } => expr_refs_name(value, name) || stmt_refs_name(body, name),
        AstStmtData::Default(s) => stmt_refs_name(s, name),
        AstStmtData::While { cond, body } => expr_refs_name(cond, name) || stmt_refs_name(body, name),
        AstStmtData::Do { body, cond } => stmt_refs_name(body, name) || expr_refs_name(cond, name),
        AstStmtData::For { init, cond, incr, body } => {
            init.as_ref().map_or(false, |s| stmt_refs_name(s, name))
                || cond.as_ref().map_or(false, |e| expr_refs_name(e, name))
                || incr.as_ref().map_or(false, |e| expr_refs_name(e, name))
                || stmt_refs_name(body, name)
        }
        AstStmtData::ForIn { var, collection, body } => {
            expr_refs_name(var, name) || expr_refs_name(collection, name) || stmt_refs_name(body, name)
        }
        AstStmtData::Return(Some(e)) => expr_refs_name(e, name),
        AstStmtData::Throw(Some(e)) => expr_refs_name(e, name),
        AstStmtData::Try { try_block, catches, finally_block } => {
            stmt_refs_name(try_block, name)
                || catches.iter().any(|c| stmt_refs_name(c, name))
                || finally_block.as_ref().map_or(false, |f| stmt_refs_name(f, name))
        }
        AstStmtData::Catch { body, .. } => stmt_refs_name(body, name),
        AstStmtData::Finally(b) => stmt_refs_name(b, name),
        AstStmtData::Synchronized { lock, body } => expr_refs_name(lock, name) || stmt_refs_name(body, name),
        AstStmtData::Autoreleasepool(b) => stmt_refs_name(b, name),
        AstStmtData::NoArc(b) => stmt_refs_name(b, name),
        AstStmtData::Decl(decl) => decl_refs_name(decl, name),
        _ => false,
    }
}

/// Whether a declaration (or its initializer/next chain) references `name`.
fn decl_refs_name(decl: &AstDecl, name: &str) -> bool {
    match &decl.data {
        AstDeclData::Variable { init, next, .. } => {
            init.as_ref().map_or(false, |e| expr_refs_name(e, name)) || next.as_ref().map_or(false, |n| decl_refs_name(n, name))
        }
        AstDeclData::Method { body, .. } => body.as_ref().map_or(false, |b| stmt_refs_name(b, name)),
        AstDeclData::Enum { values, .. } => values.iter().any(|v| expr_refs_name(v, name)),
        _ => false,
    }
}

fn convert_stmt(as_: &AstStmt, class_infos: &std::collections::BTreeMap<String, ClassInfo>) -> CgStmt {
    let line = as_.line; let col = as_.col;
    match &as_.data {
        AstStmtData::Expr(e) => CgStmt { kind: CgStmtKind::Expr, line, col, data: CgStmtData::Expr(convert_expr(e, &class_infos)) },
        AstStmtData::Compound(stmts) => CgStmt { kind: CgStmtKind::Compound, line, col, data: CgStmtData::Compound(stmts.iter().map(|s| convert_stmt(s, &class_infos)).collect()) },
        AstStmtData::If { cond, then, else_ } => CgStmt {
            kind: CgStmtKind::If, line, col,
            data: CgStmtData::If { cond: Box::new(convert_expr(cond, &class_infos)), then: Box::new(convert_stmt(then, &class_infos)), else_: else_.as_ref().map(|e| Box::new(convert_stmt(e, &class_infos))) },
        },
        AstStmtData::While { cond, body } => CgStmt {
            kind: CgStmtKind::While, line, col,
            data: CgStmtData::While { cond: Box::new(convert_expr(cond, &class_infos)), body: Box::new(convert_stmt(body, &class_infos)) },
        },
        AstStmtData::Do { body, cond } => CgStmt {
            kind: CgStmtKind::Do, line, col,
            data: CgStmtData::Do { body: Box::new(convert_stmt(body, &class_infos)), cond: Box::new(convert_expr(cond, &class_infos)) },
        },
        AstStmtData::For { init, cond, incr, body } => CgStmt {
            kind: CgStmtKind::For, line, col,
            data: CgStmtData::For {
                init: init.as_ref().map(|i| Box::new(convert_stmt(i, &class_infos))),
                cond: cond.as_ref().map(|c| Box::new(convert_expr(c, &class_infos))),
                incr: incr.as_ref().map(|i| Box::new(convert_expr(i, &class_infos))),
                body: Box::new(convert_stmt(body, &class_infos)),
            },
        },
        AstStmtData::Return(value) => match as_.kind {
            AstStmtKind::Break => CgStmt { kind: CgStmtKind::Break, line, col, data: CgStmtData::Break },
            AstStmtKind::Continue => CgStmt { kind: CgStmtKind::Continue, line, col, data: CgStmtData::Continue },
            _ => {
                let mut cg_val = value.as_ref().map(|v| Box::new(convert_expr(v, &class_infos)));
                let rt_str = CURRENT_RETURN_TYPE.lock().unwrap().clone();
                if let Some(ref rt) = rt_str {
                    if rt.contains(" *") && rt != "NPObject *" {
                        // Check if the return value is `self` or a message-send (which
                        // the checker types as `int`).  Both need a cast to the function's
                        // return type to avoid `-Wincompatible-pointer-types` in gcc.
                        let is_self = value.as_ref().map_or(false, |v| matches!(&v.data, AstExprData::VarRef { name, .. } if name == "self" || name == "_self"));
                        let is_msg = value.as_ref().map_or(false, |v| matches!(v.data, AstExprData::MsgSend { .. }));
                        // Also type-driven: a value typed `NPObject *`/`id`
                        // returned from a specific-class function (e.g. the
                        // `@"…"` literal's `jeti_stringFromCstr` call) joins
                        // implicitly in ObjC; plain C wants the spelling.
                        let is_id_val = cg_val
                            .as_ref()
                            .and_then(|c| c.type_str.as_deref())
                            .map_or(false, |t| obj_wildcard_cast(rt, t).is_some());
                        if is_self || is_msg || is_id_val {
                            cg_val = Some(Box::new(CgExpr {
                                kind: CgExprKind::Cast, type_str: Some(rt.clone()), line, col,
                                data: CgExprData::Cast { target_type: rt.clone(), expr: cg_val.take().unwrap() },
                            }));
                        }
                    }
                }
                CgStmt { kind: CgStmtKind::Return, line, col, data: CgStmtData::Return(cg_val) }
            }
        }
        AstStmtData::Goto(label) => CgStmt { kind: CgStmtKind::Goto, line, col, data: CgStmtData::Goto(label.clone()) },
        AstStmtData::Label(name) => CgStmt { kind: CgStmtKind::Label, line, col, data: CgStmtData::Label(name.clone()) },
        AstStmtData::Switch { expr, body } => CgStmt { kind: CgStmtKind::Switch, line, col, data: CgStmtData::Switch { expr: Box::new(convert_expr(expr, &class_infos)), body: Box::new(convert_stmt(body, &class_infos)) } },
        AstStmtData::Case { value, body } => CgStmt { kind: CgStmtKind::Case, line, col, data: CgStmtData::Case { value: Box::new(convert_expr(value, &class_infos)), body: Box::new(convert_stmt(body, &class_infos)) } },
        AstStmtData::Default(body) => CgStmt { kind: CgStmtKind::Default, line, col, data: CgStmtData::Default(Box::new(convert_stmt(body, &class_infos))) },
        AstStmtData::Decl(d) => {
            // Record class-typed locals for dispatch-cast resolution (see
            // LOCAL_VAR_CLASS). Declared-before-use keeps the entry ahead of
            // the sends that need it; multi-var chains arrive via `next`.
            {
                let mut map = local_var_class();
                let mut cur: Option<&AstDecl> = Some(d);
                while let Some(decl) = cur {
                    if let (Some(name), AstDeclData::Variable { var_type, next, .. }) =
                        (&decl.name, &decl.data)
                    {
                        match var_type.as_deref() {
                            Some(vt) if vt.is_pointer && vt.prim == TypePrim::Named => {
                                if let Some(n) = vt.name.as_deref() {
                                    let base = n.split('<').next().unwrap_or(n);
                                    map.insert(name.clone(), base.to_string());
                                }
                            }
                            // A redeclaration with a non-class type must not
                            // leave the old class entry behind.
                            _ => { map.remove(name); }
                        }
                        cur = next.as_deref();
                    } else {
                        cur = None;
                    }
                }
            }
            let decls = convert_decl(d, &class_infos);
            if let Some(decl_cg) = decls.into_iter().next() {
                // A type definition in statement position (`struct X { … };`
                // inside a function body) must emit the definition itself.
                // Falling through to the empty-`return` arm dropped it, so the
                // following `struct X x;` became an incomplete type.
                if matches!(
                    &decl_cg.data,
                    CgDeclData::Struct { .. } | CgDeclData::Typedef { .. } | CgDeclData::Enum { .. }
                ) {
                    let mut text = String::new();
                    emit_decl(&decl_cg, &mut text);
                    return CgStmt {
                        kind: CgStmtKind::Decl,
                        line, col,
                        data: CgStmtData::RawLine(text.trim_end().to_string()),
                    };
                }
                match decl_cg.data {
                    CgDeclData::Variable { var_type, init, is_static, is_weak, is_block, next, .. } => {
                        let (decl_type, array_suffix) = if let Some(pos) = var_type.find('[') {
                            (var_type[..pos].trim().to_string(), Some(var_type[pos..].to_string()))
                        } else {
                            (var_type, None)
                        };
                        CgStmt {
                            kind: CgStmtKind::Decl, line, col,
                            data: CgStmtData::Decl { decl_type, name: decl_cg.name, init, array_suffix, is_static, is_weak, is_block, next, attributes: decl_cg.attributes },
                        }
                    }
                    // A raw pass-through line (`_Pragma("...")`, `#pragma mark`)
                    // in statement position stays a statement, so the line lands
                    // in the emitted C exactly where it appeared (a diagnostic
                    // push/pop only means something at its own position).
                    CgDeclData::RawLine(text) => CgStmt {
                        kind: CgStmtKind::Decl, line, col,
                        data: CgStmtData::RawLine(text),
                    },
                    _ => CgStmt { kind: CgStmtKind::Empty, line, col, data: CgStmtData::Return(None) },
                }
            } else {
                CgStmt { kind: CgStmtKind::Empty, line, col, data: CgStmtData::Return(None) }
            }
        }
        AstStmtData::Autoreleasepool(body) => {
            let cg_body = convert_stmt(body, &class_infos);
            let body_stmts = match cg_body.data {
                CgStmtData::Compound(ref stmts) => stmts.clone(),
                _ => vec![cg_body],
            };
            let mut stmts: Vec<CgStmt> = Vec::new();
            stmts.push(CgStmt {
                kind: CgStmtKind::Decl, line: 0, col: 0,
                data: CgStmtData::Decl {
                    decl_type: "jeti_autoreleasepool_t *".into(),
                    name: "__jeti_pool".into(),
                    init: Some(Box::new(CgExpr {
                        kind: CgExprKind::Ident, type_str: None, line: 0, col: 0,
                        data: CgExprData::Ident("jeti_autoreleasepoolPush()".into()),
                    })),
                    array_suffix: None,
                    is_static: false,
                    is_weak: false,
                    is_block: false,
                    next: vec![],
                    attributes: Vec::new(),
                },
            });
            stmts.extend(body_stmts);
            stmts.push(CgStmt {
                kind: CgStmtKind::Expr, line: 0, col: 0,
                data: CgStmtData::Expr(CgExpr {
                    kind: CgExprKind::Ident, type_str: None, line: 0, col: 0,
                    data: CgExprData::Ident("jeti_autoreleasepoolPop(__jeti_pool)".into()),
                }),
            });
            CgStmt { kind: CgStmtKind::Compound, line, col, data: CgStmtData::Compound(stmts) }
        }
        AstStmtData::ForIn { .. } => {
            // Unreachable today: the parser has no `for (x in y)` rule yet, so
            // no ForIn node reaches codegen. CgStmtData::ForIn (and its emitter
            // at ~5441) is written and waiting; wiring the syntax up should
            // produce CgStmtData::ForIn here instead of an empty compound.
            CgStmt { kind: CgStmtKind::Compound, line, col, data: CgStmtData::Compound(Vec::new()) }
        }
        AstStmtData::NoArc(body) => {
            // @noarc { } — emit the body as-is, no ARC injection
            convert_stmt(body, class_infos)
        }
        AstStmtData::Synchronized { lock, body } => {
            // @synchronized (obj) { ... } — real mutual exclusion:
            //   { long __jeti_sync_N __attribute__((cleanup(jeti_syncAutoCleanup)))
            //       = jeti_syncLock((void *)obj);
            //     <body> }
            // The cleanup attribute releases the lock on every scope exit
            // (normal end, return, break, continue). A @throw escaping the
            // block longjmps PAST the scope — cleanup can't fire — so the
            // checker warns on throws inside a synchronized body. Freestanding
            // builds: lock/unlock are single-core no-ops (runtime_freestanding.c).
            let lock_cg = convert_expr(lock, class_infos);
            let body_cg = convert_stmt(body, class_infos);
            let holder = format!("__jeti_sync_{}", next_temp_id());
            let mut stmts: Vec<CgStmt> = Vec::new();
            stmts.push(CgStmt {
                kind: CgStmtKind::Decl, line, col,
                data: CgStmtData::Decl {
                    decl_type: "long".into(),
                    name: holder,
                    init: Some(Box::new(CgExpr {
                        kind: CgExprKind::Call, type_str: None, line, col,
                        data: CgExprData::Call {
                            name: "jeti_syncLock".into(),
                            args: vec![CgExpr {
                                kind: CgExprKind::Cast, type_str: None, line, col,
                                data: CgExprData::Cast {
                                    target_type: "void *".into(),
                                    expr: Box::new(lock_cg),
                                },
                            }],
                            vtable_class: None, alt_vtable_classes: vec![],
                            is_class_method: false, is_super: false,
                            sel_const_name: None, method_index: None, direct_specialized: false,
                        },
                    })),
                    array_suffix: None, is_static: false, is_weak: false, is_block: false,
                    next: vec![],
                    attributes: vec!["cleanup(jeti_syncAutoCleanup)".into()],
                },
            });
            match body_cg.data {
                CgStmtData::Compound(inner) => stmts.extend(inner),
                other => stmts.push(CgStmt { kind: CgStmtKind::Expr, line, col, data: other }),
            }
            CgStmt { kind: CgStmtKind::Compound, line, col, data: CgStmtData::Compound(stmts) }
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            // setjmp/longjmp exception handling with save/restore for nesting
            //
            // ARC owns ALL automatic release insertion (scope-end, @throw,
            // return, break/continue — see crates/arc). An earlier
            // "unwind-lift" shadow mechanism here DOUBLE-RELEASED: ARC
            // already releases owned locals before @throw, and the lift's
            // shadow release was not gated on __jeti_state == 1, so even the
            // normal no-throw path hit freed memory (ASan UAF, verified).
            // Removed; policy is prefer leak over double-release (Unknown-
            // merge locals may leak on the throw path — same conservative
            // direction as ARC's RefVal state machine).
            let try_cg = convert_stmt(try_block, class_infos);
            let finally_cg = finally_block.as_ref().map(|fb| convert_stmt(fb, class_infos));

            // Build the catch blocks: each Catch { param, body } becomes:
            //   { param_type param_name = __jeti_exception_value; body }
            // Wrap in "if (__jeti_state == 1) { __jeti_state = 2; <catches> }"
let mut catch_body: Vec<CgStmt> = Vec::new();
            if !catches.is_empty() {
                for c in catches.iter() {
                    if let AstStmtData::Catch { param, body } = &c.data {
                        let mut catch_stmts: Vec<CgStmt> = Vec::new();
                        // Each catch starts by marking state=2 so later catches
                        // won't match (they check __jeti_state == 1).
                        catch_stmts.push(CgStmt {
                            kind: CgStmtKind::Expr, line, col,
                            data: CgStmtData::Expr(CgExpr {
                                kind: CgExprKind::Assign, type_str: None, line, col,
                                data: CgExprData::Assign {
                                    target: Box::new(CgExpr {
                                        kind: CgExprKind::Ident, type_str: None, line, col,
                                        data: CgExprData::Ident("__jeti_state".into()),
                                    }),
                                    value: Box::new(CgExpr {
                                        kind: CgExprKind::Int, type_str: None, line, col,
                                        data: CgExprData::Int(2),
                                    }),
                                },
                            }),
                        });
                        let param_type = param.par_type.as_ref()
                            .map(|pt| cst_type_to_c_str(pt))
                            .unwrap_or_else(|| "id".into());
                        let param_name = param.name.clone().unwrap_or_else(|| "exc".into());
                        // Only emit a value-initialized declaration when the catch body
                        // actually references the parameter. Otherwise emitting
                        // `T e = __jeti_exception_value;` produces a dead store
                        // (clang -Wunused-but-set-variable / analyzer DeadStores).
                        // When unused, declare the name without an initializer and
                        // add `(void)name;` to silence the unused-variable warning.
                        let param_used = stmt_refs_name(&*body, &param_name);
                        if param_used {
                            // Cast __jeti_exception_value (an NPObject *) to the catch
                            // param type so typed catches don't trigger incompatible
                            // pointer types with -Wall -Wextra.
                            let cast_ctor = CgExpr {
                                kind: CgExprKind::Cast, type_str: None, line, col,
                                data: CgExprData::Cast {
                                    target_type: param_type.clone(),
                                    expr: Box::new(CgExpr {
                                        kind: CgExprKind::Ident, type_str: None, line, col,
                                        data: CgExprData::Ident("__jeti_exception_value".into()),
                                    }),
                                },
                            };
                            catch_stmts.push(CgStmt {
                                kind: CgStmtKind::Decl, line, col,
                                data: CgStmtData::Decl {
                                    decl_type: param_type,
                                    name: param_name,
                                    init: Some(Box::new(cast_ctor)),
                                    array_suffix: None,
                                    is_static: false,
                                    is_weak: false,
                                    is_block: false,
                                    next: vec![],
                                    attributes: Vec::new(),
                                },
                            });
                        } else {
                            catch_stmts.push(CgStmt {
                                kind: CgStmtKind::Decl, line, col,
                                data: CgStmtData::Decl {
                                    decl_type: param_type,
                                    name: param_name.clone(),
                                    init: None,
                                    array_suffix: None,
                                    is_static: false,
                                    is_weak: false,
                                    is_block: false,
                                    next: vec![],
                                    attributes: Vec::new(),
                                },
                            });
                            catch_stmts.push(CgStmt {
                                kind: CgStmtKind::Expr, line, col,
                                data: CgStmtData::Expr(CgExpr {
                                    kind: CgExprKind::Unary, type_str: None, line, col,
                                    data: CgExprData::Unary {
                                        op_str: "(void)".into(),
                                        operand: Box::new(CgExpr {
                                            kind: CgExprKind::Ident, type_str: None, line, col,
                                            data: CgExprData::Ident(param_name),
                                        }),
                                        is_postfix: false,
                                    },
                                }),
                            });
                        }
                        catch_stmts.push(convert_stmt(&*body, class_infos));
                        // If the catch type is a concrete class (not `id`), add an
                        // isa check so the catch only matches when the thrown
                        // object is an instance of the catch type (isKindOf: —
                        // subclasses included, matching ObjC).
                        let isa_check = param.par_type.as_ref().and_then(|pt| {
                            // Walk through pointer wrappers to find the Named type
                            let mut t = pt;
                            while t.is_pointer { t = t.subtype.as_ref()?; }
                            if t.prim != TypePrim::Named { return None; }
                            let name = t.name.as_ref()?;
                            let flat = name_flat(name);
                            if !class_infos.contains_key(&flat) { return None; }
                            // Build: __jeti_eh_isa((NPObject *)__jeti_exception_value,
                            //                       &jeti_Flat_class)
                            // __jeti_eh_isa walks the superclass chain and is
                            // nil-safe (runtime.c) — a subclass instance matches a
                            // parent-class arm. The old exact `isa ==` comparison
                            // silently failed to catch subclasses (probe: throw
                            // Sub, catch Super → uncaught, exit=1), unlike the
                            // eh-checked and baremetal backends, which already
                            // used this isKindOf form.
                            Some(CgExpr {
                                kind: CgExprKind::Call, type_str: None, line, col,
                                data: CgExprData::Call {
                                    name: "__jeti_eh_isa".into(),
                                    args: vec![
                                        CgExpr {
                                            kind: CgExprKind::Cast, type_str: None, line, col,
                                            data: CgExprData::Cast {
                                                target_type: "NPObject *".into(),
                                                expr: Box::new(CgExpr {
                                                    kind: CgExprKind::Ident, type_str: None, line, col,
                                                    data: CgExprData::Ident("__jeti_exception_value".into()),
                                                }),
                                            },
                                        },
                                        CgExpr {
                                            kind: CgExprKind::Unary, type_str: None, line, col,
                                            data: CgExprData::Unary {
                                                op_str: "&".into(),
                                                operand: Box::new(CgExpr {
                                                    kind: CgExprKind::Ident, type_str: None, line, col,
                                                    data: CgExprData::Ident(meta_symbol("CLASS_", &flat)),
                                                }),
                                                is_postfix: false,
                                            },
                                        },
                                    ],
                                    vtable_class: None, alt_vtable_classes: vec![],
                                    is_class_method: false, is_super: false,
                                    sel_const_name: None, method_index: None, direct_specialized: false,
                                },
                            })
                        });
                        // Wrap the catch body in an isa check if present
                        let inner: Box<CgStmt> = if let Some(cond) = isa_check {
                            Box::new(CgStmt {
                                kind: CgStmtKind::If, line, col,
                                data: CgStmtData::If {
                                    cond: Box::new(cond),
                                    then: Box::new(CgStmt {
                                        kind: CgStmtKind::Compound, line, col,
                                        data: CgStmtData::Compound(catch_stmts),
                                    }),
                                    else_: None,
                                },
                            })
                        } else {
                            Box::new(CgStmt {
                                kind: CgStmtKind::Compound, line, col,
                                data: CgStmtData::Compound(catch_stmts),
                            })
                        };
                        // if (__jeti_state == 1) { ... }
                        let state_cond = CgExpr {
                            kind: CgExprKind::Binary, type_str: None, line, col,
                            data: CgExprData::Binary {
                                op_str: "==".into(),
                                left: Box::new(CgExpr {
                                    kind: CgExprKind::Ident, type_str: None, line, col,
                                    data: CgExprData::Ident("__jeti_state".into()),
                                }),
                                right: Box::new(CgExpr {
                                    kind: CgExprKind::Int, type_str: None, line, col,
                                    data: CgExprData::Int(1),
                                }),
                            },
                        };
                        catch_body.push(CgStmt {
                            kind: CgStmtKind::If, line, col,
                            data: CgStmtData::If {
                                cond: Box::new(state_cond),
                                then: inner,
                                else_: None,
                            },
                        });
                    }
                }
            }

            // ── Build the full try/catch/finally pattern ──
            // {
            //   jmp_buf __jeti_saved;
            //   memcpy(__jeti_saved, __jeti_exception_buf, sizeof(jmp_buf));
            //   volatile int __jeti_state = 0;
            //   if (setjmp(__jeti_exception_buf) != 0) { __jeti_state = 1; }
            //   if (__jeti_state == 0) { <try_body> }
            //   <catch_body_if_state_1>
            //   memcpy(__jeti_exception_buf, __jeti_saved, sizeof(jmp_buf));
            //   <finally_block>
            //   if (__jeti_state == 1) { longjmp(...); }
            // }
            let mut try_stmts: Vec<CgStmt> = Vec::new();

            // jmp_buf __jeti_saved;
            try_stmts.push(CgStmt {
                kind: CgStmtKind::Decl, line, col,
                data: CgStmtData::Decl {
                    decl_type: "jmp_buf".into(),
                    name: "__jeti_saved".into(),
                    init: None,
                    array_suffix: None,
                    is_static: false,
                    is_weak: false,
                    is_block: false,
                    next: vec![],
                    attributes: Vec::new(),
                },
            });

            // memcpy(__jeti_saved, __jeti_exception_buf, sizeof(jmp_buf));
            try_stmts.push(CgStmt {
                kind: CgStmtKind::Expr, line, col,
                data: CgStmtData::Expr(CgExpr {
                    kind: CgExprKind::Call, type_str: None, line, col,
                    data: CgExprData::Call {
                        name: "memcpy".into(),
                        args: vec![
                            CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line, col,
                                data: CgExprData::Ident("__jeti_saved".into()),
                            },
                            CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line, col,
                                data: CgExprData::Ident("__jeti_exception_buf".into()),
                            },
                            CgExpr {
                                kind: CgExprKind::Sizeof, type_str: None, line, col,
                                data: CgExprData::Sizeof { type_str: "jmp_buf".into(), is_alignof: false },
                            },
                        ],
                        vtable_class: None,
                        alt_vtable_classes: vec![],
                        is_class_method: false,
                        is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }),
            });

            // volatile int __jeti_state = 0;
            try_stmts.push(CgStmt {
                kind: CgStmtKind::Decl, line, col,
                data: CgStmtData::Decl {
                    decl_type: "volatile int".into(),
                    name: "__jeti_state".into(),
                    init: Some(Box::new(CgExpr {
                        kind: CgExprKind::Int, type_str: None, line, col,
                        data: CgExprData::Int(0),
                    })),
                    array_suffix: None,
                    is_static: false,
                    is_weak: false,
                    is_block: false,
                    next: vec![],
                    attributes: Vec::new(),
                },
            });

            // if (setjmp(__jeti_exception_buf) != 0) { __jeti_state = 1; }
            try_stmts.push(CgStmt {
                kind: CgStmtKind::If, line, col,
                data: CgStmtData::If {
                    cond: Box::new(CgExpr {
                        kind: CgExprKind::Binary, type_str: None, line, col,
                        data: CgExprData::Binary {
                            op_str: "!=".into(),
                            left: Box::new(CgExpr {
                                kind: CgExprKind::Call, type_str: None, line, col,
                                data: CgExprData::Call {
                                    name: "setjmp".into(),
                                    args: vec![CgExpr {
                                        kind: CgExprKind::Ident, type_str: None, line, col,
                                        data: CgExprData::Ident("__jeti_exception_buf".into()),
                                    }],
                                    vtable_class: None, alt_vtable_classes: vec![],
                                    is_class_method: false, is_super: false, sel_const_name: None, method_index: None, direct_specialized: false,
                                },
                            }),
                            right: Box::new(CgExpr {
                                kind: CgExprKind::Int, type_str: None, line, col,
                                data: CgExprData::Int(0),
                            }),
                        },
                    }),
                    then: Box::new(CgStmt {
                        kind: CgStmtKind::Expr, line, col,
                        data: CgStmtData::Expr(CgExpr {
                            kind: CgExprKind::Assign, type_str: None, line, col,
                            data: CgExprData::Assign {
                                target: Box::new(CgExpr {
                                    kind: CgExprKind::Ident, type_str: None, line, col,
                                    data: CgExprData::Ident("__jeti_state".into()),
                                }),
                                value: Box::new(CgExpr {
                                    kind: CgExprKind::Int, type_str: None, line, col,
                                    data: CgExprData::Int(1),
                                }),
                            },
                        }),
                    }),
                    else_: None,
                },
            });

            // if (__jeti_state == 0) { <try_body> }
            try_stmts.push(CgStmt {
                kind: CgStmtKind::If, line, col,
                data: CgStmtData::If {
                    cond: Box::new(CgExpr {
                        kind: CgExprKind::Binary, type_str: None, line, col,
                        data: CgExprData::Binary {
                            op_str: "==".into(),
                            left: Box::new(CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line, col,
                                data: CgExprData::Ident("__jeti_state".into()),
                            }),
                            right: Box::new(CgExpr {
                                kind: CgExprKind::Int, type_str: None, line, col,
                                data: CgExprData::Int(0),
                            }),
                        },
                    }),
                    then: Box::new(try_cg),
                    else_: None,
                },
            });

            // Disarm this @try BEFORE handler selection: restore the parent
            // jmp_buf so a `@throw` inside a catch body (rethrow) longjmps to
            // the OUTER try's setjmp instead of re-entering this frame's own
            // setjmp — which would re-select the same catch and loop forever.
            try_stmts.push(CgStmt {
                kind: CgStmtKind::Expr, line, col,
                data: CgStmtData::Expr(CgExpr {
                    kind: CgExprKind::Call, type_str: None, line, col,
                    data: CgExprData::Call {
                        name: "memcpy".into(),
                        args: vec![
                            CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident("__jeti_exception_buf".into()) },
                            CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident("__jeti_saved".into()) },
                            CgExpr { kind: CgExprKind::Sizeof, type_str: None, line, col, data: CgExprData::Sizeof { type_str: "jmp_buf".into(), is_alignof: false } },
                        ],
                        vtable_class: None, alt_vtable_classes: vec![],
                        is_class_method: false, is_super: false, sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }),
            });

            // catch blocks (if any)
            try_stmts.extend(catch_body);

            // memcpy(__jeti_exception_buf, __jeti_saved, sizeof(jmp_buf));
            try_stmts.push(CgStmt {
                kind: CgStmtKind::Expr, line, col,
                data: CgStmtData::Expr(CgExpr {
                    kind: CgExprKind::Call, type_str: None, line, col,
                    data: CgExprData::Call {
                        name: "memcpy".into(),
                        args: vec![
                            CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident("__jeti_exception_buf".into()) },
                            CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident("__jeti_saved".into()) },
                            CgExpr { kind: CgExprKind::Sizeof, type_str: None, line, col, data: CgExprData::Sizeof { type_str: "jmp_buf".into(), is_alignof: false } },
                        ],
                        vtable_class: None, alt_vtable_classes: vec![],
                        is_class_method: false, is_super: false, sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }),
            });

            // finally block
            if let Some(f) = finally_cg {
                try_stmts.push(f);
            }

            // NOTE: no unwind-path release here. An earlier "unwind-lift"
            // shadow mechanism released lifted locals before the rethrow
            // longjmp, but (a) it was NOT gated on __jeti_state == 1, so the
            // NORMAL no-throw path double-released (ASan UAF, verified), and
            // (b) ARC already releases owned locals before @throw
            // (crates/arc). ARC is the single owner of automatic release
            // insertion; prefer leak over double-release.

            // if (__jeti_state == 1) { longjmp(__jeti_exception_buf, 1); }
            try_stmts.push(CgStmt {
                kind: CgStmtKind::If, line, col,
                data: CgStmtData::If {
                    cond: Box::new(CgExpr {
                        kind: CgExprKind::Binary, type_str: None, line, col,
                        data: CgExprData::Binary {
                            op_str: "==".into(),
                            left: Box::new(CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line, col,
                                data: CgExprData::Ident("__jeti_state".into()),
                            }),
                            right: Box::new(CgExpr {
                                kind: CgExprKind::Int, type_str: None, line, col,
                                data: CgExprData::Int(1),
                            }),
                        },
                    }),
                    then: Box::new(CgStmt {
                        kind: CgStmtKind::Expr, line, col,
                        data: CgStmtData::Expr(CgExpr {
                            kind: CgExprKind::Call, type_str: None, line, col,
                            data: CgExprData::Call {
                                name: "longjmp".into(),
                                args: vec![
                                    CgExpr { kind: CgExprKind::Ident, type_str: None, line, col, data: CgExprData::Ident("__jeti_exception_buf".into()) },
                                    CgExpr { kind: CgExprKind::Int, type_str: None, line, col, data: CgExprData::Int(1) },
                                ],
                                vtable_class: None, alt_vtable_classes: vec![],
                                is_class_method: false, is_super: false, sel_const_name: None, method_index: None, direct_specialized: false,
                            },
                        }),
                    }),
                    else_: None,
                },
            });

            CgStmt { kind: CgStmtKind::Compound, line, col, data: CgStmtData::Compound(try_stmts) }
        }
        AstStmtData::Catch { .. } | AstStmtData::Finally(_) => {
            CgStmt { kind: CgStmtKind::Compound, line, col, data: CgStmtData::Compound(Vec::new()) }
        }
        AstStmtData::Throw(expr) => {
            let mut stmts: Vec<CgStmt> = Vec::new();
            if let Some(e) = expr {
                // __jeti_exception_value = (expr);
                stmts.push(CgStmt {
                    kind: CgStmtKind::Expr, line, col,
                    data: CgStmtData::Expr(CgExpr {
                        kind: CgExprKind::Assign, type_str: None, line, col,
                        data: CgExprData::Assign {
                            target: Box::new(CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line, col,
                                data: CgExprData::Ident("__jeti_exception_value".into()),
                            }),
                            value: Box::new(convert_expr(e, class_infos)),
                        },
                    }),
                });
            }
            // longjmp(__jeti_exception_buf, 1);
            stmts.push(CgStmt {
                kind: CgStmtKind::Expr, line, col,
                data: CgStmtData::Expr(CgExpr {
                    kind: CgExprKind::Call, type_str: None, line, col,
                    data: CgExprData::Call {
                        name: "longjmp".into(),
                        args: vec![
                            CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line, col,
                                data: CgExprData::Ident("__jeti_exception_buf".into()),
                            },
                            CgExpr {
                                kind: CgExprKind::Int, type_str: None, line, col,
                                data: CgExprData::Int(1),
                            },
                        ],
                        vtable_class: None,
                        alt_vtable_classes: vec![],
                        is_class_method: false,
                        is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }),
            });
            CgStmt { kind: CgStmtKind::Compound, line, col, data: CgStmtData::Compound(stmts) }
        }
        AstStmtData::Asm { is_volatile, is_goto, template, outputs, inputs, clobbers, labels } => {
            let conv = |ops: &Vec<AstAsmOperand>| -> Vec<CgAsmOperand> {
                ops.iter().map(|op| CgAsmOperand {
                    name: op.name.clone(),
                    constraint: op.constraint.clone(),
                    expr: convert_expr(&op.expr, class_infos),
                }).collect()
            };
            CgStmt {
                kind: CgStmtKind::Asm, line, col,
                data: CgStmtData::Asm {
                    is_volatile: *is_volatile,
                    is_goto: *is_goto,
                    template: template.clone(),
                    outputs: conv(outputs),
                    inputs: conv(inputs),
                    clobbers: clobbers.clone(),
                    labels: labels.clone(),
                },
            }
        }
        // Deliberate catch-all: every AstStmtData variant is matched above, so this
        // arm is currently unreachable. It stays as a net so a newly added variant
        // degrades to a marked stub rather than a non-exhaustive-match build error.
        #[allow(unreachable_patterns)]
        _ => CgStmt { kind: CgStmtKind::Expr, line, col, data: CgStmtData::Expr(CgExpr { kind: CgExprKind::Call, type_str: None, line, col, data: CgExprData::Call { name: "/* stub */".into(), args: Vec::new(), vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false, sel_const_name: None, method_index: None, direct_specialized: false } }) },
    }
}

fn convert_decl(ad: &AstDecl, class_infos: &std::collections::BTreeMap<String, ClassInfo>) -> Vec<CgDecl> {
    let name = ad.name.clone().unwrap_or_default();
    let mut result = vec![];
    match ad.kind {
        AstDeclKind::Function => {
            let (return_type, func_params, body, is_variadic) = match &ad.data {
                AstDeclData::Function { return_type, params, body, has_variadic, .. } => {
                    let rt = return_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "int".into());
                    let mut cg_params = Vec::new();
                    let mut p = params.as_ref().map(|b| &**b);
                    while let Some(param) = p {
                        let pt = param.par_type.as_ref()
                            .map(|t| cst_type_to_c_str(t))
                            .unwrap_or_else(|| "int".into());
                        let pn = param_attr_decl(param);
                        cg_params.push((pt, pn));
                        p = param.next.as_ref().map(|n| &**n);
                    }
                    // Record the declared signature so later Call sites in
                    // this unit can spell the implicit id↔class-pointer
                    // casts plain C requires.
                    register_func_sig(&name, &cg_params);
                    // Set the enclosing function's return type so `convert_stmt`
                    // for Return can add covariant subclass casts.
                    *CURRENT_RETURN_TYPE.lock().unwrap() = Some(rt.clone());
                    let mut cg_body = body.as_ref().map(|b| Box::new(convert_stmt(b, &class_infos)));
                    *CURRENT_RETURN_TYPE.lock().unwrap() = None;
                    if name == "main" && !class_infos.is_empty() {
                        let mut stmts = Vec::new();
                        stmts.push(CgStmt {
                            kind: CgStmtKind::Expr, line: 0, col: 0,
                            data: CgStmtData::Expr(CgExpr {
                                kind: CgExprKind::Call, type_str: None, line: 0, col: 0,
                                    data: CgExprData::Call {
                                        name: "jeti_metaInit".into(),
                                        args: Vec::new(),
                        vtable_class: None,
                        alt_vtable_classes: vec![],
                        is_class_method: false,
                        is_super: false,
                        sel_const_name: None, method_index: None, direct_specialized: false,
                    },
                }),
            });
                        if let Some(ref mut b) = cg_body {
                            if let CgStmtData::Compound(ref mut inner) = b.data {
                                stmts.extend(inner.clone());
                            } else {
                                stmts.push(b.as_ref().clone());
                            }
                        }
                        cg_body = Some(Box::new(CgStmt {
                            kind: CgStmtKind::Compound, line: 0, col: 0,
                            data: CgStmtData::Compound(stmts),
                        }));
                    }
                    (rt, cg_params, cg_body, *has_variadic)
                }
                _ => ("int".to_string(), Vec::new(), None, false),
            };
            result.push(CgDecl {
                kind: CgDeclKind::Function, name,
                data: CgDeclData::Function {
                    return_type, params: func_params,
                    is_variadic, is_objc_class: false, body,
                },
                attributes: ad.attributes.clone(),
});
        }
        AstDeclKind::Variable => {
            let name = ad.name.clone().unwrap_or_default();
            // Register block-typed variable names so the FuncCall handler can
            // detect block invocations (for gcc/portable .invoke dispatch).
            if let AstDeclData::Variable { var_type, .. } = &ad.data {
                let type_str = var_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_default();
                // A variable is a block variable only if its type is a real
                // block (inline `^` or a registered block typedef). C fnptr
                // variables (`int (*cb)(int)` / fnptr typedefs) must stay
                // plain C calls — `fn(4,5)`, never `->invoke`.
                let is_fnptr = var_type.as_ref().map_or(false, |t| t.is_fn_ptr)
                    || FNPTR_TYPEDEF_NAMES.get().map_or(false, |m| m.lock().unwrap().contains(&type_str));
                let is_block = !is_fnptr && (
                    var_type.as_ref().map_or(false, |t| t.is_block)
                        || type_str.contains("__jeti_block_header")
                        || BLOCK_TYPEDEF_NAMES.get().map_or(false, |m| m.lock().unwrap().contains_key(&type_str)));
                if is_block {
                    let mut bv = block_vars();
                    bv.get_or_insert_with(std::collections::HashSet::new).insert(name.clone());
                }
            }
            let (var_type, init, is_static, is_extern, is_const, is_block_qual, is_weak) = match &ad.data {
                AstDeclData::Variable { var_type, init, is_static, is_extern, is_const, is_block_qual, is_weak, .. } => {
                    let vt = var_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "int".into());
                    let mut init_cg = init.as_ref().map(|i| Box::new(convert_expr(i, &class_infos)));
                    // Declared-type initializer upcast: `NPObject *o = str,`
                    // joins implicitly in ObjC; plain C wants the spelling.
                    if let Some(vt_s) = Some(vt.as_str()) {
                        if let Some(init_val) = init_cg.as_deref_mut() {
                            if let Some(st) = init_val.type_str.as_deref() {
                                if let Some(cast) = obj_wildcard_cast(vt_s, st) {
                                    let inner = std::mem::replace(init_val, CgExpr {
                                        kind: CgExprKind::Int, type_str: None, line: init_val.line, col: init_val.col,
                                        data: CgExprData::Int(0),
                                    });
                                    *init_val = CgExpr {
                                        kind: CgExprKind::Cast, type_str: Some(cast.clone()), line: inner.line, col: inner.col,
                                        data: CgExprData::Cast {
                                            target_type: cast,
                                            expr: Box::new(inner),
                                        },
                                    };
                                }
                            }
                        }
                    }
                    (vt, init_cg, *is_static, *is_extern, *is_const, *is_block_qual, *is_weak)
                }
                _ => ("int".into(), None, false, false, false, false, false),
            };
            let var_type = if is_weak && !var_type.contains("__block") {
                format!("__block {}", var_type)
            } else {
                var_type
            };
            // Decl-level `const` (`const int z = 1;`) qualifies the declared
            // object; the statement-level emitter has no is_const column, so
            // fold it into the spelled type here. Skip when the type already
            // spells it (`int const w` puts is_const on the type node and the
            // type speller emitted `const int`).
            let var_type = if is_const && !var_type.starts_with("const ") {
                format!("const {}", var_type)
            } else {
                var_type
            };
            // Follow next chain (comma-separated declarators)
            let mut next_decls = Vec::new();
            let mut n = match &ad.data { AstDeclData::Variable { ref next, .. } => next.as_ref().map(|b| &**b), _ => None };
            while let Some(next_ad) = n {
                let next_name = next_ad.name.clone().unwrap_or_default();
                let (next_type, next_init) = match &next_ad.data {
                    AstDeclData::Variable { var_type, init, .. } => (
                        // Each declarator carries its own type (per-declarator
                        // `*`/`[]`); empty string = shares the head type.
                        var_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_default(),
                        init.as_ref().map(|i| Box::new(convert_expr(i, &class_infos))),
                    ),
                    _ => (String::new(), None),
                };
                next_decls.push((next_name, next_type, next_init));
                n = match &next_ad.data { AstDeclData::Variable { ref next, .. } => next.as_ref().map(|b| &**b), _ => None };
            }
            result.push(CgDecl {
                kind: CgDeclKind::Variable, name,
                data: CgDeclData::Variable { var_type, init, is_static, is_extern, is_const, is_weak, is_block: is_block_qual, next: next_decls },
                attributes: ad.attributes.clone(),
});
        }
        AstDeclKind::Typedef => {
            let (mut alias_type_str, struct_fields, has_declarator_name) = match &ad.data {
                AstDeclData::Typedef { aliased_type, struct_fields } => {
                    let mut alias_type_str = "int".to_string();
                    let mut has_declarator_name = false;
                    if let Some(ref at) = aliased_type {
                        alias_type_str = ast_type_to_c_str(at);
                        // A declarator typedef embeds its name in the type —
                        // either a block `(^Name)` or a C function pointer
                        // `(*Name)` both carry `block_name`.
                        has_declarator_name = at.block_name.is_some();
                    }
                    let fields = struct_fields.iter().map(|f| {
                        let mut fname = f.name.clone().unwrap_or_default();
                        let ftype = match &f.data {
                            AstDeclData::Variable { var_type, .. } => var_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "int".into()),
                            AstDeclData::Ivar { ivar_type, .. } => ivar_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "int".into()),
                            _ => "int".into(),
                        };
                        // Function-pointer / block fields embed their name in
                        // the declarator (`int (*cb)(int)`), so the separate
                        // field name must not be appended again.
                        if ftype.contains("(*") || ftype.contains("(^") {
                            fname = String::new();
                        }
                        (ftype, fname, f.attributes.clone())
                    }).collect();
                    (alias_type_str, fields, has_declarator_name)
                }
                _ => ("int".into(), Vec::new(), false),
            };
            let flat_alias = name_flat(&name);
            if has_declarator_name {
                let short_block_name = match &ad.data {
                    AstDeclData::Typedef { aliased_type, .. } => {
                        aliased_type.as_ref().and_then(|at| at.block_name.clone())
                    }
                    _ => None,
                };
                let is_fnptr_typedef = match &ad.data {
                    AstDeclData::Typedef { aliased_type, .. } => {
                        aliased_type.as_ref().map_or(false, |at| at.is_fn_ptr)
                    }
                    _ => false,
                };
                if let Some(ref sn) = short_block_name {
                    if let Some(pos) = alias_type_str.find(sn.as_str()) {
                        alias_type_str.replace_range(pos..pos + sn.len(), &flat_alias);
                    }
                    if let Ok(mut guard) = BLOCK_TYPEDEF_NAMES.get_or_init(|| Mutex::new(HashMap::new())).lock() {
                        guard.insert(sn.clone(), flat_alias.clone());
                    }
                    // fnptr typedefs share the flat-name map for field/ivar
                    // resolution but must never be treated as block variables
                    // (a plain fnptr call is `fn(4,5)`, not `->invoke`).
                    if is_fnptr_typedef {
                        if let Ok(mut guard) = FNPTR_TYPEDEF_NAMES.get_or_init(|| Mutex::new(std::collections::HashSet::new())).lock() {
                            guard.insert(sn.clone());
                        }
                    }
                }
            }
            let alias = if has_declarator_name { String::new() } else { flat_alias.clone() };
            result.push(CgDecl {
                kind: CgDeclKind::Typedef, name: ad.name.clone().unwrap_or_default(),
                data: CgDeclData::Typedef { alias, type_str: alias_type_str.clone(), struct_fields },
                attributes: ad.attributes.clone(),
});
        }
        AstDeclKind::Struct => {
            let (fields, is_union) = match &ad.data {
                AstDeclData::Aggregate { fields, is_union } => (fields, *is_union),
                _ => (&Vec::new(), false),
            };
            if fields.is_empty() {
                result.push(CgDecl { kind: CgDeclKind::Variable, name, data: CgDeclData::Variable { var_type: "void".into(), init: None, is_static: false, is_extern: false, is_const: false, is_weak: false, is_block: false, next: vec![] }, attributes: Vec::new() });
            } else {
                let mut fields_simple: Vec<(String, String, Vec<String>)> = Vec::new();
                for f in fields {
                    if let AstDeclData::Ivar { ivar_type, .. } = &f.data {
                        let mut ft = ivar_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "int".into());
                        // Resolve block typedef short names to namespace-prefixed flat names
                        if let Ok(guard) = BLOCK_TYPEDEF_NAMES.get_or_init(|| Mutex::new(HashMap::new())).lock() {
                            if let Some(flat) = guard.get(&ft) {
                                ft = flat.clone();
                            }
                        }
                        // Function-pointer / block fields embed their name in
                        // the declarator (`int (*cb)(int)`), so the separate
                        // field name must not be appended again.
                        let mut fn_ = f.name.clone().unwrap_or_default();
                        if ft.contains("(*") || ft.contains("(^") {
                            fn_ = String::new();
                        }
                        fields_simple.push((ft, fn_, f.attributes.clone()));
                    }
                }
                result.push(CgDecl { kind: CgDeclKind::Struct, name, data: CgDeclData::Struct { fields: fields_simple, is_union }, attributes: ad.attributes.clone() });
            }
        }
        AstDeclKind::Enum => {
            let members: Vec<(String, String)> = match &ad.data {
                AstDeclData::Enum { members, values } => {
                    members.iter().zip(values.iter()).map(|(m, v)| {
                        let val = match &v.data { AstExprData::Int(i) => format!("{}", i), _ => m.clone() };
                        (m.clone(), val)
                    }).collect()
                }
                _ => Vec::new(),
            };
            result.push(CgDecl { kind: CgDeclKind::Enum, name, data: CgDeclData::Enum { members }, attributes: ad.attributes.clone() });
        }
        AstDeclKind::Class | AstDeclKind::Protocol => {
            result.push(CgDecl { kind: CgDeclKind::Variable, name, data: CgDeclData::Variable { var_type: "void".into(), init: None, is_static: false, is_extern: false, is_const: false, is_weak: false, is_block: false, next: vec![] }, attributes: Vec::new() });
        }
        AstDeclKind::Asm => {
            let (is_volatile, is_goto, template, outputs, inputs, clobbers, labels) = match &ad.data {
                AstDeclData::Asm { is_volatile, is_goto, template, outputs, inputs, clobbers, labels } => (
                    *is_volatile,
                    *is_goto,
                    template.clone(),
                    outputs.iter().map(|op| CgAsmOperand { name: op.name.clone(), constraint: op.constraint.clone(), expr: convert_expr(&op.expr, class_infos) }).collect(),
                    inputs.iter().map(|op| CgAsmOperand { name: op.name.clone(), constraint: op.constraint.clone(), expr: convert_expr(&op.expr, class_infos) }).collect(),
                    clobbers.clone(),
                    labels.clone(),
                ),
                _ => (false, false, String::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            };
            result.push(CgDecl { kind: CgDeclKind::Asm, name: String::new(), data: CgDeclData::Asm { is_volatile, is_goto, template, outputs, inputs, clobbers, labels }, attributes: ad.attributes.clone() });
        }
        AstDeclKind::Ivar | AstDeclKind::Method | AstDeclKind::Property | AstDeclKind::Union | AstDeclKind::Namespace => {
            result.push(CgDecl { kind: CgDeclKind::Variable, name, data: CgDeclData::Variable { var_type: "int".into(), init: None, is_static: false, is_extern: false, is_const: false, is_weak: false, is_block: false, next: vec![] }, attributes: Vec::new() });
        }
        AstDeclKind::ForwardClass => {
            let names: Vec<String> = match &ad.data {
                AstDeclData::ForwardClass { names } => names.clone(),
                _ => Vec::new(),
            };
            result.push(CgDecl { kind: CgDeclKind::ForwardClass, name: String::new(), data: CgDeclData::ForwardClass { names }, attributes: Vec::new() });
        }
        AstDeclKind::RawLine => {
            let text = match &ad.data {
                AstDeclData::RawLine(s) => s.clone(),
                _ => String::new(),
            };
            result.push(CgDecl { kind: CgDeclKind::RawLine, name: String::new(), data: CgDeclData::RawLine(text), attributes: Vec::new() });
        }
    }
    result
}

// ─── AST → CgUnit ────────────────────────────────────────────────────────────

pub fn method_c_name(msym_name: &str, class_name: &str) -> String {
    let flat_cname = name_flat(class_name);
    let flat_msym = msym_name.replace(':', "_");
    format!("{}_{}", flat_cname, flat_msym)
}

fn split_array_type(t: &str) -> (&str, &str) {
    if let Some(pos) = t.find('[') {
        (&t[..pos].trim(), &t[pos..])
    } else {
        (t, "")
    }
}

/// Emit one struct/typedef field. Function-pointer and block declarator
/// fields embed their name in the type (`int (*cb)(int)`) and must NOT be
/// split on `[` (an fnptr array has its `[N]` inside the declarator), so
/// they are emitted verbatim with no separate name.
/// Emit one field-wise value-comparison function per struct tag the checker
/// marked (`jeti_struct_eq_<tag>`). `a == b` on two value structs is a C
/// compile error, so the checker rewrites it to a call to these functions.
/// Field comparison rules:
///   - nested struct (by value)  → recursive `jeti_struct_eq_<inner>(a.f, b.f)`
///   - array field               → `memcmp(a.f, b.f, sizeof a.f) == 0`
///   - everything else (scalars, pointers) → `a.f == b.f`
/// Static and only emitted for tags actually used, so no unused warnings.
fn emit_struct_eq_functions(unit: &CgUnit, out: &mut String) {
    if unit.struct_eq_tags.is_empty() { return; }
    // Field tables for every typedef-struct / plain struct in the unit.
    let mut fields_of: std::collections::HashMap<&str, &Vec<(String, String, Vec<String>)>> =
        std::collections::HashMap::new();
    for decl in &unit.decls {
        match &decl.data {
            CgDeclData::Typedef { alias, struct_fields, .. } if !struct_fields.is_empty() => {
                fields_of.insert(alias.as_str(), struct_fields);
            }
            CgDeclData::Struct { fields, .. } => {
                fields_of.insert(decl.name.as_str(), fields);
            }
            _ => {}
        }
    }
    // Resolve a field type string to the struct tag it is a value of, if any.
    // Handles `struct Tag`, `Tag` (typedef alias) and strips trailing `[N]` is
    // handled by the caller (arrays compare via memcmp before this runs).
    fn nested_value_tag<'a>(ft: &'a str, fields_of: &'a std::collections::HashMap<&str, &Vec<(String, String, Vec<String>)>>) -> Option<&'a str> {
        let bare = ft.trim().trim_end_matches('*').trim();
        let cand: &'a str = bare.strip_prefix("struct ").map(|s| s.trim()).unwrap_or(bare);
        if ft.trim_end().ends_with('*') { return None; } // pointer field: address compare
        if fields_of.contains_key(cand) { Some(cand) } else { None }
    }
    fn is_array_type(ft: &str) -> bool {
        ft.trim_end().ends_with(']')
    }
    // Expand the tag set with nested value-struct fields (transitively):
    // `struct Outer { struct Inner in; }` used with `==` must also emit (and
    // forward-declare) jeti_struct_eq_Inner, even if Inner is never compared
    // directly. Closures/fields_of are immutable here, so re-scan until fixed.
    let mut tags: Vec<String> = unit.struct_eq_tags.clone();
    let mut i = 0;
    while i < tags.len() {
        let fields = fields_of.get(tags[i].as_str());
        if let Some(fields) = fields {
            for (ft, _, _) in fields.iter() {
                if let Some(inner) = nested_value_tag(ft, &fields_of) {
                    let inner = inner.to_string();
                    if !tags.contains(&inner) {
                        tags.push(inner);
                    }
                }
            }
        }
        i += 1;
    }
    for tag in &tags {
        // Forward declarations first: nested structs may reference
        // jeti_struct_eq_<inner> defined later in this loop (C99 forbids
        // implicit declarations, so emission order must not matter).
        let _ = write!(out, "static int jeti_struct_eq_{tag}(struct {tag} a, struct {tag} b);\n");
    }
    if !unit.struct_eq_tags.is_empty() {
        out.push('\n');
    }
    for tag in &tags {
        let _ = write!(out, "/* Value equality for struct {tag} (generated for `==` on value structs) */\n");
        let _ = write!(out, "static int jeti_struct_eq_{tag}(struct {tag} a, struct {tag} b) {{\n");
        match fields_of.get(tag.as_str()) {
            Some(fields) if !fields.is_empty() => {
                let mut parts: Vec<String> = Vec::new();
                for (ft, fn_, _) in fields.iter() {
                    if is_array_type(ft) {
                        parts.push(format!("memcmp(a.{fn_}, b.{fn_}, sizeof a.{fn_}) == 0"));
                    } else if let Some(inner) = nested_value_tag(ft, &fields_of) {
                        parts.push(format!("jeti_struct_eq_{inner}(a.{fn_}, b.{fn_})"));
                    } else {
                        parts.push(format!("a.{fn_} == b.{fn_}"));
                    }
                }
                if parts.len() == 1 {
                    let _ = write!(out, "    return {};\n", parts[0]);
                } else {
                    out.push_str("    return ");
                    out.push_str(&parts.join("\n        && "));
                    out.push_str(";\n");
                }
            }
            _ => {
                // Empty or opaque struct: vacuously equal.
                out.push_str("    return 1;\n");
            }
        }
        out.push_str("}\n\n");
    }
}

fn emit_struct_field(ft: &str, fn_: &str, fattrs: &[String], out: &mut String) {
    out.push_str("    ");
    if ft.contains("(*") || ft.contains("(^") {
        out.push_str(ft);
    } else {
        let (base, suffix) = split_array_type(ft);
        out.push_str(base);
        out.push(' ');
        out.push_str(fn_);
        out.push_str(suffix);
    }
    for a in fattrs {
        // A `: N` attribute is NOT a `__attribute__`: it is the C17 6.7.2.1
        // bit-field width the parser records on the field. Emit it as the
        // declarator's `: N` suffix (clangd-parity: the hover shows the
        // width, and the generated C keeps the real bit-field semantics —
        // the old path silently dropped the width and emitted a plain
        // `unsigned flags`).
        if a.trim_start().starts_with(':') {
            let _ = write!(out, " {}", a.trim());
        } else {
            let _ = write!(out, " __attribute__(({}))", a);
        }
    }
    out.push_str(";\n");
}

/// The C keyword introducing an aggregate tag.
fn agg_keyword(is_union: bool) -> &'static str {
    if is_union { "union" } else { "struct" }
}

/// Emit all struct definitions (typedef-structs and plain structs) in
/// dependency order so a struct referenced BY VALUE by another struct is
/// defined first (C requires complete types for value members). Pointer
/// members only need a forward declaration, so self-references and
/// references to tags that are never defined in this unit never block.
fn emit_aggregate_definitions(unit: &CgUnit, out: &mut String) -> bool {
    struct AggDef {
        tag: String,
        is_typedef: bool,
        is_union: bool,
        alias: String,
        attributes: Vec<String>,
        fields: Vec<(String, String, Vec<String>)>,
        /// Index into `unit.decls` — i.e. the position in the source, which is
        /// what pass-through lines have to line up with.
        decl_idx: usize,
    }

    let mut defs: Vec<AggDef> = Vec::new();
    for (idx, decl) in unit.decls.iter().enumerate() {
        match &decl.data {
            CgDeclData::Typedef { alias, struct_fields, .. } if !struct_fields.is_empty() => {
                defs.push(AggDef {
                    tag: alias.clone(),
                    is_typedef: true,
                    is_union: false,
                    alias: alias.clone(),
                    attributes: decl.attributes.clone(),
                    fields: struct_fields.clone(),
                    decl_idx: idx,
                });
            }
            CgDeclData::Struct { fields, is_union } => {
                defs.push(AggDef {
                    tag: decl.name.clone(),
                    is_typedef: false,
                    is_union: *is_union,
                    alias: String::new(),
                    attributes: decl.attributes.clone(),
                    fields: fields.clone(),
                    decl_idx: idx,
                });
            }
            _ => {}
        }
    }
    if defs.is_empty() {
        // Nothing to interleave with: pass-through lines stay in the ordinary
        // declaration stream, where their position relative to functions is
        // already preserved.
        return false;
    }

    let defined: std::collections::HashSet<String> =
        defs.iter().map(|d| d.tag.clone()).collect();

    // Extract struct-tag dependencies referenced from field type strings.
    let deps: Vec<std::collections::HashSet<String>> = defs.iter().map(|d| {
        let mut s = std::collections::HashSet::new();
        for (ft, _, _) in &d.fields {
            let mut rest: &str = ft;
            while let Some(pos) = rest.find("struct ") {
                rest = &rest[pos + 7..];
                let name: String = rest.chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && name != d.tag && defined.contains(&name) {
                    s.insert(name);
                }
            }
        }
        s
    }).collect();

    // Emit in *source order*, pulling each definition's dependencies in on
    // demand just ahead of it. Source order is what keeps a pass-through line
    // where the author put it — `_Pragma("pack(push, 1)")` above a struct only
    // packs that struct because it is still above it in the output — so RawLine
    // declarations are emitted here, interleaved with the definitions (the
    // caller skips them further down the file). A dependency defined later in
    // the source is still hoisted ahead of its user, as before.
    let def_of_decl: std::collections::HashMap<usize, usize> =
        defs.iter().enumerate().map(|(i, d)| (d.decl_idx, i)).collect();

    fn emit_agg_def(d: &AggDef, out: &mut String) {
        // `jeti_root` / `NPObject` are also defined by `jeti/runtime.h`, behind
        // these macros. Re-emit the guard around a *parsed* definition too, or a
        // re-processed file collides with the header it includes (the source
        // guard is a preprocessor line, so it does not survive re-emission).
        let guard = match d.tag.as_str() {
            "jeti_root" => Some("JETI_ROOT_DEFINED"),
            "NPObject" => Some("NPOBJECT_DEFINED"),
            _ => None,
        };
        if let Some(g) = guard {
            let _ = writeln!(out, "#ifndef {}", g);
            let _ = writeln!(out, "#define {}", g);
        }
        if d.is_typedef {
            out.push_str("typedef ");
            emit_attrs_prefix(&d.attributes, out);
            let _ = write!(out, "{} {} {{\n", agg_keyword(d.is_union), d.tag);
            for (ft, fn_, fattrs) in &d.fields {
                emit_struct_field(ft, fn_, fattrs, out);
            }
            let _ = write!(out, "}} {};\n", d.alias);
        } else {
            let _ = write!(out, "{} {} {{\n", agg_keyword(d.is_union), d.tag);
            for (ft, fn_, fattrs) in &d.fields {
                emit_struct_field(ft, fn_, fattrs, out);
            }
            out.push_str("}");
            emit_attrs_prefix(&d.attributes, out);
            out.push_str(";\n");
        }
        if guard.is_some() {
            out.push_str("#endif\n");
        }
    }

    let mut emitted: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut stack: Vec<(usize, bool)> = Vec::new();
    for (idx, decl) in unit.decls.iter().enumerate() {
        if let CgDeclData::RawLine(text) = &decl.data {
            out.push_str(text);
            out.push('\n');
            continue;
        }
        let Some(&i) = def_of_decl.get(&idx) else { continue };
        if emitted.contains(&i) { continue; }

        // Depth-first over this definition's dependencies, each taken in source
        // order; `true` on the stack means "dependencies done, emit me now".
        stack.clear();
        stack.push((i, false));
        let mut pending: std::collections::HashSet<usize> = std::collections::HashSet::new();
        pending.insert(i);
        while let Some((j, deps_done)) = stack.pop() {
            if emitted.contains(&j) { continue; }
            if deps_done {
                emitted.insert(j);
                emit_agg_def(&defs[j], out);
                continue;
            }
            stack.push((j, true));
            let mut missing: Vec<usize> = defs.iter().enumerate()
                .filter(|(k, d)| {
                    !emitted.contains(k) && !pending.contains(k) && *k != j && deps[j].contains(&d.tag)
                })
                .map(|(k, _)| k)
                .collect();
            missing.sort_by_key(|k| defs[*k].decl_idx);
            for k in missing.into_iter().rev() {
                pending.insert(k);
                stack.push((k, false));
            }
        }
    }
    // Anything left is a reference cycle, which C cannot express either — emit
    // it so no definition is silently dropped.
    for i in 0..defs.len() {
        if !emitted.contains(&i) { emit_agg_def(&defs[i], out); }
    }

    true
}

/// Extract the canonical signature key from a block type string.
/// E.g. `void (^CollisionEventBlock)(int, float *)` → `void(int, float *)`.
/// Used for block typedef dedup: two block types with the same return/param
/// signature shouldn't emit duplicate full typedefs.
fn block_type_signature_key(s: &str) -> String {
    if let Some(hat_pos) = s.find("(^") {
        let ret_part = s[..hat_pos].trim();
        let after = &s[hat_pos + 2..];
        if let Some(close_paren) = after.find(")(") {
            let params_block = &after[close_paren + 2..];
            if let Some(end) = params_block.rfind(')') {
                return format!("{}({})", ret_part, &params_block[..end]);
            }
        }
    }
    s.to_string()
}

/// Sentinel rendered for `TypePrim::Param` (a generic type parameter like `T`).
/// The trailing comment marks the position so monomorphization can replace
/// exactly the T positions instead of blindly replacing every `NPObject *`
/// (which corrupted real `id` positions like `-copy`/`-description` in
/// specialized copies). If a sentinel ever leaks into output it is legal C.
const T_PARAM_RENDER: &str = "NPObject * /*T*/";

/// Render a parameter's name plus any `__attribute__((...))` suffixes, e.g.
/// `x __attribute__((unused))`. The attrs are appended to the name so
/// `format_param_decl` emits `int x __attribute__((unused))`.
fn param_attr_decl(param: &CstParam) -> String {
    let mut n = param.name.clone().unwrap_or_default();
    for a in &param.attributes {
        let _ = write!(n, " __attribute__(({}))", a);
    }
    n
}

/// Format a parameter declaration, handling block types specially.
/// Block types like `void (^)(char)` with name `resultBlock` must be emitted
/// as `void (^resultBlock)(char)`, not `void (^)(char) resultBlock`.
fn format_param_decl(pt: &str, pn: &str) -> String {
    // Check if this is a block type: contains `(^)`
    if let Some(block_pos) = pt.find("(^)") {
        let before = &pt[..block_pos];
        let after = &pt[block_pos + 3..];
        format!("{}(^{}){}", before, pn, after)
    } else if let Some(block_pos) = pt.find("(^") {
        let before = &pt[..block_pos + 2];
        let after = &pt[block_pos + 2..];
        format!("{}{}{}", before, pn, after)
    } else if let Some(fn_pos) = pt.find("(*") {
        // Function-pointer declarator: same splice rationale as the block
        // branch — the declarator name lives INSIDE the parens (`int (*cb)(int)`),
        // never after the type. The parser now carries the name in CstParam.name
        // (adopted from block_name so the checker binds it); without this branch
        // the name was appended after the type: `int (*)(int) cb` — invalid C.
        let before = &pt[..fn_pos + 2];
        let after = &pt[fn_pos + 2..];
        format!("{}{}{}", before, pn, after)
    } else {
        let (base, arr_suffix) = split_array_type(pt);
        format!("{} {}{}", base, pn, arr_suffix)
    }
}

/// Rewrite a cloned CgStmt tree for a monomorphized generic class.
///
/// When `Box<T>` is specialized to `Box<Node*>`, the cloned method body still
/// refers to the generic template: `struct Box * _self = (struct Box *)self;`
/// and `T item` (rendered as `NPObject * item`). This walks the CgStmt/CgExpr
/// tree and replaces:
///   * `struct {base_flat} *` → `struct {mangled_flat} *`   (the `_self` cast)
///   * `{t_render}` → `{concrete_str}`                       (T → concrete)
///
/// `t_render` is the C string used for `TypePrim::Param` (typically `NPObject *`);
/// `concrete_str` is the rendered concrete type (e.g. `Node *`).
fn substitute_cg_stmt(
    s: &mut CgStmt,
    base_flat: &str,
    mangled_flat: &str,
    t_render: &str,
    concrete_str: &str,
) {
    match &mut s.data {
        CgStmtData::Expr(e) => substitute_cg_expr(e, base_flat, mangled_flat, t_render, concrete_str),
        CgStmtData::Compound(stmts) => {
            for st in stmts.iter_mut() {
                substitute_cg_stmt(st, base_flat, mangled_flat, t_render, concrete_str);
            }
        }
        CgStmtData::If { cond, then, else_ } => {
            substitute_cg_expr(cond, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_stmt(then, base_flat, mangled_flat, t_render, concrete_str);
            if let Some(eb) = else_ {
                substitute_cg_stmt(eb, base_flat, mangled_flat, t_render, concrete_str);
            }
        }
        CgStmtData::While { cond, body } => {
            substitute_cg_expr(cond, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_stmt(body, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgStmtData::Do { body, cond } => {
            substitute_cg_stmt(body, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_expr(cond, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgStmtData::For { init, cond, incr, body } => {
            if let Some(i) = init {
                substitute_cg_stmt(i, base_flat, mangled_flat, t_render, concrete_str);
            }
            if let Some(c) = cond {
                substitute_cg_expr(c, base_flat, mangled_flat, t_render, concrete_str);
            }
            if let Some(u) = incr {
                substitute_cg_expr(u, base_flat, mangled_flat, t_render, concrete_str);
            }
            substitute_cg_stmt(body, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgStmtData::Return(v) => {
            if let Some(e) = v {
                substitute_cg_expr(e, base_flat, mangled_flat, t_render, concrete_str);
            }
        }
        CgStmtData::Switch { expr, body } => {
            substitute_cg_expr(expr, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_stmt(body, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgStmtData::Case { value, body } => {
            substitute_cg_expr(value, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_stmt(body, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgStmtData::Default(b) => {
            substitute_cg_stmt(b, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgStmtData::Decl {
            decl_type,
            init,
            next,
            ..
        } => {
            rewrite_type_str(decl_type, base_flat, mangled_flat, t_render, concrete_str);
            if let Some(init_expr) = init {
                substitute_cg_expr(init_expr, base_flat, mangled_flat, t_render, concrete_str);
            }
            for (_, _, init_opt) in next.iter_mut() {
                if let Some(e) = init_opt {
                    substitute_cg_expr(e, base_flat, mangled_flat, t_render, concrete_str);
                }
            }
        }
        CgStmtData::ForIn { collection, body, .. } => {
            substitute_cg_expr(collection, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_stmt(body, base_flat, mangled_flat, t_render, concrete_str);
        }
        _ => {}
    }
}

fn substitute_cg_expr(
    e: &mut CgExpr,
    base_flat: &str,
    mangled_flat: &str,
    t_render: &str,
    concrete_str: &str,
) {
    match &mut e.data {
        CgExprData::Cast { target_type, expr } => {
            rewrite_type_str(target_type, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_expr(expr, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Unary { operand, .. } => {
            substitute_cg_expr(operand, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Binary { left, right, .. } => {
            substitute_cg_expr(left, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_expr(right, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Assign { target, value } => {
            substitute_cg_expr(target, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_expr(value, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Call {
            args,
            vtable_class,
            alt_vtable_classes,
            ..
        } => {
            for a in args.iter_mut() {
                substitute_cg_expr(a, base_flat, mangled_flat, t_render, concrete_str);
            }
            // Specialized calls should dispatch through the specialized vtable.
            if let Some(vc) = vtable_class {
                if name_flat(vc) == base_flat {
                    *vc = mangled_flat.to_string();
                }
            }
            for alt in alt_vtable_classes.iter_mut() {
                if name_flat(alt) == base_flat {
                    *alt = mangled_flat.to_string();
                }
            }
        }
        CgExprData::Comma(exprs) => {
            for x in exprs.iter_mut() {
                substitute_cg_expr(x, base_flat, mangled_flat, t_render, concrete_str);
            }
        }
        CgExprData::Paren(e) => {
            substitute_cg_expr(e, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Member { obj, .. } => {
            substitute_cg_expr(obj, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Arrow { obj, .. } => {
            substitute_cg_expr(obj, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Index { arr, index } => {
            substitute_cg_expr(arr, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_expr(index, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::Ternary { cond, then, else_ } => {
            substitute_cg_expr(cond, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_expr(then, base_flat, mangled_flat, t_render, concrete_str);
            substitute_cg_expr(else_, base_flat, mangled_flat, t_render, concrete_str);
        }
        CgExprData::InitList(exprs) => {
            for x in exprs.iter_mut() {
                substitute_cg_expr(x, base_flat, mangled_flat, t_render, concrete_str);
            }
        }
        _ => {}
    }
}

/// Rewrite a C type string in place for a monomorphized generic class.
///
/// Replaces `struct {base_flat} *` → `struct {mangled_flat} *` and
/// `{t_render}` → `{concrete_str}`. The struct replacement only fires when
/// the base flat name appears as a `struct Name *` pattern, so unrelated
/// types containing the base name as a substring are left alone.
fn rewrite_type_str(
    s: &mut String,
    base_flat: &str,
    mangled_flat: &str,
    t_render: &str,
    concrete_str: &str,
) {
    if s.is_empty() {
        return;
    }
    // T → concrete (T renders with the `/*T*/` sentinel; see T_PARAM_RENDER).
    // Matches both the sentinel form and, defensively, a bare `NPObject *`
    // string that came from an older path without the marker.
    if !t_render.is_empty() && s.contains(t_render) {
        *s = s.replace(t_render, concrete_str);
    }
    // `struct {base_flat} *` → `struct {mangled_flat} *`
    let needle = format!("struct {} *", base_flat);
    if s.contains(&needle) {
        let repl = format!("struct {} *", mangled_flat);
        *s = s.replace(&needle, &repl);
    }
}

/// Back-compat wrapper: no slots manifest → sorted-alphabetical layout
/// (the historical behavior, guarded at startup by the __sig check).
pub fn ast_to_cg_unit(ast: &AstUnit, backend: Backend) -> CgUnit {
    ast_to_cg_unit_with_slots(ast, backend, None)
}

pub fn ast_to_cg_unit_with_slots(ast: &AstUnit, backend: Backend, slots_manifest: Option<&[String]>) -> CgUnit {
    ast_to_cg_unit_with_slots_ext(ast, backend, slots_manifest, None, None)
}

/// Like [`ast_to_cg_unit_with_slots`], but also told which instance-method
/// selectors are **public** for this TU — i.e. declared in an `@interface` from
/// a file it imports.
///
/// The uniform vtable then lays out the public segment first (alphabetical, and
/// therefore identical in every TU that shares the same declarations) and
/// appends the methods only this TU knows about after it. That keeps every
/// public method's slot index stable across TUs even when their method sets
/// differ, which is what lets a legal two-TU split — a library plus a client
/// with its own classes — link and dispatch correctly instead of tripping the
/// `__sig` guard.
pub fn ast_to_cg_unit_with_slots_ext(
    ast: &AstUnit,
    backend: Backend,
    slots_manifest: Option<&[String]>,
    public_methods: Option<&std::collections::HashSet<String>>,
    forced_generic_instantiations: Option<&[String]>,
) -> CgUnit {
    CURRENT_BACKEND.store(backend as u8, Ordering::Relaxed);
    *block_vars() = Some(std::collections::HashSet::new());
    *block_defs() = String::new();  // reset block-expansion buffer
    *resp_helpers() = None;         // reset respondsToSelector: helper set
    *emitted_methods() = Some(std::collections::HashSet::new());
    let mut selectors = Vec::new();
    let mut classes: Vec<CgClassMeta> = Vec::new();
    let mut decls: Vec<CgDecl> = Vec::new();

    fn add_sel(selectors: &mut Vec<String>, sel: &str) {
        let sn = sel_const_name(sel);
        if !selectors.iter().any(|s| sel_const_name(s) == sn) {
            selectors.push(sel.to_string());
        }
    }

    fn collect_sel_expr(e: &AstExpr, sels: &mut Vec<String>) {
        if let AstExprData::Selector(s) = &e.data {
            add_sel(sels, s);
        }
        match &e.data {
            AstExprData::FuncCall { name, args, .. } => {
                for a in args { collect_sel_expr(a, sels); }
                // NPLog(@"...%@...") expands each %@ into a `-description` +
                // `-UTF8String` message send at codegen time. Collect those two
                // selectors so the SEL constants are emitted.
                if name == "NPLog" {
                    if let Some(AstExprData::AtString(fmt)) = args.first().map(|a| &a.data) {
                        if fmt.contains("%@") {
                            add_sel(sels, "description");
                            add_sel(sels, "UTF8String");
                        }
                    }
                }
            }
            AstExprData::MsgSend { receiver, args, .. } => {
                collect_sel_expr(receiver, sels);
                for a in args { collect_sel_expr(a, sels); }
            }
            AstExprData::IvarRef { obj, .. } => collect_sel_expr(obj, sels),
            AstExprData::PropRef { obj, .. } => collect_sel_expr(obj, sels),
            AstExprData::Unary { operand, .. } => collect_sel_expr(operand, sels),
            AstExprData::Binary { left, right, .. } => { collect_sel_expr(left, sels); collect_sel_expr(right, sels); }
            AstExprData::Assign { target, value, .. } => { collect_sel_expr(target, sels); collect_sel_expr(value, sels); }
            AstExprData::Cast { expr, .. } => collect_sel_expr(expr, sels),
            AstExprData::Ternary { cond, then, else_, .. } => { collect_sel_expr(cond, sels); collect_sel_expr(then, sels); collect_sel_expr(else_, sels); }
            AstExprData::Subscript { object, key } => { collect_sel_expr(object, sels); collect_sel_expr(key, sels); }
            AstExprData::Comma(exprs) => { for ex in exprs { collect_sel_expr(ex, sels); } }
            AstExprData::ArrayLit(elements) => { for el in elements { collect_sel_expr(el, sels); } }
            AstExprData::InitList(elements) => { for el in elements { collect_sel_expr(el, sels); } }
            _ => {}
        }
    }

    fn collect_sel_stmt(s: &AstStmt, sels: &mut Vec<String>) {
        match &s.data {
            AstStmtData::Expr(e) => collect_sel_expr(e, sels),
            AstStmtData::Decl(d) => {
                if let AstDeclData::Variable { init, .. } = &d.data {
                    if let Some(i) = init { collect_sel_expr(i, sels); }
                }
            }
            AstStmtData::If { cond, then, else_, .. } => {
                collect_sel_expr(cond, sels);
                collect_sel_stmt(then, sels);
                if let Some(el) = else_ { collect_sel_stmt(el, sels); }
            }
            AstStmtData::While { cond, body, .. } | AstStmtData::Do { body, cond } => {
                collect_sel_expr(cond, sels);
                collect_sel_stmt(body, sels);
            }
            AstStmtData::For { init, cond, incr, body, .. } => {
                if let Some(i) = init { collect_sel_stmt(i, sels); }
                if let Some(c) = cond { collect_sel_expr(c, sels); }
                if let Some(u) = incr { collect_sel_expr(u, sels); }
                collect_sel_stmt(body, sels);
            }
            AstStmtData::ForIn { collection, body, .. } => { collect_sel_expr(collection, sels); collect_sel_stmt(body, sels); }
            AstStmtData::Switch { expr, body } => {
                collect_sel_expr(expr, sels);
                collect_sel_stmt(body, sels);
            }
            AstStmtData::Case { value, body } => { collect_sel_expr(value, sels); collect_sel_stmt(body, sels); }
            AstStmtData::Default(body) => { collect_sel_stmt(body, sels); }
            AstStmtData::Compound(stmts) => { for st in stmts { collect_sel_stmt(st, sels); } }
            AstStmtData::Autoreleasepool(body) => { collect_sel_stmt(body, sels); }
            AstStmtData::NoArc(body) => { collect_sel_stmt(body, sels); }
            AstStmtData::Try { try_block, catches, finally_block } => {
                collect_sel_stmt(try_block, sels);
                for c in catches { collect_sel_stmt(c, sels); }
                if let Some(f) = finally_block { collect_sel_stmt(f, sels); }
            }
            AstStmtData::Throw(e) => { if let Some(ex) = e { collect_sel_expr(ex, sels); } }
            AstStmtData::Return(e) => { if let Some(ex) = e { collect_sel_expr(ex, sels); } }
            AstStmtData::Synchronized { lock, body } => { collect_sel_expr(lock, sels); collect_sel_stmt(body, sels); }
            _ => {}
        }
    }

    // Pre‑pass: collect block typedef short→flat name mappings so that ivar type
    // resolution (which happens before Typedef conversion) can use flat names.
    {
        let mut flat_decls: Vec<&AstDecl> = Vec::new();
        for d in &ast.decls { flatten_namespace_decls(d, &mut flat_decls); }
        for d in flat_decls {
            if d.kind == AstDeclKind::Typedef {
                if let AstDeclData::Typedef { aliased_type, .. } = &d.data {
                    if let Some(ref at) = aliased_type {
                        // Register the declarator name for flat-name resolution
                        // (blocks AND C fnptr typedefs both embed the name in
                        // the type). fnptr typedefs are also tracked separately
                        // so they are never mistaken for block variables.
                        if let Some(ref bn) = at.block_name {
                            let flat = name_flat(d.name.as_deref().unwrap_or(""));
                            if let Ok(mut guard) = BLOCK_TYPEDEF_NAMES.get_or_init(|| Mutex::new(HashMap::new())).lock() {
                                guard.insert(bn.clone(), flat);
                            }
                            if at.is_fn_ptr {
                                if let Ok(mut guard) = FNPTR_TYPEDEF_NAMES.get_or_init(|| Mutex::new(std::collections::HashSet::new())).lock() {
                                    guard.insert(bn.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // First pass: collect interface metadata (method signatures)
    use std::collections::BTreeMap;
    let mut class_infos: BTreeMap<String, ClassInfo> = BTreeMap::new();

    // Pre‑pass: register all class entries + method names so that
    // vtable dispatch resolution in method‑body conversion can find
    // protocol methods implemented in classes defined later in source
    // order (e.g. NPCollectionInspector implements collectionDidReachCapacity:
    // but NPDataContainer's method body that calls it is converted first).
    for d in &ast.decls {
        if d.kind != AstDeclKind::Class { continue; }
        let cls_name = d.name.clone().unwrap_or_default();
        let flat = name_flat(&cls_name);
        let super_name = match &d.data {
            AstDeclData::Class { super_name, .. } => super_name.clone(),
            _ => None,
        };
        class_infos.entry(flat.clone()).or_insert(ClassInfo {
            class_name: cls_name,
            flat: flat.clone(),
            super_name,
            type_params: match &d.data {
                AstDeclData::Class { type_params, .. } => type_params.clone(),
                _ => Vec::new(),
            },
            method_names: Vec::new(),
            method_sel_names: Vec::new(),
            is_class_methods: Vec::new(),
            method_bodies: Vec::new(),
            method_return_types: Vec::new(),
            method_params_list: Vec::new(),
            method_variadic: Vec::new(),
            method_owners: Vec::new(),
            category_method_names: std::collections::HashSet::new(),
            has_category_impl: false,
            protocols: Vec::new(),
            ivar_types: Vec::new(),
            ivar_names: Vec::new(),
            ivar_weak: Vec::new(),
            has_impl_decl: false,
        });
        if let AstDeclData::Class { methods: ref class_methods, is_category, .. } = &d.data {
            for m in class_methods {
                if let Some(ref mname) = m.name {
                    let sanitized = sanitize_sel_name(mname);
                    if let Some(info) = class_infos.get_mut(&flat) {
                        if !info.method_names.contains(&sanitized) {
                            let (is_class, has_variadic_m) = match &m.data {
                                AstDeclData::Method { is_class_method, has_variadic, .. } => (*is_class_method, *has_variadic),
                                _ => (false, false),
                            };
                            info.method_names.push(sanitized.clone());
                            info.method_sel_names.push(mname.clone());
                            info.is_class_methods.push(is_class);
                            info.method_bodies.push(None);
                            info.method_return_types.push(String::new());
                            info.method_params_list.push(Vec::new());
                            info.method_variadic.push(has_variadic_m);
                            info.method_owners.push(flat.clone());
                            if *is_category { info.category_method_names.insert(sanitized.clone()); }
                        }
                    }
                }
            }
        }
    }

    for d in &ast.decls {
        if d.kind != AstDeclKind::Class { continue; }
        let cls_name = d.name.clone().unwrap_or_default();
        let flat = name_flat(&cls_name);
        let cls_name_for_synth = cls_name.clone();
        let super_name = match &d.data {
            AstDeclData::Class { super_name, .. } => super_name.clone(),
            _ => None,
        };
        class_infos.entry(flat.clone()).or_insert(ClassInfo {
            class_name: cls_name,
            flat: flat.clone(),
            super_name,
            type_params: match &d.data {
                AstDeclData::Class { type_params, .. } => type_params.clone(),
                _ => Vec::new(),
            },
            method_names: Vec::new(),
            method_sel_names: Vec::new(),
            is_class_methods: Vec::new(),
            method_bodies: Vec::new(),
            method_return_types: Vec::new(),
            method_params_list: Vec::new(),
            method_variadic: Vec::new(),
            method_owners: Vec::new(),
            category_method_names: std::collections::HashSet::new(),
            has_category_impl: false,
            protocols: Vec::new(),
            ivar_types: Vec::new(),
            ivar_names: Vec::new(),
            ivar_weak: Vec::new(),
            has_impl_decl: false,
        });

        if let AstDeclData::Class { methods: ref class_methods, ivars: ref class_ivars, properties: ref class_properties, is_implementation, protocols: ref ast_protocols, type_params: ref tp, .. } = &d.data {
            if !ast_protocols.is_empty() {
                if let Some(info) = class_infos.get_mut(&flat) {
                    for p in ast_protocols {
                        // Generic type params (`NPArray<T>` under the umbrella)
                        // arrive in the same slot as protocol names in some
                        // spellings; they are not protocols — emitting
                        // &JETI_PROTO_$_T would be a dangling reference.
                        if tp.contains(p) { continue; }
                        if !info.protocols.contains(p) { info.protocols.push(p.clone()); }
                    }
                }
            }
            if *is_implementation {
                if let Some(info) = class_infos.get_mut(&flat) {
                    info.has_impl_decl = true;
                }
            }
            if let AstDeclData::Class { is_category: true, .. } = &d.data {
                if *is_implementation {
                    if let Some(info) = class_infos.get_mut(&flat) { info.has_category_impl = true; }
                }
            }
            let mut ivar_types = Vec::new();
            let mut ivar_names = Vec::new();
            let mut ivar_weak = Vec::new();

            for iv in class_ivars {
                if let AstDeclData::Ivar { ivar_type, is_weak, .. } = &iv.data {
                    let mut it = ivar_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "int".into());
                    let in_ = iv.name.clone().unwrap_or_default();
                    // Resolve block typedef short names to namespace-prefixed flat names
                    if let Ok(guard) = BLOCK_TYPEDEF_NAMES.get_or_init(|| Mutex::new(HashMap::new())).lock() {
                        if let Some(flat) = guard.get(&it) {
                            it = flat.clone();
                        }
                    }
                    ivar_types.push(it);
                    ivar_names.push(in_);
                    ivar_weak.push(*is_weak);
                }
            }

            // Store ivar data in class_infos. MERGE with any existing ivars
            // (e.g. main @interface declares `_sequencerId`, then a category
            // `@property` synthesizes `_neuralSyncRate`). The previous
            // `is_empty()` guard dropped the main-class ivars when the category
            // synthesized its own, producing a struct missing the main ivars.
            if let Some(info) = class_infos.get_mut(&flat) {
                for (idx, n) in ivar_names.iter().enumerate() {
                    if !info.ivar_names.contains(n) {
                        info.ivar_names.push(n.clone());
                        info.ivar_types.push(ivar_types[idx].clone());
                        info.ivar_weak.push(ivar_weak[idx]);
                    }
                }
            }

            // Pending raw pass-through lines (e.g. `#pragma mark`) seen since
            // the previous method. They are flushed right before the next
            // method's declaration so markers keep their source grouping.
            let mut pending_pragmas: Vec<String> = Vec::new();
            for m in class_methods {
                // Raw pass-through line (e.g. `#pragma mark`) between methods:
                // defer it and attach to the next method.
                if let AstDeclData::RawLine(text) = &m.data {
                    pending_pragmas.push(text.clone());
                    continue;
                }
                if let Some(ref mname) = m.name {
                    let sel = mname.clone();
                    add_sel(&mut selectors, &sel);

                    let ret_type = match &m.data {
                        AstDeclData::Method { return_type, .. } =>
                            return_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "NPObject *".into()),
                        _ => "NPObject *".into(),
                    };
                    let is_class = match &m.data {
                        AstDeclData::Method { is_class_method, .. } => *is_class_method,
                        _ => false,
                    };
                    let is_variadic_m = match &m.data {
                        AstDeclData::Method { has_variadic, .. } => *has_variadic,
                        _ => false,
                    };

                    let fn_name = format!("{}_{}", flat, sanitize_sel_name(&sel));

                    // Flush any deferred `#pragma mark` markers so they appear
                    // immediately before this method's declaration.
                    if !pending_pragmas.is_empty() {
                        let raw: Vec<CgDecl> = pending_pragmas.drain(..).map(|t| CgDecl {
                            kind: CgDeclKind::RawLine, name: String::new(),
                            data: CgDeclData::RawLine(t), attributes: Vec::new(),
                        }).collect();
                        if let Some(pos) = decls.iter().position(|d| d.name == fn_name) {
                            for (k, r) in raw.into_iter().enumerate() {
                                decls.insert(pos + k, r);
                            }
                        } else {
                            decls.extend(raw);
                        }
                    }

                    let mut fn_params = Vec::new();
                    let self_type = if is_class { "NPClass *" } else { "NPObject *" };
                    fn_params.push((self_type.to_string(), "self".to_string()));
                    fn_params.push(("SEL".to_string(), "_cmd".to_string()));

                    if let AstDeclData::Method { params: ref method_params, .. } = m.data {
                        let mut p = method_params.as_ref().map(|b| &**b);
                        while let Some(param) = p {
                            if let Some(ref pt) = param.par_type {
                                fn_params.push((cst_type_to_c_str(pt), param.name.clone().unwrap_or_else(|| "_arg".into())));
                            }
                            p = param.next.as_ref().map(|n| &**n);
                        }
                    }

                    let body = {
                    let old_rt = CURRENT_RETURN_TYPE.lock().unwrap().take();
                    *CURRENT_RETURN_TYPE.lock().unwrap() = Some(ret_type.clone());
                    let result = match &m.data {
                        AstDeclData::Method { body, .. } => {
                            body.as_ref().map(|b| {
                                collect_sel_stmt(b, &mut selectors);
                                let cg_body = convert_stmt(b, &class_infos);
                                if !is_class {
                                    // No _self declaration — ivar access uses inline cast
                                    // ((struct Cls *)self)->ivar so self reassignment
                                    // (e.g. self = [super init]) never becomes stale.
                                    cg_body
                                } else {
                                    cg_body
                                }
                            })
                        }
                        _ => None,
                    };
                    *CURRENT_RETURN_TYPE.lock().unwrap() = old_rt;
                    result
                };

                    let info = class_infos.get_mut(&flat).unwrap();
                    if let Some(idx) = info.method_names.iter().position(|n| *n == sanitize_sel_name(&sel)) {

                        // Method name registered (possibly by pre‑pass or an earlier @interface).
                        if let Some(body_val) = body {
                            // @implementation provides a body
                            if let Some(existing) = decls.iter_mut().find(|d| d.name == fn_name) {
                                match existing.data {
                                    CgDeclData::Function { ref mut body, ref mut params, .. } => {
                                        *body = Some(Box::new(body_val.clone()));
                                        // Also update method_bodies for generic instantiation cloning
                                        if idx < info.method_bodies.len() {
                                            info.method_bodies[idx] = Some(Box::new(body_val.clone()));
                                        }
                                        // Update return type & params in class_infos (pre‑pass may
                                        // have left empty placeholders that vtable struct emission reads).
                                        if idx < info.method_return_types.len() {
                                            info.method_return_types[idx] = ret_type.clone();
                                        }
                                        if idx < info.method_params_list.len() {
                                            info.method_params_list[idx] = fn_params.clone();
                                        }
                                        // Update parameter names from the @implementation method
                                        for i in 2..params.len().min(fn_params.len()) {
                                            if fn_params[i].1 != "value" && fn_params[i].1 != "_arg" && !fn_params[i].1.is_empty() {
                                                params[i].1 = fn_params[i].1.clone();
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                // Method was registered in pre‑pass but no CgDecl exists yet
                                // (e.g. @interface came after pre‑pass but before @implementation,
                                //  or the @interface had no body and the @implementation is first).
                                // Create it now with the body.
                                if idx < info.method_bodies.len() {
                                    info.method_bodies[idx] = Some(Box::new(body_val.clone()));
                                }
                                if idx < info.method_return_types.len() {
                                    info.method_return_types[idx] = ret_type.clone();
                                }
                                if idx < info.method_params_list.len() {
                                    info.method_params_list[idx] = fn_params.clone();
                                }
                                decls.push(CgDecl {
                                    kind: CgDeclKind::Function, name: fn_name,
                                    data: CgDeclData::Function {
                                        return_type: ret_type, params: fn_params,
                                        is_variadic: is_variadic_m, is_objc_class: true,
                                        body: Some(Box::new(body_val.clone())),
                                    },
                                                                    attributes: Vec::new(),
});
                            }
                        } else if !decls.iter().any(|d| d.name == fn_name) {
                            // @interface method with no body → create forward declaration
                            // so that the function exists even without @implementation.
                            if idx < info.method_return_types.len() {
                                info.method_return_types[idx] = ret_type.clone();
                            }
                            if idx < info.method_params_list.len() {
                                info.method_params_list[idx] = fn_params.clone();
                            }
                            decls.push(CgDecl {
                                kind: CgDeclKind::Function, name: fn_name,
                                data: CgDeclData::Function {
                                    return_type: ret_type, params: fn_params,
                                    is_variadic: is_variadic_m, is_objc_class: true,
                                    body: None,
                                },
                                                            attributes: Vec::new(),
});
                        }
                    } else {
                        info.method_names.push(sanitize_sel_name(&sel));
                        info.method_sel_names.push(sel.clone());
                        info.is_class_methods.push(is_class);
                        info.method_bodies.push(body.as_ref().map(|b| Box::new(b.clone())));
                        info.method_return_types.push(ret_type.clone());
                        info.method_params_list.push(fn_params.clone());
                        info.method_variadic.push(is_variadic_m);
                        info.method_owners.push(flat.clone());
                        
                        decls.push(CgDecl {
                            kind: CgDeclKind::Function, name: fn_name,
                            data: CgDeclData::Function {
                                return_type: ret_type, params: fn_params,
                                is_variadic: is_variadic_m, is_objc_class: true,
                                body: body.map(Box::new),
                            },
                                                    attributes: Vec::new(),
});
                    }
                }
            }

            for prop in class_properties {
                if let AstDeclData::Property { prop_type, is_readonly, is_dynamic, is_weak, .. } = &prop.data {
                    let prop_name = prop.name.clone().unwrap_or_default();
                    let ivar_name = format!("_{}", prop_name);
                    let getter_name = prop_name.clone();
                    let setter_name = format!("set{}{}:", 
                        getter_name[0..1].to_uppercase(),
                        &getter_name[1..]);

                    let it = prop_type.as_ref().map(|t| ast_type_to_c_str(t)).unwrap_or_else(|| "int".into());
                    if !ivar_names.contains(&ivar_name) {
                        ivar_types.push(it.clone());
                        ivar_names.push(ivar_name.clone());
                        // Keep the three vectors parallel: the merge pass below
                        // indexes ivar_weak by position, so a missing entry made
                        // every subsequently checked ivar look non-weak.
                        ivar_weak.push(*is_weak);
                    }

                    // Skip getter/setter generation for array properties (C cannot return arrays from functions)
                    let is_array_prop = prop_type.as_ref().map_or(false, |t| t.is_array);
                    if is_array_prop { continue; }

                    // @dynamic: getter/setter provided by @implementation, skip synthesis
                    if *is_dynamic { continue; }

                    let getter_sel = getter_name.replace(':', "_");
                    let getter_fn_name = format!("{}_{}", flat, getter_sel);
                    add_sel(&mut selectors, &getter_sel);
                    
                    let getter_return_expr = AstExpr {
                        kind: AstExprKind::IvarRef,
                        expr_type: prop_type.clone(),
                        line: 0, col: 0, end_line: 0, end_col: 0,
                        data: AstExprData::IvarRef {
                            ivar: Some(ivar_name.clone()),
                            cls: Some(cls_name_for_synth.clone()),
                            obj: Box::new(AstExpr {
                                kind: AstExprKind::Cast,
                                expr_type: None,
                                line: 0, col: 0, end_line: 0, end_col: 0,
                                data: AstExprData::Cast {
                                    target_type: AstType {
                                        prim: jeti_cst::TypePrim::Named,
                                        is_pointer: true,
                                        is_struct: true,
                                        name: Some(flat.clone()),
                                        ..AstType::new(jeti_cst::TypePrim::Named)
                                    },
                                    expr: Box::new(AstExpr {
                                        kind: AstExprKind::Self_,
                                        expr_type: None,
                                        line: 0, col: 0,
                                        data: AstExprData::VarRef { sym: None, name: "self".into() },
                                        end_line: 0,
                                        end_col: 0,
                                    }),
                                },
                            }),
                        },
                    };
                    let getter_body = AstStmt {
                        kind: AstStmtKind::Compound,
                        line: 0, col: 0,
                        data: AstStmtData::Compound(vec![
                            AstStmt {
                                kind: AstStmtKind::Return,
                                line: 0, col: 0,
                                data: AstStmtData::Return(Some(Box::new(getter_return_expr))),
                                end_line: 0,
                                end_col: 0,
                            }
                        ]),
                        end_line: 0,
                        end_col: 0,
                    };

                    let getter_params = vec![("NPObject *".to_string(), "self".to_string()), ("SEL".to_string(), "_cmd".to_string())];
                    let getter_params_clone = getter_params.clone();
                    // Only create CgDecl if it doesn't already exist (e.g. pre‑pass or @implementation registered it).
                    if decls.iter().any(|d| d.name == getter_fn_name) {
                        // Update return type and params in class_infos (pre‑pass may have placeholders).
                        let info = class_infos.get_mut(&flat).unwrap();
                        if let Some(gidx) = info.method_names.iter().position(|n| *n == getter_sel) {
                            if gidx < info.method_return_types.len() {
                                info.method_return_types[gidx] = it.clone();
                            }
                            if gidx < info.method_params_list.len() {
                                info.method_params_list[gidx] = getter_params_clone;
                            }
                        }
                    } else {
                        decls.push(CgDecl {
                            kind: CgDeclKind::Function, name: getter_fn_name.clone(),
                            data: CgDeclData::Function {
                                return_type: it.clone(), params: getter_params,
                                is_variadic: false, is_objc_class: true,
                                body: Some(Box::new(convert_stmt(&getter_body, &class_infos))),
                            },
                                                    attributes: Vec::new(),
});
                        let info = class_infos.get_mut(&flat).unwrap();
                        if !info.method_names.contains(&getter_sel) {
                            info.method_names.push(getter_sel.clone());
                            info.method_sel_names.push(getter_name);
                            info.is_class_methods.push(false);
                            info.method_return_types.push(it.clone());
                            info.method_params_list.push(getter_params_clone);
                            info.method_owners.push(flat.clone());
                                                    }
                    }

                    if !*is_readonly {
                        let setter_sel = setter_name.replace(':', "_");
                        let setter_fn_name = format!("{}_{}", flat, setter_sel);
                        add_sel(&mut selectors, &setter_sel);

                        let setter_assign_expr = AstExpr {
                            kind: AstExprKind::Assign,
                            expr_type: None,
                            line: 0, col: 0, end_line: 0, end_col: 0,
                            data: AstExprData::Assign {
                                target: Box::new(AstExpr {
                                    kind: AstExprKind::IvarRef,
                                    expr_type: None,
                                    line: 0, col: 0, end_line: 0, end_col: 0,
                                data: AstExprData::IvarRef {
                                    ivar: Some(ivar_name.clone()),
                                    cls: Some(cls_name_for_synth.clone()),
                                    obj: Box::new(AstExpr {
                                        kind: AstExprKind::Cast,
                                        expr_type: None,
                                        line: 0, col: 0, end_line: 0, end_col: 0,
                                        data: AstExprData::Cast {
                                            target_type: AstType {
                                                prim: jeti_cst::TypePrim::Named,
                                                is_pointer: true,
                                                is_struct: true,
                                                name: Some(flat.clone()),
                                                ..AstType::new(jeti_cst::TypePrim::Named)
                                            },
                                            expr: Box::new(AstExpr {
                                                kind: AstExprKind::Self_,
                                                expr_type: None,
                                                line: 0, col: 0,
                                                data: AstExprData::VarRef { sym: None, name: "self".into() },
                                                end_line: 0,
                                                end_col: 0,
                                            }),
                                        },
                                    }),
                                },
                                }),
                                value: Box::new(AstExpr {
                                    kind: AstExprKind::VarRef,
                                    expr_type: None,
                                    line: 0, col: 0,
                                    data: AstExprData::VarRef { sym: None, name: "value".into() },
                                    end_line: 0,
                                    end_col: 0,
                                }),
                            },
                        };
                        let setter_body = AstStmt {
                            kind: AstStmtKind::Compound,
                            line: 0, col: 0,
                            data: AstStmtData::Compound(vec![
                                AstStmt {
                                    kind: AstStmtKind::Expr,
                                    line: 0, col: 0,
                                    data: AstStmtData::Expr(setter_assign_expr),
                                    end_line: 0,
                                    end_col: 0,
                                }
                            ]),
                            end_line: 0,
                            end_col: 0,
                        };

                        let mut setter_params = vec![("NPObject *".to_string(), "self".to_string()), ("SEL".to_string(), "_cmd".to_string())];
                        // Use the original parameter name from the @implementation method if available
                        let existing_param_name = decls.iter()
                            .find(|d| d.name == setter_fn_name)
                            .and_then(|d| {
                                if let CgDeclData::Function { ref params, .. } = d.data {
                                    params.get(2).map(|(_, n)| n.clone())
                                } else { None }
                            })
                            .unwrap_or_else(|| "value".to_string());
                        setter_params.push((it, existing_param_name.clone()));
                        let setter_params_clone = setter_params.clone();
                        // If the method already exists in decls (from @implementation), skip adding a new declaration
                        // Just update the vtable info
                        if decls.iter().any(|d| d.name == setter_fn_name) {
                            let info = class_infos.get_mut(&flat).unwrap();
                            if !info.method_names.contains(&setter_sel) {
                                info.method_names.push(setter_sel.clone());
                                info.method_sel_names.push(setter_name);
                                info.is_class_methods.push(false);
                                info.method_return_types.push("void".to_string());
                                info.method_params_list.push(setter_params_clone);
                                info.method_owners.push(flat.clone());
                                                            }
                            // Also fix the existing declaration's parameter name if it's still "value"
                            if let Some(existing) = decls.iter_mut().find(|d| d.name == setter_fn_name) {
                                if let CgDeclData::Function { ref mut params, .. } = existing.data {
                                    if params.len() > 2 && existing_param_name != "value" {
                                        params[2].1 = existing_param_name;
                                    }
                                }
                            }
                            continue;
                        }
                        if *is_weak {
                            let ivar_expr = CgExpr {
                                kind: CgExprKind::Arrow, type_str: None, line: 0, col: 0,
                                data: CgExprData::Arrow {
                                    obj: Box::new(CgExpr {
                                        kind: CgExprKind::Cast, type_str: None, line: 0, col: 0,
                                        data: CgExprData::Cast {
                                            target_type: format!("struct {} *", flat),
                                            expr: Box::new(CgExpr {
                                                kind: CgExprKind::Ident, type_str: None, line: 0, col: 0,
                                                data: CgExprData::Ident("self".into()),
                                            }),
                                        },
                                    }),
                                    field: ivar_name.clone(),
                                },
                            };
                            let addr_of_ivar = CgExpr {
                                kind: CgExprKind::Unary, type_str: None, line: 0, col: 0,
                                data: CgExprData::Unary {
                                    op_str: "&".into(), operand: Box::new(ivar_expr.clone()), is_postfix: false,
                                },
                            };
                            let cast_addr = CgExpr {
                                kind: CgExprKind::Cast, type_str: None, line: 0, col: 0,
                                data: CgExprData::Cast {
                                    target_type: "NPObject **".into(), expr: Box::new(addr_of_ivar),
                                },
                            };
                            let value_ident = CgExpr {
                                kind: CgExprKind::Ident, type_str: None, line: 0, col: 0,
                                data: CgExprData::Ident(existing_param_name.clone()),
                            };
                            let cast_value = CgExpr {
                                kind: CgExprKind::Cast, type_str: None, line: 0, col: 0,
                                data: CgExprData::Cast {
                                    target_type: "NPObject *".into(), expr: Box::new(value_ident),
                                },
                            };
                            let assign_expr = CgExpr {
                                kind: CgExprKind::Assign, type_str: None, line: 0, col: 0,
                                data: CgExprData::Assign {
                                    target: Box::new(ivar_expr),
                                    value: Box::new(CgExpr {
                                        kind: CgExprKind::Ident, type_str: None, line: 0, col: 0,
                                        data: CgExprData::Ident(existing_param_name.clone()),
                                    }),
                                },
                            };
                            decls.push(CgDecl {
                                kind: CgDeclKind::Function, name: setter_fn_name,
                                data: CgDeclData::Function {
                                    return_type: "void".to_string(), params: setter_params,
                                    is_variadic: false, is_objc_class: true,
                                    body: Some(Box::new(CgStmt {
                                        kind: CgStmtKind::Compound, line: 0, col: 0,
                                        data: CgStmtData::Compound(vec![
                                            CgStmt { kind: CgStmtKind::Expr, line: 0, col: 0, data: CgStmtData::Expr(CgExpr {
                                                kind: CgExprKind::Call, type_str: None, line: 0, col: 0,
                                                data: CgExprData::Call {
                                                    name: "jeti_weakUnregister".into(),
                                                    args: vec![cast_addr.clone()],
                                                    vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false, sel_const_name: None, method_index: None, direct_specialized: false,
                                                },
                                            })},
                                            CgStmt { kind: CgStmtKind::Expr, line: 0, col: 0, data: CgStmtData::Expr(assign_expr.clone()) },
                                            CgStmt { kind: CgStmtKind::Expr, line: 0, col: 0, data: CgStmtData::Expr(CgExpr {
                                                kind: CgExprKind::Call, type_str: None, line: 0, col: 0,
                                                data: CgExprData::Call {
                                                    name: "jeti_weakRegister".into(),
                                                    args: vec![cast_addr.clone(), cast_value],
                                                    vtable_class: None, alt_vtable_classes: vec![], is_class_method: false, is_super: false, sel_const_name: None, method_index: None, direct_specialized: false,
                                                },
                                            })},
                                        ]),
                                    })),
                                },
                                                            attributes: Vec::new(),
});
                        } else {
                            decls.push(CgDecl {
                                kind: CgDeclKind::Function, name: setter_fn_name,
                                data: CgDeclData::Function {
                                    return_type: "void".to_string(), params: setter_params,
                                    is_variadic: false, is_objc_class: true,
                                    body: Some(Box::new(convert_stmt(&setter_body, &class_infos))),
                                },
                                                            attributes: Vec::new(),
});
                        }
                        let info = class_infos.get_mut(&flat).unwrap();
                        if !info.method_names.contains(&setter_sel) {
                            info.method_names.push(setter_sel.clone());
                            info.method_sel_names.push(setter_name);
                            info.is_class_methods.push(false);
                            info.method_return_types.push("void".to_string());
                            info.method_params_list.push(setter_params_clone);
                            info.method_owners.push(flat.clone());
                                                    }
                    }
                }
            }

            let info = class_infos.get_mut(&flat).unwrap();
            // MERGE synthesized ivars with any existing ones (e.g. main
            // @interface declares `_sequencerId`, then a category @property
            // synthesizes `_neuralSyncRate`). The previous assignment
            // `info.ivar_names = ivar_names` dropped the main-class ivars.
            for (idx, n) in ivar_names.iter().enumerate() {
                if !info.ivar_names.contains(n) {
                    info.ivar_names.push(n.clone());
                    info.ivar_types.push(ivar_types[idx].clone());
                    // The weak flags must stay index-parallel with ivar_names.
                    // Dropping this push left `info.ivar_weak` shorter than
                    // `info.ivar_names`, so `is_weak` lookups for every
                    // synthesized property resolved to false — the weak write
                    // path was never taken and the slot was never nil'd.
                    info.ivar_weak.push(ivar_weak.get(idx).copied().unwrap_or(false));
                }
            }
        }
    }

    // Flatten nested namespace decls into the top-level decls stream so that
    // typedef aliases (e.g. Block typedef `void (^ActionCompleteBlock)(...)`)
    // declared inside `@namespace { ... }` reach the C output. Without this,
    // codegen treats Namespace as a stub (convert_decl ~line 958) and silently
    // drops the typedef aliases it contains, causing `unknown type name` errors.
    fn flatten_namespace_decls<'a>(ad: &'a AstDecl, out: &mut Vec<&'a AstDecl>) {
        if ad.kind == AstDeclKind::Namespace {
            if let AstDeclData::Namespace(inner) = &ad.data {
                for d in inner { flatten_namespace_decls(d, out); }
            }
            return;
        }
        out.push(ad);
    }
    let mut flat_decls: Vec<&AstDecl> = Vec::new();
    for d in &ast.decls { flatten_namespace_decls(d, &mut flat_decls); }

    for d in flat_decls {
        if d.kind != AstDeclKind::Class {
            // For @implementation methods, update the parameter names of the existing
            // synthetic property getter/setter declarations (e.g. "value" → real parameter name "s")
            if d.kind == AstDeclKind::Method {
                if let AstDeclData::Method { params: method_params, .. } = &d.data {
                    let sel_sanitized = d.name.as_deref()
                        .map(|n| n.replace(':', "_"))
                        .unwrap_or_default();
                    if let Some(existing) = decls.iter_mut().find(|e| e.name.ends_with(&sel_sanitized)) {
                        if let CgDeclData::Function { ref mut params, .. } = existing.data {
                            let mut p = method_params.as_ref().map(|b| &**b);
                            let mut idx = 2; // skip self, _cmd
                            while let Some(param) = p {
                                if idx < params.len() {
                                    let pn = param.name.clone().unwrap_or_default();
                                    if pn != "value" && pn != "_arg" && !pn.is_empty() {
                                        params[idx].1 = pn;
                                    }
                                }
                                idx += 1;
                                p = param.next.as_ref().map(|n| &**n);
                            }
                        }
                        continue; // skip adding a new declaration
                    }
                }
            }
            // Namespace: recurse into the namespace body and push each inner decl.
            if d.kind == AstDeclKind::Namespace {
                if let AstDeclData::Namespace(inner) = &d.data {
                    for inner_d in inner.iter() {
                        for cg in convert_decl(inner_d, &class_infos) {
                            // Skip the stub markers used for ivar/method/property
                            if cg.kind == CgDeclKind::Variable {
                                if let CgDeclData::Variable { ref var_type, .. } = cg.data {
                                    if var_type == "int" || var_type == "void" { continue; }
                                }
                            }
                            decls.push(cg);
                        }
                    }
                }
                continue;
            }
            for cg in convert_decl(d, &class_infos) {
                // Skip empty struct forward declarations (e.g. `struct NPClass;`)
                if cg.kind == CgDeclKind::Variable && cg.name == d.name.clone().unwrap_or_default() {
                    if let CgDeclData::Variable { ref var_type, .. } = cg.data {
                        if var_type == "void" { continue; }
                    }
                }
                decls.push(cg);
            }
        }
    }

    // Extract impl_vars from class declarations (e.g. static globals in categories)
    for d in &ast.decls {
        if d.kind == AstDeclKind::Class {
            if let AstDeclData::Class { impl_vars, .. } = &d.data {
                for v in impl_vars {
                    for cg in convert_decl(v, &class_infos) {
                        decls.push(cg);
                    }
                }
            }
        }
    }

    // ─── Generic instantiation collection ─────────────────────────────────────
    // Scan all decls/exprs for generic class instantiations like
    // `DataPack<QuantumToken*>` (type refs with non-empty type_args) and
    // `[[DataPack<QuantumToken*> alloc] init]` (MsgSend receiver rendered as
    // `Name<T*>` string). For each unique instantiation, clone the generic
    // class's ClassInfo (substituting T → concrete type in ivar/method
    // signatures) under the mangled flat name so codegen emits a standalone
    // struct/vtable/class metadata per instantiation. Without this, references
    // to `jeti_DataPack_QuantumToken_ptr_class` are undeclared.
    let mut generic_instantiations: Vec<(String, Vec<AstType>)> = Vec::new();
    // Structured collection: any AstType carrying non-empty `type_args` IS an
    // instantiation (fqn + args read directly — no string re-parsing). This
    // covers namespace-qualified receivers (`Metal::Box<int *>`), which the
    // old string path (VarRef name re-parse) silently missed.
    fn collect_from_type(t: &AstType, out: &mut Vec<(String, Vec<AstType>)>) {
        if !t.type_args.is_empty() {
            if let Some(ref n) = t.name {
                out.push((n.clone(), t.type_args.clone()));
            }
            for a in &t.type_args { collect_from_type(a, out); }
        }
    }
    fn collect_instantiations_expr(e: &AstExpr, out: &mut Vec<(String, Vec<AstType>)>) {
        // (1) Structured: the checker-annotated expr_type of ANY expression
        //     (receiver, var ref, message send result, ...) may carry type_args.
        if let Some(t) = e.expr_type.as_ref() { collect_from_type(t, out); }
        match &e.data {
            AstExprData::MsgSend { receiver, args, .. } => {
                // (2) Legacy string path: a VarRef whose NAME is a rendered
                //     `Name<T*>` (parser-forgiven generic receiver syntax).
                if let AstExprData::VarRef { ref name, .. } = receiver.data {
                    if let Some((base, args2)) = parse_generic_type_string(name) {
                        out.push((base, args2));
                    }
                }
                collect_instantiations_expr(receiver, out);
                for a in args { collect_instantiations_expr(a, out); }
            }
            AstExprData::FuncCall { args, callee, .. } => {
                if let Some(ce) = callee { collect_instantiations_expr(ce, out); }
                for a in args { collect_instantiations_expr(a, out); }
            }
            AstExprData::Assign { target, value } => {
                collect_instantiations_expr(target, out);
                collect_instantiations_expr(value, out);
            }
            AstExprData::Binary { left, right, .. } => {
                collect_instantiations_expr(left, out);
                collect_instantiations_expr(right, out);
            }
            AstExprData::Unary { operand, .. } => collect_instantiations_expr(operand, out),
            AstExprData::Paren(inner) => collect_instantiations_expr(inner, out),
            AstExprData::Cast { expr, .. } => collect_instantiations_expr(expr, out),
            AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => {
                collect_instantiations_expr(obj, out);
            }
            AstExprData::Block { body, .. } => {
                if let Some(b) = body { walk_stmt_for_inst(b, out, &collect_instantiations_expr); }
            }
            _ => {}
        }
    }
    fn parse_generic_type_string(s: &str) -> Option<(String, Vec<AstType>)> {
        // Parse `Name<T1, T2*>` rendered strings into (base, type_args).
        // Returns None if no `<...>` present.
        let lt = s.find('<')?;
        let base = s[..lt].to_string();
        // Find matching `>` (depth-aware for nested generics)
        let mut depth = 0;
        let mut end = None;
        for (i, ch) in s[lt..].char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 { end = Some(lt + i); break; }
                }
                _ => {}
            }
        }
        let end = end?;
        let args_str = &s[lt+1..end];
        let mut args = Vec::new();
        for a in split_top_commas(args_str) {
            args.push(render_type_str_to_ast(&a));
        }
        Some((base, args))
    }
    fn split_top_commas(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0;
        let mut cur = String::new();
        for ch in s.chars() {
            match ch {
                '<' => { depth += 1; cur.push(ch); }
                '>' => { depth -= 1; cur.push(ch); }
                ',' if depth == 0 => { out.push(cur.trim().to_string()); cur.clear(); }
                _ => cur.push(ch),
            }
        }
        if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
        out
    }
    fn render_type_str_to_ast(s: &str) -> AstType {
        // Render a type argument string like `QuantumToken*` into an AstType
        // so the substitution logic can mangle it. Keep it minimal: Named base
        // + pointer flag; nested generics handled by recursion when present.
        let s = s.trim();
        let (base, is_ptr) = if let Some(stripped) = s.strip_suffix('*') {
            (stripped.trim().to_string(), true)
        } else {
            (s.to_string(), false)
        };
        // Nested generic args?
        if let Some(lt) = base.find('<') {
            if base.ends_with('>') {
                let inner = &base[lt+1..base.len()-1];
                let inner_args: Vec<AstType> = split_top_commas(inner).iter().map(|s| render_type_str_to_ast(s)).collect();
                let mut t = AstType::new(TypePrim::Named);
                t.name = Some(base[..lt].to_string());
                t.type_args = inner_args;
                t.is_pointer = is_ptr;
                return t;
            }
        }
        let mut t = AstType::new(TypePrim::Named);
        t.name = Some(base);
        t.is_pointer = is_ptr;
        t
    }
    // Walk all decls and statements/expressions to collect instantiations.
    // Structured decl-level pass: variable declarations (incl. comma chains),
    // function/method return types and params carrying `type_args`.
    fn collect_from_decl(d: &AstDecl, out: &mut Vec<(String, Vec<AstType>)>) {
        match &d.data {
            AstDeclData::Variable { var_type, init, next, .. } => {
                if let Some(t) = var_type { collect_from_type(t, out); }
                if let Some(e) = init { collect_instantiations_expr(e, out); }
                if let Some(n) = next { collect_from_decl(n, out); }
            }
            AstDeclData::Function { return_type, params, body, .. } => {
                if let Some(t) = return_type { collect_from_type(t, out); }
                // AST params are CstParam (CstType) — check type_args directly.
                let mut p = params.as_deref();
                while let Some(pp) = p {
                    if let Some(pt) = pp.par_type.as_ref() {
                        if !pt.type_args.is_empty() {
                            if let Some(ref n) = pt.name {
                                // CstType → AstType via its rendered C string
                                // (handles pointer suffix / nested generics).
                                out.push((n.clone(), pt.type_args.iter()
                                    .map(|a| render_type_str_to_ast(&cst_type_to_c_str(a)))
                                    .collect()));
                            }
                        }
                    }
                    p = pp.next.as_deref();
                }
                if let Some(b) = body { walk_stmt_for_inst(b, out, &collect_instantiations_expr); }
            }
            AstDeclData::Method { return_type, params, body, .. } => {
                if let Some(t) = return_type { collect_from_type(t, out); }
                let mut p = params.as_deref();
                while let Some(pp) = p {
                    if let Some(pt) = pp.par_type.as_ref() {
                        if !pt.type_args.is_empty() {
                            if let Some(ref n) = pt.name {
                                out.push((n.clone(), pt.type_args.iter()
                                    .map(|a| render_type_str_to_ast(&cst_type_to_c_str(a)))
                                    .collect()));
                            }
                        }
                    }
                    p = pp.next.as_deref();
                }
                if let Some(b) = body { walk_stmt_for_inst(b, out, &collect_instantiations_expr); }
            }
            AstDeclData::Namespace(inner) => {
                for d2 in inner.iter() { collect_from_decl(d2, out); }
            }
            _ => {}
        }
    }
    // Route ① transitive closure: param-keyed inner uses per template
    // (enclosing flat, inner base, inner args). `ChainArray<T>` inside
    // `ChainBox<T>`'s body dispatches through the marker flat
    // (`ChainArray_T`); cloning ChainBox<NPString *> must re-specialize
    // ChainArray with the OUTER arguments and rewrite the clone's marker
    // dispatch to the new clone.
    let mut inner_uses: Vec<(String, String, Vec<AstType>)> = Vec::new();
    for d in &ast.decls {
        if d.kind == AstDeclKind::Class {
            if let AstDeclData::Class { methods, ivars, .. } = &d.data {
                let enc_flat = name_flat(d.name.as_deref().unwrap_or(""));
                for m in methods {
                    if let AstDeclData::Method { body, .. } = &m.data {
                        if let Some(b) = body {
                            walk_stmt_for_inst(b, &mut generic_instantiations, &collect_instantiations_expr);
                            collect_inner_stmt(b, &enc_flat, &mut inner_uses);
                        }
                    }
                }
                for iv in ivars {
                    if let AstDeclData::Ivar { ivar_type, .. } = &iv.data {
                        if let Some(t) = ivar_type {
                            collect_inner_type(t, &enc_flat, &mut inner_uses);
                            if !t.type_args.is_empty() {
                                generic_instantiations.push((t.name.clone().unwrap_or_default(), t.type_args.clone()));
                            }
                        }
                    }
                }
            }
        }
        if d.kind == AstDeclKind::Function {
            if let AstDeclData::Function { body, .. } = &d.data {
                if let Some(b) = body {
                    walk_stmt_for_inst(b, &mut generic_instantiations, &collect_instantiations_expr);
                    collect_sel_stmt(b, &mut selectors);
                }
            }
        }
        // Structured: any decl node with type_args (variables, functions,
        // methods, namespaces) — covers namespace-qualified instantiations.
        collect_from_decl(d, &mut generic_instantiations);
    }
    // Multi-TU driver supplied these uses from sibling translation units.
    // The implementation TU must still contain the generic class body; this
    // only makes the use-site demand visible while preserving true
    // monomorphisation and one stable mangled symbol per specialization.
    if let Some(forced) = forced_generic_instantiations {
        for spelling in forced {
            if let Some((base, args)) = parse_generic_type_string(spelling) {
                generic_instantiations.push((base, args));
            }
        }
    }
    // Deduplicate instantiations by (base, rendered args).
    generic_instantiations.dedup_by(|a, b| {
        a.0 == b.0 && a.1.iter().map(ast_type_to_c_str).collect::<String>() == b.1.iter().map(ast_type_to_c_str).collect::<String>()
    });
    // Clone generic classes per instantiation with substituted types — as a
    // WORKLIST: cloning `Box<NPString *>` discovers its param-keyed inner
    // uses (`ChainArray<T>` → demand `ChainArray<NPString *>`) and pushes
    // them back; the loop runs to a fixed point, closing the transitive
    // instantiation chain (C++-template semantics). Demand dedup by mangled
    // name is the cycle guard (`Box<Self>` chains stop, like rustc's).
    // Substitute T → concrete type in ivar_types/method return types/params by
    // string replacement (rendered C strings use `NPObject *` for TypePrim::Param).
    let mut clone_queue: Vec<(String, Vec<AstType>)> = generic_instantiations;
    // Demand manifest of the whole worklist (direct + derived). This is the
    // cross-TU contract: a sibling TU that owns one of the *derived* templates
    // cannot see the demand from source text (the spelling is param-keyed
    // there), so the driver echoes this list back as `--generic-instantiation`.
    let mut generic_demands: Vec<String> = Vec::new();
    let mut qi = 0usize;
    while qi < clone_queue.len() {
        let (base, args) = clone_queue[qi].clone();
        qi += 1;
        let mangled = format!("{}<{}>", base, args.iter().map(ast_type_to_c_str).collect::<Vec<_>>().join(", "));
        let mangled_flat = name_flat(&mangled);
        if class_infos.contains_key(&mangled_flat) { continue; }
        // Find the generic class template to clone (by base name, flat).
        let base_flat = name_flat(&base);
        if let Some(template) = class_infos.get(&base_flat).cloned() {
            // A spelling whose arguments are all the template's own type
            // parameters (`ChainArray<T>` collected from inside ChainBox's
            // body) is the erased template, not an instantiation: cloning it
            // would fabricate `ChainArray_T` symbols (a duplicate keyed on the
            // parameter name) and the `T → "T"` no-op substitution poisons
            // substituted return types into invalid C (`(T){0}`).
            if ast_args_are_template_params(&args, &template.type_params) {
                continue;
            }
            // Recorded only past that gate: the manifest is fed back to sibling
            // TUs as `--generic-instantiation`, so an erased or param-keyed
            // spelling in it would fabricate garbage specializations there.
            generic_demands.push(mangled.clone());
            // Route ①: close the chain. Every param-keyed inner use of this
            // template gets the OUTER concrete arguments substituted into the
            // inner spelling, producing a NEW demand for the inner clone plus
            // a marker rewrite (`ChainArray_T` → `ChainArray_NPString_ptr`)
            // applied to this clone's method bodies.
            let mut inner_rewrites: Vec<(String, String)> = Vec::new();
            for (enc, ibase, iargs) in &inner_uses {
                if name_flat(enc) != base_flat { continue; }
                let inner_args: Vec<AstType> = iargs.iter().map(|a| {
                    let n = a.name.as_deref().map(|n| n.trim_end_matches('*').trim()).unwrap_or("");
                    match template.type_params.iter().position(|p| p == n) {
                        Some(idx) => args.get(idx).cloned().unwrap_or_else(|| a.clone()),
                        None => a.clone(),
                    }
                }).collect();
                let marker_spelling = format!("{}<{}>", ibase,
                    iargs.iter().map(ast_type_to_argname).collect::<Vec<_>>().join(", "));
                let marker_flat = name_flat(&marker_spelling);
                let inner_mangled = format!("{}<{}>", ibase,
                    inner_args.iter().map(ast_type_to_c_str).collect::<Vec<_>>().join(", "));
                let inner_flat = name_flat(&inner_mangled);
                if marker_flat == inner_flat { continue; }
                if !class_infos.contains_key(&inner_flat) {
                    clone_queue.push((ibase.clone(), inner_args.clone()));
                }
                inner_rewrites.push((marker_flat, inner_flat));
            }
            // Build (sentinel → concrete) pairs from the template's declared
            // type params: `NPDictionary<K, V>` with args [NPString*, NPNumber*]
            // pairs (`NPObject * /*K*/` → `NPString *`) and
            // (`NPObject * /*V*/` → `NPNumber *`) so each position substitutes
            // by name. Templates whose params are missing or arg-count-
            // mismatched fall back to the single legacy `/*T*/` marker joined
            // across all args.
            let param_pairs: Vec<(String, String)> =
                if !template.type_params.is_empty() && template.type_params.len() == args.len() {
                    template.type_params.iter().zip(args.iter())
                        .map(|(pn, a)| (format!("NPObject * /*{}*/", pn), ast_type_to_c_str(a)))
                        .collect()
                } else {
                    let concrete_str = args.iter().map(ast_type_to_c_str).collect::<Vec<_>>().join(", ");
                    vec![(T_PARAM_RENDER.to_string(), concrete_str)]
                };
            let mut sub_info = template.clone();
            sub_info.class_name = mangled.clone();
            sub_info.flat = mangled_flat.clone();
            // Apply every pair. Sentinels are mutually distinct strings, so
            // sequential passes cannot cross-replace each other's results.
            for (marker, concrete) in &param_pairs {
                // Substitute in ivar_types.
                sub_info.ivar_types = sub_info.ivar_types.iter().map(|s| s.replace(marker, concrete)).collect();
                // Substitute in method_return_types.
                sub_info.method_return_types = sub_info.method_return_types.iter().map(|s| s.replace(marker, concrete)).collect();
                // Substitute in method_params_list (each param type).
                sub_info.method_params_list = sub_info.method_params_list.iter().map(|params| {
                    params.iter().map(|(pt, pn)| (pt.replace(marker, concrete), pn.clone())).collect()
                }).collect();
            }
            sub_info.method_owners = sub_info.method_owners.iter().map(|_| mangled_flat.clone()).collect();
            // Rewrite cloned method bodies so the `_self` cast and any T-derived
            // type strings refer to the specialized class, not the generic template.
            // e.g. `struct Box * _self = (struct Box *)self;` → `struct Box_Node_ptr * ...`
            for body_cell in sub_info.method_bodies.iter_mut() {
                if let Some(b) = body_cell.as_mut() {
                    for (marker, concrete) in &param_pairs {
                        substitute_cg_stmt(b, &base_flat, &mangled_flat, marker, concrete);
                    }
                }
            }
            for (i, mname) in sub_info.method_names.iter().enumerate() {
                let fn_name = format!("{}_{}", mangled_flat, mname);
                let ret_type = sub_info.method_return_types.get(i).cloned().unwrap_or_else(|| "NPObject *".into());
                let params = sub_info.method_params_list.get(i).cloned().unwrap_or_default();
                let base_fn_name = format!("{}_{}", base_flat, mname);
                let mut body = sub_info.method_bodies.get(i)
                    .and_then(|b| b.as_ref().map(|b| Box::new(b.as_ref().clone())))
                    .or_else(|| {
                        decls.iter().find(|d| d.name == base_fn_name).and_then(|d| {
                            if let CgDeclData::Function { ref body, .. } = d.data {
                                let mut cloned = body.clone();
                                if let Some(ref mut c) = cloned {
                                    for (marker, concrete) in &param_pairs {
                                        substitute_cg_stmt(c, &base_flat, &mangled_flat, marker, concrete);
                                    }
                                }
                                cloned
                            } else { None }
                        })
                    });
                // Route ①: point the clone's param-keyed inner dispatch at the
                // real inner specialization (marker flat → inner clone flat).
                if !inner_rewrites.is_empty() {
                    if let Some(ref mut b) = body {
                        for (from, to) in &inner_rewrites {
                            rewrite_inner_dispatch_stmt(b, from, to);
                        }
                    }
                }
                decls.push(CgDecl {
                    kind: CgDeclKind::Function, name: fn_name,
                    data: CgDeclData::Function {
                        return_type: ret_type,
                        params,
                        body,
                        is_variadic: false,
                        is_objc_class: true,
                    },
                                    attributes: Vec::new(),
});
            }
            class_infos.insert(mangled_flat, sub_info);
        }
    }
    // Route ①: marker forwarders. A template body dispatches param-keyed
    // spellings through the marker flat (`ChainArray_T_first`); in template
    // (erased) mode those symbols must exist — emit one delegating function
    // per marker × method. Weak: two templates in different TUs may share a
    // marker, and the duplicates must fold at link time.
    for (_enc, ibase, iargs) in &inner_uses {
        let base_flat = name_flat(ibase);
        let Some(template) = class_infos.get(&base_flat).cloned() else { continue };
        let marker_spelling = format!("{}<{}>", ibase,
            iargs.iter().map(ast_type_to_argname).collect::<Vec<_>>().join(", "));
        let marker_flat = name_flat(&marker_spelling);
        if marker_flat == base_flat || class_infos.contains_key(&marker_flat) { continue; }
        for (i, mname) in template.method_names.iter().enumerate() {
            let has_body = template.method_bodies.get(i).map_or(false, |b| b.is_some());
            if !has_body { continue; }
            let fwd_name = format!("{}_{}", marker_flat, mname);
            if decls.iter().any(|dd| dd.name == fwd_name) { continue; }
            let target = format!("{}_{}", base_flat, mname);
            let ret = template.method_return_types.get(i).cloned().unwrap_or_else(|| "NPObject *".into());
            let params = template.method_params_list.get(i).cloned().unwrap_or_default();
            let arg_exprs: Vec<CgExpr> = params.iter().map(|(_, n)| CgExpr {
                kind: CgExprKind::Ident, type_str: None, line: 0, col: 0,
                data: CgExprData::Ident(n.clone()),
            }).collect();
            let call = CgExpr {
                kind: CgExprKind::Call, type_str: Some(ret.clone()), line: 0, col: 0,
                data: CgExprData::Call {
                    name: target, args: arg_exprs,
                    vtable_class: None, alt_vtable_classes: Vec::new(),
                    is_class_method: false, is_super: false,
                    sel_const_name: None, method_index: None,
                    direct_specialized: false,
                },
            };
            decls.push(CgDecl {
                kind: CgDeclKind::Function, name: fwd_name,
                data: CgDeclData::Function {
                    return_type: ret, params,
                    // Body is a compound wrapping the delegating return —
                    // emit_stmt renders non-compound bodies without braces.
                    body: Some(Box::new(CgStmt {
                        kind: CgStmtKind::Compound, line: 0, col: 0,
                        data: CgStmtData::Compound(vec![CgStmt {
                            kind: CgStmtKind::Return, line: 0, col: 0,
                            data: CgStmtData::Return(Some(Box::new(call))),
                        }]),
                    })),
                    is_variadic: false, is_objc_class: true,
                },
                // The definition emitter adds `__attribute__((weak))` for any
                // body-carrying function that isn't internal_linkage — cross-TU
                // duplicate markers fold at link time. No explicit attribute.
                attributes: Vec::new(),
            });
        }
    }
    // Helper to walk statements for expressions containing instantiations.
    // Use `&dyn Fn` (not generic `<F>`) so recursion goes through one shared
    // monomorphization instead of nesting generic instances infinitely
    // (the `<F>` form hit rustc's recursion limit while instantiating).
    type InstList = Vec<(String, Vec<AstType>)>;
    type InstCollector<'a> = &'a dyn Fn(&AstExpr, &mut InstList);
    fn walk_stmt_for_inst(s: &AstStmt, out: &mut InstList, f: InstCollector) {
        match &s.data {
            AstStmtData::Expr(e) => { walk_expr_for_inst(e, out, f); }
            AstStmtData::Decl(d) => { walk_decl_for_inst(d, out, f); }
            AstStmtData::If { then, else_, .. } => {
                walk_stmt_for_inst(then, out, f);
                if let Some(eb) = else_ { walk_stmt_for_inst(eb, out, f); }
            }
            AstStmtData::While { body, .. } | AstStmtData::Do { body, .. } => { walk_stmt_for_inst(body, out, f); }
            AstStmtData::For { init, cond, incr, body, .. } => {
                if let Some(i) = init { walk_stmt_for_inst(i, out, f); }
                if let Some(c) = cond { walk_expr_for_inst(c, out, f); }
                if let Some(u) = incr { walk_expr_for_inst(u, out, f); }
                walk_stmt_for_inst(body, out, f);
            }
            AstStmtData::Compound(stmts) => { for st in stmts { walk_stmt_for_inst(st, out, f); } }
            AstStmtData::Autoreleasepool(body) => { walk_stmt_for_inst(body, out, f); }
            AstStmtData::NoArc(body) => { walk_stmt_for_inst(body, out, f); }
            _ => {}
        }
    }
    fn walk_expr_for_inst(e: &AstExpr, out: &mut InstList, f: InstCollector) {
        f(e, out);
        match &e.data {
            AstExprData::FuncCall { args, .. } => {
                for a in args { walk_expr_for_inst(a, out, f); }
            }
            AstExprData::MsgSend { receiver, args, .. } => {
                walk_expr_for_inst(receiver, out, f);
                for a in args { walk_expr_for_inst(a, out, f); }
            }
            _ => {}
        }
    }
    fn walk_decl_for_inst(d: &AstDecl, out: &mut InstList, f: InstCollector) {
        match &d.data {
            AstDeclData::Function { body, .. } => { if let Some(b) = body { walk_stmt_for_inst(b, out, f); } }
            AstDeclData::Variable { var_type, init, .. } => {
                if let Some(t) = var_type {
                    if !t.type_args.is_empty() {
                        out.push((t.name.clone().unwrap_or_default(), t.type_args.clone()));
                    }
                }
                if let Some(i) = init { walk_expr_for_inst(i, out, f); }
            }
            _ => {}
        }
    }

    // ── Route ① closure walkers ─────────────────────────────────────────
    // Collect param-keyed inner uses (decl types, ivar types, and class-name
    // receiver spellings) with their enclosing class, and rewrite marker
    // dispatch in clone bodies to the real inner specialization.
    type InnerUse = (String, String, Vec<AstType>);
    fn collect_inner_expr(e: &AstExpr, enc: &str, out: &mut Vec<InnerUse>) {
        match &e.data {
            AstExprData::VarRef { name, .. } => {
                if name.contains('<') {
                    if let Some((base, args)) = parse_generic_type_string(name) {
                        out.push((enc.to_string(), base, args));
                    }
                }
            }
            AstExprData::MsgSend { receiver, args, .. } => {
                collect_inner_expr(receiver, enc, out);
                for a in args { collect_inner_expr(a, enc, out); }
            }
            AstExprData::FuncCall { callee, args, .. } => {
                if let Some(ce) = callee { collect_inner_expr(ce, enc, out); }
                for a in args { collect_inner_expr(a, enc, out); }
            }
            AstExprData::Assign { target, value } => {
                collect_inner_expr(target, enc, out);
                collect_inner_expr(value, enc, out);
            }
            AstExprData::Binary { left, right, .. } => {
                collect_inner_expr(left, enc, out);
                collect_inner_expr(right, enc, out);
            }
            AstExprData::Unary { operand, .. } => collect_inner_expr(operand, enc, out),
            AstExprData::Paren(inner) => collect_inner_expr(inner, enc, out),
            AstExprData::Cast { expr, .. } => collect_inner_expr(expr, enc, out),
            AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => {
                collect_inner_expr(obj, enc, out);
            }
            _ => {}
        }
    }
    fn collect_inner_stmt(s: &AstStmt, enc: &str, out: &mut Vec<InnerUse>) {
        match &s.data {
            AstStmtData::Expr(e) => collect_inner_expr(e, enc, out),
            AstStmtData::Decl(d) => collect_inner_decl(d, enc, out),
            AstStmtData::If { then, else_, .. } => {
                collect_inner_stmt(then, enc, out);
                if let Some(eb) = else_ { collect_inner_stmt(eb, enc, out); }
            }
            AstStmtData::While { body, .. } | AstStmtData::Do { body, .. } => collect_inner_stmt(body, enc, out),
            AstStmtData::For { init, cond, incr, body, .. } => {
                if let Some(i) = init { collect_inner_stmt(i, enc, out); }
                if let Some(c) = cond { collect_inner_expr(c, enc, out); }
                if let Some(u) = incr { collect_inner_expr(u, enc, out); }
                collect_inner_stmt(body, enc, out);
            }
            AstStmtData::Compound(stmts) => { for st in stmts { collect_inner_stmt(st, enc, out); } }
            AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => collect_inner_stmt(body, enc, out),
            _ => {}
        }
    }
    fn collect_inner_decl(d: &AstDecl, enc: &str, out: &mut Vec<InnerUse>) {
        match &d.data {
            AstDeclData::Variable { var_type, init, next, .. } => {
                if let Some(t) = var_type { collect_inner_type(t, enc, out); }
                if let Some(e) = init { collect_inner_expr(e, enc, out); }
                if let Some(n) = next { collect_inner_decl(n, enc, out); }
            }
            AstDeclData::Function { body, .. } | AstDeclData::Method { body, .. } => {
                if let Some(b) = body { collect_inner_stmt(b, enc, out); }
            }
            _ => {}
        }
    }
    fn collect_inner_type(t: &AstType, enc: &str, out: &mut Vec<InnerUse>) {
        if !t.type_args.is_empty() {
            if let Some(n) = &t.name {
                out.push((enc.to_string(), n.clone(), t.type_args.clone()));
            }
        }
        if let Some(ref s) = t.subtype { collect_inner_type(s, enc, out); }
        for a in &t.type_args { collect_inner_type(a, enc, out); }
        if let Some(ref n) = t.next { collect_inner_type(n, enc, out); }
    }
    fn rewrite_inner_dispatch_expr(e: &mut CgExpr, from: &str, to: &str) {
        match &mut e.data {
            CgExprData::Call { vtable_class, alt_vtable_classes, args, .. } => {
                for a in args.iter_mut() { rewrite_inner_dispatch_expr(a, from, to); }
                // The marker appears ONLY in the vtable-class fields (types
                // stay erased, class addresses route to the base template),
                // so the rewrite cannot touch unrelated spellings.
                if let Some(vc) = vtable_class {
                    if name_flat(vc) == from { *vc = to.to_string(); }
                }
                for alt in alt_vtable_classes.iter_mut() {
                    if name_flat(alt) == from { *alt = to.to_string(); }
                }
            }
            CgExprData::Unary { operand, .. } => rewrite_inner_dispatch_expr(operand, from, to),
            CgExprData::Binary { left, right, .. } => {
                rewrite_inner_dispatch_expr(left, from, to);
                rewrite_inner_dispatch_expr(right, from, to);
            }
            CgExprData::Assign { target, value } => {
                rewrite_inner_dispatch_expr(target, from, to);
                rewrite_inner_dispatch_expr(value, from, to);
            }
            CgExprData::Cast { expr, .. } => rewrite_inner_dispatch_expr(expr, from, to),
            CgExprData::Paren(inner) => rewrite_inner_dispatch_expr(inner, from, to),
            CgExprData::Comma(exprs) => { for x in exprs.iter_mut() { rewrite_inner_dispatch_expr(x, from, to); } }
            CgExprData::Member { obj, .. } | CgExprData::Arrow { obj, .. } => rewrite_inner_dispatch_expr(obj, from, to),
            CgExprData::Index { arr, index } => {
                rewrite_inner_dispatch_expr(arr, from, to);
                rewrite_inner_dispatch_expr(index, from, to);
            }
            CgExprData::Ternary { cond, then, else_ } => {
                rewrite_inner_dispatch_expr(cond, from, to);
                rewrite_inner_dispatch_expr(then, from, to);
                rewrite_inner_dispatch_expr(else_, from, to);
            }
            _ => {}
        }
    }
    fn rewrite_inner_dispatch_stmt(s: &mut CgStmt, from: &str, to: &str) {
        match &mut s.data {
            CgStmtData::Expr(e) => rewrite_inner_dispatch_expr(e, from, to),
            CgStmtData::Compound(stmts) => {
                for st in stmts.iter_mut() { rewrite_inner_dispatch_stmt(st, from, to); }
            }
            CgStmtData::If { cond, then, else_ } => {
                rewrite_inner_dispatch_expr(cond, from, to);
                rewrite_inner_dispatch_stmt(then, from, to);
                if let Some(eb) = else_ { rewrite_inner_dispatch_stmt(eb, from, to); }
            }
            CgStmtData::While { cond, body } => {
                rewrite_inner_dispatch_expr(cond, from, to);
                rewrite_inner_dispatch_stmt(body, from, to);
            }
            CgStmtData::Do { body, cond } => {
                rewrite_inner_dispatch_stmt(body, from, to);
                rewrite_inner_dispatch_expr(cond, from, to);
            }
            CgStmtData::For { init, cond, incr, body, .. } => {
                if let Some(i) = init { rewrite_inner_dispatch_stmt(i, from, to); }
                if let Some(c) = cond { rewrite_inner_dispatch_expr(c, from, to); }
                if let Some(u) = incr { rewrite_inner_dispatch_expr(u, from, to); }
                rewrite_inner_dispatch_stmt(body, from, to);
            }
            CgStmtData::Return(v) => { if let Some(e) = v { rewrite_inner_dispatch_expr(e, from, to); } }
            CgStmtData::Switch { expr, body } => {
                rewrite_inner_dispatch_expr(expr, from, to);
                rewrite_inner_dispatch_stmt(body, from, to);
            }
            CgStmtData::Case { value, body } => {
                rewrite_inner_dispatch_expr(value, from, to);
                rewrite_inner_dispatch_stmt(body, from, to);
            }
            CgStmtData::Default(b) => rewrite_inner_dispatch_stmt(b, from, to),
            CgStmtData::Decl { init, next, .. } => {
                if let Some(e) = init { rewrite_inner_dispatch_expr(e, from, to); }
                for (_, _, io) in next.iter_mut() { if let Some(e) = io { rewrite_inner_dispatch_expr(e, from, to); } }
            }
            CgStmtData::ForIn { collection, body, .. } => {
                rewrite_inner_dispatch_expr(collection, from, to);
                rewrite_inner_dispatch_stmt(body, from, to);
            }
            _ => {}
        }
    }
    for (flat, info) in std::mem::take(&mut class_infos) {
        // Record which `Owner_method` function bodies actually exist. A vtable
        // slot can exist without an implementation — a method a class declares
        // by conforming to a protocol, or one it declares but leaves to another
        // TU — and those slots must initialise to NULL rather than reference a
        // function that is never emitted (an undefined symbol at link time).
        for (i, mname) in info.method_names.iter().enumerate() {
            if info.is_class_methods.get(i).copied().unwrap_or(false) { continue; }
            let has_body = info.method_bodies.get(i).map_or(false, |b| b.is_some());
            if has_body {
                let owner = info.method_owners.get(i).cloned().unwrap_or(flat.clone());
                if let Some(set) = emitted_methods().as_mut() {
                    set.insert(format!("{}_{}", owner, mname));
                }
            }
        }
        classes.push(CgClassMeta {
            class_name: info.class_name,
            super_name: info.super_name.clone(),
method_names: info.method_names,
             method_sel_names: info.method_sel_names,
             is_class_methods: info.is_class_methods,
            method_return_types: info.method_return_types,
            method_params_list: info.method_params_list,
            method_variadic: info.method_variadic,
            method_owners: info.method_owners,
            category_method_names: info.category_method_names,
            has_category_impl: info.has_category_impl,
            vtable_indices: Vec::new(),
            ivar_types: info.ivar_types,
            ivar_names: info.ivar_names,
            ivar_weak: info.ivar_weak,
            properties: Vec::new(),
            has_impl: info.method_bodies.iter().any(|b| b.is_some()) || info.has_impl_decl,
            protocols: info.protocols.clone(),
        });
    }

    // Sort classes by inheritance depth so parents are processed before children.
    // This ensures that when a class inherits from its parent, the parent's method
    // list already includes its own inherited methods (from grandparents).
    {
        let n = classes.len();
        let mut depth = vec![0usize; n];
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..n {
                if let Some(ref sup) = classes[i].super_name {
                    if let Some(sup_idx) = classes.iter().position(|c| c.class_name == *sup) {
                        let d = depth[sup_idx] + 1;
                        if d > depth[i] {
                            depth[i] = d;
                            changed = true;
                        }
                    }
                }
            }
        }
        // Bubble sort by depth (stable not required, but simple)
        for i in 0..n {
            for j in i+1..n {
                if depth[j] < depth[i] {
                    classes.swap(i, j);
                    depth.swap(i, j);
                }
            }
        }
    }

    for i in 0..classes.len() {
        let (sup_name, mut method_names, mut is_class_methods, mut method_return_types, mut method_params_list, mut method_variadic, mut method_owners) = {
            let cm = &classes[i];
            (cm.super_name.clone(), cm.method_names.clone(), cm.is_class_methods.clone(), cm.method_return_types.clone(), cm.method_params_list.clone(), cm.method_variadic.clone(), cm.method_owners.clone())
        };
        if let Some(ref sup) = sup_name {
            if let Some(sup_idx) = classes.iter().position(|c| c.class_name == *sup) {
                let sup = &classes[sup_idx];
                // Build complete inherited vtable layout: all parent methods in parent order,
                // with overridden methods replaced by subclass versions at the inherited positions.
                let mut new_names: Vec<String> = Vec::new();
                let mut new_ic: Vec<bool> = Vec::new();
                let mut new_rt: Vec<String> = Vec::new();
                let mut new_pl: Vec<Vec<(String, String)>> = Vec::new();
                let mut new_var: Vec<bool> = Vec::new();
                let mut new_ow: Vec<String> = Vec::new();
                for (j, mname) in sup.method_names.iter().enumerate() {
                    new_names.push(mname.clone());
                    new_ic.push(sup.is_class_methods[j]);
                    new_rt.push(sup.method_return_types.get(j).cloned().unwrap_or_default());
                    new_pl.push(sup.method_params_list.get(j).cloned().unwrap_or_default());
                    new_var.push(sup.method_variadic.get(j).copied().unwrap_or(false));
                    let owner = sup.method_owners.get(j).cloned().unwrap_or_else(|| sup.class_name.clone());
                    new_ow.push(owner.clone());
                    // If subclass overrides, replace with subclass version
                    // Only override if same type (both instance or both class).
                    // If types differ (parent has instance, child has class method with same name),
                    // the child's method is added as a separate entry below.
                    if let Some(own_idx) = method_names.iter().position(|n| n == mname) {
                        if is_class_methods[own_idx] == sup.is_class_methods[j] {
                            let len = new_names.len();
                            new_ic[len - 1] = is_class_methods[own_idx];
                            new_rt[len - 1] = method_return_types.get(own_idx).cloned().unwrap_or_default();
                            new_pl[len - 1] = method_params_list.get(own_idx).cloned().unwrap_or_default();
                            new_var[len - 1] = method_variadic.get(own_idx).copied().unwrap_or(false);
                            new_ow[len - 1] = method_owners.get(own_idx).cloned().unwrap_or_else(|| owner);
                        }
                    }
                }
                // Append subclass-only methods (not in parent, or with different type)
                for (j, mname) in method_names.iter().enumerate() {
                    let parent_pos = sup.method_names.iter().position(|n| n == mname);
                    let should_add = match parent_pos {
                        None => true,                          // not in parent at all
                        Some(pp) => is_class_methods[j] != sup.is_class_methods[pp], // same name, different type
                    };
                    if should_add {
                        new_names.push(mname.clone());
                        new_ic.push(is_class_methods[j]);
                        new_rt.push(method_return_types.get(j).cloned().unwrap_or_default());
                        new_pl.push(method_params_list.get(j).cloned().unwrap_or_default());
                        new_var.push(method_variadic.get(j).copied().unwrap_or(false));
                        new_ow.push(method_owners.get(j).cloned().unwrap_or_default());
                    }
                }
                method_names = new_names;
                is_class_methods = new_ic;
                method_return_types = new_rt;
                method_params_list = new_pl;
                method_variadic = new_var;
                method_owners = new_ow;
            }
        }
        classes[i].method_names = method_names;
        classes[i].is_class_methods = is_class_methods;
        classes[i].method_return_types = method_return_types;
        classes[i].method_params_list = method_params_list;
        classes[i].method_variadic = method_variadic;
        classes[i].method_owners = method_owners;
    }

    // Collect unique instance method names across all classes, assign global indices.
    let mut global_instance_method_names: Vec<String> = Vec::new();
    for cm in &classes {
        for (j, mname) in cm.method_names.iter().enumerate() {
            if !cm.is_class_methods[j] && !global_instance_method_names.contains(mname) {
                global_instance_method_names.push(mname.clone());
            }
        }
    }
    // Stable cross-TU slot assignment. Without a slots manifest the order is
    // sorted-alphabetical (per-TU — different TU method sets produce different
    // layouts, caught only at startup by the __sig check). With a manifest,
    // previously-assigned methods keep their exact slot (append-only, never
    // renumbered) and new methods are appended at the end — so TUs compiled
    // with the same manifest link correctly even with different method sets.
    match slots_manifest {
        Some(manifest) => {
            // The manifest defines the COMPLETE layout: every entry keeps its
            // slot even when this TU cannot see the method (its slot is then
            // initialized to NULL below, mirroring the protocol-method
            // precedent). Filtering the manifest down to this TU's own method
            // set would shrink the layout per-TU again — exactly the cross-TU
            // mismatch the manifest exists to prevent. Methods unknown to the
            // manifest (declared in this TU only) are appended at the end,
            // preserving the append-only never-renumbered contract.
            let mut ordered: Vec<String> = manifest.to_vec();
            for mname in &global_instance_method_names {
                if !ordered.contains(mname) { ordered.push(mname.clone()); }
            }
            global_instance_method_names = ordered;
        }
        None => {
            global_instance_method_names.sort();
            // R1 (stable slots): the public segment must occupy the SAME slot
            // indices in every TU, or a legal two-TU split (library + client)
            // ends up with disagreeing layouts. Sorting by "is private" is a
            // stable sort, so both segments stay alphabetical internally.
            if let Some(public) = public_methods {
                global_instance_method_names.sort_by_key(|m| !public.contains(m));
            }
        }
    }

    // R3: the `__sig` fingerprint covers only the shared segment — see
    // `vtable_sig_segment` for the segment rules.
    //
    // §8-④: async methods additionally carry their async signature in the
    // fingerprint (see `async_sig_marks`) — the C spelling of an async method
    // is the uniform `NPTask *`, so async-ness, failability and the business
    // type T would otherwise be invisible to the guard.
    let sig_marks = async_sig_marks(ast);
    let vtable_sig_names: Vec<String> =
        vtable_sig_segment(slots_manifest, public_methods, &global_instance_method_names)
            .into_iter()
            .map(|name| match sig_marks.get(&name) {
                Some(decls) => {
                    // Unanimity rule (see `async_sig_marks`): stamp the mark
                    // only when every declaration of this selector agrees on
                    // it. A same-named synchronous declaration means the shared
                    // slot is not what the async one describes — keep the bare
                    // name rather than making agreeing TUs disagree.
                    let mut distinct: Vec<&String> = Vec::new();
                    let mut has_sync = false;
                    for m in decls {
                        match m {
                            Some(mk) => { if !distinct.contains(&mk) { distinct.push(mk); } }
                            None => has_sync = true,
                        }
                    }
                    if distinct.len() == 1 && !has_sync {
                        format!("{}{}", name, distinct[0])
                    } else {
                        name
                    }
                }
                None => name,
            })
            .collect();

    let mut method_meta: HashMap<String, (usize, String)> = HashMap::new();
    for (idx, mname) in global_instance_method_names.iter().enumerate() {
        // Find the first class that has this method to get its signature.
        let mut signature: Option<String> = None;
        for cm in &classes {
            if let Some(pos) = cm.method_names.iter().position(|n| n == mname) {
                if !cm.is_class_methods[pos] {
                    let rt = cm.method_return_types.get(pos).cloned().unwrap_or_else(|| "NPObject *".into());
                    let params = cm.method_params_list.get(pos)
                        .map(|p| p.iter().map(|(pt, _)| pt.clone()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_else(|| "NPObject *, SEL".into());
                    // Variadic method → C `...` in the fn-ptr type; the dispatch
                    // cast must match the emitted variadic signature exactly
                    // (jeti has no msgSend runtime to paper over a mismatch).
                    let v = cm.method_variadic.get(pos).copied().unwrap_or(false);
                    let ellipsis = if v { ", ..." } else { "" };
                    signature = Some(format!("{} (*)({}{})", rt, params, ellipsis));
                    break;
                }
            }
        }
        // A manifest slot for a method this TU never sees (it is defined in
        // another TU only) has no signature available here. A placeholder
        // fn-ptr type keeps the struct layout byte-identical — every vtable
        // member is a function pointer, same size and alignment — and the
        // slot is initialized to NULL by the vtable-instance emitter, exactly
        // like protocol methods this TU does not implement. No dispatch
        // through this TU can reach it.
        method_meta.insert(mname.clone(), (idx, signature.unwrap_or_else(|| "void (*)(void)".into())));
    }

    // Populate vtable_indices for each class.
    for cm in &mut classes {
        cm.vtable_indices = Vec::new();
        for (j, mname) in cm.method_names.iter().enumerate() {
            if !cm.is_class_methods[j] {
                if let Some((idx, _)) = method_meta.get(mname) {
                    cm.vtable_indices.push(*idx as i32);
                } else {
                    cm.vtable_indices.push(-1);
                }
            } else {
                cm.vtable_indices.push(-1);
            }
        }
    }

    METHOD_METADATA.set(method_meta).unwrap();

    // Build per-class method metadata for dispatch with correct signature.
    let mut class_method_meta: HashMap<String, HashMap<String, (usize, String)>> = HashMap::new();
    let global_meta = METHOD_METADATA.get().unwrap();
    for cm in &classes {
        let flat = name_flat(&cm.class_name);
        let mut inner = HashMap::new();
        for (j, mname) in cm.method_names.iter().enumerate() {
            if !cm.is_class_methods[j] {
                if let Some((idx, _)) = global_meta.get(mname) {
                    let rt = cm.method_return_types.get(j).cloned().unwrap_or_else(|| "NPObject *".into());
                    let params = cm.method_params_list.get(j)
                        .map(|p| p.iter().map(|(pt, _)| pt.clone()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_else(|| "NPObject *, SEL".into());
                    let ellipsis = if cm.method_variadic.get(j).copied().unwrap_or(false) { ", ..." } else { "" };
                    let ptr_type = format!("{} (*)({}{})", rt, params, ellipsis);
                    inner.insert(mname.clone(), (*idx, ptr_type));
                }
            }
        }
        class_method_meta.insert(flat, inner);
    }
    CLASS_METHOD_METADATA.set(class_method_meta).unwrap();

    // Class methods need their own table (see CLASS_METHOD_PARAMS): the one
    // above holds instance methods only, so class-method sends found no
    // parameter types and emitted their arguments uncast.
    let mut class_method_params: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for cm in &classes {
        let flat = name_flat(&cm.class_name);
        let mut inner = HashMap::new();
        for (j, mname) in cm.method_names.iter().enumerate() {
            if cm.is_class_methods[j] {
                let params = cm.method_params_list.get(j)
                    .map(|p| p.iter().map(|(pt, _)| pt.clone()).collect::<Vec<_>>())
                    .unwrap_or_else(|| vec!["NPClass *".into(), "SEL".into()]);
                inner.insert(mname.clone(), params);
            }
        }
        class_method_params.insert(flat, inner);
    }
    CLASS_METHOD_PARAMS.set(class_method_params).unwrap();

    // `no_arc` stays false here: this entry point feeds the header/prototype
    // paths, which never emit the ARC dealloc wrappers (that decision belongs
    // to the pipeline, which sets the flag on its own CgUnit).
    // Protocol declarations for the static metadata table (D5.2): collect
    // every @protocol in the unit with its parents and required/optional
    // selector lists (binder-recorded order preserved by the elaborator).
    let mut protocol_table: Vec<(String, Vec<String>, Vec<String>, Vec<String>)> = Vec::new();
    fn collect_protocols(decls: &[AstDecl], out: &mut Vec<(String, Vec<String>, Vec<String>, Vec<String>)>) {
        for d in decls {
            match &d.data {
                AstDeclData::Namespace(inner) => collect_protocols(inner, out),
                AstDeclData::Protocol { parents, required_methods, optional_methods } => {
                    if let Some(ref name) = d.name {
                        out.push((name.clone(), parents.clone(), required_methods.clone(), optional_methods.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    collect_protocols(&ast.decls, &mut protocol_table);
    // Stable manifest for the driver: sorted + deduped so two TUs that demand
    // the same specializations compare equal and the fixed-point loop stops.
    generic_demands.sort();
    generic_demands.dedup();
    let mut unit = CgUnit { decls, filename: ast.filename.clone(), c_headers: Vec::new(), selectors, classes, global_instance_method_names, struct_eq_tags: Vec::new(), no_arc: false, owned_classes: std::collections::HashSet::new(), instantiate_only: false, vtable_sig_names, kvc: false, protocols: protocol_table, generic_demands };
    // The authoritative record of which method function bodies exist: every
    // `CgDeclData::Function` with a body. This covers paths that do not go
    // through `ClassInfo::method_bodies` — notably @property-synthesised
    // getters/setters — which the earlier `method_bodies`-based bookkeeping
    // missed (leaving those vtable slots NULL and the program misbehaving).
    if let Some(set) = emitted_methods().as_mut() {
        for d in &unit.decls {
            if let CgDeclData::Function { body: Some(_), .. } = &d.data {
                set.insert(d.name.clone());
            }
        }
    }
    rewrite_block_var_refs(&mut unit);
    rewrite_weak_assigns(&mut unit);
    unit
}

fn rewrite_block_var_refs(unit: &mut CgUnit) {
    // Collect `__block` variable names declared INSIDE one function body.
    // Capture rewriting is function-granular: a block literal can only capture
    // variables of its *enclosing* function, so a TU-wide name set would
    // corrupt same-named params/locals in every other function.
    fn collect_block_vars_from_stmt(stmt: &CgStmt, bv: &mut std::collections::HashSet<String>) {
        match &stmt.data {
            CgStmtData::Decl { is_block, name, .. } => {
                if *is_block { bv.insert(name.clone()); }
            }
            CgStmtData::Compound(stmts) => { for s in stmts { collect_block_vars_from_stmt(s, bv); } }
            CgStmtData::If { then, else_, .. } => {
                collect_block_vars_from_stmt(then, bv);
                if let Some(el) = else_ { collect_block_vars_from_stmt(el, bv); }
            }
            CgStmtData::Switch { body, .. } => collect_block_vars_from_stmt(body, bv),
            CgStmtData::Case { body, .. } => collect_block_vars_from_stmt(body, bv),
            CgStmtData::Default(body) => collect_block_vars_from_stmt(body, bv),
            CgStmtData::While { body, .. } => collect_block_vars_from_stmt(body, bv),
            CgStmtData::Do { body, .. } => collect_block_vars_from_stmt(body, bv),
            CgStmtData::For { init, body, .. } => {
                if let Some(i) = init { collect_block_vars_from_stmt(i, bv); }
                collect_block_vars_from_stmt(body, bv);
            }
            CgStmtData::ForIn { body, .. } => collect_block_vars_from_stmt(body, bv),
            _ => {}
        }
    }
    fn rewrite_expr(e: &mut CgExpr, bv: &std::collections::HashSet<String>) {
        match &mut e.data {
            CgExprData::Ident(name) => {
                if bv.contains(name.as_str()) {
                    let name_clone = name.clone();
                    *e = CgExpr {
                        kind: CgExprKind::Arrow,
                        type_str: None,
                        line: e.line, col: e.col,
                        data: CgExprData::Arrow {
                            obj: Box::new(CgExpr {
                                kind: CgExprKind::Member,
                                type_str: None,
                                line: e.line, col: e.col,
                                data: CgExprData::Member {
                                    obj: Box::new(CgExpr {
                                        kind: CgExprKind::Ident,
                                        type_str: None,
                                        line: e.line, col: e.col,
                                        data: CgExprData::Ident(name_clone),
                                    }),
                                    field: "__forwarding".into(),
                                },
                            }),
                            field: "__value".into(),
                        },
                    };
                }
            }
            CgExprData::Unary { operand, .. } => rewrite_expr(operand, bv),
            CgExprData::Binary { left, right, .. } => { rewrite_expr(left, bv); rewrite_expr(right, bv); }
            CgExprData::Assign { target, value, .. } => { rewrite_expr(target, bv); rewrite_expr(value, bv); }
            CgExprData::Cast { expr, .. } => rewrite_expr(expr, bv),
            CgExprData::Call { args, .. } => { for a in args { rewrite_expr(a, bv); } }
            CgExprData::Comma(exprs) => { for e in exprs { rewrite_expr(e, bv); } }
            CgExprData::Member { obj, .. } => rewrite_expr(obj, bv),
            CgExprData::Arrow { obj, .. } => rewrite_expr(obj, bv),
            CgExprData::Index { arr, index } => { rewrite_expr(arr, bv); rewrite_expr(index, bv); }
            CgExprData::Ternary { cond, then, else_ } => { rewrite_expr(cond, bv); rewrite_expr(then, bv); rewrite_expr(else_, bv); }
            CgExprData::InitList(elements) => { for e in elements { rewrite_expr(e, bv); } }
            // A parenthesized use — `(weakref != NULL)` — must be descended
            // into too. Without this arm the rewrite stopped at the parens and
            // left the byref cell compared against NULL (a struct vs pointer).
            CgExprData::Paren(inner) => rewrite_expr(inner, bv),
            CgExprData::BlockLit(data) => {
                if let Some(ref mut body) = data.body {
                    rewrite_stmt(body, bv);
                }
            }
            _ => {}
        }
    }
    fn rewrite_stmt(s: &mut CgStmt, bv: &std::collections::HashSet<String>) {
        match &mut s.data {
            CgStmtData::Expr(e) => rewrite_expr(e, bv),
            CgStmtData::Compound(stmts) => { for st in stmts { rewrite_stmt(st, bv); } }
            CgStmtData::If { cond, then, else_ } => { rewrite_expr(cond, bv); rewrite_stmt(then, bv); if let Some(el) = else_ { rewrite_stmt(el, bv); } }
            CgStmtData::Switch { expr, body } => { rewrite_expr(expr, bv); rewrite_stmt(body, bv); }
            CgStmtData::Case { value, body } => { rewrite_expr(value, bv); rewrite_stmt(body, bv); }
            CgStmtData::Default(body) => rewrite_stmt(body, bv),
            CgStmtData::While { cond, body } => { rewrite_expr(cond, bv); rewrite_stmt(body, bv); }
            CgStmtData::Do { body, cond } => { rewrite_stmt(body, bv); rewrite_expr(cond, bv); }
            CgStmtData::For { init, cond, incr, body } => {
                if let Some(i) = init { rewrite_stmt(i, bv); }
                if let Some(c) = cond { rewrite_expr(c, bv); }
                if let Some(u) = incr { rewrite_expr(u, bv); }
                rewrite_stmt(body, bv);
            }
            CgStmtData::ForIn { collection, body, .. } => { rewrite_expr(collection, bv); rewrite_stmt(body, bv); }
            CgStmtData::Return(v) => { if let Some(e) = v { rewrite_expr(e, bv); } }
            CgStmtData::Decl { init, next, .. } => {
                if let Some(i) = init { rewrite_expr(i, bv); }
                for (_, _, i) in next { if let Some(ex) = i { rewrite_expr(ex, bv); } }
            }
            _ => {}
        }
    }
    // File-scope `__block` variables (rare) — their initializers may reference
    // nothing capturable, but keep the old behavior of rewriting their inits.
    let file_scope_vars: std::collections::HashSet<String> = unit.decls.iter()
        .filter_map(|d| {
            if let CgDeclData::Variable { is_block, .. } = &d.data {
                if *is_block { Some(d.name.clone()) } else { None }
            } else { None }
        })
        .collect();
    for decl in &mut unit.decls {
        match &mut decl.data {
            CgDeclData::Function { params, body, .. } => {
                let Some(ref mut b) = body else { continue };
                // Collect only THIS function's own __block declarations.
                let mut block_vars = std::collections::HashSet::new();
                collect_block_vars_from_stmt(b, &mut block_vars);
                if block_vars.is_empty() { continue; }
                // Shadow guard: a parameter with the same name as a __block
                // variable shadows it inside this function — rewriting its
                // references would corrupt the parameter. Skip those names.
                for (pname, _) in params.iter() {
                    if block_vars.contains(pname) {
                        block_vars.remove(pname);
                        eprintln!("warning: parameter '{}' shadows a __block variable; block capture rewriting skipped for it in this function", pname);
                    }
                }
                if block_vars.is_empty() { continue; }
                rewrite_stmt(b, &block_vars);
            }
            CgDeclData::Variable { init, next, is_block, .. } => {
                // A file-scope __block variable's own initializer must not be
                // rewritten against itself.
                if let Some(ref mut i) = init {
                    let own = if *is_block { Some(decl.name.clone()) } else { None };
                    let bv = if let Some(o) = &own {
                        let mut s = file_scope_vars.clone();
                        s.remove(o);
                        s
                    } else { file_scope_vars.clone() };
                    if !bv.is_empty() { rewrite_expr(i, &bv); }
                }
                for (_, _, i) in next.iter_mut() {
                    if let Some(ref mut ex) = i { rewrite_expr(ex, &file_scope_vars); }
                }
            }
            _ => {}
        }
    }
}

/// Zeroing-weak assignment rewrite for local `__weak` variables (M1).
///
/// A weak local is registered with the runtime at declaration time (the Decl
/// emitter emits `jeti_weakRegister((NPObject **)&name, (NPObject *)init)`),
/// but a later plain assignment (`weakref = strong`) bypassed the weak table:
/// the slot stayed registered against the old target (often the initial
/// NULL), so deallocating the newly-assigned target never zeroed the
/// variable — `__weak` silently behaved as a plain pointer (full_syntax_test
/// §4.16/4.17 printed `weakzero 0`; GPT review finding #2).
///
/// This pass rewrites statement-level assignments to weak locals into the
/// same unregister → assign → register sequence the weak-ivar setter path
/// emits. The RHS is evaluated once into a `__auto_type` temp (eh/pattern
/// pass precedent) so a message-send RHS cannot double-evaluate.
///
/// Function-granular, like the `__block` capture rewrite: collect each
/// function's own weak declarations first, rewrite only that body. M1 limits
/// (documented): block-literal bodies are not rewritten (block capture is a
/// separate mechanism); assignments nested inside larger expressions
/// (`if ((w = x))`) stay plain; a same-named non-weak shadow inside the
/// function would be collected as weak (flat function granularity).
fn rewrite_weak_assigns(unit: &mut CgUnit) {
    for decl in &mut unit.decls {
        if let CgDeclData::Function { body, .. } = &mut decl.data {
            let Some(ref mut b) = body else { continue };
            let mut weak_names: std::collections::HashSet<String> = std::collections::HashSet::new();
            collect_weak_decl_names(b, &mut weak_names);
            if weak_names.is_empty() { continue; }
            rewrite_weak_stmt(b, &weak_names);
        }
    }
}

fn collect_weak_decl_names(stmt: &CgStmt, weak: &mut std::collections::HashSet<String>) {
    match &stmt.data {
        CgStmtData::Decl { name, is_weak, .. } => {
            if *is_weak { weak.insert(name.clone()); }
        }
        CgStmtData::Compound(v) => v.iter().for_each(|s| collect_weak_decl_names(s, weak)),
        CgStmtData::If { then, else_, .. } => {
            collect_weak_decl_names(then, weak);
            if let Some(e) = else_ { collect_weak_decl_names(e, weak); }
        }
        CgStmtData::Switch { body, .. }
        | CgStmtData::Case { body, .. }
        | CgStmtData::Default(body) => collect_weak_decl_names(body, weak),
        CgStmtData::While { body, .. }
        | CgStmtData::Do { body, .. }
        | CgStmtData::ForIn { body, .. } => collect_weak_decl_names(body, weak),
        CgStmtData::For { init, body, .. } => {
            if let Some(i) = init { collect_weak_decl_names(i, weak); }
            collect_weak_decl_names(body, weak);
        }
        _ => {}
    }
}

fn rewrite_weak_stmt(stmt: &mut CgStmt, weak: &std::collections::HashSet<String>) {
    // Compute the replacement in a separate variable: the match borrows
    // stmt.data, so *stmt can only be assigned after the match ends.
    let mut replacement: Option<CgStmt> = None;
    match &mut stmt.data {
        CgStmtData::Expr(e) => {
            let extracted: Option<(String, Box<CgExpr>, usize, usize)> = match &mut e.data {
                CgExprData::Assign { target, value } => match &target.data {
                    CgExprData::Ident(n) if weak.contains(n.as_str()) => {
                        let line = e.line; let col = e.col;
                        let dummy = Box::new(CgExpr { kind: CgExprKind::Int, type_str: None, line, col, data: CgExprData::Int(0) });
                        Some((n.clone(), std::mem::replace(value, dummy), line, col))
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some((name, value, line, col)) = extracted {
                replacement = Some(build_weak_assign_stmts(&name, value, line, col));
            }
        }
        CgStmtData::Compound(v) => v.iter_mut().for_each(|s| rewrite_weak_stmt(s, weak)),
        CgStmtData::If { then, else_, .. } => {
            rewrite_weak_stmt(then, weak);
            if let Some(e) = else_ { rewrite_weak_stmt(e, weak); }
        }
        CgStmtData::Switch { body, .. }
        | CgStmtData::Case { body, .. }
        | CgStmtData::Default(body) => rewrite_weak_stmt(body, weak),
        CgStmtData::While { body, .. }
        | CgStmtData::Do { body, .. }
        | CgStmtData::ForIn { body, .. } => rewrite_weak_stmt(body, weak),
        CgStmtData::For { init, body, .. } => {
            if let Some(i) = init { rewrite_weak_stmt(i, weak); }
            rewrite_weak_stmt(body, weak);
        }
        _ => {}
    }
    if let Some(new_stmt) = replacement {
        *stmt = new_stmt;
    }
}

/// Build `{ __auto_type tmp = <value>; jeti_weakUnregister((NPObject **)&w);
/// w = tmp; jeti_weakRegister((NPObject **)&w, (NPObject *)tmp); }` — the
/// statement-level analogue of the weak-ivar setter's comma sequence.
fn build_weak_assign_stmts(name: &str, value: Box<CgExpr>, line: usize, col: usize) -> CgStmt {
    let tmp = format!("__jeti_weak_val_{}", next_temp_id());
    let ident = |n: &str| CgExpr {
        kind: CgExprKind::Ident, type_str: None, line, col,
        data: CgExprData::Ident(n.to_string()),
    };
    let cast_addr = CgExpr {
        kind: CgExprKind::Cast, type_str: None, line, col,
        data: CgExprData::Cast {
            target_type: "NPObject **".into(),
            expr: Box::new(CgExpr {
                kind: CgExprKind::Unary, type_str: None, line, col,
                data: CgExprData::Unary { op_str: "&".into(), operand: Box::new(ident(name)), is_postfix: false },
            }),
        },
    };
    let mk_call = |fname: &str, args: Vec<CgExpr>| CgStmt {
        kind: CgStmtKind::Expr, line, col,
        data: CgStmtData::Expr(CgExpr {
            kind: CgExprKind::Call, type_str: Some("void".into()), line, col,
            data: CgExprData::Call {
                name: fname.into(), args,
                vtable_class: None, alt_vtable_classes: vec![],
                is_class_method: false, is_super: false,
                sel_const_name: None, method_index: None, direct_specialized: false,
            },
        }),
    };
    CgStmt {
        kind: CgStmtKind::Compound, line, col,
        data: CgStmtData::Compound(vec![
            // 1. Evaluate the RHS exactly once.
            CgStmt {
                kind: CgStmtKind::Decl, line, col,
                data: CgStmtData::Decl {
                    decl_type: "__auto_type".into(),
                    name: tmp.clone(),
                    init: Some(value),
                    array_suffix: None, is_static: false, is_weak: false, is_block: false,
                    next: vec![], attributes: vec![],
                },
            },
            // 2. Detach the slot from its previous target.
            mk_call("jeti_weakUnregister", vec![cast_addr]),
            // 3. The assignment itself.
            CgStmt {
                kind: CgStmtKind::Expr, line, col,
                data: CgStmtData::Expr(CgExpr {
                    kind: CgExprKind::Assign, type_str: None, line, col,
                    data: CgExprData::Assign {
                        target: Box::new(ident(name)),
                        value: Box::new(ident(&tmp)),
                    },
                }),
            },
            // 4. Re-register against the new target.
            mk_call("jeti_weakRegister", vec![
                CgExpr {
                    kind: CgExprKind::Cast, type_str: None, line, col,
                    data: CgExprData::Cast {
                        target_type: "NPObject **".into(),
                        expr: Box::new(CgExpr {
                            kind: CgExprKind::Unary, type_str: None, line, col,
                            data: CgExprData::Unary { op_str: "&".into(), operand: Box::new(ident(name)), is_postfix: false },
                        }),
                    },
                },
                CgExpr {
                    kind: CgExprKind::Cast, type_str: None, line, col,
                    data: CgExprData::Cast {
                        target_type: "NPObject *".into(),
                        expr: Box::new(ident(&tmp)),
                    },
                },
            ]),
        ]),
    }
}

// ─── C code emission ─────────────────────────────────────────────────────────

// Emit: "((RETURN (*)(PARAMS))" — the opening of a function-pointer cast
// wrapping the vtable member access. Returns true if a cast was emitted.
fn emit_vtable_fp_cast(out: &mut String, vc_flat: &str, name: &str) -> bool {
    if let Some(meta) = CLASS_METHOD_METADATA.get().and_then(|c| c.get(vc_flat)).and_then(|m| m.get(name)) {
        let (_, ref ptr_type) = *meta;
        if let Some(paren) = ptr_type.rfind("(*)") {
            let before = &ptr_type[..paren + 2]; // "return_type (*"
            let after = &ptr_type[paren + 2..];  // ")(param1, param2, ...)"
            let _ = write!(out, "(({}{})", before, after);
            return true;
        }
    }
    false
}

/// Return type (as C string) of a vtable method, derived from its metadata
/// fn-pointer type `"return_type (*)(params)"` → `"return_type"`.
fn vtable_return_type(vc_flat: &str, name: &str) -> Option<String> {
    CLASS_METHOD_METADATA.get()
        .and_then(|c| c.get(vc_flat))
        .and_then(|m| m.get(name))
        .map(|(_, ptr_type)| {
            ptr_type.split("(*)").next().unwrap_or(ptr_type).trim().to_string()
        })
}

/// Pick the nil-messaging fallback value for a message send based on its
/// (C string) return type. `[nil msg]` returns zero for the message's return
/// type. For void/pointers a plain `0` is a valid null/zero; for struct/union
/// returns we must use a compound literal `(T){0}` so the `cond ? <call> : <fb>`
/// operands are type-compatible.
fn nil_msg_fallback(type_str: Option<&str>) -> String {
    match type_str {
        None => "0".to_string(),
        Some(t) => {
            let t = t.trim();
            if t.is_empty() || t == "void" || t.ends_with('*') || t.contains('(') || t.contains('[') {
                "0".to_string()
            } else {
                format!("({}){{0}}", t)
            }
        }
    }
}

/// Emit an if/while condition without the redundant outer parenthesis
/// layer. Binary sub-expressions always parenthesize themselves (bug #6:
/// the precedence guard), so the usual shell doubles up: `if ((x == y))`
/// clang reads as an `=` typo and warns -Wparentheses-equality (which the
/// removed pipeline `-w` used to bury). Nested binaries keep their own
/// parens — only the condition's outer shell is dropped.
fn emit_condition(cond: &CgExpr, out: &mut String) {
    match &cond.data {
        CgExprData::Binary { left, op_str, right } => {
            emit_expr(left, out);
            out.push(' ');
            out.push_str(op_str);
            out.push(' ');
            emit_expr(right, out);
        }
        _ => emit_expr(cond, out),
    }
}

fn emit_expr(e: &CgExpr, out: &mut String) {
    match &e.data {
        CgExprData::Int(val) => {
            let _ = write!(out, "{}", val);
            // Suffix-carrying literals must keep their suffix in the emitted
            // C: `1LL << 60` printed as `1 << 60` shifts in `int` (UB) and
            // corrupts the value before any widening. The suffix decision
            // comes from the parser-annotated literal type, not the value.
            if let Some(t) = e.type_str.as_deref().map(str::trim) {
                let suf = match t {
                    "long" => "L",
                    "long long" => "LL",
                    "unsigned" => "U",
                    "unsigned long" => "UL",
                    "unsigned long long" => "ULL",
                    _ => "",
                };
                if !suf.is_empty() && *val >= 0 {
                    let _ = write!(out, "{}", suf);
                }
            }
        }
        CgExprData::Float(val) => {
            let s = format!("{}", val);
            if s.contains('.') || s.contains('e') || s.contains('E') {
                let _ = write!(out, "{}f", s);
            } else {
                let _ = write!(out, "{}.0f", s);
            }
        }
        CgExprData::FloatRaw(raw) => {
            // Imaginary literal (`2.0i`) — emit verbatim so the C compiler
            // sees the imaginary part; the f64 path would drop it.
            let _ = write!(out, "{}", raw);
        }
        CgExprData::String(s) => { let _ = write!(out, "\"{}\"", s); }
        CgExprData::Char(val) => {
            let escaped = match *val {
                10 => "\\n".to_string(),
                9 => "\\t".to_string(),
                13 => "\\r".to_string(),
                92 => "\\\\".to_string(),
                39 => "\\'".to_string(),
                34 => "\\\"".to_string(),
                c if (32..=126).contains(&c) => format!("{}", c as char),
                c => format!("\\x{:02X}", c),
            };
            let _ = write!(out, "'{}'", escaped);
        }
        CgExprData::Ident(s) => { out.push_str(s); }
        CgExprData::Raw(s) => { out.push_str(s); }
        CgExprData::Unary { op_str, operand, is_postfix } => {
            if *is_postfix {
                out.push('(');
                emit_expr(operand, out);
                out.push(')');
                out.push_str(op_str);
            } else if op_str == "sizeof" {
                // sizeof needs parentheses around its operand for correct precedence
                out.push_str("sizeof(");
                emit_expr(operand, out);
                out.push(')');
            } else {
                out.push_str(op_str);
                emit_expr(operand, out);
            }
        }
        CgExprData::Binary { op_str, left, right } => {
            // Always parenthesise binary sub-expressions. Previously == and !=
            // were emitted without parens (to avoid -Wparentheses-equality
            // noise inside if()/while()), which silently changed precedence
            // when a comparison appeared as an operand of an arithmetic chain:
            // `a + b + (a != 0)` emitted as `((a + b) + a != 0)`. (bug #6)
            // Clang's -Wparentheses warnings are suppressed by the pipeline's
            // `-w`, and correctness beats warning cosmetics.
            out.push('(');
            emit_expr(left, out);
            out.push(' ');
            out.push_str(op_str);
            out.push(' ');
            emit_expr(right, out);
            out.push(')');
        }
        CgExprData::Assign { target, value } => {
            emit_expr(target, out);
            out.push_str(" = ");
            // Cast the value when target is a subclass pointer and value is a vtable call
            if let Some(ttype) = &target.type_str {
                if ttype.ends_with(" *") && ttype != "NPObject *" && matches!(value.data, CgExprData::Call { .. }) {
                    let _ = write!(out, "({})(", ttype.trim_end());
                    emit_expr(value, out);
                    out.push(')');
                    return;
                }
                // Same call, but the value is not a (fn-ptr-cast) Call — or
                // the target lies on the id side: spell the implicit
                // id↔class-pointer join ObjC allows and plain C does not.
                if let Some(vt) = value.type_str.as_deref() {
                    if let Some(cast) = obj_wildcard_cast(ttype, vt) {
                        let _ = write!(out, "({})(", cast);
                        emit_expr(value, out);
                        out.push(')');
                        return;
                    }
                }
            }
            emit_expr(value, out);
        }
        CgExprData::Cast { target_type, expr } => {
            let _ = write!(out, "({})", target_type);
            emit_expr(expr, out);
        }
        CgExprData::Call { name, args, vtable_class, alt_vtable_classes: _, is_class_method, is_super, sel_const_name, method_index: _, direct_specialized } => {
            // True monomorphization: the receiver's STATIC type named a
            // generic instantiation whose clone carries the substituted
            // signature. Call the clone's function directly — no vtable
            // lookup, no erased `NPObject *` slot in the path. The receiver
            // still passes as the first argument (the clone's `_self` is the
            // substituted class pointer). The nil guard mirrors the instance
            // send so `[nil value]` stays a safe no-op; the cast spelling
            // matches the instance arm ((NPObject *) around the receiver).
            if *direct_specialized {
                if let Some(vc) = vtable_class {
                    let flat = name_flat(vc);
                    let tid = next_temp_id();
                    let _ = write!(out, "({{ NPObject *__jeti_tmp_{} = ((NPObject *)(", tid);
                    if !args.is_empty() {
                        emit_expr(&args[0], out);
                    } else {
                        out.push_str("0");
                    }
                    let _ = write!(out, ")); __jeti_tmp_{} ? ", tid);
                    let _ = write!(out, "{}_{}(__jeti_tmp_{}", flat, name, tid);
                    let _ = write!(out, ", {}", sel_const_name.as_deref().unwrap_or("0"));
                    for (ai, arg) in args[1..].iter().enumerate() {
                        out.push_str(", ");
                        // The clone's substituted parameter types are the
                        // ABI-correct spellings — cast plain `NPObject *`
                        // temps (and other joins) exactly like the instance
                        // arm does for the uniform vtable.
                        let param_idx = ai + 2;
                        if let Some(pt) = get_vtable_param_type_for_class(&flat, name, param_idx) {
                            if pt.ends_with('*') && !pt.trim_start().starts_with("const char") && !pt.trim_start().starts_with("char ") {
                                let _ = write!(out, "({})(", pt);
                                emit_expr(arg, out);
                                out.push(')');
                                continue;
                            }
                        }
                        emit_expr(arg, out);
                    }
                    out.push(')');
                    // Statement-expression shape mirrors the instance arm:
                    // the `;` inside the braces and `})` closing are what make
                    // `({...})` a valid GNU statement expression. The nil
                    // fallback types from the CLONE's substituted return (the
                    // ABI-correct spelling) — the checker's erased/int view
                    // of a same-selector collision must not truncate the
                    // object pointer the clone actually returns.
                    let fb = nil_msg_fallback(
                        vtable_return_type(&flat, name).or_else(|| e.type_str.clone()).as_deref(),
                    );
                    let _ = write!(out, " : {}; }})", fb);
                    return;
                }
            }
            if *is_super {
                let sel = sel_const_name.as_deref().unwrap_or("0");
                let cls_flat = vtable_class.as_deref().map(|c| name_flat(c)).unwrap_or_else(|| "NPObject".to_string());
                if *is_class_method {
                    // Super CLASS method (e.g. `[super alloc]` inside a class
                    // method): dispatch through the parent class's META vtable
                    // instance (class methods live there, not on the uniform
                    // instance vtable — which has no such members and produced
                    // invalid C). `self` in a class method is the NPClass*
                    // receiver; the parent's meta vtable instance is statically
                    // known (Foundation ancestors are always inlined into the TU).
                    let _ = write!(out, "(&{}_inst)->{}(", meta_symbol("META_VTABLE_", &cls_flat), name);
                    if !args.is_empty() { emit_expr(&args[0], out); }
                    let _ = write!(out, ", {}", sel);
                    for (i, arg) in args[1..].iter().enumerate() {
                        out.push_str(", ");
                        let param_idx = i + 2;
                        if let Some(pt) = get_vtable_param_type_for_class(&cls_flat, name, param_idx) {
                            if pt.ends_with('*') && !pt.trim_start().starts_with("const char") && !pt.trim_start().starts_with("char ") {
                                let _ = write!(out, "({})(", pt);
                                emit_expr(arg, out);
                                out.push_str(")");
                                continue;
                            }
                        }
                        emit_expr(arg, out);
                    }
                    out.push(')');
                } else {
                // Super call: direct vtable instance access (typed struct member).
                // Uses the superclass's vtable instance, NOT self->isa->superclass,
                // to avoid infinite recursion with subclass runtime type.
                let _ = write!(out, "(&{})->{}(", meta_symbol("VTABLE_", &cls_flat), name);
                if !args.is_empty() { emit_expr(&args[0], out); }
                let _ = write!(out, ", {}", sel);
                for (i, arg) in args[1..].iter().enumerate() {
                    out.push_str(", ");
                    let param_idx = i + 2;
                    if let Some(pt) = get_vtable_param_type_for_class(&cls_flat, name, param_idx) {
                        if pt.ends_with('*') && !pt.trim_start().starts_with("const char") && !pt.trim_start().starts_with("char ") {
                            let _ = write!(out, "({})(", pt);
                            emit_expr(arg, out);
                            out.push_str(")");
                            continue;
                        }
                    }
                    emit_expr(arg, out);
                }
                out.push(')');
                }
            } else if let Some(vc) = vtable_class {
                let vc_flat = name_flat(vc);
                if *is_class_method {
                    // Class method: direct function call
                    let _ = write!(out, "{}_{}(", vc_flat, name);
                    if !args.is_empty() {
                        emit_expr(&args[0], out);
                        let _ = write!(out, ", {}", sel_const_name.as_deref().unwrap_or("0"));
                        for (i, arg) in args[1..].iter().enumerate() {
                            out.push_str(", ");
                            let param_idx = i + 2;
                            if let Some(pt) = get_vtable_param_type_for_class(&vc_flat, name, param_idx) {
                                if pt.ends_with('*') && !pt.trim_start().starts_with("const char") && !pt.trim_start().starts_with("char ") {
                                    let _ = write!(out, "({})(", pt);
                                    emit_expr(arg, out);
                                    out.push_str(")");
                                    continue;
                                }
                            }
                            emit_expr(arg, out);
                        }
                    }
                    out.push(')');
                } else {
                    // Instance method: uniform vtable member access through isa.
                    // ((struct jeti_vtable *)receiver->isa->vtable)->method(args)
                    let sel = sel_const_name.as_deref().unwrap_or("0");
                    // Instance message send: guard the receiver against nil so
                    // `[nil msg]` is a safe no-op returning 0/nil, matching ObjC
                    // nil-messaging semantics. The receiver is evaluated once into
                    // a temp, then dispatched only if non-nil:
                    //   ({ NPObject *__jeti_tmp_N = ((NPObject *)(recv));
                    //      __jeti_tmp_N ? <dispatch>(__jeti_tmp_N, sel, ...) : 0; })
                    // This works for both value-returning and void-returning sends.
                    let tid = next_temp_id();
                    let _ = write!(out, "({{ NPObject *__jeti_tmp_{} = ((NPObject *)(", tid);
                    if !args.is_empty() {
                        emit_expr(&args[0], out);
                        out.push_str(")");
                    } else { out.push_str("0)"); }
                    let _ = write!(out, "); __jeti_tmp_{} ? ", tid);
                    let has_cast = emit_vtable_fp_cast(out, &vc_flat, name);
                    let _ = write!(out, "((struct jeti_vtable *)__jeti_tmp_{}->isa->vtable)->{}", tid, name);
                    if has_cast { out.push(')'); }
                    let _ = write!(out, "(__jeti_tmp_{}", tid);
                    let _ = write!(out, ", {}", sel);
                    for (i, arg) in args[1..].iter().enumerate() {
                        out.push_str(", ");
                        let param_idx = i + 2;
                        if let Some(pt) = get_vtable_param_type_for_class(&vc_flat, name, param_idx) {
                            if pt.ends_with('*') && !pt.trim_start().starts_with("const char") && !pt.trim_start().starts_with("char ") {
                                let _ = write!(out, "({})(", pt);
                                emit_expr(arg, out);
                                out.push_str(")");
                                continue;
                            }
                        }
                        emit_expr(arg, out);
                    }
                    let fb = nil_msg_fallback(vtable_return_type(&vc_flat, name).as_deref());
                    let _ = write!(out, ") : {}; }})", fb);
                }
            } else if name == "autorelease" {
                // autorelease is a no-op in Jeti's non-ARC runtime; just return receiver
                if !args.is_empty() { emit_expr(&args[0], out); }
            } else {
                // Direct C function call
                // If sel_const_name is set, this is a message send that wasn't found in the vtable.
                // Emit as arrow access: receiver->method (ivar/property access)
                if sel_const_name.is_some() && !args.is_empty() {
                    emit_expr(&args[0], out);
                    let _ = write!(out, "->{}", name);
                } else {
                    out.push_str(name);
                    out.push('(');
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 { out.push_str(", "); }
                        emit_expr(arg, out);
                    }
                    out.push(')');
                }
            }
        }
        CgExprData::Comma(items) => {
            out.push('(');
            for (i, item) in items.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                emit_expr(item, out);
            }
            out.push(')');
        }
        CgExprData::Paren(e) => {
            // Re-emit the user's explicit grouping verbatim. Keeping these
            // parentheses is load-bearing: `(a = b) != c` must not collapse
            // into `a = b != c` (the assignment would swallow the comparison).
            out.push('(');
            emit_expr(e, out);
            out.push(')');
        }
        CgExprData::Member { obj, field } => {
            emit_expr(obj, out);
            let _ = write!(out, ".{}", field);
        }
        CgExprData::Arrow { obj, field } => {
            if obj.kind == CgExprKind::Cast {
                out.push('(');
            }
            emit_expr(obj, out);
            if obj.kind == CgExprKind::Cast {
                out.push(')');
            }
            let _ = write!(out, "->{}", field);
        }
        CgExprData::Index { arr, index } => {
            emit_expr(arr, out);
            out.push('[');
            emit_expr(index, out);
            out.push(']');
        }
        CgExprData::Ternary { cond, then, else_ } => {
            emit_expr(cond, out);
            out.push_str(" ? ");
            emit_expr(then, out);
            out.push_str(" : ");
            emit_expr(else_, out);
        }
        CgExprData::InitList(items) => {
            out.push_str("{ ");
            for (i, item) in items.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                emit_expr(item, out);
            }
            out.push_str(" }");
        }
        CgExprData::DesignatedInit { designators, expr } => {
            // C99 designated initializer entry: `.field = v` / `[i] = v` / chains.
            for d in designators {
                match d {
                    CgDesignator::Member(n) => { out.push('.'); out.push_str(n); }
                    CgDesignator::Index(ix) => {
                        out.push('[');
                        emit_expr(ix, out);
                        out.push(']');
                    }
                }
            }
            out.push_str(" = ");
            emit_expr(expr, out);
        }
        CgExprData::Sizeof { type_str, is_alignof } => {
            if *is_alignof {
                let _ = write!(out, "__alignof__({})", type_str);
            } else {
                let _ = write!(out, "sizeof({})", type_str);
            }
        }
        CgExprData::TypeLiteral(ts) => {
            out.push_str(ts);
        }
        CgExprData::BlockLit(data) => {
            if is_clang_backend() {
                // Emit block literal: `^return_type(params) { body }`
                let rt = &data.return_type;
                out.push('^');
                out.push_str(rt);
                out.push('(');
                for (i, (pt, pn)) in data.params.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    out.push_str(pt);
                    out.push(' ');
                    out.push_str(pn);
                }
                out.push(')');
                if let Some(ref body) = data.body {
                    out.push(' ');
                    emit_stmt_inline(body, out);
                } else {
                    out.push_str(" {}");
                }
            } else {
                // gcc/portable: use-site is a compound-literal struct initializer.
                // The struct + invoke definitions were already emitted in the
                // block_defs buffer during convert_expr.
                let tid = data.func_name.trim_start_matches("__jeti_block_").to_string();
                let _ = write!(out, "(struct __jeti_block_header *)&(struct __jeti_block_layout_{}){{ .isa=NULL, .flags=0, .reserved=0, .invoke={} }}", tid, data.func_name);
            }
        }
    }
}

// Emits a control-flow body. A compound body opens `{` on the same line as the
// preceding `) ` / `do ` (no re-indent gap); a single-statement body is emitted
// directly.
fn emit_body_inline(body: &CgStmt, out: &mut String, indent: usize) {
    match &body.data {
        CgStmtData::Compound(stmts) => {
            out.push_str("{\n");
            for st in stmts {
                emit_stmt(st, out, indent + 1);
            }
            out.push_str(&"    ".repeat(indent));
            out.push_str("}\n");
        }
        _ => emit_stmt(body, out, indent),
    }
}

// ─── Inline assembly emission ──────────────────────────────────────────────

fn emit_asm_operands(ops: &[CgAsmOperand], out: &mut String) {
    for (i, op) in ops.iter().enumerate() {
        if i > 0 { out.push_str(", "); }
        if let Some(ref n) = op.name {
            let _ = write!(out, "[{}] ", n);
        }
        let _ = write!(out, "\"{}\"(", op.constraint);
        emit_expr(&op.expr, out);
        out.push_str(")");
    }
}

fn emit_asm_syntax(is_volatile: bool, is_goto: bool, template: &str, outputs: &[CgAsmOperand], inputs: &[CgAsmOperand], clobbers: &[String], labels: &[String], out: &mut String, ind: &str) {
    out.push_str(ind);
    out.push_str("__asm__");
    if is_volatile { out.push_str(" __volatile__"); }
    if is_goto { out.push_str(" goto"); }
    out.push_str(" (\"");
    out.push_str(template);
    out.push('"');
    if !outputs.is_empty() || !inputs.is_empty() || !clobbers.is_empty() || !labels.is_empty() {
        out.push_str(" : ");
        emit_asm_operands(outputs, out);
        out.push_str(" : ");
        emit_asm_operands(inputs, out);
        out.push_str(" : ");
        for (i, c) in clobbers.iter().enumerate() {
            if i > 0 { out.push_str(", "); }
            let _ = write!(out, "\"{}\"", c);
        }
        if !labels.is_empty() {
            out.push_str(" : ");
            for (i, l) in labels.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                out.push_str(l);
            }
        }
    }
    out.push_str(");\n");
}

/// Emit a statement as a single line (no newlines) — used for clang block
/// literal bodies so they don't break the enclosing expression formatting.
fn emit_stmt_inline(s: &CgStmt, out: &mut String) {
    match &s.data {
        CgStmtData::Compound(stmts) => {
            out.push_str("{ ");
            for st in stmts {
                emit_stmt_inline(st, out);
                out.push(' ');
            }
            out.push_str("}");
        }
        CgStmtData::Expr(e) => {
            emit_expr(e, out);
            out.push(';');
        }
        CgStmtData::Return(v) => {
            out.push_str("return");
            if let Some(e) = v {
                out.push(' ');
                emit_expr(e, out);
            }
            out.push(';');
        }
        CgStmtData::Break => out.push_str("break;"),
        CgStmtData::Continue => out.push_str("continue;"),
        CgStmtData::Decl { decl_type, name, init, array_suffix, is_static, next, .. } => {
            if *is_static { out.push_str("static "); }
            out.push_str(decl_type);
            out.push(' ');
            out.push_str(name);
            // Array suffix (`[N]`) must be preserved in inline (block-body)
            // declarations too — dropping it turned `char buf[128]` into
            // `char buf` and corrupted every block-local array.
            if let Some(suffix) = array_suffix { out.push_str(suffix); }
            if let Some(i) = init {
                out.push_str(" = ");
                emit_expr(i, out);
            }
            for (n, n_type, i) in next {
                match n_type {
                    t if !t.is_empty() => {
                        out.push_str("; ");
                        out.push_str(t);
                        out.push(' ');
                    }
                    _ => { out.push_str(", "); }
                }
                out.push_str(n);
                if let Some(i) = i {
                    out.push_str(" = ");
                    emit_expr(i, out);
                }
            }
            out.push(';');
        }
        CgStmtData::If { cond, then, else_ } => {
            out.push_str("if (");
            emit_condition(cond, out);
            out.push_str(") ");
            emit_stmt_inline(then, out);
            if let Some(el) = else_ {
                out.push_str(" else ");
                emit_stmt_inline(el, out);
            }
        }
        CgStmtData::While { cond, body } => {
            out.push_str("while (");
            emit_condition(cond, out);
            out.push_str(") ");
            emit_stmt_inline(body, out);
        }
        CgStmtData::Do { body, cond } => {
            out.push_str("do ");
            emit_stmt_inline(body, out);
            out.push_str(" while (");
            emit_condition(cond, out);
            out.push_str(");");
        }
        CgStmtData::Empty => out.push_str(";"),
        // Unsupported inline forms: fall back to multi-line emission then strip newlines.
        _ => {
            let mut buf = String::new();
            emit_stmt(s, &mut buf, 0);
            buf.truncate(buf.trim_end().len());
            out.push_str(&buf.replace('\n', " "));
        }
    }
}

pub fn emit_stmt(s: &CgStmt, out: &mut String, indent: usize) {
    maybe_emit_line(s.line, out);
    let ind = "    ".repeat(indent);
    match &s.data {
        CgStmtData::Expr(e) => {
            out.push_str(&ind);
            emit_expr(e, out);
            out.push_str(";\n");
        }
        CgStmtData::Compound(stmts) => {
            out.push_str(&ind);
            out.push_str("{\n");
            for st in stmts {
                emit_stmt(st, out, indent + 1);
            }
            out.push_str(&ind);
            out.push_str("}\n");
        }
        CgStmtData::If { cond, then, else_ } => {
            out.push_str(&ind);
            out.push_str("if (");
            emit_condition(cond, out);
            out.push_str(") ");
            emit_body_inline(then, out, indent);
            if let Some(el) = else_ {
                out.push_str(&ind);
                out.push_str("else ");
                emit_body_inline(el, out, indent);
            }
        }
        CgStmtData::While { cond, body } => {
            out.push_str(&ind);
            out.push_str("while (");
            emit_condition(cond, out);
            out.push_str(") ");
            emit_body_inline(body, out, indent);
        }
        CgStmtData::Do { body, cond } => {
            out.push_str(&ind);
            out.push_str("do ");
            emit_body_inline(body, out, indent);
            out.push_str(&ind);
            out.push_str("while (");
            emit_condition(cond, out);
            out.push_str(");\n");
        }
        CgStmtData::For { init, cond, incr, body } => {
            // If the init is a Decl whose init is an alloc+init message send, the
            // Decl emitter splits it into two statements (temp + var), which is
            // invalid inside a for header. Hoist the statements out before the loop
            // and leave the for-init empty.
            let hoisted = if let Some(i) = init {
                if let CgStmtData::Decl { init: Some(di), .. } = &i.data {
                    if let CgExprData::Call { args, .. } = &di.data {
                        args.first().map_or(false, |a| matches!(a.kind, CgExprKind::Call))
                    } else { false }
                } else { false }
            } else { false };

            if hoisted {
                if let Some(i) = init {
                    emit_stmt(i, out, indent);
                }
                with_line_suppressed(|| {
                    out.push_str(&ind);
                    out.push_str("for (; ");
                    if let Some(c) = cond { emit_expr(c, out); }
                    out.push_str("; ");
                    if let Some(i) = incr { emit_expr(i, out); }
                    out.push_str(") ");
                });
                emit_body_inline(body, out, indent);
            } else {
                out.push_str(&ind);
                out.push_str("for (");
                if let Some(i) = init {
                    with_line_suppressed(|| {
                    // A for-header declaration list must stay ONE comma
                    // expression. `for (size_t p = lo, q = hi - 1; ...)` is
                    // legal C and the type may be repeated per declarator, so
                    // emit `T p = lo, T q = hi - 1` rather than letting the
                    // statement-level Decl emitter split it into separate
                    // `;`-terminated statements (invalid inside a for head).
                    if let CgStmtData::Decl { decl_type, name, init: dinit, array_suffix, next, .. } = &i.data {
                        let is_block_type = decl_type.contains("(^") || decl_type.contains("(*");
                        if is_block_type {
                            out.push_str(decl_type);
                        } else {
                            out.push_str(decl_type);
                            out.push(' ');
                            out.push_str(name);
                        }
                        if let Some(suffix) = array_suffix { out.push_str(suffix); }
                        if let Some(e) = dinit {
                            out.push_str(" = ");
                            emit_expr(e, out);
                        }
                        for (n_name, _n_type, n_init) in next {
                            // C declaration lists allow the type specifier
                            // only ONCE: `for (size_t p = lo, q = hi - 1; …)`
                            // is valid, repeating `size_t` is a syntax error.
                            // The parser stores a type per declarator (needed
                            // for `T *a, *b` at statement level), so drop it
                            // here and inherit the head declarator's type.
                            out.push_str(", ");
                            out.push_str(n_name);
                            if let Some(e) = n_init {
                                out.push_str(" = ");
                                emit_expr(e, out);
                            }
                        }
                        // The statement-level Decl emitter terminates the
                        // declaration with `;`, which used to serve as the
                        // init/cond separator here — supply it ourselves.
                        out.push_str("; ");
                    } else {
                        let mut tmp = String::new();
                        emit_stmt(i, &mut tmp, 0);
                        out.push_str(tmp.trim_end_matches('\n').trim_end());
                    }
                    out.push(' ');
                    });
                }
                else { out.push_str("; "); }
                if let Some(c) = cond { emit_expr(c, out); }
                out.push_str("; ");
                if let Some(i) = incr { emit_expr(i, out); }
                out.push_str(") ");
                emit_body_inline(body, out, indent);
            }
        }
        CgStmtData::Return(value) => {
            out.push_str(&ind);
            out.push_str("return");
            if let Some(v) = value {
                out.push(' ');
                emit_expr(v, out);
            }
            out.push_str(";\n");
        }
        CgStmtData::Break => { out.push_str(&ind); out.push_str("break;\n"); }
        CgStmtData::Continue => { out.push_str(&ind); out.push_str("continue;\n"); }
        CgStmtData::Goto(label) => { let _ = write!(out, "{}goto {};\n", ind, label); }
        // C labels are conventionally written flush at the left margin, not
        // indented like the surrounding block. Not a formatting slip.
        CgStmtData::Label(name) => { let _ = write!(out, "{}:\n", name); }
        CgStmtData::Switch { expr, body } => {
            out.push_str(&ind);
            out.push_str("switch (");
            emit_expr(expr, out);
            out.push_str(") ");
            emit_body_inline(body, out, indent);
        }
        CgStmtData::Case { value, body } => {
            out.push_str(&ind);
            out.push_str("case ");
            emit_expr(value, out);
            out.push_str(":\n");
            emit_stmt(body, out, indent + 1);
        }
        CgStmtData::Default(body) => {
            out.push_str(&ind);
            out.push_str("default:\n");
            emit_stmt(body, out, indent + 1);
        }
        CgStmtData::RawLine(text) => {
            // Verbatim pass-through (`_Pragma("...")` etc.): emitted where the
            // source line was, never hoisted — a pragma's meaning is its
            // position.
            out.push_str(&ind);
            out.push_str(text);
            out.push('\n');
        }
        CgStmtData::Decl { decl_type, name, init, array_suffix, is_static, is_weak, is_block, next, attributes } => {
            if *is_block {
                let byref_name = format!("__jeti_byref_{}", name);
                let _ = write!(out, "{}struct {} {{\n", ind, byref_name);
                let _ = write!(out, "{}    void *__isa;\n", ind);
                let _ = write!(out, "{}    struct {} *__forwarding;\n", ind, byref_name);
                let _ = write!(out, "{}    int __flags;\n", ind);
                // __value keeps the declared array shape (`size_t __value[4]`) —
                // dropping the suffix made the cell a scalar and every captured
                // subscript inside the block failed to compile
                // ("subscripted value is not an array").
                let _ = write!(out, "{}    {} __value{};\n", ind, decl_type, array_suffix.as_deref().unwrap_or(""));
                let _ = write!(out, "{}}};\n", ind);
                let _ = write!(out, "{}struct {} {} = {{\n", ind, byref_name, name);
                let _ = write!(out, "{}    .__forwarding = &{},\n", ind, name);
                let _ = write!(out, "{}    .__flags = 0,\n", ind);
                if let Some(i) = init {
                    let _ = write!(out, "{}    .__value = ", ind);
                    emit_expr(i, out);
                    out.push_str(",\n");
                }
                let _ = write!(out, "{}}};\n", ind);
                return;
            }
            for a in attributes {
                let _ = write!(out, "{}__attribute__(({})) ", ind, a);
            }
            // Detect alloc+init pattern: vtable dispatch call where the receiver is a complex expression
            // (like another function call). Emit a temp variable to avoid double evaluation.
            let should_split = init.as_ref().map_or(false, |i| {
                if let CgExprData::Call { args, .. } = &i.data {
                    args.first().map_or(false, |a| matches!(a.kind, CgExprKind::Call))
                } else { false }
            });
            if should_split {
                let decl_var_name = name; // Save the declaration variable name before shadowing
                let init_data = init.as_ref().and_then(|i| {
                    if let CgExprData::Call { ref name, ref args, vtable_class: Some(ref vc), is_class_method, is_super, ref sel_const_name, .. } = i.data {
                        Some((name.clone(), args.clone(), vc.clone(), is_class_method, is_super, sel_const_name.clone()))
                    } else { None }
                });
                if let Some((ref method_name, ref args, ref vc, _is_class_method, _is_super, sel_const_name)) = init_data {
                    out.push_str(&ind);
                    if !args.is_empty() {
                            let tid = next_temp_id();
                            // Emit: NPObject *__jeti_tmp_N = receiver;
                            // The `(NPObject *)` spelling mirrors the nil-safe
                            // send arm: jeti allows the implicit subclass→id
                            // join, plain C flags it.
                            let _ = write!(out, "NPObject *__jeti_tmp_{} = ((NPObject *)(", tid);
                            emit_expr(&args[0], out);
                            out.push_str("));\n");
                            out.push_str(&ind);
                            if *is_static { out.push_str("static "); }
                            out.push_str(decl_type);
                            out.push(' ');
                            out.push_str(decl_var_name);
                            if let Some(suffix) = array_suffix { out.push_str(suffix); }
                            // Uniform vtable member dispatch with (SubClass*) cast for concrete class pointers.
                            // Guard the temp (the alloc result) against nil for safe nil-messaging.
                            let needs_cast = decl_type.ends_with(" *") && decl_type != "NPObject *";
                            // A `NPObject *`/`id` decl takes any class-pointer
                            // dispatch result — the implicit upcast ObjC allows
                            // must be spelled in plain C.
                            let needs_upcast = !needs_cast
                                && (decl_type.trim() == "NPObject *" || decl_type.trim() == "id");
                            if needs_cast {
                                let _ = write!(out, " = ({})(", decl_type.trim_end());
                            } else if needs_upcast {
                                out.push_str(" = (NPObject *)(");
                            } else {
                                out.push_str(" = ");
                            }
                            let _ = write!(out, "__jeti_tmp_{} ? ((struct jeti_vtable *)__jeti_tmp_{}->isa->vtable)->{}(", tid, tid, method_name);
                            let _ = write!(out, "__jeti_tmp_{}", tid);
                        let _ = write!(out, ", {}", sel_const_name.as_deref().unwrap_or("0"));
                        for (ai, arg) in args[1..].iter().enumerate() {
                            out.push_str(", ");
                            if let Some(pt) = get_vtable_param_type_for_class(vc, method_name, ai + 2) {
                                if pt.ends_with('*') && !pt.trim_start().starts_with("const char") && !pt.trim_start().starts_with("char ") {
                                    let _ = write!(out, "({})(", pt);
                                    emit_expr(arg, out);
                                    out.push_str(")");
                                    continue;
                                }
                            }
                            emit_expr(arg, out);
                        }
                        out.push_str(") : 0");
                        if needs_cast || needs_upcast {
                            out.push_str(")");
                        }
                        out.push_str(";\n");
                    } else {
                        if *is_static { out.push_str("static "); }
                        out.push_str(decl_type);
                        out.push(' ');
                        out.push_str(name);
                        if let Some(suffix) = array_suffix { out.push_str(suffix); }
                        let _ = write!(out, " = ((struct jeti_vtable *)0)->{}(", method_name);
                        let _ = write!(out, "{}", sel_const_name.as_deref().unwrap_or("0"));
                        out.push_str(");\n");
                    }
                } else {
                    // Fallback (shouldn't reach here)
                    out.push_str(&ind);
                    out.push_str(decl_type);
                    out.push(' ');
                    out.push_str(name);
                    if let Some(suffix) = array_suffix { out.push_str(suffix); }
                    if let Some(i) = init { out.push_str(" = "); emit_expr(i, out); }
                    for (n_name, n_type, n_init) in next {
                        match n_type.as_str() {
                            t if !t.is_empty() => {
                                out.push_str(";\n    ");
                                out.push_str(&t);
                                out.push(' ');
                                out.push_str(&n_name);
                            }
                            _ => {
                                out.push_str(", ");
                                out.push_str(&n_name);
                            }
                        }
                        if let Some(i) = n_init {
                            out.push_str(" = ");
                            emit_expr(i, out);
                        }
                    }
                    out.push_str(";\n");
                }
            } else {
                out.push_str(&ind);
                if *is_static { out.push_str("static "); }
                // For block types like `int (^name)(int)`, the variable name is already inside the type
                // (clang spelling `(^name)…`, portable spelling `struct __jeti_block_header *name`).
                let is_block_type = decl_type.contains("(^") || decl_type.contains("(*")
                    || decl_type.contains("__jeti_block_header");
                if is_block_type {
                    out.push_str(decl_type);
                } else {
                    out.push_str(decl_type);
                    if *is_weak { out.push_str(" __attribute__((cleanup(jeti_weakAutoCleanup)))"); }
                    out.push(' ');
                    out.push_str(name);
                }
                if let Some(suffix) = array_suffix { out.push_str(suffix); }
                if let Some(i) = init {
                    // A call initializer is cast to the DECLARED type whenever
                    // that type is an object pointer. The checker has already
                    // proved the assignment legal, so the cast is always safe —
                    // and it is needed because the emitted statement-expression
                    // takes its C type from the dispatched fn-ptr's return type,
                    // which can differ from the declaration:
                    //   `id a = [e keyPath];`  →  ({ ... (NPString *(*)(..))(..) ... })
                    // `NPObject *` (the checker's fallback for an unresolved
                    // return type) vs `NPString *` here. Excluding
                    // `NPObject *` from this cast left exactly that case
                    // warning (`incompatible pointer types initializing`).
                    let needs_cast = decl_type.ends_with(" *") && !is_block_type
                        && matches!(i.data, CgExprData::Call { .. });
                    let needs_reverse_cast = decl_type == "NPObject *"
                        && i.type_str.as_ref().map_or(false, |t| t.ends_with(" *") && t != "NPObject *");
                    let needs_self_cast = decl_type.ends_with(" *") && decl_type != "NPObject *"
                        && i.type_str.as_deref() == Some("NPObject *");
                    if needs_cast {
                        let _ = write!(out, " = ({})(", decl_type.trim_end());
                        emit_expr(i, out);
                        out.push_str(")");
                    } else if needs_reverse_cast {
                        let _ = write!(out, " = (NPObject *)(");
                        emit_expr(i, out);
                        out.push_str(")");
                    } else if needs_self_cast {
                        let _ = write!(out, " = ({})(", decl_type.trim_end());
                        emit_expr(i, out);
                        out.push_str(")");
                    } else {
                        out.push_str(" = ");
                        emit_expr(i, out);
                    }
                }
                for (n_name, n_type, n_init) in next {
                    // Each declarator carries its own type (per-declarator `*`
                    // stars / array suffixes). Absent type = shares the head
                    // type, emit in the same declaration; otherwise close the
                    // head and start an independent declaration.
                    match n_type.as_str() {
                        t if !t.is_empty() => {
                            out.push_str(";\n    ");
                            out.push_str(t);
                            out.push(' ');
                            out.push_str(&n_name);
                        }
                        _ => {
                            out.push_str(", ");
                            out.push_str(&n_name);
                        }
                    }
                    if let Some(i) = n_init {
                        out.push_str(" = ");
                        emit_expr(i, out);
                    }
                }
                out.push_str(";\n");
                if *is_weak {
                    if let Some(ref init_expr) = init {
                        let _ = write!(out, "{}jeti_weakRegister((NPObject **)&{}, (NPObject *)", ind, name);
                        emit_expr(init_expr, out);
                        out.push_str(");\n");
                    }
                }
            }
        }
        CgStmtData::Asm { is_volatile, is_goto, template, outputs, inputs, clobbers, labels } => {
            emit_asm_syntax(*is_volatile, *is_goto, template, outputs, inputs, clobbers, labels, out, &ind);
        }
        CgStmtData::Empty => {}
        CgStmtData::ForIn { var_name, collection, body } => {
            out.push_str(&ind);
            let _ = write!(out, "{{ size_t _count = jeti_array_count(");
            emit_expr(collection, out);
            out.push_str(");\n");
            let _ = write!(out, "{}for (size_t _i = 0; _i < _count; _i++) {{\n", ind);
            let _ = write!(out, "{}    ", ind);
            out.push_str(var_name);
            out.push_str(" = ((");
            // type from collection element
            out.push_str("typeof(");
            emit_expr(collection, out);
            out.push_str("[0])");
            out.push_str(")");
            emit_expr(collection, out);
            out.push_str("[_i];\n");
            emit_stmt(body, out, indent + 2);
            let _ = write!(out, "{}}}\n", ind);
            let _ = write!(out, "{}}}\n", ind);
        }
    }
}

fn emit_attrs_prefix(attrs: &[String], out: &mut String) {
    for a in attrs {
        let _ = write!(out, "__attribute__(({})) ", a);
    }
}

pub fn emit_decl(d: &CgDecl, out: &mut String) {
    // Function bodies re-emit their statements via emit_stmt (which carries
    // the source mapping); everything else is generated structure. Reset to
    // the synthetic file first so a function body's first #line is always
    // emitted (never inherits a stale position from the previous decl).
    if matches!(d.data, CgDeclData::Function { body: Some(_), .. }) {
        reset_line_synthetic(out);
    }
    match &d.data {
        CgDeclData::RawLine(text) => {
            // Raw pass-through line (e.g. `#pragma mark - Foo`): emit it
            // verbatim at the position it appeared in the source.
            out.push_str(text);
            out.push('\n');
        }
        CgDeclData::ForwardClass { names } => {
            // Forward declarations are emitted once at the top of the file
            // (see emit_unit_with_headers); skip here to avoid duplicates.
            let _ = names;
        }
        CgDeclData::Function { return_type, params, body, is_variadic, .. } => {
            emit_attrs_prefix(&d.attributes, out);
            let has_internal_linkage = d.attributes.iter().any(|a| a.contains("internal_linkage"));
            if body.is_some() && !has_internal_linkage {
                // can link against a separately compiled implementation file:
                // duplicate definitions (e.g. base-class methods emitted in every
                // TU) are coalesced by the linker instead of failing.
                out.push_str("__attribute__((weak)) ");
            }
            out.push_str(return_type);
            out.push(' ');
            out.push_str(&d.name);
            out.push('(');
            for (i, (pt, pn)) in params.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                out.push_str(&format_param_decl(pt, pn));
            }
            if params.is_empty() { out.push_str("void"); }
            if *is_variadic { out.push_str(", ..."); }
            out.push(')');
            if let Some(b) = body {
                out.push(' ');
                emit_stmt(b, out, 0);
            } else {
                out.push_str(";\n");
            }
        }
        CgDeclData::Variable { var_type, init, is_static, is_extern, is_const, is_block, next, .. } => {
            if *is_block {
                let byref_name = format!("__jeti_byref_{}", d.name);
                let _ = write!(out, "struct {} {{\n", byref_name);
                out.push_str("    void *__isa;\n");
                let _ = write!(out, "    struct {} *__forwarding;\n", byref_name);
                out.push_str("    int __flags;\n");
                // Array capture: var_type carries the `[N]` suffix — keep it on
                // __value so captured subscripts inside the block compile.
                let (vbase, vsuffix) = match var_type.find('[') {
                    Some(p) => (&var_type[..p], &var_type[p..]),
                    None => (var_type.as_str(), ""),
                };
                let _ = write!(out, "    {} __value{};\n", vbase, vsuffix);
                let _ = write!(out, "}} {};\n", byref_name);
                let _ = write!(out, "struct {} {} = {{\n", byref_name, d.name);
                let _ = write!(out, "    .__forwarding = &{},\n", d.name);
                out.push_str("    .__flags = 0,\n");
                if let Some(i) = init {
                    out.push_str("    .__value = ");
                    emit_expr(i, out);
                    out.push_str(",\n");
                }
                out.push_str("};\n");
                return;
            }
            emit_attrs_prefix(&d.attributes, out);
            // `extern` is a declaration, not a definition: emitting it as a
            // plain `char **environ;` created a zero-initialized tentative
            // definition that shadowed libc's symbol.
            if *is_extern { out.push_str("extern "); }
            if *is_static { out.push_str("static "); }
            // `convert_decl` folds the decl-level `const` into the spelled type
            // (`const SEL`), so adding it again here produced `const const`.
            if *is_const && !var_type.starts_with("const ") { out.push_str("const "); }
            // For block types like `int (^name)(int)` (and function-pointer
            // types like `void (*cb)(...)`), the variable name is already
            // inside the type
            let is_block_type = var_type.contains("(^") || var_type.contains("(*");
            if is_block_type {
                // Block type already includes the variable name, just append initializer
                out.push_str(var_type);
                if let Some(i) = init {
                    out.push_str(" = ");
                    emit_expr(i, out);
                }
                out.push_str(";\n");
            } else {
                let (base, suffix) = split_array_type(var_type);
                out.push_str(base);
                out.push(' ');
                out.push_str(&d.name);
                out.push_str(suffix);
                if let Some(i) = init {
                    // When init has type NPObject * but var_type is a subclass pointer,
                    // add an explicit cast to avoid -Wincompatible-pointer-types.
                    let needs_cast = var_type.ends_with(" *") && var_type != "NPObject *"
                        && i.type_str.as_deref() == Some("NPObject *");
                    if needs_cast {
                        let _ = write!(out, " = ({})(", var_type);
                        emit_expr(i, out);
                        out.push_str(")");
                    } else {
                        out.push_str(" = ");
                        emit_expr(i, out);
                    }
                }
                for (n_name, n_type, n_init) in next {
                    // Each declarator carries its own type (per-declarator `*`
                    // stars / array suffixes). Absent type = shares the head
                    // type, emit in the same declaration; otherwise close the
                    // head and start an independent declaration.
                    match n_type.as_str() {
                        t if !t.is_empty() => {
                            out.push_str(";\n    ");
                            out.push_str(t);
                            out.push(' ');
                            out.push_str(&n_name);
                        }
                        _ => {
                            out.push_str(", ");
                            out.push_str(&n_name);
                        }
                    }
                    if let Some(i) = n_init {
                        out.push_str(" = ");
                        emit_expr(i, out);
                    }
                }
                out.push_str(";\n");
            }
        }
        CgDeclData::Typedef { alias, type_str, struct_fields } => {
            // Struct typedefs with struct_fields are already emitted at the
            // forward-declaration point (before function declarations). Skip
            // them here to avoid duplicates.
            if struct_fields.is_empty() {
                emit_attrs_prefix(&d.attributes, out);
                // Array typedefs: `typedef int Row4[4];` — the array suffix
                // belongs after the alias name, not the base type.
                let (base, suffix) = split_array_type(type_str);
                if !suffix.is_empty() {
                    let _ = write!(out, "typedef {} {}{};\n", base, alias, suffix);
                } else {
                    let _ = write!(out, "typedef {} {};\n", type_str, alias);
                }
            }
        }
        CgDeclData::Struct { fields, is_union } => {
            let _ = write!(out, "{} {} {{\n", agg_keyword(*is_union), d.name);
            for (ft, fn_, fattrs) in fields {
                emit_struct_field(ft, fn_, fattrs, out);
            }
            out.push_str("}");
            emit_attrs_prefix(&d.attributes, out);
            out.push_str(";\n");
        }
        CgDeclData::ExternFunc { return_type, params, .. } => {
            out.push_str(return_type);
            out.push(' ');
            out.push_str(&d.name);
            out.push('(');
            for (i, (pt, pn)) in params.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                out.push_str(pt);
                out.push(' ');
                out.push_str(pn);
            }
            if params.is_empty() { out.push_str("void"); }
            out.push_str(");\n");
        }
        CgDeclData::Asm { is_volatile, is_goto, template, outputs, inputs, clobbers, labels } => {
            emit_asm_syntax(*is_volatile, *is_goto, template, outputs, inputs, clobbers, labels, out, "");
        }
        CgDeclData::Enum { members } => {
            emit_attrs_prefix(&d.attributes, out);
            let _ = write!(out, "enum {} {{\n", d.name);
            for (i, (m, v)) in members.iter().enumerate() {
                if i > 0 { out.push_str(",\n"); }
                let _ = write!(out, "    {} = {}", m, v);
            }
            out.push_str("\n};\n");
            // typedef alias so that `GameState var;` (source uses typedef-enum
            // form `typedef enum {...} GameState;`) resolves. Without this,
            // C only sees `enum GameState` but not the bare `GameState` name,
            // causing `unknown type name 'GameState'`. An *anonymous* enum has
            // no tag to alias, and emitting the alias anyway produced the
            // invalid `typedef enum  ;`.
            if !d.name.is_empty() {
                let _ = write!(out, "typedef enum {} {};\n", d.name, d.name);
            }
        }
    }
}

/// Emit a one-line `/* ... */` section banner used to structure the generated
/// C code for readability. Only emitted when `comments` is enabled.
fn section_comment(out: &mut String, comments: bool, label: &str) {
    if !comments { return; }
    out.push_str("/* ");
    let total = 60usize;
    let label_len = label.len();
    let pad = if label_len >= total { 0 } else { total - label_len };
    let left = pad / 2;
    let right = pad - left;
    for _ in 0..left { out.push('-'); }
    out.push(' ');
    out.push_str(label);
    out.push(' ');
    for _ in 0..right { out.push('-'); }
    out.push_str(" */\n");
}

/// Strip any surviving type-parameter sentinels (`NPObject * /*NAME*/` — the
/// legacy `/*T*/` and the per-name `/*K*/`/`/*V*/`/user-param forms) from
/// generated C text. Called on the final output in `emit_unit_with_headers`:
/// sentinels that escaped substitution belong to the bare generic template or
/// uniform dispatch casts, where the param genuinely renders as plain
/// `NPObject *`. Text-level (not a CgUnit walk) because sentinels can hide in
/// nested method-body CgStmt trees no cheap traversal covers. Only a strict
/// identifier body is treated as a sentinel — arbitrary comments after
/// `NPObject *` pass through untouched.
fn normalize_t_sentinels_text(c: &str) -> String {
    const HEAD: &str = "NPObject * /*";
    let mut out = String::with_capacity(c.len());
    let mut rest = c;
    while let Some(pos) = rest.find(HEAD) {
        let after = &rest[pos + HEAD.len()..];
        let sentinel = after.find("*/").map(|end| {
            let name = &after[..end];
            !name.is_empty()
                && !name.contains('*')
                && !name.contains('\n')
                && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
        }).unwrap_or(false);
        if sentinel {
            let end = after.find("*/").unwrap();
            out.push_str(&rest[..pos]);
            out.push_str("NPObject *");
            rest = &after[end + 2..];
        } else {
            // Not a well-formed sentinel — keep the text and scan on.
            out.push_str(&rest[..pos + HEAD.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Installs/uninstalls the thread-local `#line` context for one unit
/// emission (RAII so nested or repeated emissions cannot leak state).
struct LineCtxGuard;
impl LineCtxGuard {
    fn install(sm: Option<&SourceMap>) -> Option<Self> {
        let sm = sm?;
        LINE_CTX.with(|c| {
            *c.borrow_mut() = Some(LineCtx {
                sm: sm.clone(),
                cur_file: String::new(),
                cur_line: 0,
            });
        });
        Some(LineCtxGuard)
    }
}
impl Drop for LineCtxGuard {
    fn drop(&mut self) {
        LINE_CTX.with(|c| *c.borrow_mut() = None);
    }
}

pub fn emit_unit_with_headers(unit: &CgUnit, c_headers: &[String], search_dirs: &[String], freestanding: bool, backend: Backend, comments: bool, eh_checked: bool) -> String {
    emit_unit_with_headers_mapped(unit, c_headers, search_dirs, freestanding, backend, comments, eh_checked, None)
}

/// Same as [`emit_unit_with_headers`], with optional `#line` emission
/// (plan 阶段 2): passing `Some(source_map)` makes every statement's mapped
/// source position a `#line` directive in the generated C.
pub fn emit_unit_with_headers_mapped(unit: &CgUnit, c_headers: &[String], search_dirs: &[String], freestanding: bool, backend: Backend, comments: bool, eh_checked: bool, source_map: Option<&SourceMap>) -> String {
    CURRENT_BACKEND.store(backend as u8, Ordering::Relaxed);
    let _line_guard = LineCtxGuard::install(source_map);
    // Metadata linkage: the default is `weak`, so the
    // many per-TU copies of a class's vtable / meta-vtable / getClass coalesce
    // to one. Ownership (rule R2 — `CgUnit::owned_classes`) decides which
    // copies below are emitted strong instead. See `docs/plans/stable_slots_plan.md`.
    let meta_weak: &str = "__attribute__((weak)) ";
    let mut out = String::new();
    if comments {
        let _ = writeln!(out, "/* ============================================================");
        let _ = writeln!(out, "   Generated by jetic — Jeti → C transpiler");
        let _ = writeln!(out, "   source : {}", unit.filename);
        let _ = writeln!(out, "   backend: {}", backend);
        let _ = writeln!(out, "   ============================================================ */");
        out.push('\n');
    } else {
        out.push_str("// Generated by jetic\n");
    }
    section_comment(&mut out, comments, "Section 1 · Requires & defines");
    if freestanding {
        // Bare-metal mode: no libc headers. The runtime header's
        // __JETI_FREESTANDING branch provides the types, jmp_buf (via
        // builtins) and non-TLS exception state.
        out.push_str("#define __JETI_FREESTANDING 1\n");
        out.push_str("#include <jeti/runtime.h>\n");
    } else {
        let has_string_h = c_headers.iter().any(|h| h.contains("string.h"));
        if !has_string_h {
            out.push_str("#include <string.h>\n");
        }
        // abort() lives in the vtable __sig verification constructor every TU
        // emits (hosted path). stdlib.h was previously only reached indirectly
        // through user includes — minimal Foundation-only files compile-failed
        // on the implicit declaration.
        let has_stdlib_h = c_headers.iter().any(|h| h.contains("stdlib.h"));
        if !has_stdlib_h {
            out.push_str("#include <stdlib.h>\n");
        }
        // -eh checked emits reads/writes of the EH globals (__jeti_eh_flag/
        // __jeti_eh_val) and calls __jeti_eh_isa in EVERY function with a
        // throwing callee. Their declarations live only in runtime.h; a pure
        // C-superset file (no Foundation import, no block literal) otherwise
        // generates C with undeclared identifiers (second-referendum root
        // cause). runtime.h is a plain C header — harmless to include.
        if eh_checked && !c_headers.iter().any(|h| h.contains("jeti/runtime.h")) {
            out.push_str("#include <jeti/runtime.h>\n");
        }
    }
    for h in c_headers {
        out.push_str(h);
        out.push('\n');
    }

    // Scan C headers for structs already defined externally
    let mut header_structs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for directive in c_headers {
        if let Some(rest) = directive.strip_prefix("#include ") {
            let rest = rest.trim();
            let path = if rest.starts_with('"') && rest.ends_with('"') {
                &rest[1..rest.len()-1]
            } else if rest.starts_with('<') && rest.ends_with('>') {
                &rest[1..rest.len()-1]
            } else {
                continue;
            };
            for dir in search_dirs {
                let full = format!("{}/{}", dir, path);
                if let Ok(content) = std::fs::read_to_string(&full) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("struct ") && trimmed.contains('{') {
                            if let Some(name_rest) = trimmed.strip_prefix("struct ") {
                                let name = name_rest.split('{').next().unwrap_or(name_rest).trim();
                                if !name.is_empty() {
                                    header_structs.insert(name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let any_has_instance = unit.classes.iter().any(|cm| cm.method_names.iter().zip(&cm.is_class_methods).any(|(_, &ic)| !ic));

    // Forward-declare vtable structs (per-class typed vtables, plus meta vtable)
    section_comment(&mut out, comments, "Section 2 · Forward declarations");
    {
        if any_has_instance {
            let _ = write!(out, "struct jeti_vtable;\n");
        }
        for cm in &unit.classes {
            let flat_cn = name_flat(&cm.class_name);
            let has_class_methods = cm.is_class_methods.iter().any(|&c| c);
            let has_super = cm.super_name.is_some();
            if has_class_methods || has_super {
                let _ = write!(out, "struct {};\n", meta_symbol("META_VTABLE_", &flat_cn));
            }
        }
    }
    if !unit.classes.is_empty() { out.push('\n'); }

    // SEL constants
    section_comment(&mut out, comments, "Section 3 · SEL constants");
    for sel in &unit.selectors {
        let h = fnv1a_hash(sel);
        let sn = sel_const_name(sel);
        let _ = write!(out, "static const SEL {} = {{.name = \"{}\", .hash = 0x{:08X}}};\n", sn, sel, h);
    }
    if !unit.selectors.is_empty() { out.push('\n'); }

    // Forward declarations + typedefs for class types
    section_comment(&mut out, comments, "Section 4 · Type declarations & typedefs");
    for cm in &unit.classes {
        let fc = name_flat(&cm.class_name);
        if fc == "jeti_root" || fc == "NPObject" {
            // Emit full struct definitions with include guard so that if
            // runtime.h (which already defines them) is included first,
            // these are silently skipped.  If runtime.h is NOT available,
            // these definitions ensure the generated code compiles.
            // Guards must match those used in include/jeti/runtime.h.
            let guard = if fc == "jeti_root" {
                "JETI_ROOT_DEFINED"
            } else {
                "NPOBJECT_DEFINED"
            };
            let _ = writeln!(out, "#ifndef {}", guard);
            let _ = writeln!(out, "#define {}", guard);
            let _ = writeln!(out, "struct {} {{", fc);
            if fc == "jeti_root" {
                let _ = writeln!(out, "    struct NPClass *isa;");
            } else {
                let _ = writeln!(out, "    struct NPClass *isa;");
            }
            let _ = writeln!(out, "    uint32_t retain_count;");
            let _ = writeln!(out, "}};");
            let _ = writeln!(out, "typedef struct {} {};", fc, fc);
            let _ = writeln!(out, "#endif");
            // This file now carries a full (guarded) definition of the root
            // struct: record it so the class-layout section below does not
            // re-emit the same body unguarded — a same-file redefinition is a
            // hard C error (stable-slots §9 blocker 1). When runtime.h *is*
            // included, its own definitions win and skipping our copy is
            // equally right, so recording unconditionally is safe either way.
            header_structs.insert(fc.to_string());
        } else {
            let _ = write!(out, "struct {};\n", fc);
            let _ = write!(out, "typedef struct {} {};\n", fc, fc);
        }
    }
    if !unit.classes.is_empty() { out.push('\n'); }

    // Forward declarations for @class forward-declared types. Only emit for
    // names that don't have a full class definition in this unit (those already
    // got `struct X; typedef struct X X;` above) and aren't runtime roots.
    for decl in &unit.decls {
        if let CgDeclData::ForwardClass { names } = &decl.data {
            let mut any = false;
            for n in names {
                let fc = name_flat(n);
                if fc == "jeti_root" || fc == "NPObject" { continue; }
                if unit.classes.iter().any(|cm| name_flat(&cm.class_name) == fc) { continue; }
                let _ = write!(out, "struct {};\n", fc);
                let _ = write!(out, "typedef struct {} {};\n", fc, fc);
                any = true;
            }
            if any { out.push('\n'); }
        }
    }

    // Forward declarations for enums (must come BEFORE struct/class metadata
    // so that `GameState var;` inside an ivar list resolves. Without this,
    // the enum decl is emitted near the end of the file (after structs that
    // reference it), causing `unknown type name 'GameState'` errors.
    for decl in &unit.decls {
        if let CgDeclData::Enum { members } = &decl.data {
            emit_attrs_prefix(&decl.attributes, &mut out);
            let _ = write!(out, "enum {} {{\n", decl.name);
            for (i, (m, v)) in members.iter().enumerate() {
                if i > 0 { out.push_str(",\n"); }
                let _ = write!(out, "    {} = {}", m, v);
            }
            out.push_str("\n};\n");
            if !decl.name.is_empty() {
                let _ = write!(out, "typedef enum {} {};\n", decl.name, decl.name);
            }
        }
    }
    if unit.decls.iter().any(|d| matches!(d.data, CgDeclData::Enum { .. })) { out.push('\n'); }

    // Forward declarations for typedefs (must come before function declarations).
    // For non-struct typedefs (`typedef int MyInt`) emit the bare typedef.
    // For struct typedefs (`typedef struct { ... } Name`) emit the full struct
    // definition here rather than a forward declaration, since anonymous structs
    // (`typedef struct { ... } Name`) have no struct tag to forward-declare.
    // Block typedefs with duplicate signatures are deduplicated: only the first
    // occurrence emits a full typedef; subsequent same-signature blocks are
    // either skipped (if alias already exists) or emit only an alias line.
    let mut seen_block_sigs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for decl in &unit.decls {
            if let CgDeclData::Typedef { ref alias, ref type_str, ref struct_fields } = decl.data {
                if struct_fields.is_empty() {
                // Block type dedup: skip full typedef if same signature seen
                if type_str.contains("(^") {
                    let sig = block_type_signature_key(type_str);
                    if !seen_block_sigs.insert(sig) {
                        continue;
                    }
                }
                if *alias == *type_str {
                    continue;
                }
                // Attributes go after `typedef` (C: `typedef __attribute__(...) type alias;`)
                out.push_str("typedef ");
                emit_attrs_prefix(&decl.attributes, &mut out);
                // Array typedefs: `typedef int Row4[4];` — the array suffix
                // belongs after the alias name, not the base type.
                let (base, suffix) = split_array_type(type_str);
                if !suffix.is_empty() {
                    let _ = write!(out, "{} {}{};\n", base, alias, suffix);
                } else {
                    let _ = write!(out, "{} {};\n", type_str, alias);
                }
            }
        }
    }
    if unit.decls.iter().any(|d| matches!(d.data, CgDeclData::Typedef { .. })) { out.push('\n'); }

    // Struct definitions (dependency-sorted so a struct referenced by value
    // by another struct is defined first; must precede function prototypes
    // that reference them).
    section_comment(&mut out, comments, "Section 5 · Struct definitions");
    let aggregates_emitted = emit_aggregate_definitions(unit, &mut out);
    if unit.decls.iter().any(|d| matches!(d.data, CgDeclData::Struct { .. })) { out.push('\n'); }

    // Struct value-comparison functions (`a == b` on two value structs of the
    // same tag). Must follow the struct definitions (needs complete field
    // types) and precede the prototypes section for tidiness.
    emit_struct_eq_functions(unit, &mut out);

    // Forward declarations for functions
    section_comment(&mut out, comments, "Section 6 · Function prototypes");
    for decl in &unit.decls {
        if let CgDeclData::Function { ref return_type, ref params, is_variadic, .. } = decl.data {
            emit_attrs_prefix(&decl.attributes, &mut out);
            let _ = write!(out, "{} {}(", return_type, decl.name);
            for (i, (pt, pn)) in params.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                out.push_str(&format_param_decl(pt, pn));
            }
            if params.is_empty() { out.push_str("void"); }
            if is_variadic { out.push_str(", ..."); }
            out.push_str(");\n");
        }
    }
    if !unit.decls.is_empty() { out.push('\n'); }

    // File-level variable declarations (must precede function definitions)
    section_comment(&mut out, comments, "Section 7 · File-level variables");
    for decl in &unit.decls {
        if let CgDeclData::Variable { ref var_type, ref init, is_static, is_extern, is_const, is_block, .. } = decl.data {
            if is_block {
                let byref_name = format!("__jeti_byref_{}", decl.name);
                let _ = write!(out, "struct {} {{\n", byref_name);
                out.push_str("    void *__isa;\n");
                let _ = write!(out, "    struct {} *__forwarding;\n", byref_name);
                out.push_str("    int __flags;\n");
                // Array capture: var_type carries the `[N]` suffix — keep it on
                // __value so captured subscripts inside the block compile.
                let (vbase, vsuffix) = match var_type.find('[') {
                    Some(p) => (&var_type[..p], &var_type[p..]),
                    None => (var_type.as_str(), ""),
                };
                let _ = write!(out, "    {} __value{};\n", vbase, vsuffix);
                let _ = write!(out, "}} {};\n", byref_name);
                let _ = write!(out, "struct {} {} = {{\n", byref_name, decl.name);
                let _ = write!(out, "    .__forwarding = &{},\n", decl.name);
                out.push_str("    .__flags = 0,\n");
                if let Some(i) = init {
                    out.push_str("    .__value = ");
                    emit_expr(i, &mut out);
                    out.push_str(",\n");
                }
                out.push_str("};\n");
                continue;
            }
            emit_attrs_prefix(&decl.attributes, &mut out);
            if is_extern { out.push_str("extern "); }
            if is_static { out.push_str("static "); }
            // See the statement-level emitter: the decl-level `const` is already
            // folded into `var_type`.
            if is_const && !var_type.starts_with("const ") { out.push_str("const "); }
            let is_block_type = var_type.contains("(^") || var_type.contains("(*");
            if is_block_type {
                out.push_str(var_type);
                if let Some(i) = init {
                    out.push_str(" = ");
                    emit_expr(i, &mut out);
                }
                out.push_str(";\n");
            } else {
                let (base, suffix) = split_array_type(var_type);
                out.push_str(base);
                out.push(' ');
                out.push_str(&decl.name);
                out.push_str(suffix);
                if let Some(i) = init {
                    out.push_str(" = ");
                    emit_expr(i, &mut out);
                }
                out.push_str(";\n");
            }
        }
    }
    if unit.decls.iter().any(|d| matches!(d.data, CgDeclData::Variable { .. })) { out.push('\n'); }

    // Forward declarations for getClass functions
    section_comment(&mut out, comments, "Section 8 · VTable & class layouts");
    for cm in &unit.classes {
        let has_class_methods = cm.is_class_methods.iter().any(|&c| c);
        let has_super = cm.super_name.is_some();
        if has_class_methods || has_super {
            let fc = name_flat(&cm.class_name);
            let _ = write!(out, "NPClass * {}(NPClass * self, SEL _cmd);\n", meta_symbol("GETCLASS_", &fc));
        }
    }
    if unit.classes.iter().any(|cm| cm.is_class_methods.iter().any(|&c| c) || cm.super_name.is_some()) {
        out.push('\n');
    }

    // VTable struct definitions (per-class, type-safe).
    // Uniform vtable: a single struct with all instance methods as typed function pointers.
    // All vtable instances use this struct type, making dispatch via member access
    // type-safe regardless of which class the receiver belongs to.
    if any_has_instance {
        // Cross-TU layout signature (R3): the fingerprint covers the SHARED
        // segment (`vtable_sig_names` — the public methods every linked TU
        // imports, or the --slots manifest). R1 keeps that segment at identical
        // slot indices everywhere and R2 keeps TU-local private dispatch inside
        // the owning TU, so private tails must NOT make the signatures differ —
        // that is the legal library+client split. Two TUs whose shared segments
        // disagree would dispatch through different layouts while the linker
        // weak-merges the vtable instances into one allocation — the signature
        // travels INSIDE the vtable struct (first member) so it merges with the
        // instance that actually won, and the per-TU constructor aborts with a
        // clear message instead of dispatching through a foreign layout.
        let sig: u64 = vtable_layout_sig(&unit.vtable_sig_names);
        let _ = write!(out, "/* vtable layout signature: {:016x} (shared methods: {}) */\n", sig, unit.vtable_sig_names.len());
        // The diagnostic needs <stdio.h>, which a translation unit that only
        // includes <jeti/runtime.h> may not pull in — and freestanding builds
        // have no stdio at all. When the standard headers are absent we still
        // need the failure to be loud, so fall back to __builtin_trap() (a
        // compiler builtin, available in hosted and freestanding alike) rather
        // than declaring stdio symbols this TU may not link against.
        let has_stdio = c_headers.iter().any(|h| h.contains("stdio.h"));
        let _ = write!(out, "__attribute__((weak)) void jeti_verify_vtable_sig(unsigned long long winner, unsigned long long mine, const char *method_list) {{\n");
        let _ = write!(out, "    if (winner != mine) {{\n");
        if has_stdio {
            let _ = write!(out, "        fprintf(stderr,\n");
            let _ = write!(out, "            \"jeti: fatal: vtable layout mismatch across translation units.\\n\"\n");
            let _ = write!(out, "            \"  linked vtable sig %016llx, this translation unit sig %016llx\\n\"\n");
            let _ = write!(out, "            \"\\n\"\n");
            let _ = write!(out, "            \"Jeti builds one uniform 'struct jeti_vtable' per translation unit over the\\n\"\n");
            let _ = write!(out, "            \"public method segment: the selectors declared in the @interfaces the TUs\\n\"\n");
            let _ = write!(out, "            \"import (or the shared --slots manifest). R1 keeps that segment at\\n\"\n");
            let _ = write!(out, "            \"identical slot indices in every TU, so two TUs whose public segments\\n\"\n");
            let _ = write!(out, "            \"disagree dispatch through different layouts, while the linker\\n\"\n");
            let _ = write!(out, "            \"weak-merges the vtable instances into one allocation.\\n\"\n");
            let _ = write!(out, "            \"\\n\"\n");
            let _ = write!(out, "            \"Re-running jetic does NOT help: the shared method sets really do differ.\\n\"\n");
            let _ = write!(out, "            \"The usual cause is the two TUs importing different or differently\\n\"\n");
            let _ = write!(out, "            \"versioned headers. Make the shared declarations identical, or build the\\n\"\n");
            let _ = write!(out, "            \"affected classes as a single TU. TU-local private methods do not\\n\"\n");
            let _ = write!(out, "            \"participate in this check.\\n\"\n");
            let _ = write!(out, "            \"See tests/multi_tu/ for worked examples of what does and does not link.\\n\"\n");
            let _ = write!(out, "            \"\\nMethods known to this TU:\\n  %s\\n\",\n");
            let _ = write!(out, "            (unsigned long long)winner, (unsigned long long)mine, method_list);\n");
            let _ = write!(out, "        abort();\n");
        } else {
            let _ = write!(out, "        (void)winner; (void)mine; (void)method_list;\n");
            let _ = write!(out, "        __builtin_trap();\n");
        }
        let _ = write!(out, "    }}\n}}\n\n");
        // R4: ivar-section counterpart of the check above. `winner_text` is the
        // layout that won the linker's weak merge (i.e. what the other TU
        // compiled the struct from), `mine_text` is this TU's own — printing
        // both turns "my reads return garbage" into a diff the user can act on.
        let _ = write!(out, "__attribute__((weak)) void jeti_verify_layout_sig(unsigned long long winner, unsigned long long mine, const char *winner_text, const char *mine_text) {{\n");
        let _ = write!(out, "    if (winner != mine) {{\n");
        if has_stdio {
            let _ = write!(out, "        fprintf(stderr,\n");
            let _ = write!(out, "            \"jeti: fatal: class ivar layout mismatch across translation units.\\n\"\n");
            let _ = write!(out, "            \"  linked layout sig %016llx, this translation unit sig %016llx\\n\"\n");
            let _ = write!(out, "            \"\\n\"\n");
            let _ = write!(out, "            \"Every class struct inlines its ancestors' ivars in declaration order, so\\n\"\n");
            let _ = write!(out, "            \"two TUs that saw different ivar sections write and read different\\n\"\n");
            let _ = write!(out, "            \"offsets for the same field — silently, as garbage values. The usual\\n\"\n");
            let _ = write!(out, "            \"cause is a stale `.hje`: one TU was compiled against an older copy of\\n\"\n");
            let _ = write!(out, "            \"the header that declares this class (an ivar was added, removed or\\n\"\n");
            let _ = write!(out, "            \"retyped after that TU was linked in).\\n\"\n");
            let _ = write!(out, "            \"\\n\"\n");
            let _ = write!(out, "            \"  linked side : %s\\n\"\n");
            let _ = write!(out, "            \"  this TU     : %s\\n\",\n");
            let _ = write!(out, "            (unsigned long long)winner, (unsigned long long)mine,\n");
            let _ = write!(out, "            winner_text ? winner_text : \"?\", mine_text ? mine_text : \"?\");\n");
            let _ = write!(out, "        abort();\n");
        } else {
            let _ = write!(out, "        (void)winner; (void)mine; (void)winner_text; (void)mine_text;\n");
            let _ = write!(out, "        __builtin_trap();\n");
        }
        let _ = write!(out, "    }}\n}}\n\n");
        let _ = write!(out, "struct jeti_vtable {{\n");
        let _ = write!(out, "    unsigned long long __sig;\n");
        // R4: the ivar-layout twin of __sig. It travels INSIDE the instance for
        // the same reason — the linker weak-merges the per-class instances, so
        // each TU reads back the layout that actually won and can compare it
        // with the one it compiled against.
        let _ = write!(out, "    unsigned long long __layout_sig;\n");
        let _ = write!(out, "    const char *__layout_text;\n");
        for mname in &unit.global_instance_method_names {
            let (_, ptr_type) = METHOD_METADATA.get().unwrap().get(mname.as_str()).unwrap();
            // ptr_type is "return_type (*)(params)". Insert mname after the "*".
            if let Some(paren) = ptr_type.rfind("(*)") {
                let before = &ptr_type[..paren + 2]; // "return_type (*"
                let after = &ptr_type[paren + 2..];  // ")(params)"
                let _ = write!(out, "    {}{}{};\n", before, mname, after);
            } else {
                let _ = write!(out, "    {} {};\n", ptr_type, mname);
            }
        }
        out.push_str("};\n\n");
    }

    // respondsToSelector: helper bodies. The member set was registered during
    // convert_expr (RESP_HELPERS); the uniform vtable struct is now complete,
    // so each helper can reference its member. Static + defined only when the
    // corresponding send actually appears in this TU (no unused warnings).
    if let Some(members) = resp_helpers().clone() {
        for member in members {
            let _ = write!(out,
                "/* respondsToSelector: helper for selector member '{member}' */\n\
                 static BOOL jeti_resp_{member}(NPObject *__o) {{\n\
                     return __o && ((struct jeti_vtable *)__o->isa->vtable)->{member} != 0;\n\
                 }}\n\n",
                member = member);
        }
    }

    // Meta vtable struct definitions (per-class, for class methods)
    for cm in &unit.classes {
        if !cm.method_names.is_empty() {
            let has_class_methods = cm.is_class_methods.iter().any(|&c| c);
            let has_super = cm.super_name.is_some();
            if has_class_methods || has_super {
                let _ = write!(out, "struct {} {{\n", meta_symbol("META_VTABLE_", &name_flat(&cm.class_name)));
                for (i, (mname, &is_class)) in cm.method_names.iter().zip(&cm.is_class_methods).enumerate() {
                    if is_class {
                        let rt = cm.method_return_types.get(i).map(|s| s.as_str()).unwrap_or("NPObject *");
                        let params = cm.method_params_list.get(i).map(|p| p.iter().map(|(pt, _)| pt.clone()).collect::<Vec<_>>().join(", ")).unwrap_or_else(|| "NPClass *, SEL".to_string());
                        // Variadic class method → `...` in the fn-ptr member type;
                        // must match the function signature exactly (same rule as
                        // the METHOD_METADATA / CLASS_METHOD_METADATA casts) or the
                        // designated initializer in the meta-vtable instance errors.
                        let ellipsis = if cm.method_variadic.get(i).copied().unwrap_or(false) { ", ..." } else { "" };
                        let _ = write!(out, "    {} (*{})({}{});\n", rt, mname, params, ellipsis);
                    }
                }
                let _ = write!(out, "    NPClass * (*class)(NPClass *, SEL);\n");
                out.push_str("};\n");
            }
        }
    }
    if !unit.classes.is_empty() { out.push('\n'); }

    // Class struct definitions (skip any whose full body is already defined —
    // in a passthrough C header, or by Section 4's guarded root fallbacks)
    // Pre-compute a map from flat class name → CgClassMeta so we can walk the
    // superclass chain when emitting a subclass's struct fields. A subclass
    // struct must physically contain the parent's ivars (C has no inheritance),
    // so we emit the superclass ivars first, then this class's own ivars.
    let class_meta_by_name: std::collections::HashMap<String, &CgClassMeta> =
        unit.classes.iter().map(|cm| (name_flat(&cm.class_name), cm)).collect();
    for cm in &unit.classes {
        let skip = header_structs.contains(&cm.class_name);
        if skip { continue; }
        if comments {
            if let Some(ref sup) = cm.super_name {
                let _ = writeln!(out, "/* Class layout: {} (super: {}) */", cm.class_name, sup);
            } else {
                let _ = writeln!(out, "/* Class layout: {} */", cm.class_name);
            }
        }
        let _ = write!(out, "struct {} {{\n", name_flat(&cm.class_name));
        let _ = write!(out, "    struct NPClass *isa;\n");
        let _ = write!(out, "    uint32_t retain_count;\n");
        // Walk superclass chain and emit ancestor ivars (flat, not embedded).
        // Each non-root struct starts with isa+retain_count (matching jeti_root)
        // followed by all ancestor ivars, then this class's own ivars.
        let mut chain: Vec<&CgClassMeta> = Vec::new();
        let mut cur = cm;
        loop {
            if let Some(ref sup) = cur.super_name {
                let sup_flat = name_flat(sup);
                if let Some(sup_cm) = class_meta_by_name.get(&sup_flat) {
                    chain.push(sup_cm);
                    cur = sup_cm;
                    continue;
                }
            }
            break;
        }
        for ancestor in chain.iter().rev() {
            for (ivt, ivn) in ancestor.ivar_types.iter().zip(ancestor.ivar_names.iter()) {
                if let Some(pos) = ivt.find('[') {
                    let decl_t = &ivt[..pos].trim();
                    let suffix = &ivt[pos..];
                    let _ = write!(out, "    {} {}{};\n", decl_t, ivn, suffix);
                } else {
                    let _ = write!(out, "    {} {};\n", ivt, ivn);
                }
            }
        }
        for (ivt, ivn) in cm.ivar_types.iter().zip(cm.ivar_names.iter()) {
            if let Some(pos) = ivt.find('[') {
                let decl_t = &ivt[..pos].trim();
                let suffix = &ivt[pos..];
                let _ = write!(out, "    {} {}{};\n", decl_t, ivn, suffix);
            } else {
                let _ = write!(out, "    {} {};\n", ivt, ivn);
            }
        }
        out.push_str("};\n");
        let _ = write!(out, "typedef struct {} {};\n", name_flat(&cm.class_name), name_flat(&cm.class_name));
        out.push('\n');
    }

    // Forward-declare class metadata variables
    section_comment(&mut out, comments, "Section 9 · Class metadata infrastructure");
    for cm in &unit.classes {
        let _ = write!(out, "extern NPClass {};\n", meta_symbol("CLASS_", &name_flat(&cm.class_name)));
    }
    if !unit.classes.is_empty() {
        out.push_str("void jeti_metaInit(void);\n\n");
    }

    // Instance vtable instances (per-class typed, with designated initializers)
    section_comment(&mut out, comments, "Section 10 · Vtable & metadata instances");
    // Same signature the struct layout above was built for (kept in sync by
    // construction — both derive from `vtable_sig_names`, the shared segment).
    let vtable_sig: u64 = vtable_layout_sig(&unit.vtable_sig_names);
    for cm in &unit.classes {
        let flat_cn = name_flat(&cm.class_name);
        if comments {
            let _ = writeln!(out, "/* VTable instance: {} */", cm.class_name);
        }
        // R2: the TU that owns the @implementation emits strong metadata.
        let cw = if class_is_owned(unit, &cm.class_name) { "" } else { "__attribute__((weak)) " };
        let _ = write!(out, "{}struct jeti_vtable {} = {{\n", cw, meta_symbol("VTABLE_", &flat_cn));
        // Stamp the layout signature this instance was built for, so whichever
        // copy of this instance wins the linker's weak merge also carries the
        // layout it was actually initialized against.
        let _ = write!(out, "    .__sig = 0x{:016x}ULL,\n", vtable_sig);
        let layout_text = class_layout_text(cm);
        let _ = write!(out, "    .__layout_sig = 0x{:016x}ULL,\n", text_layout_sig(&layout_text));
        let _ = write!(out, "    .__layout_text = {},\n", c_string_literal(&layout_text));
        for mname in &unit.global_instance_method_names {
            if let Some(pos) = cm.method_names.iter().position(|n| n == mname) {
                if !cm.is_class_methods[pos] {
                    let owner = cm.method_owners.get(pos).cloned().unwrap_or_else(|| flat_cn.clone());
                    // Fill the slot by the method's OWNER:
                    //  * emitted in this TU → reference the local definition;
                    //  * inherited (owner is a parent class) → reference
                    //    `Owner_method` even when this TU only declares it.
                    //    The definition lives in the TU that owns the parent
                    //    (rule R2) or in the precompiled library, and a
                    //    prototype already exists — the parent's method list
                    //    is only known through declarations, so every entry
                    //    has one. A NULL here made `[[App alloc] init]`
                    //    dispatch through slot 0 in a declaration-only client
                    //    TU (P3 + own class): the owner's real table is in
                    //    another TU and does not help THIS unit's dispatch.
                    //  * own protocol stub (owner == this class, no body in
                    //    this TU) → keep NULL; the slot is preserved for
                    //    layout stability and the owner TU's instance (strong
                    //    under R2) wins the merge with the real reference.
                    //  * category-introduced method (declared in a shared .hje
                    //    category @interface, implemented in another TU) →
                    //    reference `Owner_method` as a strong extern (D1
                    //    backup plan): the category TU emits the definition
                    //    (weak, so it merges); if that TU is not linked the
                    //    reference fails loudly at link time on every
                    //    platform — probes/xcat P1 showed macOS ld rejects
                    //    undefined weak symbols anyway, so strong extern is
                    //    the stable semantics (docs/plans/categories_protocol_plan.md).
                    let is_cat = cm.category_method_names.contains(mname);
                    if method_is_emitted(&owner, mname) || owner != flat_cn || is_cat {
                        let (_, ptr_type) = METHOD_METADATA.get().unwrap().get(mname.as_str()).unwrap();
                        let _ = write!(out, "    .{} = ({}){}_{},\n", mname, ptr_type, owner, mname);
                    } else {
                        let _ = write!(out, "    .{} = NULL,\n", mname);
                    }
                } else {
                    let _ = write!(out, "    .{} = NULL,\n", mname);
                }
            } else {
                let _ = write!(out, "    .{} = NULL,\n", mname);
            }
        }
        out.push_str("};\n\n");
    }

    // Meta vtable instances
    for cm in &unit.classes {
        let class_entries: Vec<(&String, &String)> = cm.method_names.iter().zip(&cm.is_class_methods).enumerate().filter(|(_, (_, &ic))| ic).map(|(idx, (n, _))| (n, &cm.method_owners[idx])).collect();
        let has_class_methods = !class_entries.is_empty();
        let has_super = cm.super_name.is_some();
        if !has_class_methods && !has_super { continue; }
        if comments {
            let _ = writeln!(out, "/* Meta vtable instance: {} */", cm.class_name);
        }
        let cw = if class_is_owned(unit, &cm.class_name) { "" } else { "__attribute__((weak)) " };
        let flat = name_flat(&cm.class_name);
        let _ = write!(out, "{}struct {} {}_inst = {{\n", cw, meta_symbol("META_VTABLE_", &flat), meta_symbol("META_VTABLE_", &flat));
        for (mname, owner) in &class_entries {
            // Same rule as the instance vtable above: an inherited class
            // method (owner is a parent class) is defined in the owner's TU
            // (R2) or the precompiled library — keep the reference. An OWN
            // class method that is merely declared here (decl-header
            // visibility, no body in this TU) must not reference a function
            // this TU never defines: keep the slot NULL for layout stability
            // and let the owner TU's strong instance win the merge with the
            // real reference. Without this, importing a decl header alone
            // pulls undefined class-method symbols into the link.
            if method_is_emitted(owner, mname) || owner.as_str() != flat {
                let _ = write!(out, "    .{} = {}_{},\n", mname, owner, mname);
            } else {
                let _ = write!(out, "    .{} = NULL,\n", mname);
            }
        }
        let _ = write!(out, "    .class = {},\n", meta_symbol("GETCLASS_", &flat));
        out.push_str("};\n\n");
    }

    // getClass implementations for each class
    for cm in &unit.classes {
        let has_class_methods = cm.is_class_methods.iter().any(|&c| c);
        let has_super = cm.super_name.is_some();
        if !has_class_methods && !has_super { continue; }
        if comments {
            let _ = writeln!(out, "/* +getClass for {} */", cm.class_name);
        }
        let cw = if class_is_owned(unit, &cm.class_name) { "" } else { "__attribute__((weak)) " };
        let _ = write!(out, "{}NPClass * {}(NPClass * self, SEL _cmd) {{\n", cw, meta_symbol("GETCLASS_", &name_flat(&cm.class_name)));
        out.push_str("    (void)_cmd;\n");
        out.push_str("    return self;\n");
        out.push_str("}\n\n");
    }

    // ARC dealloc wrappers — computed and emitted BEFORE the class-metadata
    // section: both `jeti_metaInit` and the owned-class static definitions
    // below reference the wrappers by name, and the wrappers are `static`, so
    // the definition must precede every reference (no forward declaration).
    // Skipped entirely under `-fno-jeti-arc`: MRC means the programmer owns
    // the ivars.
    let (arc_dealloc_names, arc_dealloc_defs) = if unit.no_arc {
        (std::collections::HashMap::new(), String::new())
    } else {
        emit_arc_dealloc_wrappers(&unit.classes)
    };
    out.push_str(&arc_dealloc_defs);

    // KVC accessor tables (NPPredicate support) — docs/plans/nppredicate_plan.md §2.
    // Emitted only when the pipeline saw a transitive NPPredicate import
    // (`unit.kvc`): a TU that never touches predicates pays nothing. Each
    // table maps method-name keys to generated wrapper getters whose body is
    // ONE ordinary static vtable dispatch (no reflection): object returns
    // flow through, scalars are boxed via the NPNumber factories the same way
    // `@(expr)` does. Emission is gated per-method by the SAME discipline as
    // the vtable slots: a method with a body in this TU, or an inherited one
    // (owner != this class, resolved at link time), gets a wrapper; a
    // declaration-only method stays out of the table.
    let mut kvc_syms: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if unit.kvc {
        let has_npnumber = unit.classes.iter().any(|c| c.class_name == "NPNumber");
        let mut kvc_out = String::new();
        for cm in &unit.classes {
            let flat = name_flat(&cm.class_name);
            // A class with no bodies here contributes nothing: its vtable slots
            // are NULL stubs (R2 decl-only client) and the method symbols this
            // TU could reference do not exist. Owner classes and inline-
            // implementation TUs (self-contained mode) both qualify.
            let mut any_body = class_is_owned(unit, &cm.class_name);
            if !any_body {
                for (j, mname) in cm.method_names.iter().enumerate() {
                    if !cm.is_class_methods[j] && method_is_emitted(cm.method_owners.get(j).map(|s| s.as_str()).unwrap_or(&flat), mname) {
                        any_body = true;
                        break;
                    }
                }
            }
            if !any_body { continue; }
            let mut entries: Vec<(String, String)> = Vec::new();
            for (j, mname) in cm.method_names.iter().enumerate() {
                if cm.is_class_methods[j] { continue; }
                // KVC keys are zero-argument instance methods with a
                // meaningful return (the ObjC getter convention).
                // method_params_list INCLUDES the implicit self/_cmd params,
                // so zero real args means len <= 2.
                if !cm.method_params_list.get(j).map_or(true, |p| p.len() <= 2) { continue; }
                let rt = cm.method_return_types.get(j).cloned().unwrap_or_default();
                if rt.is_empty() || rt == "void" { continue; }
                // ObjC KVC excludes the runtime/infra methods even though
                // they are zero-argument instance methods with returns.
                if matches!(mname.as_str(),
                    "init" | "copy" | "mutableCopy" | "retain" | "release"
                    | "autorelease" | "dealloc" | "class" | "description"
                    | "isEqual:" | "hash" | "self") { continue; }
                let owner = cm.method_owners.get(j).cloned().unwrap_or_else(|| flat.clone());
                // Same reference rule as the vtable slots (see the meta
                // vtable emission above): own methods need a body in this TU.
                if owner == flat && !method_is_emitted(&owner, mname) { continue; }
                let wrapper = format!("jeti_kvc_wrap_{}_{}", flat, mname);
                let fn_sym = format!("{}_{}", owner, mname);
                let sel = sel_const_name(mname);
                // Scalar whitelist: only clearly-scalar return types box via
                // NPNumber. Everything else (any pointer spelling, id,
                // instancetype, unknown typedefs) flows through as an object
                // — boxing an object pointer into a number is a silent
                // type-confusion, while treating an unknown scalar as an
                // object just yields an uncomparable value the engine
                // reports. (firstObject/lastObject bit on the old blacklist:
                // their declared `id` spelled differently and they took the
                // numberWithInt path with an object in hand.)
                //
                // Pointer spellings are classified FIRST: the multi-word
                // scalar matching below ("unsigned int", "long long") reads
                // `int *` as a prefix hit, which boxed a pointer into an int
                // (multi-TU 03_generic: a generic `- (T)pop` erased to
                // `NPObject *` and a `Stack<int *>` clone returning `int *`
                // both hit the numberWithInt path). A pointer to a built-in
                // (`int *`, `char *`, `void *`) is a non-object with no KVC
                // representation at all, so the key is left out: it stays
                // UNKNOWN and the engine reports it loudly, rather than
                // handing it a non-object to dispatch on.
                let rt_trim = rt.trim();
                let rt_lower = rt_trim.to_lowercase();
                let is_pointer = rt_trim.contains('*');
                let builtin = ["int", "long", "long long", "short", "char",
                    "float", "double", "bool", "_bool", "unsigned", "signed",
                    "size_t", "uint", "sint", "void"];
                let is_builtin = builtin.iter().any(|s| rt_lower == *s
                    || rt_lower.starts_with(&format!("{} ", s))
                    || rt_lower.starts_with(&format!("unsigned {}", s)));
                if is_pointer && is_builtin {
                    continue;
                }
                let is_scalar = !is_pointer && is_builtin;
                // The object path casts the call to `id`, so it needs a return
                // type that can legally be cast to a pointer: `id` /
                // `instancetype`, or a pointer to a non-built-in. A by-value
                // struct (`- (NPRange)bounds`, tests/nil_messaging_test.jeti) has
                // no such cast and no KVC representation — emitting the wrapper
                // broke the C build ("operand of type 'NPRange' where
                // arithmetic or pointer type is required"), so the key is left
                // out and stays UNKNOWN (loud) instead.
                let is_object = !is_scalar
                    && (matches!(rt_lower.as_str(), "id" | "instancetype")
                        || (is_pointer && !is_builtin));
                if !is_scalar && !is_object {
                    continue;
                }
                if !is_scalar {
                    let _ = write!(kvc_out,
                        "static id {w}(id self) {{\n    return (id){f}((NPObject *)self, {s});\n}}\n\n",
                        w = wrapper, f = fn_sym, s = sel);
                } else if !has_npnumber {
                    // No NPNumber in this TU: a scalar key cannot be boxed.
                    continue;
                } else if rt.contains("double") || rt.contains("float") {
                    // Class-method send: the receiver is the class object
                    // (&JETI_CLASS_$_NPNumber), same as an ordinary
                    // [NPNumber numberWithDouble:] lowering — NOT self.
                    // The wrapper's C result type is `id` while the factory
                    // returns `NPNumber *`: the explicit downcast keeps plain
                    // C -Wincompatible-pointer-types silent (ObjC would allow
                    // the implicit narrowing).
                    let _ = write!(kvc_out,
                        "static id {w}(id self) {{\n    return (id)NPNumber_numberWithDouble_(&JETI_CLASS_$_NPNumber, {s}, {f}((NPObject *)self, {s}));\n}}\n\n",
                        w = wrapper, f = fn_sym, s = sel);
                } else {
                    let _ = write!(kvc_out,
                        "static id {w}(id self) {{\n    return (id)NPNumber_numberWithInt_(&JETI_CLASS_$_NPNumber, {s}, {f}((NPObject *)self, {s}));\n}}\n\n",
                        w = wrapper, f = fn_sym, s = sel);
                }
                entries.push((mname.clone(), wrapper));
            }
            if entries.is_empty() { continue; }
            let sym = meta_symbol("KVC_", &flat);
            let _ = write!(kvc_out, "static const jeti_kvc_entry {}[] = {{\n", sym);
            for (key, wrapper) in &entries {
                let _ = write!(kvc_out, "    {{ .key = \"{}\", .get = {} }},\n", key, wrapper);
            }
            kvc_out.push_str("    { .key = NULL, .get = NULL },\n};\n\n");
            kvc_syms.insert(flat, sym);
        }
        if !kvc_out.is_empty() {
            out.push_str(&kvc_out);
        }
    }

    // Class metadata variables. Two lifetimes:
    //  * default: a tentative definition (a common symbol), so the per-TU
    //    copies merge harmlessly; the *contents* are written by `jeti_metaInit`
    //    below (weak — it only back-fills what static initialization did not
    //    already cover).
    //  * classes this TU OWNS (rule R2 — the @implementation is in this TU's
    //    main file): one fully initialized STRONG
    //    definition. `jeti_metaInit` is weak-merged — only one TU's copy runs,
    //    and it only initializes the classes THAT TU can see — so a class that
    //    exists only in the losing TU would stay zeroed (NULL vtable → segfault
    //    on its first alloc/init; pinned by
    //    tests/multi_tu/11_vtable_private_slots: the client's own class `App`
    //    vanished the moment the shared __sig guard stopped aborting first).
    //    Static initialization runs at load time in every scenario, needs no
    //    constructor ordering, works freestanding, and a later jeti_metaInit
    //    over it rewrites identical values (an idempotent no-op). References
    //    are safe: Section 9 already `extern`-declares every class symbol,
    //    and the vtable instances live in Section 10.
    section_comment(&mut out, comments, "Section 11 · Class metadata initialization");
    // Static NPProtocol instances (D5.2): one per @protocol declared in this
    // unit, R2-linked — strong here, weak extern elsewhere. Selector names
    // are stored colon-stripped (the vtable symbol convention).
    for (pname, parents, required, optional) in &unit.protocols {
        // ObjC spelling `@protocol P <ClassName>` puts a CLASS in the parent
        // list; classes have no NPProtocol instance, so referencing one would
        // be an undefined symbol (seen live: tt.jeti's
        // `@protocol NPCollectionDelegate <NPObject>`). Drop class parents —
        // their methods already reach conforming classes via the vtable.
        let class_names: std::collections::HashSet<String> = unit.classes.iter().map(|c| c.class_name.clone()).collect();
        let parents: Vec<&String> = parents.iter().filter(|p| !class_names.contains(*p)).collect();
        let pflat = name_flat(pname);
        let _ = write!(out, "__attribute__((used, weak)) struct NPProtocol JETI_PROTO_$_{} = {{
", pflat);
        let _ = write!(out, "    .name = \"{}\",
", pname);
        if parents.is_empty() {
            out.push_str("    .parents = NULL,
    .parent_count = 0,
");
        } else {
            let _ = write!(out, "    .parents = (struct NPProtocol *[]){{");
            for (i, pp) in parents.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                let _ = write!(out, "&JETI_PROTO_$_{}", name_flat(pp));
            }
            out.push_str("},
");
            let _ = write!(out, "    .parent_count = {},
", parents.len());
        }
        let emit_methods = |out: &mut String, field: &str, count_field: &str, methods: &[String]| {
            if methods.is_empty() {
                let _ = write!(out, "    .{} = NULL,\n    .{} = 0,\n", field, count_field);
            } else {
                let _ = write!(out, "    .{} = (NPProtocolMethod[]){{", field);
                for (i, m) in methods.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    let _ = write!(out, "{{ .name = \"{}\", .encoding = \"\" }}", m.trim_end_matches(':'));
                }
                out.push_str("},\n");
                let _ = write!(out, "    .{} = {},\n", count_field, methods.len());
            }
        };
        emit_methods(&mut out, "required_methods", "required_count", required);
        emit_methods(&mut out, "optional_methods", "optional_count", optional);
        out.push_str("};

");
    }
    // Per-class protocol-pointer arrays: NAMED static storage, never inline
    // compound literals. jeti_metaInit re-assigns the whole NPClass with a
    // compound literal — an inline `struct NPProtocol *[]` there would live
    // on the stack and dangle after metaInit returns (segfault on first
    // conformsToProtocol: walk; seen live with plist pointing into the stack).
    // Protocol refs use the SHORT name: the static instance above is emitted
    // under the declaration name, while class conformance lists carry the
    // namespace FQN (`LogTool::LogEntryProtocol` — name_flat would mash that
    // into "LogTool", an undefined symbol).
    let proto_short = |p: &String| -> String {
        name_flat(p.rsplit("::").next().unwrap_or(p))
    };
    let declared: std::collections::HashSet<String> = unit.protocols.iter().map(|(n, _, _, _)| name_flat(n)).collect();
    // Conformances with a static table in this unit, in declaration order. A
    // name that resolves to nothing declared here (e.g. a namespace prefix
    // that lost its `::Suffix` upstream) is dropped — referencing it would be
    // an undefined symbol, and a missing metadata entry is safer than a wrong
    // one. BOTH the pointer arrays and the NPClass initializers must go
    // through this same filter or they disagree (undefined-symbol build
    // failure seen live in log_analyzer.jeti).
    let known_protocols = |cm: &CgClassMeta| -> Vec<String> {
        cm.protocols.iter().filter(|p| declared.contains(&proto_short(p))).cloned().collect()
    };
    for cm in &unit.classes {
        let known = known_protocols(cm);
        if known.is_empty() { continue; }
        let flat = name_flat(&cm.class_name);
        let _ = write!(out, "__attribute__((used)) static struct NPProtocol *JETI_PROTOS_$_{}[] = {{", flat);
        for (i, p) in known.iter().enumerate() {
            if i > 0 { out.push_str(", "); }
            let _ = write!(out, "&JETI_PROTO_$_{}", proto_short(p));
        }
        out.push_str("};\n");
    }
    // A category implementation may introduce protocol conformances in a
    // different TU from the class owner. Publish those additions after static
    // metadata initialization; the runtime merges them idempotently into the
    // owner's NPClass table. Declaration-only clients do not register anything.
    for cm in &unit.classes {
        if !cm.has_category_impl || class_is_owned(unit, &cm.class_name) { continue; }
        let known = known_protocols(cm);
        if known.is_empty() { continue; }
        let flat = name_flat(&cm.class_name);
        let _ = write!(out, "__attribute__((constructor)) static void __jeti_category_protocols_$_{}(void) {{\n", flat);
        let _ = write!(out, "    jeti_register_category_protocols(&{}, JETI_PROTOS_$_{}, {});\n", meta_symbol("CLASS_", &flat), flat, known.len());
        out.push_str("}\n\n");
    }
    for cm in &unit.classes {
        let flat = name_flat(&cm.class_name);
        let sym = meta_symbol("CLASS_", &flat);
        let owned = class_is_owned(unit, &cm.class_name);
        if !owned {
            let _ = write!(out, "NPClass {};\n", sym);
            continue;
        }
        let _ = write!(out, "NPClass {} = {{\n", sym);
        out.push_str(&format!("        .name = \"{}\",\n", cm.class_name));
        if let Some(ref sup) = cm.super_name {
            out.push_str(&format!("        .superclass = &{},\n", meta_symbol("CLASS_", &name_flat(sup))));
        } else {
            out.push_str("        .superclass = NULL,\n");
        }
        out.push_str(&format!("        .instance_size = sizeof(struct {}),\n", flat));
        if cm.method_names.is_empty() {
            if let Some(ref sup) = cm.super_name {
                out.push_str(&format!("        .vtable = &{},\n", meta_symbol("VTABLE_", &name_flat(sup))));
            } else {
                out.push_str("        .vtable = NULL,\n");
            }
        } else {
            out.push_str(&format!("        .vtable = &{},\n", meta_symbol("VTABLE_", &flat)));
        }
        let has_class_methods = cm.is_class_methods.iter().any(|&c| c);
        if !has_class_methods && cm.super_name.is_none() {
            out.push_str("        .class_vtable = NULL,\n");
        } else {
            out.push_str(&format!("        .class_vtable = &{}_inst,\n", meta_symbol("META_VTABLE_", &flat)));
        }
        let known = known_protocols(cm);
        if known.is_empty() {
            out.push_str("        .protocols = NULL,\n        .protocol_count = 0,\n");
        } else {
            let _ = write!(out, "        .protocols = JETI_PROTOS_$_{},\n", flat);
            let _ = write!(out, "        .protocol_count = {},\n", known.len());
        }
        // .dealloc — same rule as jeti_metaInit below: an ARC-owned-ivar
        // wrapper when one was generated for this class, else the class's own
        // dealloc, else NULL.
        if let Some(wrapper) = arc_dealloc_names.get(&flat) {
            out.push_str(&format!("        .dealloc = (void (*)(NPObject *, SEL)){},\n", wrapper));
        } else if let Some(pos) = cm.method_names.iter().position(|n| n == "dealloc") {
            if !cm.is_class_methods[pos] {
                let owner = cm.method_owners.get(pos).cloned().unwrap_or_else(|| flat.clone());
                out.push_str(&format!("        .dealloc = (void (*)(NPObject *, SEL)){}_{},\n", owner, "dealloc"));
            } else {
                out.push_str("        .dealloc = NULL,\n");
            }
        } else {
            out.push_str("        .dealloc = NULL,\n");
        }
        // KVC table pointer — owner static metadata only. Decl-only clients
        // leave it NULL: their tentative-definition stubs must not clobber the
        // owner's table under weak merging (jeti_metaInit writes full struct
        // literals and would reset the field if it carried a pointer here).
        if let Some(sym) = kvc_syms.get(&flat) {
            out.push_str(&format!("        .kvc_entries = {},\n", sym));
        }
        out.push_str("    };\n");
    }
    if !unit.classes.is_empty() { out.push('\n'); }

    // Cross-TU vtable layout check. This lives in a per-TU constructor (not
    // in jeti_metaInit, which is weak-merged so only one TU's copy runs):
    // every TU's constructor is registered with the loader and runs, so each
    // translation unit validates its own compiled layout. Each vtable
    // instance carries the signature it was initialized for as its first
    // member; under R3 that signature covers the SHARED segment only (public
    // methods / manifest), so a TU-local private tail never trips it — a
    // mismatch means the shared layouts disagree, and dispatch through this
    // TU's `struct jeti_vtable` layout would read the wrong slot. Abort with
    // a clear message instead.
    if any_has_instance && !unit.classes.is_empty() {
        let method_list: Vec<String> = unit.global_instance_method_names.clone();
        let _ = write!(out, "__attribute__((constructor)) static void __jeti_vtable_layout_check(void) {{\n");
        for cm in &unit.classes {
            if cm.method_names.is_empty() && cm.super_name.is_none() { continue; }
            let vt_sym = meta_symbol("VTABLE_", &name_flat(&cm.class_name));
            let _ = write!(out, "    jeti_verify_vtable_sig((&{})->__sig, 0x{:016x}ULL, \"{} | class {} | tu {}\");\n",
                vt_sym, vtable_sig, method_list.join(" "), cm.class_name, unit.filename);
            let layout_text = class_layout_text(cm);
            let _ = write!(out, "    jeti_verify_layout_sig((&{})->__layout_sig, 0x{:016x}ULL, (&{})->__layout_text, {});\n",
                vt_sym, text_layout_sig(&layout_text), vt_sym, c_string_literal(&layout_text));
        }
        let _ = write!(out, "}}\n\n");
    }

    // (ARC dealloc wrappers are computed and emitted above, before Section 11:
    // both the owned-class static definitions and jeti_metaInit reference them.)

    // jeti_metaInit() — emitted (weak, empty when the unit has no classes):
    // hand-written `main` naturally calls jeti_meta_init(), and a class-less TU
    // must still link. Skipped when the source already carries one — re-processing
    // jeti's own generated C sees that definition as ordinary source, and a
    // second copy is a hard redefinition.
    let source_defines_meta_init = unit.decls.iter().any(|d| {
        matches!(d.data, CgDeclData::Function { .. })
            && (d.name == "jeti_metaInit" || d.name == "jeti_meta_init")
    });
    if !source_defines_meta_init {
        out.push_str(&format!("{}void jeti_metaInit(void) {{\n", meta_weak));
        for cm in &unit.classes {
            let _ = write!(out, "    {} = (NPClass){{\n", meta_symbol("CLASS_", &name_flat(&cm.class_name)));
            out.push_str(&format!("        .name = \"{}\",\n", cm.class_name));
            if let Some(ref sup) = cm.super_name {
                out.push_str(&format!("        .superclass = &{},\n", meta_symbol("CLASS_", &name_flat(sup))));
            } else {
                out.push_str("        .superclass = NULL,\n");
            }
            out.push_str(&format!("        .instance_size = sizeof(struct {}),\n", name_flat(&cm.class_name)));
            if cm.method_names.is_empty() {
                if let Some(ref sup) = cm.super_name {
                    out.push_str(&format!("        .vtable = &{},\n", meta_symbol("VTABLE_", &name_flat(sup))));
                } else {
                    out.push_str("        .vtable = NULL,\n");
                }
            } else {
                out.push_str(&format!("        .vtable = &{},\n", meta_symbol("VTABLE_", &name_flat(&cm.class_name))));
            }
            let has_class_methods = cm.is_class_methods.iter().any(|&c| c);
            let has_super = cm.super_name.is_some();
            if !has_class_methods && !has_super {
                out.push_str("        .class_vtable = NULL,\n");
            } else {
                out.push_str(&format!("        .class_vtable = &{}_inst,\n", meta_symbol("META_VTABLE_", &name_flat(&cm.class_name))));
            }
            let known = known_protocols(cm);
            if known.is_empty() {
                out.push_str("        .protocols = NULL,\n        .protocol_count = 0,\n");
            } else {
                let _ = write!(out, "        .protocols = JETI_PROTOS_$_{},\n", name_flat(&cm.class_name));
                let _ = write!(out, "        .protocol_count = {},\n", known.len());
            }
            // .dealloc — populate from the vtable so jeti_release() can call it.
            // A class with owned object ivars points at a generated wrapper
            // (see emit_arc_dealloc_wrappers): it runs the class's normal
            // dealloc chain and then releases the ivars ARC owns.
            let flat_here = name_flat(&cm.class_name);
            if let Some(wrapper) = arc_dealloc_names.get(&flat_here) {
                out.push_str(&format!("        .dealloc = (void (*)(NPObject *, SEL)){},\n", wrapper));
            } else if let Some(pos) = cm.method_names.iter().position(|n| n == "dealloc") {
                if !cm.is_class_methods[pos] {
                    let owner = cm.method_owners.get(pos).cloned().unwrap_or_else(|| name_flat(&cm.class_name));
                    out.push_str(&format!("        .dealloc = (void (*)(NPObject *, SEL)){}_{},\n", owner, "dealloc"));
                } else {
                    out.push_str("        .dealloc = NULL,\n");
                }
            } else {
                out.push_str("        .dealloc = NULL,\n");
            }
            // KVC table pointer — mirror Section 11's static initialization
            // exactly: this backfill is a FULL struct-literal assignment, so a
            // class whose table was statically initialized would have the
            // pointer zeroed here if the field were omitted (the idempotent
            // backfill contract). Only TUs that computed a table write it;
            // decl-only clients omit it here just as they do in Section 11.
            if let Some(sym) = kvc_syms.get(&flat_here) {
                out.push_str(&format!("        .kvc_entries = {},\n", sym));
            }
            out.push_str("    };\n");
        }
        out.push_str("}\n\n");
        // snake_case alias: every other runtime symbol is snake_case, so
        // hand-written host code calls `jeti_meta_init()`. Weak like the
        // original so the many per-TU copies coalesce to one.
        out.push_str(&format!("{}void jeti_meta_init(void) {{ jeti_metaInit(); }}\n\n", meta_weak));
    }

    // jeti_stringFromCstr — emitted when NPString class is present.
    // Hosted builds INTERN: identical contents map to ONE shared instance
    // (ObjC constant-`@"..."` semantics), so pointer equality across literal
    // occurrences works (`containsObject:`/`indexOfObject:` with a fresh
    // `@"key"` now finds the stored element). The table retains the object once
    // (see the `jeti_retain` below) so it truly owns its +1 forever — the
    // result is a shared constant, not a pooled temporary; MRC code must not
    // release it (same rule as ObjC constant strings). Table cap 256: when full,
    // fall back to a fresh
    // object (correct, just not interned). Lazy-init table is a plain global
    // — single-threaded assumption, same class as the runtime's weak side
    //     table. Freestanding keeps the old fresh-object body (no <string.h>).
    section_comment(&mut out, comments, "Section 12 · Runtime support");
    if unit.classes.iter().any(|c| c.class_name == "NPString") {
        out.push_str("__attribute__((weak)) NPObject *jeti_stringFromCstr(const char *cstr) {\n");
        out.push_str("#ifdef __JETI_FREESTANDING\n");
        out.push_str("    if (!cstr) cstr = \"\";\n");
        out.push_str(&format!("    NPObject *obj = jeti_alloc(&{});\n", meta_symbol("CLASS_", "NPString")));
        out.push_str("    if (!obj) return NULL;\n");
        out.push_str("    struct NPString *str = (struct NPString *)obj;\n");
        out.push_str("    size_t len = strlen(cstr);\n");
        out.push_str("    str->_cstr = (char *)malloc(len + 1);\n");
        out.push_str("    if (str->_cstr) strcpy(str->_cstr, cstr);\n");
        out.push_str("    str->_length = len;\n");
        out.push_str("    str->_hash = 0;\n");
        out.push_str("    str->_hashIsValid = 0;\n");
        out.push_str("    return jeti_autorelease(obj);\n");
        out.push_str("#else\n");
        out.push_str("    if (!cstr) cstr = \"\";\n");
        out.push_str("    static struct { const char *cstr; NPObject *obj; } jeti_intern_table[256];\n");
        out.push_str("    static int jeti_intern_count = 0;\n");
        out.push_str("    for (int i = 0; i < jeti_intern_count; i++) {\n");
        out.push_str("        if (jeti_intern_table[i].cstr == cstr || strcmp(jeti_intern_table[i].cstr, cstr) == 0)\n");
        out.push_str("            return jeti_intern_table[i].obj;\n");
        out.push_str("    }\n");
        out.push_str(&format!("    NPObject *obj = jeti_alloc(&{});\n", meta_symbol("CLASS_", "NPString")));
        out.push_str("    if (!obj) return NULL;\n");
        out.push_str("    struct NPString *str = (struct NPString *)obj;\n");
        out.push_str("    size_t len = strlen(cstr);\n");
        out.push_str("    str->_cstr = (char *)malloc(len + 1);\n");
        out.push_str("    if (str->_cstr) strcpy(str->_cstr, cstr);\n");
        out.push_str("    str->_length = len;\n");
        out.push_str("    str->_hash = 0;\n");
        out.push_str("    str->_hashIsValid = 0;\n");
        // The table must hold a reference of its OWN, not alias the object's
        // initial +1. A literal is commonly stored straight into an object ivar
        // (`h->_s = @"x";`): ARC does not retain that assignment, but the
        // synthesised dealloc does release the ivar — which would consume the
        // table's only reference, free the "immortal" constant, and leave the
        // table pointing at freed memory (heap-use-after-free in the next
        // intern lookup's strcmp). See docs/plans/arc_intern_uaf.md.
        out.push_str("    jeti_retain(obj);\n");
        out.push_str("    if (jeti_intern_count < 256) {\n");
        out.push_str("        jeti_intern_table[jeti_intern_count].cstr = str->_cstr;\n");
        out.push_str("        jeti_intern_table[jeti_intern_count].obj = obj;\n");
        out.push_str("        jeti_intern_count++;\n");
        out.push_str("    }\n");
        out.push_str("    return obj;\n");
        out.push_str("#endif\n");
        out.push_str("}\n\n");
    }

    // jeti_array_create — emitted when NPArray class is present
    if unit.classes.iter().any(|c| c.class_name == "NPArray") {
        out.push_str("__attribute__((weak)) NPObject *jeti_array_create(size_t count, ...) {\n");
        out.push_str(&format!("    NPObject *arr = jeti_alloc(&{});\n", meta_symbol("CLASS_", "NPArray")));
        out.push_str("    if (!arr) return NULL;\n");
        out.push_str("    struct NPArray *a = (struct NPArray *)arr;\n");
        out.push_str("    if (count > 0) {\n");
        out.push_str("        a->_items = (NPObject **)malloc(count * sizeof(NPObject *));\n");
        out.push_str("        if (a->_items) {\n");
        out.push_str("            va_list ap;\n");
        out.push_str("            va_start(ap, count);\n");
        out.push_str("            for (size_t i = 0; i < count; i++) {\n");
        out.push_str("                NPObject *obj = va_arg(ap, NPObject *);\n");
        out.push_str("                a->_items[i] = obj ? jeti_retain(obj) : NULL;\n");
        out.push_str("            }\n");
        out.push_str("            va_end(ap);\n");
        out.push_str("            a->_count = count;\n");
        out.push_str("            a->_capacity = count;\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("    return jeti_autorelease(arr);\n");
        out.push_str("}\n\n");
    }

    // jeti_dictionary_create — emitted when NPDictionary class is present.
    // Alternating key/value varargs, one pair per `@{}` entry.
    if unit.classes.iter().any(|c| c.class_name == "NPDictionary") {
        out.push_str("__attribute__((weak)) NPObject *jeti_dictionary_create(size_t count, ...) {\n");
        out.push_str(&format!("    NPObject *obj = jeti_alloc(&{});\n", meta_symbol("CLASS_", "NPDictionary")));
        out.push_str("    if (!obj) return NULL;\n");
        out.push_str("    struct NPDictionary *dict = (struct NPDictionary *)obj;\n");
        out.push_str("    if (count > 0) {\n");
        out.push_str("        dict->_keys = (NPObject **)calloc(count, sizeof(NPObject *));\n");
        out.push_str("        dict->_values = (NPObject **)calloc(count, sizeof(NPObject *));\n");
        out.push_str("        if (dict->_keys && dict->_values) {\n");
        out.push_str("            va_list ap;\n");
        out.push_str("            va_start(ap, count);\n");
        out.push_str("            size_t stored = 0;\n");
        out.push_str("            for (size_t i = 0; i < count; i++) {\n");
        out.push_str("                NPObject *k = va_arg(ap, NPObject *);\n");
        out.push_str("                NPObject *v = va_arg(ap, NPObject *);\n");
        out.push_str("                if (!k) continue;\n");
        out.push_str("                dict->_keys[stored] = jeti_retain(k);\n");
        out.push_str("                dict->_values[stored] = v ? jeti_retain(v) : NULL;\n");
        out.push_str("                stored++;\n");
        out.push_str("            }\n");
        out.push_str("            va_end(ap);\n");
        out.push_str("            dict->_count = stored;\n");
        out.push_str("            dict->_capacity = count;\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("    return jeti_autorelease(obj);\n");
        out.push_str("}\n\n");
    }

    // ─── Block header struct (gcc/portable: shared by all expanded blocks) ──
    if !is_clang_backend() {
        out.push_str("struct __jeti_block_header {\n    void *isa;\n    int flags;\n    int reserved;\n    void (*invoke)(void *, ...);\n};\n\n");
    }

    // ─── Block expansion definitions (gcc/portable) ──
    {
        let defs = block_defs();
        if !defs.is_empty() {
            if comments {
                out.push_str("/* Block expansion definitions (gcc/portable) */\n");
            } else {
                out.push_str("// ─── Block expansion ───\n");
            }
            out.push_str(&defs);
            out.push('\n');
        }
    }

    // Function definitions
    section_comment(&mut out, comments, "Section 13 · Function bodies");
    if comments {
        let mut method_comments: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for cm in &unit.classes {
            for (i, mname) in cm.method_names.iter().enumerate() {
                let owner = cm.method_owners.get(i).cloned().unwrap_or_else(|| name_flat(&cm.class_name));
                let key = format!("{}_{}", owner, mname);
                if method_comments.contains_key(&key) { continue; }
                let sel = cm.method_sel_names.get(i).cloned().unwrap_or_else(|| mname.clone());
                let kind = if cm.is_class_methods.get(i).copied().unwrap_or(false) { "+" } else { "-" };
                method_comments.insert(key, format!("{kind}[{} {sel}]", cm.class_name.split("::").last().unwrap_or(&cm.class_name)));
            }
        }
        for decl in &unit.decls {
            if let Some(sig) = method_comments.get(&decl.name) {
                let _ = writeln!(out, "/* {} */", sig);
            }
            // File-scope pass-through lines (`_Pragma("...")`, `#pragma mark`)
            // were already emitted above, in source order, interleaved with the
            // struct definitions they may have to precede.
            if aggregates_emitted && matches!(decl.data, CgDeclData::RawLine(_)) { continue; }
            if !matches!(decl.data, CgDeclData::Enum { .. })
                && !matches!(decl.data, CgDeclData::Typedef { .. })
                && !matches!(decl.data, CgDeclData::Variable { .. })
                && !matches!(decl.data, CgDeclData::Struct { .. }) {
                emit_decl(decl, &mut out);
                out.push('\n');
            }
        }
    } else {
        for decl in &unit.decls {
            // Skip enum, typedef, and variable decls — they are already emitted
            // in the forward-declaration phase above. Emitting again here would
            // cause `redefinition` errors.
            if matches!(decl.data, CgDeclData::Enum { .. }) { continue; }
            if matches!(decl.data, CgDeclData::Typedef { .. }) { continue; }
            if matches!(decl.data, CgDeclData::Variable { .. }) { continue; }
            if matches!(decl.data, CgDeclData::Struct { .. }) { continue; }
            if aggregates_emitted && matches!(decl.data, CgDeclData::RawLine(_)) { continue; }
            emit_decl(decl, &mut out);
            out.push('\n');
        }
    }

    normalize_t_sentinels_text(&out)
}

pub fn emit_unit(unit: &CgUnit) -> String {
    emit_unit_with_headers(unit, &[], &[], false, Backend::Portable, false, false)
}

/// Generate a C bridge header so plain C code can call Jeti methods without
/// writing vtable dispatch or SEL constants by hand.
///
/// For each class method and instance method it emits:
///   - an `extern` declaration of the generated function (`Class_method`),
///   - a `static inline` wrapper `jeti_Class_method(...)` that hides the SEL
///     (and the class object for class methods).
///
/// Usage from C:
///   #include "jeti_bridge.h"
///   NPString *s = jeti_NPString_stringWithUTF8String("hello");
///   const char *c = jeti_NPString_UTF8String(s);
/// Emit the call of the wrapped method inside a bridge-header inline wrapper,
/// followed by the checked-EH guard. `-eh checked` compiles `@throw` into
/// `__jeti_eh_flag = 1` + a zero return, so a pure-C caller that ignores the
/// flag would swallow the exception silently. The wrapper checks it and aborts
/// with ObjC wording (`jeti_eh_uncaught` lives in runtime.c; runtime.h is
/// already included at the top of every bridge header). The checked backend
/// settles every frame before returning, so there is nothing left to clean up
/// at this boundary — aborting is the safe downgrade.
fn emit_bridge_eh_guard(out: &mut String, ret: &str, call: &str) {
    if ret == "void" {
        out.push_str(&format!("    {};\n", call));
    } else {
        out.push_str(&format!("    {} __jeti_ret = {};\n", ret, call));
    }
    out.push_str("    if (__jeti_eh_flag) { jeti_eh_uncaught(); }\n");
    if ret != "void" {
        out.push_str("    return __jeti_ret;\n");
    }
}

pub fn emit_bridge_header(unit: &CgUnit) -> String {
    let mut out = String::new();
    out.push_str("// Automatically generated by jetic --emit-bridge-header. Do not edit.\n");
    out.push_str("#ifndef JETI_BRIDGE_H\n");
    out.push_str("#define JETI_BRIDGE_H\n\n");
    out.push_str("#include <jeti/runtime.h>\n\n");

    // Forward-declare all class structs.
    for cls in &unit.classes {
        let flat = name_flat(&cls.class_name);
        out.push_str(&format!("struct {};\n", flat));
        out.push_str(&format!("typedef struct {} {};\n", flat, flat));
    }
    out.push_str("typedef struct { size_t location; size_t length; } NPRange;\n\n");
    out.push_str("extern void jeti_metaInit(void);\n\n");

    for cls in &unit.classes {
        let flat = name_flat(&cls.class_name);
        if flat == "jeti_root" { continue; }
        for (i, sel) in cls.method_names.iter().enumerate() {
            let is_class = cls.is_class_methods.get(i).copied().unwrap_or(false);
            let ret = cls.method_return_types.get(i).cloned().unwrap_or_else(|| "void".into());
            let params = cls.method_params_list.get(i).cloned().unwrap_or_default();
            let fn_name = format!("{}_{}", flat, sel);
            let wrapper_name = format!("jeti_{}_{}", flat, sel);
            let orig_sel = cls.method_sel_names.get(i).cloned().unwrap_or_else(|| sel.clone());

            // method_params_list includes self and _cmd as the first two entries.
            let extra_params: Vec<(String, String)> = params.into_iter().skip(2).collect();
            let mut decl_params: Vec<String> = Vec::new();
            let mut call_names: Vec<String> = Vec::new();
            for (pt, pn) in &extra_params {
                decl_params.push(format!("{} {}", pt, pn));
                call_names.push(pn.clone());
            }

            if is_class {
                let sig = if decl_params.is_empty() {
                    "NPClass *self, SEL _cmd".to_string()
                } else {
                    format!("NPClass *self, SEL _cmd, {}", decl_params.join(", "))
                };
                out.push_str(&format!("extern {} {}({});\n", ret, fn_name, sig));
                out.push_str(&format!("extern NPClass JETI_CLASS_$_{};\n\n", flat));
                out.push_str(&format!("static inline {} {}({}) {{\n",
                    ret, wrapper_name,
                    if decl_params.is_empty() { "void".to_string() } else { decl_params.join(", ") }));
                out.push_str(&format!("    SEL _sel = sel_registerName(\"{}\");\n", orig_sel));
                let call = format!("{}(&JETI_CLASS_$_{}, _sel{})",
                    fn_name, flat,
                    if call_names.is_empty() { String::new() } else { format!(", {}", call_names.join(", ")) });
                emit_bridge_eh_guard(&mut out, &ret, &call);
                out.push_str("}\n\n");
            } else {
                let sig = if decl_params.is_empty() {
                    "NPObject *self, SEL _cmd".to_string()
                } else {
                    format!("NPObject *self, SEL _cmd, {}", decl_params.join(", "))
                };
                out.push_str(&format!("extern {} {}({});\n", ret, fn_name, sig));
                out.push_str(&format!("static inline {} {}(void *self{}) {{\n",
                    ret, wrapper_name,
                    if extra_params.is_empty() { String::new() } else { format!(", {}", decl_params.join(", ")) }));
                out.push_str(&format!("    SEL _sel = sel_registerName(\"{}\");\n", orig_sel));
                let call = format!("{}((NPObject *)self, _sel{})",
                    fn_name,
                    if call_names.is_empty() { String::new() } else { format!(", {}", call_names.join(", ")) });
                emit_bridge_eh_guard(&mut out, &ret, &call);
                out.push_str("}\n\n");
            }
        }
    }
    out.push_str("#endif /* JETI_BRIDGE_H */\n");
    out
}
#[cfg(test)]
mod vtable_sig_tests {
    use super::*;

    /// The layout signature is a pure FNV-1a hash over the sorted instance
    /// method names, which is exactly what `emit_unit_with_headers` stamps into
    /// the vtable struct and every vtable instance. Two units agree iff they
    /// compiled the same method set.
    #[test]
    fn differing_method_sets_produce_differing_signatures() {
        let a = vtable_layout_sig(&["init".to_string(), "show".to_string()]);
        let b = vtable_layout_sig(&["init".to_string(), "show".to_string(), "extra".to_string()]);
        assert_ne!(a, b, "different method sets must yield different vtable layout signatures");
    }

    #[test]
    fn identical_method_sets_produce_identical_signatures() {
        let a = vtable_layout_sig(&["init".to_string(), "show".to_string()]);
        let b = vtable_layout_sig(&["init".to_string(), "show".to_string()]);
        assert_eq!(a, b);
    }

    /// The signature must travel inside the vtable struct (first member) so it
    /// is weak-merged together with the instance that actually won the link,
    /// and the check must live in a per-TU constructor — `jeti_metaInit` is
    /// weak-merged, so only one TU's copy would ever run.
    #[test]
    fn signature_travels_in_vtable_and_check_runs_per_tu() {
        // Exercise the real emitter over a tiny class so the assertions below
        // run against actual generated C, not a hand-written fixture.
        // Build the uniform method set the pipeline would collect for a unit
        // containing `@interface Probe : NPObject` with a single `-ping`,
        // plus that class so the vtable scaffolding is actually emitted.
        let unit = CgUnit {
            decls: Vec::new(),
            filename: "probe.jeti".to_string(),
            c_headers: vec!["#include <stdio.h>".to_string()],
            selectors: Vec::new(),
            classes: vec![CgClassMeta {
                class_name: "Probe".to_string(),
                super_name: Some("NPObject".to_string()),
                method_names: vec!["ping".to_string()],
                method_sel_names: vec!["ping".to_string()],
                is_class_methods: vec![false],
                method_return_types: vec!["void".to_string()],
                method_params_list: vec![vec![]],
                method_variadic: vec![false],
                method_owners: vec!["Probe".to_string()],
                category_method_names: std::collections::HashSet::new(),
                has_category_impl: false,
                vtable_indices: vec![2],
                ivar_types: Vec::new(),
                ivar_names: Vec::new(),
                ivar_weak: Vec::new(),
                properties: Vec::new(),
                has_impl: true,
                protocols: Vec::new(),
            }],
            struct_eq_tags: Vec::new(),
            no_arc: false,
            owned_classes: std::collections::HashSet::new(),
            instantiate_only: false,
            kvc: false,
            protocols: Vec::new(),
            generic_demands: Vec::new(),
            global_instance_method_names: vec![
                "dealloc".into(), "init".into(), "ping".into(), "release".into(), "retain".into(),
            ],
            // Emitter test: the sig is computed from this list verbatim.
            vtable_sig_names: vec!["dealloc".into(), "init".into(), "ping".into(), "release".into(), "retain".into()],
        };
        // The emitter needs METHOD_METADATA populated; seed it once for this
        // test binary. `set` is idempotent from the test's point of view.
        let _ = METHOD_METADATA.set(HashMap::from([
            ("dealloc".to_string(), (0usize, "void (*)(NPObject *, SEL)".to_string())),
            ("init".to_string(), (0usize, "NPObject * (*)(NPObject *, SEL)".to_string())),
            ("ping".to_string(), (0usize, "void (*)(NPObject *, SEL)".to_string())),
            ("release".to_string(), (0usize, "void (*)(NPObject *, SEL)".to_string())),
            ("retain".to_string(), (0usize, "NPObject * (*)(NPObject *, SEL)".to_string())),
        ]));
        let headers = unit.c_headers.clone();
        let c = emit_unit_with_headers(&unit, &headers, &[], false, Backend::Clang, false, false);
        assert!(
            c.contains("unsigned long long __sig;"),
            "vtable struct must carry a __sig member so it merges with the instance"
        );
        assert!(
            c.contains("__attribute__((constructor)) static void __jeti_vtable_layout_check(void)"),
            "the layout check must live in a per-TU constructor, not in weak-merged jeti_metaInit"
        );
        assert!(c.contains("jeti_verify_vtable_sig"), "the verifier must be emitted");
    }

    /// R4: the ivar-section twin of `__sig`. A stale `.hje` must change the
    /// fingerprint, and the emitter must carry the payload (both spellings) in
    /// the vtable instance so the startup check can print the actual diff.
    #[test]
    fn differing_ivar_sections_produce_differing_layout_signatures() {
        let mut cm = CgClassMeta {
            class_name: "Widget".to_string(),
            super_name: Some("NPObject".to_string()),
            method_names: vec!["sum".to_string()],
            method_sel_names: vec!["sum".to_string()],
            is_class_methods: vec![false],
            method_return_types: vec!["int".to_string()],
            method_params_list: vec![vec![]],
            method_variadic: vec![false],
            method_owners: vec!["Widget".to_string()],
            category_method_names: std::collections::HashSet::new(),
            has_category_impl: false,
            vtable_indices: vec![2],
            ivar_types: vec!["int".into(), "int".into()],
            ivar_names: vec!["_a".into(), "_b".into()],
            ivar_weak: vec![false, false],
            properties: Vec::new(),
            has_impl: true,
            protocols: Vec::new(),
        };
        let fresh = class_layout_text(&cm);
        assert_eq!(fresh, "Widget < NPObject | int _a | int _b");
        let fresh_sig = text_layout_sig(&fresh);

        // The stale header a client still carries: one ivar retyped, one added.
        cm.ivar_types = vec!["short".into(), "int".into(), "int".into()];
        cm.ivar_names = vec!["_a".into(), "_b".into(), "_c".into()];
        cm.ivar_weak = vec![false, false, false];
        let stale = class_layout_text(&cm);
        assert_ne!(fresh_sig, text_layout_sig(&stale), "a retyped/added ivar must change the sig");

        // Reordering matters too: the struct is emitted in declaration order,
        // so a swapped pair is a real layout change, not a cosmetic one.
        cm.ivar_types = vec!["int".into(), "int".into()];
        cm.ivar_names = vec!["_b".into(), "_a".into()];
        cm.ivar_weak = vec![false, false];
        assert_ne!(fresh_sig, text_layout_sig(&class_layout_text(&cm)));
    }

    #[test]
    fn layout_check_is_emitted_per_class() {
        let unit = CgUnit {
            decls: Vec::new(),
            filename: "probe.jeti".to_string(),
            c_headers: vec!["#include <stdio.h>".to_string()],
            selectors: Vec::new(),
            classes: vec![CgClassMeta {
                class_name: "Probe".to_string(),
                super_name: Some("NPObject".to_string()),
                method_names: vec!["ping".to_string()],
                method_sel_names: vec!["ping".to_string()],
                is_class_methods: vec![false],
                method_return_types: vec!["void".to_string()],
                method_params_list: vec![vec![]],
                method_variadic: vec![false],
                method_owners: vec!["Probe".to_string()],
                category_method_names: std::collections::HashSet::new(),
                has_category_impl: false,
                vtable_indices: vec![2],
                ivar_types: vec!["int".to_string()],
                ivar_names: vec!["_grade".to_string()],
                ivar_weak: vec![false],
                properties: Vec::new(),
                has_impl: true,
                protocols: Vec::new(),
            }],
            struct_eq_tags: Vec::new(),
            no_arc: false,
            owned_classes: std::collections::HashSet::new(),
            instantiate_only: false,
            kvc: false,
            protocols: Vec::new(),
            generic_demands: Vec::new(),
            global_instance_method_names: vec![
                "dealloc".into(), "init".into(), "ping".into(), "release".into(), "retain".into(),
            ],
            vtable_sig_names: vec!["dealloc".into(), "init".into(), "ping".into(), "release".into(), "retain".into()],
        };
        let _ = METHOD_METADATA.set(HashMap::from([
            ("dealloc".to_string(), (0usize, "void (*)(NPObject *, SEL)".to_string())),
            ("init".to_string(), (0usize, "NPObject * (*)(NPObject *, SEL)".to_string())),
            ("ping".to_string(), (0usize, "void (*)(NPObject *, SEL)".to_string())),
            ("release".to_string(), (0usize, "void (*)(NPObject *, SEL)".to_string())),
            ("retain".to_string(), (0usize, "NPObject * (*)(NPObject *, SEL)".to_string())),
        ]));
        let headers = unit.c_headers.clone();
        let c = emit_unit_with_headers(&unit, &headers, &[], false, Backend::Clang, false, false);
        assert!(c.contains("unsigned long long __layout_sig;"), "vtable struct must carry the layout sig");
        assert!(c.contains("const char *__layout_text;"), "the payload must travel with the instance");
        assert!(c.contains("jeti_verify_layout_sig"), "the layout verifier must be emitted");
        assert!(
            c.contains(".\"__layout_text\"") || c.contains(".__layout_text = \"Probe < NPObject | int _grade\""),
            "each instance must stamp its own ivar spelling, got: {}",
            c.lines().find(|l| l.contains("__layout_text =")).unwrap_or("<none>")
        );
    }

    /// R3: a TU-local private method must not change the fingerprint. A legal
    /// library+client split (`tests/multi_tu/11_vtable_private_slots`) compiles
    /// different method sets over the same shared (public) segment, so both
    /// sigs must agree — that is what un-trips the runtime `__sig` guard.
    #[test]
    fn private_methods_do_not_change_the_signature_segment() {
        let public: std::collections::HashSet<String> =
            ["init".to_string(), "show".to_string()].into_iter().collect();
        // lib TU sees a private helper; the client TU does not.
        let lib = vtable_sig_segment(None, Some(&public), &[
            "init".to_string(), "privateHelper".to_string(), "show".to_string(),
        ]);
        let client = vtable_sig_segment(None, Some(&public), &[
            "init".to_string(), "show".to_string(),
        ]);
        assert_eq!(lib, client, "the shared segment must be identical despite the private tail");
        assert_eq!(vtable_layout_sig(&lib), vtable_layout_sig(&client));
    }

    #[test]
    fn manifest_mode_fingerprints_the_manifests_public_entries() {
        let manifest = vec!["alloc".to_string(), "mul".to_string(), "show".to_string()];
        let public: std::collections::HashSet<String> =
            ["show".to_string()].into_iter().collect();
        let seg = vtable_sig_segment(
            Some(&manifest), Some(&public), &["privateHelper".to_string(), "show".to_string()],
        );
        assert_eq!(seg, vec!["show".to_string()],
            "with a manifest, the manifest's public entries are the shared segment — \
             the creator's private tail must not leak into the fingerprint");
    }

    /// The manifest's creator compiles manifest-less (the file does not exist
    /// until it writes its assignment back), so creator and follower take
    /// different code paths — they must still fingerprint the same segment.
    #[test]
    fn manifest_creator_and_follower_agree_on_the_segment() {
        let public: std::collections::HashSet<String> =
            ["add".to_string(), "init".to_string()].into_iter().collect();
        // Creator: no manifest file yet → its own R1-ordered set (public
        // alphabetical first, private tail last).
        let creator = vtable_sig_segment(None, Some(&public), &[
            "add".to_string(), "init".to_string(), "mul".to_string(),
        ]);
        // Follower: reads back the manifest the creator wrote (the creator's
        // full set, private tail included).
        let manifest = vec!["add".to_string(), "init".to_string(), "mul".to_string()];
        let follower = vtable_sig_segment(Some(&manifest), Some(&public), &[]);
        assert_eq!(creator, follower, "creator (manifest-less) and follower (manifest) \
            must fingerprint the identical public segment");
        assert_eq!(vtable_layout_sig(&creator), vtable_layout_sig(&follower));
    }

    #[test]
    fn no_public_info_covers_every_method_like_pre_r3() {
        let all = ["a".to_string(), "b".to_string()];
        assert_eq!(vtable_sig_segment(None, None, &all), all);
    }
}

/// Regression guard for the second-referendum root cause: `-eh checked`
/// writes `__jeti_eh_flag` / `__jeti_eh_val` in every function with a
/// throwing callee, but their declarations live only in `jeti/runtime.h`.
/// A pure C-superset file (no Foundation import, no block literal) generates
/// C that never pulled runtime.h in, so clang rejected the whole unit with
/// `use of undeclared identifier '__jeti_eh_flag'`. The include must not
/// depend on the Foundation/blocks heuristic.
#[cfg(test)]
mod eh_runtime_include_tests {
    use super::*;

    fn unit_with_c_headers(c_headers: Vec<String>) -> CgUnit {
        CgUnit {
            decls: Vec::new(),
            filename: "c_superset.jeti".to_string(),
            c_headers,
            selectors: Vec::new(),
            classes: Vec::new(),
            global_instance_method_names: Vec::new(),
            struct_eq_tags: Vec::new(),
            no_arc: false,
            owned_classes: std::collections::HashSet::new(),
            instantiate_only: false,
            kvc: false,
            protocols: Vec::new(),
            generic_demands: Vec::new(),
            vtable_sig_names: Vec::new(),
        }
    }

    #[test]
    fn eh_checked_pulls_runtime_h_into_a_pure_c_superset_unit() {
        let unit = unit_with_c_headers(vec!["#include <stdio.h>".to_string()]);
        let c = emit_unit_with_headers(&unit, &unit.c_headers, &[], false, Backend::Clang, false, true);
        assert!(
            c.contains("#include <jeti/runtime.h>"),
            "a pure C-superset unit compiled with -eh checked must include runtime.h — \
             __jeti_eh_flag/__jeti_eh_val are otherwise undeclared (referendum #2)"
        );
    }

    #[test]
    fn legacy_backend_does_not_pull_runtime_h() {
        let unit = unit_with_c_headers(vec!["#include <stdio.h>".to_string()]);
        let c = emit_unit_with_headers(&unit, &unit.c_headers, &[], false, Backend::Clang, false, false);
        assert!(
            !c.contains("#include <jeti/runtime.h>"),
            "the default sjlj backend emits no EH global access — runtime.h must not \
             be dragged in (zero behavior change for the default path)"
        );
    }

    /// No duplicate include when the file already pulls runtime.h itself
    /// (the trace goldens do `#import jeti/runtime.h` directly).
    #[test]
    fn eh_checked_does_not_duplicate_an_existing_runtime_h_include() {
        let unit = unit_with_c_headers(vec![
            "#include <stdio.h>".to_string(),
            "#include <jeti/runtime.h>".to_string(),
        ]);
        let c = emit_unit_with_headers(&unit, &unit.c_headers, &[], false, Backend::Clang, false, true);
        assert_eq!(
            c.matches("#include <jeti/runtime.h>").count(),
            1,
            "runtime.h must not be included twice"
        );
    }
}
