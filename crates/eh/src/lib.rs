//! Checked-exception desugar ("Swift scheme", milestone 1).
//!
//! Replaces setjmp/longjmp semantics with ordinary control flow so that ARC
//! scope-end releases on intermediate frames are never skipped (the cross-
//! function leak of the sjlj backend disappears by construction):
//!
//! - `@throw e`      -> `__jeti_eh_flag = 1; __jeti_eh_val = (NPObject *)e; return <zero>;`
//! - every call site -> call, then `if (__jeti_eh_flag) { return <zero>; }`
//! - `@try` body     -> plain code (flag is provably clear on entry)
//! - `@catch (T *e)` -> `if (__jeti_eh_flag) { e = (T *)__jeti_eh_val; __jeti_eh_flag = 0; ... }`
//!                      with isa check for typed catches; non-matching typed
//!                      catch re-arms the flag and falls through.
//! - `@finally`      -> runs on the natural merge point (both paths are
//!                      ordinary control flow).
//!
//! M1 limits (enforced by `check_unit`):
//!   1. `@throw` only as a whole statement (not inside a larger expression).
//!   2. No `@throw` inside a block literal.
//!   3. No `@try` spanning... nothing else: unlike sjlj, everything else works.
//!
//! The desugar must run BEFORE ARC (pipeline Step 4.8 precedent) so the
//! injected early `return`s are ordinary control flow that ARC already
//! handles (releases owned locals before the return).

#![deny(dead_code)]

use jeti_ast::{AstDecl, AstDeclData, AstExpr, AstExprData, AstStmt, AstStmtData, AstUnit};
use std::fmt;

// ─── Diagnostics ─────────────────────────────────────────────────────────────

pub struct EhDiagnostics {
    pub errors: Vec<String>,
}

impl EhDiagnostics {
    fn new() -> Self {
        Self { errors: Vec::new() }
    }
    fn err(&mut self, msg: String) {
        self.errors.push(msg);
    }
}

impl fmt::Debug for EhDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.errors)
    }
}

// ─── Check pass (pre-desugar, M1 limits) ─────────────────────────────────────

pub fn check_unit(unit: &AstUnit) -> EhDiagnostics {
    let mut d = EhDiagnostics::new();
    for decl in &unit.decls {
        check_decl(decl, &mut d);
    }
    d
}

fn check_decl(decl: &AstDecl, d: &mut EhDiagnostics) {
    match &decl.data {
        AstDeclData::Function { body, .. } => {
            if let Some(b) = body {
                check_stmt_top(b, false, d);
            }
        }
        AstDeclData::Method { body, .. } => {
            if let Some(b) = body {
                check_stmt_top(b, false, d);
            }
        }
        AstDeclData::Class { methods, .. } => {
            for m in methods {
                check_decl(m, d);
            }
        }
        _ => {}
    }
}

/// `in_block`: inside a block literal — throws are rejected there in M1.
fn check_stmt_top(stmt: &AstStmt, in_block: bool, d: &mut EhDiagnostics) {
    match &stmt.data {
        AstStmtData::Throw(_) => {
            if in_block {
                d.err(format!(
                    "line {}: col {}: @throw inside a block literal is not supported by -eh checked yet",
                    stmt.line, stmt.col
                ));
            }
            // Whole-statement throw: fine. (Expression-position throws are
            // impossible by grammar: @throw is a statement.)
        }
        AstStmtData::Compound(inner) => {
            for s in inner {
                check_stmt_top(s, in_block, d);
            }
        }
        AstStmtData::If { then, else_, .. } => {
            check_stmt_top(then, in_block, d);
            if let Some(el) = else_ {
                check_stmt_top(el, in_block, d);
            }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. } => {
            check_stmt_top(body, in_block, d);
        }
        AstStmtData::For { body, .. } => {
            check_stmt_top(body, in_block, d);
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            check_stmt_top(try_block, in_block, d);
            for c in catches {
                if let AstStmtData::Catch { body, .. } = &c.data {
                    check_stmt_top(body, in_block, d);
                }
            }
            if let Some(f) = finally_block {
                check_stmt_top(f, in_block, d);
            }
        }
        AstStmtData::Synchronized { body, .. } | AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            check_stmt_top(body, in_block, d);
        }
        AstStmtData::Expr(e) => check_expr_blocks(e, d),
        AstStmtData::Decl(decl) => check_decl_stmt(decl, d),
        AstStmtData::Return(Some(e)) => check_expr_blocks(e, d),
        _ => {}
    }
}

fn check_expr_blocks(e: &AstExpr, d: &mut EhDiagnostics) {
    if let AstExprData::Block { body: Some(b), .. } = &e.data {
        // Block bodies compile into standalone C functions, and a `@throw`
        // inside one arms the flag exactly like any other arming call site:
        // the invoke returns normally, and the caller's guard tail (every
        // call-bearing statement is guarded) takes over. So throws are ALLOWED
        // here (differential case 06); validate the body like any statement
        // list. The one real boundary — the flag crossing a pure C frame —
        // does not apply: block invokes are jeti-generated functions whose
        // caller is guarded.
        check_stmt_top(b, false, d);
    }
    for child in expr_children(e) {
        check_expr_blocks(child, d);
    }
}

fn check_decl_stmt(decl: &AstDecl, d: &mut EhDiagnostics) {
    if let AstDeclData::Variable { init: Some(e), .. } = &decl.data {
        check_expr_blocks(e, d);
    }
    if let AstDeclData::Variable { next, .. } = &decl.data {
        if let Some(n) = next {
            check_decl_stmt(n, d);
        }
    }
}

fn expr_children(e: &AstExpr) -> Vec<&AstExpr> {
    match &e.data {
        AstExprData::Unary { operand, .. } | AstExprData::Cast { expr: operand, .. } => vec![operand],
        AstExprData::Binary { left, right, .. } | AstExprData::Assign { target: left, value: right, .. } => vec![left, right],
        AstExprData::Ternary { cond, then, else_ } => vec![cond, then, else_],
        AstExprData::MsgSend { receiver, args, .. } => {
            let mut v: Vec<&AstExpr> = vec![&**receiver];
            v.extend(args.iter());
            v
        }
        AstExprData::FuncCall { callee, args, .. } => {
            let mut v: Vec<&AstExpr> = args.iter().collect();
            if let Some(c) = callee { v.insert(0, &**c); }
            v
        }
        AstExprData::Await { inner, .. } => vec![inner],
        _ => Vec::new(),
    }
}

// ─── Desugar pass (pre-ARC) ──────────────────────────────────────────────────
//
// Rewrites (only in units compiled with -eh checked):
//
//   @throw e            ->  __jeti_eh_flag = 1;
//                           __jeti_eh_val = (NPObject *)(void *)(e);
//                           return <zero>;
//   <call sites>        ->  call; if (__jeti_eh_flag) { return <zero>; }
//   @try B C F          ->  B'  (guard-rewritten: statements after the first
//                            throwing call are wrapped in if (flag == 0))
//                           + catch arms as plain if (flag) blocks
//                           + finally at the natural merge point
//
// Everything is ordinary control flow, so ARC (which runs after this pass)
// inserts scope-end releases on ALL paths — the sjlj cross-function leak is
// impossible by construction.

use jeti_ast::{AstType};
use jeti_cst::TypePrim;

/// `NPObject *` type node (for the error value cast).
fn npobject_ptr_type() -> AstType {
    let mut t = AstType::new(TypePrim::Named);
    t.name = Some("NPObject".into());
    t.is_pointer = true;
    t
}

fn int_expr(n: i64) -> AstExpr {
    AstExpr {
        kind: jeti_ast::AstExprKind::Int, expr_type: None, line: 0, col: 0,
        data: AstExprData::Int(n),
    }
}

fn varref(name: &str) -> AstExpr {
    AstExpr {
        kind: jeti_ast::AstExprKind::VarRef, expr_type: None, line: 0, col: 0,
        data: AstExprData::VarRef { sym: None, name: name.into() },
    }
}

fn expr_stmt(e: AstExpr) -> AstStmt {
    AstStmt {
        kind: jeti_ast::AstStmtKind::Expr, line: 0, col: 0,
        data: AstStmtData::Expr(e),
    }
}

fn assign_stmt(target: &str, value: AstExpr) -> AstStmt {
    expr_stmt(AstExpr {
        kind: jeti_ast::AstExprKind::Assign, expr_type: None, line: 0, col: 0,
        data: AstExprData::Assign { target: Box::new(varref(target)), value: Box::new(value) },
    })
}

/// `return <zero-of-ret-type>;` — the throwing-early-return. `void` returns
/// bare; scalars, pointers and `id` return 0. Everything else (a named
/// non-pointer type, which may be an aggregate typedef — `NPRange` is
/// `typedef struct {...} NPRange;`) gets a zeroed compound literal `(T){0}`:
/// `return 0;` there is a clang error ("returning 'int' from a function with
/// incompatible result type"). Same shape codegen emits as a nil-messaging
/// fallback.
fn return_zero(ret: &AstType) -> AstStmt {
    let val = if ret.prim == TypePrim::Void && !ret.is_pointer {
        None
    } else if aggregate_zero(ret) {
        Some(Box::new(AstExpr {
            kind: jeti_ast::AstExprKind::Cast, expr_type: None, line: 0, col: 0,
            data: AstExprData::Cast {
                target_type: ret.clone(),
                expr: Box::new(AstExpr {
                    kind: jeti_ast::AstExprKind::InitList, expr_type: None, line: 0, col: 0,
                    data: AstExprData::InitList(vec![int_expr(0)]),
                }),
            },
        }))
    } else {
        Some(Box::new(int_expr(0)))
    };
    AstStmt {
        kind: jeti_ast::AstStmtKind::Return, line: 0, col: 0,
        data: AstStmtData::Return(val),
    }
}

