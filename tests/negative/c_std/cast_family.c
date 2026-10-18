// Cast family — clang/jetic side-by-side probe (c_standard_diagnostics_plan §G-expr).
// Compile side by side:
//   clang -x c -fsyntax-only cast_family.c
//   jetic -rewrite-jeti cast_family.jeti -o /dev/null
// Every cast below mirrors the same statement in the sibling `.jeti` file;
// the matrix in README.md records the measured verdicts (clang 18+, LP64).
int f(int v);
int main(void) {
    int x = 5;
    // ① int → pointer, from a VARIABLE: clang warns (int-to-pointer-cast)
    char *a = (char *)x;
    // ② int → pointer, from an int literal: clang is SILENT (literal escape hatch)
    char *b = (char *)123;
    // ③ void * → int: clang warns (void-pointer-to-int-cast)
    int c = (int)(void *)&x;
    // ④ int * → int directly: clang warns (pointer-to-int-cast)
    int d = (int)&x;
    // ⑤ same-width long → pointer / pointer → long: clang is SILENT
    long e = (long)&x;
    // ⑥ function designator → fn pointer: clang is SILENT
    int (*g)(int) = (int (*)(int))f;
    (void)a; (void)b; (void)c; (void)d; (void)e; (void)g;
    return 0;
}
