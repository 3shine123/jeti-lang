# C 标准判错规划（checker 自实现 · clang 为行为 oracle）

> 状态：规划。前置 Phase 0 已完成（2026-10-18：后端 `-w` 已去除；合成 C
> 透传代码噪音已清零，代表 TU 只剩故意警告——见 §4 Phase 0 条目）。
> 关联：`docs/plans/architecture.md`（诊断/SourceSpan 框架）、
> `docs/plans/source_locations_debug_lsp_plan.md`（诊断对齐方法论——本规划沿用）。

## 1. 决策（已定）

1. **jetic 默认不吞后端报错。** 旧 `main.rs` 恒推 `-w` 已删；警告透传后，
   generated C 必须对干净 `.jeti` 达到"零警告"——合成代码警戒要在 codegen
   源头修，不许用 `-w` 掩盖。
2. **C 级语法/类型判错由 checker（前端）自实现**，依据 ISO/IEC 9899（C17，
   C23 差异单列），措辞自写、位置走 SourceSpan 单一投影。
3. **oracle 是 clang 的行为，不是它的源码。** 禁止阅读clang/gcc 源码；一切
   判罚（报不报、warning 还是 error、caret 在哪）以对
   `/usr/bin/clang` 的实测为准——与 expected 家族对齐的方法论相同：
   探针 → 测量 → 冻结措辞 → 单测 → 回归。
4. 分层只留一行：**类型级、源级可见 → checker；代码生成/链接/汇编域 →
   后端透传 error（警告若源自合成代码须清零而非透传）。**

## 2. C17 约束族清单（checker 接受范围）

标注：`[银]` = 约束违规（标准要求至少一条诊断，警告也算达标）；
`[度]` = 未定义行为但 clang 默认诊断（选择性实现）；
`[测]` = 严重度/形态需探针实测后冻结。

### G-ex 词法与预处理（6.4 / 6.10）
- 浮点常量有小数点无数字（6.4.4.2）→ error `[测]`
- 宏重定义且替换体不同（6.10.3）→ warning `[测]`
- `__VA_ARGS__` 空参逗号形态、`#` 操作数约束（6.10.3.x）`[测]`
- 未知预处理指令、`#include` 找不到文件、`#else/#elif` 配对（多数 parser 已管，补对账）

### G-decl 声明面（6.7）
- 一个 declarator 多个 storage-class（6.7）→ error `[测]`
- 类型限定符重复（`const const`）
- struct/union（6.7.2.1）：成员名重复 → error；bit-field 非整型 → `[测]`；
  union 用 bit-field → error；flexible array 非尾成员 → error
- enum（6.7.2.2）：枚举值须整型常量 → error
- 数组（6.7.6.2）：尺寸非整型/负数/void/函数/不完整类型 → error
- 函数（6.7.6.3）：返回数组/函数 → error；`void` 参数与其它参数混用 → error
- typedef 别名冲突重定义（6.7.8）→ error

### G-init 初始化（6.7.9）
- 初值类型不匹配 → error（非指针）/warning（指针族，见 G-expr）`[测]`
- char 数组 ← 过长字符串（含截断 `\0` 形态）→ `[测]`（clang 行为分两档）
- aggregate 初值过多 → excess elements，warning `[测]`
- static/外部对象初值须为常量表达式 → error（"initializer element is not
  a compile-time constant" 同义措辞）`[测]`
- 指定初始化器重复给同一成员 → warning `[测]`

### G-expr 表达式 / 语句面（6.5 / 6.8）
- 标识符未声明（6.5.1）→ error（implicit declaration 时代差异：C17 必须
  诊断，clang 默认 error，`[测]`）
  —— **已实现并双侧冻结**：`tests/negative/c_std/undecl_family.*`（2026-10-18）。
  前置修复：binder 曾把任何提及的 `Ident` 自动注册为无类型 Variable（提及
  即注册），毒化 symtab 使检查恒静默——已移除；头文件表面（`errno`、
  `stdin`、`STDIN_FILENO`…）经 `feed_included_headers` + 策展绑定表豁免，
  引号 `#include "x.h"` 在场则整个未声明函数检查关闭。`for (id x in arr)`
  循环变量由 checker ForIn 臂自行入 scope（binder 不再代劳）。
