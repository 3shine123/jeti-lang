# Jeti 转译器开发 TODO

> Jeti：纯静态 Objective-C 方言、C 的超集语言，转译到 C99
> 编译器：jetic | 运行时：libjeti | 标准库：Foundation（NP-前缀）
> 核心特性：自动静态 ARC、CFG 分析、VTable 多态、完整 ObjC 语法兼容

> ⚠️ **历史文件**：本 TODO 是早期 **C 版** 计划，已与当前 **Rust 重写版** 的实现脱节
> （例如异常处理、Block、类别等早已实现）。最新状态请以 `AGENTS.md` 为准。

---

## 阶段 0：基础设施

### 0.1 项目脚手架

- [x] 创建目录结构（transpiler/、runtime/、tests/、docs/、examples/）
- [x] 编写 meson.build
- [x] 设置 Git 仓库 + .gitignore
- [x] 选择构建系统（cargo）

### 0.2 测试框架

- [ ] 设计测试目录结构（unit/、integration/、fixtures/）
- [ ] 实现测试运行器（C 或 Jeti 编写）
- [ ] 支持：编译测试、运行测试、输出对比、回归测试
- [ ] 集成到 Makefile（`make test`）

### 0.3 文档框架

- [ ] 创建 docs/ 目录结构
- [ ] 编写 README.md（项目概述、构建说明、快速开始）
- [ ] 创建 language-spec.md（语言规范占位）
- [ ] 创建 compiler-internals.md（编译器内部文档占位）
- [ ] 创建 stdlib-reference.md（标准库参考占位）

---

## 阶段 1：前端（Lexer + Parser）

### 1.1 Lexer（词法分析器）

- [x] 定义 Token 类型枚举
- [x] 实现字符流读取
- [x] 实现字符串字面量解析
- [x] 实现注释跳过（// 和 /* \*/）
- [x] 实现预处理指令识别
- [x] 实现位置信息跟踪
- [x] 编写 Lexer 单元测试（8 测试）

### 1.2 预处理器（Preprocessor）

- [x] 实现 #import / #include
- [x] 实现 #define 常量
- [x] 实现 #ifdef / #ifndef / #if / #else / #elif / #endif
- [x] 实现 #pragma
- [x] 实现 #undef
- [x] 实现头文件搜索路径（-I 选项）
- [x] 实现模块依赖图构建（检测循环依赖）
- [x] 编写预处理器单元测试（7 测试）

### 1.3 Parser（语法分析器）

- [x] 定义 CST 节点类型（cst.h, 377 行）
- [x] 实现递归下降解析器
- [x] 实现翻译单元解析
- [x] 实现 @interface（继承、协议、ivar、属性、方法）
- [x] 实现 @implementation（方法实现）
- [x] 实现 @protocol 声明
- [x] 实现 @class 前向声明
- [x] 实现 C 函数声明/定义
- [x] 实现全局变量声明
- [x] 实现 typedef 声明
- [x] 实现枚举、结构体、联合体声明
- [x] 实现方法声明解析（+/-、返回类型、选择器、参数）
- [x] 实现 @property 声明及修饰符
- [x] 实现语句（expr、compound、if、switch、while、do、for、for-in、for-init-decl、break、continue、return、goto、@try/@catch/@finally/@throw、@synchronized、@autoreleasepool）
- [x] 实现表达式（字面量、ident、self、super、消息发送、点语法、下标、括号、一元/二元/三元、赋值、逗号、Block、@selector/@encode/@protocol、字面量 @""/@[]/@{}、类型转换）
- [x] 实现错误恢复
- [x] 编写 Parser 单元测试（56 测试）

### 1.4 CST 验证与打印

- [x] 实现 CST 打印（cst_print）
- [x] 实现 CST 遍历器（Visitor 模式）
- [x] 实现 CST 结构验证（完整性检查）
- [x] 编写 CST 打印测试（8 测试）

