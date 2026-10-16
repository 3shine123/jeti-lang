# Jeti 源码位置、调试信息与 LSP 基础设施规划

> 状态：**阶段 1–4 已实施**（`#line` 发射、`-g` 调试构建、`-np-map` sidecar 均已落地；阶段 1 的 CST/AST 节点位置已统一为半开区间 `[start, end)`，表示方式见阶段 1）；**阶段 6 已交付诊断、大纲、跳转定义、悬停、补全（全局名字 + 接收者成员）、signature help 与增量同步**（`crates/lsp` 的 `jeti-lsp`：诊断走 `Pipeline::analyze_document`，其余请求都从同一次分析产出的符号索引回答，位置统一经共享 SourceSpan 投影到 UTF-16；VS Code 客户端已接线）；阶段 5 与阶段 6 的 rename/调试器适配等仍为规划。本文把编译诊断、生成 C 的源位置、调试信息和未来 LSP 所需的位置信息统一规划；当前实现事实以 `docs/plans/architecture.md` 为准。

## 目标

Jeti 源码中的错误、警告、断点和调试位置应能回到 `.jeti` / `.hje` 的文件、行、列。Clang/GDB/LLDB 继续使用生成 C 的 DWARF，不由 Jeti 手写 DWARF。Jeti AST/HIR 与 codegen 输出共用同一套源位置模型，避免 checker、LSP、source map、调试适配器各自发明映射。

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

- CST、AST 的表达式、语句、声明统一携带半开区间 `[start, end)`：`line` 是预处理展开行，`col` 已经是**源列**（parser 读入 token 时完成重映射），并成对保存 `end_line` / `end_col`。编译器合成的节点 end 记为 `0`，含义是“无源来源”。
- `crates/cst/src/source_map.rs` 把展开行映射到 `(文件, 行, origin)` 并给出带字节 offset 的 `SourceSpan`：`span_from_inline`（原始 lexer 位置，套用宏列修正）与 `span_from_node`（节点位置，列已是源列，**不**再修正）；`SourceFile::offset_to_lsp` / `lsp_to_offset` 负责 byte ↔ UTF-16 转换，`line_col_to_offset` / `offset_to_line_col` 负责行列 ↔ offset。
- `-np-map` sidecar 改用 serde/serde_json 序列化（磁盘 schema 不变，仍为版本化 JSON），并补上 `from_json`（拒绝未知 `version`）。
- Parser、binder、EH、defer、async、checker 等诊断已通过 pipeline 的 `translate_lines` 映射部分行号。
- codegen 已按 Jeti 节点边界发射 `#line`（阶段 2，默认开，`-fno-line-directives` 关闭）；CLI 把生成 C 通过 stdin 交给 Clang，clang 诊断指向 `.jeti`/`.hje` 源位置。
- jetic 已有 `-g` 调试构建选项（传 `-g -O0` 给 C 编译器）；`-np-map` 可输出版本化 sidecar source map。
- AST 降级会生成 ARC、EH、async、defer、pattern 等代码；单纯 `#line` 不能表达一条 Jeti 语句拆成多条 C 语句、变量位置或状态机映射。

## 设计原则

1. **位置以源码为准**：内部位置保存规范化源文件身份和半开区间 `[start, end)`；行列用于展示，字节 offset 用于精确切片和增量解析。列的单位（UTF-8 byte、Unicode scalar 或 UTF-16）须由 LSP 协议适配层明确转换，不能混用。
2. **保留多文件来源**：`#import` 展开后的节点必须继续关联最初的 `.jeti` / `.hje` 文件，不能只保存主文件名或展开行号。
3. **映射生成代码片段**：codegen 的可归属输出应携带 `SourceSpan`；纯生成辅助代码标记为 synthetic，并可关联触发它的 Jeti span。
4. **一个事实源，多种消费者**：诊断格式化、JSON source map、LSP、断点/栈帧映射都消费同一份位置模型。Clang 的 `#line` 是兼容 C 工具链的投影，不是另一份独立映射真相。
5. **不改变语言语义**：位置改造不改变解析路径、C 超集行为、vtable/owner 规则和 ARC/EH 语义。
6. **诚实处理降级**：一段 C 对应多个 Jeti 位置时，source map 可记录多个来源/生成种类；第一版 debugger 显示语句级位置，不承诺表达式级单步。