- 下标（6.5.2.1）：操作数须指针/数组 → error；非整数下标 → error
- 调用（6.5.2.2）：对象非函数指针 → error；原型已知：
  实参过少 → error `[测]`；实参过多（变参/原型）→ `[测]`
  —— **已实现并双侧冻结**：`tests/negative/c_std/call_family.*`（2026-10-18）。
  措辞三形：too few `expected N, have M`（caret 在 `)`）；too many 多参
  `expected N, have M`；too many 单参 `expected single argument 'a'`（引用
  参数名，`function_param_names` 表）。called-object caret 在 `(`。
  `(void)` 参数列表计 0 参；block 与 fn-ptr 变量、typedef 别名
  （`IntFn`/`SimpleBlock`，`block_alias_names`）合法调用；成员 fn-ptr
  `d.fn(9)` 空名 callee 跳过；stdlib 白名单（`feed_included_headers` 门）
  豁免 passthrough 头；引号 include 关闭检查。assign 值内的调用（含
  嵌套）经 `check_call_family_deep` 深遍历克隆子树覆盖；assign 值位
  caret 差 1 列（span 含 `;`）。
- 成员访问（6.5.2.3）：非 struct/union 操作数 → error；`->`/`.` 混用类型错 → error
- `++/--/`（6.5.2.4/6.5.3.1）：须可修改左值且为标量 → error（含 const 写、
  对 rvalue 自增）
- 一元（6.5.3.2/3）：`*` 须指针、`-/+/~` 须算术、`!` 须标量 → error
- sizeof（6.5.3.4）：对函数/不完整类型 → error
- cast（6.5.4）：cast 到不完整类型/对函数值 cast → error；
  整-指针往返族（对象指针↔整数，6.3.2.3/6.5.4 参照）：细分
  `-Wint-to-pointer-cast`（变量形态；字面量特判）、
  `-Wvoid-pointer-to-int-cast`、`-Wpointer-to-int-cast`，warning
  —— **已实现并双侧冻结**：`tests/negative/c_std/cast_family.*` +
  README 矩阵（2026-10-18 测量；同宽静默、函数设计符静默、
  `&`→`T *` 推导修正、async/eh 合成 cast 靠 `__jeti_` 内部引用过滤）
- `%`（6.5.5）：操作数须整型 → error；`/0`、`%0` 常量零 → `[度]`
- 移位（6.5.7）：非整型操作数 → error；负/超宽移位数 → `[度]`
- 关系（6.5.8）：不类似的指针互相比较 → warning `[测]`
- 相等（6.5.9）：指针↔整数比较 → warning `[测]`
- 位运算（6.5.10–6.5.12）：操作数须整型 → error
- `&&`/`||`（6.5.13/14）：操作数须标量 → error
- `?:`（6.5.15）：中后臂类型不兼容 → warning（同型指针例外族）`[测]`
- 赋值（6.5.16）：目标须可修改左值（const/rvalue/数组/成员 const  atte）
  → error；不兼容指针赋值 → warning（clang 默认不升 error）`[测]`；
  整↔指针赋值 → warning；struct↔标量冲突 → error `[测]`
  —— **已实现并双侧冻结**：`tests/negative/c_std/assign_family.*`
  （2026-10-18；实测量化不对称：指针↔指针不合 = **warning**，int↔指针 =
  **error**，与 clang 一致）。hint 随值形状变（`&v` → `remove &`、普通变量
  → `dereference with *` / `take the address with &`）。目标类型只信
  scope frames 真实声明（宽松回退曾把 `self = [super init]`、ivar 存
  全线误报）；子类值→父类目标 silent（probe_objc.m 测量），id/Class/SEL/
  instancetype/NPObject* 通配与 codegen `obj_wildcard_cast` 同策略；
  `void *` 双向 silent、0/nil 字面量 silent。const-赋值随 declarator
  修复转正（`int *const cp` 声明可解析可回显，`type_display` 按
  `int *const` 拼写；前导 `const int` 的 decl 级标志折叠进 scope 类型；
  顶层 const 读取 `q = cp` 按 C 静默）。
- return（6.8.6.4）：void 函数 return 带值 → error `[测]`；
  非 void 落尾无 return → clang 默认对非空函数有 `-Wreturn-type`（warning）`[测]`
- switch（6.8.4.2）：条件须整型 → error（Jeti pattern 分支豁免：
  对象 switch 是 Jeti 扩展语法，仅对 C 常量形态分支执行本约束）；
  case 值重复 → error；default 多个 → error；case 非常量 → error
  —— **已实现并双侧冻结**：`tests/negative/c_std/switch_family.*`
  （2026-10-18）。`case 1.5` 措辞 `integer constant expression must have
  integer type, not 'double'`；重复 literal case caret 在值；第二个
  default caret 在关键字；default 之后的 case legal（实测 silent）。
  解引用链主体（`switch (*p)`）经 `deref_subject_type` 折叠（宽松 Unary
  推导不解引用，曾误报）。枚举成员与字面量同值的重复（`case RED` after
  `case 1`）经 `enum_member_values` 折叠，clang 双拼写措辞
  `duplicate case value: '1' and 'RED' both equal '1'` 原样复刻。
