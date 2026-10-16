//! Pattern-switch lowering (`SwitchPat` → goto/if dispatch).
//!
//! Runs as pipeline Step 3.95 — AFTER eh desugar (3.85) and defer splicing
//! (3.9), BEFORE ARC (4). By the time ARC/checker/codegen see the AST there
//! are no pattern switches left: they only ever see plain C statements
//! (`Compound`/`If`/`Decl`/`Goto`/`Label`), exactly like the defer and eh
//! passes' contract. Codegen gets ZERO new arms.
//!
//! ## Lowering shape
//!
//! ```c
//! /* source */
//! switch (value) {
//!     case > 10:            big();
//!     case NSString *s when [s length] > 3: use(s);
//!     case @"hello":        greet();
//!     default:              other();
//! }
//! /* lowered */
//! {
//!     __auto_type __jeti_sw = value;               /* evaluated once */
//!     if (__jeti_sw > 10) goto __jeti_case_0;      /* Cond */
//!     if (jeti_isKindOfClass((NPObject *)__jeti_sw, &JETI_CLASS_$_NSString)
//!         && __jeti_s_len_guard) goto __jeti_case_1;   /* Bind + when */
//!     if ([__jeti_sw isEqual:@"hello"]) goto __jeti_case_2;  /* literal */
//!     goto __jeti_case_d;                          /* default */
//!     goto __jeti_sw_end;                          /* no default: skip */
//! __jeti_case_0:;
//!     big();
//!     goto __jeti_sw_end;
//! __jeti_case_1:;
//!     /* binding alias declared in the arm body scope */
//!     goto __jeti_sw_end;
//! ...
//! __jeti_sw_end:;
//! }
//! ```
//!
//! ## Break rewriting (scope-aware, defer-jump-set precedent)
//!
//! A `break` inside an arm body binds to the innermost enclosing loop/switch
//! in the ORIGINAL source. Once arms become if/goto bodies, a bare `break`
//! would bind to the enclosing `if`'s loop context wrongly (or escape the
//! switch entirely). So every `break` that belongs to THIS switch (i.e. seen
//! while no deeper loop/switch has been entered) is rewritten to
//! `goto __jeti_sw_end`. `break`s inside nested loops/switches are left
//! untouched — the same two-set discipline the defer pass uses (`pending` vs
//! `jump`).
//!
//! ## Fallthrough
//!
//! Without `break`, control falls from one arm into the next — the C
//! semantics, preserved by the `goto __jeti_case_{i+1}` chain: each arm's
//! body ends with `goto __jeti_sw_end` ONLY if the original body already
//! ended in break-like flow... actually simpler: each label block falls
//! through to the next label block naturally (labels are adjacent), and a
//! rewritten `break` jumps to the end. This reproduces C fallthrough
//! exactly: no synthetic gotos between arms.
//!
//! ## M1 limits (documented in AGENTS.md)
//! - `continue` inside an arm binds to the enclosing loop as before (arms
//!   don't change loop nesting) — untouched.
//! - Return/throw inside arms work naturally (plain C statements).
//! - Object-literal equality is `isEqual:` (value semantics); nil subject
//!   short-circuits to "no match" (messaging nil yields 0).
use jeti_ast::{AstArm, AstDecl, AstDeclData, AstDeclKind, AstExpr, AstExprData, AstExprKind,
              AstPattern, AstStmt, AstStmtData, AstStmtKind, AstType};
use jeti_cst::TypePrim;

/// Unit-level entry: lower every `SwitchPat` under every decl (methods,
/// functions, namespaces, class method lists). Same traversal shape as
/// `jeti_defer::desugar_unit` — namespaces must recurse or their switches
/// would reach codegen's catch-all and be silently dropped.
pub fn desugar_unit(unit: &mut jeti_ast::AstUnit) {
    for decl in &mut unit.decls {
        lower_decl(decl);
    }
}

fn lower_decl(decl: &mut AstDecl) {
    match &mut decl.data {
        AstDeclData::Function { body: Some(ref mut b), .. }
        | AstDeclData::Method { body: Some(ref mut b), .. } => lower_stmt(b),
        AstDeclData::Class { ref mut methods, .. } => {
            for m in methods.iter_mut() {
                lower_decl(m);
            }
        }
        AstDeclData::Namespace(members) => {
            for m in members.iter_mut() {
                lower_decl(m);
            }
        }
        _ => {}
    }
}

