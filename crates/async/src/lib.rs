//! Async/await desugar — Async Semantics v1 (doc/plans/async_semantics_v1.md).
//!
//! `async` is part of the signature and means MAY suspend. A call to an async
//! method must be `await`-ed (rule 2 — the checker enforces it); the desugar
//! rewrites each async method into a **state-machine function** operating on
//! a heap task, and the only sync→async bridge is `+[NPTask blockOn:<async
//! block>]` (rule 3), which lowers to a blocking `jeti_task_await`.
//!
//! - `await e` splits the body into segments; each segment runs inside one
//!   `switch (task->state)` arm.
//! - Locals live in a per-method `NPTaskFrame` struct so they survive
//!   suspension (codegen's @try lifting precedent).
//! - break/continue across awaits become state jumps (no C break/continue
//!   across switch arms).
//! - `@try` spanning an await is rejected in milestone 1 (jmp_buf cannot
//!   survive suspension); `@noarc` is allowed.
//! - ARC settle: object locals lifted into the frame are released at task
//!   finish/cancel (single convergence point).
//!
//! This module currently provides:
//!   1. async classification (body contains `await` — chain infection),
//!   2. `@try` spanning an await rejection.
//!
//! Note (doc/async_nptask_plan.md): bare async calls from sync contexts are
//! LEGAL — form A (lazy task creation, handle is a first-class value). The
//! old M1 sync-context call check was removed when `NPTask<T>` handles
//! became real values; `[task start]` / `await` drive them afterwards.

use jeti_ast::{AstDecl, AstDeclData, AstExpr, AstExprData, AstExprKind, AstStmt, AstStmtData};

/// Does this expression subtree contain an `await`?
pub fn expr_contains_await(e: &AstExpr) -> bool {
    match &e.data {
        AstExprData::Await { .. } => true,
        AstExprData::MsgSend { receiver, args, .. } => {
            expr_contains_await(receiver) || args.iter().any(expr_contains_await)
        }
        AstExprData::FuncCall { callee, args, .. } => {
            callee.as_ref().map_or(false, |c| expr_contains_await(c))
                || args.iter().any(expr_contains_await)
        }
        AstExprData::Unary { operand, .. } => expr_contains_await(operand),
        AstExprData::Binary { left, right, .. } => {
            expr_contains_await(left) || expr_contains_await(right)
        }
        AstExprData::Assign { target, value } => {
            expr_contains_await(target) || expr_contains_await(value)
        }
        AstExprData::Cast { expr, .. } => expr_contains_await(expr),
        // Parenthesized — `(await [x y])` as a message argument (the mandated
        // in-message spelling) or any grouped subexpression. Missing this arm
        // made the whole shape invisible: no suspension point, no M2 rewrite,
        // and the raw NPTask* leaked into a scalar parameter.
        AstExprData::Paren(inner) => expr_contains_await(inner),
        AstExprData::Ternary { cond, then, else_ } => {
            expr_contains_await(cond)
                || expr_contains_await(then)
                || expr_contains_await(else_)
        }
        AstExprData::Comma(exprs) => exprs.iter().any(expr_contains_await),
        AstExprData::Subscript { object, key } => {
            expr_contains_await(object) || expr_contains_await(key)
        }
        AstExprData::InitList(items) | AstExprData::ArrayLit(items) => {
            items.iter().any(expr_contains_await)
        }
        _ => false,
    }
}

/// Does this statement subtree contain an `await`?
pub fn stmt_contains_await(s: &AstStmt) -> bool {
    match &s.data {
        AstStmtData::Expr(e) => expr_contains_await(e),
        AstStmtData::Compound(stmts) => stmts.iter().any(stmt_contains_await),
        AstStmtData::If { cond, then, else_ } => {
            expr_contains_await(cond)
                || stmt_contains_await(then)
                || else_.as_ref().map_or(false, |e| stmt_contains_await(e))
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            expr_contains_await(cond) || stmt_contains_await(body)
        }
        AstStmtData::For { init, cond, incr, body } => {
            init.as_ref().map_or(false, |x| stmt_contains_await(x))
                || cond.as_ref().map_or(false, |x| expr_contains_await(x))
                || incr.as_ref().map_or(false, |x| expr_contains_await(x))
                || stmt_contains_await(body)
        }
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => expr_contains_await(e),
        AstStmtData::Switch { expr, body } => {
            expr_contains_await(expr) || stmt_contains_await(body)
        }
        AstStmtData::Case { value, body } => {
            expr_contains_await(value) || stmt_contains_await(body)
        }
        AstStmtData::Default(body) => stmt_contains_await(body),
        AstStmtData::Synchronized { lock, body } => {
            expr_contains_await(lock) || stmt_contains_await(body)
        }
        AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => stmt_contains_await(body),
        AstStmtData::Decl(d) => decl_contains_await(d),
        _ => false,
    }
}

fn decl_contains_await(d: &AstDecl) -> bool {
    match &d.data {
        AstDeclData::Variable { init, next, .. } => {
            init.as_ref().map_or(false, |x| expr_contains_await(x))
                || next.as_ref().map_or(false, |x| decl_contains_await(x))
        }
        _ => false,
    }
}

// ── §7 body self-evidence: what counts as a suspension point ────────────────
//
// An `await` is the only suspension-point shape left (Async Semantics v1
// rule 2 removed statement-position auto-drive). The statement-position
// async-call arm below is kept ONLY to feed the `suspends` flag the
// reconcile/diagnostic path still reads; it does not rewrite anything.

/// Does this statement subtree contain a suspension point (an `await`, or a
/// statement-position call to a known async method / async-block thunk)?
fn stmt_suspends(s: &AstStmt, async_sels: &std::collections::HashSet<String>) -> bool {
    match &s.data {
        AstStmtData::Expr(e) => expr_suspends(e, async_sels),
        AstStmtData::Compound(stmts) => stmts.iter().any(|st| stmt_suspends(st, async_sels)),
        AstStmtData::If { cond, then, else_ } => {
            expr_suspends(cond, async_sels)
                || stmt_suspends(then, async_sels)
                || else_.as_ref().map_or(false, |e| stmt_suspends(e, async_sels))
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            expr_suspends(cond, async_sels) || stmt_suspends(body, async_sels)
        }
        AstStmtData::For { init, cond, incr, body } => {
            init.as_ref().map_or(false, |x| stmt_suspends(x, async_sels))
                || cond.as_ref().map_or(false, |x| expr_suspends(x, async_sels))
                || incr.as_ref().map_or(false, |x| expr_suspends(x, async_sels))
                || stmt_suspends(body, async_sels)
        }
        // Return/Throw are VALUE-ESCAPE positions: `return [self compute:42]`
        // is the legal lazy form A (task created, not started) — it yields a
        // handle to the CALLER and does not suspend THIS body. Only an
        // explicit `await` counts there (§2.1 form A).
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => expr_contains_await(e),
        AstStmtData::Switch { expr, body } => {
            expr_suspends(expr, async_sels) || stmt_suspends(body, async_sels)
        }
        AstStmtData::Case { body, .. } | AstStmtData::Default(body) => {
            stmt_suspends(body, async_sels)
        }
        AstStmtData::Decl(d) => decl_suspends(d, async_sels),
        _ => false,
    }
}

fn decl_suspends(d: &AstDecl, async_sels: &std::collections::HashSet<String>) -> bool {
    match &d.data {
        AstDeclData::Variable { init, next, .. } => {
            init.as_ref().map_or(false, |x| expr_suspends(x, async_sels))
                || next.as_ref().map_or(false, |x| decl_suspends(x, async_sels))
        }
        _ => false,
    }
}

/// Expression-position suspension: only an explicit `await` counts. A bare
/// async call in EXPRESSION position (e.g. an argument) yields a handle the
/// caller decides about — not a suspension of THIS body.
fn expr_suspends(e: &AstExpr, async_sels: &std::collections::HashSet<String>) -> bool {
    match &e.data {
        AstExprData::Await { .. } => true,
        // STATEMENT-position async call: the statement walk hands the whole
        // statement's expression here, so a top-level call to a known async
        // method is the auto-drive form (§5.1 / §2.1). Recurse into subexpressions
        // only for awaits — a nested async call is handle-yielding, not suspension.
        AstExprData::MsgSend { selector, .. } => {
            async_sels.contains(&selector.replace(':', ""))
                || expr_contains_await(e)
        }
        AstExprData::FuncCall { .. } => expr_yields_task_static(e, async_sels)
            || expr_contains_await(e),
        // A ternary of two task-yielding branches at statement position is
        // the same auto-drive shape (§5.1) — count it as a suspension point
        // so reconcile stays consistent with the desugar's drive pass.
        AstExprData::Ternary { .. } => {
            expr_yields_task_static(e, async_sels) || expr_contains_await(e)
        }
        AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => {
            expr_suspends(inner, async_sels)
        }
        _ => expr_contains_await(e),
    }
}

/// Static (pre-desugar) counterpart of `expr_yields_task`: async methods are
/// known from the declaration scan; async-block thunks don't exist yet at
/// check time, but an async block IIFE still has its callee block — detect
/// the call shape `FuncCall { callee: Block { is_async: true, … } }`.
fn expr_yields_task_static(e: &AstExpr, async_sels: &std::collections::HashSet<String>) -> bool {
    match &e.data {
        AstExprData::MsgSend { selector, .. } => {
            async_sels.contains(&selector.replace(':', ""))
        }
        AstExprData::FuncCall { callee, .. } => {
            matches!(&callee.as_ref().map(|c| &c.data),
                Some(AstExprData::Block { is_async: true, .. }))
        }
        // Statement-position `cond ? [self a:x] : [self b:y]` auto-drives
        // like a bare send when BOTH branches yield tasks (§5.1); a
        // null-pointer-constant branch drives too (jeti_task_await(NULL) is
        // total, mirroring expr_yields_task); mixed arithmetic branches are
        // a ternary type-compat error, not a drive site.
        AstExprData::Ternary { then, else_, .. } => {
            let nc = |x: &AstExpr| match &x.data {
                AstExprData::Int(0) => true,
                AstExprData::Cast { expr, .. } => matches!(&expr.data, AstExprData::Int(0)),
                _ => false,
            };
            (expr_yields_task_static(then, async_sels)
                && expr_yields_task_static(else_, async_sels))
                || (expr_yields_task_static(then, async_sels) && nc(else_))
                || (nc(then) && expr_yields_task_static(else_, async_sels))
        }
        _ => false,
    }
}

/// Async Semantics v1 rule 6: `await` applied to an expression that can never
/// yield a task handle is a no-op — the operand evaluates synchronously and
/// the await just forwards its value. A WARNING, never an error (the stage-2
/// Swift-model switch tightens call-site discipline separately). Deliberately
/// conservative to avoid false positives:
///   * identifier operands are skipped — `await t` may await a task handle,
///     and proving which needs the symbol table this pass doesn't have;
///   * Paren/Cast wrappers are transparent (expr_yields_task_static has no
///     arms for them by design);
///   * every other operand is classified by the desugar's own rule
///     (expr_yields_task_static), so the lint can never contradict the
///     lowering.
fn warn_await_on_sync_stmt(
    s: &AstStmt,
    async_sels: &std::collections::HashSet<String>,
    warns: &mut Vec<String>,
) {
    match &s.data {
        AstStmtData::Expr(e) => warn_await_on_sync_expr(e, async_sels, warns),
        AstStmtData::Return(Some(e)) => warn_await_on_sync_expr(e, async_sels, warns),
        AstStmtData::Decl(d) => warn_await_on_sync_decl(d, async_sels, warns),
        AstStmtData::If { cond, then, else_, .. } => {
            warn_await_on_sync_expr(cond, async_sels, warns);
            warn_await_on_sync_stmt(then, async_sels, warns);
            if let Some(e) = else_ { warn_await_on_sync_stmt(e, async_sels, warns); }
        }
        AstStmtData::While { cond, body, .. } => {
            warn_await_on_sync_expr(cond, async_sels, warns);
            warn_await_on_sync_stmt(body, async_sels, warns);
        }
        AstStmtData::Do { cond, body, .. } => {
            warn_await_on_sync_expr(cond, async_sels, warns);
            warn_await_on_sync_stmt(body, async_sels, warns);
        }
        AstStmtData::For { init, cond, body, .. } => {
            if let Some(i) = init { warn_await_on_sync_stmt(i, async_sels, warns); }
            if let Some(c) = cond { warn_await_on_sync_expr(c, async_sels, warns); }
            warn_await_on_sync_stmt(body, async_sels, warns);
        }
        AstStmtData::Switch { expr, body, .. } => {
            warn_await_on_sync_expr(expr, async_sels, warns);
            warn_await_on_sync_stmt(body, async_sels, warns);
        }
        AstStmtData::Case { value, body, .. } => {
            warn_await_on_sync_expr(value, async_sels, warns);
            warn_await_on_sync_stmt(body, async_sels, warns);
        }
        AstStmtData::Default(body) => warn_await_on_sync_stmt(body, async_sels, warns),
        AstStmtData::Try { try_block, catches, finally_block } => {
            warn_await_on_sync_stmt(try_block, async_sels, warns);
            for c in catches { warn_await_on_sync_stmt(c, async_sels, warns); }
            if let Some(f) = finally_block { warn_await_on_sync_stmt(f, async_sels, warns); }
        }
        AstStmtData::Compound(stmts) => {
            for st in stmts { warn_await_on_sync_stmt(st, async_sels, warns); }
        }
        AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            warn_await_on_sync_stmt(body, async_sels, warns);
        }
        AstStmtData::Synchronized { lock, body, .. } => {
            warn_await_on_sync_expr(lock, async_sels, warns);
            warn_await_on_sync_stmt(body, async_sels, warns);
        }
        _ => {}
    }
}

fn warn_await_on_sync_decl(
    d: &AstDecl,
    async_sels: &std::collections::HashSet<String>,
    warns: &mut Vec<String>,
) {
    match &d.data {
        AstDeclData::Variable { init, next, .. } => {
            if let Some(e) = init { warn_await_on_sync_expr(e, async_sels, warns); }
            if let Some(n) = next { warn_await_on_sync_decl(n, async_sels, warns); }
        }
        _ => {}
    }
}

fn warn_await_on_sync_expr(
    e: &AstExpr,
    async_sels: &std::collections::HashSet<String>,
    warns: &mut Vec<String>,
) {
    match &e.data {
        AstExprData::Await { inner } => {
            // Classify the operand: strip transparent wrappers first, then
            // apply the desugar's own task-yield rule.
            let mut op: &AstExpr = inner;
            while let AstExprData::Paren(p) | AstExprData::Cast { expr: p, .. } = &op.data {
                op = p;
            }
            let identifier_op = matches!(op.data, AstExprData::VarRef { .. });
            if !identifier_op && !expr_yields_task_static(op, async_sels) {
                warns.push(format!(
                    "{}:{}: 'await' has no effect here — the operand is not async and yields no task handle (Async Semantics v1 rule 6)",
                    e.line, e.col));
            }
            warn_await_on_sync_expr(inner, async_sels, warns);
        }
        AstExprData::MsgSend { receiver, args, .. } => {
            warn_await_on_sync_expr(receiver, async_sels, warns);
            for a in args { warn_await_on_sync_expr(a, async_sels, warns); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { warn_await_on_sync_expr(c, async_sels, warns); }
            for a in args { warn_await_on_sync_expr(a, async_sels, warns); }
        }
        AstExprData::Unary { operand, .. } => warn_await_on_sync_expr(operand, async_sels, warns),
        AstExprData::Binary { left, right, .. }
        | AstExprData::Assign { target: left, value: right } => {
            warn_await_on_sync_expr(left, async_sels, warns);
            warn_await_on_sync_expr(right, async_sels, warns);
        }
        AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => {
            warn_await_on_sync_expr(inner, async_sels, warns);
        }
        AstExprData::Ternary { cond, then, else_ } => {
            warn_await_on_sync_expr(cond, async_sels, warns);
            warn_await_on_sync_expr(then, async_sels, warns);
            warn_await_on_sync_expr(else_, async_sels, warns);
        }
        AstExprData::Comma(exprs) => {
            for x in exprs { warn_await_on_sync_expr(x, async_sels, warns); }
        }
        AstExprData::Subscript { object, key } => {
            warn_await_on_sync_expr(object, async_sels, warns);
            warn_await_on_sync_expr(key, async_sels, warns);
        }
        AstExprData::InitList(items) | AstExprData::ArrayLit(items) => {
            for x in items { warn_await_on_sync_expr(x, async_sels, warns); }
        }
        AstExprData::Block { body, .. } => {
            if let Some(b) = body { warn_await_on_sync_stmt(b, async_sels, warns); }
        }
        _ => {}
    }
}

/// Async Semantics v1 rule 2 (doc/plans/async_semantics_v1.md §2): a call to
/// an async method MUST be awaited. A bare task-yielding expression whose
/// value is silently discarded (statement position, comma tail, cast-to-void,
/// block-literal no-op call) used to auto-drive (hosted) — the Swift-model
/// switch removes that third meaning; the error points at the legal
/// spellings. `awaited` marks an operand already consumed by an enclosing
/// `await` (casts do NOT count — see the Cast arm).
fn check_call_site_discipline_stmt(
    s: &AstStmt,
    async_sels: &std::collections::HashSet<String>,
    errs: &mut Vec<String>,
) {
    match &s.data {
        AstStmtData::Expr(e) => check_call_site_discipline_expr(e, false, async_sels, errs),
        // v1 rule 2 has NO "capture instead" escape: `return [f x];` and
        // `NPTask<T> *t = [f x];` are bare async calls too — under the Swift
        // model a bare call creates no handle (handles come only from spawn,
        // stage 3), so both are errors here.
        AstStmtData::Return(Some(e)) => {
            check_call_site_discipline_expr(e, false, async_sels, errs)
        }
        AstStmtData::Decl(d) => check_call_site_discipline_decl(d, async_sels, errs),
        AstStmtData::If { cond, then, else_, .. } => {
            check_call_site_discipline_expr(cond, false, async_sels, errs);
            check_call_site_discipline_stmt(then, async_sels, errs);
            if let Some(e) = else_ { check_call_site_discipline_stmt(e, async_sels, errs); }
        }
        AstStmtData::While { cond, body, .. } => {
            check_call_site_discipline_expr(cond, false, async_sels, errs);
            check_call_site_discipline_stmt(body, async_sels, errs);
        }
        AstStmtData::Do { cond, body, .. } => {
            check_call_site_discipline_expr(cond, false, async_sels, errs);
            check_call_site_discipline_stmt(body, async_sels, errs);
        }
        AstStmtData::For { init, cond, body, .. } => {
            if let Some(i) = init { check_call_site_discipline_stmt(i, async_sels, errs); }
            if let Some(c) = cond { check_call_site_discipline_expr(c, false, async_sels, errs); }
            check_call_site_discipline_stmt(body, async_sels, errs);
        }
        AstStmtData::Switch { expr, body, .. } => {
            check_call_site_discipline_expr(expr, false, async_sels, errs);
            check_call_site_discipline_stmt(body, async_sels, errs);
        }
        AstStmtData::Case { value, body, .. } => {
            check_call_site_discipline_expr(value, false, async_sels, errs);
            check_call_site_discipline_stmt(body, async_sels, errs);
        }
        AstStmtData::Default(body) => check_call_site_discipline_stmt(body, async_sels, errs),
        AstStmtData::Try { try_block, catches, finally_block } => {
            check_call_site_discipline_stmt(try_block, async_sels, errs);
            for c in catches { check_call_site_discipline_stmt(c, async_sels, errs); }
            if let Some(f) = finally_block { check_call_site_discipline_stmt(f, async_sels, errs); }
        }
        AstStmtData::Compound(stmts) => {
            for st in stmts { check_call_site_discipline_stmt(st, async_sels, errs); }
        }
        AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            check_call_site_discipline_stmt(body, async_sels, errs);
        }
        AstStmtData::Synchronized { lock, body, .. } => {
            check_call_site_discipline_expr(lock, false, async_sels, errs);
            check_call_site_discipline_stmt(body, async_sels, errs);
        }
        _ => {}
    }
}

