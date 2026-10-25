//! Every struct field keeps its own qualifiers.
//!
//! `parse_struct_body` used to skip a `const` / `volatile` / `static` sitting
//! right after a field's `;` — but that token belongs to the NEXT field:
//! `const char *a; const char *b;` re-emitted `b` as `char *`. In the generated
//! C this showed up as the vtable's `const char *(*)(NPObject *, SEL)` members
//! mismatching their implementations ("incompatible function pointer types").
//!
//! One test per file: codegen's method metadata is a process-global `OnceLock`,
//! so a second `transpile` in the same test process panics.

use jetic::pipeline::Pipeline;

#[test]
fn a_second_struct_field_keeps_its_const() {
    let src = "\
struct Vt {
    const unsigned char * (*bytes)(void *p, int sel);
    const char * (*get)(void *p, int sel);
};
int f(void) { return 0; }
";
    let mut pipeline = Pipeline::new();
    let c = pipeline.transpile(src, "probe.jeti").expect("transpile");
    assert!(
        c.contains("const unsigned char * (*bytes)"),
        "the first field lost its const:\n{c}",
    );
    assert!(
        c.contains("const char * (*get)"),
        "the second field lost its const:\n{c}",
    );
}
