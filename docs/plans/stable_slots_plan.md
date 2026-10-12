# 稳定 vtable 槽位（3b）实施计划

> **本文是 stable slots / multi-TU 迁移历史和决策记录。**
> 当前架构请阅读 `doc/architecture.md`。
>
> 状态：**已完成**——R1/R2/R3 已于 2026-10-02/03 落地并全量验证（见 §9–§11）；
> 原验收用例 `tests/multi_tu/11_vtable_private_slots/` 已从 `EXPECT_FAIL` 摘牌转为
> `expected.txt` 正向比对。下文按时间顺序保留计划、探索、回退与验证记录；
> 与现状不符的表述（`Foundation.decl.jth`、`-fstrong-metadata`、`jeti_metaInit` 旧限制等）
> 为历史记录：**当前状态见 §11 / `doc/architecture.md`**。
> 原始验收目标：“待实施。验收用例已入库：`tests/multi_tu/11_vtable_private_slots/`”

## 1. 问题

Jeti 为每个 TU 生成**一个 uniform `struct jeti_vtable`**，字段是「该 TU 见过的全部实例方法」，
无 manifest 时按**字母序**排列（`crates/codegen/src/codegen.rs` 的 `None => sort()` 分支）。

于是两个 TU 只要方法集不同，布局就不同：

```
lib  : 43 字段  sig=0xedd6fb9b1c8b2ce1
main : 43 字段  sig=0x07f3ae69bac7647b
jeti: fatal: vtable layout mismatch across translation units.
```

而链接器会把两个 vtable 实例 **weak 合并成一份**，所以一旦 sig 校验被绕过（或场景更隐蔽），
派发就会读到错误的槽位。当前靠 `jeti_verify_vtable_sig` **大声失败**兜底 —— 这是安全网，
不是解药：**合法**的两 TU 分工也会被拒。

复现（已入库）：

* `lib.jeti` 实现 `Widget` + 一个头文件没声明的私有方法 `privateHelper`；
* `main.jeti` 只通过 `model.jth` 调用 `Widget`，并定义自己的类 `App`。

两者对 `Widget` 的公共认知完全一致，却被判为布局冲突。

## 2. 目标

**让 vtable 的「公共部分」在所有 TU 中布局一致；私有方法只影响自己 TU 的尾部槽位。**

验收标准：

1. `tests/multi_tu/11_vtable_private_slots/` 正常运行并输出 `run=42`（届时删除该目录的
   `EXPECT_FAIL` 与 `EXPECT_FAIL_MATCH`）；
2. `tests/multi_tu/09_layout_mismatch/` **继续** `EXPECT_FAIL` —— 两个 TU 各自实现同一个类且
   方法集不同，那是真正非法的用法，必须继续大声报错；（R2 落地后该用例改在**链接期**以
   `duplicate symbol` 大声失败，比运行期 `__sig` 中止更早、更明确，`EXPECT_FAIL_MATCH`
   已相应更新。）
3. 全部门槛不回归：`test_all` / `eh_matrix` / `arc_order` / `eh_diff` / trace / `clang_gcc_stress`。

## 3. 三条规则

### R1 —— 字段顺序：公共段 + 私有段

```text
[公共段] 来自 #import 展开（即头文件）声明的方法，字母序
[私有段] 仅在本 TU 主文件里出现的方法，字母序（追加在尾部）
```

私有段在尾部是关键：**公共段内任意方法的 offset 在两侧 TU 中完全一致**。
（manifest 模式下已有同样的规则：“Methods unknown to the manifest … are appended at the end”。）

### R2 —— 实例归属：谁实现，谁生成　✅ 已落地（2026-10-02，详见 §9）

`JETI_VTABLE_$_X`、`JETI_META_VTABLE_$_X_inst`、`JETI_GETCLASS_$_X` 等元数据只由
**拥有该类 `@implementation` 的 TU** 发**强符号**，其余 TU 发弱桩。

否则：lib 的 `Widget` 实例（41 + `privateHelper`）会被 main 生成的实例（41 个字段）weak 合并覆盖，
lib 自己的私有方法调用就会越界。

