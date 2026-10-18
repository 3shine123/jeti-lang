# C 标准判错对照矩阵（c_standard_diagnostics_plan Phase 1）

`tests/negative/c_std/` 是逐语料族的双侧对照 probe：`.c` 用真 clang 编译，
`.jeti` 用 jetic 编译，判罚（报/不报、severity、caret 位）逐条对齐。
无 main 的模块文件不会被 test_all 自动运行——它们是语料，不是可执行测试。

## cast_family（6.5.4 / 6.3.2.3，2026-10-18 测量）

| # | 语句 | clang（默认） | jetic checker | 一致 |
|---|------|--------------|---------------|------|
| ① | `(char *)x`（变量 int） | warn `-Wint-to-pointer-cast` | warn `cast to 'char *' from smaller integer type 'int'` | ✓ 同位同措辞 |
| ② | `(char *)123`（字面量） | silent（字面量特判） | silent（同规则） | ✓ |
| ③ | `(int)(void *)&x` | warn `-Wvoid-pointer-to-int-cast` | warn `cast to smaller integer type 'int' from 'void *'` | ✓ |
| ④ | `(int)&x` 直接 | warn `-Wpointer-to-int-cast` | warn `cast to smaller integer type 'int' from 'int *'` | ✓ |
| ⑤ | `(long)&x` 同宽 | silent | silent | ✓ |
| ⑥ | `(int (*)(int))f`（函数设计符） | silent | silent | ✓ |

测量命令（行为可能随 clang 版本漂移，重测请留档）：
- `/usr/bin/clang -x c -fsyntax-only tests/negative/c_std/cast_family.c`
- `./target/debug/jetic tests/negative/c_std/cast_family.jeti -rewrite-jeti -o /tmp/x.c`

同步测出的附加裁决（探针已跑，全_syntax_test.jeti 1.21/1.22 夹具）：
- `(long)` / `(unsigned)` / `(int)` 浮点截断、int↔int 收窄：双侧 silent
- `size_t` 与指针同宽：`(int)[arr indexOfObject:…]`（size_t → int）双侧 silent
- `&` 操作数的静态类型是 `T *`（checker 修复：此前把 `&x` 按 `int` 推导，
  造成 `(void *)&x` 的误报）
- async / eh 降级注入的 cast 保留 origin span（合成标记），checker 通过
  `__jeti_` 内部引用识别跳过，绝不把编译器自己的 codegen 误报给用户