## 分阶段实施

### 阶段 1：统一 SourceFile / SourceSpan / SourceMap —— ✅ 已实施

- **表示方式（记录在此，避免后来者另立机制）**：CST/AST 节点**不内嵌已解析的 `SourceSpan`**，而是保存与 CST 同构的裸位置 `line/col + end_line/end_col`；`SourceMap::span_from_node` 是唯一解析点，在消费边界（诊断、LSP、debugger adapter）把节点位置解析成 `file + 行列 + byte offset`。好处：elaborator 与各 lowering pass 对“文件”无感知，只做位置拷贝，SourceMap 无需穿过 20+ crate；这与既有诊断渲染器“边界解析”的做法一致，也不构成第二套映射机制。
- 载体：`crates/cst/src/source_map.rs`（`SourceSpan` / `LspPosition` / `LspRange` / `SourceFile`）、`crates/cst/src/cst.rs`（`CstExpr` / `CstStmt` / `CstDecl` / `CstArm` 的 `end_*`）、`crates/lexer`（`Token` 的 `end_line` / `end_col`）、`crates/ast/src/ast.rs`（`AstExpr` / `AstStmt` / `AstDecl` / `AstArm` 的 `end_*`）。
- **起点在构造解析函数入口捕获**：parser 原先在节点字面量所在处读 `self.previous`（构造点快照，类/函数因此会落在 `@end` / `}` 上），现改为入口捕获；`parse_function_decl_or_definition` / `parse_typedef` 的起点由调用方传入。语句、声明、函数体、typedef/enum/property/class/protocol/method/namespace/using/块字面量/初始化列表均已覆盖；逗号链后续声明符、ivar、枚举成员等“按名构造”的细节节点保留名字 token 作起点。
- 降级 pass 克隆节点时按同一规则（`X.line → X.end_line`）保留原 span；新增辅助节点 end 记为 `0`（synthetic）。
- 后续修正：elaborator 曾把**函数调用的区间取成被调用者（callee）的区间**，于是实参看起来落在调用之外。现在用 CST 节点自身的 end（parser 在 `)` 之后盖的章），`callee(args)` 的区间覆盖整条调用。回归见 `crates/elaborator/tests/ast_spans.rs::a_call_extent_covers_its_arguments`。
- 回归：`crates/parser/tests/cst_spans.rs`（CST 区间）、`crates/elaborator/tests/ast_spans.rs`（AST 区间）；`-np-map` 端到端 schema 不变。

验收：包含多层 `#import` 的诊断能得到正确文件、行、列；非 ASCII 源码列位置稳定；parser 恢复和既有诊断不退化。

### 阶段 2：按源位置发射 C `#line`

- 在生成 C 的用户代码边界发出 `#line <原始行> "<转义后的路径>"`；路径必须正确转义反斜杠和引号。
- 进入 runtime shim、静态元数据、vtable 检查器等纯生成区域时切换到稳定的虚拟文件名（例如 `<jeti-generated>`）；回到用户方法或表达式时恢复原始 `.jeti` / `.hje` 位置。
- imported 方法使用其定义文件的位置；不得把所有展开代码错误映射到 main 文件。
- `#line` 发射以稳定 SourceSpan 为输入，不通过对生成 C 做文本扫描猜源位置。

验收：由 Jeti 生成的 C 编译错误和启用的 Clang warning 指向原始 Jeti 文件/行；用户可通过 `-rewrite-jeti` 检查生成 C 与 `#line` 边界。测试主文件、导入 `.jeti`、`.hje` 和 synthetic 代码。

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
  （诊断、大纲、定义跳转已落地；补全与悬停仍待做。）