---

## 阶段 2：语义分析（Semantic Analysis）

### 2.1 符号表（Symbol Table）

- [x] 设计符号表数据结构（支持作用域嵌套）
- [x] 实现类符号
- [x] 实现方法符号
- [x] 实现 ivar 符号
- [x] 实现属性符号
- [x] 实现协议符号
- [x] 实现变量符号
- [x] 实现类型符号
- [x] 实现选择器符号（Selector Symbol）
- [x] 实现全局符号表管理（跨文件）
- [x] 编写符号表单元测试（5 测试）

### 2.2 名称解析（Name Binding）

- [x] 实现类名解析
- [x] 实现方法名解析
- [x] 实现 ivar 解析
- [x] 实现变量解析
- [x] 实现 self/super 解析
- [x] 实现选择器解析（@selector 验证）
- [x] 实现协议名解析
- [x] 实现 typedef 解析
- [x] 处理名称冲突和隐藏规则
- [x] 编写名称解析单元测试（16 测试）

### 2.3 类型检查（Type Checking）

- [x] 定义类型系统（基础类型、指针、对象类型、Block、数组、函数）
- [ ] 协议组合类型（id<Protocol1, Protocol2>）
- [x] 类型等价判断
- [x] 类型兼容性检查
- [x] 数值提升规则
- [x] 对象类型转换检查
- [x] id 类型约束检查
- [ ] Block 类型签名检查
- [x] 方法返回类型检查
- [x] 赋值类型检查
- [x] 数组/字典字面量类型推断
- [ ] 泛型类型检查
- [x] 编写类型检查单元测试（12 测试）

### 2.4 @property 展开（Elaboration）

- [x] 实现 @synthesize 展开（ivar + getter + setter 自动生成）
- [x] 实现 @dynamic 标记
- [x] 实现属性修饰符语义（readonly/weak/assign/retain/copy/nonatomic 已解析存储）
- [ ] 实现点语法到消息发送的转换（当前直接生成 struct 成员访问）
- [ ] 编写属性展开单元测试

### 2.5 协议一致性检查

- [x] 检查类是否实现协议的所有 @required 方法（checker.c: check_class_protocols）
- [x] 检查方法签名是否匹配协议声明
- [ ] 生成协议 witness table 映射
- [x] 处理协议继承
- [ ] 编写协议检查单元测试

### 2.6 类别（Category）处理

- [ ] 解析 @interface ClassName (CategoryName)
- [ ] 合并类别方法到主类方法表
- [ ] 检测方法冲突
- [ ] 更新 vtable 布局
- [ ] 处理类别属性
- [ ] 要求所有类别在编译期已知
- [ ] 编写类别处理单元测试

---

## 阶段 3：VTable 与对象布局

### 3.1 VTable 布局计算

- [x] 设计 VTable 结构（jeti_vtable in object.h）
- [x] 实现方法索引分配算法（根类从 0、子类继承、覆盖保持相同、追加新方法）
- [x] 生成索引常量宏
- [x] 处理类方法 VTable（元类 VTable）
- [ ] 处理协议方法映射到类 VTable 索引
- [x] 生成 VTable 初始化代码（C 结构体初始化器）
- [x] 编写 VTable 布局测试（3 测试）

### 3.2 对象内存布局

- [x] 设计对象头结构（jeti_object：isa 指针）
- [x] 计算 ivar 偏移量（父类在前、子类在后）
- [x] 生成对象结构体定义（C struct）
- [ ] 生成 ivar 访问宏/内联函数
- [x] 编写对象布局测试（同 layout 测试）

### 3.3 类元数据生成

- [x] 设计类元数据结构（NPClass 含 name/superclass/instance_size/vtable）
- [x] 生成类元数据常量定义（jeti_ClassName_class 变量）
- [x] 实现 +alloc 通用逻辑（NPObject 的 +alloc 方法）
- [x] 实现 +init 方法（NPObject 的 -init 方法）
- [x] 实现 +class 方法（自动生成 jeti_ClassName_getClass C 函数）
- [x] 编写类元数据测试（integration/test_class_meta.sh，12 项检查）