「拥有」的判据最终定为：**该类的 `@implementation` 位于本 TU 的 main 文件**（不是 `#import`
带进来的）。这样自包含模式下所有 Foundation 实现仍然是弱符号（多个 TU 重复的副本照旧合并），
而两个 TU 都在各自 main 文件里实现同一个类 → **链接期 duplicate symbol**。

### R3 —— 签名校验面向公共段

`jeti_verify_vtable_sig` 比较的应是**公共段**（本 TU 见到的声明集）的签名，
允许尾部私有段存在差异。这样 R1 + R2 生效后，校验从“拦合法场景”变回“抓真非法场景”。

## 4. 为什么这三条足够

| 场景 | 结果 |
|---|---|
| main 调用 `Widget.show`（公共） | offset 在公共段内，两侧一致 ✓ |
| lib 调用自己的 `privateHelper` | 走 lib 生成的 `Widget` 实例，私有段是自己写的 ✓ |
| main 调用 `App.run` | 走 main 生成的 `App` 实例 ✓，别人不碰 |
| 两个 TU 都实现同一个类且方法不同（09 用例） | 仍是非法：R2 会让两边都定义实例 → 链接器报冲突，或 R3 判定公共段不符 → 继续大声失败 ✓ |

## 5. 分步实施

| 步 | 内容 | 验证方式 |
|---|---|---|
| **A** | pipeline 收集「公共方法集」（来自导入文件的方法声明，用 `source_map` 判定），传入 codegen | 单元可读：打印集合内容 |
| **B** | R1：`None` 分支改为「先字母序 sort，再稳定地把私有段排到尾部」 | 对比两个 TU 的 `struct jeti_vtable`：公共段 offset 应一致 |
| **C** | R2：元数据 / vtable 实例只由实现该类的 TU 生成 | `11_vtable_private_slots` 应能跑通（sig 校验此时可能仍需 D）—— ✅ 已落地（见 §9） |
| **D** | R3：sig 校验改为公共段 | ✅ 已实现并验证（2026-10-03，见 §10） |
| **E** | 删除 11 用例的 EXPECT_FAIL 标记，跑全套回归 | ✅ 标记已删、`expected.txt`（`run=42`）比对通过；multi_tu 11/11 |

## 6. 风险与回退

* **风险点**：R2 触及元数据生成条件（`JETI_VTABLE_$_X`、`JETI_CLASS_$_X`、`JETI_GETCLASS_$_X`），
  import 链（diamond、category）下的“谁拥有实现”判定需要仔细处理。
* **回退**：每步独立提交，`git reset --hard <上一步>` 即可；`11_vtable_private_slots` 的
  `EXPECT_FAIL` 标记本身就是“尚未完成”的断言，任何一步倒退都会让它继续 PASS（failed as expected），
  不会静默变成假绿。
* **不要**顺手去动 `weak` 的默认策略（那是 P3 的话题）；本计划只解决布局一致性。

## 7. C 的探索结论（第一次尝试，2026-10-02，已回退）

A+B 完成后动手做 C，试了两条路，都只走到一半：

1. **私有方法不进 vtable** —— 有效，且比"公共段 + 私有段分区"更干净。实测两个 TU 的
   `struct jeti_vtable` 字段**从"73 相同 + 1 不同"变成 73 = 73 完全一致**，私有方法改为
   直接调用 `Owner_method(recv, sel, ...)`（只有定义它的 TU 能走到这里，所以不需要槽位）。
   这条规则本身应当保留。

2. **vtable 实例改 `extern`**（只让实现类的 TU 生成实例）—— 方向正确但**不够**：
   编译通过、sig 冲突消失，运行时段错误。原因是**只看到声明的 TU 仍会生成
   `JETI_META_VTABLE_$_X_inst`（类方法派发用）和 `JETI_CLASS_$_X` 的空实例**，其 NULL 槽
   可能在 weak 合并中胜出。

**卡点**：类元数据的生成分散在 **6 个 `for cm in &unit.classes` 循环**里
（vtable / meta vtable / class / getclass / 前置声明 …），要做 "实例归属" 就得让它们全部
遵循同一条件，并配套一片 `extern` 声明区。改动面比预估大，且中途无法保证不引入回归，
因此**回退**（`git checkout` 掉未提交的改动；A+B 已在 `255b322` 里）。