/// Lower every `SwitchPat` in the unit's statement trees. Called once per
/// decl body by the pipeline (Step 3.95). Returns nothing — lowering cannot
/// fail at M1 (malformed patterns are parse errors already).
pub fn lower_stmts(stmts: &mut Vec<AstStmt>) {
    for i in 0..stmts.len() {
        lower_stmt(&mut stmts[i]);
    }
}

pub fn lower_stmt(s: &mut AstStmt) {
    match &mut s.data {
        AstStmtData::Compound(inner) => lower_stmts(inner),
        AstStmtData::If { cond: _, then, else_ } => {
            lower_stmt(then);
            if let Some(e) = else_ { lower_stmt(e); }
        }
        AstStmtData::SwitchPat { .. } => {
            // Take the node, replace with lowered statements.
            let taken = std::mem::replace(s, make_compound());
            *s = lower_switch_pat(taken);
        }
        // Loops and plain switches recurse into their bodies (a SwitchPat
        // can sit inside them); everything else has no nested statements.
        AstStmtData::Switch { body, .. } => lower_stmt(body),
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
        | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. } => lower_stmt(body),
        AstStmtData::Try { try_block, catches, finally_block } => {
            lower_stmt(try_block);
            for c in catches.iter_mut() { lower_stmt(c); }
            if let Some(f) = finally_block { lower_stmt(f); }
        }
        AstStmtData::Catch { body, .. } => lower_stmt(body),
        AstStmtData::Synchronized { body, .. } | AstStmtData::Autoreleasepool(body)
        | AstStmtData::NoArc(body) | AstStmtData::Defer(body) => lower_stmt(body),
        _ => {}
    }
}

fn make_compound() -> AstStmt {
    AstStmt { kind: AstStmtKind::Compound, line: 0, col: 0, data: AstStmtData::Compound(Vec::new()), end_line: 0, end_col: 0 }
}

fn make_goto(label: &str, line: usize, col: usize) -> AstStmt {
    AstStmt { kind: AstStmtKind::Goto, line, col, data: AstStmtData::Goto(label.to_string()), end_line: 0, end_col: 0 }
}

fn make_label_wrap(label: &str, body: AstStmt, line: usize, col: usize) -> AstStmt {
    // AST's Label carries only the name; the labeled body follows as the
    // next statement. Wrap both so the pair stays adjacent.
    AstStmt {
        kind: AstStmtKind::Compound, line, col,
        data: AstStmtData::Compound(vec![
            AstStmt { kind: AstStmtKind::Label, line, col, data: AstStmtData::Label(label.to_string()), end_line: 0, end_col: 0 },
            body,
        ]),
        end_line: 0,
        end_col: 0,
    }
}