### 3.4 选择器（SEL）表

- [ ] 收集所有 @selector 使用点
- [ ] 生成选择器常量池
- [ ] 实现选择器等价判断
- [ ] 编写选择器表测试

---

## 阶段 4：中间表示（AST + CFG）

### 4.1 Typed AST 设计

- [x] 定义 AST 节点基类
- [x] 实现表达式节点（ast_expr_t）
- [x] 实现语句节点（ast_stmt_t）
- [x] 实现声明节点（ast_decl_t）
- [x] 实现类型节点（ast_type_t）
- [x] 实现 AST 打印（ast_print）
- [x] 编写 AST 单元测试（5 测试）

### 4.2 CST 到 AST 转换（Elaborator）

- [x] CST → AST 转换（elaborator.c, 592 行）
- [x] 转换类声明
- [x] 转换方法实现
- [x] 转换消息发送
- [x] 转换字面量
- [x] 转换 for-in
- [x] 转换 @synchronized
- [x] 转换异常语句
- [ ] 实现 Block 表达式展开
- [x] 编写 Elaborator 单元测试（4 测试）

### 4.3 CFG 构建

- [x] 定义基本块数据结构
- [x] 定义 CFG 图结构
- [x] 实现函数级 CFG 构建
- [ ] 实现 CFG 可视化（DOT 格式输出）
- [x] 编写 CFG 构建测试（5 测试）

### 4.4 数据流分析框架

- [ ] 实现通用数据流分析框架
- [ ] 实现到达定义分析
- [ ] 实现活跃变量分析
- [ ] 实现可用表达式分析
- [ ] 编写数据流分析测试

---

## 阶段 5：静态 ARC（自动引用计数）

### 5.1 Ownership 推断

- [x] 定义 Ownership 状态枚举（OWN_UNKNOWN/RETAINED/UNRETAINED/AUTORELEASED）
- [x] 实现表达式 Ownership 推断
- [x] 实现方法返回 Ownership 推断（Apple 命名约定）
- [x] 编写 Ownership 推断测试（16 测试）

### 5.2 局部 ARC 分析

- [x] 线性扫描基本块语句
- [x] 处理隐式 self
- [x] 处理隐式 _cmd
- [x] 编写局部 ARC 测试（8 测试）

### 5.3 全局 ARC 分析

- [x] 实现合并点 Ownership 一致性检查
- [x] 实现循环中对象生存期分析
- [ ] 实现异常路径 ARC（同一函数 @throw 已处理；**跨函数 unwind 泄漏待修**）
- [x] 编写全局 ARC 测试（4 测试）

### 5.4 Retain/Release 插入

- [x] 实现插入点确定
- [x] 生成 jeti_retain() / jeti_release() 调用
- [x] 优化冗余 retain/release 对
- [x] 编写插入测试（3 测试）

### 5.5 ARC 验证

- [x] 实现引用计数平衡检查
- [x] 检测可能的内存泄漏
- [x] 检测过度释放
- [x] 检测循环引用
- [x] 生成 ARC 诊断信息
- [x] 编写 ARC 验证测试（5 测试）

---

## 阶段 6：后端（C99 CodeGen）

### 6.1 C99 AST 设计

- [x] 定义 C99 AST 节点（cg_expr_t, cg_stmt_t, cg_decl_t, cg_unit_t）
- [x] 实现 C99 AST 打印
- [x] 实现 C99 AST 生命周期管理
- [x] 编写 C99 AST 测试（7 测试）

### 6.2 Jeti AST 到 C99 AST 转换