- 调试器适配器消费 sidecar，把 Jeti 断点转换成可执行源码行/PC，把生成 C 栈帧和运行时 crash 位置映射回 Jeti。
- source map 同时支持展开导入源与主 TU，并允许一条 Jeti 语句映射到多段 C。
- 先支持 LLDB/GDB 的文件行断点与栈帧回映；单步、变量显示和 async await 逐步扩展。

**进展（语法/类型诊断 MVP —— 骨架已落地）**

- 新增 crate `crates/lsp`（`jeti-lsp`，lib + bin）。bin 只做 stdio 帧读写；协议状态机、诊断投影和 URI 处理都在 lib 里，可在无子进程的情况下测试。
- 诊断来源是编译器自己的前端：`Pipeline::analyze`（`crates/jetic/src/pipeline.rs`）按 CLI 同样的顺序跑
  preprocess → parse → bind → elaborate →（checked EH / `@defer` / legacy `@finally` / pattern / async desugar）→ checker，
  返回结构化 `Diagnostic`，**不做 ARC、不做 codegen**。因此 LSP 报出的就是 CLI 会报的同一条诊断，没有第二套 linter。
  与 `transpile` 共享 `Pipeline::prepare`（预处理 + 解析），前端装配只有一份。
- 位置只跨界一次：`Diagnostic::to_source_span()` → `SourceSpan::to_lsp_range(text)`，即 byte 列 → UTF-16 code unit
  的唯一投影点（§32）。诊断指向的文件决定用哪份文本做投影（导入头文件用头文件自己的列）。
- 已支持的协议子集：`initialize` / `initialized` / `shutdown` / `exit`，`textDocument/didOpen|didChange|didClose`，
  **full sync**（`textDocumentSync: 1`），服务端推送 `textDocument/publishDiagnostics`（错误=1，警告=2，note=3，`source: "jeti"`）。
- 诊断宽度：parser 诊断本来就用 token 长度给出 `end_col`；checker 的 error/warning 录制点已从单 caret 升级到
  `check_error_over` / `check_warning_over`（节点同一行且有更宽的 span 时用节点自身区间，否则退回单 caret，
  绝不编造区间）。合成节点（`end == 0`）与多行节点保持单 caret。
- 未做（后续阶段）：rename（需要项目级引用索引）、分析结果的缓存/编辑合并、workspace folders、ARC 专属诊断、
  `-Werror` 之外的设置协商（`workspace/configuration`），以及成员补全里的协议/泛型细化。

**进展（大纲 + 定义跳转 —— 已落地）**

- `Pipeline::analyze` 拆成 `analyze`（只要诊断，CLI/测试用）与 `analyze_document`（额外回传**同一个** AST 与
  `SourceMap`）。LSP 的每一次 `didOpen`/`didChange` 只跑一次前端，诊断与符号索引都从这一次运行里派生；
  `initialize` 里声明 `documentSymbolProvider` 与 `definitionProvider`。请求**不会**重新解析缓冲区。
- 新模块 `crates/lsp/src/index.rs`：把 AST 走成一棵符号树 + 名字表。
  索引的声明种类：class / category / protocol / method / property / ivar / function / 文件级 variable /
  typedef / struct·union / enum / namespace（可嵌套）。
  **不索引** forward 声明（那不是定义）、enum 成员与 protocol 的方法清单（AST 里没有它们的位置——没有位置的
  跳转目标比没有更糟）、以及局部变量。
- 位置照旧只走共享模型：每个节点 `(line,col)…(end_line,end_col)` 经 `SourceMap::span_from_node`，
  再 `SourceSpan::to_lsp_range(text)` 投到 UTF-16。**没有源位置的合成节点不进索引**（`end == 0` 意味着不能指）。
  导入是内联的，所以一个 import 了 Foundation 的文件，其 AST 也含 Foundation 的声明：这对查找是好事
  （可以从自己的代码跳进头文件），大纲按路径把它们过滤掉。