fn check_call_site_discipline_decl(
    d: &AstDecl,
    async_sels: &std::collections::HashSet<String>,
    errs: &mut Vec<String>,
) {
    match &d.data {
        AstDeclData::Variable { init, next, .. } => {
            if let Some(e) = init { check_call_site_discipline_expr(e, false, async_sels, errs); }
            if let Some(n) = next { check_call_site_discipline_decl(n, async_sels, errs); }
        }
        _ => {}
    }
}

fn check_call_site_discipline_expr(
    e: &AstExpr,
    awaited: bool,
    async_sels: &std::collections::HashSet<String>,
    errs: &mut Vec<String>,
) {
    let mut awaited = awaited;
    match &e.data {
        AstExprData::Await { inner, .. } => {
            // The await site itself: its operand is consumed here — including
            // through grouping/casts, which pass `awaited` through below.
            // Identifier operands are NOT tracked (task vars are desugar-ctx
            // state this pass doesn't have) — no false positives by design.
            awaited = true;
            check_call_site_discipline_expr(inner, awaited, async_sels, errs);
        }
        AstExprData::MsgSend { receiver, selector, args, .. } => {
            check_call_site_discipline_expr(receiver, false, async_sels, errs);
            for a in args { check_call_site_discipline_expr(a, false, async_sels, errs); }
            if !awaited && async_sels.contains(&selector.replace(':', "")) {
                errs.push(format!(
                    "{0}:{1}: call to async method '{2}' must be awaited — an async call yields a task handle, not a value (Async Semantics v1 rule 2)",
                    e.line, e.col, selector));
            }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { check_call_site_discipline_expr(c, false, async_sels, errs); }
            for a in args { check_call_site_discipline_expr(a, false, async_sels, errs); }
            // Rule 2 covers async-block IIFEs too: invoking `async ^{…}(args)`
            // yields a task handle, so the invocation must be awaited. The
            // checker runs BEFORE the desugar, so the callee is still the
            // block literal — matched directly or through a grouping paren
            // (the shapes the desugar's IIFE arm accepts). A statement- or
            // argument-position bare IIFE is an error; only
            // `await (async ^…)(args)` consumes the task.
            if let Some(c) = callee {
                let is_async_block_callee = match &c.data {
                    AstExprData::Block { is_async: true, .. } => true,
                    AstExprData::Paren(inner) => matches!(&inner.data,
                        AstExprData::Block { is_async: true, .. }),
                    _ => false,
                };
                if is_async_block_callee && !awaited {
                    errs.push(format!(
                        "{0}:{1}: call to an async block must be awaited — invoking 'async ^{{…}}' yields a task handle, not a value (Async Semantics v1 rule 2)",
                        e.line, e.col));
                }
            }
        }
        AstExprData::Unary { operand, .. } => {
            check_call_site_discipline_expr(operand, false, async_sels, errs)
        }
        AstExprData::Binary { left, right, .. } => {
            check_call_site_discipline_expr(left, false, async_sels, errs);
            check_call_site_discipline_expr(right, false, async_sels, errs);
        }
        AstExprData::Assign { target, value } => {
            check_call_site_discipline_expr(target, false, async_sels, errs);
            check_call_site_discipline_expr(value, false, async_sels, errs);
        }
        AstExprData::Paren(inner) => {
            check_call_site_discipline_expr(inner, awaited, async_sels, errs)
        }
        // A cast cannot satisfy the await requirement: `(void)[self compute]`
        // is an explicit discard, not a consume — the call still leaks its
        // task handle, so it must error like any bare call.
        AstExprData::Cast { expr: inner, .. } => {
            check_call_site_discipline_expr(inner, false, async_sels, errs)
        }
        AstExprData::Ternary { cond, then, else_ } => {
            check_call_site_discipline_expr(cond, false, async_sels, errs);
            // A ternary is ONE async expression: when the whole thing is
            // awaited (`await (c ? [a] : [b])`), its branches are consumed by
            // that single await — same reasoning as Paren. A BARE ternary
            // passes awaited=false down, so its async branches still error.
            check_call_site_discipline_expr(then, awaited, async_sels, errs);
            check_call_site_discipline_expr(else_, awaited, async_sels, errs);
        }
        AstExprData::Comma(exprs) => {
            let last = exprs.len().saturating_sub(1);
            for (i, x) in exprs.iter().enumerate() {
                check_call_site_discipline_expr(x, awaited && i == last, async_sels, errs);
            }
        }
        AstExprData::Subscript { object, key } => {
            check_call_site_discipline_expr(object, false, async_sels, errs);
            check_call_site_discipline_expr(key, false, async_sels, errs);
        }
        AstExprData::InitList(items) | AstExprData::ArrayLit(items) => {
            for x in items { check_call_site_discipline_expr(x, false, async_sels, errs); }
        }
        AstExprData::Block { body: Some(b), is_async: true, .. } => {
            check_call_site_discipline_stmt(b, async_sels, errs);
        }
        _ => {}
    }
}

/// Is the given method async? The `async` declaration marker decides; for
/// legacy shapes without a marker the body containing an `await` still
/// classifies (see the comment inside).
pub fn method_is_async(m: &AstDecl) -> bool {
    match &m.data {
        // The declaration marker IS the classification (Async Semantics v1:
        // `async` is a signature concept). The old body-contains-await
        // heuristic is gone — it reclassified a SYNC method that merely
        // contains an `await` (e.g. a blockOn: bridge, whose synthesized
        // Await lands in the body before this runs) as async and ran the M2
        // rewrite over it, emitting `return __jeti_task` from an `int`
        // function. An unmarked body with `await` is a checker error before
        // the desugar ever runs, so nothing legal depended on the heuristic.
        AstDeclData::Method { async_marker, .. } => *async_marker,
        // Top-level functions keep the legacy body probe — the parser's
        // async-marker plumbing for top-level functions is a registered
        // known gap (async-semantics v1 §4), not touched in this stage.
        AstDeclData::Function { body: Some(b), .. } => stmt_contains_await(b),
        _ => false,
    }
}

/// Diagnostics collected during the async pre-pass (reported by the pipeline
/// before desugar runs; any error aborts compilation).
pub struct AsyncDiagnostics {
    pub errors: Vec<String>,
    /// Recoverable issues (currently unused: async-modifier reconciliation
    /// reports errors in both directions). Purple pipeline warnings; `-Werror`
    /// escalates.
    pub warnings: Vec<String>,
}

/// Pre-desugar validation over the whole AST unit:
/// - reject `@try` blocks that contain an `await` (jmp_buf cannot survive a
///   suspension point in milestone 1),
/// - reject calling a non-void async method from a synchronous context
///   (methods not classified async). Milestone 1 reports these; call-graph
///   precision improves in later milestones.
pub fn check_unit(unit: &jeti_ast::AstUnit) -> AsyncDiagnostics {
    let mut diags = AsyncDiagnostics { errors: Vec::new(), warnings: Vec::new() };

    // ── async-modifier reconciliation (doc/async_nptask_plan.md, stage B) ──
    // The modifier is a signature-level flag; the body's awaits decide the
    // truth. Only implementations (methods WITH a body) reconcile against the
    // body — header-only `@interface` methods have no body to contradict
    // them (cross-TU safety, same rule as the proto-conformance check).
    // Interface/impl marker agreement is checked separately below.
    let mut iface_markers: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    // Async selector set for the §7 self-evidence walk below (same shape as
    // the desugar pass's pre-collection — colon-stripped).
    let mut async_sels: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Class IMPLEMENTATIONS only — never protocol requirement names. The
    // Protocol AST variant stores bare selector strings with no async marker,
    // so collecting from it classified every sync protocol method (`speak`,
    // `draw`, `printOn:` …) as async and made rule-2 checker error on
    // ordinary protocol calls. Calls through `id<Proto>` to conformers
    // implemented in this unit are covered by the impl classes here; a
    // conformer in ANOTHER TU cannot be driven anyway (the M2 entry fn is
    // static to its owner TU — cross-TU async calls are not a supported
    // shape).
    for decl in &unit.decls {
        if let AstDeclData::Class { methods, .. } = &decl.data {
            for m in methods {
                if method_is_async(m) {
                    if let Some(sel) = &m.name {
                        async_sels.insert(sel.replace(':', ""));
                    }
                }
            }
        }
    }
    for decl in &unit.decls {
        if let AstDeclData::Class { methods, is_implementation: false, cls_sym: Some(cls), .. } = &decl.data {
            for m in methods {
                if let (AstDeclData::Method { async_marker, .. }, Some(sym)) = (&m.data, &m.name) {
                    iface_markers.insert(format!("{}::{}", cls, sym), *async_marker);
                }
            }
        }
    }
    for decl in &unit.decls {
        match &decl.data {
            AstDeclData::Class { methods, cls_sym: Some(cls), .. } => {
                for m in methods {
                    let (marker, body) = match &m.data {
                        AstDeclData::Method { async_marker, body, .. } => (*async_marker, body),
                        _ => continue,
                    };
                    let Some(sym) = &m.name else { continue };
                    // The marker is part of the signature: @interface and
                    // @implementation must agree.
                    let key = format!("{}::{}", cls, sym);
                    if let Some(&iface_marked) = iface_markers.get(&key) {
                        if iface_marked != marker {
                            diags.errors.push(format!(
                                "{}:{}: 'async' modifier mismatch on '{}': the @interface and @implementation disagree — the modifier is part of the method signature",
                                m.line, m.col, sym));
                        }
                    }
                    // Body reconciliation — implementations only. Two distinct
                    // notions, deliberately not conflated:
                    //   * `suspends` (an `await`, or a statement-position
                    //     auto-driven async call — §7 self-evidence) proves a
                    //     marked body really suspends.
                    //   * `real_await` (an actual `await` node in THIS body)
                    //     is what makes an UNMARKED body a lying signature.
                    // A plain method that merely drives an async block IIFE at
                    // statement position blocks, it does not suspend and it
                    // has no `await` — legal per §2.1 例1/例2.
                    if let Some(b) = body {
                        let suspends = stmt_suspends(b, &async_sels);
                        let real_await = stmt_contains_await(b);
                        reconcile_marker(marker, suspends, real_await, sym, m.line, m.col, &mut diags);
                        // v1 rule 6 lint: `await` on a non-async operand. Only
                        // inside async bodies — an `await` in a sync body is
                        // already a hard error from the context walk, and
                        // stacking a warning on it is noise. Needs the
                        // unit-wide async_sels set, so it runs here rather
                        // than inside check_async_context.
                        if marker {
                            warn_await_on_sync_stmt(b, &async_sels, &mut diags.warnings);
                        }
                        // v1 rule 2 call-site discipline: every async call in
                        // the body must be awaited — in async bodies AND sync
                        // ones (rule 2 has no context exemption).
                        check_call_site_discipline_stmt(b, &async_sels, &mut diags.errors);
                    }
                }
            }
            // Top-level functions (main included): `await` here is the
            // BLOCKING-join entry form (doc/async_nptask_plan.md — sync
            // context awaits pump/drive the task inline). Legal without the
            // modifier; only class methods reconcile.
            _ => {}
        }
    }

    // Walk every method body AND top-level function body (main included):
    // @try/await check. (The M1 sync-context call check is gone — a bare
    // async call from sync code is the legal lazy form A.)
    for decl in &unit.decls {
        match &decl.data {
            AstDeclData::Class { methods, .. } => {
                for m in methods {
                    if let (AstDeclData::Method { body: Some(b), .. }, Some(sym)) = (&m.data, &m.name) {
                        check_stmt_for_try_await(b, sym, &mut diags);
                    }
                }
            }
            AstDeclData::Function { body: Some(b), .. } => {
                let sym = decl.name.clone().unwrap_or_else(|| "<function>".to_string());
                check_stmt_for_try_await(b, &sym, &mut diags);
            }
            _ => {}
        }
    }

    // ── §6 migration: old `(async NPTask<T>)` spellings are a semantic
    // change under the new model (`async T`'s T is the business type, so
    // `async NPTask<int>` becomes "business type = task handle"). Error until
    // existing uses migrate to `(async T)` — never silent.
    for decl in &unit.decls {
        let methods: Box<dyn Iterator<Item = &AstDecl>> = match &decl.data {
            AstDeclData::Class { methods, .. } => Box::new(methods.iter()),
            _ => Box::new(std::iter::empty()),
        };
        for m in methods {
            if let AstDeclData::Method { async_marker, return_type: Some(rt), .. } = &m.data {
                if *async_marker {
                    check_async_nptask_spelling(rt, m.line, m.col, &mut diags);
                }
            }
        }
    }

    // ── §7 (async_soft_keywords_plan): await context + body self-evidence ──
    for decl in &unit.decls {
        match &decl.data {
            AstDeclData::Class { methods, .. } => {
                for m in methods {
                    let (marker, body, sym) = match &m.data {
                        AstDeclData::Method { async_marker, body, .. } =>
                            (*async_marker, body, m.name.clone().unwrap_or_else(|| "<method>".into())),
                        _ => continue,
                    };
                    if let Some(b) = body {
                        check_async_context(b, &sym, marker, &mut diags);
                    }
                }
            }
            AstDeclData::Function { body: Some(b), async_marker, .. } => {
                let sym = decl.name.clone().unwrap_or_else(|| "<function>".into());
                // Async Semantics v1 rule 1: `async` is a capability ("MAY
                // suspend"), NOT "must suspend" — a body with no `await` is
                // legal (cache hit, protocol impl that happens to be sync).
                // The old §4.2 body-self-evidence error is gone (doc/plans/
                // async_semantics_v1.md §3). The reverse check (await without
                // the modifier) stays a hard error, enforced below.
                check_async_context(b, &sym, *async_marker, &mut diags);
                if *async_marker {
                    warn_await_on_sync_stmt(b, &async_sels, &mut diags.warnings);
                }
                // v1 rule 2 call-site discipline — same rule as methods.
                check_call_site_discipline_stmt(b, &async_sels, &mut diags.errors);
            }
            _ => {}
        }
    }
    diags
}






/// §5.3-1: `await` is legal only inside an async context — an async
/// method body (checked via the method-level reconciliation below), an async
/// function body, or an `async ^…` block body. A `await` inside a PLAIN block
/// body is an error (replaces the old silent-swallow bug). (Async Semantics
/// v1 rule 1 removed the old "never suspends" self-evidence check.)
fn check_async_context(
    body: &AstStmt,
    owner: &str,
    owner_is_async: bool,
    diags: &mut AsyncDiagnostics,
) {
    // The owner's async modifier seeds the walk. Body self-evidence (async
    // owner with no suspension point) is reported by the callers: methods via
    // reconcile_marker, functions inline in check_unit (§4.2 — same rule).
    check_stmt_async_ctx(body, owner, owner_is_async, diags);
}