- [x] 实现方法转换（含 self, _cmd 参数）
- [x] 实现消息发送转换（vtable 静态派发：`((struct vtable *)obj->isa->vtable)->method(args)`）
- [x] 实现字面量转换（int/float/string/bool/nil/null）
- [x] 实现表达式转换（var_ref, ivar_ref, prop_ref, binary, unary, assign, cast, call, comma, sizeof 等）
- [x] 实现类定义转换（struct 含 ivar 字段展开：类型+字段名）
- [x] 实现属性访问转换（ivar 内联 + vtable 派发 getter/setter）
- [x] 实现 Block 转换（struct + invoke 函数 + 注册/发射）
- [x] 实现异常转换（@try → label/goto 模式，@throw → goto __jeti_throw）
- [x] 编写 CodeGen 单元测试（12+10 测试）

### 6.3 头文件生成（.h → .h）

- [x] 从 .h 生成 .h 文件（通过 -H 命令行选项）
- [x] 处理头文件保护宏
- [x] 处理依赖的头文件
- [x] 编写头文件生成测试（6 测试）

### 6.4 实现文件生成（.jeti → .c）

- [x] 生成 #include 指令（收集自源码 .h 递归导入）
- [x] 生成 struct 定义（对象头：isa + retain_count，跳过 NPObject/NPClass 由 runtime.h 提供）
- [x] 生成静态常量（VTable 索引宏：`#define jeti_Class_vtable_index_method N`）
- [x] 生成 VTable struct 类型定义 + vtable 实例初始化
- [x] 生成类元数据初始化（jeti_init() 函数）
- [x] 生成方法实现函数（含 @synthesize 生成的 getter/setter）
- [x] 生成辅助函数（jeti_init 初始化）
- [x] 格式化输出

### 6.5 代码优化（生成期）

- [x] 内联简单 accessor（属性有已知 ivar 时直接 self->ivar 而非 vtable 派发）
- [x] 去虚拟化（编译期已知类型直接 vtable 派发编码，无运行时 msgSend）
- [ ] 常量折叠
- [ ] 死代码消除
- [ ] 编写优化测试

---

## 阶段 7：运行时（libjeti）

### 7.1 核心运行时

- [x] 定义 jeti_object / jeti_class / jeti_vtable 基础结构（object.h）
- [x] 定义 NPObject / NPClass 公共类型（object.h，与生成代码一致）
- [x] 实现 jeti_retain()（递增 retain_count）
- [x] 实现 jeti_release()（递减，到 0 时 free）
- [x] 实现 jeti_alloc()（calloc + 设 isa + retain_count=1）
- [x] 实现 jeti_init()（返回 self）
- [x] 实现 jeti_autorelease()
- [x] 实现 np_class_create / np_vtable_alloc / np_object_alloc（jeti_class.c）
- [x] 运行时头文件统一为 object.h（无 jeti_msgSend / sel_registerName）
- [ ] 实现 jeti_dealloc()（释放对象内存）
- [ ] 实现 jeti_copy()
- [ ] 实现 jeti_hash() / jeti_isEqual()
- [ ] 实现 jeti_description()
- [ ] 编写运行时核心测试

### 7.2 Block 运行时支持

- [ ] 定义 NPConcreteStackBlock / NPConcreteGlobalBlock / NPConcreteMallocBlock
- [ ] 实现 jeti_Block_copy() / jeti_Block_release()
- [ ] 实现 Block 的 retain/release 语义
- [ ] 编写 Block 运行时测试

### 7.3 弱引用支持

- [ ] 设计弱引用表
- [ ] 实现 jeti_storeWeak / jeti_loadWeak / jeti_destroyWeak
- [ ] 在 jeti_release() 到 0 时自动置零所有弱引用
- [ ] 实现弱引用表线程安全
- [ ] 编写弱引用测试

### 7.4 自动释放池

