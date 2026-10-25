use jeti_ast::*;
use jeti_cfg::*;
use jeti_ownership::*;
use jeti_cst::TypePrim;

#[derive(Debug, Clone, Copy)]
pub enum ArcActionKind {
    Retain, Release, Autorelease,
}

#[derive(Debug, Clone)]
pub struct ArcAction {
    pub kind: ArcActionKind,
    pub target: Option<Box<AstExpr>>,
    pub insert_after_idx: usize,
    pub insert_at_end: bool,
}

#[derive(Debug, Clone)]
pub struct ArcResult {
    pub actions: Vec<ArcAction>,
    pub leak_warnings: Vec<String>,
    /// Variables referenced from inside a block literal anywhere in the
    /// function. A block capture is only established when the literal is
    /// evaluated, and the captured object must stay alive for as long as the
    /// block can be *called* — so an exit expression that might invoke a block
    /// takes the temporary-based lowering even when it does not mention the
    /// variable by name (`return blk() - 7;` with `blk` capturing `h`).
    pub captured: Vec<String>,
    /// Per-`goto` cleanup plan (see `GotoCleanup`): which scopes the jump
    /// leaves, or that the jump is refused.
    pub goto_plan: Vec<GotoCleanup>,
    /// Hard ARC errors (as opposed to `leak_warnings`): the analysis found a
    /// construct it cannot lower correctly, and the pipeline must refuse to
    /// emit code for it.
    pub errors: Vec<String>,
}

impl ArcResult {
    pub fn new() -> Self {
        ArcResult {
            actions: Vec::new(),
            leak_warnings: Vec::new(),
            captured: Vec::new(),
            goto_plan: Vec::new(),
            errors: Vec::new(),
        }
    }
}

fn npobject_ptr_type() -> AstType {
    let mut t = AstType::new(TypePrim::Named);
    t.name = Some("NPObject".to_string());
    t.is_struct = true;
    t.is_pointer = true;
    t
}

fn make_release_stmt(target: &AstExpr) -> AstStmt {
    // The release argument must always be an `NPObject *`: synthesized
    // releases reach typed ivar slots (`NPMutableString *`, `FsApi *`, …)
    // and plain C flags every narrowing to the runtime parameter
    // (-Wincompatible-pointer-types), while jeti's ObjC semantics allow the
    // implicit upcast. Spell the upcast in the AST so the emitted call is
    // warning-clean on every backend.
    let cast_arg = AstExpr {
        kind: AstExprKind::Cast, expr_type: None, line: target.line, col: target.col,
        data: AstExprData::Cast {
            target_type: npobject_ptr_type(),
            expr: Box::new(target.clone()),
        },
        end_line: 0,
        end_col: 0,
    };
    AstStmt {
        kind: AstStmtKind::Expr, line: 0, col: 0,
        data: AstStmtData::Expr(AstExpr {
            kind: AstExprKind::FuncCall, expr_type: None, line: 0, col: 0,
            data: AstExprData::FuncCall {
                func: None, name: "jeti_release".to_string(), callee: None, args: vec![cast_arg],
            },
            end_line: 0,
            end_col: 0,
        }),
        end_line: 0,
        end_col: 0,
    }
}

fn is_object_type(t: &AstType) -> bool {
    t.is_pointer || t.prim == TypePrim::Id || t.prim == TypePrim::Instancetype
}

fn is_scope_stmt(s: &AstStmt) -> bool {
    matches!(s.data,
        AstStmtData::Compound(_) |
        AstStmtData::Autoreleasepool(_) |
        AstStmtData::Synchronized { .. }
    )
}

fn var_ref_expr(name: &str) -> AstExpr {
    AstExpr {
        kind: AstExprKind::VarRef, expr_type: None, line: 0, col: 0,
        data: AstExprData::VarRef { sym: None, name: name.to_string() },
        end_line: 0,
        end_col: 0,
    }
}

