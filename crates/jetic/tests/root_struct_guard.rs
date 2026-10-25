//! A parsed `struct NPObject { … }` / `struct jeti_root { … }` must be
//! re-emitted behind its include guard.
//!
//! `jeti/runtime.h` also defines `NPObject` (hosted) and `jeti_root`
//! (freestanding), behind `NPOBJECT_DEFINED` / `JETI_ROOT_DEFINED`. The source
//! guard is a preprocessor line, so it does not survive re-emission — codegen
//! has to put it back, or re-processing jeti's own generated C hits a hard
//! "redefinition of 'NPObject'".
//!
//! One test per file: codegen's method metadata is a process-global `OnceLock`,
//! so a second `transpile` in the same test process panics.

use jetic::pipeline::Pipeline;

#[test]
fn a_parsed_root_struct_keeps_its_include_guard() {
    let src = "\
struct NPObject { struct NPClass *isa; unsigned int retain_count; };
int main(void) { return 0; }
";
    let mut pipeline = Pipeline::new();
    let c = pipeline.transpile(src, "probe.jeti").expect("transpile");
    assert!(c.contains("#ifndef NPOBJECT_DEFINED"), "guard opener missing:\n{c}");
    assert!(c.contains("#define NPOBJECT_DEFINED"), "guard marker missing:\n{c}");
    assert!(c.contains("#endif"), "guard closer missing:\n{c}");
}
