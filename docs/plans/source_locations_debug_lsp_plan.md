# Jeti 源码位置、调试信息与 LSP 基础设施规划

> 状态：**阶段 1–4 已实施**（`#line` 发射、`-g` 调试构建、`-np-map` sidecar 均已落地，见各阶段标注）；阶段 5–6 仍为规划。本文把编译诊断、生成 C 的源位置、调试信息和未来 LSP 所需的位置信息统一规划；当前实现事实以 `doc/architecture.md` 为准。

## 目标

Jeti 源码中的错误、警告、断点和调试位置应能回到 `.jeti` / `.jth` 的文件、行、列。Clang/GDB/LLDB 继续使用生成 C 的 DWARF，不由 Jeti 手写 DWARF。Jeti AST/HIR 与 codegen 输出共用同一套源位置模型，避免 checker、LSP、source map、调试适配器各自发明映射。

目标体验：

```text
main.jeti:42:13  Jeti 调用表达式
    ↓ codegen/source map
main.c:120:4   生成代码
    ↓ Clang -g
DWARF: main.jeti:42
```

LSP 不必等待 native debugger：它可直接消费 parser/CST/AST 和同一 `SourceSpan`，提供语法诊断、补全和跳转。Source map 是编译器输出与调试工具之间的共同基础，不是 LSP 的启动前置条件。

## 当前基线

- CST、AST 的表达式、语句和声明主要保存预处理后 buffer 的 `line` / `col`，没有统一的文件 ID、结束位置或字节 offset。
- `crates/cst/src/source_map.rs` 以逐行表把预处理展开行映射到 `(文件, 行)`；列位置不映射，导入来源也没有稳定 source ID。
- Parser、binder、EH、defer、async、checker 等诊断已通过 pipeline 的 `translate_lines` 映射部分行号。
- codegen 已按 Jeti 节点边界发射 `#line`（阶段 2，默认开，`-fno-line-directives` 关闭）；CLI 把生成 C 通过 stdin 交给 Clang，clang 诊断指向 `.jeti`/`.jth` 源位置。
- jetic 已有 `-g` 调试构建选项（传 `-g -O0` 给 C 编译器）；`-np-map` 可输出版本化 sidecar source map。
- AST 降级会生成 ARC、EH、async、defer、pattern 等代码；单纯 `#line` 不能表达一条 Jeti 语句拆成多条 C 语句、变量位置或状态机映射。

## 设计原则

1. **位置以源码为准**：内部位置保存规范化源文件身份和半开区间 `[start, end)`；行列用于展示，字节 offset 用于精确切片和增量解析。列的单位（UTF-8 byte、Unicode scalar 或 UTF-16）须由 LSP 协议适配层明确转换，不能混用。
2. **保留多文件来源**：`#import` 展开后的节点必须继续关联最初的 `.jeti` / `.jth` 文件，不能只保存主文件名或展开行号。
3. **映射生成代码片段**：codegen 的可归属输出应携带 `SourceSpan`；纯生成辅助代码标记为 synthetic，并可关联触发它的 Jeti span。
4. **一个事实源，多种消费者**：诊断格式化、JSON source map、LSP、断点/栈帧映射都消费同一份位置模型。Clang 的 `#line` 是兼容 C 工具链的投影，不是另一份独立映射真相。
5. **不改变语言语义**：位置改造不改变解析路径、C 超集行为、vtable/owner 规则和 ARC/EH 语义。
6. **诚实处理降级**：一段 C 对应多个 Jeti 位置时，source map 可记录多个来源/生成种类；第一版 debugger 显示语句级位置，不承诺表达式级单步。

## 分阶段实施

### 阶段 1：统一 SourceFile / SourceSpan / SourceMap

- 为每个读取或导入的源文件分配稳定 `SourceId`，记录规范化路径、显示路径和源文本/行起始 offset。
- 将当前逐行 `SourceMap` 扩展成预处理映射：展开 buffer 的区间映射到原文件的区间，至少保留起止行列；对不能精确归属的 C passthrough、宏替换或合成文本标为 synthetic/unknown。
- CST 和 AST 节点逐步使用 `SourceSpan`，先覆盖表达式、语句、声明及 parser/checker 诊断触点；降级 pass 克隆节点时必须保留原 span，新增辅助节点要带 synthetic origin。
- 保持兼容访问器以减少一次性大改；禁止继续新增只存裸 `line`、却丢失文件来源的跨阶段节点。

验收：包含多层 `#import` 的诊断能得到正确文件、行、列；非 ASCII 源码列位置稳定；parser 恢复和既有诊断不退化。

### 阶段 2：按源位置发射 C `#line`

- 在生成 C 的用户代码边界发出 `#line <原始行> "<转义后的路径>"`；路径必须正确转义反斜杠和引号。
- 进入 runtime shim、静态元数据、vtable 检查器等纯生成区域时切换到稳定的虚拟文件名（例如 `<jeti-generated>`）；回到用户方法或表达式时恢复原始 `.jeti` / `.jth` 位置。
- imported 方法使用其定义文件的位置；不得把所有展开代码错误映射到 main 文件。
- `#line` 发射以稳定 SourceSpan 为输入，不通过对生成 C 做文本扫描猜源位置。

