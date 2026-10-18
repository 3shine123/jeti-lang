// Switch/case-family probe — clang side (mirror of switch_family.jeti).
// Measure with:
//   /usr/bin/clang -fsyntax-only tests/negative/c_std/switch_family.c
enum Color { RED = 1 };

int main(void) {
    int v = 1;
    int *pv = &v;
    switch (pv) {               // ① pointer subject → error
        case 1: break;
    }
    switch (v) {
        case 1: break;
        case 1: break;          // ② duplicate case value → error
        case 1.5: break;        // ③ non-integer constant → error
        default: break;
        default: break;         // ④ multiple default labels → error
    }
    switch (v) {
        case 1: break;
        case RED: break;        // ⑤ duplicate: '1' and 'RED' both equal 1 → error
    }
    switch (v) {
        case 1: break;
        default: break;
        case 2: break;          // ⑥ case after default → silent (legal C)
    }
    return 0;
}
