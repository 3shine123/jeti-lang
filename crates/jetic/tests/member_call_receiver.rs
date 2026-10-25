//! A member call through a parenthesized or address-of receiver must keep its
//! callee.
//!
//! `(c).add(&c, 5)` / `(&c)->add(&c, 7)` used to collapse to the argument list
//! read as a comma expression (`(&c, 5)`) — a silent miscompile. The
//! address-of form is exactly the generated vtable-dispatch shape
//! `((T (*)(…))x->vtable->m)(args)`, so the checker then inferred the call's
//! type from the last argument (a `SEL`) and reported bogus "incompatible
//! operand types" errors on jeti's own generated C.

use jetic::pipeline::Pipeline;

fn transpile(src: &str) -> String {
    let mut pipeline = Pipeline::new();
    pipeline.transpile(src, "probe.jeti").expect("transpile")
}

#[test]
fn parenthesized_and_address_of_receivers_keep_their_callee() {
    let src = "\
typedef struct Counter { int base; int (*add)(struct Counter *self, int x); } Counter;
static int counter_add(struct Counter *self, int x) { return self->base + x; }
int use(Counter *c) {
    int a = c->add(c, 1);
    int b = (c)->add(c, 2);
    int d = (*c).add(c, 3);
    int e = (&*c)->add(c, 4);
    return a + b + d + e;
}
";
    let c = transpile(src);
    // Every receiver keeps its callee (the emitted text is extra-parenthesized
    // but the member name and the argument list stay attached).
    assert!(c.contains("c->add(c, 1)"), "plain receiver lost:\n{c}");
    assert!(c.contains("(c)->add(c, 2)"), "parenthesized receiver lost:\n{c}");
    assert!(c.contains("(*c)).add(c, 3)"), "deref receiver lost:\n{c}");
    assert!(c.contains("(&(*c)))->add(c, 4)"), "address-of receiver lost:\n{c}");
    // The argument list must never leak as the value (the comma-expression bug).
    assert!(!c.contains("= (c, 1)"), "argument list leaked as the value:\n{c}");
    assert!(!c.contains("= (c, 2)"), "argument list leaked as the value:\n{c}");
}