/// Walk statements tracking whether we are inside an async context. Entering
/// a plain `^{…}` block literal flips async OFF (a plain block never inherits
/// the enclosing async context — §5.3-1); entering an `async ^…` block flips
/// it ON.
fn check_stmt_async_ctx(s: &AstStmt, owner: &str, in_async: bool, diags: &mut AsyncDiagnostics) {
    match &s.data {
        AstStmtData::Expr(e) => check_expr_async_ctx(e, owner, in_async, diags),
        AstStmtData::Return(Some(e)) => check_expr_async_ctx(e, owner, in_async, diags),
        AstStmtData::Decl(d) => check_decl_async_ctx(d, owner, in_async, diags),
        AstStmtData::If { cond, then, else_, .. } => {
            check_expr_async_ctx(cond, owner, in_async, diags);
            check_stmt_async_ctx(then, owner, in_async, diags);
            if let Some(e) = else_ { check_stmt_async_ctx(e, owner, in_async, diags); }
        }
        AstStmtData::While { cond, body, .. } => {
            check_expr_async_ctx(cond, owner, in_async, diags);
            check_stmt_async_ctx(body, owner, in_async, diags);
        }
        AstStmtData::Do { cond, body, .. } => {
            check_expr_async_ctx(cond, owner, in_async, diags);
            check_stmt_async_ctx(body, owner, in_async, diags);
        }
        AstStmtData::For { init, cond, body, .. } => {
            if let Some(i) = init { check_stmt_async_ctx(i, owner, in_async, diags); }
            if let Some(c) = cond { check_expr_async_ctx(c, owner, in_async, diags); }
            check_stmt_async_ctx(body, owner, in_async, diags);
        }
        AstStmtData::Switch { expr, body, .. } => {
            check_expr_async_ctx(expr, owner, in_async, diags);
            check_stmt_async_ctx(body, owner, in_async, diags);
        }
        AstStmtData::Case { value, body, .. } => {
            check_expr_async_ctx(value, owner, in_async, diags);
            check_stmt_async_ctx(body, owner, in_async, diags);
        }
        AstStmtData::Default(body) => check_stmt_async_ctx(body, owner, in_async, diags),
        AstStmtData::Try { try_block, catches, finally_block } => {
            check_stmt_async_ctx(try_block, owner, in_async, diags);
            for c in catches { check_stmt_async_ctx(c, owner, in_async, diags); }
            if let Some(f) = finally_block { check_stmt_async_ctx(f, owner, in_async, diags); }
        }
        AstStmtData::Compound(stmts) => {
            for st in stmts { check_stmt_async_ctx(st, owner, in_async, diags); }
        }
        AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            check_stmt_async_ctx(body, owner, in_async, diags);
        }
        AstStmtData::Synchronized { lock, body, .. } => {
            check_expr_async_ctx(lock, owner, in_async, diags);
            check_stmt_async_ctx(body, owner, in_async, diags);
        }
        _ => {}
    }
}

fn check_decl_async_ctx(d: &AstDecl, owner: &str, in_async: bool, diags: &mut AsyncDiagnostics) {
    match &d.data {
        AstDeclData::Variable { init, next, .. } => {
            if let Some(e) = init { check_expr_async_ctx(e, owner, in_async, diags); }
            if let Some(n) = next { check_decl_async_ctx(n, owner, in_async, diags); }
        }
        _ => {}
    }
}

fn check_expr_async_ctx(e: &AstExpr, owner: &str, in_async: bool, diags: &mut AsyncDiagnostics) {
    match &e.data {
        // The suspension operator itself: legal iff we are inside an async
        // context (§5.3-1). Plain-block bodies flip `in_async` off below, so
        // an await there lands on the error arm.
        AstExprData::Await { inner, .. } => {
            if !in_async {
                diags.errors.push(format!(
                    "{}:{}: 'await' outside an async context — await is legal only in an async method/function body or an 'async ^{{…}}' block body (§5.3-1)",
                    e.line, e.col));
            }
            check_expr_async_ctx(inner, owner, in_async, diags);
        }
        // Block literals manage the async context themselves.
        AstExprData::Block { body, is_async, .. } => {
            if let Some(b) = body {
                check_stmt_async_ctx(b, owner, *is_async, diags);
            }
        }
        AstExprData::MsgSend { receiver, args, .. } => {
            check_expr_async_ctx(receiver, owner, in_async, diags);
            for a in args { check_expr_async_ctx(a, owner, in_async, diags); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { check_expr_async_ctx(c, owner, in_async, diags); }
            for a in args { check_expr_async_ctx(a, owner, in_async, diags); }
        }
        AstExprData::Binary { left, right, .. }
        | AstExprData::Assign { target: left, value: right } => {
            check_expr_async_ctx(left, owner, in_async, diags);
            check_expr_async_ctx(right, owner, in_async, diags);
        }
        AstExprData::Unary { operand, .. } => check_expr_async_ctx(operand, owner, in_async, diags),
        AstExprData::PropRef { obj, .. } | AstExprData::IvarRef { obj, .. } => {
            check_expr_async_ctx(obj, owner, in_async, diags);
        }
        AstExprData::Subscript { object, key } => {
            check_expr_async_ctx(object, owner, in_async, diags);
            check_expr_async_ctx(key, owner, in_async, diags);
        }
        AstExprData::Paren(inner) => check_expr_async_ctx(inner, owner, in_async, diags),
        AstExprData::Cast { expr, .. } => check_expr_async_ctx(expr, owner, in_async, diags),
        AstExprData::Ternary { cond, then, else_ } => {
            check_expr_async_ctx(cond, owner, in_async, diags);
            check_expr_async_ctx(then, owner, in_async, diags);
            check_expr_async_ctx(else_, owner, in_async, diags);
        }
        AstExprData::Comma(exprs) => {
            for x in exprs { check_expr_async_ctx(x, owner, in_async, diags); }
        }
        _ => {}
    }
}

/// §6: `(async NPTask<T>)` legacy spelling detection. Under the new model the
/// type after `async` is the BUSINESS type; keeping the old spelling would
/// silently make the task handle itself the business result. Error, with the
/// migration spelling spelled out.
fn check_async_nptask_spelling(rt: &AstType, line: usize, col: usize, diags: &mut AsyncDiagnostics) {
    if rt.name.as_deref() == Some("NPTask") {
        diags.errors.push(format!(
            "{}: {}: legacy spelling 'async NPTask<…>' is not valid under the new async model — the type after 'async' is the BUSINESS result type; write '(async T)' (e.g. '(async User *)'), the task handle is implied",
            line, col));
    }
}

/// Reconcile the `async` return-type modifier against the body's awaits
/// (doc/async_nptask_plan.md, stage B). The modifier is part of the
/// signature, so a lying signature is an error in BOTH directions:
///   marked + await     → ok
///   marked, no await   → ok — Async Semantics v1 rule 1: `async` means
///                        "MAY suspend" (a capability), not "must suspend";
///                        cache hits and sync protocol impls are normal
///                        (doc/plans/async_semantics_v1.md §2-1)
///   await, unmarked    → error — the signature is lying: a suspending method
///                        MUST be declared `(async T)`
///   unmarked, no await → ok
fn reconcile_marker(
    marked: bool,
    suspends: bool,
    real_await: bool,
    sym: &str,
    line: usize,
    col: usize,
    diags: &mut AsyncDiagnostics,
) {
    // `suspends` is no longer consulted for the marked case (v1 rule 1);
    // the parameter stays because the statement-position auto-drive pass
    // still distinguishes it until the Swift-model switch (stage 2).
    let _ = suspends;
    if !marked && real_await {
        diags.errors.push(format!(
            "{}:{}: '{}' contains 'await' but is not declared 'async' — a suspending method must declare the async modifier",
            line, col, sym));
    }
}



/// Reject `@try` containing an `await` anywhere in its try/catch/finally.
fn check_stmt_for_try_await(s: &AstStmt, owner: &str, diags: &mut AsyncDiagnostics) {
    match &s.data {
        AstStmtData::Try { try_block, catches, finally_block } => {
            if stmt_contains_await(try_block)
                || catches.iter().any(stmt_contains_await)
                || finally_block.as_ref().map_or(false, |f| stmt_contains_await(f))
            {
                diags.errors.push(format!(
                    "{}: @try spanning an await is not supported yet: \
                     a jmp_buf cannot survive a suspension point — move the \
                     @try outside the loop/segment or drop the awaits inside it",
                    owner
                ));
            }
            check_stmt_for_try_await(try_block, owner, diags);
        }
        AstStmtData::Compound(stmts) => {
            for st in stmts { check_stmt_for_try_await(st, owner, diags); }
        }
        AstStmtData::If { then, else_, .. } => {
            check_stmt_for_try_await(then, owner, diags);
            if let Some(e) = else_ { check_stmt_for_try_await(e, owner, diags); }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
        | AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            check_stmt_for_try_await(body, owner, diags);
        }
        AstStmtData::For { init, body, .. } => {
            if let Some(i) = init { check_stmt_for_try_await(i, owner, diags); }
            check_stmt_for_try_await(body, owner, diags);
        }
        AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. } => {
            check_stmt_for_try_await(body, owner, diags);
        }
        AstStmtData::Default(body) => check_stmt_for_try_await(body, owner, diags),
        _ => {}
    }
}

// (Milestone 1 task-wrapper desugar REMOVED — Async Semantics v1 rules 2/3
// made it unreachable: bare async calls are checker errors, statement
// positions no longer auto-drive, and the only sync→async entry is blockOn,
// which lowers through the M2/block-thunk machinery instead.)

/// `[task start]` on an NPTask handle → `jeti_task_start(task)`. The handle
/// is a runtime struct, not an NPObject — a vtable message send would be
/// undefined (doc/async_nptask_plan.md, stage D).
fn is_task_start_send(e: &AstExpr) -> bool {
    if let AstExprData::MsgSend { receiver, args, .. } = &e.data {
        if args.is_empty() {
            if let Some(t) = &receiver.expr_type {
                if t.is_pointer && t.name.as_deref().map_or(false, |n| n == "NPTask" || n.starts_with("NPTask<")) {
                    return true;
                }
            }
        }
    }
    false
}