/// Lower one pattern switch into a compound of plain C statements.
fn lower_switch_pat(sw: AstStmt) -> AstStmt {
    let (subject, arms, has_default, default_body, line, col) = match sw.data {
        AstStmtData::SwitchPat { expr, arms, has_default, default_body } =>
            (expr, arms, has_default, default_body, sw.line, sw.col),
        _ => unreachable!("lower_switch_pat on non-SwitchPat"),
    };
    // Every lowered switch gets its own label namespace: C labels are
    // function-scoped, so nested pattern switches would otherwise emit
    // duplicate `__jeti_sw_end` / `__jeti_case_0` names (hard C error).
    let id = SWITCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let subj = "__jeti_sw".to_string();
    let end_label = format!("__jeti_sw{}_end", id);
    let default_label = format!("__jeti_case_{}_d", id);
    let mut out: Vec<AstStmt> = Vec::new();

    // 1. Materialize the subject exactly once. This pass runs BEFORE the
    //    checker, so the static type is unknown: an object-valued subject
    //    (Bind arm, or a value-semantics @literal arm) is declared
    //    `NPObject *` with the expression cast to it — `__auto_type` would
    //    inherit the pointer type and the checker rejects an object
    //    initializing a scalar. Scalar subjects (plain constants / dangling
    //    comparisons) keep `__auto_type` (eh precedent).
    let object_subject = arms.iter().any(|a| match &a.pattern {
        AstPattern::Bind { .. } => true,
        AstPattern::Const(e) => is_object_literal_expr(e),
        AstPattern::Cond(_) => false,
    });
    let init = *subject;
    let (sty, sinit) = if object_subject {
        (npobject_type(), cast_to_npobject(init))
    } else {
        (auto_type(), init)
    };
    out.push(var_decl(&subj, sty, Some(sinit), line, col));

    // 1b. Hoist every type binding above the dispatch chain. A `when` guard is
    //     evaluated in the `if (...) goto ...` tests, which run BEFORE any arm
    //     body — so `case NPNumber *n when n.intValue > 3:` referenced `n`
    //     before its declaration and the generated C failed to compile
    //     ("use of undeclared identifier 'n'"). Declaring the aliases here
    //     makes them visible to both the guards and the bodies. Each alias is
    //     just a pointer cast, so evaluating it for a non-matching arm is
    //     harmless; the type test remains the `isKindOfClass` call.
    //
    //     A name bound by more than one arm (pathological, but parseable:
    //     `case NPString *s:` + `case NPNumber *s:`) would be a C redeclaration
    //     in this shared scope, so only the first is hoisted; the later arm
    //     keeps its own in-body declaration. `hoisted[i]` records which arms got
    //     the shared declaration, so the body pass below knows not to
    //     re-declare it.
    let mut hoisted_names: Vec<String> = Vec::new();
    let mut hoisted: Vec<bool> = Vec::with_capacity(arms.len());
    for arm in &arms {
        match &arm.pattern {
            AstPattern::Bind { ty, name } if !hoisted_names.iter().any(|n| n == name) => {
                hoisted_names.push(name.clone());
                hoisted.push(true);
                out.push(bind_alias_decl(ty, name, &subj, arm.line, arm.col));
            }
            AstPattern::Bind { .. } => hoisted.push(false),
            _ => hoisted.push(false),
        }
    }

    // 2. Dispatch chain: one if per arm, in source order.
    for (i, arm) in arms.iter().enumerate() {
        let test = arm_test(arm, &subj);
        let lbl = case_label(id, i);
        let mut cond = test;
        if let Some(g) = &arm.guard {
            // guard ANDs with the pattern test (binding already visible via
            // the alias decl inside the body — guard uses the SUBJECT, which
            // is always in scope).
            let gline = g.line; let gcol = g.col;
            cond = AstExpr {
                kind: AstExprKind::Binary, expr_type: None, line: gline, col: gcol,
                data: AstExprData::Binary { op: 17, left: Box::new(cond), right: g.clone() },
                end_line: 0,
                end_col: 0,
            };
        }
        // `if (cond) goto lbl;` — an unmatched arm must keep testing the next
        // one, so the test can never fall through unconditionally.
        out.push(AstStmt {
            kind: AstStmtKind::If, line, col,
            data: AstStmtData::If {
                cond: Box::new(cond),
                then: Box::new(make_goto(&lbl, line, col)),
                else_: None,
            },
            end_line: 0,
            end_col: 0,
        });
    }
    // Default / no-default.
    if has_default {
        out.push(make_goto(&default_label, line, col));
    } else {
        out.push(make_goto(&end_label, line, col));
    }

    // 3. Arm bodies as adjacent labeled blocks (natural fallthrough like C).
    for (i, arm) in arms.into_iter().enumerate() {
        let AstArm { pattern, guard: _, body, line: aline, col: acol, .. } = &arm;
        let (aline, acol) = (*aline, *acol);
        let mut body = (**body).clone();
        let pattern = pattern.clone();
        // Bind arm: the alias is already declared in the enclosing scope (see
        // the hoist in step 1b) so `when` guards could see it. Re-declaring it
        // here would shadow that with the same value — legal C, but a
        // redundant declaration. `hoisted[i]` is false only for a later arm
        // reusing an already-hoisted name, which would be a C redeclaration in
        // the shared scope and so needs its own in-body declaration.
        if let AstPattern::Bind { ty, name } = &pattern {
            if !hoisted[i] {
                let alias = bind_alias_decl(ty, name, &subj, aline, acol);
                if let AstStmtData::Compound(inner) = &mut body.data {
                    inner.insert(0, alias);
                } else {
                    let inner = vec![alias, body.clone()];
                    body = AstStmt { kind: AstStmtKind::Compound, line: aline, col: acol, data: AstStmtData::Compound(inner), end_line: 0, end_col: 0 };
                }
            }
        }
        // Lower any pattern switch nested inside this arm body FIRST, so the
        // break rewrite below only ever sees plain C statements.
        lower_stmt(&mut body);
        // Rewrite breaks that belong to THIS switch — inner loops/switches
        // stop the walk (their breaks are theirs).
        rewrite_breaks(&mut body, &end_label);
        let lbl = case_label(id, i);
        out.push(make_label_wrap(&lbl, body, aline, acol));
    }
    if has_default {
        // Fallback arm: the `default:` body, already grouped with its
        // fallthrough siblings by the parser's flat-arm collector.
        let mut body = default_body.map(|b| *b).unwrap_or_else(make_compound);
        lower_stmt(&mut body);
        rewrite_breaks(&mut body, &end_label);
        out.push(make_label_wrap(&default_label, body, line, col));
    }

    out.push(make_label_wrap(&end_label, make_compound(), line, col));
    AstStmt { kind: AstStmtKind::Compound, line, col, data: AstStmtData::Compound(out), end_line: 0, end_col: 0 }
}

