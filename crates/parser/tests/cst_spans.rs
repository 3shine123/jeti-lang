//! CST node extents (`end_line` / `end_column`) and starts — the position data
//! the LSP/debugger layer consumes.
//!
//! These pin the *invariant* and the concrete numbers for the constructs whose
//! parser builds its node long after the construct's first token (the case that
//! used to report the node at its last token): statements, declarations,
//! typedefs, class/protocol/method. A node's extent covers its own tokens; a
//! trailing `;` belongs to the enclosing statement, and detail nodes built from
//! an already-consumed name (comma-chain declarators, ivars, enum members) start
//! at that name.

use jeti_cst::{CstDecl, CstDeclData, CstStmt, CstStmtData};

fn parse(src: &str) -> jeti_cst::TranslationUnit {
    let mut p = jeti_parser::Parser::new(src);
    p.parse_translation_unit().expect("parse")
}

fn line_len(src: &str, line: usize) -> usize {
    src.lines().nth(line - 1).map(|l| l.len() + 1).unwrap_or(1)
}

fn extent(d: &CstDecl) -> (usize, usize, usize, usize) {
    (d.line, d.column, d.end_line, d.end_column)
}

#[test]
fn decl_and_stmt_extents_cover_their_tokens() {
    let src = "int x = 42;\nvoid f(void) { [self foo:1 bar:2]; }\n";
    let tu = parse(src);
    assert_eq!(tu.decls.len(), 2, "expected two top-level decls");

    let var: &CstDecl = &tu.decls[0];
    assert_eq!(var.line, 1);
    // The declaration's extent covers its own tokens (`int x = 42`, columns
    // 1..11); the trailing `;` belongs to the enclosing statement level.
    assert_eq!((var.column, var.end_line, var.end_column), (1, 1, 11));
    assert!(var.end_column <= line_len(src, 1), "decl end past EOL");

    // The initializer `42` is a leaf node — its extent must be non-empty.
    match &var.data {
        CstDeclData::Variable { initializer, .. } => {
            let init = initializer.as_ref().expect("initializer");
            assert_eq!((init.line, init.col), (1, 9));
            assert_eq!((init.end_line, init.end_col), (1, 11));
        }
        other => panic!("first decl is not a variable: {other:?}"),
    }

    // The function starts at its return type, not at its closing brace.
    let func = &tu.decls[1];
    assert_eq!((func.line, func.column), (2, 1));
    match &func.data {
        CstDeclData::Function { body, .. } => {
            let body = body.as_ref().expect("body");
            // `void f(void) { [self foo:1 bar:2]; }` — the `{` is column 14 and
            // the `}` column 36, so the body's exclusive end is column 37.
            assert_eq!((body.line, body.column), (2, 14));
            assert_eq!((body.end_line, body.end_column), (2, 37));
            assert!(body.end_column <= line_len(src, 2), "body end past EOL");
            match &body.data {
                CstStmtData::Compound(stmts) => {
                    let stmt: &CstStmt = &stmts[0];
                    // A statement starts at its own first token, not the brace.
                    assert_eq!((stmt.line, stmt.column), (2, 16));
                    assert_eq!((stmt.end_line, stmt.end_column), (2, 35));
                }
                other => panic!("function body is not a compound: {other:?}"),
            }
        }
        other => panic!("second decl is not a function: {other:?}"),
    }
}

#[test]
fn control_flow_statements_span_their_whole_construct() {
    let src = "\
void g(void) {
    if (a) {
        b = 1;
    }
    for (int i = 0; i < 3; i = i + 1) {
        c = i;
    }
    while (d) {
        d = d - 1;
    }
}
";
    let tu = parse(src);
    let body = match &tu.decls[0].data {
        CstDeclData::Function { body: Some(b), .. } => b,
        other => panic!("expected a function body: {other:?}"),
    };
    let stmts = match &body.data {
        CstStmtData::Compound(s) => s,
        other => panic!("expected a compound: {other:?}"),
    };
    let got: Vec<(usize, usize, usize, usize)> = stmts
        .iter()
        .map(|s| (s.line, s.column, s.end_line, s.end_column))
        .collect();
    // `if` occupies lines 2..=4, `for` 5..=7, `while` 8..=10, each starting at
    // its keyword and ending one past the closing brace.
    assert_eq!(got[0], (2, 5, 4, 6));
    assert_eq!(got[1], (5, 5, 7, 6));
    assert_eq!(got[2], (8, 5, 10, 6));
}

#[test]
fn typedef_and_enum_span_the_whole_declaration() {
    let src = "typedef int myInt;\nenum Color { RED, GREEN };\n";
    let tu = parse(src);
    assert_eq!(extent(&tu.decls[0]), (1, 1, 1, 19));
    assert_eq!(extent(&tu.decls[1]), (2, 1, 2, 26));
}

#[test]
fn class_protocol_and_methods_span_their_blocks() {
    let src = "@interface Foo : NPObject\n- (int)bar;\n@end\n";
    let tu = parse(src);
    let cls = &tu.decls[0];
    assert_eq!(extent(cls), (1, 1, 3, 5));
    match &cls.data {
        CstDeclData::Class { methods, .. } => {
            assert_eq!(methods.len(), 1);
            assert_eq!(extent(&methods[0]), (2, 1, 2, 12));
        }
        other => panic!("expected a class interface: {other:?}"),
    }

    let proto_src = "@protocol Proto\n- (void)p;\n@end\n";
    let ptu = parse(proto_src);
    let proto = &ptu.decls[0];
    assert_eq!(extent(proto), (1, 1, 3, 5));
    match &proto.data {
        CstDeclData::ProtocolData { methods, .. } => {
            assert_eq!(extent(&methods[0]), (2, 1, 2, 11));
        }
        other => panic!("expected a protocol: {other:?}"),
    }
}

#[test]
fn multi_line_bodies_span_lines() {
    let src = "void g(void) {\n    int y = 1;\n    int z = 2;\n}\n";
    let tu = parse(src);
    let body = match &tu.decls[0].data {
        CstDeclData::Function { body: Some(b), .. } => b,
        other => panic!("expected function with body: {other:?}"),
    };
    assert_eq!((body.line, body.column), (1, 14), "body starts on the `{{` line");
    assert_eq!(body.end_line, 4, "body ends on the closing brace line");
}
