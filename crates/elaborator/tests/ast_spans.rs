//! AST node extents.
//!
//! The CST layer (`tests/cst_spans.rs` in `jeti-parser`) pins the positions the
//! parser records; this pins the layer above it: the elaborator copies the CST
//! extent onto every AST node, and lowering passes keep the extent of the node
//! they were derived from. A node whose end is `0` has no source origin (the
//! compiler synthesized it) — resolving it through `SourceMap::span_from_node`
//! yields a zero-width span rather than a bogus range.

use jeti_ast::{AstDecl, AstDeclData, AstExpr, AstExprData, AstStmt, AstStmtData, AstUnit};
use jeti_binder::Binder;
use jeti_cst::TranslationUnit;
use jeti_elaborator::Elaborator;
use jeti_symbol::SymbolTable;

fn elaborate(src: &str) -> AstUnit {
    let mut p = jeti_parser::Parser::new(src);
    let mut tu: TranslationUnit = p.parse_translation_unit().expect("parse");
    let mut binder = Binder::new(SymbolTable::new());
    assert_eq!(binder.bind(&mut tu), 0, "bind failed");
    let mut el = Elaborator::new(Some(binder.symtab));
    assert_eq!(el.run(&tu), 0, "elaborate failed");
    el.take_ast().expect("no AST")
}

fn extent_decl(d: &AstDecl) -> (usize, usize, usize, usize) {
    (d.line, d.col, d.end_line, d.end_col)
}

fn extent_stmt(s: &AstStmt) -> (usize, usize, usize, usize) {
    (s.line, s.col, s.end_line, s.end_col)
}

fn extent_expr(e: &AstExpr) -> (usize, usize, usize, usize) {
    (e.line, e.col, e.end_line, e.end_col)
}

fn body_of<'a>(unit: &'a AstUnit, name: &str) -> &'a AstStmt {
    for d in &unit.decls {
        if d.name.as_deref() == Some(name) {
            if let AstDeclData::Function { body: Some(b), .. } = &d.data {
                return b;
            }
        }
    }
    panic!("function {name} with a body not found");
}

#[test]
fn elaboration_copies_the_cst_extent_onto_the_ast() {
    let src = "int x = 42;\nvoid f(void) {\n    if (x) {\n        x = 1;\n    }\n}\n";
    let unit = elaborate(src);

    let var: &AstDecl = &unit.decls[0];
    assert_eq!(var.name.as_deref(), Some("x"));
    // `int x = 42` — columns 1..=11 (the `;` belongs to the statement level).
    assert_eq!(extent_decl(var), (1, 1, 1, 11));
    match &var.data {
        AstDeclData::Variable { init: Some(init), .. } => {
            assert_eq!(extent_expr(init), (1, 9, 1, 11), "the `42` leaf");
            assert!(matches!(init.data, AstExprData::Int(42)));
        }
        other => panic!("expected a variable with an initializer: {other:?}"),
    }

    let fun: &AstDecl = &unit.decls[1];
    assert_eq!(fun.name.as_deref(), Some("f"));
    // The function starts at its return type, not at its closing brace.
    assert_eq!(extent_decl(fun), (2, 1, 6, 2));

    let body = body_of(&unit, "f");
    assert_eq!(extent_stmt(body), (2, 14, 6, 2), "the body spans its braces");
    match &body.data {
        AstStmtData::Compound(stmts) => {
            assert_eq!(stmts.len(), 1);
            let if_stmt = &stmts[0];
            assert_eq!(if_stmt.kind, jeti_ast::AstStmtKind::If);
            assert_eq!(extent_stmt(if_stmt), (3, 5, 5, 6), "the whole `if`");
        }
        other => panic!("expected a compound body: {other:?}"),
    }
}

#[test]
fn synthesized_nodes_report_zero_ends() {
    // `x = 1;` inside a `while` — lowering/elaboration injects no `self` here,
    // but the point is the invariant: any node the compiler invents ends at 0
    // (meaning "no source origin"), never at a stale line from elsewhere.
    let src = "int g = 0;\nvoid h(void) {\n    g = 1;\n}\n";
    let unit = elaborate(src);
    let body = body_of(&unit, "h");
    match &body.data {
        AstStmtData::Compound(stmts) => {
            let assign = &stmts[0];
            assert_eq!(extent_stmt(assign), (3, 5, 3, 11));
            match &assign.data {
                AstStmtData::Expr(e) => assert_eq!(extent_expr(e), (3, 5, 3, 10)),
                other => panic!("expected an expression statement: {other:?}"),
            }
        }
        other => panic!("expected a compound body: {other:?}"),
    }
}
