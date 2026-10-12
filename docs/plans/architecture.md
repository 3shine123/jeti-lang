# Jeti 当前架构

> 本文是当前架构的**唯一事实来源**，面向新贡献者。
> 历史决策与迁移过程见：`doc/stable_slots_plan.md`（stable slots / multi-TU 迁移史）、`doc/arc_intern_uaf.md`（ARC intern UAF 修复记录）。
> 用户向文档见 `README.md` / `CHINESE.md`。

## 1. 编译流程

Jeti 是 Objective-C 风格语法转译到 C99 的静态语言。`jetic` 的流水线（Rust，`crates/`）：

```text
.jeti/.jth 源文件
  → 预处理（#import 内联、C 轨道分离）
  → lexer / parser → CST
  → elaborator（@interface/impl 匹配、协议合并）
  → binder（符号表、selector 注册、协议方法传播）
  → 各降级 pass（eh desugar → defer splice → pattern-switch → ARC 注入 → checker）
  → codegen → C99
  → C 编译器（clang/gcc）→ .o → 链接 libjeti.a (+ libjetifoundation.a)
```

要点：

- **无运行时消息派发**——没有 `objc_msgSend`，全部方法是静态 vtable 派发，选择子在编译期解析为槽位索引。
- checker 默认开启（协议一致性、类型兼容、格式串检查等）；`-fno-checker` 可关。
- EH 默认后端 `checked`（见 §9）。
- 链接期自动带上 `libjeti.a`（运行时）与 `libjetifoundation.a`（若找到，见 §7）。

## 2. `.jth` / `.jeti` 语义

| 扩展名 | 角色 | 说明 |
|---|---|---|
| `.jth` | Jeti 头文件 | 声明：`@interface`、`@protocol`、typedef、struct。只经 `#import` 内联，从不直接编译。 |
| `.jeti` | Jeti 实现文件 | 定义：`@implementation`、函数、`int main`。两种用法：① **独立 translation unit 编译**（multi-TU 模式）；② 在 self-contained 模式下被 `#import` **内联**进主文件。 |

`#import` 只对 `.jth`/`.jeti` 做 jeti 内联；`#include` 的 `.h`/`.c` 原样透传给 C 编译器。jetic 总是定义 `__JETI__` 宏，供需要同时被纯 C 编译器使用的头文件做 `#ifdef` 分支。

`.jth` 头文件在 multi-TU 模式下**必须保留完整 struct 布局**（ivar 区）——因为 Jeti 是 C 超集，`@public` ivar 直访、struct 字段访问、成员函数指针调用都是合法用户代码，客户端 TU 需要完整布局，不能把 opaque struct 当默认规则。

## 3. self-contained 与 multi-TU 的区别

### self-contained / unity mode

```jeti
#import <Foundation/Foundation.jeti>   // 自包含伞头：声明 + 实现全部内联
```

- Foundation 的实现被 `#import` 内联进你的 TU，**无需任何库**。
- 内联进来的实现**不是 main 文件**，其类元数据是**弱符号**（R2 规则，见 §4）。
- 适合单文件、快速试验、兼容旧构建方式。缺点：每个使用 Foundation 的 TU 都复制一份实现，编译慢、体积大。

### precompiled Foundation / multi-TU mode（推荐工程模式）

```jeti
#import <Foundation/Foundation.jth>   // 纯声明伞头：零实现内联
```

```bash
./tools/build-foundation-lib.sh                    # 构建一次 → libjetifoundation.a
jetic app.jeti -o app                                # jetic 自动找到并链接库
# 或显式：
jetic app.jeti -I include -L target/foundation -ljetifoundation -o app
```

- Foundation 只在库里存一份；你的 TU 只生成自己的代码 + 弱占位元数据。
- 多个 TU 时用多输入：`jetic main.jeti lib.jeti -o app`（`.o`/`.a` 也可直接作输入）。
- auto-link 规则：仅**纯声明客户端**自动链库；TU 内联了 Foundation 实现（`Foundation.jeti`，直接或经导入的 `.jth` 传递）时跳过，避免库的强 vtable 赢得弱合并而触发 `__sig` 中止。