**方向性发现（重要）**：本计划原以为"3b 是 P3 的地基"，但 C 的尝试说明**依赖可能是反的**：

* 在**自包含模式**下，Foundation 的实现被每个 TU 展开，于是"这个类归谁实现"根本无从判断
  （每个 TU 都"实现"了 NPString）——这正是 C 卡住的根源；
* 若先做 **P3（预编译库 + 纯声明头）**，用户 TU 里就**只有自己的 `@implementation`**，
  "实例归属"就退化成简单的"用户 vs 库"两分，C 会容易得多。

**建议的下一步顺序**：A+B（已完成）→ **P3**（预编译 Foundation 库 + 纯声明头）→ 回到 C
（此时只需处理"用户自己的类"，元数据归属自然清晰）。

在 C 完成之前，`11_vtable_private_slots` 保持 `EXPECT_FAIL`：它是"尚未完成"的断言，
任何回退都会让它继续以"预期失败"通过，不会静默变成假绿。

## 8. P3 进展（2026-10-02）

### 已完成

* **`include/Foundation/Foundation.decl.jth`** —— 纯声明头（14 行）。只需剥掉
  `Foundation.jth` 里的 9 行 `#import "*.jeti"`：**其它 `.jth` 本来就是纯声明**，所以这一步是
  删 9 行，不是重新组织头文件。
* **库构建已验证**（86K）：

  ```bash
  jetic -rewrite-jeti -fstrong-metadata include/Foundation/Foundation.jth -o Foundation.c -I include   # 3175 行
  clang -c Foundation.c -o Foundation.o -I include
  ar rcs libjetifoundation.a Foundation.o
  ```

  > 历史记录：`-fstrong-metadata` 已于 2026-10-03 删除（方案 A 落地，见 §11），
  > 此段仅为当时事实；当前建库统一走 `tools/build-foundation-lib.sh`，无旗标。

  ⚠️ 建库**必须**带 `-fstrong-metadata`（P3 落地时新增）：否则库里的元数据仍是
  `weak`，客户 TU 的空桩会在链接期胜出 → 段错误（详见下方「剩余一步」）。

* **实测效果**（`#import <Foundation/Foundation.decl.jth>` + 链接库）：

  | | 自包含模式（现状） | P3 模式 |
  |---|---|---|
  | 生成的 C | 含 ~3000 行 Foundation 实现 | **2085 行总计** |
  | `__attribute__((weak))` | 193 | **36** |

  也就是说：**你要的"生成代码干净"已经达成**，剩下的只是让它也能正确运行。

### 剩余一步 —— 已完成 ✅（2026-10-02，第二趟）

**问题**：用户 TU 只看到声明，所以它仍然生成 `JETI_VTABLE_$_NPString` 之类的**空桩**；
链接器的 weak 合并可能选中空桩而不是库里的真表，于是 `[s length]` 走空指针 → 段错误（rc=139）。

**实现**：

1. **CLI 标志 `-fstrong-metadata`** —— `crates/jetic/src/main.rs`（手动解析两处 + clap 补全
   定义 + `DOUBLE_TO_SINGLE` + 帮助文本），透传 `Pipeline::strong_metadata` →
   `CgUnit::strong_metadata`。
2. **codegen 元数据 emit 条件化**（`crates/codegen/src/codegen.rs`，`emit_unit_with_headers`
   内的一个 `meta_weak` 前缀）：默认仍是 `__attribute__((weak))`，`-fstrong-metadata` 时去掉：
   * `JETI_VTABLE_$_X` 实例（vtable 循环）
   * `JETI_META_VTABLE_$_X_inst`（meta-vtable 循环）
   * `JETI_GETCLASS_$_X`（getClass 循环）
   * `jeti_metaInit` / `jeti_meta_init`
   * （`JETI_CLASS_$_X` 本身是 tentative definition —— common 符号，天然合并；其**内容**由
     `jeti_metaInit` 写入，所以把 `jeti_metaInit` 变强就够了，变量无需改动。）
