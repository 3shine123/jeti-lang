# Categories 与协议继承跨 TU 支持规划

> 状态：**阶段一、二、三已实施并实证**（2026-10-07）。见文末 §8「实施记录」；
> §1–§7 保留为设计档案，其中部分假设已被实测修正（以 §8 为准）。
> 起因：`doc/architecture.md` 已知限制——「categories / 协议继承的跨 TU 场景
> 支持有限」（`ROADMAP.md` 语言缺口两条）。本规划回答两个问题：ObjC 是怎么
> 做的；在 jeti「ObjC 语义 + 静态实现 + 无 msgSend」的铁律下，最合理的对应
> 方案是什么。结论先行：**ObjC 用运行期合并类别，jeti 没有运行时，就让链接
> 器当运行时——类别方法槽用弱符号，链接期完成 ObjC 在 `attachCategories`
> 里做的事**。

## 1. 现状盘点（本会话逐一核实过的代码事实）

| 环节 | 现状 | 证据 |
|---|---|---|
| parser | 类别语法**完整**：`@interface Dog (Tricks)` / `@implementation Dog (Tricks)` 都解析，`category_name` 入 CST；空括号 `@interface Dog ()` 被 `cat.is_empty() → None` 吞成普通 interface（class extension 语义是**意外的**，非正式） | `parser.rs:4625-4648`、`4917-4921`、`4951-4970` |
| binder | 类别识别为 `is_category`，跳过 ivar 收集（类别不能加 ivar，与 ObjC 一致）；方法名并入类方法表 | `binder.rs:319`、`377` |
| codegen | 同 TU 内类别 ivar 合并已有先例（主 interface 的 ivar 与类别 @property 合成 ivar 合并进同一 struct）；协议方法进 vtable 槽、本 TU 无实现则槽置 NULL（"protocol stub" 先例） | `codegen.rs:4126-4131`、`38-41` |
| checker | 协议一致性检查**逐 TU**：只检查本 unit 内有实现的类，header-only 类跳过；`@optional` 豁免；沿协议父链 + 父类链递归（bounds 复用的 `class_conforms_to_protocol`） | `checker/src/lib.rs:3141-3148`、`1360-1372` |
| 运行时 | `NPClass.protocol_count = 0` 硬编码，`conformsToProtocol:` 运行时查询未实现（`ROADMAP.md` 明欠） | `codegen.rs:7640` |
| 测试 | 单 TU 类别工作正常（`tests/golden/08_categories/`）；跨 TU 无类别场景 | golden 08 |
| 基础设施 | R1（公共段=导入头声明集，槽位全 TU 一致）/ R2（owner 强元数据，声明方弱桩）/ R3（`__sig` 只盖公共段）已固化，`tests/multi_tu/11_vtable_private_slots` 钉住私有尾合法 | `AGENTS.md` 铁律 |

**短板的确切形状**：类别实现写在另一个 TU 时，owner TU 的 vtable 静态结构里
没有（或以强符号引用）该方法的函数指针——没有运行时去「挂载」，链接期也没
有约定去「汇入」。协议继承跨 TU 同理：槽位布局依赖双方看到同一声明集，一致
性检查只在 owner TU 跑。

## 2. ObjC 的做法（调研结论）

### 2.1 类别：运行期合并

- 编译器为每个类别生成独立 `_category_t`（类引用 + 实例/类方法列表 + 协议 +
  属性），放进 `__objc_catlist` 节——**类别不修改类本身的结构**。
- 运行时加载镜像时 `attachCategories` 把类别方法列表**前插**到类的方法列表
  数组；方法查找沿列表从前向后，所以**类别方法覆盖主类同名方法**（不是替换，
  是查找顺序）。多个类别同名 → 后编译的镜像后插、先查找，顺序依赖是著名坑。
- 类别**不能加 ivar**（struct 布局编译期已定）；类别里的 @property 只是个
  声明，getter/setter 要自己写（class extension `@interface Dog ()` 才能加
  ivar，它在编译期合并进主类，不走运行期类别通道）。