## 4. owner / strong / weak metadata 规则（R2）

「owner」= **该类的 `@implementation` 位于本 TU 的 main 文件**（不是经 `#import` 带进来的）。

| TU 角色 | 生成的类元数据（vtable / meta vtable / `JETI_CLASS_$_X` / getClass） |
|---|---|
| owner TU | **强符号**（真实表） |
| 纯声明客户端 / 内联方 | **弱桩**（槽位保留，未实现的方法槽为 NULL） |

后果：

- 两个 TU 都在各自 main 文件实现同一个类 → **链接期 duplicate symbol**（这是故意的、清晰的错误；不是运行期错位）。
- 链接时库/owner 的强表胜出，客户端弱桩被合并掉——这就是"纯声明客户端保持弱，正是正确"的原因。
- **不需要任何手动旗标**：`-fstrong-metadata` 已从编译器删除，归属由"谁的 main 文件持有 `@implementation`"自动推导。

## 5. vtable 公共段、私有段与 `__sig`（R1/R3，已完成）

每个类一个 uniform `struct jeti_vtable`（函数指针数组），布局分两段：

```text
[公共段] 该类在 #import 展开后（即共享 .jth）声明的方法，字母序
[私有段] 仅在本 TU main 文件出现的方法，字母序，追加在尾部
```

- **R1**（字段顺序）：公共段内任意方法的槽位偏移在所有 TU 中一致；私有段只影响自己 TU 的尾部。
- **私有方法**只能在 owner TU 内派发（跨 TU 没有槽位）——把要跨 TU 调的方法写进共享 `.jth`。
- **R3**（`__sig` 校验）：启动期签名校验只覆盖**公共段**（本 TU 见到的声明集），允许尾部私有段差异。签名不一致（例如某 TU 的 `.jth` 过期）会大声 abort 并打印双方方法集，而不是静默读错槽位。类别新增协议通过 category TU 的幂等注册合并进 owner 的 `NPClass`；类别方法声明仍必须出现在共享 `.jth`。
- **R2**（实例归属）见 §4。三条均已落地并由 `tests/multi_tu/`（13 用例）守护。

## 6. class metadata：静态初始化

- owner TU 通过**静态 `NPClass` 定义初始化**类元数据（编译期常量，`__data` 段），不依赖运行期构造调用。
- `jeti_metaInit` 保留为**兼容路径的幂等回填**：仅 self-contained 单 TU 构建（`#import "*.jeti"` 伞形内联）中，弱合并后真正被执行的那份会补齐静态初始化没覆盖的部分。正常 owner / multi-TU 路径**不依赖**它。

## 7. Foundation 构建方式

预编译库由 `tools/build-foundation-lib.sh` 以**逐 TU wrapper** 方式构建：

1. 对每个 Foundation 类实现（`NPObject.jeti` … `NPError.jeti`），脚本生成一个 wrapper TU = `#import <Foundation/Foundation.jth>`（声明面）+ 该 `.jeti` 全文（实现）；
2. 逐个转译、C 编译成 `.o`——每个 wrapper 的 main 文件就是该 `.jeti` 本身，所以 R2 判定每个类都是 owner，元数据为**强符号**；
3. `ar` 归档为 `libjetifoundation.a`，`nm` 校验强符号（9/9）。

细节：伞头 `Foundation.jeti` 不是类，被构建脚本显式跳过（归档它会重复内联全部实现）；库经 jetic auto-link 自动链接（查找顺序：二进制旁 → `target/foundation` → `/opt/jeti/lib` → `/usr/local/lib/jeti` → 用户 `-L`），仅对纯声明客户端生效；`-ffreestanding`、shared 模式、显式 `-ljetifoundation` 均抑制自动链接。

## 8. 泛型：单态化为主，裸拼写擦除兼容

