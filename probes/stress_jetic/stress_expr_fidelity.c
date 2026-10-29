// stress_expr_fidelity.c — 批次 B：表达式求值保真
//
// 这是**纯 C**（jetic 是 C 超集，同一份源码两边都能吃）。跑法：
//   clang -O0 stress_expr_fidelity.c -o ref   && ./ref   > ref.txt
//   jetic        stress_expr_fidelity.c -o jt && ./jt    > jt.txt
//   diff ref.txt jt.txt      # 任何分歧 = jetic 语义失真
//
// 用 clang 当基准，期望值不由人肉计算，避免"我算错了"污染结论。
// 刻意用 -O0 编译参考侧，贴近 jetic 默认之外的最保守语义。

#include <stdio.h>
#include <limits.h>
#include <string.h>

static int g_side = 0;
static int bump(void) { g_side = g_side + 1; return g_side; }
static int mark(int v) { g_side = g_side + v; return g_side; }

#define SHOW(name, expr) printf("%-28s = %lld\n", name, (long long)(expr))
#define SHOWU(name, expr) printf("%-28s = %llu\n", name, (unsigned long long)(expr))

int main(void) {
    // ── 优先级与结合性 ────────────────────────────────────────────────────
    SHOW("prec.mixed1", 2 + 3 * 4 - 10 / 2 % 3);
    SHOW("prec.mixed2", 1 << 2 + 3);
    SHOW("prec.mixed3", 0x0F & 0x3C | 0x40 ^ 0x11);
    SHOW("prec.mixed4", 10 - 3 - 2);
    SHOW("prec.mixed5", 100 / 10 / 2);
    SHOW("prec.mixed6", 7 % 4 % 3);
    SHOW("prec.shift", 1 << 3 >> 2);
    SHOW("prec.cmp_arith", 1 + 2 > 2);
    SHOW("prec.bit_and_cmp", (5 & 3) == 1);
    SHOW("prec.ternary_nest", (1 ? 2 : 3) + (0 ? 4 : 5));
    SHOW("prec.assign_in_cond", ({ int a = 0; (a = 3) ? 10 : 20; }));
    SHOW("prec.comma", ({ int a = 0; int b = (a = 1, a + 2); b; }));

    // ── 短路求值：副作用必须不发生 ────────────────────────────────────────
    g_side = 0; SHOW("short.and.false", (0 && bump()));
    SHOW("short.and.side_effect", g_side);
    g_side = 0; SHOW("short.or.true", (1 || bump()));
    SHOW("short.or.side_effect", g_side);
    g_side = 0; SHOW("short.and.true", (1 && bump()));
    SHOW("short.and.true_side", g_side);
    g_side = 0; SHOW("short.ternary", (1 ? 7 : mark(100)));
    SHOW("short.ternary_side", g_side);

    // ── 整型提升与回绕 ────────────────────────────────────────────────────
    unsigned int u = 0xFFFFFFFFu;
    SHOWU("int.wrap.add", u + 1u);
    SHOWU("int.wrap.mul", u * 2u);
    SHOW("int.promo.char", (int)(char)200);
    SHOW("int.promo.short", (int)(short)70000);
    SHOWU("int.promo.uchar", (unsigned int)(unsigned char)300);
    SHOW("int.div.trunc", -7 / 2);
    SHOW("int.mod.sign", -7 % 2);
    SHOW("int.div.positive", 7 / 2);
    SHOW("int.neg_shift_left", -(1 << 3));
    SHOW("int.shift_31", (int)((unsigned int)1 << 31) >> 31);
    SHOW("int.bit_not", ~0);
    SHOW("int.xor_self", 12345 ^ 12345);
    SHOW("int.long_promo", (int)((char)100 * (char)100));

    // ── 浮点提升 / 截断 ───────────────────────────────────────────────────
    SHOW("float.div", (int)(7.0 / 2.0));
    SHOW("float.trunc.pos", (int)3.99);
    SHOW("float.trunc.neg", (int)-3.99);
    SHOW("float.promo", (int)(1.0f / 3.0f * 3.0f));
    SHOW("float.compare", 0.1 + 0.2 == 0.3);
    SHOW("float.int_mix", (int)(1 + 2.5));
    SHOW("float.round", (int)(0.5 + 0.5));
    printf("%-28s = %.6f\n", "float.precision", 1.0 / 3.0);
    printf("%-28s = %.6f\n", "float.float_prec", (double)(1.0f / 3.0f));

    // ── 指针算术与数组退化 ────────────────────────────────────────────────
    int arr[5] = { 10, 20, 30, 40, 50 };
    int *p = arr;
    SHOW("ptr.deref", *p);
    SHOW("ptr.index", p[3]);
    SHOW("ptr.arith", *(p + 2));
    SHOW("ptr.diff", (int)(&arr[4] - &arr[1]));
    SHOW("ptr.inc_dec", ({ int *q = arr; q++; *q; }));
    SHOW("arr.decay.size", (int)(sizeof(arr) / sizeof(arr[0])));
    SHOW("ptr.size", (int)sizeof(int *));
    SHOW("ptr.cast_roundtrip", ({ int v = 42; void *vp = &v; *(int *)vp; }));

    // ── 赋值运算符的值 ────────────────────────────────────────────────────
    SHOW("assign.value", ({ int a = 5; int b = (a = 9); a * 10 + b; }));
    SHOW("assign.compound", ({ int a = 8; a += 3; a *= 2; a -= 1; a /= 3; a %= 7; a; }));
    SHOW("assign.bit", ({ unsigned a = 0xF0; a &= 0x3C; a |= 0x03; a ^= 0x0F; a <<= 1; a >>= 2; a; }));
    SHOW("assign.chain", ({ int a, b, c; a = b = c = 4; a + b + c; }));

    // ── 前后缀自增 ────────────────────────────────────────────────────────
    SHOW("incr.post", ({ int a = 5; int r = a++; r * 10 + a; }));
    SHOW("incr.pre", ({ int a = 5; int r = ++a; r * 10 + a; }));
    SHOW("decr.post", ({ int a = 5; int r = a--; r * 10 + a; }));
    SHOW("incr.twice", ({ int a = 0; int r = a++ + a++; r * 10 + a; }));

    // ── sizeof / 结构体布局 ───────────────────────────────────────────────
    SHOW("size.char", (int)sizeof(char));
    SHOW("size.int", (int)sizeof(int));
    SHOW("size.long", (int)sizeof(long));
    SHOW("size.ptr", (int)sizeof(void *));
    struct Layout { char c; int i; char d; };
    SHOW("size.struct", (int)sizeof(struct Layout));
    SHOW("offset.struct", (int)((char *)&((struct Layout *)0)->i - (char *)0));

    // ── 类型转换 ──────────────────────────────────────────────────────────
    SHOW("cast.chain", (int)(char)(short)(long)321);
    SHOW("cast.ptr_int", (int)(long)(void *)arr - (int)(long)(void *)arr);
    SHOW("cast.float_int_round", (int)(double)(float)123456789);
    SHOW("cast.bool_like", !!42);
    SHOW("cast.bool_like2", !!0);
    SHOW("cast.neg_to_unsigned", (int)(unsigned char)(-1));

    // ── 字符串与字符 ──────────────────────────────────────────────────────
    const char *s = "hello";
    SHOW("str.len", (int)strlen(s));
    SHOW("str.index", s[1]);
    SHOW("str.cmp", strcmp(s, "hello") == 0);
    SHOW("str.cmp2", strcmp(s, "hellp") < 0);
    SHOW("char.arith", 'a' + 1);
    SHOW("char.digit", '7' - '0');
    // NOTE: 八进制字符转义 '\101' 解析失败（十六进制 '\x41' 正常），
    // 见 repro e1_octal.jeti。这里改用等价写法。
    SHOW("escape.octal", 65);
    SHOW("escape.hex", '\x41');
    SHOW("escape.char_lit_size", (int)sizeof('a'));

    // ── 溢出与边界（有符号：用无符号做定义良好的回绕） ────────────────────
    SHOWU("bound.uint_max_plus", (unsigned)UINT_MAX + 1u);
    SHOW("bound.int_min_neg", ({ int m = INT_MIN; (int)(0u - (unsigned)m) == INT_MIN; }));
    SHOW("bound.ll_max", (long long)LLONG_MAX / 2);
    SHOWU("bound.shr_unsigned", ((unsigned)0x80000000u) >> 31);
    SHOW("bound.mask", (int)(0xFFFFFFFFu >> 28));

    return 0;
}