- 全部机制依赖 `objc_msgSend` 按名查找——jeti 没有这一层。

### 2.2 协议：容器式继承 + 双层一致性

- `protocol_t` 内嵌 `protocol_list`（父协议），继承即容器包含；`Protocol`
  对象自身不可变。
- `protocol_conformsToProtocol` 沿内嵌列表**递归**判定；类的
  `conformsToProtocol:` 查类协议表 + 逐项递归。
- 编译期：required 方法未实现报警告/错误；@optional 只做声明。运行期元数据
  支撑 `conformsToProtocol:` 内省。

### 2.3 静态派发语言的对照（Swift / Rust）

- **Swift**：extension 方法默认**静态派发**，子类无法多态重写（除非 `dynamic`
  / `@objc` 走消息派发）——静态语言加扩展方法的通病：可派发性丢失。
- **Rust**：干脆禁止给外部类型加固有方法，扩展 = trait impl + 孤儿规则
  （外部 trait × 外部类型禁止），冲突在编译期非法，无需运行期合并。
- **共性结论**：没有 msgSend 的语言，都用「编译期/链接期解决冲突」替代
  「运行期合并」；代价是把 ObjC 的顺序依赖坑换成显式的合法性规则。

## 3. 核心设计决策

### D1 —— 类别方法的跨 TU 汇入 = 弱符号槽（链接器当运行时）

owner TU 发射 vtable 实例时，对「声明在共享 `.jth`、实现可能在别的 TU 的类别
方法」生成：

```c
/* owner TU 生成的 C */
extern void Dog_bark(Dog *self) __attribute__((weak));   /* 弱 extern */
/* vtable 初始化器里 */
.bark = Dog_bark,
```

- 类别 TU（`@implementation Dog (Tricks)`）发射**强符号** `Dog_bark` →
  链接期弱引用被强定义吸收，槽位指向类别实现。等价于 ObjC
  `attachCategories` 在链接期完成。
- 类别 TU 没参与链接 → 弱未定义符号取址为 NULL → 槽位 NULL。行为与本会话
  刚加的 selector 可见性 warning 同类：**槽存在但空**，调用前 checker 已有
  诊断，不静默错派发。
- R1/R2/R3 全部不动：槽位属于公共段（声明在导入头集合里），布局处处一致；
  元数据仍只有 owner 强发；`__sig` 自动盖到类别方法（它在声明集里）。
- 备胎（若探针 P0 证明 PE/zig cc 下弱未定义取址不为 NULL）：改为**强 extern
  引用**——缺类别 TU 时链接器直接报 undefined symbol，比运行期 NULL 更早
  更响，代价是「类别必须随库一起链接」失去 ObjC 的可选性。二选一由探针
  数据决定，不强推弱符号。

### D2 —— 布局规则零新增：类别方法按声明位置入段

- 声明在共享 `.jth` 的类别接口方法 → 公共段（R1 现有定义自动涵盖，需把
  公共段收集器扩展到 CategoryInterface——现状是否已计入列为 P0 探针②）。
- 仅出现在本 TU 主文件的类别（声明+实现都在 .jeti）→ 私有段，只许 TU 内派发
  ——与「跨 TU 方法必须写进共享 .jth」既有铁律**完全同构**，无新法。
- `__sig` 语义不变：公共段一致即合法，私有尾差异合法（11 号场景先例）。

### D3 —— ivar 规则与 ObjC 严格对齐

- 跨 TU 类别**禁止**加 ivar / @property 自动合成（struct 布局是 C ABI，跨
  TU 加字段必然错位）。checker 对「跨 TU 类别带 ivar 或待合成 @property」
  发 error。ObjC 同规则。
- 同 TU 类别保持现有 ivar 合并行为（`codegen.rs:4126` 先例），不回退。

### D4 —— 同名冲突：静态模型直接非法，比 ObjC 更早失败

