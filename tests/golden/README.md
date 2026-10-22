# Jeti 黄金测试集

按功能分类的系统化回归测试。每个测试包含 `.jeti`（源码）和 `.out`（期望输出）文件。

## 目录结构

```
01_basics/          基础功能（hello world, 纯 C 互操作）
02_class/           类系统（继承、方法调用、self/super）
03_properties/      属性（getter/setter、dot syntax、@synthesize、@dynamic）
04_arc/             ARC 内存管理（retain/release、作用域、分支、dealloc、weak）
05_autoreleasepool/ 自动释放池（简单/顺序池、return、循环）
06_polymorphism/    多态（vtable 动态分发、id 类型）
07_protocols/       协议（声明、合规、编译失败测试）
08_categories/      分类（命名分类、扩展）
09_blocks/          Block 语法（字面量、变量捕获）
10_edge_cases/      边界情况（nil、instancetype、@class、@selector 等）
11_multi_file_union/多文件 #import 联合（故意失败样本用 -F 后缀标记）
12_namespace/       @namespace / @using
13_foundation/      Foundation 系列（按类分子目录）：01_npobject…07_npmutablearray 容器基础（NPString / NPArray / NPMutableArray / NPLog / description）、08_dispatch isKindOfClass: / respondsToSelector: / isEqual:、09_extras 字典键下标 `d[@"k"]` 读（objectForKey:）/写（setObject:forKey:）+ NPAssert + 数组下标回归、10_values NPNull / NPValue / NPDate / NPData·NPMutableData / NPEnumerator、11_io NPProcessInfo / NPFileManager / NPData 文件读写（连带修复 C 八进制字面量被当十进制的编译器 Bug）、12_notify NPNotificationCenter / NPCountedSet / NPCache（连带记录 `(^ *` 块指针类型解析死循环缺口）、13_timer NPTimer + 运行期 timer 层（虚拟时钟驱动整张语义表）
14_generics/        泛型单态化
15_exceptions/      @try / @catch / @finally / @throw
16_control_flow/    控制流（if / while / for / switch）
17_operators/       运算符
18_types/           类型系统
19_weak_ivar/       __weak 引用
20_blocks_advanced/ Block 进阶（__block 捕获、嵌套）
22_c_superset/      C 超集语法（struct、C 风格 cast、函数指针）
23_asm/             Inline asm（extended asm、命名操作数、asm goto）
24_asm_fusion/      asm 融合压测（内联+外部 asm × 类/协议/Block/异常/struct/fn-ptr）
25_freestanding/    裸机模式（-freestanding，需自带 build.sh，不在默认套件）
26_baremetal_stress/裸机压力测试（同上，需 build.sh）
28_refcount_trace/  引用计数追踪器（-trace-refcount）输出快照
29_block_array/     Block + 数组组合
30_for_in/          for (T x in coll) 遍历
31_proto_conformance/ 协议一致性检查 + 协议组合 P & Q
33_struct_eq/       struct == / != 值比较
34_async/           await 状态机（async 调用唯一形态 await；sync main 经 blockOn 桥进入）
35_variadic_method/ 真 variadic 方法（va_list）
36_defer/           @defer 作用域退出执行
37_async_modifier/  async 修饰符 + 调用点纪律（裸 async 调用必拦；blockOn 桥；软关键字：async 业务类型；归一化为 NPTask<T>*）
38_macros/          jeti 语法宏展开（双轨 #define）
39_complex/         C99 _Complex 透传
40_nparray_generic/ NPArray<T> / NPDictionary<K,V> 真单态化 + 元素类型检查
41_switch_pat/      switch 模式匹配（case T *x / case > 10 / when 守卫）
42_boxed_literal/   装箱字面量 @(expr) / @YES / @NO / @'c'
43_dict_literal/    字典字面量 @{ key: value }
44_designated_init/ C99 指定初始化器六形态
45_nullability/     nullability 注解（nonnull/nullable 透传 + checker）
46_generic_bounds/  泛型协议约束 T : Proto（实例化检查 + 继承重申/弱化）
47_async_blocks/    async 块字面量（IIFE + 三元混合；负例 async_block_ternary_mix-F）
48_async_throws_await/ 裸 `throws`/`throws(T)`（同步+异步同一拼写）+ 消息内括号 await（实参位/接收者位）
49_soft_keywords/   软关键字矩阵：async/await/throws/when/in/nullable/nonnull 七词 × 字段/局部/ivar/方法名/选择子位 + 关键字位 + `id` 各名字位（README 含三列消歧表）
50_blockon_paren_async/ 括号包裹的 blockOn 实参：`[NPTask blockOn:(async ^…)]` 三种拼写（裸/括号/括号+体内 await）均过桥执行（2026-10-15 M3 回归）
51_block_return_suffix/ 整数后缀返回类型推导：块字面量省略返回时 `7L`/`7LL`/`7U`/十六进制后缀落正确宽度（2026-10-15 P0 回归，lexer int_suffix → parser expr_type → checker 保留）
```

> 每个目录的 `README.md` 记录该特性的**实现要点、判据表、M1 限制与踩过的坑**——排查问题时先看它，比读 `.jeti` 快得多。
>
> 故意失败的样本**不放在 golden 目录**（否则被当普通 FAIL 计数），统一在 `tests/negative/`，已在 `test_all.py` 的 glob 排除；需要"必须编译失败且报错清晰"的用例（如协议缺必需方法、`@(obj)` 非法装箱、`throws` 撒谎）都在那里。
>
> `25_freestanding/` 与 `26_baremetal_stress/` 各有专属 `build.sh`（需要 `-freestanding` 与裸机 assembler），**不在默认测试套件**，用它们自己的 runner 跑。

> `28_refcount_trace/` 与其他 golden 目录不同：每个 `.jeti` 的 `.out` 不是程序运行输出，而是 `jetic -trace-refcount -trace-no-color -trace-max-iters 2` 的追踪快照。运行方式：
>
> ```bash
> ./tests/golden/28_refcount_trace/run_trace_golden.sh
> # JETIC=/path/to/jetic ./tests/golden/28_refcount_trace/run_trace_golden.sh
> ```
>
> 用例覆盖：多级 retain/release（1→4→0）、double-release 负计数检测、泄漏检测、ARC 自动注入的 `jeti_release`、别名共享、嵌套 `@autoreleasepool`（`@noarc` 内手动 autorelease）、if/else 分支状态克隆、循环迭代产生独立 `Class#N` 身份。`.jeti` 本身仍是可编译运行的合法 Jeti 程序，会被 `test_all.py` 的 glob 照常编译+运行。

> x86_64 汇编是跨架构用例，不放入默认 arm64 测试套件，单独位于 `asm_x64/`（见下文）。

## x86_64 / Rosetta 测试

在 arm64 Mac 上通过 Rosetta 运行 x86_64 汇编：

```bash
jetic -arch x86_64 run -asm asm_x64/asm_x86_ext.s asm_x64/asm_x86_fusion_test.jeti
# 或
./asm_x64/build.sh
```

要点：
- 使用系统原生的 `/usr/bin/arch`（不是 uutils 的 `arch`，后者没有 `-x86_64` 切换能力）。
- 未安装 Rosetta 时先 `softwareupdate --install-rosetta --agree-to-license`。
- `-arch x86_64` 让 clang 交叉产出 x86_64 Mach-O，执行时被 Rosetta 自动翻译。
- x86_64 内联 asm 用 `%r` 即可（32 位操作数直接用 eax 等），无需 ARM64 的 `%w` 修饰符。
- Rosetta 安装后若立即报 "Bad CPU type in executable"，等 oahd 激活后重试。

## 运行方式

```bash
# 方式 1: 顶层脚本
./test_golden.sh

# 方式 2: 通过 test_all.sh（--golden 标志）
./test_all.sh --golden

# 方式 3: 直接运行 Python runner
python3 test/golden/test_golden.py

# 方式 4: Shell runner
./test/golden/run_golden.sh
```

## 选项

| 参数 | 说明 |
|------|------|
| `-jN` | 并行 N 任务 |
| `-fsemantic` | 启用语义检查 |
| `-fno-micrit-arc` | 禁用 ARC |
| `--update` | 用实际输出更新 .out 文件 |
| `-v` | 详细输出（仅 shell runner） |

## 添加新测试

1. 在对应分类目录下创建 `test_name.jeti`
2. 运行测试，用 `--update` 生成 .out 文件
3. 验证输出正确后提交

## 特殊文件

- `protocol_fail.jeti` — 无 `.out` 文件，预期编译失败
- 其他所有 `.jeti` 文件必须对应一个 `.out` 文件

## 当前状态

运行 `./test_golden.sh` 查看当前通过/失败情况。
