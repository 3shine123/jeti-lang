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

/// …and a genuinely unterminated subscript still says so.
#[test]
fn an_unterminated_subscript_still_reports_the_bracket() {
    let src = "int main(void) {\n    int a[4];\n    a[0;\n    return 1;\n}\n";
    let mut p = Parser::new(src);
    let _ = p.parse_translation_unit().unwrap();
    assert!(
        p.last_error().contains("expected ']' after subscript"),
        "got:\n{}",
        p.last_error()
    );
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