- `textDocument/definition`：先用文档文本定位光标处的符号名（消息发送里把整条 selector 当一个符号：
  `setObject:atIndex:`；声明里的关键字部分保留冒号 `run:`），再拿名字查索引，返回**全部**同名声明
  （LSP 允许一个请求多个 location，比如 selector 在声明与实现各有一处）。
  多个候选不是猜：光标落在 receiver 或实参上时返回那个普通标识符（局部变量——索引里没有，于是不跳），
  查不到就回 `null`，绝不编造目标。
  selector 的识别是**文本级**的（从光标向前匹配括号、只取括号内紧邻 `:` 的标识符段），刻意不做 AST 遍历：
  它只决定"拿哪个名字去查"，返回的区间全部来自共享 span 模型，所以误判最多造成跳不到，不会跳到编造的位置。
- URI：靶点在被编辑的文档里就原样复用客户端给的 URI（避免 macOS `/var` → `/private/var` 规范化后
  编辑器为同一文件再开一个标签）；靶点在导入的头文件里才做绝对化 + 规范化（`file_uri`），否则相对路径
  会变成打不开的 `file://relative/...`。
- 回归：`cargo test -p jeti-lsp`（18 个单测 + 真实子进程会话，覆盖 outline、跳到同文件的两处声明、
  跳到 `include/Foundation/NPString.hje`、以及 miss 返回 `null`）；`test/extension.test.js` 覆盖两个 provider
  的 VS Code 类型换算。

**进展（悬停 + 补全 —— 已落地）**

- `textDocument/hover`：光标处符号 → 索引 → markdown。内容 = 种类（class / method / protocol / …）+ 名字 +
  `detail`（`@interface:NPObject`、`- greet:` 这类），然后是**该声明的原文**（从靶点文件的行区间切出来，
  最多 10 行，所以整个类体不会糊满弹窗），最后是 `_Declared in <相对工作区根的路径>:<行>_`；
  同名多处时补一句 `N declarations`（selector 的声明 + 实现就是这种）。共享 span 模型照旧：
  靶点文件用打开缓冲区或磁盘文本做投影。
- `textDocument/completion`：光标处的名字当**前缀**（去掉尾部冒号，所以 `setOb` 与 `setObject:` 都能给出
  `setObject:atIndex:`），索引里按前缀筛（大小写敏感，与语言一致），去重（同一个类声明两处只给一条）、
  按字母排序、上限 2000 条兜底。**kind 用的是 `CompletionItemKind` 而不是 `SymbolKind`** —— 两套枚举编号不同，
  编辑器图标看的是前者，索引里显式转换（class→7、method→2…）。
- 局限（明确写在这里）：**成员补全已落地**（见下）；signature help 与 rename 还没有。

**进展（成员补全 —— 已落地）**

- 成员补全要先回答两件事：光标处的接收者是什么类型，以及光标在 **selector 位**还是**实参位**。
  纯文本答不了后者（`[a do:x |]` 与 `[a do:thing|]` 长得一样），所以索引在构建时（只对被编辑的文件）
  为每个消息发送记录一个 `SendSite`：整条 `[...]` 的范围、接收者范围、接收者名字（`VarRef`/`self`）、
  **接收者静态类型**（checker 填好的 `expr_type` —— 所以 `[[a make] |]` 这种嵌套接收者也解析得出）、
  以及每个实参的范围。光标落在 receiver 或任一实参里 → 不是 selector 位，不给成员（退回全局名字）；
  否则用接收者类型查类成员表。
- 作用域：每个方法/函数一个 `Scope`（参数、`self` 类型、所属类的 ivar、体内声明的局部变量；块级作用域被压平，
  后声明者胜）。接收者优先按**名字**查作用域（局部/`self` → 实例方法），查不到再当**类名**（→ 类方法），
  最后才用 `expr_type`。
- 类成员表覆盖**参与编译的全部文件**（局部变量的类型通常在头文件里），沿 `super_name` 向上收集，
  区分 method / property / ivar；对象接收者只给实例成员，类接收者只给类方法。
- 一个实测到的坑：类名接收者的 receiver 节点被 elaborator 重写成覆盖**整条 send** 的范围（不含接收者独有信息），
  于是"光标在 receiver 上"的判定只在 receiver 范围确实窄于 send 时才生效——否则 `[Widget make]` 永远被判成
  "光标在接收者上"，类方法一条都给不出。