/// True when the return type needs `(T){0}` rather than `0`.
///
/// A `Named` non-pointer type carries only a name, so there is no way to tell
/// an aggregate typedef (`NPRange`) from a scalar one (`size_t`) here — and
/// both take the compound literal: it is the only C99 form valid for the
/// former, and still perfectly valid for the latter. `Sel` joins them because
/// jeti's `SEL` is a struct, not an integer — and a bare `SEL` has NO name
/// (`TypePrim::Sel` with `name: None`), so it must be matched on the prim
/// alone or the tail guard emits `return 0;` into a struct-returning function
/// (json_editor's `static SEL cmdSel`, clang: "returning 'int' from a
/// function with incompatible result type 'SEL'"). Pointers, `id`, blocks,
/// function pointers, arrays and unsized/named-less types keep `0`.
fn aggregate_zero(t: &AstType) -> bool {
    !t.is_pointer
        && !t.is_block
        && !t.is_fn_ptr
        && !t.is_array
        && (matches!(t.prim, TypePrim::Sel)
            || (t.name.is_some() && matches!(t.prim, TypePrim::Named)))
}

/// Wrap `s` in `if (!__jeti_eh_flag) { s }`.
fn guard_stmt(s: AstStmt) -> AstStmt {
    // `if (__jeti_eh_flag == 0) { s }` — Binary op 12 is `==` (codegen
    // op_to_str table, verified in AGENTS.md).
    AstStmt {
        kind: jeti_ast::AstStmtKind::If, line: 0, col: 0,
        data: AstStmtData::If {
            cond: Box::new(AstExpr {
                kind: jeti_ast::AstExprKind::Binary, expr_type: None, line: 0, col: 0,
                data: AstExprData::Binary {
                    op: 12,
                    left: Box::new(varref("__jeti_eh_flag")),
                    right: Box::new(int_expr(0)),
                },
            }),
            then: Box::new(AstStmt {
                kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
                data: AstStmtData::Compound(vec![s]),
            }),
            else_: None,
        },
    }
}

/// `__jeti_eh_flag == <n>` — the query form used by every guard below.
fn flag_is(n: i64) -> AstExpr {
    AstExpr {
        kind: jeti_ast::AstExprKind::Binary, expr_type: None, line: 0, col: 0,
        data: AstExprData::Binary {
            op: 12, // ==
            left: Box::new(varref("__jeti_eh_flag")),
            right: Box::new(int_expr(n)),
        },
    }
}

/// `__jeti_eh_flag == 0 && <cond>` — loop conditions. A body that may arm the
/// flag must stop looping (ObjC leaves the loop at the throw), so the condition
/// is only consulted while nothing is in flight.
fn cond_and_flag_clear(cond: AstExpr) -> AstExpr {
    AstExpr {
        kind: jeti_ast::AstExprKind::Binary, expr_type: None, line: 0, col: 0,
        data: AstExprData::Binary {
            op: 17, // &&
            left: Box::new(flag_is(0)),
            right: Box::new(cond),
        },
    }
}

/// `<a> || <b>` (op 18) — used to hand a saved in-flight flag back on @try exit.
/// The codegen op table is `17 => "&&", 18 => "||"` (`codegen.rs:766`); op 17
/// here would AND away a freshly raised exception, so an inner @try could not
/// propagate to an enclosing @catch (differential cases 03/04/05).
fn logical_or(a: AstExpr, b: AstExpr) -> AstExpr {
    AstExpr {
        kind: jeti_ast::AstExprKind::Binary, expr_type: None, line: 0, col: 0,
        data: AstExprData::Binary {
            op: 18,
            left: Box::new(a),
            right: Box::new(b),
        },
    }
}

/// `name(args...)` — runtime calls injected by the desugar (`jeti_retain`,
/// `jeti_autorelease`, `__jeti_eh_*`).
fn func_call(name: &str, args: Vec<AstExpr>) -> AstExpr {
    AstExpr {
        kind: jeti_ast::AstExprKind::FuncCall, expr_type: None, line: 0, col: 0,
        data: AstExprData::FuncCall {
            func: None,
            name: name.into(),
            callee: None,
            args,
        },
    }
}

/// Monotonic counter for the per-`@try` saved-flag local and the per-arm
/// caught-object local (unique names, no clobbering across nesting).
fn next_seq() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// A plain `T name = init;` (or bare `T name;`) declaration statement.
fn var_decl(name: &str, ty: AstType, init: Option<AstExpr>, line: usize, col: usize) -> AstStmt {
    AstStmt {
        kind: jeti_ast::AstStmtKind::Decl,
        line, col,
        data: AstStmtData::Decl(AstDecl {
            kind: jeti_ast::AstDeclKind::Variable,
            name: Some(name.into()),
            line, col,
            data: AstDeclData::Variable {
                var_type: Some(Box::new(ty)),
                init: init.map(Box::new),
                is_static: false, is_extern: false, is_const: false,
                is_block_qual: false, is_weak: false,
                next: None,
            },
            attributes: Vec::new(),
        }),
    }
}

pub fn desugar_unit(unit: &mut AstUnit) {
    let fx = EffectTable::build(unit);
    for decl in &mut unit.decls {
        desugar_decl(decl, &fx);
    }
}

/// Legacy (sjlj) backend: make `@finally` run on the early-`return` path.
///
/// The checked backend splices a copy of the finally body in front of every
/// `return` while it rewrites `@try` (see `rewrite_try`). The sjlj backend
/// leaves `@try` to codegen, so the same splice has to happen as its own pass,
/// before ARC (so the ARC-injected cleanup lands after the finally copy, i.e.
/// the finally runs while the locals are still alive).
pub fn splice_finally_exits(unit: &mut AstUnit) {
    for decl in &mut unit.decls {
        splice_finally_decl(decl);
    }
}

fn splice_finally_decl(d: &mut AstDecl) {
    match &mut d.data {
        AstDeclData::Function { body: Some(b), .. } | AstDeclData::Method { body: Some(b), .. } => {
            splice_finally_stmt(b);
        }
        AstDeclData::Class { methods, .. } => {
            for m in methods {
                splice_finally_decl(m);
            }
        }
        AstDeclData::Namespace(members) => {
            for m in members {
                splice_finally_decl(m);
            }
        }
        _ => {}
    }
}

fn splice_finally_stmt(s: &mut AstStmt) {
    match &mut s.data {
        AstStmtData::Compound(v) => for st in v { splice_finally_stmt(st); },
        AstStmtData::If { then, else_, .. } => {
            splice_finally_stmt(then);
            if let Some(e) = else_ { splice_finally_stmt(e); }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
        | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. } => {
            splice_finally_stmt(body);
        }
        AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. }
        | AstStmtData::Default(body) => splice_finally_stmt(body),
        AstStmtData::Synchronized { body, .. } | AstStmtData::Autoreleasepool(body)
        | AstStmtData::NoArc(body) => splice_finally_stmt(body),
        AstStmtData::Catch { body, .. } => splice_finally_stmt(body),
        AstStmtData::Finally(b) => splice_finally_stmt(b),
        AstStmtData::Try { try_block, catches, finally_block } => {
            // Inner @try first, so an outer splice lands after the inner copy.
            splice_finally_stmt(try_block);
            for c in catches { splice_finally_stmt(c); }
            if let Some(f) = finally_block {
                splice_finally_stmt(f);
                let fin = (**f).clone();
                splice_finally_before_exits(try_block, &fin);
            }
        }
        _ => {}
    }
}

fn desugar_decl(decl: &mut AstDecl, fx: &EffectTable) {
    match &mut decl.data {
        AstDeclData::Function { func_sym, return_type, body, .. } => {
            if let Some(b) = body {
                let rt = return_type.as_ref().map(|t| t.as_ref().clone()).unwrap_or_else(|| AstType::new(TypePrim::Int));
                // main: an exception escaping it is UNCAUGHT — the tail guard
                // aborts with ObjC wording instead of silently returning zero
                // (differential case 07).
                let is_main = func_sym.as_deref() == Some("main");
                desugar_body(b, &rt, is_main, fx);
            }
        }
        AstDeclData::Method { return_type, body, .. } => {
            if let Some(b) = body {
                let rt = return_type.as_ref().map(|t| t.as_ref().clone()).unwrap_or_else(|| AstType::new(TypePrim::Id));
                desugar_body(b, &rt, false, fx);
            }
        }
        AstDeclData::Class { methods, .. } => {
            for m in methods {
                desugar_decl(m, fx);
            }
        }
        // Namespace members carry functions with bodies too — without this
        // arm their @throw sites were never desugared (defer crate hit the
        // identical silent-drop bug first).
        AstDeclData::Namespace(members) => {
            for m in members {
                desugar_decl(m, fx);
            }
        }
        _ => {}
    }
}

/// Rewrite a function body.
///
/// NOTE: there is deliberately NO `__jeti_eh_flag = 0;` entry clear. The flag is
/// a *pending exception* global, and the code that runs while an exception is in
/// flight is exactly the cleanup ARC injects (scope-end `jeti_release` →
/// `dealloc`, itself a jeti method body). Clearing the flag in every body
/// destroys the pending exception the moment any cleanup runs — the M1.5 bug
/// that made `@catch` never fire in 01/02 (case 01: `middle` releases `m`,
/// `Item_dealloc` clears the flag, `main`'s `@catch` never sees it).
///
/// What leaves the frame instead: a body that may arm the flag gets a tail
/// guard, so an uncaught exception returns `<zero>` to the caller (callers test
/// the flag right after the call). ARC's scope-end releases run *before* that
/// return — the cross-frame cleanup this backend exists for.
fn desugar_body(body: &mut AstStmt, ret: &AstType, is_main: bool, fx: &EffectTable) {
    if let AstStmtData::Compound(stmts) = &mut body.data {
        let zero_ret = return_zero(ret);
        rewrite_stmts(stmts, &zero_ret, fx);
        if stmts.iter().any(|st| fx.stmt_may_arm(st)) {
            if is_main {
                // Uncaught exception escaping main: abort with ObjC wording.
                // No return value of ours — the process is dying here.
                stmts.push(AstStmt {
                    kind: jeti_ast::AstStmtKind::If, line: 0, col: 0,
                    data: AstStmtData::If {
                        cond: Box::new(flag_is(1)),
                        then: Box::new(AstStmt {
                            kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
                            data: AstStmtData::Compound(vec![
                                expr_stmt(func_call("jeti_eh_uncaught", vec![])),
                            ]),
                        }),
                        else_: None,
                    },
                });
            } else {
                stmts.push(if_armed_ret(&zero_ret));
            }
        }
    }
}