/// `drive` = this expression sits at STATEMENT position: a `[t start]` there
/// becomes an enqueue (`jeti_task_start`) — Async Semantics v1 removed the
/// hosted statement-position blocking drive (rule 2: `await` is the only
/// blocking form; the checker rejects bare task-yielding expressions, so the
/// old "enqueue vs drive by call site" split has nothing left to drive).
/// The parameter stays for the rewrite shape: statement position enqueues,
/// every other position is a no-op for the start send.
fn lower_task_starts_expr(e: &mut AstExpr, ctx: &AsyncCtx) {
    if is_task_start_send(e) || {
        // `[t start]` with t a known task var (desugar precedes the checker,
        // so expr_type is unannotated — decide from the collected context).
        matches!(&e.data, AstExprData::MsgSend { receiver, .. }
            if matches!(&receiver.data, AstExprData::VarRef { name, .. }
                if ctx.task_vars.contains(name)))
    } {
        let line = e.line;
        let col = e.col;
        let taken = std::mem::replace(e, int_expr(0, line, col));
        let AstExprData::MsgSend { receiver, .. } = taken.data else { unreachable!() };
        let start_fn = "jeti_task_start";
        *e = AstExpr {
            kind: AstExprKind::FuncCall, expr_type: None, line, col,
            data: AstExprData::FuncCall {
                func: None, name: start_fn.to_string(), callee: None,
                args: vec![*receiver],
            },
            end_line: 0,
            end_col: 0,
        };
        return;
    }
    match &mut e.data {
        AstExprData::MsgSend { receiver, args, .. } => {
            lower_task_starts_expr(receiver, ctx);
            for a in args.iter_mut() { lower_task_starts_expr(a, ctx); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { lower_task_starts_expr(c, ctx); }
            for a in args.iter_mut() { lower_task_starts_expr(a, ctx); }
        }
        AstExprData::Unary { operand, .. } => lower_task_starts_expr(operand, ctx),
        AstExprData::Binary { left, right, .. } => {
            lower_task_starts_expr(left, ctx);
            lower_task_starts_expr(right, ctx);
        }
        AstExprData::Assign { target, value } => {
            lower_task_starts_expr(target, ctx);
            lower_task_starts_expr(value, ctx);
        }
        AstExprData::Comma(parts) => { for p in parts.iter_mut() { lower_task_starts_expr(p, ctx); } }
        AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => lower_task_starts_expr(inner, ctx),
        _ => {}
    }
}

fn lower_task_starts_stmt(s: &mut AstStmt, ctx: &mut AsyncCtx) {
    match &mut s.data {
        AstStmtData::Expr(e) => lower_task_starts_expr(e, ctx),
        AstStmtData::Return(Some(e)) => lower_task_starts_expr(e, ctx),
        AstStmtData::If { cond, then, else_ } => {
            lower_task_starts_expr(cond, ctx);
            lower_task_starts_stmt(then, ctx);
            if let Some(e) = else_ { lower_task_starts_stmt(e, ctx); }
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            lower_task_starts_expr(cond, ctx);
            lower_task_starts_stmt(body, ctx);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(i) = init { lower_task_starts_stmt(i, ctx); }
            if let Some(c) = cond { lower_task_starts_expr(c, ctx); }
            if let Some(u) = incr { lower_task_starts_expr(u, ctx); }
            lower_task_starts_stmt(body, ctx);
        }
        AstStmtData::Compound(stmts) => { for st in stmts.iter_mut() { lower_task_starts_stmt(st, ctx); } }
        AstStmtData::Decl(d) => lower_task_starts_decl(d, ctx),
        // §5.1: a statement-position task-yielding call inside a switch case
        // body auto-drives too (an async method's entry body is wrapped in a
        // state-machine switch by the time this runs).
        AstStmtData::Switch { expr, body } => {
            lower_task_starts_expr(expr, ctx);
            lower_task_starts_stmt(body, ctx);
        }
        AstStmtData::Case { value, body } => {
            lower_task_starts_expr(value, ctx);
            lower_task_starts_stmt(body, ctx);
        }
        AstStmtData::Default(body) => lower_task_starts_stmt(body, ctx),
        _ => {}
    }
}

fn lower_task_starts_decl(d: &mut AstDecl, ctx: &mut AsyncCtx) {
    match &mut d.data {
        AstDeclData::Variable { var_type, init, next, .. } => {
            // Collect task-handle variables unit-wide: any variable whose
            // declared type is NPTask* (source order — later `[t start]`
            // sends in any body, main included, can then match).
            if let Some(vt) = var_type.as_deref() {
                if vt.is_pointer && vt.name.as_deref().map_or(false, |n| n == "NPTask" || n.starts_with("NPTask<")) {
                    if let Some(n) = &d.name { ctx.task_vars.insert(n.clone()); }
                }
            }
            // A variable initialiser is not statement position: keep enqueue.
            if let Some(i) = init { lower_task_starts_expr(i, ctx); }
            if let Some(n) = next { lower_task_starts_decl(n, ctx); }
        }
        AstDeclData::Function { body: Some(b), .. } => lower_task_starts_stmt(b, ctx),
        AstDeclData::Class { methods, .. } => {
            for m in methods.iter_mut() {
                if let AstDeclData::Method { body: Some(b), .. } = &mut m.data {
                    lower_task_starts_stmt(b, ctx);
                }
            }
        }
        _ => {}
    }
}

// ─── Milestone 2: real state machine (frame + entry fn + driver) ────────────

use jeti_ast::{AstDeclKind, AstStmtKind, AstType};
use jeti_cst::{TagKind, TypePrim};

/// Named struct field type for the frame: `struct <Method>_frame`.
fn frame_type_name(symbol: &str) -> String {
    format!("{}_frame", sanitize_symbol(symbol))
}

fn sanitize_symbol(sym: &str) -> String {
    sym.chars().map(|c| match c {
        ':' => '_', ' ' => '_', '<' => '_', '>' => '_', '*' => 'p', ',' => '_',
        '(' | ')' => '_',
        other => other,
    }).collect()
}

/// Build the M2 state-machine form for one async method:
///   - a `struct <M>_frame { params + locals }` typedef (top-level decl)
///   - a `static int <M>_state(NPTask *t)` entry (top-level decl)
///   - rewrite the method body to: create + fill params + join + return result
///
/// In M2's synchronous-drive model the entry runs all states in one resume
/// (each awaited expression evaluates synchronously inside its state), so
/// observable behavior matches M1 — but the frame carries the params/locals
/// and the entry is externally drivable (M3's scheduler can call resume).
fn desugar_method_m2(
    method_symbol: &str,
    params: &[(String, AstType)],
    body: &mut AstStmt,
    top_decls: &mut Vec<AstDecl>,
    return_type: Option<&AstType>,
    async_ctx: &mut AsyncCtx,
    self_source: &str,
    self_cls: Option<&str>,
) {
    // 1. Frame carries ONLY the params in M2: the synchronous-drive model
    // never suspends between a local's declaration and its last use, so
    // locals live on the C stack inside the entry. (M3 lifts locals that
    // actually cross an await.)
    let mut frame_fields: Vec<(String, AstType)> = Vec::new();
    for (n, t) in params {
        if !frame_fields.iter().any(|(name, _)| name == n) {
            frame_fields.push((n.clone(), t.clone()));
        }
    }
    // Zero-param methods get an empty frame struct; codegen skips empty
    // struct bodies, leaving `sizeof(struct …_frame)` on an incomplete type.
    // Pad so the frame is always a complete type.
    if frame_fields.is_empty() {
        frame_fields.push(("__jeti_pad".to_string(), AstType::new(TypePrim::Char)));
    }

    let ftname = frame_type_name(method_symbol);
    let flat = sanitize_symbol(method_symbol);
    let entry_name = format!("jeti_async_state_{}", flat);

    // 2. Frame typedef decl.
    // Fields use the Ivar variant: codegen's Struct branch reads
    // AstDeclData::Ivar.ivar_type for field types (Variable is skipped).
    let fields_decls: Vec<AstDecl> = frame_fields.iter().map(|(n, t)| AstDecl {
        kind: AstDeclKind::Ivar,
        name: Some(n.clone()),
        line: body.line, col: body.col,
        data: AstDeclData::Ivar {
            ivar_sym: None,
            ivar_type: Some(Box::new(t.clone())),
            is_weak: false,
        },
        attributes: Vec::new(),
        end_line: body.end_line,
        end_col: body.end_col,
    }).collect();
    let frame_decl = AstDecl {
        kind: AstDeclKind::Struct,
        name: Some(ftname.clone()),
        line: body.line, col: body.col,
        data: AstDeclData::Aggregate { fields: fields_decls, is_union: false },
        attributes: Vec::new(),
        end_line: body.end_line,
        end_col: body.end_col,
    };
    top_decls.push(frame_decl);

    // 3. Rewrite `await e` → state-transition + synchronous eval:
    //    ((t)->state = N + 1, (e)) with N from a per-method counter.
    //    Each Await also marks its enclosing Decl to persist into the frame —
    //    in M2 the frame is the single source of truth for locals: rewrite all
    //    local VarRefs/Decls to frame access.
    //    Simplification (M2): locals are read/written through `f->` only for
    //    params (needed by the entry across resumes). Body-internal locals
    //    stay on the C stack: within one synchronous resume there is no
    //    suspension between their declaration and last use, so the frame only
    //    needs params. awaited call results flow through expressions directly.
    //    (M3 lifts cross-await locals when real suspension exists.)
    let _ = &frame_fields;

    // 3b. Lower awaits with a state counter: each await bumps the counter so
    // the state machine has one state per suspension point (all running
    // synchronously in M2's drive model).
    let mut state_counter: i32 = 1;
    let mut ctx = std::mem::take(async_ctx);
    lower_awaits_m2(body, &entry_name, &mut state_counter, &mut ctx);
    *async_ctx = ctx;
    // Final state number (the completion state).
    let final_state = state_counter;

    // 4. Entry function:
    //   static int jeti_async_state_<M>(NPTask *__jeti_task) {
    //     struct <M>_frame *__jeti_f = (struct <M>_frame *)__jeti_task->frame;
    //     switch (__jeti_task->state) {
    //       case 1: ...body with lowered awaits...
    //       case <final>: __jeti_task->state = -1; return 1;
    //     }
    //     return 1;
    //   }
    // The body runs in state 1 (one synchronous pass). States 2..final-1 are
    // the per-await re-entry points; in M2's drive model resume() loops until
    // finished, and since the body completes within state 1, later states are
    // only entered if a real suspension (M3) yields first.
    let orig_stmt = std::mem::replace(body, AstStmt {
        kind: AstStmtKind::Compound, line: body.line, col: body.col,
        data: AstStmtData::Compound(Vec::new()),
        end_line: body.end_line,
        end_col: body.end_col,
    });
    let (o_kind, o_line, o_col, body_stmts) = match orig_stmt.data {
        AstStmtData::Compound(inner) => (orig_stmt.kind, orig_stmt.line, orig_stmt.col, inner),
        other => (orig_stmt.kind, orig_stmt.line, orig_stmt.col, vec![AstStmt { kind: orig_stmt.kind, line: orig_stmt.line, col: orig_stmt.col, data: other, end_line: orig_stmt.end_line, end_col: orig_stmt.end_col }]),
    };
    let (body_kind, body_line, body_col) = (o_kind, o_line, o_col);
    let _ = (body_kind, body_line, body_col);

    // Build the switch: case 1 = body; case final = finish.
    // The entry can only see the task: rewrite param references to
    // `__jeti_f->param` (the frame) and `self` to `__jeti_task->self_obj`,
    // then rewrite returns to
    // the completion protocol (result/state/return 1).
    let mut case_body_stmts = body_stmts;
    // Only params are rewritten to `__jeti_f->param` (the frame carries them
    // across the entry boundary). Body-internal locals stay on the C stack:
    // in M2's synchronous-drive model there is no suspension between their
    // declaration and last use, so rewriting their reads to the (never
    // written) frame fields would read garbage.
    let params_only: Vec<(String, AstType)> = params.to_vec();
    for stmt in case_body_stmts.iter_mut() {
        rewrite_entry_refs_stmt(stmt, &params_only, self_cls);
    }
    // §9 value typing for the RETURN path: the AST carries an async method's
    // return type as the task handle `NPTask<T> *`, so the business type is the
    // first type argument (a block thunk passes its business type directly).
    let business_ty = match return_type {
        Some(t) if t.is_pointer && t.name.as_deref() == Some("NPTask") => {
            t.type_args.first()
        }
        other => other,
    };
    rewrite_returns_to_result(&mut case_body_stmts, business_ty.map_or(false, type_is_object));
    // struct <FT> *__jeti_f = (struct <FT> *)__jeti_task->frame;  (case 1 head)
    let f_decl = AstStmt {
        kind: AstStmtKind::Decl, line: body.line, col: body.col,
        data: AstStmtData::Decl(AstDecl {
            kind: AstDeclKind::Variable,
            name: Some("__jeti_f".to_string()),
            line: body.line, col: body.col,
            data: AstDeclData::Variable {
                var_type: Some(Box::new({
                    let mut st = AstType::new(TypePrim::Named);
                    st.name = Some(ftname.clone());
                    st.is_struct = true;
                    let mut t = AstType::new(TypePrim::Named);
                    t.is_pointer = true;
                    t.subtype = Some(Box::new(st));
                    t
                })),
                init: Some(Box::new(AstExpr {
                    kind: AstExprKind::Cast, expr_type: None, line: body.line, col: body.col,
                    data: AstExprData::Cast {
                        target_type: {
                            let mut st = AstType::new(TypePrim::Named);
                            st.name = Some(ftname.clone());
                            st.is_struct = true;
                            let mut t = AstType::new(TypePrim::Named);
                            t.is_pointer = true;
                            t.subtype = Some(Box::new(st));
                            t
                        },
                        expr: Box::new(member_expr("__jeti_task", "frame", body.line, body.col)),
                    },
                    end_line: body.end_line,
                    end_col: body.end_col,
                })),
                is_static: false, is_extern: false, is_const: false,
                is_block_qual: false, is_weak: false, next: None,
            },
            attributes: Vec::new(),
            end_line: body.end_line,
            end_col: body.end_col,
        }),
        end_line: body.end_line,
        end_col: body.end_col,
    };
    case_body_stmts.insert(0, f_decl);
    // Append: __jeti_task->state = -1; return 1;
    case_body_stmts.push(AstStmt {
        kind: AstStmtKind::Expr, line: body.line, col: body.col,
        data: AstStmtData::Expr(AstExpr {
            kind: AstExprKind::Assign, expr_type: None, line: body.line, col: body.col,
            data: AstExprData::Assign {
                target: Box::new(member_expr("__jeti_task", "state", body.line, body.col)),
                value: Box::new(int_expr(-1, body.line, body.col)),
            },
            end_line: body.end_line,
            end_col: body.end_col,
        }),
        end_line: body.end_line,
        end_col: body.end_col,
    });
    case_body_stmts.push(AstStmt {
        kind: AstStmtKind::Return, line: body.line, col: body.col,
        data: AstStmtData::Return(Some(Box::new(int_expr(1, body.line, body.col)))),
        end_line: body.end_line,
        end_col: body.end_col,
    });

    let case1 = AstStmt {
        kind: AstStmtKind::Case, line: body.line, col: body.col,
        data: AstStmtData::Case {
            value: Box::new(int_expr(1, body.line, body.col)),
            body: Box::new(AstStmt {
                kind: AstStmtKind::Compound, line: body.line, col: body.col,
                data: AstStmtData::Compound(case_body_stmts),
                end_line: body.end_line,
                end_col: body.end_col,
            }),
        },
        end_line: body.end_line,
        end_col: body.end_col,
    };
    let case_final = AstStmt {
        kind: AstStmtKind::Case, line: body.line, col: body.col,
        data: AstStmtData::Case {
            value: Box::new(int_expr(final_state.max(2) as i64, body.line, body.col)),
            body: Box::new(AstStmt {
                kind: AstStmtKind::Compound, line: body.line, col: body.col,
                data: AstStmtData::Compound(vec![
                    AstStmt {
                        kind: AstStmtKind::Expr, line: body.line, col: body.col,
                        data: AstStmtData::Expr(AstExpr {
                            kind: AstExprKind::Assign, expr_type: None, line: body.line, col: body.col,
                            data: AstExprData::Assign {
                                target: Box::new(member_expr("__jeti_task", "state", body.line, body.col)),
                                value: Box::new(int_expr(-1, body.line, body.col)),
                            },
                            end_line: body.end_line,
                            end_col: body.end_col,
                        }),
                        end_line: body.end_line,
                        end_col: body.end_col,
                    },
                    AstStmt {
                        kind: AstStmtKind::Return, line: body.line, col: body.col,
                        data: AstStmtData::Return(Some(Box::new(int_expr(1, body.line, body.col)))),
                        end_line: body.end_line,
                        end_col: body.end_col,
                    },
                ]),
                end_line: body.end_line,
                end_col: body.end_col,
            }),
        },
        end_line: body.end_line,
        end_col: body.end_col,
    };
    let switch_stmt = AstStmt {
        kind: AstStmtKind::Switch, line: body.line, col: body.col,
        data: AstStmtData::Switch {
            expr: Box::new(member_expr("__jeti_task", "state", body.line, body.col)),
            body: Box::new(AstStmt {
                kind: AstStmtKind::Compound, line: body.line, col: body.col,
                data: AstStmtData::Compound(vec![case1, case_final]),
                end_line: body.end_line,
                end_col: body.end_col,
            }),
        },
        end_line: body.end_line,
        end_col: body.end_col,
    };

    // Entry params: (NPTask *__jeti_task). The name is deliberately RESERVED:
    // a bare `t` collides with a same-named user local in the rewritten body —
    // `NPTask<int> *t = [self cached]; await t;` expanded `t->self_obj`
    // against the *uninitialised* new local (SIGSEGV at 0x18, lldb-confirmed).
    let entry_param = cst_param("NPTask", "__jeti_task");
    let entry_decl = AstDecl {
        kind: AstDeclKind::Function,
        name: Some(entry_name.clone()),
        line: body.line, col: body.col,
        end_line: body.end_line, end_col: body.end_col,
        data: AstDeclData::Function {
            func_sym: None,
            return_type: Some(Box::new(AstType::new(TypePrim::Int))),
            params: Some(Box::new(entry_param)),
            body: Some(Box::new(AstStmt {
                kind: AstStmtKind::Compound, line: body.line, col: body.col,
                end_line: body.end_line, end_col: body.end_col,
                data: AstStmtData::Compound(vec![
                    switch_stmt,
                    // Fallback: a state outside 1..=final (corrupt task, or a
                    // resume after completion) must not fall off the end of a
                    // non-void function — UB, clang -Wreturn-type (external
                    // review finding). Treat it as completion so the driver's
                    // join loop terminates instead of spinning.
                    AstStmt {
                        kind: AstStmtKind::Expr, line: body.line, col: body.col,
                        data: AstStmtData::Expr(AstExpr {
                            kind: AstExprKind::Assign, expr_type: None, line: body.line, col: body.col,
                            data: AstExprData::Assign {
                                target: Box::new(member_expr("__jeti_task", "state", body.line, body.col)),
                                value: Box::new(int_expr(-1, body.line, body.col)),
                            },
                            end_line: body.end_line,
                            end_col: body.end_col,
                        }),
                        end_line: body.end_line,
                        end_col: body.end_col,
                    },
                    AstStmt {
                        kind: AstStmtKind::Return, line: body.line, col: body.col,
                        data: AstStmtData::Return(Some(Box::new(int_expr(1, body.line, body.col)))),
                        end_line: body.end_line,
                        end_col: body.end_col,
                    },
                ]),
            })),
            has_variadic: false,
            throws: None,
            async_marker: false,
        },
        attributes: Vec::new(),
    };
    top_decls.push(entry_decl);

    // NPTask design (doc/async_nptask_plan.md, stage D): the driver is LAZY —
    // create the task, fill the frame params, and return the handle. No join:
    // execution starts at [task start] / await. The method's C return type is
    // NPTask * (codegen renders NPTask<T> as NPTask *).
    let create_call = AstExpr {
        kind: AstExprKind::FuncCall, expr_type: None, line: body.line, col: body.col,
        data: AstExprData::FuncCall {
            func: None, name: "jeti_task_create".to_string(), callee: None,
            args: vec![
                ident_expr(&entry_name, body.line, body.col),
                ident_expr("self", body.line, body.col),
                AstExpr {
                    kind: AstExprKind::FuncCall, expr_type: None, line: body.line, col: body.col,
                    data: AstExprData::FuncCall {
                        func: None, name: "__builtin_sizeof_frame".to_string(), callee: None, args: vec![],
                    },
                    end_line: body.end_line,
                    end_col: body.end_col,
                },
            ],
        },
        end_line: body.end_line,
        end_col: body.end_col,
    };
    let _ = create_call; // replaced below by sizeof-based version
    // sizeof expr: AST Sizeof { type_expr, expr: None } → codegen emits sizeof(type)
    let sizeof_frame = AstExpr {
        kind: AstExprKind::Sizeof, expr_type: None, line: body.line, col: body.col,
        data: AstExprData::Sizeof {
            type_expr: {
                let mut t = AstType::new(TypePrim::Named);
                t.name = Some(ftname.clone());
                t.is_struct = true;
                t
            },
            expr: None,
        },
        end_line: body.end_line,
        end_col: body.end_col,
    };
    let create_call = AstExpr {
        kind: AstExprKind::FuncCall, expr_type: None, line: body.line, col: body.col,
        data: AstExprData::FuncCall {
            func: None, name: "jeti_task_create".to_string(), callee: None,
            args: vec![
                ident_expr(&entry_name, body.line, body.col),
                ident_expr(self_source, body.line, body.col),
                sizeof_frame,
            ],
        },
        end_line: body.end_line,
        end_col: body.end_col,
    };
    // Driver: task create FIRST, then fill frame params, then join.
    // ((struct <M>_frame *)__jeti_task->frame)->param = param;
    let mut pre: Vec<AstStmt> = Vec::new();
    let task_decl = AstDecl {
        kind: AstDeclKind::Variable,
        name: Some("__jeti_task".to_string()),
        line: body.line, col: body.col,
        data: AstDeclData::Variable {
            var_type: Some(Box::new(jeti_task_ptr_type())),
            init: Some(Box::new(create_call)),
            is_static: false, is_extern: false, is_const: false,
            is_block_qual: false, is_weak: false, next: None,
        },
        attributes: Vec::new(),
        end_line: body.end_line,
        end_col: body.end_col,
    };
    pre.push(AstStmt { kind: AstStmtKind::Decl, line: body.line, col: body.col, data: AstStmtData::Decl(task_decl), end_line: body.end_line, end_col: body.end_col });
    for (pname, _) in params {
        // obj expr: (struct FT *)(__jeti_task->frame)
        let frame_access = AstExpr {
            kind: AstExprKind::Cast, expr_type: None, line: body.line, col: body.col,
            data: AstExprData::Cast {
                target_type: {
                    let mut st = AstType::new(TypePrim::Named);
                    st.name = Some(ftname.clone());
                    st.is_struct = true;
                    let mut t = AstType::new(TypePrim::Named);
                    t.is_pointer = true;
                    t.subtype = Some(Box::new(st));
                    t
                },
                expr: Box::new(member_expr("__jeti_task", "frame", body.line, body.col)),
            },
            end_line: body.end_line,
            end_col: body.end_col,
        };
        pre.push(AstStmt {
            kind: AstStmtKind::Expr, line: body.line, col: body.col,
            data: AstStmtData::Expr(AstExpr {
                kind: AstExprKind::Assign, expr_type: None, line: body.line, col: body.col,
                data: AstExprData::Assign {
                    target: Box::new(AstExpr {
                        kind: AstExprKind::IvarRef, expr_type: None, line: body.line, col: body.col,
                        data: AstExprData::IvarRef {
                            obj: Box::new(frame_access),
                            ivar: Some(pname.clone()), cls: None,
                        },
                        end_line: body.end_line,
                        end_col: body.end_col,
                    }),
                    value: Box::new(ident_expr(pname, body.line, body.col)),
                },
                end_line: body.end_line,
                end_col: body.end_col,
            }),
            end_line: body.end_line,
            end_col: body.end_col,
        });
    }

    // Join: void case → plain call; else result = (T)(unsigned long)join(t)
    // via a result variable the return statement references.
    let is_void = return_type.map_or(true, |t| t.prim == TypePrim::Void && !t.is_pointer);
    let mut post: Vec<AstStmt> = Vec::new();
    if is_void {
        post.push(call_stmt("jeti_task_join", vec![ident_expr("__jeti_task", body.line, body.col)], body.line, body.col));
    } else {
        // long __jeti_r = (long)jeti_task_join(t);
        post.push(AstStmt {
            kind: AstStmtKind::Decl, line: body.line, col: body.col,
            data: AstStmtData::Decl(AstDecl {
                kind: AstDeclKind::Variable,
                name: Some("__jeti_r".to_string()),
                line: body.line, col: body.col,
                data: AstDeclData::Variable {
                    var_type: Some(Box::new(AstType::new(TypePrim::Long))),
                    init: Some(Box::new(AstExpr {
                        kind: AstExprKind::Cast, expr_type: None, line: body.line, col: body.col,
                        data: AstExprData::Cast {
                            target_type: AstType::new(TypePrim::Long),
                            expr: Box::new(call_expr("jeti_task_join", vec![ident_expr("__jeti_task", body.line, body.col)], body.line, body.col)),
                        },
                        end_line: body.end_line,
                        end_col: body.end_col,
                    })),
                    is_static: false, is_extern: false, is_const: false,
                    is_block_qual: false, is_weak: false, next: None,
                },
                attributes: Vec::new(),
                end_line: body.end_line,
                end_col: body.end_col,
            }),
            end_line: body.end_line,
            end_col: body.end_col,
        });
    }
    // Lazy driver: no join — the body is just create + param fill + return
    // the handle. (The old eager driver's join/return-cast is gone; results
    // are read via jeti_task_await.)
    let _ = is_void;
    let mut out = pre;
    out.push(AstStmt {
        kind: AstStmtKind::Return, line: body.line, col: body.col,
        data: AstStmtData::Return(Some(Box::new(ident_expr("__jeti_task", body.line, body.col)))),
        end_line: body.end_line,
        end_col: body.end_col,
    });
    *body = AstStmt { kind: AstStmtKind::Compound, line: body.line, col: body.col, data: AstStmtData::Compound(out), end_line: body.end_line, end_col: body.end_col };
}

// ─── M2 helpers ──────────────────────────────────────────────────────────────

/// Lower awaits for the M2 entry: `await e` →
/// `(t->state = N, (e))` where N is the next state number. In M2's drive
/// model the state write is bookkeeping only (the body completes within one
/// resume); states still exist so M3 can insert real suspension points.
/// Desugar context: which selectors are async methods (colon-stripped) and
/// which local variables are known to hold task handles (populated during the
/// walk, in source order). Desugar runs BEFORE the checker, so expr_type is
/// not annotated — all "does this yield a task?" decisions come from here.
#[derive(Default)]
struct AsyncCtx {
    async_sels: std::collections::HashSet<String>,
    task_vars: std::collections::HashSet<String>,
    /// Hidden async-block thunk function names (§5 desugar): a call to one of
    /// these yields a task handle, so statement-position IIFEs drive like
    /// async method calls (drive_entry_call / expr_yields_task).
    async_block_fns: std::collections::HashSet<String>,
    /// §9 await-value typing: does the awaited call's BUSINESS result come
    /// back as an object/pointer (vs a scalar word)? The await lowering needs
    /// this to pick the value cast — `(long)` for scalars (the runtime stores
    /// them in the `void *` result slot as truncated words), NO cast for
    /// pointers (`void *` converts to `T *` implicitly). Before this table
    /// existed the cast was unconditional, so every object-returning await
    /// (`NPString *s = await [self make];`) was an int-conversion error.
    /// A selector/function/var missing from all three is treated as scalar
    /// (the historical behavior).
    sel_ret_obj: std::collections::HashMap<String, bool>,
    block_ret_obj: std::collections::HashMap<String, bool>,
    task_var_obj: std::collections::HashMap<String, bool>,
    /// §8-① elided-return inference (`async ^{ … }` with the return type
    /// omitted): the declared business return type of EVERY method — sync and
    /// async alike — keyed by colon-stripped selector. `sel_ret_obj` above only
    /// knows async ones, but a `return [obj msg];` inside an elided block is
    /// usually a synchronous call.
    sel_ret_obj_all: std::collections::HashMap<String, bool>,
    /// Same oracle for top-level functions (`return f();`).
    fn_ret_obj: std::collections::HashMap<String, bool>,
    /// Is the current body a method (does `self` exist in scope)? Set by the
    /// §5 block-desugar walk before each method/function body: an IIFE call
    /// site passes the live `self` inside methods, and `(NPObject *)0` in
    /// plain functions/main — a block body that actually uses `self` there
    /// is already a checker error, so the null is never dereferenced.
    in_method: bool,
    /// The enclosing class name (cls_sym) while walking method bodies — the
    /// typed-self cast target for the M2 entry (`((struct Cls *)
    /// __jeti_task->self_obj)`); None in plain functions/main, where a `self`
    /// reference is already a checker error.
    current_class: Option<String>,
    /// Diagnostics produced while inferring elided block return types; handed
    /// back by `desugar_unit_m2` and printed as async warnings by the driver.
    warnings: Vec<String>,
    /// Hard errors from the same inference: rule C — a value-carrying elided
    /// block whose `return` expressions mix an object and a scalar result
    /// never downgrades silently; the driver fails the unit on these.
    errors: Vec<String>,
}

/// Does this expression evaluate to an NPTask handle? (async call whose
/// selector is a known async method, a variable recorded as a task handle, or
/// a call to a hidden async-block thunk)
/// §9: object/pointer business result? Jeti instances are always `Type *` in
/// the generated C, so a pointer spelling is the test; `id`/`instancetype`
/// spellings are objects by construction, everything else is a scalar word.
fn type_is_object(t: &AstType) -> bool {
    t.is_pointer
        || matches!(t.prim, TypePrim::Id | TypePrim::Instancetype)
        || t.name.as_deref().map_or(false, |n| n == "id" || n == "instancetype")
}

/// §9: is the awaited call's business result an object? See `AsyncCtx`'s
/// `*_ret_obj` maps — an unknown target defaults to `false` (scalar cast).
fn awaited_result_is_object(e: &AstExpr, ctx: &AsyncCtx) -> bool {
    match &e.data {
        AstExprData::MsgSend { selector, .. } => ctx
            .sel_ret_obj
            .get(&selector.replace(':', ""))
            .copied()
            .unwrap_or(false),
        AstExprData::FuncCall { name, .. } => ctx
            .block_ret_obj
            .get(name)
            .copied()
            .unwrap_or(false),
        AstExprData::VarRef { name, .. } => ctx
            .task_var_obj
            .get(name)
            .copied()
            .unwrap_or(false),
        AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => {
            awaited_result_is_object(inner, ctx)
        }
        _ => false,
    }
}

/// §8-① (`docs/plans/async_soft_keywords_plan.md`): infer the return type of
/// an elided `async ^{ … }` block from its body. The rule, decided here:
///
/// * no value-returning `return` (none, or only bare `return;`) → `async void`
///   (`None`, the historical behavior: the block's result is never read);
/// * every value return is statically classifiable and they agree → that
///   class (an object instance, or a scalar word);
/// * otherwise → a scalar word, plus a warning telling the user to spell the
///   type explicitly. The inference is deliberately limited to what the AST
///   alone can prove (no type-checker here — the desugar runs first): a
///   declared selector/function result, a cast, or a literal. A bare variable
///   or an arithmetic expression is NOT classifiable, and guessing "scalar"
///   for an object-typed variable would emit `(long)` over a pointer.
fn elided_block_return_type(
    declared: Option<Box<AstType>>,
    body: Option<&AstStmt>,
    ctx: &mut AsyncCtx,
    line: usize,
    col: usize,
) -> Option<Box<AstType>> {
    if declared.is_some() { return declared; }
    let body = match body { Some(b) => b, None => return None };
    let mut rets: Vec<&AstExpr> = Vec::new();
    collect_elided_returns(body, &mut rets);
    if rets.is_empty() {
        // Nothing is returned: `async void` — no inference needed, no warning.
        return None;
    }
    let mut seen: Option<bool> = None;
    let mut ambiguous = false;
    for r in rets {
        match classify_elided_ret(r, ctx) {
            Some(o) => match seen {
                None => seen = Some(o),
                Some(prev) if prev != o => ambiguous = true,
                _ => {}
            },
            None => ambiguous = true,
        }
    }
    if ambiguous {
        // Rule C: all value-carrying returns in one elided block must have
        // compatible types — the inference never "picks one". A hard error,
        // not a downgrade: a silent object/scalar mix would truncate one
        // branch's result (an object stored as a word, or a scalar read as
        // a pointer).
        ctx.errors.push(format!(
            "{}:{}: incompatible return types in this 'async ^{{…}}' block: \
             its 'return' expressions mix an object and a scalar result — \
             declare the return type explicitly ('async ^T') or make every \
             branch return the same kind of value",
            line, col));
        return Some(Box::new(AstType::new(TypePrim::Long)));
    }
    if seen.is_none() {
        ctx.warnings.push(format!(
            "{}:{}: cannot infer the omitted return type of this 'async ^{{…}}' \
             block: its 'return' expressions are not statically classifiable \
             (only a declared selector/function result, a cast or a literal is) \
             — treating the result as a scalar word. Write the type explicitly \
             ('async ^T') if the block really returns an object",
            line, col));
        return Some(Box::new(AstType::new(TypePrim::Long)));
    }
    Some(Box::new(if seen == Some(true) {
        AstType::new(TypePrim::Id)
    } else {
        AstType::new(TypePrim::Long)
    }))
}

/// §8-①: every `return <expr>` that belongs to THIS block body. Nested block
/// literals (and any function-like construct) keep their own returns.
fn collect_elided_returns<'a>(s: &'a AstStmt, out: &mut Vec<&'a AstExpr>) {
    match &s.data {
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => out.push(e),
        AstStmtData::Compound(stmts) => {
            for st in stmts { collect_elided_returns(st, out); }
        }
        AstStmtData::If { then, else_, .. } => {
            collect_elided_returns(then, out);
            if let Some(e) = else_ { collect_elided_returns(e, out); }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. } => {
            collect_elided_returns(body, out);
        }
        AstStmtData::For { init, body, .. } => {
            if let Some(i) = init { collect_elided_returns(i, out); }
            collect_elided_returns(body, out);
        }
        AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. } => {
            collect_elided_returns(body, out);
        }
        AstStmtData::Default(body) => collect_elided_returns(body, out),
        AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            collect_elided_returns(body, out);
        }
        AstStmtData::Synchronized { body, .. } => collect_elided_returns(body, out),
        AstStmtData::Try { try_block, catches, finally_block } => {
            collect_elided_returns(try_block, out);
            for c in catches { collect_elided_returns(c, out); }
            if let Some(f) = finally_block { collect_elided_returns(f, out); }
        }
        _ => {}
    }
}