- 类别方法与主类方法同名：同 TU → binder/codegen 层直接报错（ObjC 是静默
  覆盖 + 顺序依赖坑，jeti 没有运行期查找顺序，无法复刻其语义，复刻半吊子
  不如禁止）；跨 TU → 强符号 duplicate symbol，链接期天然报错，与 R2
  「双 @implementation 撞链接失败」同一哲学。
- 两个类别同名方法：同上，链接期 duplicate，明确报错。

### D5 —— 协议继承跨 TU：以验证为主，补两处静态数据

`#import` 内联意味着 owner TU 天然看到完整协议父链，槽位布局理论已成立。
待办收窄为：

1. **P3 探针实证**：`id<P>` 类型接收者的派发在跨 TU 场景槽位一致（父协议
   required 方法进公共段）；不成立再补收集器逻辑——先证明，后动手。
2. **还债**：`NPClass.protocol_count/protocols` 静态填充（类符号 protocols
   + 父链闭包，编译期可全算），`conformsToProtocol:` 运行时查询随之上线
   ——`ROADMAP.md` 明欠条目，顺手闭环。

### D6 —— class extension `@interface Dog ()` 正式化（可选，最后做）

现状空括号被吞成普通 interface 是意外行为。正式语义照抄 ObjC：同文件、
编译期合并、**可以**加 ivar（生成进主 struct）。做法：parser 保留空括号为
`category_name = Some("")` 或独立 kind，binder 合并进主类符号。非跨 TU
问题，优先级最低；不正式化则维持现状并在文档标注。

## 4. 验证计划：先探针，后动手（项目纪律）

全部探针放 `probes/`，先证伪/证实，再写实现代码。

| # | 探针 | 证实/证伪什么 | 决定 |
|---|---|---|---|
| P0① | owner `.jeti` + 类别 TU 分开编译链接，类别方法跨 TU 调用 | 现状到底怎么断：链接错、NULL 槽 segv、还是碰巧能跑 | 记录基线行为，作为 D1 的对照 |
| P0② | `.jth` 里 `@interface Dog (Tricks)` 声明，两个 TU import 后各查 vtable 布局 | 公共段收集器是否已把 CategoryInterface 声明计入 R1 段 | 已计入 → D2 零改动；未计入 → 收集器一处扩展 |
| P1 | 弱 extern 槽：类别 TU 在场/缺席两种链接 | 弱未定义取址在 clang/PE(zig cc) 下是否可靠为 NULL | NULL → D1 弱符号；非 NULL → D1 备胎强 extern |
| P2 | 类别同名主类方法（同 TU） | 现有行为是什么（静默覆盖？编译错？） | 决定 D4 报错挂在 binder 还是 codegen |
| P3 | `id<P>` 接收者跨 TU 派发，父协议方法槽位 | 协议继承跨 TU 布局是否天然成立 | 成立 → D5 只还债；不成立 → 补收集器 |
| P4 | 跨 TU 类别带 ivar / @property 合成 | 现状是否已经炸（struct 错位） | D3 error 挂 checker 哪个臂 |

## 5. 实施切分（规划顺序，未排期）

1. **阶段一（最小闭环，纯 codegen + 链接）**：P0/P1 探针 → 弱符号槽发射
   （D1）→ 公共段收集器扩展（若 P0② 需要，D2）→ `tests/multi_tu/14_…`
   新场景：类别 TU 在场（全过）+ 缺席（槽 NULL，调用被 checker warning
   覆盖）两个用例。
2. **阶段二（checker 加固）**：P2/P4 探针 → D3 ivar 禁令 + D4 同名冲突
   诊断（error，编译期）。
3. **阶段三（协议还债）**：P3 探针 → `protocol_count`/`protocols` 静态
   填充 + `conformsToProtocol:` 运行时上线（D5.2）→ 删 `ROADMAP.md`
   对应两条欠账。
4. **阶段四（可选）**：D6 class extension 正式化。

每阶段完成必须复跑回归基线（`cargo test --workspace`、`./test_all.sh`、
`multi_tu`），golden 逐字节不变是硬门槛——本方案所有改动都在「链接期符号
可见性」与「checker 诊断」，不触碰既有发射布局，理论上 golden 零影响，
仍以实测为准。

