//! `@defer` — scope-exit execution (Go-style), desugar pass.
//!
//! Design: AGENTS.md `@defer` section (finalized 2026-09-29). Runs at pipeline
//! Step 3.9 — after `-eh checked` desugar (whose `@throw`s are already
//! ordinary returns) and BEFORE ARC, so that:
//!
//! - ARC's scope-end releases are injected at the same exit points but AFTER
//!   the user's defer statements — deferred code runs while objects are
//!   still alive;
//! - `-eh checked` needs no special case: its throws became returns before
//!   this pass ran, and returns are spliced like any other exit.
//!
//! Semantics (M1): a defer registers its body with the innermost enclosing
//! compound block. The body runs at EVERY exit of that block — natural end,
//! any `return` (at any depth), `break`/`continue` that jump OUT of the
//! defers registered between here and the innermost loop/switch, and
//! same-function `@throw` — innermost-block defers first (LIFO). Loop-body
//! blocks re-run their defers every iteration (a block exits once per
//! iteration). Variables are the enclosing scope's own stack slots: no
//! capture, no copy.
//!
//! Two tracked sets (both innermost-first):
//! - `pending`: everything registered so far — fires on `return`/`@throw`
//!   (function-level exits);
//! - `jump`: the subset that fires on `break`/`continue` — the defers
//!   registered between here and the innermost loop/switch. Entering a
//!   loop/switch body resets `jump` to empty; plain blocks accumulate it as
//!   "this block's own defers ++ incoming jump".
//!
//! M1 restrictions: `@defer` must appear directly inside a block; defer
//! bodies must not contain `return`/`break`/`continue`/`@throw`. Cross-
//! function `@throw` (sjlj longjmp past intermediate frames) skips those
//! frames' defers — the same documented limitation as ARC's scope-end
//! releases.
//!
//! Codegen sees ordinary statements afterwards; zero codegen changes.

use jeti_ast::ast::*;

pub struct DeferDiagnostics {
    pub errors: Vec<String>,
}

impl DeferDiagnostics {
    fn error(&mut self, line: usize, col: usize, msg: &str) {
        self.errors.push(format!("{}:{}: {}", line, col, msg));
    }
}

/// Validate + desugar every `@defer` in the unit (in place).
pub fn desugar_unit(unit: &mut AstUnit) -> DeferDiagnostics {
    let mut diags = DeferDiagnostics { errors: Vec::new() };
    for decl in &mut unit.decls {
        desugar_decl(decl, &mut diags);
    }
    diags
}

fn desugar_decl(decl: &mut AstDecl, diags: &mut DeferDiagnostics) {
    match &mut decl.data {
        // Function/method bodies are implicit blocks: a defer at their top
        // level runs at every `return` and at the implicit end.
        AstDeclData::Function { body: Some(ref mut b), .. }
        | AstDeclData::Method { body: Some(ref mut b), .. } => {
            let none: Vec<AstStmt> = Vec::new();
            desugar_stmt(b, &none, &none, diags);
        }
        AstDeclData::Class { ref mut methods, .. } => {
            for m in methods.iter_mut() {
                desugar_decl(m, diags);
            }
        }
        // Functions declared inside `@namespace` — otherwise their defers
        // would never splice and codegen's catch-all would silently drop them.
        AstDeclData::Namespace(members) => {
            for m in members.iter_mut() {
                desugar_decl(m, diags);
            }
        }
        _ => {}
    }
}