3. **`collect_public_methods` 判定改为「在 `@interface` 中声明」**
   （`crates/jetic/src/pipeline.rs`，即 `is_implementation: false`）。
   * **保留了「来自导入文件」的过滤**：若一并去掉，主文件自己的 `@interface`（如
     `11_vtable_private_slots` 里的 `App`）会挤进公共段，反而把公共方法错位。文档原话
     「不依赖 source_map」不准确，实测必须两者同时成立。
   * 原先两侧布局不一致的真正来源是 `NPError.localizedDescription`：它只在导入的
     `@implementation`（`.jeti`）里出现，库 TU 当成公共、客户 TU 当成私有 → 布局分叉。
     改判后两侧的公共集完全一致（sig 相等）。

### 验证（P3 端到端）

```text
sig：库 TU == 客户 TU（0xbe99f65a63e20021）
弱符号库（不加 -fstrong-metadata）：rc=139（空桩胜出 → 段错误）
强符号库（-fstrong-metadata）：len=5 s=hello，rc=0   ✅
```

回归门槛：`test_all` 343/351（0 failed）、`cargo test --workspace` 149/149、
`tests/multi_tu/run_multi_tu.sh` 11/11（`09`、`11` 仍 `EXPECT_FAIL`，未假绿）。

> 工具：`Foundation.decl.jth` 引用的两个脚本已补入库
> （`tools/make-decl-headers.sh`、`tools/build-foundation-lib.sh`）。建库**统一走脚本**——
> 它自带 `-fstrong-metadata`，并用 `nm` 校验元数据的确是强符号（弱符号会让守卫直接失败）。
> 保护测试：`tests/strong_metadata/run_strong_metadata_test.sh`（四项：强符号 / 弱客户端链接 /
> 双强符号必须 duplicate symbol / 两个所有者必须 duplicate symbol）。
>
> 顺带修掉一个与本计划无关的既有 ARC 缺陷：`full_syntax_test.jeti` 的 intern 字符串
> heap-use-after-free（`test_all` 里那唯一一个 `SUSPECT` 来源）→ 已修复，见
> `doc/arc_intern_uaf.md`，回归守护 `tests/arc_intern/`。

## 9. R2 落地：按类归属（2026-10-02）

### 判据

「拥有」= 该类的 `@implementation` 位于**本 TU 的 main 文件**（`source_map` 判定，
`collect_owned_classes`，`crates/jetic/src/pipeline.rs`）。

* `Pipeline::owned_classes` → `CgUnit::owned_classes`；
* `emit_unit_with_headers` 的 vtable / meta-vtable / getClass 三个循环各自算
  `cw = if strong_metadata || owned_classes.contains(class) { "" } else { weak }`。

### 为什么判据必须是「main 文件」而不是「本 TU 里有 @implementation」

自包含模式下 `#import <Foundation/Foundation.jth>` 会把 Foundation 的 `.jeti` 内联进
**每一个** TU。若「有 `@implementation` 就算拥有」，则每个 TU 都拥有 `NPString`，两边都发
强符号 → `duplicate symbol`，现有 01–08 会全部链接失败（**实测如此**）。

限定「main 文件」后：`#import` 带进来的实现不授予归属，自包含模式行为**完全不变**；
而「两个 TU 各自在 main 文件里实现同一个类」正是真正的非法用法 → 链接期大声失败。

### 实测

* `09_layout_mismatch`：从运行期 `__sig` 中止改为**链接期** `duplicate symbol`。
  挂具已支持「链接期 EXPECT_FAIL」（`tests/multi_tu/run_multi_tu.sh`），
  `EXPECT_FAIL_MATCH` 随之更新。
* 其余 multi_tu 01–08、10、11 行为不变（每个类只有一个 main 文件所有者）。
* `tests/strong_metadata` 增至 4 项，第 4 项在**不带任何标志**时验证「两个所有者必须
  duplicate symbol」。

### 仍未做（`-fstrong-metadata` 为何还不能删）

> 历史记录：本小节的两项阻塞已在 §9 更新与 §11 方案 A 落地中全部清偿，
> `-fstrong-metadata` 已删除；当前状态见 `doc/architecture.md` §6/§7。