- **主模型是编译期单态化**：`Box<T>` / `NPArray<T>` / `NPDictionary<K,V>` 等泛型类按使用点实例化（专属结构体、方法副本、vtable/元数据，`T` 以 `TypePrim::Param` 哨兵按参数名替换）；checker 把接收者的 `type_args` 代入方法签名做元素类型检查。
- **裸容器拼写保留擦除兼容行为**：不带实参的 `NPArray`（无 `<...>`）按 `id` 擦除处理，行为与历史版本一致、零迁移；裸 → 特化赋值会发 warning（`-Werror` 可升级），特化 → 裸静默。
- `@[...]` 字面量全元素同型时推断为 `NPArray<X>`，混合回退裸 `NPArray`。
- **协议约束（`T : Proto`）**：类级 bound，编译期强制。接受 ObjC 拼写 `T : id<Summable>`（剥 `id<>` 按裸协议名存储）、裸名 `T : Summable`、类指针 bound `T : NSObject *`。checker 在**显式特化的实例化点**（变量声明）验证实参：违约报 error（一次报齐）；逃逸通道——`id`/`instancetype`/嵌套类型参数槽/未解析类（forward 壳）/无法解析的 bound 名——一律放行（漏报优于误报）。继承规则：子类必须重申父类 bound（显式拼写，`.jth` 读者看到全部约束）、不得弱化（重申的 bound 须相等或更具体：协议在父 bound 继承链上 / 类是父 bound 子类）；实例化沿类链检查可证明位置的祖先 bound。裸拼写（无 type_args）永不触发（擦除兼容）；`-fno-checker` 关闭；**bound 不进 codegen，golden 输出逐字节不变**。不支持方法级约束。设计定稿见 `doc/generic_bounds_plan.md`。

## 9. 异常处理：checked 默认，legacy 回退

| 后端 | 旗标 | 机制 | 限制 |
|---|---|---|---|
| `checked`（**默认**） | `-eh checked`（默认即开启） | 显式错误旗标 + 调用点检查链，零 `setjmp/longjmp`；ARC 每帧正常结算 | `@throw` 只能作整条语句；守卫密度由异常效果分析收窄（零 `@try` 程序零守卫） |
| `legacy` | `-eh legacy`（别名 `-eh sjlj`） | 旧 SJLJ：`setjmp/longjmp` + TLS 异常槽 | 跨函数 `@throw` 会跳过中间帧的 scope-end release（中间帧 owned 对象泄漏） |

## 10. 当前已知限制（如实）

- **legacy EH 跨帧泄漏**：见 §9；需要跨帧异常安全用默认 checked。
- **裸泛型只有 warning**：裸容器拼写是擦除兼容行为（无元素类型检查），真正的元素检查需要显式 type args。
- 类别追加协议要求类别方法声明位于共享 `.jth`；类别 TU 通过启动期注册补入 owner metadata（详见 `tests/multi_tu/16_category_protocol`）。
- **泛型类方法返回类型的实例化**不完整（`ROADMAP.md`）。
- **无 debug info**（不生成 DWARF）。

源码位置当前只由预处理器逐行 `SourceMap` 支持部分诊断回映；AST 尚无统一文件级完整 span，codegen 也未发射 `#line`。因此上面的“无 debug info”仍是现状。SourceSpan、source map、Clang DWARF、LSP/debugger 的目标与阶段计划见 `doc/source_locations_debug_lsp_plan.md`。

## 11. 推荐命令示例

```bash
# 构建 jetic（Rust，debug 二进制在 target/debug/jetic）
cargo build --release

# 单测 + 全量集成测试（test_all.sh 是 test_all.py 的并行包装）
cargo test --workspace
./test_all.sh -j4

# 看 jetic 生成的 C（排查第一步）
jetic -rewrite-jeti app.jeti -o app.c

# self-contained 模式（单文件、快速试验）
jetic run hello.jeti

# multi-TU（多个 jeti 源 + .o/.a 混合输入）
jetic main.jeti lib.jeti -o app

# 预编译 Foundation 库（推荐工程模式）
./tools/build-foundation-lib.sh      # 构建一次
jetic app.jeti -o app                  # 纯声明客户端：auto-link

# 其他常用
jetic -trace-refcount app.jeti         # 引用计数静态追踪
jetic app.jeti -emit-bridge-header app.h -o app.c   # C 桥接头
jetic -ffreestanding kernel.jeti       # 裸机（自备运行时）
```