/// `if (__jeti_eh_flag == 1) { return <zero>; }` — function-tail propagation.
fn if_armed_ret(zero_ret: &AstStmt) -> AstStmt {
    AstStmt {
        kind: jeti_ast::AstStmtKind::If, line: 0, col: 0,
        data: AstStmtData::If {
            cond: Box::new(flag_is(1)),
            then: Box::new(AstStmt {
                kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
                data: AstStmtData::Compound(vec![zero_ret.clone()]),
            }),
            else_: None,
        },
    }
}

/// Rewrite a statement list in place. `zero_ret` is cloned per throw site
/// (cheap AST nodes).
fn rewrite_stmts(stmts: &mut Vec<AstStmt>, zero_ret: &AstStmt, fx: &EffectTable) {
    let mut i = 0;
    while i < stmts.len() {
        // Sequence this statement's own call-bearing subexpressions into
        // preceding temporaries first (differential case 02): the spliced
        // declarations are ordinary arming statements, so the guard chosen
        // below blocks the rest of the *expression*, not just the rest of the
        // statement list. Re-entering at the same index lets the loop rewrite
        // them and build the nested guards.
        let pre = hoist_stmt_calls(&mut stmts[i]);
        if !pre.is_empty() {
            for (k, p) in pre.into_iter().enumerate() {
                stmts.insert(i + k, p);
            }
            continue;
        }

        rewrite_stmt(&mut stmts[i], zero_ret, fx);

        // If the rewritten statement can leave the flag armed (a call site
        // or a throw), the WHOLE remaining tail goes into ONE guard block so
        // control falls to the end of the enclosing compound when the flag is
        // set (ARC's scope-end releases must run — a bare early return would
        // skip them, reintroducing the sjlj leak inside loops/nesting).
        //
        // One block, not one guard per statement: per-statement guards would
        // pull each declaration into its own `if` scope, breaking later
        // references (`use of undeclared identifier`). A single block spans
        // exactly the rest of this compound, so lexical scoping is unchanged;
        // nested arming sites create nested guards via the recursion below.
        if !fx.stmt_may_arm(&stmts[i]) {
            i += 1;
            continue;
        }
        let tail: Vec<AstStmt> = stmts.drain(i + 1..).collect();
        if tail.is_empty() {
            return;
        }
        let mut tail_block = AstStmt {
            kind: jeti_ast::AstStmtKind::Compound,
            line: 0,
            col: 0,
            data: AstStmtData::Compound(tail),
        };
        if let AstStmtData::Compound(inner) = &mut tail_block.data {
            rewrite_stmts(inner, zero_ret, fx);
        }
        stmts.push(guard_stmt(tail_block));
        return;
    }
}

// ─── Exception effect analysis (stage-3 guard narrowing) ────────────────────
//
// Stage-3 rule set: only calls that can actually arm the error flag get
// guards. Sources of truth:
//   - `@throws(T)` annotation  → MayThrow (call sites guard after it)
//   - naked `@throws`          → AlwaysThrows (returns zero with the flag
//     armed — the guard is LIVE: it either propagates or falls to a catch arm)
//   - everything else          → NoThrow (the checker's `@throws` escape rule
//     forces every escaping throw to be declared, so an absent annotation is
//     a real promise; a body that still contains a bare `@throw` upgrades its
//     own entry to MayThrow for `-fno-checker` builds)
// C runtime primitives / libc are whitelisted. Free functions unknown to this
// TU default to MayThrow (foreign-TU helpers cannot be verified); unannotated
// methods default to NoThrow — methods are the volume path (Foundation sends)
// and the aggressive-but-documented initial cut (dynamic ⇒ MayThrow from the
// spec would re-flood every send).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Effect {
    NoThrow,
    MayThrow,
    AlwaysThrows,
}

struct EffectTable {
    /// Method effects keyed by selector (normalized: EVERY colon stripped —
    /// symbol-table convention; a trim_end_matches would silently miss
    /// multi-part selectors).
    methods: std::collections::HashMap<String, Effect>,
    /// Free-function effects keyed by name.
    functions: std::collections::HashMap<String, Effect>,
}

/// M1 encoding of the bare `@throws` form: `Some(Void)` non-pointer.
fn throw_effect(th: &AstType) -> Effect {
    if th.prim == TypePrim::Void && !th.is_pointer {
        Effect::AlwaysThrows
    } else {
        Effect::MayThrow
    }
}

/// C runtime primitives and libc functions that cannot run jeti code and
/// therefore cannot arm the error flag.
const NOTHROW_C_FUNCS: &[&str] = &[
    "jeti_retain", "jeti_release", "jeti_autorelease", "jeti_malloc", "jeti_free",
    "jeti_alloc", "jeti_init", "jeti_stringFromCstr", "jeti_string_from_cstr",
    "jeti_metaInit", "jeti_meta_init", "jeti_isKindOfClass", "jeti_isKindOf",
    "jeti_weakRegister", "jeti_weakUnregister", "jeti_weakClearAll",
    "jeti_syncLock", "jeti_syncUnlock", "jeti_eh_isa", "jeti_eh_uncaught",
    "jeti_task_create", "jeti_task_resume", "jeti_task_join",
    "printf", "fprintf", "sprintf", "snprintf", "puts", "putchar", "fputs",
    "fwrite", "strlen", "strcmp", "strncmp", "memcpy", "memset", "memmove",
    "abort", "exit", "calloc", "malloc", "free", "realloc",
    "kputs", "kputdec", "kputhex", "kputc", "kprintf",
    "NPLog", "__NPLogv",
    "jeti_array_create", "jeti_dictionary_create",
    "va_start", "va_arg", "va_end", "va_copy",
];

fn is_nothrow_c_func(name: &str) -> bool {
    NOTHROW_C_FUNCS.iter().any(|f| *f == name)
}

/// Symbol-table selector convention: strip EVERY colon (a `trim_end_matches`
/// would silently miss multi-part selectors — checker subscript-bug precedent).
fn normalize_selector(sel: &str) -> String {
    sel.chars().filter(|c| *c != ':').collect()
}

impl EffectTable {
    fn build(unit: &AstUnit) -> EffectTable {
        let mut t = EffectTable {
            methods: std::collections::HashMap::new(),
            functions: std::collections::HashMap::new(),
        };
        for decl in &unit.decls {
            t.scan_decl(decl);
        }
        t.propagate(unit);
        t
    }

    fn scan_decl(&mut self, decl: &AstDecl) {
        match &decl.data {
            AstDeclData::Function { func_sym, body, throws, .. } => {
                if let Some(name) = func_sym {
                    let base = throws.as_deref().map_or(Effect::NoThrow, throw_effect);
                    self.functions.insert(name.clone(), base);
                }
                // A body that still contains a bare `@throw` (unannotated —
                // checker error path, or `-fno-checker`) must not be trusted
                // as NoThrow.
                if throws.is_none() {
                    if let (Some(name), Some(b)) = (func_sym, body) {
                        if self.body_throws(b) {
                            self.functions.insert(name.clone(), Effect::MayThrow);
                        }
                    }
                }
            }
            AstDeclData::Method { body, throws, .. } => {
                if let Some(sel) = decl.name.as_deref() {
                    let key = normalize_selector(sel);
                    if let Some(th) = throws.as_deref() {
                        self.methods.insert(key, throw_effect(th));
                    } else if let Some(b) = body {
                        if self.body_throws(b) {
                            self.methods.insert(key, Effect::MayThrow);
                        }
                    }
                }
            }
            AstDeclData::Class { methods, .. } => {
                for m in methods {
                    self.scan_decl(m);
                }
            }
            AstDeclData::Namespace(members) => {
                for m in members {
                    self.scan_decl(m);
                }
            }
            _ => {}
        }
    }

    /// Does this statement subtree contain a `@throw` statement? (M1: throws
    /// only exist as whole statements — expressions need no walk.)
    fn body_throws(&self, s: &AstStmt) -> bool {
        match &s.data {
            AstStmtData::Throw(_) => true,
            AstStmtData::Compound(inner) => inner.iter().any(|x| self.body_throws(x)),
            AstStmtData::If { then, else_, .. } => {
                self.body_throws(then)
                    || else_.as_ref().map_or(false, |el| self.body_throws(el))
            }
            AstStmtData::While { body, .. } | AstStmtData::Do { body, .. } => self.body_throws(body),
            AstStmtData::For { init, body, .. } => {
                init.as_ref().map_or(false, |i| self.body_throws(i)) || self.body_throws(body)
            }
            AstStmtData::Try { try_block, catches, finally_block } => {
                self.body_throws(try_block)
                    || catches.iter().any(|c| match &c.data {
                        AstStmtData::Catch { body, .. } => self.body_throws(body),
                        _ => false,
                    })
                    || finally_block.as_ref().map_or(false, |f| self.body_throws(f))
            }
            AstStmtData::Switch { body, .. } => self.body_throws(body),
            AstStmtData::Case { body, .. } | AstStmtData::Default(body) => self.body_throws(body),
            AstStmtData::Synchronized { body, .. } => self.body_throws(body),
            AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => self.body_throws(body),
            _ => false,
        }
    }

    fn effect_of_call(&self, e: &AstExpr) -> Effect {
        match &e.data {
            AstExprData::MsgSend { selector, .. } => {
                *self.methods.get(&normalize_selector(selector)).unwrap_or(&Effect::NoThrow)
            }
            AstExprData::FuncCall { name, .. } => {
                if let Some(eff) = self.functions.get(name.as_str()) {
                    *eff
                } else if is_nothrow_c_func(name) {
                    Effect::NoThrow
                } else {
                    Effect::MayThrow
                }
            }
            // Assigning the error flag IS the arming mechanism: the desugared
            // `@throw` compound ends in `__jeti_eh_flag = 1;` and its helpers
            // (jeti_retain/jeti_autorelease) are NoThrow-whitelisted — without
            // this rule the rewritten throw would look harmless and lose the
            // tail guard that routes control to the catch arms / caller.
            AstExprData::Assign { target, .. } if matches!(&target.data,
                AstExprData::VarRef { name, .. } if name == "__jeti_eh_flag") => Effect::MayThrow,
            _ => Effect::NoThrow,
        }
    }

