//! Parser error recovery.
//!
//! Before these tests, `consume()` advanced past the token it failed to match.
//! For a missing `;` that ate the *next* declaration's leading token
//! (`int b = 2` lost its `int`, the remnant `b = 2` parsed as a harmless
//! assignment), so every second error stayed hidden — a 5-line file of missing
//! semicolons reported 3 errors instead of 5. The fix leaves the offending
//! token in place and lets the recovery loops in `parse_translation_unit` /
//! `parse_compound_statement` resync.

use jeti_parser::Parser;

/// Every missing `;` must produce exactly one error, at the right line.
#[test]
fn missing_semicolons_report_one_error_per_line() {
    let src = "int a = 1\nint b = 2\nint c = 3\nint d = 4\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 4, "expected one error per missing ';', got:\n{err}");
    assert!(err.contains("1:"), "line 1 missing from:\n{err}");
    assert!(err.contains("2:"), "line 2 missing from:\n{err}");
    assert!(err.contains("3:"), "line 3 missing from:\n{err}");
    assert!(err.contains("4:"), "line 4 missing from:\n{err}");
}

/// Inside a function body the same contract holds, and the stray `}`
/// / EOF cascades the old advance-on-error produced must not appear.
#[test]
fn missing_semicolons_in_body_report_one_error_per_line() {
    let src = "int main() {\n    int a = 1\n    int b = 2\n    int c = 3\n    return 0\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 4, "expected one error per missing ';', got:\n{err}");
    assert!(!err.contains("expected '}'"), "cascading '}}' error leaked:\n{err}");
    assert!(!err.contains("got EOF"), "cascading EOF error leaked:\n{err}");
}

/// A missing `;` before a message send is reported on the line that is missing
/// it, at the end of the statement — not as a subscript error on the next line.
///
/// `[receiver selector]` is the dominant bracketed form in Jeti, and after a
/// forgotten `;` the next line's send used to be read as `expr[…`: the user saw
/// "expected ']' after subscript" on the *next* line, no error at all on the line
/// they broke, and a fragment's missing `;` reported twice at one position.
/// clang reports exactly one error here, at `2:20`.
#[test]
fn a_missing_semicolon_before_a_message_send_points_at_the_send() {
    let src = "int main(void) {\n    [sysLog release]\n    [userConfig release];\n    return 0;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 1, "one mistake must produce one error:\n{err}");
    assert!(err.contains("2:"), "the error must point at line 2, the one missing the ';':\n{err}");
    assert!(!err.contains("subscript"), "the next line's send was read as a subscript:\n{err}");
    assert!(!err.contains("3:"), "a recovery fragment on line 3 produced its own error:\n{err}");
}

/// The guard above must not cost real subscripts anything.
#[test]
fn subscripts_still_parse() {
    let src = "int f(int i);\n\
               int main(void) {\n\
               \x20   int a[4];\n\
               \x20   int b[2];\n\
               \x20   a[0] = a[i + 1];\n\
               \x20   a[f(i)] = a[b[0]];\n\
               \x20   return a[1];\n\
               }\n";
    let mut p = Parser::new(src);
    let unit = p.parse_translation_unit().expect("parse");
    assert_eq!(p.error_count(), 0, "subscripts must still parse:\n{}", p.last_error());
    assert_eq!(unit.decls.len(), 2);
}

/// …and a genuinely unterminated subscript still says so — clang's bare
/// `expected ']'`, caret in the hole (at the `;`, not the token before it).
#[test]
fn an_unterminated_subscript_still_reports_the_bracket() {
    let src = "int main(void) {\n    int a[4];\n    a[0;\n    return 1;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert!(err.contains("expected ']'"), "got:\n{err}");
    assert!(err.contains("3:8"), "the caret must sit at the ';' (3:8):\n{err}");
}

/// Recovery must not corrupt a correct file: a mixed file (one bad line in the
/// middle) still parses every good declaration around it.
#[test]
fn recovery_preserves_good_declarations() {
    let src = "int a = 1;\nint b = 2\nint c = 3;\n";
    let mut p = Parser::new(src);
    let unit = p.parse_translation_unit().unwrap();
    assert_eq!(p.error_count(), 1, "only line 2 is bad:\n{}", p.last_error());
    let names: Vec<_> = unit.decls.iter().filter_map(|d| d.name.clone()).collect();
    assert!(names.contains(&"a".to_string()), "a lost: {names:?}");
    assert!(names.contains(&"c".to_string()), "c lost: {names:?}");
}

/// A clean file must parse with zero errors and zero recovery interference.
#[test]
fn clean_file_parses_with_no_errors() {
    let src = "int a = 1;\nint b = 2;\nint main() { return 0; }\n";
    let mut p = Parser::new(src);
    let unit = p.parse_translation_unit().unwrap();
    assert_eq!(p.error_count(), 0, "{}", p.last_error());
    assert_eq!(unit.decls.len(), 3, "expected 3 decls, got {}", unit.decls.len());
}

/// A literal with no closing quote is reported *at the quote*, and the code
/// after it still parses.
///
/// The lexer used to scan on for the next `"` anywhere in the buffer, so a
/// forgotten quote in a file that inlines `#import`s — thousands of lines of
/// Foundation — produced a single error pointing at the end of the file and
/// swallowed every diagnostic under it. A raw newline now ends the literal
/// exactly as it does in C, so clang's message lands on the quote itself, and
/// what follows keeps its own errors.
#[test]
fn a_missing_closing_quote_is_reported_at_the_quote() {
    let src = "int main(void) {\n    printf(\"widget tag: %s\\n, [w objectTag]);\n    return 0;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 1, "one mistake must produce one error:\n{err}");
    assert!(err.contains("missing terminating '\"' character"), "{err}");
    assert!(err.contains("2:12"), "the error must point at the quote on line 2:\n{err}");
    assert!(!err.contains("3:"), "a recovery fragment on line 3 reported its own error:\n{err}");
}

