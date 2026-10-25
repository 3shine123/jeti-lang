//! Struct definitions must survive codegen.
//!
//! Two shapes used to be lost, both leaving an incomplete type behind:
//!
//! - an anonymous inline struct in a declaration —
//!   `static struct { const char *cstr; void *obj; } t[256];` re-emitted as
//!   `static int t[256];`, so every `t[i].cstr` was rejected by the C compiler
//!   ("member reference base type 'int' is not a structure or union");
//! - a local struct definition in statement position — `struct X { … };` inside
//!   a function fell through to the empty-`return` arm of the statement
//!   converter, so the definition vanished and the next `struct X x;` was an
//!   incomplete type.
//!
//! Both are checked in ONE test: codegen's method metadata is a process-global
//! `OnceLock`, so a second `transpile` in the same test process panics.

use jetic::pipeline::Pipeline;

#[test]
fn struct_definitions_survive_codegen() {
    let src = "\
static struct { const char *cstr; void *obj; } t[256];
int use(const char *s) {
    struct Local { int a; int b; };
    struct Local x;
    x.a = 1;
    for (int i = 0; i < 4; i++) {
        if (t[i].cstr == s) return i;
    }
    return x.a;
}
";
    let mut pipeline = Pipeline::new();
    let c = pipeline.transpile(src, "probe.jeti").expect("transpile");

    // Anonymous struct: the body must travel to codegen verbatim.
    assert!(
        c.contains("struct { const char *cstr; void *obj; }"),
        "anonymous struct body was dropped (the type collapsed to int):\n{c}",
    );
    assert!(c.contains("t[i].cstr"), "field access lost:\n{c}");

    // Local struct definition: emitted where it appeared, never as `return;`.
    assert!(c.contains("struct Local {"), "local struct definition dropped:\n{c}");
    assert!(c.contains("int a;") && c.contains("int b;"), "fields dropped:\n{c}");
    assert!(
        !c.contains("return;\n    struct Local"),
        "the definition was replaced by a stray `return;`:\n{c}",
    );
}