/// Rewrite one statement in place.
///
/// `pending` = all defers registered so far (innermost-first); fires before
/// `return` and same-function `@throw`. `jump` = the subset that fires before
/// `break`/`continue` here (see module docs). Registered bodies are CLONED
/// into exit points and stay registered for further exits.
fn desugar_stmt(s: &mut AstStmt, pending: &[AstStmt], jump: &[AstStmt], diags: &mut DeferDiagnostics) {
    match &mut s.data {
        AstStmtData::Compound(stmts) => desugar_block(stmts, pending, jump, diags),
        AstStmtData::If { cond, then, else_ } => {
            desugar_expr(cond, diags);
            desugar_stmt(then, pending, jump, diags);
            if let Some(el) = else_ { desugar_stmt(el, pending, jump, diags); }
        }
        // break/continue bind to the innermost loop/switch: defers registered
        // OUTSIDE it must not fire on them — hence jump=[] for the body.
        // `return` inside the loop still fires everything (pending).
        AstStmtData::While { cond, body } => {
            desugar_expr(cond, diags);
            desugar_stmt(body, pending, &[], diags);
        }
        AstStmtData::Do { body, cond } => {
            desugar_stmt(body, pending, &[], diags);
            desugar_expr(cond, diags);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(i) = init { desugar_stmt(i, pending, &[], diags); }
            if let Some(c) = cond { desugar_expr(c, diags); }
            if let Some(x) = incr { desugar_expr(x, diags); }
            desugar_stmt(body, pending, &[], diags);
        }
        AstStmtData::ForIn { var, collection, body } => {
            desugar_expr(var, diags);
            desugar_expr(collection, diags);
            desugar_stmt(body, pending, &[], diags);
        }
        AstStmtData::Switch { expr, body } => {
            desugar_expr(expr, diags);
            desugar_stmt(body, pending, &[], diags);
        }
        // Pattern switch arms run before the pattern crate lowers them, but
        // their bodies can register defers like any block.
        AstStmtData::SwitchPat { expr, arms, .. } => {
            desugar_expr(expr, diags);
            for arm in arms.iter_mut() {
                if let Some(ref mut g) = arm.guard { desugar_expr(g, diags); }
                desugar_stmt(&mut arm.body, pending, &[], diags);
            }
        }
        AstStmtData::Case { value, body } => {
            desugar_expr(value, diags);
            desugar_stmt(body, pending, jump, diags);
        }
        AstStmtData::Default(body) => desugar_stmt(body, pending, jump, diags),
        // break/continue also carry `Return` data — the AST distinguishes
        // them by `kind` (see elaborator's Return conversion).
        AstStmtData::Return(_) => {
            let is_jump = matches!(s.kind, AstStmtKind::Break | AstStmtKind::Continue);
            if is_jump {
                if !jump.is_empty() { splice_before(s, jump); }
            } else if !pending.is_empty() {
                splice_before(s, pending);
            }
        }
        // Same-function `@throw`: run this block's defers before the throw
        // executes (the longjmp happens after the operand evaluates).
        AstStmtData::Throw(_) => {
            if !pending.is_empty() { splice_before(s, pending); }
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            desugar_stmt(try_block, pending, jump, diags);
            for c in catches.iter_mut() { desugar_stmt(c, pending, jump, diags); }
            if let Some(f) = finally_block { desugar_stmt(f, pending, jump, diags); }
        }
        AstStmtData::Catch { body, .. } => desugar_stmt(body, pending, jump, diags),
        AstStmtData::Finally(body) => desugar_stmt(body, pending, jump, diags),
        AstStmtData::Synchronized { lock, body } => {
            desugar_expr(lock, diags);
            desugar_stmt(body, pending, jump, diags);
        }
        AstStmtData::Autoreleasepool(body) => desugar_stmt(body, pending, jump, diags),
        AstStmtData::NoArc(body) => desugar_stmt(body, pending, jump, diags),
        // Reaching a Defer here means it was NOT directly inside a block
        // (e.g. the sole body of an `if`) — M1 rejects; the block-position
        // case is handled by desugar_block, which never lands in this arm.
        AstStmtData::Defer(_) => {
            diags.error(s.line, s.col,
                "'@defer' must appear directly inside a block — wrap it in '{ }'");
        }
        AstStmtData::Decl(d) => desugar_decl_exprs(d, diags),
        AstStmtData::Expr(e) => desugar_expr(e, diags),
        AstStmtData::Goto(_) | AstStmtData::Label(_) | AstStmtData::Asm { .. } => {}
    }
}