/// §8-①: can this `return <expr>` be classified from the AST alone? `Some(true)`
/// = an object instance (a pointer result), `Some(false)` = a scalar word,
/// `None` = not provable.
fn classify_elided_ret(e: &AstExpr, ctx: &AsyncCtx) -> Option<bool> {
    match &e.data {
        // A declared result: the method/function's business return type decides.
        AstExprData::MsgSend { selector, .. } => {
            ctx.sel_ret_obj_all.get(&selector.replace(':', "")).copied()
        }
        AstExprData::FuncCall { name, .. } => ctx.fn_ret_obj.get(name).copied(),
        // An awaited value has the awaited call's business result type.
        AstExprData::Await { inner, .. } => classify_elided_ret(inner, ctx),
        // An explicit cast IS the answer.
        AstExprData::Cast { target_type, .. } => Some(type_is_object(target_type)),
        AstExprData::Paren(inner) => classify_elided_ret(inner, ctx),
        // A string literal is an NPString instance.
        AstExprData::String(_) | AstExprData::AtString(_) => Some(true),
        AstExprData::Int(_) | AstExprData::Float(_) | AstExprData::FloatRaw(_)
        | AstExprData::Char(_) | AstExprData::Bool(_) => Some(false),
        // `nil` / `NULL` / `self` are object references (and `nil` stores
        // correctly as a scalar word too, so this never miscompiles).
        AstExprData::VarRef { name, .. } => match name.as_str() {
            "nil" | "NULL" | "self" => Some(true),
            // A plain variable: not classifiable without the type checker.
            _ => None,
        },
        _ => None,
    }
}

fn expr_yields_task(e: &AstExpr, ctx: &AsyncCtx) -> bool {
    match &e.data {
        AstExprData::VarRef { name, .. } => ctx.task_vars.contains(name),
        AstExprData::MsgSend { selector, .. } => {
            ctx.async_sels.contains(&selector.replace(':', ""))
        }
        AstExprData::FuncCall { name, .. } => ctx.async_block_fns.contains(name),
        AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => {
            expr_yields_task(inner, ctx)
        }
        // `cond ? [self a:x] : [self b:y]` yields a task only when BOTH
        // branches do — that is also when the branches have a consistent
        // handle type (mixed arithmetic branches are the checker's
        // ternary-compat error, not a drive site). A null-pointer-constant
        // branch (`: 0` / `(id)0`) also drives: jeti_task_await(NULL)
        // returns NULL, so the await stays total — without this the checker
        // would accept the ternary (legal C) while the handle fed straight
        // into a scalar.
        AstExprData::Ternary { then, else_, .. } => {
            let nc = |x: &AstExpr| match &x.data {
                AstExprData::Int(0) => true,
                AstExprData::Cast { expr, .. } => matches!(&expr.data, AstExprData::Int(0)),
                _ => false,
            };
            (expr_yields_task(then, ctx) && expr_yields_task(else_, ctx))
                || (expr_yields_task(then, ctx) && nc(else_))
                || (nc(then) && expr_yields_task(else_, ctx))
        }
        _ => false,
    }
}

fn lower_awaits_m2(s: &mut AstStmt, entry: &str, state: &mut i32, ctx: &mut AsyncCtx) {
    match &mut s.data {
        AstStmtData::Expr(e) => lower_awaits_m2_expr(e, state, ctx),
        AstStmtData::Compound(stmts) => { for st in stmts { lower_awaits_m2(st, entry, state, ctx); } }
        AstStmtData::If { cond, then, else_ } => {
            lower_awaits_m2_expr(cond, state, ctx);
            lower_awaits_m2(then, entry, state, ctx);
            if let Some(e) = else_ { lower_awaits_m2(e, entry, state, ctx); }
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            lower_awaits_m2_expr(cond, state, ctx);
            lower_awaits_m2(body, entry, state, ctx);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(i) = init { lower_awaits_m2(i, entry, state, ctx); }
            if let Some(c) = cond { lower_awaits_m2_expr(c, state, ctx); }
            if let Some(u) = incr { lower_awaits_m2_expr(u, state, ctx); }
            lower_awaits_m2(body, entry, state, ctx);
        }
        AstStmtData::Return(Some(e)) => lower_awaits_m2_expr(e, state, ctx),
        AstStmtData::Decl(d) => lower_awaits_m2_decl(d, state, ctx),
        _ => {}
    }
    let _ = entry;
}

fn lower_awaits_m2_decl(d: &mut AstDecl, state: &mut i32, ctx: &mut AsyncCtx) {
    if let AstDeclData::Variable { var_type, init, next, .. } = &mut d.data {
        // A variable DECLARED as an NPTask handle is a task var for the rest of
        // the walk regardless of its initializer: a sync method may hand back a
        // task object (`NPTask<int> *t = [self loadCachedValue];`), and the
        // design allows `await` on any `NPTask<T> *` expression — a variable,
        // ivar or return value — not only on the result of an async call
        // (doc/async_nptask_plan.md §调用点语义). The initializer-shape probe
        // below cannot see this: its selector is not an async method.
        if let Some(vt) = var_type.as_deref() {
            if vt.is_pointer
                && vt.name.as_deref().map_or(false, |n| n == "NPTask" || n.starts_with("NPTask<"))
            {
                if let Some(n) = &d.name {
                    ctx.task_vars.insert(n.clone());
                    // §9: `NPTask<T> *` — T decides the await-value cast.
                    ctx.task_var_obj.insert(
                        n.clone(),
                        vt.type_args.first().map_or(false, type_is_object),
                    );
                }
            }
        }
        if let Some(i) = init {
            lower_awaits_m2_expr(i, state, ctx);
            // A variable initialized from a task-yielding expression becomes
            // a task handle for the rest of the walk (source order).
            if expr_yields_task(i, ctx) {
                if let Some(n) = &d.name {
                    ctx.task_vars.insert(n.clone());
                    let obj = awaited_result_is_object(i, ctx);
                    ctx.task_var_obj.insert(n.clone(), obj);
                }
            }
        }
        if let Some(n) = next { lower_awaits_m2_decl(n, state, ctx); }
    }
}