/// A literal that runs on past the end of its line is not a literal: C ends it
/// at the newline, and so must we.
///
/// Accepting it here was worse than a wrong message: the `.jeti` parsed, we
/// emitted C containing a raw newline inside a string, and only the C compiler
/// refused — a file the editor showed as clean until it was built.
#[test]
fn a_literal_spanning_a_newline_is_rejected() {
    let src = "int main(void) {\n    printf(\"a\nb\");\n    return 0;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert!(
        err.contains("missing terminating '\"' character"),
        "a literal across a newline must be rejected:\n{err}"
    );
}

/// A missing expression after an operator: clang's `expected_expression`.
///
/// Before this fix the empty parse propagated through the `?` in silence, so
/// `int a =;` produced zero diagnostics and only the C compiler ever saw the
/// hole. Every position where an operator has already been consumed — and an
/// expression is therefore required — now reports exactly one error, on the
/// unexpected token (the sites: initializers, assignment RHS, the ten binary
/// levels, and both ternary branches).
#[test]
fn a_missing_initializer_expression_is_reported() {
    let src = "int main(void) {\n    int a =;\n    return 0;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 1, "one mistake must produce one error:\n{err}");
    assert!(err.contains("expected expression"), "{err}");
    assert!(err.contains("2:12"), "the error must point at the ';' on line 2:\n{err}");
    assert!(!err.contains("3:"), "a recovery fragment on line 3 reported its own error:\n{err}");
}

/// Every binary operator level — plus a compound assignment — reports its
/// missing operand: one error per broken line, none cascaded, and the clean
/// lines between them stay clean.
#[test]
fn a_missing_binary_operand_is_reported_per_operator() {
    let src = "int main(void) {\n\
               \x20   int a = 1 +;\n\
               \x20   int b = 2 *;\n\
               \x20   int c = 3 <<;\n\
               \x20   int d = 4 <;\n\
               \x20   int e = 5 ==;\n\
               \x20   int f = 6 &;\n\
               \x20   int g = 7 ^;\n\
               \x20   int h = 8 |;\n\
               \x20   int i = 9 &&;\n\
               \x20   int j = 10 ||;\n\
               \x20   int k;\n\
               \x20   k +=;\n\
               \x20   return 0;\n\
               }\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 11, "one error per broken line:\n{err}");
    for line in 2..=11 {
        assert!(err.contains(&format!("{line}:")), "line {line} missing from:\n{err}");
    }
    assert!(err.contains("13:"), "the compound assignment (line 13) missing:\n{err}");
    assert!(!err.contains("12:"), "line 12 is clean and must not report:\n{err}");
}

/// Both ternary branches are required expressions.
#[test]
fn missing_ternary_branches_are_reported() {
    let src = "int main(void) {\n    int a = 1 ? : 2;\n    int b = 1 ? 2 : ;\n    return 0;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 2, "one error per broken branch:\n{err}");
    assert_eq!(
        err.matches("expected expression").count(),
        2,
        "both errors must name the missing expression:\n{err}"
    );
    assert!(err.contains("2:"), "line 2 (true branch) missing:\n{err}");
    assert!(err.contains("3:"), "line 3 (false branch) missing:\n{err}");
}

/// The positions where an empty expression is *legal* — before an operator,
/// where the callers guard with lookahead — must stay clean.
#[test]
fn legitimate_empty_positions_still_parse() {
    let src = "int main(void) {\n    int a = 1, b = 2;\n    for (;;) { break; }\n    if (a) { return b; }\n    return 0;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    assert_eq!(p.error_count(), 0, "{}", p.last_error());
}

/// The same family beyond the operator levels: a consumed syntactic piece
/// makes an expression required — a cast's operand, a comma operator's second
/// operand, a subscript's index, a case label's constant (head and the
/// multi-value comma list), and a call argument after its comma. Each hole
/// reports once, at the token standing in its place. Before this fix these
/// were worse than silent: the parse filled a synthesized `0`, so `(int);`
/// meant `(int)0` and `a[] = 1;` meant `a[0] = 1;`.
#[test]
fn a_missing_expression_after_a_consumed_piece_is_reported() {
    let src = "\
int f(int x);
int main(void) {
    int a = (int);
    int b = (1, );
    int c[2]; c[] = 1;
    switch (0) { case : break; }
    switch (0) { case 1,: break; }
    f(1,);
    return 0;
}
";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    let err = p.last_error();
    assert_eq!(p.error_count(), 6, "one error per hole:\n{err}");
    for line in 3..=8 {
        assert!(err.contains(&format!("{line}:")), "line {line} missing from:\n{err}");
    }
    assert_eq!(
        err.matches("expected expression").count(),
        6,
        "every error must name the missing expression:\n{err}"
    );
}

/// The list-shaped positions that DO allow an empty slot stay clean: a
/// trailing comma in an initializer list, a zero-argument call, a valid case
/// label, and the statement-level omissions (`for(;;)`).
#[test]
fn legal_trailing_commas_and_empty_calls_still_parse() {
    let src = "\
int f(int x);
int g(void);
int main(void) {
    int a[] = {1,};
    int b[] = {1,2,};
    switch (0) { case 0: break; }
    f(1);
    g();
    for (;;) { break; }
    return 0;
}
";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    assert_eq!(p.error_count(), 0, "{}", p.last_error());
}
