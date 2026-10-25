//! A function-pointer *type* whose parameter list ends in `...` must keep it.
//!
//! `int (*fp)(int, ...)` re-emitted as `int (*fp)(int)` made every call
//! signature-mismatched ("too many arguments to function call") and broke
//! AGENTS.md iron law 6 (a vtable fn-ptr cast must include `, ...`).
//!
//! One test per file: codegen's method metadata is a process-global
//! `OnceLock`, so a second `transpile` in the same test process panics.

use jetic::pipeline::Pipeline;

#[test]
fn variadic_fn_ptr_type_keeps_its_ellipsis() {
    let src = "\
extern int (*fp)(int a, ...);
int call(void) { return fp(1, 2, 3); }
";
    let mut pipeline = Pipeline::new();
    let c = pipeline.transpile(src, "probe.jeti").expect("transpile");
    assert!(
        c.contains("int (*fp)(int, ...)"),
        "the ellipsis was lost from the fn-ptr type:\n{c}",
    );
}
