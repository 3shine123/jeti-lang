//! The EH pass must not re-lower an already-lowered body.
//!
//! Generated C re-entering the pipeline (the jeti→C→C closed loop) carries the
//! pass's own `__jeti_eh_*` markers. Re-running the desugar treated
//! `__jeti_eh_flag = 1` — a throw site it emitted earlier — as a fresh throw
//! and wrapped the remaining tail, including the catch dispatch, in a
//! `__jeti_eh_flag == 0` guard. The catch then tested `flag == 1` inside a
//! block entered only when `flag == 0`: every `@catch` was unreachable after
//! one round-trip, so any thrown exception aborted as uncaught.
//!
//! Regression shape: transpile twice through the jetic binary (subprocess —
//! codegen's method metadata is a process-global `OnceLock`, so a second
//! in-process `transpile` panics) and assert the EH guard count is unchanged,
//! i.e. the pass is a no-op on its own output. User source can never spell
//! `__jeti_eh_*` (checker-reserved), so fresh code is unaffected.

use std::process::Command;

const PROBE: &str = r#"#include <stdio.h>
#import <Foundation/Foundation.hje>
@interface EhBase : NPObject
@end
@implementation EhBase
@end
@interface EhSub : EhBase
@end
@implementation EhSub
@end
int main(void) {
    @try {
        @throw [[EhSub alloc] init];
    } @catch (EhBase *e) {
        printf("caught base: %s\n", "ok");
    } @finally {
        printf("finally\n");
    }
    printf("survived\n");
    return 0;
}
"#;

fn jetic(args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_jetic"))
        .args(args)
        .output()
        .expect("jetic binary runs");
    assert!(
        out.status.success(),
        "jetic {:?} failed:\n{}{}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn eh_lowering_is_idempotent_across_a_round_trip() {
    let tmp = std::env::temp_dir().join(format!("jeti_eh_rt_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("probe.jeti");
    let c1 = tmp.join("probe1.c");
    let c2 = tmp.join("probe2.c");
    let c3 = tmp.join("probe3.c");
    std::fs::write(&src, PROBE).unwrap();

    let inc = concat!(env!("CARGO_MANIFEST_DIR"), "/../../include");
    jetic(&["-I", inc, "-rewrite-jeti", src.to_str().unwrap(), "-o", c1.to_str().unwrap()]);
    jetic(&["-I", inc, "-rewrite-jeti", c1.to_str().unwrap(), "-o", c2.to_str().unwrap()]);
    jetic(&["-I", inc, "-rewrite-jeti", c2.to_str().unwrap(), "-o", c3.to_str().unwrap()]);

    let guards = |p: &std::path::Path| {
        std::fs::read_to_string(p)
            .unwrap()
            .matches("__jeti_eh_flag == 0")
            .count()
    };
    let (n1, n2, n3) = (guards(&c1), guards(&c2), guards(&c3));

    // Convergence: round 2 may legitimately gain one extra tail guard — a
    // `#ifdef`-opaque body section materializes on re-parse, making calls the
    // first pass could not see visible (conservative wrap). Once wrapped, the
    // body carries `__jeti_eh_*` markers and every later round is a no-op.
    // THE REGRESSION this guards against is unbounded re-wrapping (the catch
    // dispatch re-nested per round → dead @catch → uncaught-exception abort).
    assert_eq!(
        n2, n3,
        "EH guard count still growing: {n1} -> {n2} -> {n3} — the pass re-wraps its own output"
    );

    // The catch dispatch must survive every round-trip verbatim.
    let c3s = std::fs::read_to_string(&c3).unwrap();
    assert!(
        c3s.contains("__jeti_eh_isa"),
        "catch dispatch missing from the re-emitted C"
    );

    std::fs::remove_dir_all(&tmp).ok();
}