// ─── exit-point cleanup ordering ────────────────────────────────────────────
//
// ARC releases an owned local when control leaves its scope — but only AFTER
// the expression that causes the exit has been fully evaluated. When the exit
// expression READS a local that is about to be released, the release moves
// behind a temporary:
//
//     return [[h text] length];  ⇒  __auto_type t = [[h text] length];
//                                   jeti_release(h);
//                                   return t;
//
// Two rules, pinned down by `tests/arc_order/`:
//
//   1. OWNERSHIP TRANSFER is only a bare local (`return h;`) — the object is
//      handed to the caller, so it must NOT be released. An exit expression
//      that merely *reads* the local (`return [h text];`,
//      `return [[h text] length] - 5;`) is not a transfer: the release still
//      happens, it just has to happen after the expression.
//   2. Every non-transferred local is released exactly once per path — a
//      returning branch must not rob the fall-through path of its release.
static ARC_TMP_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Names an object whose ownership LEAVES this frame in the exit expression, so
/// it must not be released here.
///
/// Two shapes qualify:
///   * `return h;` — a bare owned local is handed to the caller.
///   * `return jeti_autorelease(h);` / `return [h autorelease];` — the object
///     is handed to the autorelease pool; the value the caller receives is
///     +0 and must stay valid, so releasing it here would be a premature free.
///     Foundation relies on this (`NPArray +arrayWithObjects:count:`).
///
/// Deliberately NOT a transfer: an expression that merely *reads* the local
/// (`return [h text];`, `return [[h text] length] - 5;`). Those keep their
/// release, ordered after the expression — the old analyzer treated a
/// `MsgSend` on a bare receiver as a transfer, which leaked `h` instead.
fn transferred_var(e: &AstExpr) -> Option<&str> {
    match &e.data {
        AstExprData::VarRef { name, .. } => Some(name.as_str()),
        AstExprData::FuncCall { name, args, .. }
            if name == "jeti_autorelease" && args.len() == 1 =>
        {
            match &args[0].data {
                AstExprData::VarRef { name, .. } => Some(name.as_str()),
                _ => None,
            }
        }
        AstExprData::MsgSend { receiver, selector, args, .. }
            if selector == "autorelease" && args.is_empty() =>
        {
            match &receiver.data {
                AstExprData::VarRef { name, .. } => Some(name.as_str()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn collect_expr_vars(e: &AstExpr, out: &mut Vec<String>) {
    match &e.data {
        AstExprData::VarRef { name, .. } => out.push(name.clone()),
        AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => collect_expr_vars(obj, out),
        AstExprData::Unary { operand, .. } => collect_expr_vars(operand, out),
        AstExprData::Cast { expr, .. } | AstExprData::Paren(expr) | AstExprData::Boxed(expr)
        | AstExprData::Await { inner: expr, .. } => collect_expr_vars(expr, out),
        AstExprData::Binary { left, right, .. } | AstExprData::Assign { target: left, value: right } => {
            collect_expr_vars(left, out);
            collect_expr_vars(right, out);
        }
        AstExprData::Ternary { cond, then, else_ } => {
            collect_expr_vars(cond, out);
            collect_expr_vars(then, out);
            collect_expr_vars(else_, out);
        }
        AstExprData::MsgSend { receiver, args, .. } => {
            collect_expr_vars(receiver, out);
            for a in args { collect_expr_vars(a, out); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { collect_expr_vars(c, out); }
            for a in args { collect_expr_vars(a, out); }
        }
        AstExprData::Subscript { object, key } => {
            collect_expr_vars(object, out);
            collect_expr_vars(key, out);
        }
        AstExprData::Comma(es) | AstExprData::ArrayLit(es) | AstExprData::InitList(es) => {
            for x in es { collect_expr_vars(x, out); }
        }
        AstExprData::DictLit { keys, values } => {
            for x in keys { collect_expr_vars(x, out); }
            for x in values { collect_expr_vars(x, out); }
        }
        AstExprData::DesignatedInit { expr, .. } => collect_expr_vars(expr, out),
        AstExprData::Sizeof { expr: Some(expr), .. } => collect_expr_vars(expr, out),
        // A block literal's body only RUNS later, but the literal itself is
        // evaluated here — its captures are established at that moment, so a
        // release that follows the evaluation is correctly ordered.
        AstExprData::Block { body: Some(body), .. } => collect_stmt_vars(body, out),
        _ => {}
    }
}

fn collect_stmt_vars(s: &AstStmt, out: &mut Vec<String>) {
    match &s.data {
        AstStmtData::Compound(v) => for x in v { collect_stmt_vars(x, out); },
        AstStmtData::Expr(e) => collect_expr_vars(e, out),
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => collect_expr_vars(e, out),
        AstStmtData::Decl(d) => {
            if let AstDeclData::Variable { init: Some(i), .. } = &d.data {
                collect_expr_vars(i, out);
            }
        }
        AstStmtData::If { cond, then, else_ } => {
            collect_expr_vars(cond, out);
            collect_stmt_vars(then, out);
            if let Some(e) = else_ { collect_stmt_vars(e, out); }
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            collect_expr_vars(cond, out);
            collect_stmt_vars(body, out);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(x) = init { collect_stmt_vars(x, out); }
            if let Some(x) = cond { collect_expr_vars(x, out); }
            if let Some(x) = incr { collect_expr_vars(x, out); }
            collect_stmt_vars(body, out);
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            collect_stmt_vars(try_block, out);
            for c in catches { collect_stmt_vars(c, out); }
            if let Some(f) = finally_block { collect_stmt_vars(f, out); }
        }
        AstStmtData::Catch { body, .. } => collect_stmt_vars(body, out),
        AstStmtData::Finally(b) => collect_stmt_vars(b, out),
        AstStmtData::Synchronized { lock, body } => {
            collect_expr_vars(lock, out);
            collect_stmt_vars(body, out);
        }
        AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body)
        | AstStmtData::Defer(body) => collect_stmt_vars(body, out),
        AstStmtData::Switch { expr, body } => {
            collect_expr_vars(expr, out);
            collect_stmt_vars(body, out);
        }
        AstStmtData::Case { value, body } => {
            collect_expr_vars(value, out);
            collect_stmt_vars(body, out);
        }
        AstStmtData::Default(body) => collect_stmt_vars(body, out),
        _ => {}
    }
}

/// Does the exit expression read any of `vars`?
fn exit_expr_reads(e: &AstExpr, vars: &[String]) -> bool {
    if vars.is_empty() { return false; }
    let mut names: Vec<String> = Vec::new();
    collect_expr_vars(e, &mut names);
    names.iter().any(|n| vars.iter().any(|v| v == n))
}

// ─── block-capture harvesting ───────────────────────────────────────────────
//
// A block literal runs later, but it is *constructed* where it appears, and it
// keeps referring to the outer locals it captures. If such a local is released
// before the block is called, the call reads freed memory. Harvest every name
// referenced from inside a block body so the exit lowering can stay
// conservative about it.

fn collect_captures_stmt(s: &AstStmt, out: &mut Vec<String>) {
    match &s.data {
        AstStmtData::Compound(v) => for x in v { collect_captures_stmt(x, out); },
        AstStmtData::Expr(e) => collect_captures_expr(e, out),
        AstStmtData::Decl(d) => {
            if let AstDeclData::Variable { init: Some(i), .. } = &d.data {
                collect_captures_expr(i, out);
            }
        }
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => collect_captures_expr(e, out),
        AstStmtData::If { cond, then, else_ } => {
            collect_captures_expr(cond, out);
            collect_captures_stmt(then, out);
            if let Some(e) = else_ { collect_captures_stmt(e, out); }
        }
        AstStmtData::While { cond, body } | AstStmtData::Do { body, cond } => {
            collect_captures_expr(cond, out);
            collect_captures_stmt(body, out);
        }
        AstStmtData::For { init, cond, incr, body } => {
            if let Some(x) = init { collect_captures_stmt(x, out); }
            if let Some(x) = cond { collect_captures_expr(x, out); }
            if let Some(x) = incr { collect_captures_expr(x, out); }
            collect_captures_stmt(body, out);
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            collect_captures_stmt(try_block, out);
            for c in catches { collect_captures_stmt(c, out); }
            if let Some(f) = finally_block { collect_captures_stmt(f, out); }
        }
        AstStmtData::Catch { body, .. } => collect_captures_stmt(body, out),
        AstStmtData::Finally(b) => collect_captures_stmt(b, out),
        AstStmtData::Synchronized { lock, body } => {
            collect_captures_expr(lock, out);
            collect_captures_stmt(body, out);
        }
        AstStmtData::Autoreleasepool(b) | AstStmtData::NoArc(b) | AstStmtData::Defer(b) => {
            collect_captures_stmt(b, out);
        }
        AstStmtData::Switch { expr, body } => {
            collect_captures_expr(expr, out);
            collect_captures_stmt(body, out);
        }
        AstStmtData::Case { value, body } => {
            collect_captures_expr(value, out);
            collect_captures_stmt(body, out);
        }
        AstStmtData::Default(b) => collect_captures_stmt(b, out),
        _ => {}
    }
}

fn collect_captures_expr(e: &AstExpr, out: &mut Vec<String>) {
    match &e.data {
        // The body's references ARE the capture set.
        AstExprData::Block { body: Some(b), .. } => collect_stmt_vars(b, out),
        AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => collect_captures_expr(obj, out),
        AstExprData::Unary { operand, .. } => collect_captures_expr(operand, out),
        AstExprData::Cast { expr, .. } | AstExprData::Paren(expr) | AstExprData::Boxed(expr)
        | AstExprData::Await { inner: expr, .. } => collect_captures_expr(expr, out),
        AstExprData::Binary { left, right, .. } | AstExprData::Assign { target: left, value: right } => {
            collect_captures_expr(left, out);
            collect_captures_expr(right, out);
        }
        AstExprData::Ternary { cond, then, else_ } => {
            collect_captures_expr(cond, out);
            collect_captures_expr(then, out);
            collect_captures_expr(else_, out);
        }
        AstExprData::MsgSend { receiver, args, .. } => {
            collect_captures_expr(receiver, out);
            for a in args { collect_captures_expr(a, out); }
        }
        AstExprData::FuncCall { callee, args, .. } => {
            if let Some(c) = callee { collect_captures_expr(c, out); }
            for a in args { collect_captures_expr(a, out); }
        }
        AstExprData::Subscript { object, key } => {
            collect_captures_expr(object, out);
            collect_captures_expr(key, out);
        }
        AstExprData::Comma(es) | AstExprData::ArrayLit(es) | AstExprData::InitList(es) => {
            for x in es { collect_captures_expr(x, out); }
        }
        AstExprData::DictLit { keys, values } => {
            for x in keys { collect_captures_expr(x, out); }
            for x in values { collect_captures_expr(x, out); }
        }
        AstExprData::DesignatedInit { expr, .. } => collect_captures_expr(expr, out),
        _ => {}
    }
}

// ─── goto / label scope analysis ────────────────────────────────────────────
//
// C labels are function-scoped, so `goto` can jump *out of* nested blocks —
// nobody releases the locals of the scopes it leaves behind, which leaks every
// owned object declared there (the pattern-switch lowering emits exactly this
// shape: arm bodies live in nested compounds and `goto __jeti_swN_end` jumps
// out of them).
//
// Each statement list is identified by its SCOPE PATH: the sequence of
// enclosing-statement indices from the function body down. That is stricter
// than a plain depth — two sibling blocks sit at the same depth but have
// different paths, and jumping between them skips initializers. A `goto` is
// legal exactly when the target label's path is a PREFIX of the goto's path
// (the label is in an enclosing scope, or in the same list).

/// What to do for one `goto`: release `collect_vars(stack, from)` before the
/// jump. `from == None` marks a jump the analysis refuses (reported in
/// `ArcResult::errors`).
#[derive(Debug, Clone)]
pub struct GotoCleanup {
    pub line: usize,
    pub col: usize,
    pub from: Option<usize>,
}

struct RawGoto {
    line: usize,
    col: usize,
    label: String,
    path: Vec<usize>,
}

struct LabelInfo {
    name: String,
    path: Vec<usize>,
    index_in_list: usize,
}

fn walk_labels_gotos_stmt(s: &AstStmt, idx: usize, path: &mut Vec<usize>,
                          labels: &mut Vec<LabelInfo>, gotos: &mut Vec<RawGoto>) {
    path.push(idx);
    match &s.data {
        AstStmtData::Compound(v) => walk_labels_gotos(v, path, labels, gotos),
        other => {
            let single = AstStmt { kind: s.kind, line: s.line, col: s.col, data: other.clone(), end_line: s.end_line, end_col: s.end_col };
            walk_labels_gotos(std::slice::from_ref(&single), path, labels, gotos);
            // NOTE: the clone above is only used for traversal; nothing here
            // mutates the AST (the rewriting pass walks separately).
        }
    }
    path.pop();
}

fn walk_labels_gotos(list: &[AstStmt], path: &mut Vec<usize>,
                     labels: &mut Vec<LabelInfo>, gotos: &mut Vec<RawGoto>) {
    for (i, s) in list.iter().enumerate() {
        match &s.data {
            // `index_in_list == 0` means the label heads its own block: a jump
            // to it cannot skip an initializer. The pattern-switch lowering
            // emits exactly the shape `{ label: body }`.
            AstStmtData::Label(n) => labels.push(LabelInfo {
                name: n.clone(),
                path: path.clone(),
                index_in_list: i,
            }),
            AstStmtData::Goto(n) => gotos.push(RawGoto {
                line: s.line, col: s.col, label: n.clone(), path: path.clone(),
            }),
            _ => {}
        }
        match &s.data {
            // NOTE: `walk_labels_gotos_stmt` descends into a *compound*; for
            // the wrapper statements below the BODY must be passed, never the
            // wrapper itself — passing it would clone a non-compound and
            // recurse forever.
            AstStmtData::Compound(_) => walk_labels_gotos_stmt(s, i * 2, path, labels, gotos),
            AstStmtData::Autoreleasepool(b) => walk_labels_gotos_stmt(b, i * 2, path, labels, gotos),
            AstStmtData::NoArc(b) | AstStmtData::Defer(b) => walk_labels_gotos_stmt(b, i * 2, path, labels, gotos),
            AstStmtData::Synchronized { body, .. } => walk_labels_gotos_stmt(body, i * 2, path, labels, gotos),
            AstStmtData::If { then, else_, .. } => {
                walk_labels_gotos_stmt(then, i * 2, path, labels, gotos);
                if let Some(e) = else_ { walk_labels_gotos_stmt(e, i * 2 + 1, path, labels, gotos); }
            }
            AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
            | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. } => {
                walk_labels_gotos_stmt(body, i * 2, path, labels, gotos);
            }
            AstStmtData::Try { try_block, catches, finally_block } => {
                walk_labels_gotos_stmt(try_block, i * 2, path, labels, gotos);
                for (k, c) in catches.iter().enumerate() {
                    walk_labels_gotos_stmt(c, i * 2 + 1 + k, path, labels, gotos);
                }
                if let Some(f) = finally_block {
                    walk_labels_gotos_stmt(f, i * 2 + 64, path, labels, gotos);
                }
            }
            AstStmtData::Catch { body, .. } => walk_labels_gotos_stmt(body, i * 2, path, labels, gotos),
            AstStmtData::Finally(b) => walk_labels_gotos_stmt(b, i * 2, path, labels, gotos),
            _ => {}
        }
    }
}

/// Turn the collected labels/gotos into per-`goto` cleanup plans.
fn build_goto_plan(body: &AstStmt, errors: &mut Vec<String>) -> Vec<GotoCleanup> {
    let mut labels: Vec<LabelInfo> = Vec::new();
    let mut gotos: Vec<RawGoto> = Vec::new();
    let mut path: Vec<usize> = Vec::new();
    if let AstStmtData::Compound(v) = &body.data {
        walk_labels_gotos(v, &mut path, &mut labels, &mut gotos);
    }

    let mut plan: Vec<GotoCleanup> = Vec::new();
    for g in gotos {
        let target = labels.iter().rev().find(|l| l.name == g.label);
        let Some(t) = target else {
            // Unknown label (or one that only exists in another function):
            // leave it to the C compiler.
            continue;
        };

        // Longest common scope prefix: the scopes the jump actually leaves.
        let common = g
            .path
            .iter()
            .zip(t.path.iter())
            .take_while(|(a, b)| a == b)
            .count();

        if common == t.path.len() {
            // The label is in an enclosing list (or the same one): a plain
            // scope exit — release everything the jump leaves behind.
            plan.push(GotoCleanup { line: g.line, col: g.col, from: Some(common) });
        } else if t.index_in_list == 0 {
            // Jumping INTO a block, but landing on its very first statement:
            // no initializer is skipped (this is the pattern-switch shape
            // `{ label: body }`). Still a scope exit, so release up to the
            // common ancestor.
            plan.push(GotoCleanup { line: g.line, col: g.col, from: Some(common) });
        } else {
            errors.push(format!(
                "goto '{}' jumps into a nested or sibling scope past its first statement; \
                 an owned object declared there could be released before it is initialized",
                g.label
            ));
            plan.push(GotoCleanup { line: g.line, col: g.col, from: None });
        }
    }
    plan
}

fn make_tmp_decl(name: &str, init: AstExpr) -> AstStmt {
    let mut ty = AstType::new(TypePrim::Named);
    ty.name = Some("__auto_type".to_string());
    AstStmt {
        kind: AstStmtKind::Decl, line: 0, col: 0,
        data: AstStmtData::Decl(AstDecl {
            kind: AstDeclKind::Variable,
            name: Some(name.to_string()),
            line: 0, col: 0,
            data: AstDeclData::Variable {
                var_type: Some(Box::new(ty)),
                init: Some(Box::new(init)),
                is_static: false, is_extern: false, is_const: false,
                is_block_qual: false, is_weak: false,
                next: None,
            },
            attributes: Vec::new(),
            end_line: 0,
            end_col: 0,
        }),
        end_line: 0,
        end_col: 0,
    }
}

/// Insert the exit-point cleanup for `return` / `@throw` at `pos`.
///
/// `vars` is drained (released locals removed). Returns how far the caller
/// must advance its statement index past what was (re)written here.
fn insert_exit_cleanup(
    stmts: &mut Vec<AstStmt>,
    pos: usize,
    vars: &mut Vec<String>,
    exit_expr: Option<&AstExpr>,
    is_throw: bool,
    captured: &[String],
) -> usize {
    // Rule 1: ownership transfer — `return h;` / `@throw h;`.
    if let Some(e) = exit_expr {
        if let Some(t) = transferred_var(e) {
            vars.retain(|v| v != t);
        }
    }
    if vars.is_empty() { return 1; }

    // Either the expression mentions the local outright, or the local is
    // captured by a block that the expression might call. Both force the
    // evaluate-then-release order.
    let needs_tmp = exit_expr.map_or(false, |e| exit_expr_reads(e, vars))
        || vars.iter().any(|v| captured.iter().any(|c| c == v));
    // Reverse declaration order: locals unwind like a stack, and this is the
    // order ARC has always emitted (tests/golden/28_refcount_trace pins it).
    let releases: Vec<AstStmt> = vars
        .iter()
        .rev()
        .map(|n| make_release_stmt(&var_ref_expr(n)))
        .collect();
    let n_releases = releases.len();
    vars.clear();

    if !needs_tmp {
        // The exit expression does not read any of the locals: releasing first
        // is already correct, and avoids a needless temporary.
        for (k, r) in releases.into_iter().enumerate() {
            stmts.insert(pos + k, r);
        }
        return n_releases + 1;
    }

    // Rule 2: evaluate the expression into a temporary, THEN release, THEN exit.
    // The temporary carries no ownership of its own — it is a plain copy of the
    // already-computed value, so no retain/release is introduced.
    let seq = ARC_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = format!("__jeti_arc_ret_{}", seq);
    let expr = exit_expr.expect("needs_tmp implies an expression").clone();
    let exit_stmt = if is_throw {
        AstStmt {
            kind: AstStmtKind::Throw, line: 0, col: 0,
            data: AstStmtData::Throw(Some(Box::new(var_ref_expr(&tmp)))),
            end_line: 0,
            end_col: 0,
        }
    } else {
        AstStmt {
            kind: AstStmtKind::Return, line: 0, col: 0,
            data: AstStmtData::Return(Some(Box::new(var_ref_expr(&tmp)))),
            end_line: 0,
            end_col: 0,
        }
    };

    let mut rewritten = Vec::with_capacity(n_releases + 2);
    rewritten.push(make_tmp_decl(&tmp, expr));
    rewritten.extend(releases);
    rewritten.push(exit_stmt);
    let count = rewritten.len();

    stmts.remove(pos);
    for (k, s) in rewritten.into_iter().enumerate() {
        stmts.insert(pos + k, s);
    }
    count
}

// ─── local analysis (directly inserts releases into AST) ───────────────────

// One scope frame on the analysis stack. `vars` are retained object locals
// declared in this scope; `declared` lists every variable declared in this
// scope (object-typed) so a later assignment like `t = [[T alloc] init]` after
// a bare `T *t;` can be registered as a retained local. Statics/globals are
// never declared inside the analyzed body, so they stay exempt. `inside_loop`
// marks a loop body so that break/continue only release variables declared
// within the loop (not its enclosing scopes).
struct Scope {
    vars: Vec<String>,
    declared: Vec<String>,
    inside_loop: bool,
}

impl Scope {
    fn new(inside_loop: bool) -> Self { Scope { vars: Vec::new(), declared: Vec::new(), inside_loop } }
}

// Collect all live vars in scopes `stack[from..]` (deduped, top-level last).
fn collect_vars(stack: &[Scope], from: usize) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for s in stack[from.min(stack.len())..].iter() {
        for n in s.vars.iter() {
            if !v.contains(n) { v.push(n.clone()); }
        }
    }
    v
}

pub fn arc_local_analyze(body: &mut AstStmt, _cfg: &Cfg, method_name: &str) -> ArcResult {
    let _ = ownership_for_method(method_name);
    let mut res = ArcResult::new();

    // Harvest block captures and label depths first: the statement walk needs
    // both while it rewrites the body in place.
    collect_captures_stmt(body, &mut res.captured);
    let plan = build_goto_plan(body, &mut res.errors);
    res.goto_plan = plan;

    let stmts = match body.data {
        AstStmtData::Compound(ref mut stmts) => stmts,
        _ => return res,
    };

    /// `throw_floor` is the scope-stack depth of the nearest enclosing `@try`
    /// body (`0` when there is none). A `@throw` only unwinds as far as that
    /// `@try`: the handler is in the same function, so locals declared *outside*
    /// the `@try` are still live afterwards and must NOT be released on the
    /// throwing path — releasing them there (and again at scope end) is a
    /// double free. Locals declared *inside* the `@try` really do go away with
    /// it and are released at the throw site.
    fn analyze_scope(stmts: &mut Vec<AstStmt>, res: &mut ArcResult, stack: &mut Vec<Scope>, inside_loop: bool, throw_floor: usize) {
        stack.push(Scope::new(inside_loop));
        let my_idx = stack.len() - 1;

        let mut i = 0;
        while i < stmts.len() {
            // ── @noarc blocks: skip entirely (no ARC analysis) ──
            if matches!(stmts[i].data, AstStmtData::NoArc(_)) {
                i += 1;
                continue;
            }
            // ── Handle nested scopes (recurse) ──
            if is_scope_stmt(&stmts[i]) {
                let inner: *mut Vec<AstStmt> = match &mut stmts[i].data {
                    AstStmtData::Compound(ref mut inner) => inner as *mut Vec<AstStmt>,
                    AstStmtData::Autoreleasepool(ref mut body) => {
                        if let AstStmtData::Compound(ref mut inner) = body.data { inner as *mut Vec<AstStmt> } else { std::ptr::null_mut() }
                    }
                    AstStmtData::Synchronized { ref mut body, .. } => {
                        if let AstStmtData::Compound(ref mut inner) = body.data { inner as *mut Vec<AstStmt> } else { std::ptr::null_mut() }
                    }
                    _ => std::ptr::null_mut(),
                };
                if !inner.is_null() {
                    unsafe { recurse_scope(&mut *inner, res, stack, throw_floor); }
                }
                i += 1;
                continue;
            }

            // ── Handle variable declarations ──
            if let AstStmtData::Decl(d) = &stmts[i].data {
                if let AstDeclData::Variable { init: Some(ref init_val), ref var_type, .. } = d.data {
                    if ownership_for_expr(init_val) == Ownership::Retained {
                        if let Some(ref name) = d.name {
                            if var_type.as_ref().map_or(false, |t| is_object_type(t)) {
                                stack[my_idx].vars.push(name.clone());
                            }
                        }
                    }
                }
                if let Some(ref name) = d.name {
                    stack[my_idx].declared.push(name.clone());
                }
                i += 1;
                continue;
            }

            // ── Handle break/continue: release ONLY vars inside the nearest loop ──
            if matches!(stmts[i].kind, AstStmtKind::Break | AstStmtKind::Continue) {
                let mut from = stack.len();
                for idx in (0..stack.len()).rev() {
                    from = idx;
                    if stack[idx].inside_loop { break; }
                }
                let mut to_release = collect_vars(stack, from);
                let n = insert_exit_cleanup(stmts, i, &mut to_release, None, false, &res.captured);
                for s in stack[from..].iter_mut() { s.vars.clear(); }
                i += n;
                continue;
            }

            // ── Handle return: release all live vars (current + enclosing), then exit ──
            //
            // NOTE: the live set is deliberately NOT cleared afterwards. Each
            // exit point gets its own cleanup on its own path, so a returning
            // branch does not rob the fall-through path of its release (clearing
            // here used to leak every subsequent path's locals — see
            // tests/arc_order/05_early_return).
            let is_return = matches!(stmts[i].data, AstStmtData::Return(_));
            if is_return {
                let ret_expr = match &stmts[i].data { AstStmtData::Return(e) => e.clone(), _ => None };
                let mut all = collect_vars(stack, 0);
                let _ = insert_exit_cleanup(
                    stmts, i, &mut all, ret_expr.as_ref().map(|e| e.as_ref()), false, &res.captured,
                );
                // A `return` statement leaves the scope unconditionally: the
                // statements after it are unreachable, so analyzing them would
                // inject cleanup that can never run (dead code, and spurious
                // double-release reports from -trace-refcount). Stop here — the
                // scope-end insertion below is skipped for the same reason.
                stack.pop();
                return;
            }

            // ── Handle throw: evaluate the thrown expression, release, rethrow ──
            if matches!(stmts[i].data, AstStmtData::Throw(_)) {
                let throw_expr = match &stmts[i].data { AstStmtData::Throw(e) => e.clone(), _ => None };
                // Only the locals that the @try boundary actually loses get
                // released here (see `throw_floor`). Outer locals stay live for
                // the handler in this same function and are released later, on
                // their own path.
                let mut all = collect_vars(stack, throw_floor);
                let _ = insert_exit_cleanup(
                    stmts, i, &mut all, throw_expr.as_ref().map(|e| e.as_ref()), true, &res.captured,
                );
                // `@throw` leaves the scope unconditionally — same reasoning as
                // the `return` arm above.
                stack.pop();
                return;
            }

            // ── Handle goto: release the locals of every scope it jumps out of ──
            //
            // C labels are function-scoped, so a `goto` that leaves a nested
            // block would otherwise leak every owned local declared there (the
            // pattern-switch lowering emits exactly this: arm bodies sit in
            // nested compounds and jump out to the switch's end label). The
            // locals stay registered in their scopes: any path that reaches the
            // scope end without taking this goto still releases them there, so
            // each path releases exactly once.
            if let AstStmtData::Goto(_) = &stmts[i].data {
                let (line, col) = (stmts[i].line, stmts[i].col);
                let planned = res
                    .goto_plan
                    .iter()
                    .find(|g| g.line == line && g.col == col)
                    .and_then(|g| g.from);
                if let Some(from) = planned {
                    let mut leaving = collect_vars(stack, from);
                    let n = insert_exit_cleanup(stmts, i, &mut leaving, None, false, &res.captured);
                    i += n;
                } else {
                    // Either an illegal jump (already an error, the pipeline
                    // refuses to emit) or a label outside analysis — leave the
                    // statement untouched.
                    i += 1;
                }
                continue;
            }

            // ── Handle if/else: recurse into branches ──
            if stmts[i].kind == AstStmtKind::If {
                let if_taken = std::mem::replace(&mut stmts[i].data, AstStmtData::Expr(AstExpr { kind: AstExprKind::Int, expr_type: None, line: 0, col: 0, data: AstExprData::Int(0), end_line: 0, end_col: 0 }));
                if let AstStmtData::If { cond, mut then, mut else_ } = if_taken {
                    if !matches!(then.data, AstStmtData::Compound(_)) {
                        let body = std::mem::replace(&mut then.data, AstStmtData::Compound(vec![]));
                        then.data = AstStmtData::Compound(vec![AstStmt { kind: then.kind, line: then.line, col: then.col, data: body, end_line: then.end_line, end_col: then.end_col }]);
                    }
                    if let AstStmtData::Compound(ref mut inner) = then.data {
                        analyze_scope(inner, res, stack, false, throw_floor);
                    }
                    if let Some(ref mut el) = else_ {
                        if !matches!(el.data, AstStmtData::Compound(_)) {
                            let body = std::mem::replace(&mut el.data, AstStmtData::Compound(vec![]));
                            el.data = AstStmtData::Compound(vec![AstStmt { kind: el.kind, line: el.line, col: el.col, data: body, end_line: el.end_line, end_col: el.end_col }]);
                        }
                        if let AstStmtData::Compound(ref mut inner) = el.data {
                            analyze_scope(inner, res, stack, false, throw_floor);
                        }
                    }
                    stmts[i].data = AstStmtData::If { cond, then, else_ };
                }
                i += 1;
                continue;
            }

            // ── Handle while, for, for-in: recurse into body (loop scope) ──
            let is_loop = matches!(stmts[i].kind, AstStmtKind::While | AstStmtKind::For | AstStmtKind::ForIn);
            if is_loop {
                let loop_taken = std::mem::replace(&mut stmts[i].data, AstStmtData::Expr(AstExpr { kind: AstExprKind::Int, expr_type: None, line: 0, col: 0, data: AstExprData::Int(0), end_line: 0, end_col: 0 }));
                match loop_taken {
                    AstStmtData::While { cond, mut body } => {
                        recurse_loop_body(&mut body, res, stack, throw_floor);
                        stmts[i].data = AstStmtData::While { cond, body };
                    }
                    AstStmtData::For { init, cond, incr, mut body } => {
                        // Register an object declared in the for-init (e.g. `for (Mini *o = [[Mini alloc] init]; ...)`)
                        // in the enclosing scope so it's released at scope end. Its declaration
                        // is hoisted out of the for header by codegen.
                        if let Some(ref init_stmt) = init {
                            if let AstStmtData::Decl(d) = &init_stmt.data {
                                if let AstDeclData::Variable { init: Some(ref init_val), ref var_type, .. } = d.data {
                                    if ownership_for_expr(init_val) == Ownership::Retained {
                                        if let Some(ref name) = d.name {
                                            if var_type.as_ref().map_or(false, |t| is_object_type(t)) {
                                                stack[my_idx].vars.push(name.clone());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        recurse_loop_body(&mut body, res, stack, throw_floor);
                        stmts[i].data = AstStmtData::For { init, cond, incr, body };
                    }
                    AstStmtData::ForIn { var, collection, mut body } => {
                        recurse_loop_body(&mut body, res, stack, throw_floor);
                        stmts[i].data = AstStmtData::ForIn { var, collection, body };
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }

            // ── Handle @try/@catch/@finally ──
            if stmts[i].kind == AstStmtKind::Try {
                let try_taken = std::mem::replace(&mut stmts[i].data, AstStmtData::Expr(AstExpr { kind: AstExprKind::Int, expr_type: None, line: 0, col: 0, data: AstExprData::Int(0), end_line: 0, end_col: 0 }));
                if let AstStmtData::Try { mut try_block, mut catches, mut finally_block } = try_taken {
                    // Everything analyzed from here on is inside this @try (or
                    // its handlers): a `@throw` unwinds no further than this
                    // point, so that depth is the new throw_floor. A nested @try
                    // inside installs its own, deeper floor.
                    let floor = stack.len();
                    if let AstStmtData::Compound(ref mut inner) = try_block.data { analyze_scope(inner, res, stack, false, floor); }
                    for c in catches.iter_mut() {
                        if let AstStmtData::Catch { ref mut body, .. } = c.data {
                            if let AstStmtData::Compound(ref mut inner) = body.data { analyze_scope(inner, res, stack, false, floor); }
                        }
                    }
                    if let Some(ref mut fb) = finally_block {
                        if let AstStmtData::Compound(ref mut inner) = fb.data { analyze_scope(inner, res, stack, false, floor); }
                    }
                    stmts[i].data = AstStmtData::Try { try_block, catches, finally_block };
                }
                i += 1;
                continue;
            }

            // ── Handle manual `[var release]` / `jeti_release(var)`: forget var everywhere ──
            if let AstStmtData::Expr(e) = &stmts[i].data {
                match &e.data {
                    AstExprData::MsgSend { receiver, selector, args, .. } if selector == "release" && args.is_empty() => {
                        if let AstExprData::VarRef { name, .. } = &receiver.data {
                            drop_var(stack, name);
                        }
                    }
                    AstExprData::FuncCall { name, args, .. } if name == "jeti_release" && args.len() == 1 => {
                        if let AstExprData::VarRef { name, .. } = &args[0].data {
                            drop_var(stack, name);
                        }
                    }
                    AstExprData::Assign { target, value } => {
                        // `t = [[T alloc] init]` written after a bare `T *t;`
                        // declaration: the variable now owns a retained object.
                        // Only register it if it was declared in the current
                        // scope (never statics/globals, which are declared at
                        // impl/namespace level and are exempt from ARC).
                        if ownership_for_expr(value) == Ownership::Retained {
                            if let AstExprData::VarRef { name, .. } = &target.data {
                                let declared_here = stack[my_idx].declared.contains(name);
                                let already = collect_vars(stack, my_idx).contains(name);
                                if declared_here && !already {
                                    stack[my_idx].vars.push(name.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            i += 1;
        }

        // ── End of scope: release this scope's own remaining vars ──
        //
        // Skipped when the scope ends in an unconditional exit (`return` /
        // `@throw`): that statement already released every live local on its own
        // path, and the fall-out path does not exist. Inserting here as well
        // would release twice on the exiting path.
        let exits_unconditionally = stmts
            .last()
            .map_or(false, |s| matches!(s.data, AstStmtData::Return(_) | AstStmtData::Throw(_)));
        if !exits_unconditionally {
            // Reverse declaration order, matching the exit-point order.
            for name in stack[my_idx].vars.iter().rev() {
                stmts.push(make_release_stmt(&var_ref_expr(name)));
            }
        }
        stack.pop();
    }

    fn drop_var(stack: &mut Vec<Scope>, name: &str) {
        for s in stack.iter_mut() {
            s.vars.retain(|v| v != name);
        }
    }

    fn recurse_scope(inner: &mut Vec<AstStmt>, res: &mut ArcResult, stack: &mut Vec<Scope>, throw_floor: usize) {
        analyze_scope(inner, res, stack, false, throw_floor);
    }

    fn recurse_loop_body(body: &mut AstStmt, res: &mut ArcResult, stack: &mut Vec<Scope>, throw_floor: usize) {
        // Wrap non-compound bodies so releases are inserted in a scope.
        let inner: *mut Vec<AstStmt> = match body.data {
            AstStmtData::Compound(ref mut inner) => inner as *mut Vec<AstStmt>,
            _ => {
                let wrapped = std::mem::replace(&mut body.data, AstStmtData::Compound(vec![]));
                if let AstStmtData::Compound(ref mut v) = body.data {
                    v.push(AstStmt { kind: body.kind, line: body.line, col: body.col, data: wrapped, end_line: body.end_line, end_col: body.end_col });
                }
                match body.data {
                    AstStmtData::Compound(ref mut inner) => inner as *mut Vec<AstStmt>,
                    _ => std::ptr::null_mut(),
                }
            }
        };
        if !inner.is_null() {
            unsafe { analyze_scope(&mut *inner, res, stack, true, throw_floor); }
        }
    }

    let mut stack: Vec<Scope> = Vec::new();
    // No enclosing @try at the top level: a throw here leaves the function.
    analyze_scope(stmts, &mut res, &mut stack, false, 0);
    res
}

// ─── global analysis (placeholder) ─────────────────────────────────────────

pub fn arc_global_analyze(_cfg: &Cfg, _res: &mut ArcResult, _method_name: &str) {}

// ─── loop analysis (placeholder) ───────────────────────────────────────────

pub fn arc_analyze_loops(_cfg: &Cfg, _res: &mut ArcResult, _method_name: &str) {}

// ─── retain/release insertion (no-op now, analysis inserts directly) ───────

pub fn arc_insert_actions(_body: &mut AstStmt, _res: &ArcResult) {}

// ─── redundant pair optimization ───────────────────────────────────────────

pub fn arc_optimize_pairs(body: &mut AstStmt) {
    let stmts = match &mut body.data {
        AstStmtData::Compound(ref mut s) => s,
        _ => return,
    };
    let mut remove: Vec<usize> = Vec::new();
    for i in 0..stmts.len().saturating_sub(1) {
        if remove.contains(&i) { continue; }
        if let Some(j) = is_retain_release_pair(&stmts[i], &stmts[i + 1]) {
            if j { remove.push(i); remove.push(i + 1); }
        }
    }
    if remove.is_empty() { return; }
    let mut write = 0;
    for i in 0..stmts.len() {
        if !remove.contains(&i) {
            stmts[write] = stmts[i].clone();
            write += 1;
        }
    }
    stmts.truncate(write);
}

fn is_retain_release_pair(s1: &AstStmt, s2: &AstStmt) -> Option<bool> {
    let e1 = match &s1.data { AstStmtData::Expr(e) => e, _ => return None };
    let e2 = match &s2.data { AstStmtData::Expr(e) => e, _ => return None };
    let (name1, args1) = match &e1.data { AstExprData::FuncCall { name, args, .. } => (name, args), _ => return None };
    let (name2, args2) = match &e2.data { AstExprData::FuncCall { name, args, .. } => (name, args), _ => return None };
    if args1.len() != 1 || args2.len() != 1 { return None; }
    // The synthesize helper wraps release/retain args in an `(NPObject *)`
    // upcast cast — unwrap it when identifying the variable (the cast is a
    // pure C-pointer spelling, not a different slot).
    let var_of = |a: &AstExpr| -> Option<String> {
        match &a.data {
            AstExprData::VarRef { name, .. } => Some(name.clone()),
            AstExprData::Cast { expr, .. } => match &expr.data {
                AstExprData::VarRef { name, .. } => Some(name.clone()),
                _ => None,
            },
            _ => None,
        }
    };
    let t1 = var_of(&args1[0])?;
    let t2 = var_of(&args2[0])?;
    if t1 != t2 { return None; }
    let is_retain = |n: &str| n == "jeti_retain";
    let is_release = |n: &str| n == "jeti_release";
    if (is_retain(name1) && is_release(name2)) || (is_release(name1) && is_retain(name2)) {
        Some(true)
    } else {
        None
    }
}
// ─── returned-object rule (ObjC ARC) ────────────────────────────────────────

/// ObjC ARC's "returned object" rule: a method outside the
/// alloc/new/copy/mutableCopy family that returns a freshly created (+1)
/// value must hand it to the caller at +0 — clang balances this by
/// autoreleasing the return. Without it the value leaks: the caller never
/// releases a non-alloc selector's result, and nothing else balances it
/// (found by the refcount tracer — `+make { return [[X alloc] init]; }`
/// compiled to a value nobody releases). init-family methods legitimately
/// return +1 (`self`); a thrown value is balanced by the EH machinery
/// (`@throw` retains/autoreleases/releases it), so only `return` wraps.
pub fn autorelease_owned_returns(body: &mut AstStmt, selector: &str) {
    if selector.starts_with("alloc")
        || selector.starts_with("new")
        || selector.starts_with("copy")
        || selector.starts_with("mutableCopy")
        || selector.starts_with("init")
    {
        return;
    }
    autorelease_returns_stmt(body);
}

/// A +1 value: an alloc/new/copy/mutableCopy send, or an `init` send on one.
/// Literals are excluded — `return @"x"` hands back a constant.
fn expr_is_owned_creation(e: &AstExpr) -> bool {
    match &e.data {
        AstExprData::MsgSend { receiver, selector, .. } => {
            let s = selector.as_str();
            if s.starts_with("alloc")
                || s.starts_with("new")
                || s.starts_with("copy")
                || s == "mutableCopy"
            {
                true
            } else if s.starts_with("init") {
                expr_is_owned_creation(receiver)
            } else {
                false
            }
        }
        _ => false,
    }
}

fn autorelease_returns_stmt(s: &mut AstStmt) {
    match &mut s.data {
        AstStmtData::Compound(v) => {
            for x in v.iter_mut() {
                autorelease_returns_stmt(x);
            }
        }
        AstStmtData::Return(Some(e)) => {
            if expr_is_owned_creation(e) {
                let (line, col) = (e.line, e.col);
                let inner = (**e).clone();
                *e = Box::new(AstExpr {
                    kind: AstExprKind::FuncCall,
                    expr_type: None,
                    line,
                    col,
                    data: AstExprData::FuncCall {
                        func: None,
                        name: "jeti_autorelease".to_string(),
                        callee: None,
                        args: vec![inner],
                    },
                    end_line: line,
                    end_col: col,
                });
            }
        }
        AstStmtData::If { then, else_, .. } => {
            autorelease_returns_stmt(then);
            if let Some(el) = else_ {
                autorelease_returns_stmt(el);
            }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. } => {
            autorelease_returns_stmt(body)
        }
        AstStmtData::For { init, body, .. } => {
            if let Some(i) = init {
                autorelease_returns_stmt(i);
            }
            autorelease_returns_stmt(body);
        }
        AstStmtData::ForIn { body, .. } => autorelease_returns_stmt(body),
        AstStmtData::Try { try_block, catches, finally_block } => {
            autorelease_returns_stmt(try_block);
            for c in catches.iter_mut() {
                autorelease_returns_stmt(c);
            }
            if let Some(f) = finally_block {
                autorelease_returns_stmt(f);
            }
        }
        AstStmtData::Catch { body, .. } => autorelease_returns_stmt(body),
        AstStmtData::Finally(b) => autorelease_returns_stmt(b),
        AstStmtData::Synchronized { body, .. } => autorelease_returns_stmt(body),
        AstStmtData::Autoreleasepool(b) | AstStmtData::NoArc(b) | AstStmtData::Defer(b) => {
            autorelease_returns_stmt(b)
        }
        _ => {}
    }
}