验收：由 Jeti 生成的 C 编译错误和启用的 Clang warning 指向原始 Jeti 文件/行；用户可通过 `-rewrite-jeti` 检查生成 C 与 `#line` 边界。测试主文件、导入 `.jeti`、`.jth` 和 synthetic 代码。

### 阶段 3：调试构建选项与 Clang DWARF —— ✅ 已实施

- jetic 提供 `-g`：向 C 编译器传 `-g -O0`，一致地作用于主 TU、额外 TU（multi-TU 子进程转译）和 runtime.c。
- `-g` 与优化级别分开传递（`-g` 不隐含用户 `-O`）；debug preset 固定 `-g -O0`。
- `#line` + `-g` 下 LLDB 已验证显示 `.jeti` 文件名/行号；首版聚焦语句级停靠。
- 回归：`cargo test --workspace` 与 `./test_all.sh` 全绿，golden 输出不变。

### 阶段 4：稳定 source map 文件格式 —— ✅ 已实施

- `-np-map` 在 `.jeti` 源旁落盘 `<input>.jeti.map`，版本化 JSON（`SOURCE_MAP_VERSION = 1`，schema 见 `crates/cst/src/source_map_file.rs`）。
- 内容：`version` / `generator` / `primary_source` / `generated`（path + FNV-1a 内容哈希）/ `sources`（含各源文件内容哈希）/ `mappings`（生成 C 字节区间 + 行列 → 源行，`kind` 区分 source/synthetic，含 `synthetic` 标志）。
- 接线：`main.rs`（`-np-map` 解析、落盘）→ `pipeline.ov_map` → transpile 提取 → `pipeline.last_source_map` 回取。
- `generated.path` 语义：`-rewrite-jeti` 记录真实 `.c` 路径；compile/run 模式 C 经 stdin 进编译器，记 `"<stdin>"`，由内容哈希锚定实际构建的 C 字节。
- 已知边界：sidecar 只覆盖主 TU——multi-TU 模式下额外 TU 的生成 C 是链接后即删的临时文件，`-np-map` 有意不转发给它们。
- 因为 TU 可能内联多个源文件，schema 使用 `sources[]`，而不是单一 `source` 字段。
- 每个 mapping 至少包含生成 C 起止 offset/行列、Jeti 源文件 ID 与 span、节点/生成种类、synthetic 标记及可选 origin span。
- 映射应描述 C 的实际字节范围，并提供 source content hash 或 build identity，防止 IDE 拿旧 map 对新 C。
- 格式需可增量扩展、稳定排序、路径可移植；调试器和 LSP 不依赖未版本化的内部 Rust struct 序列化。

验收：映射能把 C 行列反查到 Jeti span；多 TU、import、ARC/EH 生成段、文件改名/旧 map 场景有明确处理。

### 阶段 5：变量与生成代码调试体验

- 保留用户局部变量名；编译器临时变量采用明确前缀并避免覆盖用户名称。
- 评估 C `artificial` 属性及 Clang 支持范围，只对内部辅助函数/变量使用；不能依赖属性替代 source map。
- 对 ARC retain/release、checked EH 守卫、pattern dispatch 和 async 状态机记录 origin span 与生成类别。
- async await 的状态机位置、优化后的变量 location、表达式级 stepping 作为独立能力逐项验收。

### 阶段 6：LSP 与调试器适配

- LSP 复用 lexer/parser/checker 的 SourceSpan；先实现语法/类型诊断，再补全和定义跳转。无需等待 source-map sidecar 或 DWARF。
- 调试器适配器消费 sidecar，把 Jeti 断点转换成可执行源码行/PC，把生成 C 栈帧和运行时 crash 位置映射回 Jeti。
- source map 同时支持展开导入源与主 TU，并允许一条 Jeti 语句映射到多段 C。
- 先支持 LLDB/GDB 的文件行断点与栈帧回映；单步、变量显示和 async await 逐步扩展。

## 回归矩阵

- 单文件与多 TU；主 `.jeti`、导入 `.jeti`、`.jth`；相对/绝对路径及路径含空格。
- C compiler error 和 warning 的文件行号；`-rewrite-jeti` 输出的 `#line` 检查。
- ARC、checked EH、defer、pattern switch、async lowering 对源位置的保留。
- Clang `-g -O0` 下 LLDB/GDB 的文件定位；C 代码断点和 Jeti 断点映射。
- 现有 checker/parser 诊断、C superset 与 golden 输出语义不变。
- 后端能力要明示：`#line` 属于 C 预处理器；DWARF 首先验证 Clang。GCC/其他后端分别验收，不默认推断兼容。

## 非目标与限制

- 第一版不手写 DWARF，也不承诺优化构建中的完整变量位置。
- `#line` 能修正文件名和行号，不能单独表达一个 Jeti 表达式生成的多条 C、ARC/EH 控制流、async 状态机或断点语义。
- LSP 的语法高亮/补全/跳转/诊断可先基于 parser/AST 独立交付；source map 主要连接生成 C、原生调试信息和 debugger adapter。
- source map 不等同于 JavaScript source map 的既定格式；采用自有版本化 schema，除非未来工具链互操作证明兼容格式有益。