    /// Worst effect over an expression node and its subtree.
    fn node_effect(&self, e: &AstExpr) -> Effect {
        let mut worst = self.effect_of_call(e);
        if worst == Effect::AlwaysThrows {
            return worst;
        }
        for c in Self::effect_children(e) {
            match self.node_effect(c) {
                Effect::AlwaysThrows => return Effect::AlwaysThrows,
                Effect::MayThrow => worst = Effect::MayThrow,
                Effect::NoThrow => {}
            }
        }
        worst
    }

    /// Children relevant to arming analysis. Block literals are excluded: a
    /// call inside a block body throws in the block's own function at invoke
    /// time, not synchronously at this use site.
    fn effect_children(e: &AstExpr) -> Vec<&AstExpr> {
        match &e.data {
            AstExprData::Unary { operand, .. } | AstExprData::Cast { expr: operand, .. } => vec![operand],
            AstExprData::Binary { left, right, .. }
            | AstExprData::Assign { target: left, value: right } => vec![left, right],
            AstExprData::Ternary { cond, then, else_ } => vec![cond, then, else_],
            AstExprData::MsgSend { receiver, args, .. } => {
                let mut v: Vec<&AstExpr> = vec![&**receiver];
                v.extend(args.iter());
                v
            }
            AstExprData::FuncCall { callee, args, .. } => {
                let mut v: Vec<&AstExpr> = args.iter().collect();
                if let Some(c) = callee {
                    v.insert(0, &**c);
                }
                v
            }
            AstExprData::Subscript { object, key } => vec![object, key],
            AstExprData::Comma(exprs) => exprs.iter().collect(),
            AstExprData::InitList(items) | AstExprData::ArrayLit(items) => items.iter().collect(),
            AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => vec![obj],
            AstExprData::Await { inner, .. } => vec![inner],
            _ => Vec::new(),
        }
    }

    fn expr_may_arm(&self, e: &AstExpr) -> bool {
        self.node_effect(e) != Effect::NoThrow
    }

    fn decl_may_arm(&self, d: &AstDecl) -> bool {
        match &d.data {
            AstDeclData::Variable { init, next, .. } => {
                init.as_ref().map_or(false, |e| self.expr_may_arm(e))
                    || next.as_ref().map_or(false, |n| self.decl_may_arm(n))
            }
            _ => false,
        }
    }

    /// Does this (already rewritten) statement possibly leave the error flag
    /// set when control passes *beyond* it? Transitive: a nested statement
    /// that arms (e.g. `if (c) @throw e;`, or a loop whose body throws) must
    /// guard the tail of the enclosing statement list too.
    fn stmt_may_arm(&self, s: &AstStmt) -> bool {
        match &s.data {
            AstStmtData::Throw(_) => true,
            AstStmtData::Expr(e) => self.expr_may_arm(e),
            AstStmtData::Decl(d) => self.decl_may_arm(d),
            AstStmtData::Return(Some(e)) => self.expr_may_arm(e),
            AstStmtData::If { cond, then, else_ } => {
                self.expr_may_arm(cond) || self.stmt_may_arm(then)
                    || else_.as_ref().map_or(false, |el| self.stmt_may_arm(el))
            }
            AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
                self.expr_may_arm(cond) || self.stmt_may_arm(body)
            }
            AstStmtData::For { init, cond, incr, body } => {
                init.as_ref().map_or(false, |i| self.stmt_may_arm(i))
                    || cond.as_ref().map_or(false, |c| self.expr_may_arm(c))
                    || incr.as_ref().map_or(false, |c| self.expr_may_arm(c))
                    || self.stmt_may_arm(body)
            }
            AstStmtData::Compound(inner) => inner.iter().any(|x| self.stmt_may_arm(x)),
            AstStmtData::Try { try_block, catches, finally_block } => {
                self.stmt_may_arm(try_block)
                    || catches.iter().any(|c| match &c.data {
                        AstStmtData::Catch { body, .. } => self.stmt_may_arm(body),
                        _ => false,
                    })
                    || finally_block.as_ref().map_or(false, |f| self.stmt_may_arm(f))
            }
            AstStmtData::Switch { expr, body } => self.expr_may_arm(expr) || self.stmt_may_arm(body),
            AstStmtData::Case { value, body } => self.expr_may_arm(value) || self.stmt_may_arm(body),
            AstStmtData::Default(body) => self.stmt_may_arm(body),
            AstStmtData::Synchronized { lock, body } => self.expr_may_arm(lock) || self.stmt_may_arm(body),
            AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => self.stmt_may_arm(body),
            _ => false,
        }
    }

    /// Transitive closure: a function/method that calls a MayThrow callee can
    /// itself return with the flag still armed (its guard tail returns <zero>
    /// without clearing), so its callers must guard after it too. Iterate to
    /// fixpoint — `middle` calling throwing `boom` upgrades middle, which then
    /// upgrades main (differential case 01's cross-frame chain).
    fn propagate(&mut self, unit: &AstUnit) {
        loop {
            let mut upgrades: Vec<(bool, String)> = Vec::new();
            for decl in &unit.decls {
                self.collect_upgrade(decl, &mut upgrades);
            }
            if upgrades.is_empty() {
                break;
            }
            for (is_method, key) in upgrades {
                if is_method {
                    self.methods.insert(key, Effect::MayThrow);
                } else {
                    self.functions.insert(key, Effect::MayThrow);
                }
            }
        }
    }

    fn collect_upgrade(&self, decl: &AstDecl, out: &mut Vec<(bool, String)>) {
        match &decl.data {
            AstDeclData::Function { func_sym, body, .. } => {
                if let (Some(name), Some(b)) = (func_sym, body) {
                    if self.functions.get(name) == Some(&Effect::NoThrow) && self.stmt_may_arm(b) {
                        out.push((false, name.clone()));
                    }
                }
            }
            AstDeclData::Method { body, .. } => {
                if let (Some(sel), Some(b)) = (decl.name.as_deref(), body) {
                    let key = normalize_selector(sel);
                    if self.methods.get(&key) == Some(&Effect::NoThrow) && self.stmt_may_arm(b) {
                        out.push((true, key));
                    }
                }
            }
            AstDeclData::Class { methods, .. } => {
                for m in methods {
                    self.collect_upgrade(m, out);
                }
            }
            AstDeclData::Namespace(members) => {
                for m in members {
                    self.collect_upgrade(m, out);
                }
            }
            _ => {}
        }
    }
}

/// Selector families whose result the caller owns (`alloc`/`new`/`copy`/
/// `mutableCopy`-prefixed, plus `init`). Ownership in jeti is decided by method
/// *name* (`crates/ownership/src/ownership.rs`), so hoisting such a call into a
/// temporary would move its +1 away from the binding site ARC accounts for.
fn owns_result(selector: &str) -> bool {
    let head = selector.split(':').next().unwrap_or(selector);
    ["alloc", "new", "init", "copy", "mutableCopy"].iter().any(|p| head.starts_with(p))
}

/// Any call in the expression whose result the caller owns. Such an expression
/// is left exactly as written: hoisting a sibling would reorder it against the
/// owned call.
fn expr_has_owning_call(e: &AstExpr) -> bool {
    match &e.data {
        AstExprData::MsgSend { receiver, selector, args, .. } => {
            owns_result(selector)
                || expr_has_owning_call(receiver)
                || args.iter().any(expr_has_owning_call)
        }
        AstExprData::FuncCall { args, .. } => args.iter().any(expr_has_owning_call),
        AstExprData::Unary { operand, .. } | AstExprData::Cast { expr: operand, .. } => {
            expr_has_owning_call(operand)
        }
        AstExprData::Binary { left, right, .. }
        | AstExprData::Assign { target: left, value: right } => {
            expr_has_owning_call(left) || expr_has_owning_call(right)
        }
        AstExprData::Ternary { cond, then, else_ } => {
            expr_has_owning_call(cond) || expr_has_owning_call(then) || expr_has_owning_call(else_)
        }
        AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => expr_has_owning_call(obj),
        _ => false,
    }
}

/// `__auto_type` — GNU "infer the declared type"; the pass cannot know the type
/// of a hoisted call, and a bare `__auto_type` leaves the bitmap/AST untouched.
fn auto_type() -> AstType {
    let mut t = AstType::new(TypePrim::Named);
    t.name = Some("__auto_type".to_string());
    t
}

/// Split this statement's call-bearing subexpressions into preceding
/// `__auto_type` temporaries, in evaluation order, and return them for the
/// caller to splice *in front of* the statement.
///
/// Why: the checked backend interrupts at statement granularity, but ObjC stops
/// the moment a call throws — the rest of the same expression never runs.
/// Differential case 02 locks this in: `x = [T boom] + [T bar];` must not reach
/// `[T bar]` (clang prints no `bar executed`). After splicing, the temporaries
/// are ordinary arming statements, so the existing guard machinery blocks the
/// remainder of the expression for free.
///
/// Boundaries (deliberate):
///   - the statement's own top-level expression is not hoisted in statement
///     positions where a void result is legal (`[obj foo];`, `return [obj bar];`)
///   - C function calls are not hoisted (Foundation's runtime primitives would
///     gain a temporary each; only jeti message sends can `@throw` here)
///   - expressions containing an owned call (see `owns_result`) are untouched
fn hoist_stmt_calls(s: &mut AstStmt) -> Vec<AstStmt> {
    let mut pre: Vec<AstStmt> = Vec::new();
    let (line, col) = (s.line, s.col);
    match &mut s.data {
        AstStmtData::Expr(e) => hoist_expr(e, true, &mut pre, line, col),
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => {
            hoist_expr(e, true, &mut pre, line, col)
        }
        AstStmtData::Decl(d) => {
            if let AstDeclData::Variable { init: Some(e), .. } = &mut d.data {
                // `top = true`: nothing is *replaced* here — a fresh declaration
                // has no earlier value to preserve, and the spliced temporaries
                // are declarations themselves (`top` keeps them from being
                // hoisted again).
                hoist_expr(e, true, &mut pre, line, col);
            }
        }
        AstStmtData::If { cond, .. } => hoist_expr(cond, false, &mut pre, line, col),
        AstStmtData::Switch { expr, .. } => hoist_expr(expr, false, &mut pre, line, col),
        AstStmtData::Synchronized { lock, .. } => hoist_expr(lock, false, &mut pre, line, col),
        _ => {}
    }
    pre
}