/// Splice a block's statements: register its `@defer`s, recurse, then append
/// this block's own defers at the natural end (LIFO). `outer` defers stay
/// registered for function-level exits; `jump` passes through plus this
/// block's own defers for break/continue inside.
fn desugar_block(stmts: &mut Vec<AstStmt>, outer: &[AstStmt], jump: &[AstStmt], diags: &mut DeferDiagnostics) {
    let mut pending: Vec<AstStmt> = outer.to_vec();
    let mut n_own = 0usize; // defers registered by THIS block (pending[..n_own])
    let mut i = 0;
    while i < stmts.len() {
        if matches!(stmts[i].data, AstStmtData::Defer(_)) {
            let node = std::mem::replace(&mut stmts[i], empty_stmt());
            if let AstStmtData::Defer(mut body) = node.data {
                validate_defer_body(&body, diags);
                // A block literal inside the defer body is its own function:
                // desugar it so ITS defers attach to the literal's body, not
                // to ours.
                let none: Vec<AstStmt> = Vec::new();
                desugar_stmt(&mut body, &none, &none, diags);
                // Prepend: pending[0] is the most recent (innermost) defer.
                pending.insert(0, *body);
                n_own += 1;
            }
            stmts.remove(i);
            continue; // do not advance — the next statement slid into slot i
        }
        // Snapshot semantics: a defer registered LATER in this block must not
        // fire at exits that happen EARLIER, so per statement the jump set is
        // "this block's defers so far ++ incoming jump".
        let mut inner_jump: Vec<AstStmt> = pending[..n_own].to_vec();
        inner_jump.extend(jump.iter().cloned());
        desugar_stmt(&mut stmts[i], &pending, &inner_jump, diags);
        i += 1;
    }
    if n_own > 0 {
        // Natural end of the block: run this block's own defers (LIFO).
        // Outer defers are NOT spliced here — their block will splice them.
        let tail: Vec<AstStmt> = pending[..n_own].to_vec();
        stmts.extend(tail);
    }
}

fn splice_before(s: &mut AstStmt, pending: &[AstStmt]) {
    let line = s.line;
    let col = s.col;
    let old = std::mem::replace(s, empty_stmt());
    let mut stmts: Vec<AstStmt> = Vec::with_capacity(pending.len() + 1);
    for d in pending {
        stmts.push(d.clone());
    }
    stmts.push(old);
    *s = AstStmt {
        kind: AstStmtKind::Compound, line, col,
        data: AstStmtData::Compound(stmts),
    };
}

fn empty_stmt() -> AstStmt {
    AstStmt {
        kind: AstStmtKind::Compound, line: 0, col: 0,
        data: AstStmtData::Compound(Vec::new()),
    }
}

/// M1: a defer body must not contain `return`/`break`/`continue`/`@throw` —
/// they would change or skip the cleanup itself. (Block literals inside the
/// body were already desugared with a fresh scope before validation of the
/// surrounding statements happens; expressions are not walked: a `return`
/// inside a block literal belongs to the literal's function.)
fn validate_defer_body(s: &AstStmt, diags: &mut DeferDiagnostics) {
    // break/continue share the `Return` data variant (see elaborator);
    // tell them apart by statement kind.
    match &s.data {
        AstStmtData::Return(_) => match s.kind {
            AstStmtKind::Break => diags.error(s.line, s.col,
                "'break' inside an '@defer' body is not supported (M1)"),
            AstStmtKind::Continue => diags.error(s.line, s.col,
                "'continue' inside an '@defer' body is not supported (M1)"),
            _ => diags.error(s.line, s.col,
                "'return' inside an '@defer' body is not supported (M1) — restructure the cleanup"),
        },
        // `@throw` in a defer body: the eh desugar (Step 3.85) runs BEFORE
        // this pass, so a checked-mode throw here is never rewritten and
        // would fall through to raw sjlj longjmp — mixing the two backends.
        AstStmtData::Throw(_) => diags.error(s.line, s.col,
            "'@throw' inside an '@defer' body is not supported (M1) — cleanup code must not throw"),
        AstStmtData::Compound(stmts) => for st in stmts { validate_defer_body(st, diags); },
        AstStmtData::If { then, else_, .. } => {
            validate_defer_body(then, diags);
            if let Some(el) = else_ { validate_defer_body(el, diags); }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
        | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. }
        | AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. }
        | AstStmtData::Default(body) | AstStmtData::Synchronized { body, .. }
        | AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body)
        | AstStmtData::Catch { body, .. } | AstStmtData::Finally(body)
        | AstStmtData::Defer(body) => validate_defer_body(body, diags),
        AstStmtData::Try { try_block, catches, finally_block } => {
            validate_defer_body(try_block, diags);
            for c in catches { validate_defer_body(c, diags); }
            if let Some(f) = finally_block { validate_defer_body(f, diags); }
        }
        _ => {}
    }
}