- [x] 设计 jeti_autoreleasepool 结构
- [x] 实现 jeti_autoreleasepool_push() / pop()
- [x] 实现 jeti_autorelease()
- [x] 处理线程局部存储（__thread）
- [x] 编写自动释放池测试（lang-test/golden/05_autoreleasepool/ 4 个 .jeti 文件）

### 7.5 异常支持（可选）✅ 已实现（Rust 版）

- [x] 基于 setjmp/longjmp 的异常机制
- [x] ~~实现 jeti_try / jeti_catch / jeti_finally 宏~~（Rust 版直接把 `@try/@catch/@finally` 降级为 setjmp/longjmp，无宏）
- [x] 实现异常对象传递
- [ ] 处理异常路径的 ARC（同一函数内 `@throw` 会在重抛前释放局部对象；**跨函数 unwind 仍泄漏**，待修）
- [x] 编写异常测试

### 7.6 线程支持

- [ ] 实现 jeti_thread_create() / join()
- [ ] 实现线程局部存储
- [ ] 实现原子操作封装
- [ ] 编写线程测试

---

## 阶段 8：Foundation 标准库

### 8.1 核心类（Tier 1）

- [x] 实现 NPObject（根类）
- [x] 实现 NPString
- [x] 实现 NPMutableString
- [x] 实现 NPArray
- [x] 实现 NPMutableArray
- [ ] 实现 NPDictionary
- [ ] 实现 NPMutableDictionary
- [ ] 实现 NPSet
- [ ] 实现 NPMutableSet
- [ ] 实现 NPData
- [ ] 实现 NPMutableData
- [ ] 实现 NPNumber
- [ ] 实现 NPValue
- [ ] 实现 NPEnumerator
- [ ] 实现 NPFastEnumeration 协议
- [ ] 编写核心类测试

### 8.2 基础功能（Tier 2）

- [ ] 实现 NPDate / NPCalendar
- [ ] 实现 NPURL
- [ ] 实现 NPStream / NPInfutStream / NPOutputStream
- [ ] 实现 NPFileManager
- [ ] 实现 NPJSONSerialization
- [ ] 实现 NPPropertyList
- [ ] 实现 NPCoder / NPKeyedArchiver / NPKeyedUnarchiver
- [ ] 实现 NPUUID
- [ ] 实现 NPLocale
- [ ] 实现 NPBundle
- [ ] 实现 NPProcessInfo
- [ ] 实现 NPUserDefaults（简化版）
- [ ] 编写基础功能测试

### 8.3 并发与通知（Tier 3）

- [ ] 实现 NPThread
- [ ] 实现 NPLock / NPRecursiveLock / NPCondition
- [ ] 实现 NPOperation / NPOperationQueue（简化）
- [ ] 实现 NPNotification / NPNotificationCenter
- [ ] 实现 NPTimer
- [ ] 实现 NPRunLoop（简化版）
- [ ] 编写并发测试

### 8.4 高级集合（Tier 4）

- [ ] 实现 NPPointerArray
- [ ] 实现 NPHashTable
- [ ] 实现 NPMapTable
- [ ] 实现 NPIndexSet / NPMutableIndexSet
- [ ] 实现 NPCharacterSet / NPMutableCharacterSet
- [ ] 实现 NPRegularExpression
- [ ] 实现 NPAttributedString / NPMutableAttributedString
- [ ] 实现 NPPredicate
- [ ] 实现 NPCache
- [ ] 编写高级集合测试

### 8.5 网络（Tier 5，可选）

- [ ] 实现 NPURLSession（简化版）
- [ ] 实现 NPURLRequest / NPURLResponse
- [ ] 编写网络测试

---

## 阶段 9：工具链与集成

### 9.1 编译器驱动（Driver）

- [ ] 实现命令行参数解析
- [ ] 实现编译流程编排
- [ ] 实现错误报告格式化
- [ ] 实现编译缓存
- [ ] 编写驱动测试

### 9.2 包管理器（可选）