/// Unique tag source for one lowered switch (see `lower_switch_pat`).
static SWITCH_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn case_label(id: usize, i: usize) -> String { format!("__jeti_case_{}_{}", id, i) }

/// The if-test for one arm, splicing the subject where the pattern needs it.
fn arm_test(arm: &AstArm, subj: &str) -> AstExpr {
    let (line, col) = (arm.line, arm.col);
    match &arm.pattern {
        AstPattern::Const(e) => {
            // Object literal (@"x" / @1 / @YES / @'c' / @(expr)) → isEqual:
            // value semantics. Plain constant → `==` (the constant arm only
            // got here because is_object_literal flagged it, but stay safe).
            if is_object_literal_expr(e) {
                // [subj isEqual:<literal>]
                AstExpr {
                    kind: AstExprKind::MsgSend, expr_type: None, line, col,
                    data: AstExprData::MsgSend {
                        receiver: Box::new(ident(subj, line, col)),
                        method: None,
                        vtable_index: -1,
                        is_class_method: false,
                        is_super: false,
                        super_name: None,
                        selector: "isEqual:".to_string(),
                        args: vec![(**e).clone()],
                    },
                    end_line: 0,
                    end_col: 0,
                }
            } else {
                // subj == <const>
                AstExpr {
                    kind: AstExprKind::Binary, expr_type: None, line, col,
                    data: AstExprData::Binary { op: 12, left: Box::new(ident(subj, line, col)), right: Box::new((**e).clone()) },
                    end_line: 0,
                    end_col: 0,
                }
            }
        }
        AstPattern::Cond(e) => splice_subject(e, subj),
        AstPattern::Bind { ty, .. } => {
            // jeti_isKindOfClass((NPObject *)subj, &JETI_CLASS_$_<Flat>)
            let flat = flat_type_name(ty);
            AstExpr {
                kind: AstExprKind::FuncCall, expr_type: None, line, col,
                data: AstExprData::FuncCall {
                    func: None,
                    name: "jeti_isKindOfClass".to_string(),
                    callee: None,
                    args: vec![
                        cast_to_npobject(ident(subj, line, col)),
                        AstExpr {
                            kind: AstExprKind::VarRef, expr_type: None, line, col,
                            data: AstExprData::VarRef { sym: None, name: format!("&JETI_CLASS_$_{}", flat) },
                            end_line: 0,
                            end_col: 0,
                        },
                    ],
                },
                end_line: 0,
                end_col: 0,
            }
        }
    }
}

/// Replace empty-Ident subject placeholders in a Cond expression with real
/// subject references. Walks Binary chains; the placeholder is an Ident("")
/// on the LEFT of each comparison.
fn splice_subject(e: &AstExpr, subj: &str) -> AstExpr {
    let mut e = e.clone();
    splice_subject_in(&mut e, subj);
    e
}

fn splice_subject_in(e: &mut AstExpr, subj: &str) {
    match &mut e.data {
        AstExprData::Binary { left, right, .. } => {
            if let AstExprData::VarRef { name, .. } = &left.data {
                if name.is_empty() {
                    let (l, c) = (left.line, left.col);
                    **left = ident(subj, l, c);
                }
            }
            splice_subject_in(left, subj);
            splice_subject_in(right, subj);
        }
        _ => {}
    }
}

/// Rewrite `break` statements that belong to THIS switch (not to an inner
/// loop/switch) into `goto <end>` — the defer pass's jump-set discipline.
fn rewrite_breaks(s: &mut AstStmt, end_label: &str) {
    if matches!(s.kind, AstStmtKind::Break) {
        let (l, c) = (s.line, s.col);
        *s = make_goto(end_label, l, c);
        return;
    }
    match &mut s.data {
        // Entering a loop/switch stops the rewrite: a `break` inside binds to
        // that inner loop/switch, not to the pattern switch being lowered.
        // (SwitchPat can no longer occur here — arm bodies are lowered before
        // they are rewritten — but it is listed defensively.)
        AstStmtData::While { .. } | AstStmtData::Do { .. }
        | AstStmtData::For { .. } | AstStmtData::ForIn { .. }
        | AstStmtData::Switch { .. } | AstStmtData::SwitchPat { .. } => {}
        AstStmtData::Compound(inner) => {
            for st in inner.iter_mut() { rewrite_breaks(st, end_label); }
        }
        AstStmtData::If { then, else_, .. } => {
            rewrite_breaks(then, end_label);
            if let Some(e) = else_ { rewrite_breaks(e, end_label); }
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            rewrite_breaks(try_block, end_label);
            for c in catches.iter_mut() { rewrite_breaks(c, end_label); }
            if let Some(f) = finally_block { rewrite_breaks(f, end_label); }
        }
        AstStmtData::Catch { body, .. } => rewrite_breaks(body, end_label),
        AstStmtData::Synchronized { body, .. } | AstStmtData::Autoreleasepool(body)
        | AstStmtData::NoArc(body) => rewrite_breaks(body, end_label),
        _ => {}
    }
}