/// Walk `e` in evaluation order, replacing every hoistable call by a fresh
/// temporary and pushing its declaration onto `pre`. `top` marks the node the
/// statement's own result comes from (kept in place, see `hoist_stmt_calls`).
fn hoist_expr(e: &mut AstExpr, top: bool, pre: &mut Vec<AstStmt>, line: usize, col: usize) {
    match &mut e.data {
        AstExprData::MsgSend { receiver, args, .. } => {
            hoist_expr(receiver, false, pre, line, col);
            for a in args.iter_mut() {
                hoist_expr(a, false, pre, line, col);
            }
        }
        AstExprData::FuncCall { args, .. } => {
            for a in args.iter_mut() {
                hoist_expr(a, false, pre, line, col);
            }
        }
        AstExprData::Binary { op, left, right, .. } => {
            hoist_expr(left, false, pre, line, col);
            // `&&` (17) / `||` (18) evaluate the right operand only when the
            // left one does not already decide the result. Hoisting a send out
            // of it would run the call unconditionally: side effects the source
            // never asks for, and — for a selector the receiver's class does not
            // implement — a NULL vtable slot dispatch. C semantics win over the
            // extra stop-at-throw precision inside the skipped operand.
            if *op != 17 && *op != 18 {
                hoist_expr(right, false, pre, line, col);
            }
        }
        AstExprData::Assign { value, .. } => hoist_expr(value, false, pre, line, col),
        AstExprData::Unary { operand, .. } | AstExprData::Cast { expr: operand, .. } => {
            hoist_expr(operand, false, pre, line, col)
        }
        AstExprData::Ternary { cond, .. } => {
            hoist_expr(cond, false, pre, line, col);
            // Only one branch runs, so both branches stay exactly as written.
            // A hoisted send would evaluate the branch that is not taken —
            // observable side effects, and for a selector the receiver's class
            // does not implement, a NULL vtable slot dispatch. The condition is
            // always evaluated, so it keeps the stop-at-throw hoist.
        }
        AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => {
            hoist_expr(obj, false, pre, line, col)
        }
        _ => {}
    }

    let hoistable = !top
        && match &e.data {
            AstExprData::MsgSend { selector, .. } => !owns_result(selector),
            _ => false,
        };
    if !hoistable || expr_has_owning_call(e) {
        return;
    }
    let tmp = format!("__jeti_eh_tmp_{}", next_seq());
    let value = std::mem::replace(e, varref(&tmp));
    pre.push(var_decl(&tmp, auto_type(), Some(value), line, col));
}

/// Structural rewrite of one statement.
fn rewrite_stmt(s: &mut AstStmt, zero_ret: &AstStmt, fx: &EffectTable) {
    match &mut s.data {
        AstStmtData::Throw(expr) => {
            // Ownership mirrors clang's ARC lowering of `@throw`:
            //   NPObject *__jeti_eh_thrown_N = (NPObject *)(<e>);  /* ARC owns this temp */
            //   __jeti_eh_val = __jeti_eh_thrown_N;
            //   jeti_retain(__jeti_eh_val);      /* the in-flight exception holds one ref */
            //   jeti_autorelease(__jeti_eh_val); /* clang's objc_retainAutorelease: the
            //                                       object dies when the enclosing
            //                                       @autoreleasepool drains */
            //   __jeti_eh_flag = 1;
            //
            // NO `return <zero>` here: control must reach the end of the
            // enclosing statement list, otherwise a `@catch`/`@finally` in the
            // SAME function would never run (the try body's guard block skips
            // the remaining statements). Outside any @try the function-tail
            // guard `if (flag) return <zero>` does the frame-crossing.
            let n = next_seq();
            let tmp = format!("__jeti_eh_thrown_{}", n);
            let val_expr: Box<AstExpr> = match expr.take() {
                Some(e) => e,
                None => Box::new(varref("nil")),
            };
            let cast = AstExpr {
                kind: jeti_ast::AstExprKind::Cast, expr_type: None, line: s.line, col: s.col,
                data: AstExprData::Cast { target_type: npobject_ptr_type(), expr: val_expr },
            };
            *s = AstStmt {
                kind: jeti_ast::AstStmtKind::Compound, line: s.line, col: s.col,
                data: AstStmtData::Compound(vec![
                    var_decl(&tmp, npobject_ptr_type(), Some(cast), s.line, s.col),
                    assign_stmt("__jeti_eh_val", varref(&tmp)),
                    expr_stmt(func_call("jeti_retain", vec![varref("__jeti_eh_val")])),
                    expr_stmt(func_call("jeti_autorelease", vec![varref("__jeti_eh_val")])),
                    assign_stmt("__jeti_eh_flag", int_expr(1)),
                ]),
            };
        }
        AstStmtData::Expr(e) => rewrite_expr_stmt(e),
        AstStmtData::Decl(d) => rewrite_decl(d, fx),
        AstStmtData::Return(Some(e)) => rewrite_expr(e, fx),
        AstStmtData::If { cond, then, else_ } => {
            rewrite_expr(cond, fx);
            rewrite_stmt(then, zero_ret, fx);
            if let Some(el) = else_ {
                rewrite_stmt(el, zero_ret, fx);
            }
        }
        AstStmtData::While { cond, body } => {
            // Re-testing the flag before every re-entry is only necessary when
            // something inside the loop can ARM it (a may-throw call, `@throw`,
            // a guarded call chain, ...). For a pure loop (`while (i < 5) i++;`)
            // the guard is dead weight on every iteration, so consult the effect
            // table — the same test the `for (;;)` branch below already used.
            let arms = fx.expr_may_arm(cond) || fx.stmt_may_arm(body);
            rewrite_expr(cond, fx);
            rewrite_stmt(body, zero_ret, fx);
            if arms {
                **cond = cond_and_flag_clear((**cond).clone());
            }
        }
        AstStmtData::Do { body, cond } => {
            let arms = fx.expr_may_arm(cond) || fx.stmt_may_arm(body);
            rewrite_stmt(body, zero_ret, fx);
            rewrite_expr(cond, fx);
            if arms {
                **cond = cond_and_flag_clear((**cond).clone());
            }
        }
        AstStmtData::For { init, cond, incr, body } => {
            // Every clause can arm the flag, so the guard is needed if ANY of
            // them may — including init (it runs before the first condition
            // evaluation) and incr (it runs right before the next one).
            let init_arms = init.as_ref().map_or(false, |i| fx.stmt_may_arm(i));
            let cond_arms = cond.as_ref().map_or(false, |c| fx.expr_may_arm(c));
            let incr_arms = incr.as_ref().map_or(false, |c| fx.expr_may_arm(c));
            let arms = init_arms || cond_arms || incr_arms || fx.stmt_may_arm(body);
            if let Some(init_s) = init.as_mut() {
                rewrite_stmt(init_s, zero_ret, fx);
            }
            if let Some(c) = cond.as_mut() {
                rewrite_expr(c, fx);
                if arms {
                    **c = cond_and_flag_clear((**c).clone());
                }
            } else if arms {
                // `for (;;)` with a throwing body: with no condition at all the
                // loop would spin forever once the flag is armed.
                *cond = Some(Box::new(flag_is(0)));
            }
            if let Some(inc) = incr.as_mut() { rewrite_expr(inc, fx); }
            rewrite_stmt(body, zero_ret, fx);
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            let tb = try_block.as_mut();
            let cs = catches;
            let fb: Option<&mut AstStmt> = finally_block.as_deref_mut();
            let line = s.line; let col = s.col;
            *s = rewrite_try(tb, cs, fb, zero_ret, fx, line, col);
        }
        AstStmtData::Compound(inner) => rewrite_stmts(inner, zero_ret, fx),
        AstStmtData::Synchronized { body, .. } | AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            rewrite_stmt(body, zero_ret, fx);
        }
        AstStmtData::Switch { expr, body } => {
            rewrite_expr(expr, fx);
            rewrite_stmt(body, zero_ret, fx);
        }
        AstStmtData::Case { value, body } => {
            rewrite_expr(value, fx);
            rewrite_stmt(body, zero_ret, fx);
        }
        AstStmtData::Default(body) => rewrite_stmt(body, zero_ret, fx),
        // Try/Catch/Finally are handled by rewrite_try below; other leaf
        // statements (Break, Continue, Goto, Label, Asm, bare Return) have
        // no call sites.
        _ => {}
    }
}

/// Rewrite `@try B C* F?` into guard/catch form.
///
///   {
///     <B rewritten: throwing call sites arm the flag; following statements
///      are guarded so control falls through to the end of the block>
///     <catch arms in order>
///     <finally>
///   }
///
/// The flag is provably clear on try entry (propagation returns immediately;
/// a catch takes the flag), so the try body needs no entry check.
/// Wrap a catch parameter declaration: `T name = (T)__jeti_eh_val;` or bare.
fn catch_param_decl(param_type: &str, param_name: &str, init: Option<AstExpr>, line: usize, col: usize) -> AstStmt {
    let vt = type_from_c_spelling(param_type);
    AstStmt {
        kind: jeti_ast::AstStmtKind::Decl,
        line, col,
        data: AstStmtData::Decl(AstDecl {
            kind: jeti_ast::AstDeclKind::Variable,
            name: Some(param_name.into()),
            line, col,
            data: AstDeclData::Variable {
                var_type: Some(Box::new(vt)),
                init: init.map(Box::new),
                is_static: false, is_extern: false, is_const: false,
                is_block_qual: false, is_weak: false,
                next: None,
            },
            attributes: Vec::new(),
        }),
    }
}