自动判据覆盖不到**库自身**：`tools/build-foundation-lib.sh` 编译的是
`include/Foundation/Foundation.jth`，其 `@implementation` 全部来自 `#import "*.jeti"`，
按 R2 判据都算弱符号。要让库自动变强，需要「按 `.jeti` 分别编译」的构建形态（每个 `.jeti`
作为 main 文件 → 各自拥有自己的类），但该形态目前**不受支持**：

1. ~~**生成 C 自我冲突**~~ ✅ **已修复（2026-10-03，同日第二轮）**：直接编译
   `include/Foundation/NPObject.jeti` 时 `struct jeti_root` / `struct NPObject` 曾在 Section 4
   （带 `#ifndef` 的兜底定义）与类布局节各出现一次（后者无保护）。修法：Section 4 发射兜底体后
   把 tag 登记进 `header_structs`，类布局节既有的 `header_structs.contains` 跳过自然生效——
   同一生成文件每个根结构体恰好定义一次；runtime.h 在场时其定义胜出、跳过同样正确（无条件登记
   两态皆安全）。守护：`tests/strong_metadata` check 7。
2. **`jeti_metaInit` 是单个弱符号**：只有一个 TU 的副本会运行，而它只初始化「该 TU 见得到的
   类」。按 `.jeti` 分 TU 后每个 TU 只见到自己 + 依赖，其它类会漏初始化 —— 需要把类初始化改为
   **按类**（每类一个构造函数或每类一个 init 函数，全部运行）。

因此 `-fstrong-metadata` 目前是**库 TU** 的必要开关，也是「我在提供这些类」这一意图的唯一显式
表达。要删掉它，得先解决上面两点。

### 更新（2026-10-03）：独立 TU 工作流已免旗标（评审确认项关闭）

评审问「独立 TU 模式下是否要手写 `-fstrong-metadata`」。实证结论：**不用**——自动 strong
已随 R2 存在，归属判据按**主文件是否持有 `@implementation`**，与扩展名无关：

| 形态 | 命令 | 自有类元数据（无旗标，nm 实证） |
|------|------|------------------------------|
| 自包含 `.jth`（带 `@implementation`） | `jetic -rewrite-jeti main.jth` | **STRONG**（vtable + meta-vtable） |
| `.jeti` 主文件（带 `@implementation`） | `jetic -rewrite-jeti main.jeti` | **STRONG**（同上） |
| 纯声明 `.jth`（无实现进 TU） | `jetic -rewrite-jeti decl.jth` | WEAK（正确：R2 客户端空桩必须输给 owner 的表） |
| `Foundation.jth`（实现经 `#import "*.jeti"` 进来） | 库构建脚本 | WEAK——`#import` 不授 ownership，**这正是库构建仍必须带旗标的原因** |

- 守护：`tests/strong_metadata` 增至 **9 项**（新增 5a–5d：`.jth`/`.jeti` 两形态自动 strong
  正例；6：decl-only 反例），`nm` 直读符号强弱。**编译器零改动**——本节只是把既有行为
  钉进回归。
- 注意：jetic 无 `-c` 模式（`-o x.o` 产出的是可执行文件），独立 TU 的转译命令是
  `-rewrite-jeti`；评审设想的 `jetic -c main.jeti → 自动 strong` 对应的现实命令即上表第二行。
- ~~「仍未做」清单收窄为仅库侧~~ **同日再收窄**：阻塞点 1「生成 C 自我冲突」已修复（见 §9
  「仍未做」第 1 条），按 `.jeti` 分 TU 只剩 `jeti_metaInit` 弱合并漏初始化——且对 R2 归属类
  已被 §10 的静态初始化强 `NPClass` 大幅缓解（归属类的元数据加载期即完成，不再依赖 metaInit
  跑到）。Foundation 库构建仍需旗标：`#import` 不授 ownership 是语义事实，不是缺陷。

## 10. R3 落地：`__sig` 只覆盖公共段（2026-10-03）