- [ ] ~~设计包描述格式~~
- [ ] ~~实现依赖解析~~
- [ ] ~~实现包下载/安装~~
- [ ] ~~实现版本管理~~
- [ ] ~~编写包管理器测试~~

### 9.3 IDE 支持（可选）

- [ ] 实现 LSP 基础
- [ ] 编写 VSCode 插件
- [ ] 编写 Vim/Neovim 插件

### 9.4 调试支持

- [ ] ~~生成调试信息~~
- [ ] ~~映射 Jeti 源码行到 C 源码行~~
- [ ] ~~支持 GDB/LLDB 调试~~
- [ ] ~~实现 jeti-gdb 包装脚本~~

---

## 阶段 10：编译器自举

### 10.1 用 Jeti 重写前端

- [ ] 用 Jeti 实现 Lexer
- [ ] 用 Jeti 实现 Parser
- [ ] 用 Jeti 实现 CST
- [ ] 用 Jeti 实现符号表
- [ ] 用 Jeti 实现类型检查器
- [ ] 用 Jeti 实现 Elaborator
- [ ] 用 C 编写 Jeti 运行时（保持）

### 10.2 自举验证

- [ ] 用 C-jetic 编译 Jeti-jetic
- [ ] 得到 Jeti-jetic 可执行文件
- [ ] 用 Jeti-jetic 编译自身
- [ ] 比较两次输出的一致性
- [ ] 修复不一致问题
- [ ] 实现自举后的持续集成

### 10.3 性能优化

- [ ] 分析 Jeti-jetic 性能瓶颈
- [ ] 优化 AST 内存布局
- [ ] 优化符号表查找
- [ ] 优化字符串处理
- [ ] 实现并行编译

---

## 附录：文件命名规范

| 类型       | 扩展名        | 示例                                |
| -------- | ---------- | --------------------------------- |
| Jeti 头文件 | .h        | `Foundation.jth`, `NPString.jth`    |
| Jeti 源文件 | .jeti        | `main.jeti`, `NPPerson.jeti`          |
| C 头文件    | .h         | `jetiruntime.h`, `NPPerson.h`（生成） |
| C 源文件    | .c         | `main.c`, `NPPerson.c`（生成）        |
| 对象文件     | .o         | `main.o`                          |
| 可执行文件    | 无          | `myapp`                           |
| 静态库      | .a         | `libjeti.a`                       |
| 动态库      | .so/.dylib | `libjeti.so`                      |

## 附录：命名前缀规范

| 范畴             | 前缀                  | 示例                                              |
| -------------- | ------------------- | ----------------------------------------------- |
| 标准库类           | NP                  | `NPObject`, `NPString`, `NPArray`               |
| 运行时函数          | jeti_               | `jeti_retain()`, `jeti_release()`               |
| 运行时类型          | jeti_               | `jeti_object`, `jeti_class`                     |
| 编译器生成结构        | jeti_               | `jeti_NPString`, `jeti_NPString_vtable`         |
| 编译器生成函数        | jeti_ClassName_     | `jeti_NPString_length()`                        |
| 编译器生成常量        | jeti_               | `jeti_NPString_class`, `jeti_sel_initWithName_` |
| Block 内部结构     | __jeti_block_       | `__jeti_block_adder_0`                          |
| Block byref 结构 | __jeti_block_byref_ | `__jeti_block_byref_counter`                    |
| 内部临时变量         | __jeti_             | `__jeti_try_buf`, `__jeti_state`                |

## 附录：测试统计

| 测试套件                 | 测试数量    | 状态       |
| -------------------- | -------:| -------- |
| jeti_lexer           | 14      | ✅        |
| jeti_parser          | 6       | ✅        |
| jeti_preprocessor    | 1       | ✅        |
| **总计**               | **21**  | **全部通过** |

## 附录：完成进度概览