- `obj.` / `obj->` 是唯一 AST 答不了的成员位（成员名没打出来时节点还不存在），这一路退化为**文本**取值：
  读光标前的标识符，再走同一套作用域解析。
- 遍历用 `BodyWalk` 显式列出 `AstStmtData` / `AstExprData` 的**全部**变体（不用 `_ =>`），
  所以 AST 新增变体会编译失败，而不是悄悄少收集；没有源位置的合成节点跳过。
- 未做：协议成员（`id<P>`）与泛型参数替换（`NPArray<T>` 的 `T` 具体化）。
**进展（项目配置文件：compile_commands.json / compile_flags.txt —— 已落地）**

- 编辑器设置解决了"编辑器要知道你怎么构建"，但每个项目都要手填一次。按 clangd 的方式让**项目自己声明**。
- `compile_commands.json`（逐文件）：取 `file` 与文档匹配的那条；没有匹配就用**目录最接近**的那条
  （clangd 的 inference；同一目录多条时取最前面那条，即 clangd 文档里的 "first entry" 回退）。
  相对 `-I` 按该条的 `directory` 解析（格式要求）；`command` 字符串按空白切分（clangd 也如此）。
- `compile_flags.txt`（整棵树）：一行一个 flag，对所有文档生效。
- 搜索从文档所在目录向上，**到工作区根为止**（`initialize` 的 `rootUri` 或 `workspaceFolders[0]`），
  避免嵌套项目误吃父项目的文件；同一目录内 `compile_commands.json` 优先。
  文档尚未存盘也能定位：`canonical()` 会规范化到最近存在的祖先再补回剩余路径
  （macOS 上 `/tmp` → `/private/tmp` 这类差异因此不会让匹配失败）。
- 只接受**改变诊断**的开关（`-I`/`--include`、`-fno-jeti-arc`、`-fno-checker`、`-eh`），其它（`-o`、`-g`…）忽略；
  `-D` 不适用——jetic 没有用户宏开关。解析用**与服务端命令行同一个** `apply_flags`，
  所以一个 flag 在命令行、配置文件、编辑器设置三处含义一致。
- 合并规则：项目文件**细化**编辑器设置——目录相加、关掉的开关保持关掉、`-eh` 以文件里的显式值为准。
- 缓存：`compile_commands.json` 可能很大而每次敲键都分析一次，所以按文件 + mtime 缓存，变了才重读；
  `compile_flags.txt` 很小，每次读。
- 证据：单测覆盖 per-file 命中 / 推断 / 向上搜索在工作区根停止 / 坏 JSON 视为不存在 / `-o` 的值不被当成目录；
  会话测试起真实二进制、只给 `rootUri`，让服务器自己找到 `compile_flags.txt` 并断言零诊断；
  `compile_commands.json` 也用真实探测验证过（MRC 文件 0 诊断）。
- `compile_flags.txt` 的 `#` 注释是 jetic 的**扩展**（clangd 的实现是"逐行 trim、跳过空行、其余整行作为一个参数"，
  没有注释语法；注释行会让 clangd 的编译命令变成 unknown argument）。本仓库根目录就放了一个**全注释**的
  `compile_flags.txt`：默认什么也不做，取消注释 `-fno-jeti-arc` 即可让整棵树按 MRC 分析（对应测试脚本的
  ARC→MRC 回退）。文件头注释写明了来龙去脉，别当垃圾删掉。
- 参数解析的一处友好化：`compile_flags.txt` 是"一行一个参数"，所以 `-I include` 会作为一个整体到达，
  取 `-I` 后面的值时 trim 掉空格（clangd 在这种情况下会失败，这里按用户显然的意图处理）。
- 不做：`.clangd`（clangd 自己的 YAML 配置，语义不同且需要 YAML 依赖）。

**进展（编辑器与构建一致：诊断开关 —— 已落地）**