> 状态：**已实现并全量验证通过**（2026-10-03）。
> 验证过程中暴露并修掉了三个连带缺口（见本节末尾"验证链连带修复"）；
> `tests/multi_tu/11_vtable_private_slots/` 的 `EXPECT_FAIL` / `EXPECT_FAIL_MATCH`
> 已删除，用例转为 `expected.txt` 正向比对。

### 实现内容（`crates/codegen/src/codegen.rs`）

| 触点 | 内容 |
|---|---|
| `CgUnit.vtable_sig_names: Vec<String>` | 新字段：指纹覆盖的「共享段」方法名（两处测试构造点同步） |
| `vtable_sig_segment()` | 纯函数三态段选择：有公共段信息 → **公共段**（manifest 模式下取 manifest ∩ 公共——creator 无 manifest 文件可读、按自己的公共段算，follower 读 manifest 过滤同一公共集，两者才相等）；无公共段信息 → 全量（pre-R3 行为） |
| 两处 `vtable_layout_sig(...)` 调用点 | struct 定义处与实例发射处的 sig 计算均改读 `vtable_sig_names` |
| 失败诊断文案 | 改为解释「公共段不一致」：TU-local 私有方法不参与校验 |
| 单测 +3 | 私有尾部不改段 / manifest 即段 / 无公共信息=全量（`vtable_sig_tests`） |
| `tests/multi_tu/11_vtable_private_slots/expected.txt` | `run=42`（验收输出，摘牌后生效） |

### 为什么三条规则合起来是安全的

| 保证 | 来源 |
|---|---|
| 公共方法槽位所有 TU 一致 | R1（公共段字母序在前） |
| 私有方法派发只发生在归属 TU | R2（归属 TU 强符号实例；09 双归属在链接期 duplicate symbol） |
| 两 TU 指纹相等 ⇔ 公共段一致 | R3（sig 只吃公共段） |

于是 case 11（lib 多一个私有 `privateHelper`，main 多一个自己的 `App`）在 R3 下：
两侧公共段相同 → sig 相同 → 不中止；`App` 的派发走 main 自己的强符号实例，
`privateHelper` 只在 lib 的实例里。09（两 TU 都实现 Widget）行为不变：
R2 在链接期拒绝，比 sig 更早。

### 验证结果（2026-10-03 全绿）

```text
cargo build                 0 error / 0 warning
cargo test --workspace      153/153（vtable_sig_tests 7 例全过）
run_multi_tu.sh             11/11（10、11 转 PASS；09 仍在链接期 duplicate symbol）
run_strong_metadata_test.sh 4/4
run_arc_intern_test.sh      PASS
test_all.sh -j4             347/355 passed, 0 failed, SUSPECT=0
P3+自有类端到端             rc=0（len=5 s=hello run=42；App 为客户端自有实现）
```

### 验证链连带修复（三个真缺口，R3 摘牌后才暴露）

| # | 缺口 | 修法 |
|---|------|------|
| 1 | **manifest 模式 creator/follower 指纹不一致**（case 10）：creator 先编译时 manifest 文件尚不存在 → 按"自己名字 ∩ 公共段"算；follower 读到 manifest → 按"manifest 全量"算（含 creator 的私有尾部）→ 误中止 | `vtable_sig_segment` 的 manifest 分支同样过公共段过滤——creator 与 follower 对同一公共集取交集，天然相等 |
| 2 | **败方 TU 独有类从未初始化**（case 11）：`jeti_metaInit` 弱合并只跑一个 TU 的副本，且只初始化该 TU 见得到的类 → 只存在于 main 的 `App` 的 `NPClass` 恒为零值 → `[[App alloc] init]` 解引用 NULL vtable（§9 阻塞点 2 的运行期形态） | 归属类（R2 集合或 `-fstrong-metadata`）发**完整初始化的强 `NPClass` 定义**（静态初始化，加载期完成，与构造函数顺序无关，裸机可用）；`jeti_metaInit` 本体零改动（自包含模式行为不变）；ARC dealloc wrapper 发射点相应上移到 Section 11 之前（static 定义须先于引用） |
| 3 | **仅声明 TU 的继承槽位为 NULL**（P3+自有类）：实例 vtable 对"本 TU 未发射的方法"一律填 NULL → 客户端自有类继承自库类的 `init/retain/...` 槽位全空 → 首次派发 segfault（自包含模式因 Foundation 实现总是内联而不暴露） | 槽位按 owner 分派：本 TU 已发射→引用；**继承槽位（owner 是父类）→ 一律引用 `Owner_method`**（定义在归属 TU 或预编译库，原型已存在）；仅本类协议存根（owner=本类且无实现）保持 NULL——槽位保留语义不变 |