- label（6.8.1）：goto 目标不存在（6.8.6.1 须指向同函数 label）/ 函数内重复 label → error
- break/continue 位置（6.8.6.2/6.8.6.3）：循环外 → error（parser 层）
- 逗号/条件里的 void 等类型约束随项收录

### G-tu 翻译单元（6.9）
- 链接前同一标识符类型冲突（同 TU 重声明/重定义不一致）→ error `[测]`
- 函数声明与外部定义签名冲突 → error

### 明确不进 checker（保持后端透传 or 不做）
- 内联 asm 指令体合法性（保留给目标后端，原样透传 error）
- 链接层（重复符号/未定义符号——链接器已称职）
- `[度]` 中后置候选：unsequenced（6.5 求值对面，clang `-Wunsequenced`
  非默认？→ 测后再排队）、溢出常量折叠族、别名
- clang/GNU 扩展类警告（unused、「unused-parameter」等非标准要求项）：
  后端 clang 原生已给，checker 不重造

## 3. 现状差距

`crates/checker` 今日覆盖：ARC 内存模型/空安全/throws 对账/格式串/泛型/
消息解析 —— 全部是 Jeti 语义域；C17 约束族（G-ex/G-decl/G-init/G-expr/G-tu）
中 **G-expr 五族已实现并冻结**（cast 往返、未声明标识符、调用、赋值、
switch/case —— 见 §2 各条目与 `tests/negative/c_std/README.md` 矩阵），
其余（G-init/G-tu/G-ex 及 G-expr 余量：下标/成员访问/自增/一元/sizeof/
`%`/移位/关系/相等/位运算/`&&`/`?:`/return/label/break-continue）未实现。
基础设施已备好：
`check_error/check_warning/check_error_span/record` + journal span + Severity
+ `warnings_as_diagnostics` 通道（LSP 自动可见），并已有
`is_subclass_of`/`type_display`/`AstType` 全套类型骨干可复用。
新增可复用件：`function_returns`/`function_param_names`/`function_variadic`/
`block_alias_names` 四张登记表、`feed_included_headers`（passthrough 头
豁免面）、`deref_subject_type`（解引用链折叠）、
`assign_target_type`（只信真实声明的目标解析）。

## 4. 里程碑

- **Phase 0（已完成 2026-10-18）透传转换落地**：全库（代表 TU
  `full_syntax_test.jeti`）合成 C 警告清零——clang `-fsyntax-only` 仅剩
  §1.22 故意保留的三条 int/ptr 往返警告与探针命令行假象。落地位置：
  ① ARC dealloc 合成释放点补 `(NPObject *)`（codegen）；② ARC pass
  AST 注入的 `jeti_release` 参数同样补上（arc `make_release_stmt`，
  retain/release 配对识别同步拆 Cast）；③ KVC 包装器返回值补 `(id)`
  （codegen）；④ `-Wparentheses-equality`：if/while/do 条件加
  `emit_condition`——Binary 自身括号 bug #6 保留，只去掉 if/while 外壳
  （41 条 → 0）；⑤ vtable 参数表带泛型哨兵标记 `NPObject * /*V*/`，
  `parse_vtable_param_type` 剥标记+trim，nil-safe 分派臂的每参数强制
  转换终于生效；⑥ alloc+init decl 分路：接收者上转 + 参数逐位强制 +
  NPObject*/id 声明变量上转（codegen `should_split` 臂）；⑦ 属性赋值
  目标补 ivar type_str（PropRef 臂），Assign 发射臂按
  `obj_wildcard_cast`（仅 id/NPObject* 通配 join）上转；⑧ Return 转换
  臂扩类型驱动上转（`@"…"` → `NPString *`）；⑨ plain-C 函数签名注册表
  `FUNC_PARAM_TYPES`（Function decl 转换时登记），FuncCall 参数按声明
  类型上转（`fs_take_fn(@"s")` 族）；⑩ respondsToSelector 合成帮手
  接收者上转；⑪ async `jeti_task_create` self 补 `NPObject *`
  （类方法 self 是 NPClass*）；⑫ async 语句位 await 整句包 `(void)`
  （unused-value 4→0），并修 M2/tolevel 双包裹（`is_void_cast` 去重）；
  ⑬ await 标量恢复按声明业务类型精确收窄（`printf("%d", await t:int)`
  不再 -Wformat）；⑭ EH 方法体尾护在 `if (flag==1) return 0;` 后补
  无条件零返回（clang 证不了 flag∈{0,1}；同 block-literal 修复理由；
  值类型才加，void 免噪声）；⑮ jetic 驱动尾随 `-x none` 只在后有
  文件输入时才发（eh_diff 7 个 diff 污染的根因之一是驱动命令行假象）。
  全基线回归绿：cargo test --workspace / test_all 482/493 0 失败 /
  multi_tu 16/16 / eh_matrix 18/18 / **eh_diff 8/8** / strong_metadata
  10/10 / arc_intern PASS。
  附注：后端 `-w` 恒定参数已删除（checks 露出前置条件）；
  cast 往返族已实现（见 §G-expr cast 条目的测量记录与 corpus）。
  未穷尽项（后续 TU 扩大验收时再收）：语句位裸函数参 `@""` 字面量
  进入非注册 runtime 原型的路径仍依赖 FUNC_PARAM_TYPES 登记面。
