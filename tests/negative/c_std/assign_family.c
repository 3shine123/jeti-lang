// Assignment-family probe — clang side (mirror of assign_family.jeti). Measure with:
//   /usr/bin/clang -fsyntax-only tests/negative/c_std/assign_family.c
struct S { int a; };
struct T { int b; };

int main(void) {
    int v = 0;
    int *p = &v;
    char *c = 0;
    struct S s;
    struct T t;
    const int ci = 1;
    int *const cp = 0;

    cp = &v;       // ① const-qualified target → error
    p = c;         // ② ptr↔ptr incompatible → warning
    v = p;         // ③ ptr → int → error 'dereference with *'
    v = &v;        // ④ int ← &value → error 'remove &'
    p = v;         // ⑤ int → ptr → error 'take the address with &'
    s = v;         // ⑥ struct ← scalar → error
    s = t;         // ⑦ struct ← foreign struct → error
    p = 0;         // ⑧ zero literal → silent
    v = 2;         // ⑨ int ← int → silent
    void *vp = p;  // ⑩ void* both ways → silent
    c = vp;
    return 0;
}