/// Walk a declaration chain's initializers (block literals may hold defers).
fn desugar_decl_exprs(d: &mut AstDecl, diags: &mut DeferDiagnostics) {
    let mut cur: Option<&mut AstDecl> = Some(d);
    while let Some(decl) = cur.take() {
        let mut next_opt: Option<&mut AstDecl> = None;
        if let AstDeclData::Variable { init, next, .. } = &mut decl.data {
            if let Some(e) = init { desugar_expr(e, diags); }
            next_opt = next.as_deref_mut();
        }
        cur = next_opt;
    }
}

/// Walk an expression tree for block literals — the only expressions that can
/// contain statements. A block literal compiles to its own function, so its
/// defers attach to the literal's body block with a FRESH pending set (outer
/// defers do not fire inside it).
fn desugar_expr(e: &mut AstExpr, diags: &mut DeferDiagnostics) {
    match &mut e.data {
        AstExprData::Block { body: Some(ref mut b), .. } => {
            let none: Vec<AstStmt> = Vec::new();
            desugar_stmt(b, &none, &none, diags);
        }
        AstExprData::MsgSend { receiver, args, .. } => {
            desugar_expr(receiver, diags);
            for a in args.iter_mut() { desugar_expr(a, diags); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { desugar_expr(c, diags); }
            for a in args.iter_mut() { desugar_expr(a, diags); }
        }
        AstExprData::Unary { operand, .. } => desugar_expr(operand, diags),
        AstExprData::Binary { left, right, .. } => {
            desugar_expr(left, diags);
            desugar_expr(right, diags);
        }
        AstExprData::Assign { target, value, .. } => {
            desugar_expr(target, diags);
            desugar_expr(value, diags);
        }
        AstExprData::Cast { expr, .. } => desugar_expr(expr, diags),
        AstExprData::ArrayLit(items) | AstExprData::InitList(items) => {
            for x in items.iter_mut() { desugar_expr(x, diags); }
        }
        AstExprData::DictLit { keys, values } => {
            for k in keys.iter_mut() { desugar_expr(k, diags); }
            for v in values.iter_mut() { desugar_expr(v, diags); }
        }
        AstExprData::DesignatedInit { expr, .. } => desugar_expr(expr, diags),
        AstExprData::Subscript { object, key } => {
            desugar_expr(object, diags);
            desugar_expr(key, diags);
        }
        AstExprData::Ternary { cond, then, else_ } => {
            desugar_expr(cond, diags);
            desugar_expr(then, diags);
            desugar_expr(else_, diags);
        }
        AstExprData::Comma(items) => {
            for x in items.iter_mut() { desugar_expr(x, diags); }
        }
        AstExprData::Paren(inner) | AstExprData::Await { inner, .. } => desugar_expr(inner, diags),
        AstExprData::Sizeof { expr: Some(ref mut x), .. } => desugar_expr(x, diags),
        AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => {
            desugar_expr(obj, diags)
        }
        _ => {}
    }
}