- 起因：在编辑器里打开一个 MRC 风格的测试文件（`tests/tt.jeti`，`dealloc` 里手写 `[_next release]`），
  波浪线报 `explicit 'release' not allowed in ARC mode`。**这不是误报**：`jetic tests/tt.jeti -o …`
  在同样的 65:9 报同样的错，`-fno-jeti-arc` 下则完全干净。该文件之所以在测试里能过，是因为
  **MRC 重试在 `test_all.py` 里**（第 223 行依次试 ARC / MRC），`jetic` 自己不做这件事。
  也就是说：编辑器和编译器说的话一致，缺的是"编辑器不知道这个项目是 MRC 构建的"。
- 修法：`jeti-lsp` 接受 CLI 自己的诊断相关开关 —— `-fno-jeti-arc`、`-fno-checker`、`-eh checked|legacy|sjlj`
  （只接受**改变诊断**的那些；`-o`/`-g` 之类与编辑器无关，直接忽略）。这些开关经
  `AnalysisOptions` 进入每次分析的 `Pipeline`（`no_arc` / `no_checker` / `eh_checked`），
  与 `transpile` 用的是同一批字段。
- 扩展侧：`jeti.arc`（布尔，默认 true；关掉即 `-fno-jeti-arc`）与 `jeti.arguments`（字符串数组，
  用 `jetic` 的拼写传项目自己的开关）。改设置会重启服务器。
- 证据：`tests/tt.jeti` 在默认下 7 条诊断，加 `-fno-jeti-arc` 后 **0 条**；单测钉住"同一段 MRC 代码：
  默认报 ARC 错误 / `no_arc` 下干净且符号索引照常"，会话测试用真实二进制跑 `-fno-jeti-arc` 断言零诊断，
  `test/extension.test.js` 断言 `jeti.arc:false` + `jeti.arguments` 确实进入 spawn 参数。
- 边界（仍然诚实）：编辑器只报告"按这些开关编译时会报什么"；它不链接、不运行，
  也不替用户猜项目该怎么构建——构建设置由用户显式说明。

**进展（增量同步 —— 已落地）**

- `textDocumentSync: 2`（Incremental）。`didChange` 按 LSP 规定**顺序**应用每条 change：带 `range` 的做区间替换，
  不带 range 的整篇替换（编辑器整篇重写时就是这么发的）。行列换算复用 `SourceFile::lsp_to_offset`
  （0 基行 + UTF-16 列 → 字节 offset），所以 "emoji 之后的那个字符" 落在编辑器说的地方，而不是按字节数算。
- 区间不合法（行列越界、start > end）时**不接受**该改动并停止应用（继续用旧文本），绝不猜——猜出来的文本会与
  编辑器显示的内容悄悄分叉。
- 客户端不再重发整个缓冲区：`lsp-client.js` 直接转发 VS Code 的 `contentChanges`（range 已是 0 基行 + UTF-16 列，
  与 LSP 同坐标，无需转换）；事件里没有 contentChanges 时退化为整篇替换，服务器同样接受。
- **诚实说明**：增量同步省的是传输与 JSON 体积，**不减少分析开销**——每次改动仍跑一遍完整前端
  （典型文件 ~25 ms，9 KB ~30 ms）。真正的大文件杠杆是缓存（头文件文本/解析结果复用）与合并高频编辑（debounce）。
- 回归：单测覆盖插入/删除/替换/整篇替换/UTF-16 前后编辑/非法区间拒绝，以及服务器级"区间编辑后诊断跟着移动"；
  会话测试用真实二进制做"删掉 `;` 报错、再插回清零"；`test/extension.test.js` 覆盖经 VS Code 事件对象
  （带 range）的整条路径。

**进展（signature help —— 已落地）**

- `textDocument/signatureHelp`：光标所在的调用 → 签名 + 当前在填第几个参数。
  消息发送复用成员补全那套接收者解析（`SendSite` 的 `expr_type` / 作用域 / 类名），
  自由函数走 `FuncCall` 记录（`CallSite`）查函数签名表。
- 参数下标由**实参区间**算出（就是成员补全用来区分 selector/实参的那份数据）：光标在某个实参里 → 它的下标；
  都不在 → 光标之前开始的实参个数（正在输入的那个），并按签名参数个数夹取。