/// Parse a minimal C type spelling back into an AstType (catch params are
/// `id`, `Class *`-style named pointers, or scalars).
fn type_from_c_spelling(s: &str) -> AstType {
    let s = s.trim();
    if s == "id" {
        return AstType::new(TypePrim::Id);
    }
    let mut t = AstType::new(TypePrim::Named);
    if let Some(stripped) = s.strip_suffix(" *") {
        t.is_pointer = true;
        t.name = Some(stripped.trim().to_string());
    } else {
        t.name = Some(s.to_string());
    }
    t
}

fn rewrite_try(
    try_block: &mut AstStmt,
    catches: &mut Vec<AstStmt>,
    finally_block: Option<&mut AstStmt>,
    zero_ret: &AstStmt,
    fx: &EffectTable,
    line: usize, col: usize,
) -> AstStmt {
    //   {
    //       int __jeti_eh_saved_N = __jeti_eh_flag;   /* hand in-flight state back on exit */
    //       __jeti_eh_flag = 0;
    //       [int __jeti_eh_done_N = 0;]               /* only when there are catches */
    //       <try body: throwing call sites arm the flag, the tail is guarded>
    //       if (__jeti_eh_flag == 1 && __jeti_eh_done_N == 0) {
    //           if (__jeti_eh_isa((NPObject *)__jeti_eh_val, &JETI_CLASS_$_T)) {  /* typed only */
    //               __jeti_eh_done_N = 1;
    //               __jeti_eh_flag = 0;
    //               T *e = (T *)__jeti_eh_val;
    //               <body>
    //           }
    //       }
    //       <finally>
    //       __jeti_eh_flag = __jeti_eh_saved_N || __jeti_eh_flag;
    //   }
    //
    // Entry save/clear: the body must not see an exception that was already in
    // flight when the @try was entered (e.g. a @try inside a @finally).
    // Exit restore: an unhandled flag stays visible to the enclosing guard tail
    // and therefore propagates to the caller.
    //
    // `__jeti_eh_done_N` is the objc_end_catch latch — once an arm has handled
    // the exception its siblings must not, so an exception thrown *by* an arm
    // body (rethrow, case 04) leaves the @try instead of re-entering a sibling.
    let seq = next_seq();
    let saved = format!("__jeti_eh_saved_{}", seq);
    let done = format!("__jeti_eh_done_{}", seq);

    let mut out: Vec<AstStmt> = Vec::new();
    out.push(var_decl(&saved, AstType::new(TypePrim::Int), Some(varref("__jeti_eh_flag")), line, col));
    out.push(assign_stmt("__jeti_eh_flag", int_expr(0)));
    if !catches.is_empty() {
        out.push(var_decl(&done, AstType::new(TypePrim::Int), Some(int_expr(0)), line, col));
    }

    // Try body: rewrite; also guard trailing statements after any flag-arming
    // statement (rewrite_stmts handles statement lists; if the body is a
    // single non-compound statement there is no tail to guard).
    // Remember where it landed: the finally pass below splices into it.
    let try_body_idx = out.len();
    if let AstStmtData::Compound(inner) = &mut try_block.data {
        rewrite_stmts(inner, zero_ret, fx);
        out.push(AstStmt {
            kind: jeti_ast::AstStmtKind::Compound, line: try_block.line, col: try_block.col,
            data: AstStmtData::Compound(std::mem::take(inner)),
        });
    } else {
        rewrite_stmt(try_block, zero_ret, fx);
        out.push(try_block.clone());
    }

    // Catch arms. Each becomes:
    //   if (__jeti_eh_flag == 1 && __jeti_eh_done_N == 0) {
    //       [if (__jeti_eh_isa((NPObject *)val, &class)) {]   /* typed catch only */
    //       __jeti_eh_done_N = 1;
    //       __jeti_eh_flag = 0;
    //       <param decl if used>
    //       <body rewritten>
    //       [}]
    //   }
    // A failed isa test leaves flag and `done` untouched, so the next arm — or,
    // when nothing matches, the enclosing guard tail — still sees the
    // exception.
    for c in catches.iter_mut() {
        if let AstStmtData::Catch { param, body } = &mut c.data {
            let param_type = param.par_type.as_ref()
                .map(|pt| cst_type_to_c_str(pt))
                .unwrap_or_else(|| "id".into());
            let param_name = param.name.clone().unwrap_or_else(|| "exc".into());
            let is_id_catch = param_type == "id" || param_type.contains("NPObject");

            let mut arm: Vec<AstStmt> = vec![
                assign_stmt(&done, int_expr(1)),
                assign_stmt("__jeti_eh_flag", int_expr(0)),
            ];
            // Declare the catch param only if the body uses it (dead-store
            // precedent from codegen's sjlj path).
            let param_used = stmt_refs_name(body, &param_name);
            if param_used {
                let cast = AstExpr {
                    kind: jeti_ast::AstExprKind::Cast, expr_type: None, line, col,
                    data: AstExprData::Cast { target_type: type_from_c_spelling(&param_type), expr: Box::new(varref("__jeti_eh_val")) },
                };
                arm.push(catch_param_decl(&param_type, &param_name, Some(cast), line, col));
            } else {
                arm.push(catch_param_decl(&param_type, &param_name, None, line, col));
            }
            rewrite_stmt(body, zero_ret, fx);
            arm.push((**body).clone());

            let mut handled = AstStmt {
                kind: jeti_ast::AstStmtKind::Compound, line, col,
                data: AstStmtData::Compound(arm),
            };
            if !is_id_catch {
                handled = AstStmt {
                    kind: jeti_ast::AstStmtKind::If, line, col,
                    data: AstStmtData::If {
                        cond: Box::new(isa_test_expr(&param_type, line, col)),
                        then: Box::new(handled),
                        else_: None,
                    },
                };
            }

            out.push(AstStmt {
                kind: jeti_ast::AstStmtKind::If, line, col,
                data: AstStmtData::If {
                    cond: Box::new(AstExpr {
                        kind: jeti_ast::AstExprKind::Binary, expr_type: None, line, col,
                        data: AstExprData::Binary {
                            op: 17, // &&
                            left: Box::new(AstExpr {
                                kind: jeti_ast::AstExprKind::Binary, expr_type: None, line, col,
                                data: AstExprData::Binary {
                                    op: 12, // ==
                                    left: Box::new(varref("__jeti_eh_flag")),
                                    right: Box::new(int_expr(1)),
                                },
                            }),
                            right: Box::new(AstExpr {
                                kind: jeti_ast::AstExprKind::Binary, expr_type: None, line, col,
                                data: AstExprData::Binary {
                                    op: 12, // ==
                                    left: Box::new(varref(&done)),
                                    right: Box::new(int_expr(0)),
                                },
                            }),
                        },
                    }),
                    then: Box::new(handled),
                    else_: None,
                },
            });
        }
    }

    // Finally: runs with the flag isolated from an exception already in flight
    // (ObjC completes the @finally and only then resumes unwinding), so the
    // body's own guards do not skip its remaining statements. A flag raised
    // *inside* the finally is OR-ed back, so that exception propagates too.
    if let Some(f) = finally_block {
        rewrite_stmt(f, zero_ret, fx);
        // A `@finally` must also run when the try body leaves through `return`
        // (ObjC semantics). The body is emitted as plain statements above, so a
        // `return` inside it would jump straight past the finally block. Splice
        // a copy of the finally body immediately before every `return` in the
        // try body. Nesting works out because an outer @try splices into the
        // already-spliced inner sequence: its copy lands directly in front of
        // the `return`, i.e. AFTER the inner copy — which is the required order
        // (inner finally first, then outer finally).
        if let Some(tb) = out.get_mut(try_body_idx) {
            splice_finally_before_exits(tb, f);
        }
        out.push(isolated_block(f.clone(), line, col));
    }

    // Hand the in-flight state back (an unhandled flag must keep propagating).
    out.push(assign_stmt("__jeti_eh_flag", logical_or(varref(&saved), varref("__jeti_eh_flag"))));

    AstStmt {
        kind: jeti_ast::AstStmtKind::Compound, line, col,
        data: AstStmtData::Compound(out),
    }
}

/// Insert a copy of `fin` immediately before every `return` in `s`.
///
/// Used to give `@finally` its ObjC meaning on the early-return path: the
/// try body is a plain statement list by the time this runs, so without the
/// splice a `return` inside the try would skip the finally entirely. The copy
/// is placed directly in front of the `return`, so when nested @try blocks
/// splice in turn the innermost finally stays closest to the `return` and the
/// order is inner → outer, which is what ObjC specifies.
fn splice_finally_before_exits(s: &mut AstStmt, fin: &AstStmt) {
    match &mut s.data {
        AstStmtData::Compound(v) => {
            let mut rewritten: Vec<AstStmt> = Vec::with_capacity(v.len() + 1);
            for mut st in std::mem::take(v) {
                if matches!(st.data, AstStmtData::Return(_)) {
                    rewritten.push(fin.clone());
                    rewritten.push(st);
                } else {
                    splice_finally_before_exits(&mut st, fin);
                    rewritten.push(st);
                }
            }
            *v = rewritten;
        }
        AstStmtData::If { then, else_, .. } => {
            splice_finally_before_exits(then, fin);
            if let Some(e) = else_ { splice_finally_before_exits(e, fin); }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
        | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. } => {
            splice_finally_before_exits(body, fin);
        }
        AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. }
        | AstStmtData::Default(body) => splice_finally_before_exits(body, fin),
        AstStmtData::Synchronized { body, .. } | AstStmtData::Autoreleasepool(body)
        | AstStmtData::NoArc(body) => splice_finally_before_exits(body, fin),
        AstStmtData::Try { try_block, catches, finally_block } => {
            splice_finally_before_exits(try_block, fin);
            for c in catches { splice_finally_before_exits(c, fin); }
            if let Some(f) = finally_block { splice_finally_before_exits(f, fin); }
        }
        AstStmtData::Catch { body, .. } => splice_finally_before_exits(body, fin),
        AstStmtData::Finally(b) => splice_finally_before_exits(b, fin),
        _ => {}
    }
}

