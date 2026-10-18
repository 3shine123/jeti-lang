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

## undecl_family（6.5.1，2026-10-18 测量）

| # | 语句 | clang（默认） | jetic checker | 一致 |
|---|------|--------------|---------------|------|
| ① | `x`（未声明变量，值位） | error `use of undeclared identifier 'x'`，caret 在名 | 同文同位 error | ✓ |
| ② | `ghost_fn(1)` | error `call to undeclared function 'ghost_fn'; ISO C99 and later do not support implicit function declarations` | 同文同位 error | ✓ |
| ③ | `known(2)`（本 TU 已定义） | silent | silent | ✓ |

实现要点：checker 的"已声明"判定走 binder 符号表。binder 曾把**任何提及**
的 `Ident` 自动注册为无类型 Variable（提及即注册），毒化 symtab 使检查恒静默；
已移除（真实声明仍经 bind_decl/params/协议成员登记）。头文件表面的豁免见
下"stdlib 豁免"。

## call_family（6.5.2.2，2026-10-18 测量）

| # | 语句 | clang（默认） | jetic checker | 一致 |
|---|------|--------------|---------------|------|
| ⑦ | `two_args(1)` | error `too few arguments to function call, expected 2, have 1`，caret 在 `)` | 同文同位 | ✓ |
| ⑧ | `one_arg(1, 2)`（单参） | error `too many arguments to function call, expected single argument 'a', have 2 arguments` | 同文；assign 值位 caret 差 1 列（span 把 `;` 计入） | ✓ 措辞/位 |
| ⑨ | `three(1,2,3,4)`（多参） | error `too many arguments to function call, expected 3, have 4` | 同文 | ✓ |
| ⑩ | `not_fn(1)`（int 变量调用） | error `called object type 'int' is not a function or function pointer`，caret 在 `(` | 同文同位 | ✓ |
| ⑪ | 正确 arity | silent | silent | ✓ |
| ⑫ | variadic 任意 argc | silent | silent（variadic 位跳过） | ✓ |
| ⑬ | block / fn-ptr 变量调用、typedef 别名（`IntFn f1; f1(9)`） | silent | silent（block_alias_names 登记 block 与 fn-ptr typedef） | ✓ |

测过的补裁决：`(void)` 参数列表按 0 参数计（clang 同）；跨 TU/文件内后置
声明经 collect_signatures 先行登记不误报。

## assign_family（6.5.16，2026-10-18 测量）

| # | 语句 | clang（默认） | jetic checker | 一致 |
|---|------|--------------|---------------|------|
| ① | `cp = &v`（`int *const cp`） | error `cannot assign to variable 'cp' with const-qualified type 'int *const'` | **未判定** — `int *const` declarator 是既有解析/生成缺口（声明整体丢失），见下 | ✗（既有 bug 阻塞） |
| ② | `p = c`（`int *` ← `char *`） | warn `incompatible pointer types assigning to 'int *' from 'char *'` | 同文 warning | ✓ |
| ③ | `v = p` | error `…pointer to integer conversion…; dereference with *` | 同文 | ✓ |
| ④ | `v = &v` | error `…; remove &` | 同文 | ✓ |
| ⑤ | `p = v` | error `…integer to pointer conversion…; take the address with &` | 同文 | ✓ |
| ⑥ | `s = v`（struct ← int） | error `assigning to 'struct S' from incompatible type 'int'` | 同文 | ✓ |
| ⑦ | `s = t`（struct ← 异名 struct） | error `…from incompatible type 'struct T'` | 同文 | ✓ |
| ⑧ | `p = 0` / `v = 2` / `void *` 双向 | silent | silent | ✓ |

ObjC 对象指针（`probe_objc.m` 测得，jeti 侧同规则）：
- 子类值 → 父类目标：silent（上转型）
- 父类值 → 子类目标 / 无关类：warn `incompatible pointer types assigning…`
- `id`/`instancetype`/`Class`/`SEL`/`NPObject *` 双向：silent
  （与 codegen `obj_wildcard_cast` 同一策略）

实现要点：目标类型**只信 scope frames 里的真实声明**（宽松回退把 `self`、
ivar 读都答成 `int`，曾把 `self = [super init]` 全线误报）；值侧 Binary/
Ternary/下标/成员读不判定（模型塌成 int，指针算术会误报）；`&x` 值经包装
成 `T *` 后可判定。

## switch_family（6.8.4.2，2026-10-18 测量）

| # | 语句 | clang（默认） | jetic checker | 一致 |
|---|------|--------------|---------------|------|
| ① | `switch (pv)`（指针主体） | error `statement requires expression of integer type ('int *' invalid)`，caret 在 `switch` | 同文同位 | ✓ |
| ② | `case 1:` 重复 | error `duplicate case value '1'`，caret 在重复值 | 同文同位 | ✓ |
| ③ | `case 1.5:` | error `integer constant expression must have integer type, not 'double'` | 同文 | ✓ |
| ④ | 第二个 `default:` | error `multiple default labels in one switch` | 同文 | ✓ |
| ⑤ | `default:` 之后的 `case` | silent | silent | ✓ |
| ⑥ | `switch (dir)`（enum 主体） | silent（穷尽性另行检查） | silent | ✓ |
| ⑦ | `case RED:`（enum 成员与字面量同值） | error `duplicate case value: '1' and 'RED' both equal '1'` | **silent** — 无常量折叠（enum_members 只存名字不存值） | ✗（记录缺口） |

`switch (*p)`（`char *p`）：silent —— 解引用链经 `deref_subject_type` 折叠
（宽松 Unary 推导不解引用，曾误报指针主体）。

## stdlib 豁免（`#include`/`#import <C头>` passthrough 门）

`#include <x.h>` / `#import <x.h>` 由 preprocessor 收进 `pre.c_headers`，
pipeline 经 `Checker::feed_included_headers` 喂给 checker：
- **函数**表：逐头门定（`printf` 需 stdio.h 在场），C17 库条目手工策展，
  只作白名单门、不发明类型；
- **绑定**表（`errno`、`stdin`、`STDIN_FILENO`、`ECHO`、`INT_MAX`…）：
  传递性表面（termios.h 拉 unistd 的 FD 常数），门为"任一含绑定的已审计头
  在场即全表生效"——漏报是 passthrough 按设计工作，误报不可接受；
- 引号 `#include "x.h"` 在场时整个未声明函数检查关闭（不可审计的外部面）。

## 已知缺口（本 tranche 记录，按计划推进）

1. `int *const cp = 0;` declarator：解析/生成把声明整体丢弃（基线亦然，
   非 checker 回归），const-赋值 ①因此被阻塞；连带 `const int z = 1;`
   生成 C 丢 `const`（codegen 不回显 is_const）。
2. `case RED`（枚举成员与字面量同值）不报重复：需要 enum 成员值表 +
   常量折叠。
3. 赋值值内的**嵌套**调用（参数位、cast 操作数内）不进 call family；
   直接值/目标位的调用已覆盖。
4. assign 值位调用的 caret 差 1 列（span 含 `;`）。
5. clang 对 `to match this '('` 的 note（括号栈）仍是 parser 待办。