| 阶段                 | 完成度  | 备注                                                               |
| ------------------ | ----:| ---------------------------------------------------------------- |
| 0.1 脚手架            | 100% |                                                                  |
| 0.2 测试框架           | 50%  | 使用 test_all.sh + meson test，无专用运行器                               |
| 0.3 文档框架           | 0%   |                                                                  |
| 1.1 Lexer          | 90%  | 全部关键字/操作符已在 token.h 覆盖                                           |
| 1.2 Preprocessor   | 100% | -I 选项 + 循环依赖检测已完成                                                |
| 1.3 Parser         | 100% | typedef/struct/union/enum + 变量声明已完成                              |
| 1.4 CST            | 100% | Visitor + 验证已完成，7 测试                                             |
| 2.1 符号表            | 100% | 选择器符号 + 跨文件管理已完成                                                 |
| 2.2 名称解析           | 100% | @selector + typedef + 冲突处理已完成，16 测试                              |
| 2.3 类型检查           | 88%  | 缺泛型                                                              |
| 协议组合               | 100% | id⟨Proto⟩ 解析+绑定+检查+消息分发全流程 ✅                                     |
| 方法参数作用域            | 100% | 方法参数现在声明为变量再绑体 ✅                                                 |
| 2.4 @property 展开   | 50%  | 基本展开已完成（elaborator）                                              |
| 2.5 协议检查           | 60%  | 编译期协议一致性检查 done                                                  |
| 2.6 类别             | 100% | 3 测试全通过                                                          |
| 3.1 VTable         | 95%  | 元类 VTable 已实现，缺协议 VTable 映射                                      |
| 3.2 对象布局           | 80%  | 布局计算 + C struct 生成 done，缺 ivar 字段展开                              |
| 3.3 类元数据           | 100% | +class 自动生成 + vtable 隔离 + 12 项集成测试                               |
| 3.4 选择器表           | 0%   |                                                                  |
| 4.1 Typed AST      | 95%  | 全部节点完备，5 测试                                                      |
| 4.2 Elaborator     | 90%  | 全部 CST→AST 转换 done，4 测试                                          |
| 4.3 CFG            | 90%  | 全部流程节点覆盖，5 测试                                                    |
| 4.4 数据流分析          | 0%   |                                                                  |
| 5.1 Ownership 推断   | 100% | 16 测试                                                            |
| 5.2 局部 ARC         | 100% | 8 测试                                                             |
| 5.3 全局 ARC         | 80%  | 缺异常路径 ARC，4 测试                                                   |
| 5.4 Retain/Release | 100% | 3 测试                                                             |
| 5.5 ARC 验证         | 100% | 5 测试                                                             |
| 6.1 C99 AST        | 100% | 7 测试                                                             |
| 6.2 AST→C99 转换     | 100% | 全部节点转换完成                                                         |
| 6.3 头文件生成          | 100% | 6 测试                                                             |
| 6.4 实现文件生成         | 95%  | typedef/struct/union/enum 生成 + struct/方法/VTable/元数据/init 全部 done |
| 6.5 代码优化           | 30%  | 内联 accessor + 去虚拟化 done                                          |
| 7.1 核心运行时          | 90%  | retain/release/alloc/init/class/autorelease 全部实现                 |

---

## **Objective-C NS 库头文件与实现总览**

1. 头文件位置
   所有 NS 头文件都在 Foundation 框架的 Headers 目录中：
   /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks/Foundation.framework/Headers/
   共 174 个头文件，涵盖约 386 个 NS 类*。