/// Run a statement with the exception flag isolated from an exception that is
/// already in flight, handing it back afterwards:
///   { int __jeti_eh_saved_N = __jeti_eh_flag; __jeti_eh_flag = 0; <s>;
///     __jeti_eh_flag = __jeti_eh_saved_N || __jeti_eh_flag; }
/// Used for @finally bodies; a @try body does the same inline in `rewrite_try`
/// (there the saved value has to stay live until the arms have run).
fn isolated_block(s: AstStmt, line: usize, col: usize) -> AstStmt {
    let saved = format!("__jeti_eh_saved_{}", next_seq());
    AstStmt {
        kind: jeti_ast::AstStmtKind::Compound, line, col,
        data: AstStmtData::Compound(vec![
            var_decl(&saved, AstType::new(TypePrim::Int), Some(varref("__jeti_eh_flag")), line, col),
            assign_stmt("__jeti_eh_flag", int_expr(0)),
            s,
            assign_stmt("__jeti_eh_flag", logical_or(varref(&saved), varref("__jeti_eh_flag"))),
        ]),
    }
}

/// isa test for a typed catch, used as the arm's condition:
///   __jeti_eh_isa((NPObject *)__jeti_eh_val, &JETI_CLASS_$_<Flat>)
/// 1 = match, 0 = mismatch (runtime.c). A mismatch is a plain false condition:
/// flag and the arm's `done` latch stay untouched, so the next arm — or the
/// enclosing guard tail — keeps seeing the exception.
fn isa_test_expr(param_type: &str, line: usize, col: usize) -> AstExpr {
    let flat = param_type.trim_end_matches(" *").replace("::", "__");
    func_call("__jeti_eh_isa", vec![
        AstExpr {
            kind: jeti_ast::AstExprKind::Cast, expr_type: None, line, col,
            data: AstExprData::Cast {
                target_type: npobject_ptr_type(),
                expr: Box::new(varref("__jeti_eh_val")),
            },
        },
        // &JETI_CLASS_$_<Flat> — codegen emits the weak class metadata
        // definition (and extern decl) in every TU, so this resolves at link
        // time even for header-only imported classes.
        AstExpr {
            kind: jeti_ast::AstExprKind::VarRef, expr_type: None, line, col,
            data: AstExprData::VarRef { sym: None, name: format!("&JETI_CLASS_$_{}", flat) },
        },
    ])
}

/// Rewrite an expression-statement: nothing to do unless we later split
/// multi-call expressions (M2). Call sites after the first arming statement
/// are covered by guard tails.
fn rewrite_expr_stmt(_e: &mut AstExpr) {}

/// Recurse into an expression: block literal bodies get the same statement-list
/// rewrite as function bodies (differential case 06 — a `@throw` inside a
/// block arms the flag; the invoke returns normally and the caller's guard
/// tail interrupts). No function-tail `return zero` is added inside the block:
/// the flag must stay armed across the invoke boundary, and the block's own
/// return type is not ours to return from.
fn rewrite_expr(e: &mut AstExpr, fx: &EffectTable) {
    if let AstExprData::Block { body: Some(b), .. } = &mut e.data {
        if let AstStmtData::Compound(stmts) = &mut b.data {
            let zero = return_zero(&AstType::new(TypePrim::Id));
            rewrite_stmts(stmts, &zero, fx);
        }
    }
}

/// Variable declarations with initializers: recurse so a block literal in the
/// init gets the same body rewrite as anywhere else (differential case 06 —
/// `ThrowerBlock b = ^{ @throw ... };` enters through a declaration).
fn rewrite_decl(d: &mut AstDecl, fx: &EffectTable) {
    if let AstDeclData::Variable { init: Some(e), next, .. } = &mut d.data {
        rewrite_expr(e, fx);
        if let Some(n) = next {
            rewrite_decl(n, fx);
        }
    }
}

/// Minimal C spelling of a CstType (catch param types only — pointer-to-
/// named, id, or scalar).
fn cst_type_to_c_str(t: &jeti_cst::CstType) -> String {
    use jeti_cst::TagKind as TK;
    let mut s = String::new();
    if t.is_const { s.push_str("const "); }
    match t.prim {
        TypePrim::Id => s.push_str("id"),
        TypePrim::Named | TypePrim::Param => {
            match t.tag {
                TK::Struct => s.push_str("struct "),
                TK::Union => s.push_str("union "),
                TK::Enum => s.push_str("enum "),
                TK::None => {}
            }
            s.push_str(t.name.as_deref().unwrap_or("id"));
        }
        TypePrim::Void => s.push_str("void"),
        TypePrim::Char => s.push_str("char"),
        TypePrim::Short => s.push_str("short"),
        TypePrim::Int => s.push_str("int"),
        TypePrim::Long => s.push_str("long"),
        TypePrim::LongLong => s.push_str("long long"),
        TypePrim::Float => s.push_str("float"),
        TypePrim::Double => s.push_str("double"),
        TypePrim::Bool => s.push_str("BOOL"),
        TypePrim::Signed => s.push_str("signed"),
        TypePrim::Unsigned => s.push_str("unsigned"),
        TypePrim::Class => s.push_str("Class"),
        TypePrim::Sel => s.push_str("SEL"),
        TypePrim::Instancetype => s.push_str("instancetype"),
    }
    if t.is_pointer {
        s.push_str(" *");
    }
    s
}

/// Does this statement subtree reference `name` as a VarRef? (dead-store
/// guard precedent: codegen's sjlj catch emission)
fn stmt_refs_name(s: &AstStmt, name: &str) -> bool {
    match &s.data {
        AstStmtData::Expr(e) => expr_refs_name(e, name),
        AstStmtData::Compound(inner) => inner.iter().any(|x| stmt_refs_name(x, name)),
        AstStmtData::If { cond, then, else_ } => {
            expr_refs_name(cond, name) || stmt_refs_name(then, name)
                || else_.as_ref().map_or(false, |el| stmt_refs_name(el, name))
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            expr_refs_name(cond, name) || stmt_refs_name(body, name)
        }
        AstStmtData::For { init, cond, incr, body } => {
            init.as_ref().map_or(false, |x| stmt_refs_name(x, name))
                || cond.as_ref().map_or(false, |x| expr_refs_name(x, name))
                || incr.as_ref().map_or(false, |x| expr_refs_name(x, name))
                || stmt_refs_name(body, name)
        }
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => expr_refs_name(e, name),
        AstStmtData::Decl(d) => decl_refs_name(d, name),
        AstStmtData::Try { try_block, catches, finally_block } => {
            stmt_refs_name(try_block, name)
                || catches.iter().any(|c| match &c.data {
                    AstStmtData::Catch { body, .. } => stmt_refs_name(body, name),
                    _ => false,
                })
                || finally_block.as_ref().map_or(false, |f| stmt_refs_name(f, name))
        }
        AstStmtData::Synchronized { lock, body } | AstStmtData::Switch { expr: lock, body } => {
            expr_refs_name(lock, name) || stmt_refs_name(body, name)
        }
        AstStmtData::Case { value, body } => expr_refs_name(value, name) || stmt_refs_name(body, name),
        AstStmtData::Default(body) => stmt_refs_name(body, name),
        AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => stmt_refs_name(body, name),
        _ => false,
    }
}

fn expr_refs_name(e: &AstExpr, name: &str) -> bool {
    match &e.data {
        AstExprData::VarRef { name: n, .. } => n == name,
        AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => expr_refs_name(obj, name),
        AstExprData::MsgSend { receiver, args, .. } => {
            expr_refs_name(receiver, name) || args.iter().any(|a| expr_refs_name(a, name))
        }
        AstExprData::FuncCall { callee, args, .. } => {
            callee.as_ref().map_or(false, |c| expr_refs_name(c, name))
                || args.iter().any(|a| expr_refs_name(a, name))
        }
        AstExprData::Unary { operand, .. } | AstExprData::Cast { expr: operand, .. } => expr_refs_name(operand, name),
        AstExprData::Binary { left, right, .. } | AstExprData::Assign { target: left, value: right } => {
            expr_refs_name(left, name) || expr_refs_name(right, name)
        }
        AstExprData::Ternary { cond, then, else_ } => {
            expr_refs_name(cond, name) || expr_refs_name(then, name) || expr_refs_name(else_, name)
        }
        AstExprData::Subscript { object, key } => expr_refs_name(object, name) || expr_refs_name(key, name),
        AstExprData::Comma(exprs) => exprs.iter().any(|x| expr_refs_name(x, name)),
        AstExprData::InitList(items) | AstExprData::ArrayLit(items) => items.iter().any(|x| expr_refs_name(x, name)),
        AstExprData::Await { inner, .. } => expr_refs_name(inner, name),
        _ => false,
    }
}

fn decl_refs_name(d: &AstDecl, name: &str) -> bool {
    match &d.data {
        AstDeclData::Variable { init, next, .. } => {
            init.as_ref().map_or(false, |e| expr_refs_name(e, name))
                || next.as_ref().map_or(false, |n| decl_refs_name(n, name))
        }
        _ => false,
    }
}