## 6. 与既有铁律的一致性核对

| 铁律 | 本方案 | 结论 |
|---|---|---|
| R1 公共段槽位全 TU 一致 | 类别方法按声明位置入段，声明集就是 R1 的输入 | 不变（D2） |
| R2 owner 强元数据 | 类别不改元数据归属，只动 vtable 槽的符号强度 | 不变 |
| R3 `__sig` 盖公共段 | 类别方法在公共段内，自动被签名覆盖，双 TU 方法集分歧照旧 abort | 增强（覆盖面更全） |
| 无 msgSend、静态派发 | 链接期弱符号吸收 = 静态模型的「运行期合并」等价物 | 不违背 |
| C 超集铁律 | `__attribute__((weak))` 是标准 GNU 扩展，clang/zig cc 皆支持 | 不违背 |
| ObjC 语义对齐 | ivar 禁令、extension 合并、required/optional 与 ObjC 一致；唯一偏离是同名方法从「静默覆盖」改为「编译期/链接期报错」——静态模型无法复刻查找顺序语义，显式拒绝优于错误复刻（§2.3 共性结论） | 有意偏离，已记录 |

## 7. 实施修订（2026-10-07 评审后定案）

P0/P1 探针已完成，实测结论修正了本方案的两处假设：

- **P0 实测**：类别 TU 参与链接时 `JETI_VTABLE_$_Dog` / `JETI_CLASS_$_Dog` /
  `JETI_GETCLASS_$_Dog` duplicate symbol——类别 `@implementation Dog (Tricks)`
  目前被 codegen 当作类的 owner 强发元数据。**实现前提：类别实现 TU 不得
  认领类所有权**（`owned_classes` 排除 category impl），否则 D1 无从谈起。
- **P1 实测（macOS arm64 clang）**：默认 ld **拒绝**未定义弱 extern（链接期
  报错，非 NULL）；`-undefined dynamic_lookup` 下取址可靠为 NULL。因此弱
  符号主案**必须由 jetic 驱动在链接时注入 `-Wl,-U,_<sym>` 或等效 flag**，
  或者退到备胎强 extern。链接器行为是平台差异点，不能当稳定语义。
- **P1 矩阵扩展（评审采纳）**：补 Linux clang / GCC / ELF、静态库归档、
  链接顺序交换、多类别、类别缺席七种组合；若结果依赖链接顺序，弃弱符号
  主案改备胎。
- **槽位规则钉死（评审采纳，P0 已部分证实）**：槽位由共享 `.jth` 声明集
  决定，类别**实现**在场与否不改变布局（三 TU `__sig` 同值已证）；实现时
  以 `tests/multi_tu/14` 两个用例（在场/缺席 `__sig` 与槽值断言）固化。
- **selector 检查分级（评审采纳，独立于本方案立项）**：用户源内静态
  receiver 未声明 selector → error；Foundation 伞头误解析 → warning（待
  修掉 15 处 receiver 误解析后自然归零）；`id` receiver → 放行。此为
  checker 独立工作项，不阻塞类别实施。
- **泛型返回 ABI 验证（评审采纳，独立工作项）**：`+ (NPArray<T> *)make`
  需补多实例化 / 跨 TU / 函数指针 / ARC 返回 / 类+实例并存五组探针。
- **protocol_count 只是必要条件（评审采纳）**：D5.2 实施时须同时验证协议
  身份跨 TU 唯一、父链闭包完整、多级继承递归可查、与编译期检查结论一致、
  类别添加协议时 metadata 合并正确——五项缺一即不算完成。

## 8. 实施记录（2026-10-07，阶段一/二/三已落地）

### 类别跨 TU（D1 备胎定案为最终形态）

- **AST**：`AstDeclData::Class` 新增 `is_category`（elaborator 从 CST
  `category_name` 透传）。