2. 核心类 -> 头文件映射
   类    头文件
   NSString    NSString.h
   NSArray / NSMutableArray    NSArray.h
   NSDictionary / NSMutableDictionary    NSDictionary.h
   NSNumber / NSValue    NSValue.h
   NSData / NSMutableData    NSData.h
   NSURL    NSURL.h
   NSError    NSError.h
   NSSet / NSMutableSet    NSSet.h
   NSDate    NSDate.h
   NSBundle    NSBundle.h
   NSProcessInfo    NSProcessInfo.h
   NSFileManager    NSFileManager.h
   NSUserDefaults    NSUserDefaults.h
   NSNotificationCenter    NSNotification.h
   NSThread    NSThread.h
   NSRunLoop    NSRunLoop.h
   NSTimer    NSTimer.h
   NSOperation / NSOperationQueue    NSOperation.h
   NSLock / NSCondition    NSLock.h
   NSPipe    NSFileHandle.h
   NSFileHandle    NSFileHandle.h
   NSStream / NSInfutStream / NSOutputStream    NSStream.h
   NSJSONSerialization    NSJSONSerialization.h
   NSXMLParser    NSXMLParser.h
   NSRegularExpression    NSRegularExpression.h
   NSPredicate / NSExpression    NSPredicate.h / NSExpression.h
   NSCoder / NSKeyedArchiver    NSCoder.h / NSKeyedArchiver.h
   NSCache    NSCache.h
   NSCalendar / NSDateComponents    NSCalendar.h
   NSLocale    NSLocale.h
   NSTimeZone    NSTimeZone.h
   NSNull    NSNull.h
   NSURLSession    NSURLSession.h
   NSXPCConnection    NSXPCConnection.h
   NSDecimalNumber    NSDecimalNumber.h
   NSFormatter / NSDateFormatter    NSFormatter.h / NSDateFormatter.h
   NSMapTable / NSHashTable / NSPointerArray    NSMapTable.h / NSHashTable.h / NSPointerArray.h
   NSIndexSet    NSIndexSet.h
   NSOrderedSet    NSOrderedSet.h
   NSProxy    NSProxy.h
3. 常用函数 -> 头文件映射
   函数    头文件
   NSLog / NSLogv    NSObjCRuntime.h
   NSStringFromSelector / NSSelectorFromString    NSObjCRuntime.h
   NSStringFromClass / NSClassFromString    NSObjCRuntime.h / NSBundle.h
   NSHomeDirectory / NSTemporaryDirectory    NSPathUtilities.h
   NSSearchPathForDirectoriesInDomains    NSPathUtilities.h
   NSPageSize / NSRoundUpToMultipleOfPageSize    NSZone.h
4. 实现文件位置
   macOS 上所有 NS 类的实现都在系统 dyld 共享缓存中，不以独立二进制文件形式存在：
   
   # SDK 中只有链接存根
   
   /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks/Foundation.framework/Foundation.tbd
   
   ##### 实际二进制在共享缓存中 (SIP 保护，不可直接读取)
   
   /System/Library/Frameworks/Foundation.framework/Versions/C/Foundation  → 指向共享缓存
   可以通过 dyld_info 工具访问共享缓存中的符号：
   
   ##### 查看 Foundation 中所有 NS* 类
   
   dyld_info -exports /System/Library/Frameworks/Foundation.framework/Versions/Current/Foundation | grep "_OBJC_CLASS_\$_NS" | wc -l
   
   ##### → 386 个 NS* 类
   
   ##### 反汇编某个函数
   
   dyld_info -exports /System/Library/Frameworks/Foundation.framework/Versions/Current/Foundation | grep "_NSLog"
5. 其他包含 NS* 的框架
   框架    包含的 NS* 类
   CoreFoundation    CFString (NSString 底层), CFArray, CFDictionary 等
   AppKit    NSView, NSWindow, NSImage, sNSFont 等 UI 类
   CoreData    NSManagedObject, NSFetchRequest, NSPersistentContainer 等
   Security    SecCertificate, SecKey 等安全相关
   总结：Foundation 框架是所有 NS 类的"家"，头文件在 SDK 的 Headers 目录，实现全部打包在 dyld 共享缓存中，不可单独提取。

---

*最后更新：2026-07-09*
*维护者：3shine123*
*当前状态：21/38 个 golden test 通过，11 个测试套件全部通过*