## 11. 方案定案：Foundation 按 `.jeti` 分 TU 用 A——owner 静态初始化（2026-10-03）

> 用户拍板（"我想要更稳健的方案"）→ **方案 A**。本节是决策存档：探针起点、A/B 对比、B 的复活条件、开工路线图。

### 探针数据（路线图第 1 步的起点，2026-10-03 实测）

9 个 Foundation `.jeti` 逐个当主文件编译（`jetic -rewrite-jeti <f>.jeti -I include` + `clang -c`）：

- **9/9 独立编译通过**——NPObject（§9 修复解锁）之外，其余 8 个（NPArray / NPDictionary / NPError / NPMutableArray / NPMutableDictionary / NPMutableString / NPNumber / NPString）全部直接通过，无需逐个清理，好于预期。
- import 链（实现经 `#import "*.jeti"` 内联为**非 owner 弱副本**，R2 语义不变、链接期正确合并）：NPError → NPObject+NPString；NPMutableArray → NPArray；NPMutableDictionary → NPDictionary；NPNumber → NPObject；NPString → NPObject；NPArray / NPDictionary / NPMutableString / NPObject 无 `.jeti` import。

### A vs B（按本项目实际约束）

| 轴 | A：owner 静态初始化 | B：registration fragment + 运行时遍历 |
|---|---|---|
| 初始化时机 | 加载期完成（§10 已实证） | 需运行时遍历或构造器 |
| 裸机 `-ffreestanding` | 纯静态数据，零运行时依赖 | `.init_array` crt0 不跑，每个内核自行接线 |
| 多平台 | 普通 C 全局定义，链接器机制 | section 遍历 API 三平台三样（ELF `__start_/__stop_` / Mach-O `getsectiondata` / PE 另一套） |
| 漏注册失败模式 | **链接期响亮**（undefined / duplicate symbol） | **运行期静默**（段错误）——正是 stable-slots 工程消灭的那类错误 |
| `--gc-sections` | 存活（NPClass 被分配点引用） | fragment 无静态引用会被丢弃（需每平台 `KEEP()`） |
| "谁赢" | 链接期定死：owner 强 / 客户端弱（R2 已实证） | 注册顺序随链接序，first/last-wins 不可移植 |

决定性论据：① Jeti 元数据全是编译期常量（FNV 哈希、`sizeof`、extern 地址——case 11 跨 TU 静态初始化已实证），B 的运行时遍历**没有活干**，而"纯静态"是语言身份；② A 不是新方案——§10 的静态初始化强 `NPClass` 已落地全量验证，Foundation 分 TU 后每个 `.jeti` 恰好是自己类的 owner，A 只是把既有路径铺满。

### B 的复活条件（存档，勿轻易捡回）

满足其一才重议：① 动态类加载/插件机制；② 元数据出现运行时才能算出的字段；③ 需要 ObjC 式运行时类发现/反射。三者均违背纯静态定位，目前看不到路径。

### 开工路线图（下一阶段执行，勿重新论证）

1. ~~逐文件探针~~ ✅（本节上表，9/9）。
2. ~~库构建脚本改"逐 TU 编译 + `ar`"；**`-fstrong-metadata` 删除**~~ ✅（2026-10-03，见下方落地记录）。
3. ~~`jeti_metaInit` 退役验证~~ ✅（2026-10-03：全类静态初始化后，强空 `jeti_metaInit` 覆盖弱合并副本、程序照常运行——metaInit 只剩幂等回填，退役成立）。
4. ~~P3 端到端 + 全量回归~~ ✅（2026-10-03，见下方落地记录）。

### 落地记录 ✅（2026-10-03，本会话）——方案 A 全链完成