// ─── constructors ───────────────────────────────────────────────────────────

fn ident(name: &str, line: usize, col: usize) -> AstExpr {
    AstExpr {
        kind: AstExprKind::VarRef, expr_type: None, line, col,
        data: AstExprData::VarRef { sym: None, name: name.to_string() },
        end_line: 0,
        end_col: 0,
    }
}

/// `NPObject *` — the erased type every jeti object shares (subject and
/// binding casts both go through it).
fn npobject_type() -> AstType {
    let mut t = AstType::new(TypePrim::Named);
    t.name = Some("NPObject".to_string());
    t.is_pointer = true;
    t
}

fn cast_to_npobject(e: AstExpr) -> AstExpr {
    let t = npobject_type();
    let (l, c) = (e.line, e.col);
    AstExpr { kind: AstExprKind::Cast, expr_type: None, line: l, col: c, data: AstExprData::Cast { target_type: t, expr: Box::new(e) }, end_line: 0, end_col: 0 }
}

/// `__auto_type` — GNU type-inference; leaves downstream type handling
/// untouched (eh pass precedent, eh/src/lib.rs auto_type()).
fn auto_type() -> AstType {
    let mut t = AstType::new(TypePrim::Named);
    t.name = Some("__auto_type".to_string());
    t
}

/// A plain `T name = init;` declaration statement (eh precedent).
fn var_decl(name: &str, ty: AstType, init: Option<AstExpr>, line: usize, col: usize) -> AstStmt {
    AstStmt {
        kind: AstStmtKind::Decl,
        line, col,
        data: AstStmtData::Decl(AstDecl {
            kind: AstDeclKind::Variable,
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
            end_line: 0,
            end_col: 0,
        }),
        end_line: 0,
        end_col: 0,
    }
}

/// Bind-arm alias: `T *name = (T *)(NPObject *)__jeti_sw;` — declared inside
/// the arm body so scoping stays block-local (no cross-arm leakage).
fn bind_alias_decl(ty: &AstType, name: &str, subj: &str, line: usize, col: usize) -> AstStmt {
    let mut t = ty.clone();
    t.is_pointer = true;
    let subj_e = ident(subj, line, col);
    let cast = AstExpr {
        kind: AstExprKind::Cast, expr_type: None, line, col,
        data: AstExprData::Cast { target_type: t.clone(), expr: Box::new(cast_to_npobject(subj_e)) },
        end_line: 0,
        end_col: 0,
    };
    var_decl(name, t, Some(cast), line, col)
}

/// Flatten a bound type to its class-metadata symbol segment: `NSString` →
/// `NSString`; namespaced `NS::Obj` → `NS__Obj` (codegen's name_flat rule).
fn flat_type_name(ty: &AstType) -> String {
    let base = ty.name.clone().unwrap_or_else(|| "NPObject".to_string());
    base.replace("::", "__")
}

/// True when the arm label is an ObjC object literal, i.e. a *value-semantics*
/// pattern compared with `isEqual:` rather than a C constant. Mirrors the
/// parser's `is_object_literal` (parser/src/parser.rs) — the two must agree, or
/// an all-literal switch is routed to the C path and emits `case <boxed>:`.
///
/// Covers `@"..."` (AtString), `@(expr)` (Boxed, rewritten by the checker into
/// an `NPNumber` factory) and the desugared `@N`/`@YES`/`@'c'` form (a message
/// send on the `NPNumber` class — see the parser's `mk_npnumber_send`).
fn is_object_literal_expr(e: &AstExpr) -> bool {
    match &e.data {
        AstExprData::AtString(_) | AstExprData::Boxed(_) => true,
        AstExprData::MsgSend { receiver, .. } => {
            matches!(receiver.data, AstExprData::AtString(_))
                || matches!(&receiver.data, AstExprData::VarRef { name, .. } if name == "NPNumber")
        }
        _ => false,
    }
}