fn lower_awaits_m2_expr(e: &mut AstExpr, state: &mut i32, ctx: &mut AsyncCtx) {
    let line = e.line;
    let col = e.col;
    match &mut e.data {
        AstExprData::Await { inner } => {
            lower_awaits_m2_expr(inner, state, ctx);
            *state += 1;
            let n = *state;
            // (t->state = N, (long)jeti_task_await(e))
            // NPTask design (stage D): async calls are LAZY — the awaited
            // expression evaluates to an NPTask handle, and the await must
            // drive it (jeti_task_await blocks/drives inline, caches the
            // result) instead of synchronously evaluating the handle.
            let state_assign = AstExpr {
                kind: AstExprKind::Assign, expr_type: None, line, col,
                data: AstExprData::Assign {
                    target: Box::new(member_expr("__jeti_task", "state", line, col)),
                    value: Box::new(int_expr(n as i64, line, col)),
                },
                end_line: 0,
                end_col: 0,
            };
            let taken = std::mem::replace(&mut **inner, int_expr(0, line, col));
            // Only wrap when the awaited expression yields a task handle.
            // NOTE: desugar runs BEFORE the checker, so expr_type is not
            // annotated yet — decide from the AST shape: a message send whose
            // selector names an async method, or a variable known to hold a
            // task handle (both collected unit-wide in desugar_unit_m2).
            let yields_task = expr_yields_task(&taken, ctx);
            // §9 value typing — must be read BEFORE `taken` moves into the
            // await call below.
            let result_is_object = awaited_result_is_object(&taken, ctx);
            if !yields_task {
                *e = AstExpr {
                    kind: AstExprKind::Comma, expr_type: None, line, col,
                    data: AstExprData::Comma(vec![state_assign, taken]),
                    end_line: 0,
                    end_col: 0,
                };
                return;
            }
            let await_call = AstExpr {
                kind: AstExprKind::FuncCall, expr_type: None, line, col,
                data: AstExprData::FuncCall {
                    func: None, name: "jeti_task_await".to_string(), callee: None,
                    args: vec![taken],
                },
                end_line: 0,
                end_col: 0,
            };
            // §9 value typing: the runtime carries every result in one
            // `void *` slot. A scalar business result must be cast back to a
            // word (`(long)` — the store truncated it), while an object
            // result must NOT be cast: `void *` converts to `T *` implicitly
            // and an unconditional `(long)` made every object-returning await
            // an int-conversion error (`NPString *s = await [self make];`).
            let value_expr = if result_is_object {
                await_call
            } else {
                AstExpr {
                    kind: AstExprKind::Cast, expr_type: None, line, col,
                    data: AstExprData::Cast {
                        target_type: AstType::new(TypePrim::Long),
                        expr: Box::new(await_call),
                    },
                    end_line: 0,
                    end_col: 0,
                }
            };
            let cast = value_expr;
            // B′ (§9): no try-await passthrough — failure propagation rides
            // the runtime's failed-flag/EH-flag channel inside
            // jeti_task_await, mark-agnostic (plan §9).
            let parts = vec![state_assign, cast];
            *e = AstExpr {
                kind: AstExprKind::Comma, expr_type: None, line, col,
                data: AstExprData::Comma(parts),
                end_line: 0,
                end_col: 0,
            };
        }
        AstExprData::MsgSend { receiver, args, .. } => {
            lower_awaits_m2_expr(receiver, state, ctx);
            for a in args.iter_mut() { lower_awaits_m2_expr(a, state, ctx); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { lower_awaits_m2_expr(c, state, ctx); }
            for a in args.iter_mut() { lower_awaits_m2_expr(a, state, ctx); }
        }
        AstExprData::Unary { operand, .. } => lower_awaits_m2_expr(operand, state, ctx),
        AstExprData::Binary { left, right, .. } => {
            lower_awaits_m2_expr(left, state, ctx);
            lower_awaits_m2_expr(right, state, ctx);
        }
        AstExprData::Assign { target, value } => {
            lower_awaits_m2_expr(target, state, ctx);
            lower_awaits_m2_expr(value, state, ctx);
        }
        AstExprData::Cast { expr, .. } => lower_awaits_m2_expr(expr, state, ctx),
        AstExprData::Ternary { cond, then, else_ } => {
            lower_awaits_m2_expr(cond, state, ctx);
            lower_awaits_m2_expr(then, state, ctx);
            lower_awaits_m2_expr(else_, state, ctx);
        }
        AstExprData::Comma(exprs) => { for x in exprs.iter_mut() { lower_awaits_m2_expr(x, state, ctx); } }
        AstExprData::Subscript { object, key } => {
            lower_awaits_m2_expr(object, state, ctx);
            lower_awaits_m2_expr(key, state, ctx);
        }
        // `await (expr)` keeps the user's parens in the AST — an IIFE inside
        // them is still a task-yielding call to unwrap (§4.3).
        AstExprData::Paren(inner) => lower_awaits_m2_expr(inner, state, ctx),
        AstExprData::InitList(items) | AstExprData::ArrayLit(items) => {
            for x in items.iter_mut() { lower_awaits_m2_expr(x, state, ctx); }
        }
        _ => {}
    }
}

/// Rewrite references inside the entry body: params → `__jeti_f->param`,
/// `self` → `((struct Cls *)__jeti_task->self_obj)` (typed when the enclosing
/// class is known, bare member otherwise). The entry only receives the task,
/// so all param/self access must go through the frame / task.
fn rewrite_entry_refs_stmt(s: &mut AstStmt, params: &[(String, AstType)], self_cls: Option<&str>) {
    match &mut s.data {
        AstStmtData::Expr(e) => rewrite_entry_refs_expr(e, params, self_cls),
        AstStmtData::Compound(stmts) => {
            for st in stmts { rewrite_entry_refs_stmt(st, params, self_cls); }
        }
        AstStmtData::Decl(d) => rewrite_entry_refs_decl(d, params, self_cls),
        AstStmtData::If { cond, then, else_ } => {
            rewrite_entry_refs_expr(cond, params, self_cls);
            rewrite_entry_refs_stmt(then, params, self_cls);
            if let Some(e) = else_ { rewrite_entry_refs_stmt(e, params, self_cls); }
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            rewrite_entry_refs_expr(cond, params, self_cls);
            rewrite_entry_refs_stmt(body, params, self_cls);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(i) = init { rewrite_entry_refs_stmt(i, params, self_cls); }
            if let Some(c) = cond { rewrite_entry_refs_expr(c, params, self_cls); }
            if let Some(u) = incr { rewrite_entry_refs_expr(u, params, self_cls); }
            rewrite_entry_refs_stmt(body, params, self_cls);
        }
        AstStmtData::Return(Some(e)) => rewrite_entry_refs_expr(e, params, self_cls),
        // The entry body is wrapped in a state-machine switch before this walk
        // only for awaits; rewrite_entry_refs runs on the ORIGINAL body, but
        // walk switches/cases anyway so any shape reaches the rewriter.
        AstStmtData::Switch { expr, body } => {
            rewrite_entry_refs_expr(expr, params, self_cls);
            rewrite_entry_refs_stmt(body, params, self_cls);
        }
        AstStmtData::Case { value, body } => {
            rewrite_entry_refs_expr(value, params, self_cls);
            rewrite_entry_refs_stmt(body, params, self_cls);
        }
        AstStmtData::Default(body) => rewrite_entry_refs_stmt(body, params, self_cls),
        AstStmtData::Throw(Some(e)) => rewrite_entry_refs_expr(e, params, self_cls),
        AstStmtData::Try { try_block, catches, finally_block } => {
            rewrite_entry_refs_stmt(try_block, params, self_cls);
            for c in catches.iter_mut() { rewrite_entry_refs_stmt(c, params, self_cls); }
            if let Some(f) = finally_block { rewrite_entry_refs_stmt(f, params, self_cls); }
        }
        AstStmtData::Synchronized { lock, body } => {
            rewrite_entry_refs_expr(lock, params, self_cls);
            rewrite_entry_refs_stmt(body, params, self_cls);
        }
        _ => {}
    }
}

fn rewrite_entry_refs_decl(d: &mut AstDecl, params: &[(String, AstType)], self_cls: Option<&str>) {
    if let AstDeclData::Variable { init, next, .. } = &mut d.data {
        if let Some(i) = init { rewrite_entry_refs_expr(i, params, self_cls); }
        if let Some(n) = next { rewrite_entry_refs_decl(n, params, self_cls); }
    }
}

fn rewrite_entry_refs_expr(e: &mut AstExpr, params: &[(String, AstType)], self_cls: Option<&str>) {
    let line = e.line;
    let col = e.col;
    match &mut e.data {
        AstExprData::VarRef { name, .. } => {
            if name == "self" {
                // Typed self: wrap the task's self_obj in `((struct Cls *)…)`
                // when the enclosing class is known. codegen's IvarRef emitter
                // has its inline-cast special case ONLY for a bare `self`
                // Ident, and a raw `__jeti_task->self_obj` is an untyped
                // NPObject* — so `self->ivar` inside an async body used to
                // emit a C member access on NPObject and fail to compile.
                // The Cast wrapper both type-checks the ivar access in C and
                // gives the PropRef arrow-forcing below a recognizable shape.
                let self_obj = member_expr("__jeti_task", "self_obj", line, col);
                *e = match self_cls {
                    Some(cls) => AstExpr {
                        kind: AstExprKind::Cast, expr_type: None, line, col,
                        data: AstExprData::Cast {
                            target_type: struct_ptr_type(cls),
                            expr: Box::new(self_obj),
                        },
                        end_line: 0,
                        end_col: 0,
                    },
                    None => self_obj,
                };
            } else if params.iter().any(|(n, _)| n == name) {
                // __jeti_f->param
                *e = member_expr("__jeti_f", name, line, col);
            }
        }
        AstExprData::MsgSend { receiver, args, .. } => {
            rewrite_entry_refs_expr(receiver, params, self_cls);
            for a in args.iter_mut() { rewrite_entry_refs_expr(a, params, self_cls); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { rewrite_entry_refs_expr(c, params, self_cls); }
            for a in args.iter_mut() { rewrite_entry_refs_expr(a, params, self_cls); }
        }
        AstExprData::Unary { operand, .. } => rewrite_entry_refs_expr(operand, params, self_cls),
        AstExprData::Binary { left, right, .. } => {
            rewrite_entry_refs_expr(left, params, self_cls);
            rewrite_entry_refs_expr(right, params, self_cls);
        }
        AstExprData::Assign { target, value } => {
            rewrite_entry_refs_expr(target, params, self_cls);
            rewrite_entry_refs_expr(value, params, self_cls);
        }
        AstExprData::Cast { expr, .. } => rewrite_entry_refs_expr(expr, params, self_cls),
        AstExprData::Ternary { cond, then, else_ } => {
            rewrite_entry_refs_expr(cond, params, self_cls);
            rewrite_entry_refs_expr(then, params, self_cls);
            rewrite_entry_refs_expr(else_, params, self_cls);
        }
        AstExprData::Comma(exprs) => {
            for x in exprs.iter_mut() { rewrite_entry_refs_expr(x, params, self_cls); }
        }
        AstExprData::Subscript { object, key } => {
            rewrite_entry_refs_expr(object, params, self_cls);
            rewrite_entry_refs_expr(key, params, self_cls);
        }
        // The block param itself often appears as the OBJECT of an ivar
        // access (`userID->_cstr`) or property access — rewrite the receiver
        // too, not just the bare VarRef spelling.
        AstExprData::IvarRef { obj, .. } => rewrite_entry_refs_expr(obj, params, self_cls),
        AstExprData::PropRef { obj, is_arrow, .. } => {
            rewrite_entry_refs_expr(obj, params, self_cls);
            // `self.service` inside an entry: `self` became a task-self
            // IvarRef node (bare, or Cast-wrapped when typed), whose type the
            // emitter may not know (NPObject*) — the `.`/`->` decision then
            // defaults to `.` and generates invalid C. Force the arrow form
            // when the receiver was rewritten.
            let rewritten_self = match &obj.data {
                AstExprData::IvarRef { ivar: Some(n), obj: inner, .. } => {
                    n == "self_obj" && matches!(&inner.data,
                        AstExprData::VarRef { name: v, .. } if v == "__jeti_task")
                }
                AstExprData::Cast { expr, .. } => matches!(&expr.data,
                    AstExprData::IvarRef { ivar: Some(n), obj: inner, .. }
                        if n == "self_obj" && matches!(&inner.data,
                            AstExprData::VarRef { name: v, .. } if v == "__jeti_task")),
                _ => false,
            };
            if rewritten_self {
                *is_arrow = true;
            }
        }
        // `await (expr)` keeps the user's parens in the AST — walk through them.
        AstExprData::Paren(inner) => rewrite_entry_refs_expr(inner, params, self_cls),
        AstExprData::Await { inner, .. } => rewrite_entry_refs_expr(inner, params, self_cls),
        AstExprData::InitList(items) | AstExprData::ArrayLit(items) => {
            for x in items.iter_mut() { rewrite_entry_refs_expr(x, params, self_cls); }
        }
        _ => {}
    }
}

/// Rewrite `return e;` inside the entry body to the completion protocol:
/// `t->result = (void *)e; t->state = -1; return 1;` for an object result,
/// `t->result = (void *)(unsigned long)e; …` for a scalar one.
/// (void return → just `t->state = -1; return 1;`.)
///
/// The result slot is a `void *`, so the two cases need opposite treatment: a
/// scalar needs the explicit integer cast (C rejects `void * = int`), while an
/// object must NOT be routed through an integer cast — `(unsigned)` truncated
/// the pointer to 32 bits and the awaiting side then dereferenced garbage
/// (`NPString *s = await [self make];` segfaulted on 64-bit).
fn rewrite_returns_to_result(stmts: &mut Vec<AstStmt>, returns_object: bool) {
    for s in stmts.iter_mut() {
        rewrite_returns_stmt(s, returns_object);
    }
}

fn rewrite_returns_stmt(s: &mut AstStmt, returns_object: bool) {
    let line = s.line;
    let col = s.col;
    match &mut s.data {
        AstStmtData::Return(ret) => {
            let taken = ret.take();
            let mut repl: Vec<AstStmt> = Vec::new();
            if let Some(e) = taken {
                // t->result = (void *)e;   /   (void *)(unsigned long)e;
                let value_expr = if returns_object {
                    *e
                } else {
                    AstExpr {
                        kind: AstExprKind::Cast, expr_type: None, line, col,
                        data: AstExprData::Cast {
                            target_type: named_type("unsigned long", false),
                            expr: Box::new(*e),
                        },
                        end_line: 0,
                        end_col: 0,
                    }
                };
                repl.push(AstStmt {
                    kind: AstStmtKind::Expr, line, col,
                    data: AstStmtData::Expr(AstExpr {
                        kind: AstExprKind::Assign, expr_type: None, line, col,
                        data: AstExprData::Assign {
                            target: Box::new(member_expr("__jeti_task", "result", line, col)),
                            value: Box::new(AstExpr {
                                kind: AstExprKind::Cast, expr_type: None, line, col,
                                data: AstExprData::Cast {
                                    target_type: named_type("void", true),
                                    expr: Box::new(value_expr),
                                },
                                end_line: 0,
                                end_col: 0,
                            }),
                        },
                        end_line: 0,
                        end_col: 0,
                    }),
                    end_line: 0,
                    end_col: 0,
                });
            }
            repl.push(AstStmt {
                kind: AstStmtKind::Expr, line, col,
                data: AstStmtData::Expr(AstExpr {
                    kind: AstExprKind::Assign, expr_type: None, line, col,
                    data: AstExprData::Assign {
                        target: Box::new(member_expr("__jeti_task", "state", line, col)),
                        value: Box::new(int_expr(-1, line, col)),
                    },
                    end_line: 0,
                    end_col: 0,
                }),
                end_line: 0,
                end_col: 0,
            });
            repl.push(AstStmt {
                kind: AstStmtKind::Return, line, col,
                data: AstStmtData::Return(Some(Box::new(int_expr(1, line, col)))),
                end_line: 0,
                end_col: 0,
            });
            // Replace this single Return with a Compound of the repl statements.
            *s = AstStmt {
                kind: AstStmtKind::Compound, line, col,
                data: AstStmtData::Compound(repl),
                end_line: 0,
                end_col: 0,
            };
        }
        AstStmtData::Compound(stmts) => rewrite_returns_to_result(stmts, returns_object),
        AstStmtData::If { then, else_, .. } => {
            rewrite_returns_stmt(then, returns_object);
            if let Some(e) = else_ { rewrite_returns_stmt(e, returns_object); }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
        | AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body) => {
            rewrite_returns_stmt(body, returns_object);
        }
        AstStmtData::For { init, body, .. } => {
            if let Some(i) = init { rewrite_returns_stmt(i, returns_object); }
            rewrite_returns_stmt(body, returns_object);
        }
        _ => {}
    }
}

// Expression builders shared by the M2 rewriter.


fn named_type(name: &str, is_ptr: bool) -> AstType {
    let mut inner = AstType::new(TypePrim::Named);
    inner.name = Some(name.to_string());
    if is_ptr {
        let mut t = AstType::new(TypePrim::Named);
        t.is_pointer = true;
        t.subtype = Some(Box::new(inner));
        t
    } else {
        inner
    }
}

/// `struct Cls *` — the typed-self cast target for the M2 entry. The tag
/// fields are what make codegen's ast_type_to_c_str render the `struct`
/// keyword before the class name (same shape the elaborator produces for
/// declared struct types).
fn struct_ptr_type(cls_name: &str) -> AstType {
    let mut inner = AstType::new(TypePrim::Named);
    inner.name = Some(cls_name.to_string());
    inner.is_struct = true;
    inner.tag = TagKind::Struct;
    let mut t = AstType::new(TypePrim::Named);
    t.is_pointer = true;
    t.subtype = Some(Box::new(inner));
    t
}

fn int_expr(v: i64, line: usize, col: usize) -> AstExpr {
    AstExpr { kind: AstExprKind::Int, expr_type: None, line, col, data: AstExprData::Int(v), end_line: 0, end_col: 0 }
}

fn ident_expr(name: &str, line: usize, col: usize) -> AstExpr {
    AstExpr {
        kind: AstExprKind::VarRef, expr_type: None, line, col,
        data: AstExprData::VarRef { sym: None, name: name.to_string() },
        end_line: 0,
        end_col: 0,
    }
}

fn member_expr(obj: &str, field: &str, line: usize, col: usize) -> AstExpr {
    // t->state / t->result — obj is an identifier expression.
    AstExpr {
        kind: AstExprKind::IvarRef, expr_type: None, line, col,
        data: AstExprData::IvarRef {
            obj: Box::new(ident_expr(obj, line, col)),
            ivar: Some(field.to_string()), cls: None,
        },
        end_line: 0,
        end_col: 0,
    }
}

fn call_expr(name: &str, args: Vec<AstExpr>, line: usize, col: usize) -> AstExpr {
    AstExpr {
        kind: AstExprKind::FuncCall, expr_type: None, line, col,
        data: AstExprData::FuncCall { func: None, name: name.to_string(), callee: None, args },
        end_line: 0,
        end_col: 0,
    }
}

fn call_stmt(name: &str, args: Vec<AstExpr>, line: usize, col: usize) -> AstStmt {
    AstStmt { kind: AstStmtKind::Expr, line, col, data: AstStmtData::Expr(call_expr(name, args, line, col)), end_line: 0, end_col: 0 }
}

fn jeti_task_ptr_type() -> AstType {
    let mut inner = AstType::new(TypePrim::Named);
    inner.name = Some("NPTask".to_string());
    let mut t = AstType::new(TypePrim::Named);
    t.is_pointer = true;
    t.subtype = Some(Box::new(inner));
    t
}

/// Build a CstParam for the entry function's `(NPTask *t)`.
fn cst_param(type_name: &str, name: &str) -> jeti_cst::CstParam {
    let mut t = jeti_cst::CstType::new(TypePrim::Named);
    t.name = Some(type_name.to_string());
    let mut ptr = jeti_cst::CstType::new(TypePrim::Named);
    ptr.is_pointer = true;
    ptr.name = Some(type_name.to_string());
    ptr.subtype = Some(Box::new(t));
    jeti_cst::CstParam {
        name: Some(name.to_string()),
        par_type: Some(Box::new(ptr)),
        external_name: None,
        next: None,
        attributes: Vec::new(),
    }
}

/// §8-② (async_soft_keywords_plan): the new parser stores the BUSINESS type on
/// an `async T` signature, but the pipeline runs on the old model's contract —
/// the signature carries the task handle `NPTask<T> *`:
///   - the checker's await-unwrap reads the type arg off it (`NPTask<T> *` → T),
///   - codegen renders it as the uniform `NPTask *` (there is no `NPTask_int`),
///   - the desugared body `return __jeti_task` returns exactly that handle.
/// Restore that shape right after check_unit (so the §6 legacy-spelling error
/// still fires on `(async NPTask<…>)`) and before the checker (so
/// `method_returns` sees the handle form, byte-identical to the old spelling).
/// Runs on BOTH the @interface and the @implementation copy of a method: the
/// checker's method_returns table is last-writer-wins, and the emitted C
/// prototype and definition must agree.
fn normalize_async_signatures(unit: &mut jeti_ast::AstUnit) {
    for decl in unit.decls.iter_mut() {
        match &mut decl.data {
            AstDeclData::Class { methods, .. } => {
                for m in methods.iter_mut() {
                    if let AstDeclData::Method {
                        return_type: Some(rt), async_marker, ..
                    } = &mut m.data
                    {
                        if *async_marker {
                            normalize_task_handle(rt);
                        }
                    }
                }
            }
            AstDeclData::Function { return_type: Some(rt), async_marker, .. } => {
                if *async_marker {
                    normalize_task_handle(rt);
                }
            }
            _ => {}
        }
    }
}

/// `async T` → the task-handle type `NPTask<T> *`. The result is a SINGLE AST
/// node (`name = "NPTask"`, `is_pointer`, `type_args = [T]`) — the checker's
/// unwrap (`t.is_pointer && t.name == "NPTask"`, type arg read off
/// `type_args[0]`) and codegen's `NPTask *` render both read that one node;
/// a pointer-wrapper shape would never match. `async void` → the bare handle
/// (no type arg = void result, per the checker's NPTask<void> rule).
fn normalize_task_handle(rt: &mut AstType) {
    if rt.name.as_deref() == Some("NPTask") {
        return; // already a task handle (legacy shape) — idempotent no-op
    }
    let is_void = rt.prim == TypePrim::Void && !rt.is_pointer;
    let business = std::mem::replace(rt, AstType::new(TypePrim::Void));
    let mut handle = AstType::new(TypePrim::Named);
    handle.name = Some("NPTask".to_string());
    handle.class_ref = Some("NPTask".to_string());
    handle.is_pointer = true;
    if !is_void {
        handle.type_args = vec![business];
    }
    *rt = handle;
}

/// Apply the M2 desugar over the whole unit.
pub fn desugar_unit_m2(unit: &mut jeti_ast::AstUnit) -> (Vec<String>, Vec<String>) {
    // Signature normalization BEFORE anything else in this pass: the desugar,
    // the checker, and codegen all key off the old-model handle shape.
    normalize_async_signatures(unit);
    // The new parser stores the BUSINESS type on an `async T` signature, but
    // the whole pipeline runs on the old model's contract: the signature
    // carries the task handle `NPTask<T> *` (checker's await-unwrap reads
    // type_args off it, codegen renders it as the uniform `NPTask *`, and the
    // desugared body returns the handle). Restore that shape here — after
    // check_unit (so the §6 legacy-spelling error still fires on
    // `(async NPTask<…>)`) and before the checker (so `method_returns` sees
    // the handle form, exactly as if the source had used the old spelling).
    normalize_async_signatures(unit);

    // Pre-collect async method selectors (colon-stripped) so await lowering
    // can tell task-yielding calls from sync ones (expr_type is not
    // annotated yet — desugar runs before the checker).
    let mut async_sels: std::collections::HashSet<String> = std::collections::HashSet::new();
    // §9 await-value typing: the declared business result type of each async
    // method, so the await lowering can cast scalar results back to a word and
    // leave object results alone (see AsyncCtx::sel_ret_obj).
    let mut sel_ret_obj: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for decl in unit.decls.iter() {
        // Class implementations only — see check_unit's async_sels note
        // (protocol names carry no async marker and would poison the set).
        if let AstDeclData::Class { methods, .. } = &decl.data {
            for m in methods {
                if method_is_async(m) {
                    if let Some(sel) = &m.name {
                        async_sels.insert(sel.replace(':', ""));
                        if let AstDeclData::Method { return_type, .. } = &m.data {
                            // The AST stores an async method's return type as
                            // the TASK HANDLE `NPTask<T> *` (same shape the
                            // checker's await unwrap reads), so the BUSINESS
                            // type — the one the await value actually has — is
                            // the first type argument.
                            let business = return_type
                                .as_deref()
                                .and_then(|t| t.type_args.first());
                            let is_obj = business.map_or(false, type_is_object);
                            let key = sel.replace(':', "");
                            // Prefer an object answer when TUs disagree (a
                            // declaration-only client may see a weaker spelling).
                            let slot = sel_ret_obj.entry(key).or_insert(is_obj);
                            *slot = *slot || is_obj;
                        }
                    }
                }
            }
        }
    }
    // §8-①: the same object/scalar oracle, but for EVERY method and top-level
    // function — the elided-block inference needs to type `return [obj msg];`
    // for synchronous selectors too.
    let mut sel_ret_obj_all: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let mut fn_ret_obj: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for decl in unit.decls.iter() {
        match &decl.data {
            AstDeclData::Class { methods, .. } => {
                for m in methods {
                    if let (AstDeclData::Method { return_type, .. }, Some(sel)) = (&m.data, &m.name) {
                        // Signatures are the handle form here
                        // (`normalize_async_signatures` ran above): an async
                        // method's business type is T of `NPTask<T> *`, a sync
                        // method's is the declared type itself.
                        let is_obj = if method_is_async(m) {
                            return_type.as_deref().and_then(|t| t.type_args.first())
                                .map_or(false, type_is_object)
                        } else {
                            return_type.as_deref().map_or(false, type_is_object)
                        };
                        let slot = sel_ret_obj_all.entry(sel.replace(':', "")).or_insert(is_obj);
                        *slot = *slot || is_obj;
                    }
                }
            }
            AstDeclData::Function { return_type, .. } => {
                if let Some(n) = &decl.name {
                    fn_ret_obj.insert(n.clone(), return_type.as_deref().map_or(false, type_is_object));
                }
            }
            _ => {}
        }
    }
    let mut ctx = AsyncCtx {
        async_sels,
        task_vars: std::collections::HashSet::new(),
        async_block_fns: std::collections::HashSet::new(),
        sel_ret_obj,
        sel_ret_obj_all,
        fn_ret_obj,
        block_ret_obj: std::collections::HashMap::new(),
        task_var_obj: std::collections::HashMap::new(),
        in_method: false,
        current_class: None,
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    // Async block literals (§5 desugar) FIRST: every async `^{…}` in the unit
    // (method bodies, plain functions, main) becomes a hidden top-level thunk
    // here, so all later lowering sees plain shapes only —
    //  • an `await` inside a block body is still an Await node and is lowered
    //    by the block thunk's own desugar_method_m2 call below;
    //  • a statement-position IIFE is already a thunk call when the method
    //    desugar / drive_entry_call runs, so auto-drive and await-unwrap see
    //    an ordinary task-yielding call;
    //  • thunks are Functions, so the method loop below never rewrites them.
    // Thunks are prepended ahead of everything (frame/entry before references).
    let mut block_thunk_seq: usize = 0;
    let mut top_decls2: Vec<AstDecl> = Vec::new();
    for decl in unit.decls.iter_mut() {
        match &mut decl.data {
            AstDeclData::Class { methods, cls_sym, .. } => {
                ctx.current_class = cls_sym.clone();
                ctx.in_method = true;
                for m in methods.iter_mut() {
                    if let AstDeclData::Method { body: Some(ref mut b), .. } = &mut m.data {
                        lower_async_blocks_stmt(b, &mut block_thunk_seq, &mut top_decls2, &mut ctx);
                    }
                }
            }
            AstDeclData::Function { body: Some(ref mut b), .. } => {
                ctx.current_class = None;
                ctx.in_method = false;
                lower_async_blocks_stmt(b, &mut block_thunk_seq, &mut top_decls2, &mut ctx);
            }
            _ => {}
        }
    }
    if !top_decls2.is_empty() {
        top_decls2.extend(unit.decls.drain(..));
        unit.decls = top_decls2;
    }
    let mut top_decls: Vec<AstDecl> = Vec::new();
    for decl in unit.decls.iter_mut() {
        if let AstDeclData::Class { methods, cls_sym, .. } = &mut decl.data {
            ctx.current_class = cls_sym.clone();
            for m in methods.iter_mut() {
                let is_async = method_is_async(m);
                if !is_async { continue; }
                if let AstDeclData::Method { body: Some(ref mut b), return_type, params, .. } = &mut m.data {
                    // Extract param list (name, type) from CstParam chain.
                    let mut plist: Vec<(String, AstType)> = Vec::new();
                    let mut p = params.as_ref().map(|b| &**b);
                    while let Some(param) = p {
                        if let (Some(n), Some(t)) = (&param.name, &param.par_type) {
                            plist.push((n.clone(), AstType::from_cst_type(t)));
                        }
                        p = param.next.as_ref().map(|n| &**n);
                    }
                    let sym = m.name.clone().unwrap_or_default();
                    let rt = return_type.as_deref().cloned();
                    let self_cls = ctx.current_class.clone();
                    desugar_method_m2(&sym, &plist, b, &mut top_decls, rt.as_ref(), &mut ctx, "self", self_cls.as_deref());
                }
            }
        }
    }
    // Async block literals (§5 desugar) FIRST — see the walk above. The
    // thunks' frame/entry declarations land in top_decls2, prepended ahead of
    // everything (frame/entry before references).
    // Frame structs and entry functions must appear BEFORE the class
    // implementations that reference them: prepend to the unit.
    let mut new_decls = top_decls;
    new_decls.extend(unit.decls.drain(..));
    unit.decls = new_decls;
    // `[task start]` appears in CALLER bodies too (e.g. main), outside async
    // methods — rewrite it unit-wide (doc/async_nptask_plan.md, stage D).
    // Likewise `await e` in NON-async bodies (main / plain functions): the
    // blocking-join entry form lowers to jeti_task_await(e) here; async
    // method bodies were already rewritten above.
    for decl in &mut unit.decls {
        lower_task_starts_decl(decl, &mut ctx);
        lower_toplevel_awaits_decl(decl);
    }
    // §8-① diagnostics the elided-return inference produced: rule-C
    // incompatibilities are hard errors (driver fails the unit), the
    // unclassifiable fallback stays a warning (`-Werror` escalates those).
    (std::mem::take(&mut ctx.errors), std::mem::take(&mut ctx.warnings))
}

/// `await e` in a non-async (synchronous) body → `jeti_task_await(e)`.
/// Async Semantics v1 rule 3: the only legal sync→async bridge is
/// `+[NPTask blockOn:<async block>]`, which compiles to exactly this
/// blocking-join form (`await` inside the block + the block-on call site
/// itself). Must NOT touch async method bodies (their awaits were already
/// lowered with state-machine bookkeeping above; the double rewrite would
/// corrupt the Comma chains).
fn lower_toplevel_awaits_expr(e: &mut AstExpr) {
    if let AstExprData::Await { inner, .. } = &mut e.data {
        let line = e.line;
        let col = e.col;
        let taken = std::mem::replace(&mut **inner, int_expr(0, line, col));
        // (long)jeti_task_await(e) — the runtime returns void * (the cached
        // result slot); a scalar result flows through the long cast like the
        // legacy eager driver did. Object/pointer results keep their bits.
        let await_call = AstExpr {
            kind: AstExprKind::FuncCall, expr_type: None, line, col,
            data: AstExprData::FuncCall {
                func: None, name: "jeti_task_await".to_string(), callee: None,
                args: vec![taken],
            },
            end_line: 0,
            end_col: 0,
        };
        *e = AstExpr {
            kind: AstExprKind::Cast, expr_type: None, line, col,
            data: AstExprData::Cast {
                target_type: AstType::new(TypePrim::Long),
                expr: Box::new(await_call),
            },
            end_line: 0,
            end_col: 0,
        };
        return;
    }
    match &mut e.data {
        AstExprData::MsgSend { receiver, args, .. } => {
            lower_toplevel_awaits_expr(receiver);
            for a in args.iter_mut() { lower_toplevel_awaits_expr(a); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { lower_toplevel_awaits_expr(c); }
            for a in args.iter_mut() { lower_toplevel_awaits_expr(a); }
        }
        AstExprData::Unary { operand, .. } => lower_toplevel_awaits_expr(operand),
        AstExprData::Binary { left, right, .. } => {
            lower_toplevel_awaits_expr(left);
            lower_toplevel_awaits_expr(right);
        }
        AstExprData::Assign { target, value } => {
            lower_toplevel_awaits_expr(target);
            lower_toplevel_awaits_expr(value);
        }
        AstExprData::Comma(parts) => { for p in parts.iter_mut() { lower_toplevel_awaits_expr(p); } }
        AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => lower_toplevel_awaits_expr(inner),
        _ => {}
    }
}

fn lower_toplevel_awaits_stmt(s: &mut AstStmt) {
    match &mut s.data {
        AstStmtData::Expr(e) => {
            lower_toplevel_awaits_expr(e);
        }
        AstStmtData::Return(Some(e)) => lower_toplevel_awaits_expr(e),
        AstStmtData::If { cond, then, else_ } => {
            lower_toplevel_awaits_expr(cond);
            lower_toplevel_awaits_stmt(then);
            if let Some(e) = else_ { lower_toplevel_awaits_stmt(e); }
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            lower_toplevel_awaits_expr(cond);
            lower_toplevel_awaits_stmt(body);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(i) = init { lower_toplevel_awaits_stmt(i); }
            if let Some(c) = cond { lower_toplevel_awaits_expr(c); }
            if let Some(u) = incr { lower_toplevel_awaits_expr(u); }
            lower_toplevel_awaits_stmt(body);
        }
        AstStmtData::Compound(stmts) => { for st in stmts.iter_mut() { lower_toplevel_awaits_stmt(st); } }
        AstStmtData::Decl(d) => lower_toplevel_awaits_decl(d),
        // Same switch walk as lower_task_starts_stmt: the entry body of an
        // async method is a state-machine switch by the time this runs, and a
        // statement-position task-yielding call inside a case auto-drives (§5.1).
        AstStmtData::Switch { expr, body } => {
            lower_toplevel_awaits_expr(expr);
            lower_toplevel_awaits_stmt(body);
        }
        AstStmtData::Case { value, body } => {
            lower_toplevel_awaits_expr(value);
            lower_toplevel_awaits_stmt(body);
        }
        AstStmtData::Default(body) => lower_toplevel_awaits_stmt(body),
        _ => {}
    }
}

// Hosted drive at the statement position REMOVED (Async Semantics v1 rule 2):
// a bare task-yielding call at statement position is a checker ERROR now
// (check_call_site_discipline), and the lowering side no longer rewrites it.
// The only sync→async bridge is `+[NPTask blockOn:<async block>]` (which
// lowers to the same jeti_task_await blocking join); the only concurrency
// entry is `+[NPTask spawn:<async block>]` (stage 3).

fn lower_toplevel_awaits_decl(d: &mut AstDecl) {
    match &mut d.data {
        AstDeclData::Variable { init, next, .. } => {
            if let Some(i) = init { lower_toplevel_awaits_expr(i); }
            if let Some(n) = next { lower_toplevel_awaits_decl(n); }
        }
        // Plain functions AND the async entries emitted by desugar_method_m2 /
        // block thunks: statement-position task-yielding calls inside them
        // auto-drive in hosted mode (§5.1 语句位 IIFE — an IIFE inside an
        // async method body lives in the entry by the time this runs).
        AstDeclData::Function { body: Some(b), .. } => {
            lower_toplevel_awaits_stmt(b);
        }
        AstDeclData::Class { methods, .. } => {
            for m in methods.iter_mut() {
                // Skip async methods: their awaits are state-machine
                // bookkeeping now; a second rewrite would nest await calls.
                let async_m = method_is_async(m);
                if let AstDeclData::Method { body: Some(b), .. } = &mut m.data {
                    if !async_m { lower_toplevel_awaits_stmt(b); }
                }
            }
        }
        _ => {}
    }
}

// ─── Async block literals (async_soft_keywords_plan.md §5) ──────────────────
//
// `async T? ^params { body }` lowers to a hidden top-level THUNK function:
//
//     NPTask *jeti_async_block_N(T ^bparam, ..., NPObject *__jeti_self_cap) {
//         NPTask *__jeti_task = jeti_task_create(jeti_async_state_…, (id)__jeti_self_cap, sizeof(frame));
//         ((struct …_frame *)__jeti_task->frame)->bparam = bparam; …
//         return __jeti_task;
//     }
//
// i.e. exactly the M2 lazy driver shape desugar_method_m2 emits for async
// methods, with `self` sourced from a trailing hidden capture parameter
// instead of the method receiver. The thunk's ORIGINAL body — awaits
// included — is then handed to desugar_method_m2, which builds the frame
// struct + `jeti_async_state_…` entry (await lowering, capture lifetimes).
// Inside the entry, `self` resolves to the task's self_obj the same way it
// does for methods.
//
// Call sites:
// - IIFE `async ^…(args)` → `thunk(args, self)` — a task-yielding call, so a
//   statement-position IIFE auto-drives via drive_entry_call (hosted), and an
//   `await` IIFE unwraps like any async call.
// - Bare literal (created ≠ executed, §5.3-3) → `(void)thunk` — no side
//   effects, value discarded. A variable of async-block type is a §8-②
//   declaration-spelling question, out of scope here.

fn lower_async_blocks_stmt(
    s: &mut AstStmt,
    seq: &mut usize,
    top_decls: &mut Vec<AstDecl>,
    ctx: &mut AsyncCtx,
) {
    match &mut s.data {
        AstStmtData::Expr(e) => lower_async_blocks_expr(e, seq, top_decls, ctx),
        AstStmtData::Return(Some(e)) => lower_async_blocks_expr(e, seq, top_decls, ctx),
        AstStmtData::If { cond, then, else_ } => {
            lower_async_blocks_expr(cond, seq, top_decls, ctx);
            lower_async_blocks_stmt(then, seq, top_decls, ctx);
            if let Some(e) = else_ { lower_async_blocks_stmt(e, seq, top_decls, ctx); }
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { cond, body } => {
            lower_async_blocks_expr(cond, seq, top_decls, ctx);
            lower_async_blocks_stmt(body, seq, top_decls, ctx);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(i) = init { lower_async_blocks_stmt(i, seq, top_decls, ctx); }
            if let Some(c) = cond { lower_async_blocks_expr(c, seq, top_decls, ctx); }
            if let Some(u) = incr { lower_async_blocks_expr(u, seq, top_decls, ctx); }
            lower_async_blocks_stmt(body, seq, top_decls, ctx);
        }
        AstStmtData::Switch { expr, body } => {
            lower_async_blocks_expr(expr, seq, top_decls, ctx);
            lower_async_blocks_stmt(body, seq, top_decls, ctx);
        }
        AstStmtData::Case { body, .. } => lower_async_blocks_stmt(body, seq, top_decls, ctx),
        AstStmtData::Compound(stmts) => {
            for st in stmts.iter_mut() { lower_async_blocks_stmt(st, seq, top_decls, ctx); }
        }
        AstStmtData::Decl(d) => lower_async_blocks_decl(d, seq, top_decls, ctx),
        _ => {}
    }
}

fn lower_async_blocks_decl(
    d: &mut AstDecl,
    seq: &mut usize,
    top_decls: &mut Vec<AstDecl>,
    ctx: &mut AsyncCtx,
) {
    match &mut d.data {
        AstDeclData::Variable { init, next, .. } => {
            if let Some(i) = init { lower_async_blocks_expr(i, seq, top_decls, ctx); }
            if let Some(n) = next { lower_async_blocks_decl(n, seq, top_decls, ctx); }
        }
        _ => {}
    }
}

fn lower_async_blocks_expr(
    e: &mut AstExpr,
    seq: &mut usize,
    top_decls: &mut Vec<AstDecl>,
    ctx: &mut AsyncCtx,
) {
    // Recurse FIRST so nested async blocks lower innermost-first: an inner
    // IIFE's thunk call is just an argument expression to the outer rewrite.
    match &mut e.data {
        AstExprData::MsgSend { receiver, selector, args, .. } => {
            // ── +[NPTask blockOn:<async block>] — the ONLY sync→async bridge
            // (Async Semantics v1 rule 3). Intercept BEFORE the generic
            // recursion: a bare block argument would otherwise hit the
            // §5.3-3 "block literal with no execution site" arm and be
            // discarded as (void)0. Instead: build the block's thunk, wrap
            // the call in a synthesized Await, and let the unit-wide
            // toplevel-awaits pass (sync bodies) or the M2 state machine
            // (async bodies) lower it to jeti_task_await — the exact
            // blocking-join form rule 3 specifies. The checker never sees
            // the synthesized Await (it runs before the desugar), so using
            // blockOn in main is legal while a user-written `await` there
            // stays an error.
            // `blockOn:junk:` — any selector starting with `blockOn:` on NPTask
            // is bridge territory: the class declares no such methods itself,
            // so a non-`blockOn:` shape is a rule-3 error, not a vtable dispatch.
            if selector.starts_with("blockOn:")
                && matches!(&receiver.data, AstExprData::VarRef { name, .. } if name == "NPTask")
            {
                if selector != "blockOn:" {
                    ctx.errors.push(format!(
                        "{0}:{1}: +[NPTask blockOn:] takes exactly one async block argument — write '[NPTask blockOn:async ^{{ … }}]' (rule 3)",
                        e.line, e.col));
                    return;
                }
                if let [arg] = &mut args[..] {
                    // `(async ^{…})` — a parenthesized block argument: look
                    // through the Paren so the paren spelling takes the bridge
                    // too. Without this the arm below misses, the call falls
                    // through to a vtable dispatch for a selector NPTask does
                    // not declare, and clang rejects the generated C
                    // (`NPTask->blockOn_` — probe-verified). The block expr is
                    // moved out whole (kind/line/col preserved); the Paren
                    // wrapper is not preserved across the bridge — the thunk
                    // call replaces the whole message-send expression.
                    if let AstExprData::Paren(inner) = &mut arg.data {
                        if matches!(&inner.data, AstExprData::Block { is_async: true, .. }) {
                            let blk = std::mem::replace(
                                &mut **inner,
                                int_expr(0, arg.line, arg.col),
                            );
                            *arg = blk;
                        }
                    }
                    if let AstExprData::Block { params, return_type, body, is_async } =
                        &mut arg.data
                    {
                        if *is_async {
                            let params = std::mem::take(params);
                            let return_type = return_type.take();
                            let mut body = body.take();
                            let line = e.line;
                            let col = e.col;
                            let thunk_name = format!("jeti_async_block_{}", seq);
                            *seq += 1;
                            let return_type = elided_block_return_type(
                                return_type, body.as_deref(), ctx, line, col,
                            );
                            // Nested async blocks inside the bridged body must
                            // become thunks BEFORE this thunk's M2 desugar sees
                            // them — otherwise they lower as plain C blocks and
                            // their awaits never suspend (probe-verified: an
                            // inner `return await [a calc:x];` miscompiled to a
                            // synchronous vtable call inside a plain block).
                            // Classify FIRST (above), lower AFTER — the comma
                            // form the lowering produces is unclassifiable.
                            if let Some(ref mut b) = body {
                                lower_async_blocks_stmt(b, seq, top_decls, ctx);
                            }
                            build_async_block_thunk(
                                &thunk_name, &params, return_type.as_deref(), body,
                                top_decls, ctx,
                            );
                            ctx.async_block_fns.insert(thunk_name.clone());
                            ctx.block_ret_obj.insert(
                                thunk_name.clone(),
                                return_type.as_deref().map_or(false, type_is_object),
                            );
                            // Hidden capture argument: live `self` inside a
                            // method, `(NPObject *)0` in plain functions/main
                            // (same rule as the IIFE arm).
                            let capture = if ctx.in_method {
                                ident_expr("self", line, col)
                            } else {
                                AstExpr {
                                    kind: AstExprKind::Cast, expr_type: None, line, col,
                                    data: AstExprData::Cast {
                                        target_type: named_type("NPObject", true),
                                        expr: Box::new(int_expr(0, line, col)),
                                    },
                                    end_line: 0,
                                    end_col: 0,
                                }
                            };
                            let call = call_expr(&thunk_name, vec![capture], line, col);
                            *e = AstExpr {
                                kind: AstExprKind::Await, expr_type: None, line, col,
                                data: AstExprData::Await { inner: Box::new(call) },
                                end_line: 0,
                                end_col: 0,
                            };
                            return;
                        }
                        // A plain block is synchronous and cannot be driven by
                        // the bridge — rule 3 names the `async ^` spelling. A
                        // silent fall-through here emitted `NPTask->blockOn_`
                        // (probe-verified): codegen has no blockOn vtable slot,
                        // and clang rejected it with an unrelated-looking error.
                        ctx.errors.push(format!(
                            "{0}:{1}: +[NPTask blockOn:] requires an async block — write 'async ^{{ … }}'; a plain block is synchronous and cannot be driven by the bridge (rule 3)",
                            e.line, e.col));
                        return;
                    }
                    // Wrong shape entirely: a paren-wrapped plain block gets
                    // the same fix-it; anything else is a shape error.
                    let is_paren_plain_block = matches!(&arg.data, AstExprData::Paren(inner)
                        if matches!(&inner.data, AstExprData::Block { is_async: false, .. }));
                    ctx.errors.push(format!(
                        "{0}:{1}: +[NPTask blockOn:] requires an async block argument — write '[NPTask blockOn:async ^{{ … }}]'{2} (rule 3)",
                        e.line, e.col,
                        if is_paren_plain_block { " — a plain block is synchronous and cannot be driven by the bridge" } else { "" }));
                    return;
                }
                // Arity: `blockOn:` takes exactly one argument.
                ctx.errors.push(format!(
                    "{0}:{1}: +[NPTask blockOn:] takes exactly one async block argument — write '[NPTask blockOn:async ^{{ … }}]' (rule 3)",
                    e.line, e.col));
                return;
            }
            lower_async_blocks_expr(receiver, seq, top_decls, ctx);
            for a in args.iter_mut() { lower_async_blocks_expr(a, seq, top_decls, ctx); }
        }
        // IIFE `async ^…(args)`: the elaborator keeps the block literal as the
        // callee. Lower the block into a thunk, then rebuild the call as
        // `thunk(args, self)` — a task-yielding call, so a statement-position
        // IIFE auto-drives via drive_entry_call and an `await` IIFE unwraps
        // like any async call (§4.3 semantics).
        AstExprData::FuncCall { callee: Some(c), args, .. } => {
            // `(async ^…)(args)` — a parenthesized block callee is wrapped in
            // Paren; unwrap it so the parenthesized IIFE takes the same arm
            // as the direct `async ^…(args)` form. Without this the callee
            // falls through to the generic call path and renders as an empty
            // function name — the call degrades to `(args)` (probe-verified
            // silent miscompile: the block never ran, the arg echoed back).
            let is_async_block_callee = match &c.data {
                AstExprData::Block { is_async: true, .. } => true,
                AstExprData::Paren(inner) => matches!(&inner.data,
                    AstExprData::Block { is_async: true, .. }),
                _ => false,
            };
            if is_async_block_callee {
                let (params, return_type, mut body) = match std::mem::replace(
                    &mut c.data,
                    AstExprData::Int(0),
                ) {
                    AstExprData::Block { params, return_type, body, .. }
                        => (params, return_type, body),
                    AstExprData::Paren(inner) => match inner.data {
                        AstExprData::Block { params, return_type, body, .. }
                            => (params, return_type, body),
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
                };
                let line = e.line;
                let col = e.col;
                let thunk_name = format!("jeti_async_block_{}", seq);
                *seq += 1;
                // §8-①: an elided block's return type is inferred BEFORE the
                // thunk is built — the thunk signature, the return-to-result
                // rewriting and the await-site value cast all key off it.
                let return_type = elided_block_return_type(return_type, body.as_deref(), ctx, line, col);
                // Nested async blocks inside the IIFE body: lower to thunks
                // before this thunk's M2 desugar runs (same rule as the
                // blockOn arm — classify first, lower after).
                if let Some(ref mut b) = body {
                    lower_async_blocks_stmt(b, seq, top_decls, ctx);
                }
                build_async_block_thunk(&thunk_name, &params, return_type.as_deref(), body, top_decls, ctx);
                ctx.async_block_fns.insert(thunk_name.clone());
                ctx.block_ret_obj.insert(
                    thunk_name.clone(),
                    return_type.as_deref().map_or(false, type_is_object),
                );
                let mut call_args = std::mem::take(args);
                // Hidden capture argument: the live `self` inside a method;
                // `(NPObject *)0` in plain functions/main (§5) — there is no
                // self in scope there, and a block body that uses it is
                // already a checker error, so the null is never dereferenced.
                if ctx.in_method {
                    call_args.push(ident_expr("self", line, col));
                } else {
                    call_args.push(AstExpr {
                        kind: AstExprKind::Cast, expr_type: None, line, col,
                        data: AstExprData::Cast {
                            target_type: named_type("NPObject", true),
                            expr: Box::new(int_expr(0, line, col)),
                        },
                        end_line: 0,
                        end_col: 0,
                    });
                }
                *e = call_expr(&thunk_name, call_args, line, col);
                return;
            }
            lower_async_blocks_expr(c, seq, top_decls, ctx);
            for a in args.iter_mut() { lower_async_blocks_expr(a, seq, top_decls, ctx); }
        }
        AstExprData::FuncCall { callee: None, args, .. } => {
            for a in args.iter_mut() { lower_async_blocks_expr(a, seq, top_decls, ctx); }
        }
        AstExprData::Unary { operand, .. } => lower_async_blocks_expr(operand, seq, top_decls, ctx),
        AstExprData::Binary { left, right, .. } => {
            lower_async_blocks_expr(left, seq, top_decls, ctx);
            lower_async_blocks_expr(right, seq, top_decls, ctx);
        }
        AstExprData::Assign { target, value } => {
            lower_async_blocks_expr(target, seq, top_decls, ctx);
            lower_async_blocks_expr(value, seq, top_decls, ctx);
        }
        AstExprData::Comma(parts) => {
            for p in parts.iter_mut() { lower_async_blocks_expr(p, seq, top_decls, ctx); }
        }
        AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => {
            lower_async_blocks_expr(inner, seq, top_decls, ctx)
        }
        AstExprData::Await { inner, .. } => lower_async_blocks_expr(inner, seq, top_decls, ctx),
        AstExprData::Subscript { object, key } => {
            lower_async_blocks_expr(object, seq, top_decls, ctx);
            lower_async_blocks_expr(key, seq, top_decls, ctx);
        }
        // Bare async block literal (created ≠ executed, §5.3-3): the block
        // has NO execution site, and a thunk call is pure task creation with
        // no observable side effect — lower the whole thing to a `(void)0`
        // no-op. Emitting the call would (a) need placeholder args for every
        // block param and (b) look like a task-yielding call, so the
        // statement-position auto-drive would START the task — exactly the
        // execution §5.3-3 forbids.
        // A variable of async-block type is the §8-② declaration-spelling
        // question, out of scope here.
        AstExprData::Block { params, return_type, body, is_async } if *is_async => {
            let line = e.line;
            let col = e.col;
            let params = std::mem::take(params);
            let return_type = return_type.take();
            let body = body.take();
            let thunk_name = format!("jeti_async_block_{}", seq);
            *seq += 1;
            // Thunk still carries the block's params (the body references
            // them through the frame); it just never gets CALLED here.
            let return_type = elided_block_return_type(return_type, body.as_deref(), ctx, line, col);
            build_async_block_thunk(&thunk_name, &params, return_type.as_deref(), body, top_decls, ctx);
            ctx.async_block_fns.insert(thunk_name.clone());
            ctx.block_ret_obj.insert(
                thunk_name.clone(),
                return_type.as_deref().map_or(false, type_is_object),
            );
            *e = AstExpr {
                kind: AstExprKind::Cast, expr_type: None, line, col,
                data: AstExprData::Cast {
                    target_type: AstType::new(TypePrim::Void),
                    expr: Box::new(int_expr(0, line, col)),
                },
                end_line: 0,
                end_col: 0,
            };
        }
        _ => {}
    }
}

/// Build the hidden thunk function for one async block literal. `params` are
/// the block's declared parameters (elaborator-normalized AST types); `body`
/// is the original block body (awaits still in AST form). The thunk's
/// trailing hidden param captures `self`; the body delegates to the M2
/// machinery (frame struct + `jeti_async_state_…` entry + lazy driver), with
/// `self` sourced from the capture parameter.
fn build_async_block_thunk(
    thunk_name: &str,
    params: &[(AstType, String)],
    return_type: Option<&AstType>,
    body: Option<Box<AstStmt>>,
    top_decls: &mut Vec<AstDecl>,
    ctx: &mut AsyncCtx,
) {
    let Some(body) = body else { return };
    let line = body.line;
    let col = body.col;
    // Thunk params: block params + hidden self capture (trailing), as a
    // CstParam chain — the same shape the M2 param extraction
    // (desugar_method_m2) consumes. Built by reverse fold.
    let mut next: Option<Box<jeti_cst::CstParam>> = Some(Box::new(jeti_cst::CstParam {
        name: Some("__jeti_self_cap".to_string()),
        par_type: Some(Box::new({
            let mut t = AstType::new(TypePrim::Named);
            t.name = Some("NPObject".to_string());
            t.is_pointer = true;
            t.to_cst_type()
        })),
        external_name: None,
        next: None,
        attributes: Vec::new(),
    }));
    for (pt, pn) in params.iter().rev() {
        next = Some(Box::new(jeti_cst::CstParam {
            name: Some(pn.clone()),
            par_type: Some(Box::new(pt.to_cst_type())),
            external_name: None,
            next,
            attributes: Vec::new(),
        }));
    }
    let head = next;

    // The thunk body IS the lazy driver desugar_method_m2 emits (create +
    // param fill + return handle); it consumes `body` (awaits) into the
    // frame/entry declarations it prepends to top_decls.
    let mut driver_body = body;
    // desugar_method_m2 consumes (name, type) pairs; the elaborator hands us
    // (type, name) — flip before delegating.
    let m2_params: Vec<(String, AstType)> =
        params.iter().map(|(t, n)| (n.clone(), t.clone())).collect();
    // The thunk's hidden capture param is the task's self_obj, so block-body
    // `self` flows through the same `__jeti_task->self_obj` rewrite as method
    // bodies — pass the enclosing class for the typed-self cast too.
    let self_cls = ctx.current_class.clone();
    desugar_method_m2(
        thunk_name,
        &m2_params,
        &mut driver_body,
        top_decls,
        return_type,
        ctx,
        "__jeti_self_cap",
        self_cls.as_deref(),
    );

    let thunk_decl = AstDecl {
        kind: AstDeclKind::Function,
        name: Some(thunk_name.to_string()),
        line, col,
        data: AstDeclData::Function {
            func_sym: None,
            return_type: Some(Box::new(jeti_task_ptr_type())),
            params: head,
            body: Some(Box::new(*driver_body)),
            has_variadic: false,
            throws: None,
            async_marker: false,
        },
        attributes: Vec::new(),
        end_line: 0,
        end_col: 0,
    };
    top_decls.push(thunk_decl);
}