**库构建（wrapper TU 形态）**：`tools/build-foundation-lib.sh` 重写为逐 TU 编译——每个 Foundation `.jeti` 生成一个 wrapper（`#import <Foundation/Foundation.decl.jth>` 前置 + 原 `.jeti` 全文），`@implementation` 落在 wrapper 的主文件里 → R2 ownership 保持、元数据自动 STRONG（nm 校验 9/9 强符号）。声明面 = decl.jth 全集，与客户端一致 → `__sig` 公共段一致。库 372K（`target/foundation/libjetifoundation.a`）。

**`-fstrong-metadata` 删除**（能删就删，无过渡期）：CLI 参数、clap Arg、`DOUBLE_TO_SINGLE`、flag 表、手动 help、两处解析点、pipeline 赋值、`Pipeline.strong_metadata` 字段、`CgUnit.strong_metadata` 字段、codegen 全部 `unit.strong_metadata ||` 合取（R2 独占判定）、multi-input 转发注释与提示。旧拼写两种参数位置都响亮失败（`Unknown argument` / `cannot read`），check 3 双序断言守护。

**metaInit 退役实证**：客户端 TU 的 9 个 `NPClass` 全部走 §10 静态初始化（`__data` 段，非 BSS）；用 `.o` 位置参数直链强空 `jeti_metaInit` 覆盖弱合并副本 → 客户端全程正常（`[metaInit: strong-empty override ran]`，输出逐字节不变）——metaInit 在 R2 归属世界只剩幂等回填，仅对 `#import "*.jeti"` 的库构建路径仍有意义。

**P3 端到端**：`Foundation.decl.jth` + `-ljetifoundation` 客户端探针全绿（string/array/for-in/dict/isEqual，exit=0）。

**实施中挖出并修掉的两个真 bug（探针实证，勿重蹈）**：

1. **meta-vtable 对"声明可见但本 TU 未实现"的类方法无条件引用函数符号** → 链接 undefined（`NPError_errorWithCode_domain_` 等）。实例 vtable 在多 TU 会话已立"槽位保留、引用不发"的 EMITTED_METHODS 规则，meta-vtable 实例循环漏镜像。修法：同款 `method_is_emitted(owner, mname) || owner != 本类` 判定，own 未实现槽位 NULL（codegen.rs Meta vtable instances 循环）。decl-only 客户端 TU 现在完全不产生兄弟类的 undefined 引用。
2. **给 9 个 `.jeti` 源文件直接补 decl.jth import 是方向性错误**（已回退，`git diff` 归零）：`#import` 链让声明面泄漏进所有内联方——自包含 TU（如 trace golden 只 import NPObject）突然"看见" NPString/NPArray 的声明 → 桩 vtable 继承槽位引用 `NPArray_count` 等库符号 → 无库链接 undefined（7 个 test_all 失败）。正确形态是**wrapper TU 只存在于库构建时**：声明面扩大不污染源文件，自包含 TU 保持它要求的声明面。

**回归门槛（全部实测）**：cargo 153/0、multi_tu 12/12、strong_metadata 10/10（check 3 已改测旗标删除的响亮拒绝）、arc_intern ASan PASS、test_all **347/355, 0 failed**（6 canceled 交互式、2 个 `-F` 既有基线、SUSPECT=0）——与基线精确一致。

**残留挂账**：`jeti_metaInit` 弱合并漏初始化对 R2 归属类已被静态强 `NPClass` 缓解；`#import "*.jeti"` 的库构建路径（Foundation.jth 单 TU）仍需 metaInit 真身——已在构建脚本注释与 AGENTS 中如实声明。

> **追记（2026-10-03）——伞头交换**：用户拍板 `Foundation.decl.jth` 命名太怪，按 `.jth`/`.jeti` 约定交换：**`Foundation.jth` 现为纯声明伞头**（本文 136–152 行等处写的 `Foundation.decl.jth` 一律读作它），**`Foundation.jeti` 为自包含伞头**（原 `Foundation.jth` 的实现内联形态），`Foundation.decl.jth` 与 `tools/make-decl-headers.sh` 已删除。`build-foundation-lib.sh` 的 wrapper 前置行相应换为 `Foundation.jth`。上文历史记录保留原样，仅作对照。