## 12. KVC 与 NPPredicate（谓词 / 过滤）

谓词引擎**全部在 Foundation 库的 C 里**（`NPPredicate.jeti`）；jetic 从不解析格式串，它只负责发射键值访问表。

### KVC 访问表（`JETI_KVC_$_X`）

- **门控**：TU 的展开导入集中出现 `NPPredicate` 时才发射（`#import <Foundation/NPPredicate.jth>`，或经 `NPArray.jth` / `NPSet.jth` 传递性带入）。
- **归属**：表随类元数据走 —— owner TU 发**强**表（写进静态 `NPClass` 的 `.kvc_entries`），纯声明客户端发弱桩，链接期强表胜出；与 §4 的 R2 是同一套合并规则（`tests/multi_tu/13_kvc_predicate` 守护）。
- **条目 = 零参实例 getter**（ObjC getter 约定）：排除运行时/基础设施方法（`init`/`copy`/`retain`/`release`/`class`/`description`/`isEqual:`/`hash`/`self` …），私有方法只在 owner TU 中出现。
- **包装分类**（按声明返回类型；判据先看指针拼写）：
  - 内建标量 → 走 `NPNumber` 装箱的包装 getter；
  - `id`/`instancetype`/指向非内建类型的指针 → 对象路径（直接返回 `id`）；
  - **按值 struct/union（如 `NPRange`）与指向内建类型的指针（`int *`/`char *`/`void *`）不发表条目** —— 前者 `(id)` 转换非法，后者会把指针当整数装箱（`-Wint-conversion`）；这类键保持未注册，查键按"未命中"处理。
- **查键**：`jeti_kvc_value(receiver, "key")` 按点号分段走链，任一段未命中即返回 `NULL`。`-valueForKey:` 对未命中**大声 abort**；谓词求值路径把未命中当 `nil`（比较为假、`= nil` 为真）——不会静默误命中。
- self-contained 伞头构建中 `jeti_metaInit` **幂等回填** `.kvc_entries`（§6）；owner / multi-TU 路径不依赖它。

### NPPredicate DSL

格式串**运行时**解析，支持：比较 `= == != <> < > <= >=`；连接 `AND && OR || NOT !`；字符串 `BEGINSWITH ENDSWITH CONTAINS LIKE MATCHES`（`[c]` 忽略大小写、`[d]` 接受并忽略）；`IN {...}` 与 `BETWEEN {a, b}`（聚合字面量同时接受数字与引号字符串）；量词 `ANY` / `ALL` / `NONE <数组键>`，其中**元素是隐式主语**（`ANY tags = 'dev'`、`ANY tags LIKE '*dev*'`）。`%@` 占位符在解析前替换，`predicateFormat` 回读的是替换后的串。完整语法与语义以 `include/Foundation/NPPredicate.jth` 为准。

### 宿主过滤 API

`NPArray -filteredArrayUsingPredicate:` / `-indexOfObjectMatchingPredicate:`、`NPMutableArray -filterUsingPredicate:`、`NPSet -filteredArrayUsingPredicate:`（Set 子类经 `NPSet.jth` 公共段继承该槽位）；`nil` 谓词 = 恒等（不过滤）。这些方法落在**公共段**，新库因此带新的 `__sig`：升级后需重建 `libjetifoundation.a` 并重编客户端；不新增 ivar、不改存储布局。

### 回归

`tests/predicate_filter_test.jeti`（自检 38 项：DSL / 占位符 / 量词 / KVC 类链与未命中 / 宿主 API）+ `tests/multi_tu/13_kvc_predicate`（跨 TU 强弱表合并）+ `tests/nil_messaging_test.jeti`（按值 struct 返回不得发表条目）。