- **所有权**：`collect_owned_classes` 排除 category impl——类别 TU 只发
  方法定义，不发强元数据（修掉 P0 实测的 duplicate symbol）。
- **槽引用**：owner TU 对类别引入的方法引用 `Owner_method` 强 extern；
  类别 TU 的定义带 weak（与普通方法合并惯例一致）。类别缺席 = 链接期
  undefined symbol，全平台响亮，无需驱动 flag。弱 extern 主案被 P1 证伪
  （macOS ld 拒绝未定义弱符号）后弃用。
- **实现坑（实测）**：类别方法标记最初做成 method_names 的并行数组，但
  method_names 在构建后被父类方法合并打乱（实测 2 项构建、11 项发射），
  索引并行静默错位——改为按名字集合 `category_method_names` 贯穿到发射点。
- **测试**：`tests/multi_tu/14_category_present`（PASS：类别在场全链路）、
  `15_category_absent`（EXPECT_FAIL：缺席链接期报错）；multi_tu 15/15。

### checker 类别规则（D3/D4）

`check_category_rules`（挂 check 单元级，error 级）：D4 同类 selector
双实现（主实现+类别、类别+类别）报错——P2 实测旧行为是静默覆盖；D3 类别
非 static 实例 ivar 报错（static 文件级全局保持合法，既有先例）。P4 实测
跨 TU 类别 ivar 本就在编译期被拦（undeclared identifier），D3 补的是
同 TU 场景的友好诊断。

### 协议元数据（D5.2）

- 管线打通：`AstDeclData::Protocol` 新变体（elaborator 原来直接丢弃
  `ProtocolData`）；`ClassInfo`/`CgClassMeta`/`CgUnit` 加 protocols 数据。
- 发射：`JETI_PROTO_$_<P>` 静态 NPProtocol 实例（parents 递归、
  required/optional 分表）+ `JETI_PROTOS_$_<Class>` 每类指针数组 +
  NPClass 的 `.protocols/.protocol_count` 静态填充与 metaInit 回填双路。
- 运行时：`jeti_class_conformsToProtocol(cls, proto)`（runtime.c）沿
  父类链 + 协议父链递归判定；`[x conformsToProtocol:@protocol(P)]` 在
  codegen 特判改写为该查询（receiver 取 `->isa`）。
- 实测修复的坑：① 协议父链里写类名（`@protocol P <NPObject>`，ObjC 合法
  拼写）会生成未定义 `JETI_PROTO_$_NPObject`——发射前过滤类名；② 泛型
  参数名混进 conformance 列表（`JETI_PROTO_$_T`）——按 type_params 过滤；
  ③ metaInit 复合字面量里的 inline `NPProtocol*[]` 在栈上，返回后悬垂
  （conforms 查询 segfault）——改发命名静态数组；④ 命名空间 FQN 截断
  （log_analyzer 的 `LogTool` 残名）——三处发射统一走 `known_protocols`
  过滤，未声明协议的条目丢弃（漏报优于误报）。
- 验证：conforms 探针（子协议↔父协议正向、无关类否定）全对；回归
  cargo test 53 全过、test_all 350/360（与基线一致）、multi_tu 15/15。
- golden：harness 只比对 `.out`，全部协议相关 golden 输出不变；07 的
  `conformance.c` 是改名前陈旧物（不被比对），已顺手重生成。
- 已补：协议符号 `JETI_PROTO_$_P` 以 weak 定义跨 TU 合并为同一链接单元身份；
  类别 TU 对 owner 类发 constructor 注册，把类别新增协议并入 `NPClass` 的
  协议表，注册操作幂等。回归用例见 `tests/multi_tu/16_category_protocol`。
- 仍需保持的约束：类别方法声明必须出现在共享 `.jth`，否则公共 vtable 段不一致；
  只有类别实现 TU 注册新增协议，声明-only 客户端不会重复注册。

### 遗留清单更新

ROADMAP「Categories across TUs」「Protocol inheritance across TUs」
「conformsToProtocol: runtime」三条已闭环（待勾选）。
