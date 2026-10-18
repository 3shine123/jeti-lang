// Undeclared-identifier family — clang side (mirror of undecl_family.jeti).
// Measure with:
//   /usr/bin/clang -fsyntax-only tests/negative/c_std/undecl_family.c
int known(int v) { return v; }
int main(void) {
    int y = x + 1;        // ① undeclared identifier 'x' → error
    ghost_fn(1);          // ② implicit function declaration → error
    int z = known(2);     // ③ declared → silent
    return y + z;
}