- 签名 label 是 LSP 侧拼出来的（方法声明的参数是 CST 节点、没有源码区间，无法从声明文本切出参数标签）：
  `- (void)greet:(NPString *)name from:(NPString *)where`、`int helper(int value, int other)`。
  `parameters[i].label` 是 label 的子串（LSP 按子串匹配），所以参数高亮不需要额外算偏移。
  类型渲染是 LSP 自己的小渲染器（checker 的 printer 是私有且只吃 AST 类型，而方法参数是 CST 类型）——
  **仅用于显示**，不影响任何诊断文本。
- 触发字符 `:`（关键字 selector 的下一段参数正好从这里开始）。
- 未做：重载/多候选的排序（现在报文里给出全部候选，`activeSignature` 固定 0）、泛型参数替换后的类型显示。
- 回归：`cargo test -p jeti-lsp` 现为 26 个单测 + 会话测试（会话里断言两段 selector 的 `activeParameter=1`、
  label 与参数标签逐字正确）；`test/extension.test.js` 覆盖 `SignatureHelp`/`SignatureInformation`/
  `ParameterInformation` 的换算。

- 回归：`cargo test -p jeti-lsp` 现为 23 个单测 + 会话测试（会话里断言 `[s UTF8S` 只给出 `UTF8String`；
  单测钉住"receiver/实参位不给成员"、"类接收者只给类方法"、"继承成员可见"、"标量接收者无成员"）；
  `test/extension.test.js` 覆盖同一路径经 VS Code provider 的换算。

**进展（编辑器客户端 —— 已接线）**

- `.vscode/extensions/jeti-lang-support` 在语法高亮之外加了 language client，分两层：
  `lsp-client.js` 只做传输与会话状态（**不 import `vscode`**，所以 node harness 能直接驱动真实
  `jeti-lsp` 二进制）；`extension.js` 只做 VS Code 胶水（设置解析、文档事件转发、
  `publishDiagnostics` → `DiagnosticCollection`、LSP severity → VS Code severity）。
- 无 npm 依赖（只用 `vscode` 与 node 内建模块），不需要 `npm install`，因此不引入 `node_modules`
  和打包工具链。将来要做补全/悬停/跳转这类请求时，再评估换成 `vscode-languageclient`。
- 设置：`jeti.serverPath`（给路径就按路径；给裸名字先在工作区里找 `target/debug|release/jeti-lsp`，
  再走 `PATH`）、`jeti.searchPaths`（→ `--include`，相对工作区根；文档自身目录永远优先）。
  改设置会重启服务器。
- 试用：`./tools/run-vscode-extension.sh [项目目录]` 起一个 Extension Development Host 窗口
  （`--extensionDevelopmentPath`，不装进用户自己的编辑器配置）；持久安装走
  `vsce package` + `code --install-extension`，`.vscodeignore` 保证测试目录不进 VSIX。
- 验证分工：`test/client.test.js`（真实子进程跑 initialize / didOpen / didChange / didClose /
  shutdown）与 `test/extension.test.js`（stub 掉 `vscode`，覆盖设置解析、事件转发、
  LSP→VS Code 诊断换算、非 `jeti` 文档被忽略）都能在 CI 跑；`package.json` 走
  `vsce package` + 装进临时 `--extensions-dir` 验证过 VS Code 会接受这个清单。
  **没有**做无头断言的是真实 VS Code 窗口里的交互——那需要人工在 dev host 里看一眼。
- 回归：`cargo test -p jeti-lsp`（含真实子进程的 stdio 会话集成测试）；CLI 侧 `cargo test --workspace`、`./test_all.sh`、
  `multi_tu` / `eh_matrix` / `eh_diff` 保持全绿（诊断文案形状不变，checker 注释宽度按 clang `~~~~^~~~~` 惯例加宽）。

## 回归矩阵

- 单文件与多 TU；主 `.jeti`、导入 `.jeti`、`.hje`；相对/绝对路径及路径含空格。
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