// ─── Regression tests (referendum #3 prerequisites) ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use jeti_ast::AstType;
    use jeti_cst::{CstParam, TypePrim};

    /// ③ A bare `SEL` (`TypePrim::Sel` with no name) is a *struct* in jeti's
    /// runtime, so its tail guard must emit `(SEL){0}`, never `0`. Regression
    /// for json_editor's `static SEL cmdSel` — clang rejected the emitted
    /// `return 0;` ("returning 'int' from a function with incompatible result
    /// type 'SEL'").
    #[test]
    fn bare_sel_takes_compound_literal_zero() {
        assert!(
            aggregate_zero(&AstType::new(TypePrim::Sel)),
            "bare SEL must take (SEL){{0}} — its prim alone is decisive, no name required"
        );
    }

    #[test]
    fn named_types_take_compound_literal_but_scalars_do_not() {
        let mut sel_named = AstType::new(TypePrim::Sel);
        sel_named.name = Some("SEL".into());
        assert!(aggregate_zero(&sel_named), "named SEL still takes (T){{0}}");

        let mut range = AstType::new(TypePrim::Named);
        range.name = Some("NPRange".into());
        assert!(aggregate_zero(&range), "named non-pointer (typedef) takes (T){{0}}");

        assert!(!aggregate_zero(&AstType::new(TypePrim::Int)), "int keeps return 0;");
        assert!(!aggregate_zero(&AstType::new(TypePrim::Void)), "void keeps plain form");

        let mut p = AstType::new(TypePrim::Sel);
        p.is_pointer = true;
        assert!(!aggregate_zero(&p), "pointer to SEL keeps 0");
    }

    /// ⑤ The checked desugar must produce ordinary control flow — no `@try`
    /// node may survive, because setjmp/longjmp is emitted *only* by the
    /// codegen `Try` arm (the sjlj backend). If a `Try` survived here, the
    /// checked path would silently depend on the hosted-only setjmp ABI.
    #[test]
    fn checked_desugar_leaves_no_try_node() {
        let throw = AstStmt {
            kind: jeti_ast::AstStmtKind::Throw, line: 0, col: 0,
            data: AstStmtData::Throw(Some(Box::new(int_expr(1)))),
        };
        let try_block = AstStmt {
            kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
            data: AstStmtData::Compound(vec![throw]),
        };
        let catch = AstStmt {
            kind: jeti_ast::AstStmtKind::Catch, line: 0, col: 0,
            data: AstStmtData::Catch {
                param: CstParam {
                    par_type: None, name: Some("e".into()), external_name: None,
                    next: None, attributes: Vec::new(),
                },
                body: Box::new(AstStmt {
                    kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
                    data: AstStmtData::Compound(vec![]),
                }),
            },
        };
        let finally = AstStmt {
            kind: jeti_ast::AstStmtKind::Finally, line: 0, col: 0,
            data: AstStmtData::Finally(Box::new(AstStmt {
                kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
                data: AstStmtData::Compound(vec![]),
            })),
        };
        let try_stmt = AstStmt {
            kind: jeti_ast::AstStmtKind::Try, line: 0, col: 0,
            data: AstStmtData::Try {
                try_block: Box::new(try_block),
                catches: vec![catch],
                finally_block: Some(Box::new(finally)),
            },
        };
        let body = AstStmt {
            kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
            data: AstStmtData::Compound(vec![try_stmt]),
        };
        let func = AstDecl {
            kind: jeti_ast::AstDeclKind::Function, name: Some("probe".into()), line: 1, col: 1,
            data: AstDeclData::Function {
                func_sym: None, return_type: Some(Box::new(AstType::new(TypePrim::Int))),
                params: None, body: Some(Box::new(body)),
                has_variadic: false, throws: None, async_marker: false,
            },
            attributes: Vec::new(),
        };
        let mut unit = AstUnit { decls: vec![func], filename: "probe.jeti".into() };
        desugar_unit(&mut unit);

        assert!(
            !unit_has_try(&unit),
            "checked desugar must rewrite every @try away (else codegen's setjmp arm runs)"
        );
        assert!(unit_uses_eh_flag(&unit), "checked desugar must arm the __jeti_eh_flag protocol");
    }

    fn unit_has_try(unit: &AstUnit) -> bool {
        fn stmt_has_try(s: &AstStmt) -> bool {
            match &s.data {
                AstStmtData::Try { .. } => true,
                AstStmtData::Compound(v) => v.iter().any(stmt_has_try),
                AstStmtData::If { then, else_, .. } => {
                    stmt_has_try(then) || else_.as_ref().map_or(false, |e| stmt_has_try(e))
                }
                _ => false,
            }
        }
        unit.decls.iter().any(|d| match &d.data {
            AstDeclData::Function { body, .. } | AstDeclData::Method { body, .. } => {
                body.as_ref().map_or(false, |b| stmt_has_try(b))
            }
            _ => false,
        })
    }

    fn unit_uses_eh_flag(unit: &AstUnit) -> bool {
        // Recursive: the guard condition desugar emits is
        // `__jeti_eh_flag == 1 && __jeti_eh_done_N == 0` — a Binary, not a bare
        // VarRef. A top-level-only match silently read every real guard as
        // "no flag" (this test's own bug, not the product's).
        fn expr_uses(e: &AstExpr) -> bool {
            if matches!(&e.data, AstExprData::VarRef { name, .. } if name == "__jeti_eh_flag") {
                return true;
            }
            EffectTable::effect_children(e).into_iter().any(expr_uses)
        }
        fn stmt_uses(s: &AstStmt) -> bool {
            match &s.data {
                AstStmtData::If { cond, then, else_ } => {
                    expr_uses(cond) || stmt_uses(then)
                        || else_.as_ref().map_or(false, |e| stmt_uses(e))
                }
                AstStmtData::Compound(v) => v.iter().any(stmt_uses),
                AstStmtData::While { cond, body } | AstStmtData::Do { cond, body } => {
                    expr_uses(cond) || stmt_uses(body)
                }
                AstStmtData::For { init, cond, incr, body } => {
                    init.as_ref().map_or(false, |i| stmt_uses(i))
                        || cond.as_ref().map_or(false, |c| expr_uses(c))
                        || incr.as_ref().map_or(false, |c| expr_uses(c))
                        || stmt_uses(body)
                }
                AstStmtData::Try { try_block, catches, finally_block } => {
                    stmt_uses(try_block) || catches.iter().any(stmt_uses)
                        || finally_block.as_ref().map_or(false, |f| stmt_uses(f))
                }
                AstStmtData::Catch { body, .. } => stmt_uses(body),
                AstStmtData::Finally(b) => stmt_uses(b),
                AstStmtData::Synchronized { body, .. }
                | AstStmtData::Autoreleasepool(body)
                | AstStmtData::NoArc(body) => stmt_uses(body),
                AstStmtData::Switch { expr, body } => expr_uses(expr) || stmt_uses(body),
                AstStmtData::Case { value, body } => expr_uses(value) || stmt_uses(body),
                AstStmtData::Default(body) => stmt_uses(body),
                AstStmtData::Expr(e) => expr_uses(e),
                AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => expr_uses(e),
                _ => false,
            }
        }
        unit.decls.iter().any(|d| match &d.data {
            AstDeclData::Function { body, .. } | AstDeclData::Method { body, .. } => {
                body.as_ref().map_or(false, |b| stmt_uses(b))
            }
            _ => false,
        })
    }

    /// Guard density: a loop whose body cannot arm the flag must NOT get its
    /// condition wrapped in `(flag == 0) && ...`. The `for (;;)` branch always
    /// consulted `stmt_may_arm`; `while` / `do` / `for`-with-cond armed
    /// unconditionally, so `while (i < 5) i++;` — no call anywhere — still paid
    /// a global flag read on every single iteration.
    #[test]
    fn pure_loop_condition_is_not_wrapped_in_a_flag_guard() {
        let mut unit = unit_with_while(None);
        desugar_unit(&mut unit);
        let cond = while_cond(&unit).expect("probe must contain a while loop");
        assert!(
            !expr_uses_flag(cond),
            "a loop body with nothing throwable must not pay a flag guard"
        );
    }

    /// The mirror image: when the body really can throw, ObjC unwinds out of
    /// the loop, so the flag must still be re-tested before every re-entry.
    /// Narrowing the guard must not silently drop this case.
    #[test]
    fn throwing_loop_condition_keeps_its_flag_guard() {
        let throw = AstStmt {
            kind: jeti_ast::AstStmtKind::Throw, line: 0, col: 0,
            data: AstStmtData::Throw(Some(Box::new(int_expr(1)))),
        };
        let mut unit = unit_with_while(Some(throw));
        desugar_unit(&mut unit);
        let cond = while_cond(&unit).expect("probe must contain a while loop");
        assert!(
            expr_uses_flag(cond),
            "a body that arms the flag must keep the re-entry guard"
        );
    }

    // ── helpers for the two guard-density tests ──────────────────────────────

    fn expr_uses_flag(e: &AstExpr) -> bool {
        if matches!(&e.data, AstExprData::VarRef { name, .. } if name == "__jeti_eh_flag") {
            return true;
        }
        EffectTable::effect_children(e).into_iter().any(expr_uses_flag)
    }

    fn while_cond(unit: &AstUnit) -> Option<&AstExpr> {
        fn walk(s: &AstStmt) -> Option<&AstExpr> {
            match &s.data {
                AstStmtData::While { cond, .. } => Some(cond),
                AstStmtData::Compound(v) => v.iter().find_map(walk),
                _ => None,
            }
        }
        unit.decls.iter().find_map(|d| match &d.data {
            AstDeclData::Function { body: Some(b), .. }
            | AstDeclData::Method { body: Some(b), .. } => walk(b),
            _ => None,
        })
    }

    /// `int probe(void) { while (1) { 1; [extra] } }` — the condition is a
    /// constant and the body is a bare expression, so the *only* thing that can
    /// put a flag guard on the condition is the effect analysis under test.
    fn unit_with_while(extra: Option<AstStmt>) -> AstUnit {
        let mut items = vec![AstStmt {
            kind: jeti_ast::AstStmtKind::Expr, line: 0, col: 0,
            data: AstStmtData::Expr(int_expr(1)),
        }];
        if let Some(x) = extra {
            items.push(x);
        }
        let while_stmt = AstStmt {
            kind: jeti_ast::AstStmtKind::While, line: 0, col: 0,
            data: AstStmtData::While {
                cond: Box::new(int_expr(1)),
                body: Box::new(AstStmt {
                    kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
                    data: AstStmtData::Compound(items),
                }),
            },
        };
        let body = AstStmt {
            kind: jeti_ast::AstStmtKind::Compound, line: 0, col: 0,
            data: AstStmtData::Compound(vec![while_stmt]),
        };
        let func = AstDecl {
            kind: jeti_ast::AstDeclKind::Function, name: Some("probe".into()), line: 1, col: 1,
            data: AstDeclData::Function {
                func_sym: None, return_type: Some(Box::new(AstType::new(TypePrim::Int))),
                params: None, body: Some(Box::new(body)),
                has_variadic: false, throws: None, async_marker: false,
            },
            attributes: Vec::new(),
        };
        AstUnit { decls: vec![func], filename: "probe.jeti".into() }
    }
}