- **Phase 1 矩阵测量（已完成 2026-10-18）**：`tests/negative/c_std/`
  对照 corpus 扩到 5 族（cast / undecl / call / assign / switch，每对
  `.c`/`.jeti` 同源，正例+反例齐备），矩阵入同目录 README.md（报/不报、
  severity、caret 位、缺口显式记录）。clang oracle 探针（/tmp probe_*）：
  未声明标识符、too few/many（单参命名形）、called-object、const 赋值、
  指针互赋 warning、int↔指针 error+hint、struct←标量/异名 struct、
  switch 四形、`(void)`=0 参、void* 双向 silent、ObjC 子类上转 silent
  （probe_objc.m）、`default` 后 case legal。
- **Phase 2 首批落地（已完成 2026-10-18，G-expr 五族）**：未声明标识符、
  调用族（arity 三形 + called-object + 未声明函数）、赋值族（warning/
  error 不对称 + hint + ObjC 通配策略）、switch/case 四形、cast 往返
  （Phase 0 已含）。checker 单测 per family（lsp harness，clang 冻结
  措辞逐条断言）+ `-Werror` 贯通单测（jetic pipeline）。
  前置基建：`function_returns`/`function_param_names`/`function_variadic`
  三表、`block_alias_names`（block+fn-ptr typedef）、
  `feed_included_headers`（stdlib 函数/绑定双表豁免）、克隆求值深度闸门
  （防 clone 二次报）、binder 提及注册移除（未声明检查的前提）。
  追随修复：declarator 位 `const`/`volatile` 误读为变量名（声明曾整体
  丢失）→ star 后/前导位消费 + codegen const 回显 + checker scope 折叠；
  `case RED` 同值折叠（`enum_member_values`）；assign 值内嵌套调用深
  遍历（`check_call_family_deep`）。
  全基线回归绿：cargo test --workspace 272/0、test_all 482/493 0 失败
  （39 ARC→MRC 与 Phase 0 完全一致）、multi_tu 16/16、eh_matrix 18/18、
  eh_diff 8/8、strong_metadata 10/10、arc_intern PASS。
- **Phase 3 全量铺开**：G-init/G-tu/G-ex 余量按表逐族推进，每族一 PR。
- **Phase 4 验收基准**：全语料 clang vs jetic 双向 matrix 一致率 ≥
  12/12 同构（解析层前例），matrix.md 入 tests/negative 文档；回归全绿。

## 5. 措辞与严重度冻结规则

- 消息措辞以 C 标准术语名命名（"constraint violation" 不出现在文案，
  直接陈述事实），句式沿用 expected 家族的 clang 措辞（fetch 行为是
  实测产物，非源码拷贝）。
- severity 默认每族一个决定（存储为常量表），**默认严重度 = 与 clang 实测
  一致**（多为 warning，逐条同severity同caret位）；`-Werror` 统一升级路径
  已有（`jetic -Werror`），给 checker warnings 对其打通（2026-10-18 用户
  裁决：clang 对齐 + `-Werror`，不采用"约束违反一律 error"路线）。
- 每条消息尾部不带标记类别（不仿 `-Wxxx` 标签），类别映射存表供未来
  `-W(no)-family` 开关扩展。
- 一切 `TBD[测]` 的测量记录进 `tests/negative/c_std/matrix.md`，测量命令
  和日期留档（行为可能随 clang 版本漂移）。

## 6. 风险与护栏

- **Jeti 扩展与 C 约束冲突点**（对象 switch、@ 前缀、typed container、
  `@'c'`、算子语义）：约束检查先过「Jeti 扩展形态识别」，只在 C 形态
  分支触警报，绝不让检查改变已合法的 parsing 路径（C 超集铁律）。
- **新诊断不得改变可编译性**：全部新条目默认 warning（除标准明确必须
  error 的类别），默认线在温和区间，`-Werror` 才提升，等用户 opt-in。
- **Foundation 头文件需零诊断自洁**：`is_foundation_source` 过滤需
  迁拓到新族（与格式串家族同一条护栏）。
- 里程碑推进随时可中断：每族独立、可单独回滚，矩阵是唯一的真相记录。
