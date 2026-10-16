[-> English](README.md)

<div align="center">
<img src="doc/assets/logo/jeti_border_gray_on_amber.svg" alt="Jeti_avatar" width="210">

# Jeti 编程语言

[**查看项目示例**](#项目示例)

[概述](#概述) · [为什么要创造出 Jeti？](#为什么要创造出-jeti) · [项目示例](#项目示例) · [快速开始](#快速开始) · [语言特性](#语言特性) · [新特性](#新特性) · [编译与运行](#编译与运行) · [代码示例](#代码示例) · [设计原则](#设计原则) · [路线图](#路线图) · [FAQ](#faq)

</div>

---

> ⚠️注意
> **GitHub 网页上的发行版（Release）一般比 push 上来的源码旧。** 仓库里展示的
> Release 常常落后于 `main` 分支的代码——我经常 push 完新提交就忘了发新版本。
> 想要最新特性和修复，请 **clone 仓库自行从源码构建**（见 [构建](#构建)）；
> 只有当你更愿意用较旧但稳定的快照时，才下载 Release。

---

## **概述**

Jeti 是一门**纯静态**的 Objective-C 方言（C 超集语言）。Jeti 源码被转译为 C99，再由 Clang 编译为原生机器码。没有运行时消息转发，没有 GC 暂停，没有 JIT 预热——所有方法派发、内存管理、多态都在编译期完成。目前能跑，有小游戏和工具在里面跑着。如果你觉得有意思，可以拿来试试。

我不是想替代 ObjC 或 Swift，只是单纯怀念 ObjC 的语法，想在静态编译的世界里让它再活一次。☺️

---

## 为什么要创造出 Jeti？

我喜欢 ObjC 的消息发送语法 `[obj message]`，但 ObjC 的运行时（`objc_msgSend`）太重。Jeti 保留这套语法，把它静态编译成干净的 C。

Jeti 还是一门年轻的语言，尚不能用于生产环境。它想回答的问题："如果把 ObjC 转译到静态 C，会是什么样子"。

### 它做了什么

- 方法调用 → 编译期算好 VTable 偏移量，没有 objc_msgSend
- 内存管理 → CFG 静态分析，编译期决定在哪 retain/release
- 生成 C99 → 人能读懂的 C，不是 IR

### 设计目标

- **C 兼容**：C 超集，合法的 C 就是合法的 Jeti
- **可读**：生成的 C 代码是给人看的
- **轻量**：只有一个静态的迷你运行时

### 为什么保留 `NP-` 前缀

这门语言改过名，但 Foundation 类一直保留 `NP-` 前缀（`NPObject`、`NPString`、`NPArray`……）。它来自 **"Nupa"**——项目成为 Jeti 之前的最初工作名。

保留它是刻意的：

- **看得见的历史**——每个 `NP` 类都带着项目起点的名字，即使语言名几经更换。
- **与 `NS`/`CF` 同一角色**——ObjC 用两字母前缀标记框架归属（`NSObject`、`NSString`）；`NP` 在 Jeti 的 Foundation 里扮演的正是这个角色。完整命名规则见 `NAMING.md`。
- **稳定优先于折腾**——改类名会破坏所有现存 `.jeti` 文件、桥接头（`jeti_NPString_UTF8String`）和元数据符号（`JETI_CLASS_$_NPString`），却换不来任何语义收益。前缀与语言名无关，所以语言改名不影响它。

所以：语言叫 Jeti，编译器叫 `jetic`，但类永远是 `NP*`——Nupa 留在标准库里的指纹。

---

## 项目示例

[![examples](https://img.shields.io/badge/examples-000?style=for-the-badge)](examples/)

`examples/` 

| 项目                    | 说明                                                                      | 运行                            |
| --------------------- | ----------------------------------------------------------------------- | ----------------------------- |
| **`04_soma-kernel/`** | 很小的 32 位 i386 操作系统内核（NASM + C + Jeti），裸机 `-ffreestanding` 模式                 | `./run.sh` 或 `./run.sh --gui` |
| **`03_LibUI/`**       | 基于 [libui-ng](https://github.com/libui-ng/libui-ng) 的 GUI 应用，全部回调纯 Jeti | `./run_libui.sh`              |
| **`02_ncurses/`**     | 终端示例（`ncurses_demo`、`sysmon`），使用 `Terminal::Ncurses` 绑定                 | `make run`                    |
| **`01_JSONEditor/`**  | 多文件 JSON 编辑器，分屏终端预览                                                     | `jetic run json_editor.jeti`    |

---

## 快速开始

### 依赖

- Clang（>= 14）
- Rust（stable，含 cargo）
- Git

### 构建

```bash
git clone https://github.com/3shine123/jeti-lang.git
cd jeti-lang
cargo build --release
```

### 安装

构建完成后，`jetic` 同目录下会自动生成 `install.sh`（以及头文件和 `libjeti.a`）。直接运行它即可安装到系统：

```bash
# 源码编译后——脚本就在二进制旁边
cd target/release        # 或 target/debug（如果你跑的是 cargo build）
./install.sh             # 默认安装到 /opt/jeti
./install.sh /usr/local  # 可选：换成其他前缀
```

脚本会安装：

- **二进制** → `<prefix>/bin/jetic`
- **静态库** → `<prefix>/lib/libjeti.a`
- **头文件** → `<prefix>/include/`
- **系统头文件** → `/usr/local/include/{Foundation,jeti}/`（需写权限；无权限时自动跳过，可用 sudo 重试，或传第二个参数指定目录，如 `./install.sh /opt/jeti ~/include`）

安装脚本会自动检测系统语言（中文 / English）。

或者下载预编译的 Release 压缩包（`jeti-<platform>.tar.gz` 或 `.zip`），解压后运行里面的 `install.sh`：

```bash
tar xzf jeti-x86_64-unknown-linux-musl.tar.gz
cd jeti-x86_64-unknown-linux-musl
./install.sh
```

> **提示：** 把 `jetic` 加入 PATH 并装好系统头文件后，`<jeti/runtime.h>` 和 `<Foundation/...>` 会自动被找到，无需手动加 `-I include`。

### 编译一个 Jeti 程序

```bash
# 只输出 C 代码（自动推导 .jeti → .c）
jetic -rewrite-jeti hello.jeti
jetic hello.jeti -rewrite-jeti               # flag 放哪都行
jetic -rewrite-jeti hello.jeti -o out.c      # 也可以显式指定路径
# （双横线 --rewrite-jeti 形式同样接受）

# 单独用 Clang 编译转译后 C 代码（两种方式）：
#   1) 直接编译运行时源码
clang -I include -o hello hello.c include/jeti/runtime.c
#   2) 链接编译好的 libjeti.a（位于 jetic 二进制同目录）
clang -I include -o hello hello.c -Ltarget/release -ljeti

# 编译到可执行文件
jetic hello.jeti -o hello_bin                # 转译 + 编译 + 链接

# 多 TU：额外位置参数作为附加编译单元一起编译链接；.o/.a 原样链接
jetic main.jeti lib.jeti -I include -o app     # 两条编译单元一条命令（无需手动 clang）
jetic main.jeti lib.o libfoo.a -o app        # jeti 源码与预编译目标混用

# 预编译 Foundation 库：构建一次，处处链接
./tools/build-foundation-lib.sh                            # → target/foundation/libjetifoundation.a
jetic app.jeti -I include -L target/foundation -ljetifoundation -o app   # 显式链接
jetic app.jeti -o app                                        # 或：库可找到时自动链接（纯声明客户端）

# 编译 + 运行
jetic run hello.jeti
jetic run hello.jeti -o hello_bin            # 运行后保留二进制
jetic run hello.jeti                          # 运行后自动清理临时文件

# 显示编译警告
jetic -v run hello.jeti

# [!] 错误：不用 -rewrite-jeti 却输出 .c
jetic hello.jeti -o hello.c   → Error: use -rewrite-jeti to output C code

# [!] 错误：没有指定任何输出方式
jetic hello.jeti              → Error: specify -o or -rewrite-jeti
```

### Foundation：两种使用模式

Foundation 支持两种模式，都完整可用；**真实工程推荐预编译库模式**。

**self-contained / unity 模式**——实现经 `#import` 内联，无需任何库。适合单文件、快速试验和兼容旧构建方式：

```jeti
// hello.jeti
#import <Foundation/Foundation.jeti>   // 声明 + 实现全部内联

int main() {
    NPLog(@"hello %@", [NPString stringWithUTF8String:"world"]);
    return 0;
}
```

```bash
jetic run hello.jeti
```

**预编译 Foundation / multi-TU 模式（推荐）**——实现存进一次构建的静态库；你的 TU 只编译自己的代码：

```jeti
// app.jeti
#import <Foundation/Foundation.hje>   // 纯声明——不内联任何东西

int main() {
    NPLog(@"hello %@", [NPString stringWithUTF8String:"world"]);
    return 0;
}
```

```bash
./tools/build-foundation-lib.sh   # 一次 → target/foundation/libjetifoundation.a
jetic app.jeti -o app               # 库被自动找到并链接
```

底层区别：self-contained 模式下内联实现不是其 TU 的主文件，类元数据是弱符号（每个 TU 重复一份再合并）；库模式下每个 Foundation `.jeti` 作为独立 TU 编译，其 `@implementation` 持有元数据并发射为强符号——归档里只存一份。owner/strong/weak 完整规则见 `docs/plans/architecture.md`。

### Shell 补全（Tab 自动补全）

`jetic` 自带用 [clap_complete](https://crates.io/crates/clap_complete) 生成的 **zsh / bash / fish** 补全脚本。随时可用以下命令重新生成：

```bash
jetic -gen-completions zsh > _jetic
jetic -gen-completions bash > jetic.bash
jetic -gen-completions fish > jetic.fish
```

`install.sh` 也会把脚本装进安装包（`share/jetic/completions/`）。

**zsh** —— 把目录加进 `fpath`（必须在 `compinit` 之前）：

```zsh
fpath=(/opt/jeti/share/jetic/completions $fpath)
autoload -U compinit && compinit
```

**bash**：

```bash
source /opt/jeti/share/jetic/completions/jetic.bash
```

**fish**：

```fish
source /opt/jeti/share/jetic/completions/jetic.fish
```

装完新版本后清一下 zsh 缓存：`rm -f ~/.zcompdump*`，再开新终端。

### 运行测试

```bash
# 运行所有测试
./test_all.sh -j4

# 运行 Rust 单元测试
cargo test --workspace
```

---

### 编辑器支持（LSP）

`jeti-lsp` 是一个 stdio 语言服务器：为正在编辑的缓冲区报出**语法、类型与 async 诊断**，另外提供**大纲**、
**跳转定义**、**悬停**与**补全**。它跑的是编译器自己的前端（`Pipeline::analyze_document`），因此诊断与 CLI
完全一致——不是另一套会说谎的 linter；位置只跨协议边界一次，走共享的 `SourceSpan` 模型
（1 基 byte 列 → LSP UTF-16 code unit）。

```bash
cargo build -p jeti-lsp           # → target/debug/jeti-lsp
jeti-lsp --include include        # 在 stdin/stdout 上说 LSP
```

每次编辑只跑一次分析：诊断推给客户端，大纲 / 跳转 / 悬停 / 补全用的符号索引都来自**同一棵 AST**——
请求不会重新解析缓冲区。跳转定义先认出光标下的名字（`setObject:atIndex:` 这样的整条 selector 算一个名字），
再返回它的全部声明，包括导入头文件里的，所以在自己代码里点 `NPString` 会跳进 `Foundation/NPString.hje`；
悬停显示该声明的原文；补全给出索引里的全部名字（类、方法 selector、函数、typedef、enum、全局变量），
以及**接收者的成员**：在 `[str ` 之后给你 `NPString` 自己的方法与继承来的方法，`[Widget ` 给该类方法，
`obj.` 同样可用；实参位置不会被当成 selector 位置（由 AST 判断，不是靠文本猜）。
signature help 显示光标所在调用的签名并高亮正在填的参数（`: ` 触发）。

当前范围：生命周期握手、增量文档同步（`didOpen`/`didChange`/`didClose` → `publishDiagnostics`）、
`textDocument/documentSymbol`、`textDocument/definition`、`textDocument/hover`、`textDocument/completion`
（全局名字 + 接收者成员；`id<P>` 的协议成员与泛型参数替换还没做）、`textDocument/signatureHelp`，
文档同步是**增量**的（客户端只发改动的区间，不重发整个缓冲区）。rename、ARC 专属诊断尚未实现。

### VS Code

仓库自带的扩展（`.vscode/extensions/jeti-lang-support`）除语法高亮外，还包含了 language client：
它会拉起 `jeti-lsp`，把诊断渲染成编辑器里的波浪线，填充大纲视图与面包屑，支持跳转定义、悬停（显示声明原文）、
全局名字补全与接收者成员补全，以及调用签名提示。用一个临时窗口试（不会装进你自己的编辑器配置）：

```bash
./tools/run-vscode-extension.sh                 # 本仓库
./tools/run-vscode-extension.sh ~/my/project    # 任意 Jeti 项目
```

扩展优先在工作区里找 `target/debug/jeti-lsp` / `target/release/jeti-lsp`（所以在 Jeti 仓库里
零配置即可用），找不到再用 `PATH`。设置项：

| 设置 | 默认值 | 含义 |
| --- | --- | --- |
| `jeti.serverPath` | `jeti-lsp` | 服务器可执行文件；给路径就按路径用，给裸名字则回退到 `PATH`。 |
| `jeti.searchPaths` | `["include", "."]` | 解析 `#import` 的 `--include` 目录，相对工作区根目录。 |
| `jeti.arc` | `true` | 按 ARC 模式分析（编译器默认）。用 `-fno-jeti-arc` 构建的项目请关掉。 |
| `jeti.arguments` | `[]` | 额外的服务器参数，用 `jetic` 的拼写：`-fno-jeti-arc`、`-fno-checker`、`-eh legacy`。 |

后两项存在的原因是：**编辑器必须和你的构建一致**。默认情况下服务器完全按 `jetic` 的默认分析
（ARC 开、checker 开），所以手写 retain/release 的旧代码会显示
`explicit 'release' not allowed in ARC mode` —— **因为编译器就是这么报的**（那些测试文件能过，是因为
测试脚本会再用 MRC 重试一遍）。告诉编辑器你是怎么构建的，能过的代码就不再报错。

客户端**没有任何 npm 依赖**（只用 `vscode` 与 node 内建模块），因此不需要 `npm install`。
诊断来自编译器前端，这里报出的东西与命令行 `jetic` 报出的一致。

---

## 语言特性

### 类系统

```jeti
@interface Animal : NPObject {
@public
    NPString *_name;
}
- (instancetype)initWithName:(NPString *)name;
- (void)speak;
@property (readonly) NPString *name;
@end

@implementation Animal
- (instancetype)initWithName:(NPString *)name {
    self = [super init];
    if (self) {
        _name = name;
    }
    return self;
}
- (void)speak {
    printf("...\n");
}
@end
```

### 协议

```jeti
@protocol Drawable
- (void)draw;
- (BOOL)isVisible;
@end

@interface Shape : NPObject <Drawable>
@end
```

### 属性

```jeti
@interface Person : NPObject
@property NPString *name;
@property int age;
@property (readonly) NPString *identifier;
@end
```

### 类别（Category）

```jeti
@interface Person (Printing)
- (void)printGreeting;
@end

@implementation Person (Printing)
- (void)printGreeting {
    printf("Hello, my name is %s\n", [self name]);
}
@end
```

### Block

```jeti
int (^square)(int) = ^int(int x) {
    return x * x;
};

void (^logAndCall)(NPString *, void (^)(void)) = ^void(NPString *msg, void (^next)(void)) {
    printf("[LOG] %s\n", msg);
    if (next) next();
};
```

### @autoreleasepool

```jeti
@autoreleasepool {
    NPString *temp = [NPString stringWithUTF8String:"hello"];
    // temp 在 pool pop 时自动 release
}
```

### @selector

```jeti
SEL sel = @selector(doSomething:);
```

### C 完全兼容

```jeti
#include <stdio.h>
#include <stdlib.h>

@interface Wrapper : NPObject
- (void)callCFunction;
@end
```

### C 属性（__attribute__）

Jeti 支持 `__attribute__((...))` 透传。你可以在全局声明和 struct 字段上直接写 C 的 `__attribute__`，编译器会把它们原样保留到生成的 C 代码中。

```jeti
__attribute__((packed))
struct Point {
    int x;
    int y;
};

__attribute__((format(printf, 1, 2)))
int my_log(const char *fmt, ...);
```

编译器内置了一个 **590 个属性的三分类表**（来源：Clang 和 GCC 官方文档）。`-backend` 选项控制允许使用哪些属性：

| 选项                        | 行为                                                          |
| ----------------------- | ----------------------------------------------------------- |
| `-backend=clang`（默认）    | 允许 clang 专属属性（如 `availability`、`diagnose_if`、`objc_direct`）；gcc 专属属性报错 |
| `-backend=portable`     | 只允许 gcc 和 clang 都支持的属性，其余报错                                 |
| `-backend=gcc`          | 允许 gcc 专属属性（如 `strub`、`optimize`、`stack_protect`）               |

不在表中的未知属性只产生 warning 并透传，绝不会报错。

### C 桥接（`-emit-bridge-header`）

Jeti 转译为 C 后，C 代码可以直接调用 Jeti 对象方法。但消息派发需要写 vtable 下标和 SEL 常量，代码冗长且易错。`-emit-bridge-header` 选项为每个方法生成一个 `static inline` 包装函数，让 C 代码像调用普通 C 函数一样调用 Jeti 对象。

**用法**：先转译 Jeti 库成 C，同时生成桥接头：

```bash
jetic -rewrite-jeti lib.jeti -o lib.c -emit-bridge-header lib.h
```

然后 C 代码包含桥接头，直接调用：

```c
#include "lib.h"

int main(void) {
    jeti_metaInit();  // 元数据回填——见下方说明

    // 类方法：jeti_<类名>_<方法名>(参数...)
    NPString *s = jeti_NPString_stringWithUTF8String_("Hello");

    // 实例方法：jeti_<类名>_<方法名>(self, 参数...)
    size_t len = jeti_NPString_length(s);
    const char *cstr = jeti_NPString_UTF8String(s);

    // 嵌套消息发送（等价于 Jeti 的 [[s UTF8String] ...]）
    const char *nested = jeti_NPString_UTF8String(
        jeti_NPString_stringWithUTF8String_("nested")
    );

    // 多参数消息发送（等价于 Jeti 的 [arr replaceObjectAtIndex:0 withObject:obj]）
    NPArray *arr = jeti_NPArray_arrayWithObject_(s);
    jeti_NPArray_replaceObjectAtIndex_withObject_(arr, 0, s);

    // 多参数带命名空间：selector 的每个 : 对应函数名里的一个 _
    // [obj foo:arg1 bar:arg2] → jeti_<类>_foo_bar_(obj, arg1, arg2)
    // [m replaceCharactersInRange:rng withString:str]
    // → jeti_NPMutableString_replaceCharactersInRange_withString_(m, rng, str)
}
```

编译时链接 `lib.c` 和 `runtime.c`：

```bash
clang caller.c lib.c include/jeti/runtime.c -I include -o app
```

⚠️ 桥接头使用了 `sel_registerName` 在运行时解析 selector，因此**不需要**依赖 codegen 生成的 `static const` SEL 常量（这些常量跨文件不可见）。类元数据本身由每个主文件持有 `@implementation` 的 TU 在**加载期静态初始化**——所有 `jetic` 工作流皆是如此。`jeti_metaInit()` 保留在示例中作为无害的幂等回填；只有当构建通过 `#import "*.jeti"` 获取实现时（单 TU 伞形构建，无任何 TU 持有元数据）才**真正必需**。

#### 从 C 管理内存

Jeti 的 **ARC 是编译期概念，只分析 `.jeti` 源码**——C 代码调用桥接函数时，返回值不会自动 retain/release。需要手动管理，遵循 ObjC 的内存管理命名约定：

| 方法家族                               | 调用者拥有？         | C 端怎么做                                 |
| ---------------------------------- | -------------- | -------------------------------------- |
| `alloc`、`new`、`copy`、`mutableCopy` | ✅ +1           | 用完必须 `jeti_release(obj)`               |
| `init`                             | ❌ 消耗 alloc     | 不需要操作                                  |
| 其他（如 `stringWithUTF8String:`）      | ❌ autoreleased | 不需要操作；若需跨 pool 存活，先 `jeti_retain(obj)` |

```c
#include "lib.h"

int main(void) {
    jeti_metaInit();
    jeti_autoreleasepool_t *pool = jeti_autoreleasepoolPush();

    // 便利构造器返回 autoreleased 对象，在当前 pool 内使用即可
    NPString *s = jeti_NPString_stringWithUTF8String_("hello");
    printf("%s\n", jeti_NPString_UTF8String(s));

    // 如需跨 pool 存活：先 retain，用完 release
    NPString *t = jeti_NPString_stringWithUTF8String_("world");
    jeti_retain(t);
    jeti_autoreleasepoolPop(pool);      // t 存活（retain 过）
    printf("%s\n", jeti_NPString_UTF8String(t));
    jeti_release(t);

    // alloc/copy 家族返回 +1 → 必须 release
    NPString *copy = jeti_NPString_copy(s);
    jeti_release(copy);
}
```

`jeti_retain`、`jeti_release`、`jeti_autorelease`、`jeti_autoreleasepoolPush`/`jeti_autoreleasepoolPop` 声明在 `<jeti/runtime.h>` 中，对任何 Jeti 对象都可用。这就是 MRC（手动引用计数）模型——从 C 侧看，Jeti 对象就是按"我拥有/不拥有"约定管理的裸指针。

### 新特性

Jeti 在 Objective-C 语法基础上，加入了一些 ObjC 本身没有的语言特性。

**近期亮点：**

- **谓词 / KVC（`NPPredicate`）** — 格式串解析器与求值引擎**全在 Foundation 库的 C 里**（`age > 18 AND name BEGINSWITH[c] 'A'`、`ANY tags LIKE '*dev*'`），底层是编译期发射的 KVC 访问表（`JETI_KVC_$_X`，owner TU 出强表），并带宿主过滤 API（`filteredArrayUsingPredicate:` / `indexOfObjectMatchingPredicate:` / `filterUsingPredicate:`；`nil` 谓词 = 恒等不过滤）。编译器从不解析格式串 —— 见 `docs/plans/architecture.md` §12。
- **集合（`NPSet` / `NPMutableSet` / `NPOrderedSet`）** — Foundation 库里的哈希桶集合容器（元素唯一，`containsObject:` / `anyObject` / `setWithObjects:count:`），`NPOrderedSet` 额外保持插入顺序；全部容器方法走静态 vtable 派发，跨 TU 安全。
- **原生裸机支持（`-ffreestanding`）** — 编译为自包含 C，无 libc、无 Foundation、无 TLS；`@try/@catch` 走默认的 `-eh checked` 后端（纯旗标 + 守卫，**完全不用 `setjmp/longjmp`**，这正是裸机可用的前提；`-eh legacy` 才回退到 `__builtin_setjmp/longjmp`），零样板的 `runtime_freestanding.c` 提供 bump allocator、`JETI_CLASS_$_jeti_root`、异常状态和 `memcpy`。
- **C 超集** — `@protocol` + 一致性检查、`@property` + `@synthesize`、`instancetype`、`@public` ivar、点语法、struct + 函数指针、内联汇编、C 风格类型转换。
- **类型化 `@catch`** — 每个 catch 块现在检查 `isa == &JETI_CLASS_$_Class`，只有匹配的类才进入该处理器；多个 catch 正确隔离。
- **ARC 修复** — 作用域栈模型不再在嵌套作用域结束时释放父作用域变量；`for` 初始化对象提升修复了泄漏和非法 `for` 头。
- **`@noarc` 块** — 块级 MRC：在 ARC 模式下，`@noarc { }` 块内允许手动 `retain`/`release`/`dealloc`/`autorelease`；是 `-fno-jeti-arc` 和 clang `-fno-objc-arc` 的块级等价物。
- **`__attribute__` 透传 + `-backend`** — 完整支持 C 的 `__attribute__((...))` 和所有 `__` 前缀的 C 预定义标识符（`__FILE__`、`__LINE__`、`__builtin_*`、`__extension__`、`__typeof__`、`__alignof__` 等）；`-backend` 选项控制哪些编译器专属属性允许使用。

### for-in 遍历

```jeti
for (NPString *s in arr) {
    printf("%s\n", [s UTF8String]);
}
```

在 parser 层 desugar 为对 `[coll count]` / `[coll objectAtIndex:]` 的下标循环——集合表达式只求值一次，nil 安全（`[nil count]` 为 0），元素是借用语义（不 retain/release）。普通 C 数组和 `NPArray` 都能用。

### 协议一致性检查

checker 现在会验证声明了 `<Proto>` 的类实现了协议的全部必需方法（递归遍历父协议；`@optional` 方法豁免）：

```
class 'Circle' does not implement required method 'draw' from protocol 'Drawable'
```

### 协议组合（`P & Q`）

复用 C 的 `&` 运算符同时要求多个协议——零新语法：

```jeti
// ① 交集类型：接收者必须同时实现两个协议
void render(id<Drawable & Serializable> item);

// ② 组合协议声明
@protocol Renderable <Drawable & Serializable>
@end
```

协议类型只是编译期约束标签——不影响 vtable 槽位，跨编译单元布局不变。

### Foundation 类型分发（`isKindOfClass:` / `respondsToSelector:` / `isEqual:`）

根类现在实现了 ObjC 官方拼写的类型分发三件套，不需要任何新语言结构就能写惯用的多路分发：

```jeti
for (id item in items) {
    if ([item isKindOfClass:[Dog class]]) {
        [(Dog *)item bark];
    } else if ([item respondsToSelector:@selector(draw)]) {
        [(id<Drawable>)item draw];
    }
}
```

`isKindOfClass:` 沿 isa 链查找，`respondsToSelector:` 查统一 vtable，`isEqual:` 在根类上默认指针相等（与 `NSObject` 一致）——而 `NPString` 与 `NPNumber` 各自重写为**值相等**，这正是字典键能用的前提。旧拼写 `isKindOf:` 保留作兼容别名。

### struct `==` / `!=` 值比较

C 直接拒绝 struct 的 `a == b`；Jeti 复用现有运算符，desugar 为生成的逐字段比较函数：

```jeti
struct Point a = {1, 2};
struct Point b = {1, 2};

if (a == b) { ... }   // 逐字段比较
if (a != b) { ... }

p == &a               // 指针比较语义不变
```

### async/await

async 方法用修饰符直接声明**业务返回类型**——`async int`、`async void`，乃至 `async NPArray<NPString *> *`；挂起用裸软关键字 `await`（上下文敏感：名为 `await` 的 C 标识符在其他所有位置仍按标识符解析）：

```jeti
@interface Fetcher : NPObject
- (async int)compute:(int)n;   // 会挂起，产出 int
- (async void)runAll;          // async void = 入口方法
@end

@implementation Fetcher
- (async int)compute:(int)n {
    int raw = await n;                // 挂起点
    return raw * 2;                   // 方法体直接返回 T，不返回任务
}

- (async void)runAll {
    int x = await [self compute:21]; // await 调用会解包成 T
    NPLog(@"result=%d", x);
}
@end

int main() {
    // v1 规则3：blockOn: 是唯一同步→异步桥——裸 async 调用在任何位置都是 error（规则2）。
    // 接收者在块内分配（块 thunk 阶段3前只捕获参数+self）。
    [NPTask blockOn:async ^ void {
        Fetcher *f = [[Fetcher alloc] init];
        await [f runAll];             // 规则2：async 调用必须 await
    }];
    return 0;
}
```

设计规则：

- **`async` 直接修饰业务类型**——`async int` / `async void` / `async NPString *`。旧的 `(async NPTask<T>)` 拼写已废除（报迁移 error）；裸 `- (NPTask<int>)load` 是**普通同步方法**，只返回一个任务对象。
- **签名声明、checker 强制**——有 `await` 不标 → error。体内无 `await` 合法（Async Semantics v1 规则 1：`async` 表示*允许*挂起——缓存命中、碰巧同步的协议实现都是正常形态；见 `docs/plans/async_semantics_v1.md`）。**调用 async 方法只有一种形态：`await [f x]`**（规则2）——裸 async 调用在任何位置都是 checker error（语句位/表达式位/cast 弃值/return 位/同步方法内）：不再有"裸调用创建任务"、语句位自动驱动、同步调用方自动 wrapper。同步→异步唯一显式桥是 `+[NPTask blockOn:<async 块>]`（规则3）；`await` 是唯一阻塞驱动形态。
- **`await` 降级为状态机**——方法体在挂起点被拆进 `switch(task->state)` 驱动的堆上 `NPTask`；活过挂起点的局部变量提升进每方法一个的 frame 结构体。
- **消息发送内的 `await` 必须加小括号**——实参写 `[self wrap:(await [self load:4])]`，接收者写 `[(await [self box:2]) value]`；裸写 `[self wrap:await [self load:4]]` 是解析错误（带修正提示）：不加括号会被读成 await 盖住整条消息链（Swift 语义），Jeti 刻意不为它定义优先级。选择子位（`[task await]`）与名为 `await` 的 C 标识符不受影响；cast/分组括号合法——`(int)(await [self load:3])` 先驱动任务再转换。
- **错误处理走普通 ObjC 式 EH**——可失败的 async 方法用 `throws(ErrType *)` 标注（文档性，与同步侧同一拼写）；调用方用普通 `@try/@catch` 处理失败；无人接则顶层 loud abort。单纯的挂起不触发 `@finally`，失败的 await 才触发。（checked EH 后端，默认。）
- 协作式单线程调度器（`jeti_sched_run`）与 I/O 集成是下一个里程碑。

### `switch` 模式匹配（`case` 模式）

`case` 标签可以写**模式**，不只是整型常量。类型分发的内核仍是方法链——模式 desugar 成 `isKindOfClass:` / `isEqual:` / 比较——但你写成声明式的样子：

```jeti
// 对象模式可以在同一个 switch 里自由混用：
switch (subject) {
    case NPString *s:                          // 类型绑定 → isKindOfClass:
        NPLog(@"string: %s", [s UTF8String]);
        break;
    case NPNumber *n when [n intValue] > 3:    // 类型绑定 + `when` 守卫
        NPLog(@"number: %d", [n intValue]);
        break;
    case @"literal":                           // 对象字面量 → isEqual:（值语义）
        NPLog(@"matched a literal");
        break;
    default:
        break;
}

// 比较模式用在**标量** subject 上：
switch (n) {
    case > 100:
        NPLog(@"big");
        break;
    case > 0 && < 100:                         // 区间
        NPLog(@"small");
        break;
    default:
        break;
}

// 普通多值常量仍是纯 C：
switch (n) {
    case 1, 2, 3:
        NPLog(@"one of 1-3");
        break;
    default:
        break;
}
```

| 模式 | 降级为 |
|------|--------|
| `T *name` | `jeti_isKindOfClass(subject, &JETI_CLASS_$_T)`；臂内 `name` 已绑定为 `(T *)subject` |
| `> 10` / `< 10` / `>= 0` / `<= 9` | `subject > 10`（subject 填进悬空的操作数位） |
| `> 0 && < 100` | `subject > 0 && subject < 100` |
| `@"lit"` / `@42` / `@YES` / `@'c'` / `@(expr)` | `[subject isEqual:<字面量>]`——值语义，`@"lit"` 能匹配**另一个**内容相同的 NPString |
| `T *x when <expr>` | 类型测试再 `&&` 上守卫 |
| 其余 | 普通 C 常量，用 `==` 比较 |

`when` 是**上下文关键词**——`int when = 1;` 照常编译。臂自上而下逐个测试、首个命中即生效；`break`（或走到臂尾）离开 switch。**fallthrough 遵循 C 语义**：没有 `break` 的臂会落入下一臂的臂体。

永远不可能是 C 常量的 `case` 标签——`case [obj msg]:`、`case f():`、`case a = b:`——会报 `case label is not a constant expression`，而不是漏进生成的 C、再在那里报一个无法对应源码的错误。

M1 限制，全部**报错而非静默编译错**：

- **常量臂不能与模式臂混用**——C 路径需要原始 body，模式路径会丢弃它。拆成两个 switch。
- **悬空比较臂不能与类型绑定／对象字面量臂混用**，也**不能用在语法上就是对象的 subject 上**（`switch (@7) { case > 100: ... }`）。两种情况都会让 subject 变成对象，于是 `subject > 100` 变成**指针**比较——恒真，且毫无提示。改对标量 switch（`switch ([o intValue])`）。
- 残留 M1 缺口：持有对象的**变量** subject（`id o = @7; switch (o) { case > 100: ... }`）仍会退化——要判对它需要 parser 拿不到的类型信息。
- 嵌套模式没有跨层 `case` 作用域（内层 `case` 归内层 switch，与 C 一致）。

实现：parser 判定每个标签的形态并把整个 switch 摊平成一张臂表；一个新的降级 pass 在 checker 之前把它改写成 `goto`/`if` 链 + C 标签——所以生成的 C 仍是纯 C99，且不引入新 IR。Golden：`tests/golden/42_switch_pat/`。

### 装箱字面量（`@(expr)` / `@YES` / `@NO` / `@'c'`）

```jeti
NPNumber *a = @123;          // int
NPNumber *b = @1.5;          // double
NPNumber *c = @YES;          // BOOL → 1
NPNumber *d = @'c';          // char
NPNumber *e = @(i + 1);      // 工厂由操作数的静态类型决定
```

每个形态都产出真正的 `NPNumber`：字面量自身的类型选定工厂（`numberWithInt:` / `numberWithDouble:` / `numberWithChar:`），`@(expr)` 则按**操作数的静态类型**选——`double`/`float` → `numberWithDouble:`、`BOOL` → `numberWithBool:`、`char` → `numberWithChar:`、`long`/`long long` → `numberWithLongLong:`、其余整数 → `numberWithInt:`。装箱结果就是普通对象，照常派发：`[@(i * 2) intValue]`。

`@(expr)` 的改写落在 **checker** 而不是 parser：parser 没有类型，后端是 C99 更没有 `_Generic` 可用。改写复用普通消息发送节点，静态派发与 nil 守卫因此零特判——与对象下标、struct `==` 同一套机制。非算术类型不会被默默装箱，而是报错：

```
illegal type 'NPString *' in a boxed expression — '@(...)' accepts arithmetic and BOOL values only
```

### 字典字面量（`@{ key: value }`）

```jeti
NPDictionary *d = @{ @"a": @1, @"b": @2, @"c": @3 };
NPLog(@"%d", [[d objectForKey:@"b"] intValue]);   // 2
printf("%lu\n", (unsigned long)[d count]);        // 3

NPMutableDictionary *m = [NPMutableDictionary dictionary];
[m setObject:@10 forKey:@"x"];
[m setObject:@11 forKey:@"x"];   // 相等的键是替换，不追加
[m removeObjectForKey:@"x"];

NPDictionary *empty = @{};       // `@{}` 是空字典（数组是 `@[]`）
```

键用 `isEqual:` 比较，因此 `NPString`/`NPNumber` 键是**值语义**。字符串字面量已 **interning**（同内容 → 同一对象，ObjC 常量串语义），且值相等仍是语义保证——用新写的 `@"b"` 查询照样能找到条目。存储照搬 `NPArray`——两个平行对象数组 + 线性扫描；`count` / `objectForKey:` / `allKeys` / `allValues` / `copy` / `description` / 按内容的 `isEqual:` 补全了 API。条目必须是对象（与 ObjC 一致）：

```
illegal type 'int' in a dictionary literal — keys and values must be Objective-C objects
```

### 异常语义（`-eh checked` —— 默认后端）

Jeti 的异常是**不用栈展开的 ObjC 异常语义**。`@try`/`@catch`/`@finally`/`@throw` 的行为与 clang `-fobjc-arc-exceptions` 模式完全一致——差分测试套件（`tests/eh_diff/run_eh_diff.sh`）把每个用例同时跑在 jetic 与真 clang/ObjC 下、逐行 diff stderr，锁定这一保证（7/7 通过）。

```jeti
@interface Boom : NPObject
- (void)fire;
@end

@implementation Boom
- (void)fire {
    @throw @"negative input";
}
@end

int main() {
    @try {
        Boom *b = [[Boom alloc] init];
        [b fire];                       // 执行到此为止
        NPLog(@"never runs");
    }
    @catch (NPString *e) {
        NPLog(@"caught: %@", e);
    }
    @finally {
        NPLog(@"finally always runs");
    }
    return 0;
}
```

你得到的语义：

- **ARC 结算每一帧** —— 异常穿透中间帧时，每帧拥有的局部对象照常释放。没有 `longjmp`，没有跳过的清理，没有泄漏。
- **throw 立即中断** —— 同一语句的剩余部分不再求值：`x = [a risky] + [b sideEffect];` 不会执行 `sideEffect`。
- **typed catch 链按 isa 匹配** —— 不匹配的 `@catch` 放行给外层 `@try`；catch 内重抛传播到外层处理器，不会重入本层。
- **`@finally` 顺序** —— 内层 finally 在外层 catch 之前执行；外层 finally 在外层 catch 之后执行。
- **block 字面量内的 `@throw`** —— 像普通调用点一样传播到外层 `@try`。
- **未捕获异常 abort** —— 输出 ObjC 措辞 `*** Terminating app due to uncaught exception of class 'NPString'`，退出码 1。
- **C 调用方不会错过异常** —— 桥接头 wrapper 检查错误旗标并 abort，而不是静默返回零值。

**`-eh checked` 已是默认后端**——直接 `jetic run` 就用它。`-eh legacy`（别名 `-eh sjlj`）切回旧的零开销 setjmp 后端，是一条完整的回退路径；该后端有经典限制：跨函数抛出会跳过中间帧的清理（见下方已知限制）。

### `throws` —— 声明式异常

`@throw` 与 `throws` 只差一个字母，语义却完全不同——对应 Java 里 `throw`（语句）与 `throws`（声明子句）的分工。

| | `@throw` | `throws` |
|--|----------|-----------|
| 是什么 | **语句** —— 运行时抛出异常 | **声明标注** —— 编译期元数据 |
| 写在哪 | 函数/方法体内 | 声明尾部，`;` 或 `{` 之前 |
| 形态 | `@throw expr;` | `throws(T *)` 或裸 `throws` |
| 进生成的 C 吗 | 进（setjmp/旗标机制） | **永不** —— 无代码、不占 vtable 槽位 |

```jeti
@interface Repo : NPObject
- (NPString *)fetch:(const char *)url throws(NPError *);   // 会抛 NPError *
- (int)parse:(const char *)s throws;                       // 会抛，类型不注明
- (int)count;                                              // 从不抛
@end

@implementation Repo
- (NPString *)fetch:(const char *)url throws(NPError *) {
    if (!url) {
        @throw [[NPError alloc] init];   // 语句：抛出
    }
    return @"ok";
}
@end
```

苹果在**这个槽位**（声明 `;` 前的尾置元数据）已经用宏占了十几年：`NS_DESIGNATED_INITIALIZER`、`NS_REQUIRES_NIL_TERMINATION`、`API_AVAILABLE(...)`。Jeti 把同一槽位扶正为一等语法，并让 checker 直接对账。

**checker 强制什么**

- `throws(T *)` —— 逃逸出本声明的每个 `@throw`，其静态类型必须与 `T` 相容（允许子类）：
  `error: '@throw' of type 'AppError *' does not match the declared 'throws(NPString *)'`
- 裸 `throws` —— 体内必须确有逃逸的 `@throw`：
  `error: 'liar' is marked 'throws' but its body never executes '@throw'`
- 不写 —— 逃逸的 `@throw` 报 error：
  `error: '@throw' escapes 'bad' without a 'throws' annotation; declare it with 'throws(<type>)' or handle it with a local '@try'`

被**同一体内** `@try` 捕获的 `@throw` 不算逃逸，因此 `main`、以及自己就地兜住的 helper 都不需要标注：

```jeti
static void bad(int n) {                        // error：逃逸出 'bad'
    if (n < 0) {
        @throw [[AppError alloc] init];
    }
}

static void good(int n) throws(AppError *) {   // 已声明 —— 通过
    if (n < 0) {
        @throw [[AppError alloc] init];
    }
}

static void guarded(int n) {                    // 通过 —— 就地捕获
    @try {
        if (n < 0) {
            @throw [[AppError alloc] init];
        }
    }
    @catch (AppError *e) {
    }
}
```

类型判定刻意保守：`@"..."` 字面量、裸 C 字符串、Cast 目标类型、已知类型的变量会被判定；**消息发送不判**（只有 selector 的注册表无从得知其类），因此它对任意声明类型都放行。标注纯编译期——增删 `throws` 不改变生成的 C、程序输出与 ARC 行为。两个关键词互相写错位置本身也是 error：体内写 `throws`、声明上写 `@throw(...)`，各会收到一条指明正确关键词的诊断。

#### 隐式根类（jeti_root）

Jeti 现在支持用户自定义根类。你不再需要强制继承 `NPObject`——不写父类的 `@interface` 会自动获得编译器注入的隐式根类 `jeti_root`，同时保持 `id` 类型的统一性和静态派发能力。

**之前：**

```jeti
@interface Animal : NPObject   // 必须继承 NPObject
```

**之后：**

```jeti
@interface Animal              // 不写父类 → 隐式根类
@interface Animal : NPObject   // 显式继承 NPObject 仍然合法
```

两者都合法，且 `id` 可以指向任何 Jeti 对象。

#### 核心机制

当用户不写父类时，编译器自动注入 `jeti_root`：

```jeti
// 用户代码：
@interface Animal {
    int age;
}
- (void)speak;
@end

// 编译器视为：
@interface Animal : jeti_root {
    int age;
}
- (void)speak;
@end
```

生成的 C 代码：

```c
// 编译器内置结构
struct jeti_object_header {
    struct jeti_vtable *vtable;
};

struct jeti_root {
    struct jeti_object_header header;
};

// Animal 的 struct
struct Animal {
    struct jeti_root __super;  // 包含 header
    int age;
};
```

#### id 的新定义

```c
typedef struct jeti_root *jeti_id_t;
```

`id` 不再绑定任何具体类，只要求对象以 `jeti_root` 开头：

```jeti
Animal *a = [[Animal alloc] init];
id obj = a;                    // ✅ 合法，Animal 继承自 jeti_root
[obj speak];                   // 静态派发：obj->header.vtable[...]
```

#### NPObject vs jeti_root

| 写法                          | 含义                       | 适用场景            |
| --------------------------- | ------------------------ | --------------- |
| `@interface Xxx`            | 隐式继承 `jeti_root`，最轻量     | 自定义内存布局、内核、嵌入式  |
| `@interface Xxx : NPObject` | 显式继承，获得 retain/release 等 | 用户态应用、需要完整运行时支持 |

```jeti
// 自定义根类：轻量，无引用计数
@interface KernelTask {
    int pid;
    int priority;
}
- (void)run;
@end

// 使用 NPObject：完整功能，自动内存管理
@interface UserModel : NPObject
@property NPString *name;
@end
```

#### 裸机 / Freestanding 支持（`-ffreestanding`）

Jeti 可以编译为**无 libc、无 Foundation、无 TLS** 的自包含 C，直接用于内核、MCU、嵌入式裸机开发。

```bash
jetic -rewrite-jeti -ffreestanding kernel.jeti   # 生成自包含 C
```

`-ffreestanding` 模式下转译出的 C：

- 不 `#include <string.h>`，改 `#include <jeti/runtime.h>`（freestanding 分支）
- `@try/@catch/@finally` 走默认 `-eh checked` 后端：纯旗标 + 守卫，零 `setjmp/longjmp`、零 `jmp_buf`（`-eh legacy` 才用 `__builtin_setjmp/longjmp` + 普通全局而非 `__thread`）
- 类型（`SEL`/`NPClass`/`NPObject`/`id`）自含
- **不捆绑 Clang Blocks 运行时** —— block 字面量引用 `__NSConcreteStackBlock`/`_Block_copy`/`_Block_release`；真裸机上要么链接一个 Blocks runtime 移植，要么用 `-backend portable`/`-backend gcc`（block 展开为普通 C 函数，无 ABI 符号）

用户只需提供：`jeti_jeti_root_class`、异常全局（如用 `@try`）、`memcpy`（如用 `@try`）、freestanding 头（`stdint.h`/`stddef.h`/`stdbool.h`）。

**裸机分配器 + `[[Class alloc] init]`**（`include/jeti/runtime_freestanding.c`）：

```jeti
@interface HeapCounter {
    int total;
}
+ (id) alloc;
- (id) init;
- (int) add:(int)x;
@end
@implementation HeapCounter
+ (id) alloc  { return jeti_alloc(self); }   // bump allocator
- (id) init   { return self; }
- (int) add:(int)x { total += x; return total; }
@end

void demo(void) {
    HeapCounter *c = [[HeapCounter alloc] init];  // 裸机堆分配
    [c add:10];                                    // → 10
}
```

已跑通的特性（`examples/04_soma-kernel/` 的 i386 保护模式内核 + `tests/golden/25_freestanding/`）：

- `@namespace` + `@interface`（隐式根类）
- 类方法 / 实例方法消息派发
- `@try/@catch/@finally`
- `@selector`、内联 asm、C 类型转换
- `[[Class alloc] init]` 裸机堆分配 + ARC 自动 `jeti_release`

运行示例（soma-kernel 在 qemu 下）：

```
[jeti] class method [SomaCore::Calculator compute:21] = 43
[jeti] instance methods on C-created obj: add:7 -> 7, add:35 -> 42, value = 42
[jeti] @try/@catch demo:
       try body, throwing...
       caught [e errorCode] = 42
       finally always runs
       after-try continues
[jeti] alloc+init (bump allocator):
       [c add:10]=10 [c add:20]=30 [c value]=30
```

#### 方法派发

所有 Jeti 对象通过统一的 VTable 机制静态派发：

```c
// [obj doSomething:arg]
obj->header.vtable[INDEX_doSomething](obj, arg);
```

编译器为每个选择器分配全局固定索引，所有类的 VTable 在同一位置存储对应方法的函数指针。

#### 内核友好设计

对象头极简，只有 vtable 指针（8 bytes on 64-bit），引用计数由编译期 ARC 静态分析管理，不占用运行时对象空间。

#### 状态

   已实现：

- [x] 隐式根类注入（语义分析阶段）
- [x] `jeti_root` 和 `jeti_object_header` 的 C 代码生成
- [x] `id` → `jeti_id_t` 的类型映射
- [x] 统一 VTable 索引分配
- [x] 根类/子类 struct 生成
- [x] 单元测试覆盖

#### @namespace 命名空间

`@namespace` 用于组织类、函数、常量等代码实体，避免全局命名冲突。这是 ObjC 没有的特性——在传统 ObjC 中需要用前缀（如 `NS`、`UI`）来模拟。

```jeti
@namespace Game
    @interface Player : NPObject {
        int health;
    }
    - (id)init;
    - (int)getHealth;
    @end

    @implementation Player
    - (id)init {
        self = [super init];
        if (self) health = 100;
        return self;
    }
    - (int)getHealth { return health; }
    @end
@endnamespace

@namespace UI
    @interface HUD : NPObject {}
    - (void)showPlayerHealth:(Game::Player *)player;
    @end
@endnamespace
```

**编码规则**：命名空间通过 `::` 分隔，转译为 C 时使用 `__` 编码。

| Jeti 符号                   | 转译后的 C 符号                    |
| ------------------------- | ---------------------------- |
| `Game::Player`            | `Game__Player`               |
| `Game::Entities::Enemy`   | `Game__Entities__Enemy`      |
| 方法 `-[Game::Player init]` | `Game__Player_init`          |
| VTable                    | `JETI_VTABLE_$_Game__Player` |
| 类元数据                      | `JETI_CLASS_$_Game__Player`  |

**特性**：

- 支持嵌套命名空间（`Game::Entities::Enemy`）
- 支持跨命名空间引用（`Game::Player *player`）
- 支持命名空间内的 `@class` 前向声明（`@class Player;`）——codegen 会发射 `struct Game__Player;` + `typedef struct Game__Player Game__Player;`，使前向声明的类型可在方法签名中使用
- 支持跨命名空间继承（`@interface HUD : Engine::Graphics::Renderable`）
- 无需前缀约定，编译器自动编码 C 符号
- 无命名空间的类保持向后兼容

#### @using 导入机制

`@using` 用于将其他命名空间中的符号导入当前作用域，避免每次使用都写完整限定名。支持三种形式：

**形式一：导入完整限定名**

```jeti
@using Game::Player;
Game::Player *p = [[Game::Player alloc] init];
// 可以直接用 Player 代替 Game::Player
Player *p = [[Player alloc] init];
```

**形式二：导入并指定别名**

```jeti
@using GP = Game::Player;
// 用 GP 作为 Game::Player 的别名
GP *p = [[GP alloc] init];
```

**形式三：导入整个命名空间**

```jeti
@using namespace Game;
// Game 命名空间下的所有类可以直接用短名访问
Player *p = [[Player alloc] init];
Enemy *e = [[Enemy alloc] init];
```

**冲突检测**：

- 如果短名与当前作用域已有符号冲突，编译器报错
- 如果两个 @using 条目导入相同的短名，编译器报二义性错误
- 别名和短名在 `@using` 声明所在的文件作用域内有效

#### `@noarc` 块 — 块级 MRC

在 ARC 模式下，checker 禁止手动内存管理：

```jeti
[obj release]; // 错误：explicit 'release' not allowed in ARC mode
```

`@noarc { }` 划定一个可以手动管理内存的块——它是 `-fno-jeti-arc`（以及 clang 的 `-fno-objc-arc`）的块级等价物：

```jeti
@noarc {
    [obj retain];
    [obj release];
    [obj autorelease];
}
```

要点：

- **块级作用域** — 只有 `@noarc { }` 内的语句豁免。块外仍使用静态 ARC，块外手动 `retain`/`release`/`dealloc`/`autorelease` 是编译错误。
- **不注入 ARC** — ARC 分析器完全跳过 `@noarc` 块，不为其中使用的对象插入任何 retain/release。
- **运行时方法豁免** — `retain`/`release`/`dealloc`/`autorelease` 自身的实现无需 `@noarc` 即可调用这些方法。
- **全程序等价物** — `-fno-jeti-arc` 把整个程序切到 MRC；`@noarc` 对单个块做同样的事。
- **Foundation** — NPString/NPMutableString 的便捷构造器（`+stringWithUTF8String:`、`+stringWithString:`）把刻意为之的 `autorelease` 包在 `@noarc { }` 里。

---

#### 引用计数追踪（`-trace-refcount`）

`-trace-refcount` 在 **ARC 注入之后**对 AST 跑一个静态引用计数模拟器，按时间顺序打印每个存活对象的计数追踪，然后直接退出（不生成代码、不编译）。它是调试辅助工具，用来验证每个对象恰好被 release 一次（无泄漏、无二次释放）。

```bash
jetic -trace-refcount app.jeti                              # 彩色追踪
jetic -trace-refcount -trace-no-color -trace-max-iters 2 app.jeti
```

选项：

- **`-trace-no-color`** — 关闭 ANSI 颜色（便于 diff / CI 管道）。
- **`-trace-max-iters <N>`** — 每个循环在追踪中模拟的迭代次数（默认 2）；每轮迭代都会得到全新的 `Cat#N` 对象身份。
- 颜色含义：**绿** = 计数增加 · **蓝** = 计数减少（仍存活）· **青** = 已释放（归零）· **红** = 二次释放/过度释放，以及 Summary 中的 `possible leak`/`over-released` · **黄** = 信息性的未追踪目标警告。
- 对象身份 = 分配位置（每个类一个 `Class#N` 计数器）；创建源是 `alloc`/`new`/`copy`/`mutableCopy` 前缀、`init` 链回其接收者，以及 `@"..."`/`@[...]` 字面量。
- 离开被追踪作用域的对象不计入泄漏 Summary：`return`/`@throw` 的结果、`static` 单例、`@"..."`/`@[...]` 字面量和方法参数。收尾的 `== Summary ==` 报告存活（`possible leak`）、过度释放（计数为负）和已释放对象，并带分配位置。

### `@defer` —— 作用域退出执行

Go 风格的延迟清理：`@defer { ... }` 把自己的 body 注册到**最内层复合语句块**上，body 在该块的**每一处出口**执行——块尾自然出口、任意深度的 `return`、跳出该块的 `break`/`continue`、同函数 `@throw`——最内层优先（LIFO）。

```jeti
- (void)work {
    FILE *f = fopen("cfg.txt", "r");
    @defer { fclose(f); }            // 下面每处出口都会执行
    if (!ready) { return; }          // return 前先执行 defer
    @defer { printf("second\n"); }   // 多个 defer：LIFO
    ...
}                                    // 块尾：先 second 后 fclose
```

语义：

- **循环体块**里的 defer **每轮迭代结束**执行——`continue`/`break` 时同样执行。
- `break`/`continue` 只触发"跳转处与最内层 loop/switch 之间"注册的 defer；注册在循环**外层**的 defer 不会被误触发（不双发）。
- 变量直接读写——**无捕获、无拷贝**（普通 C 作用域，刻意与 block 的捕获语义不同）。
- **ARC 顺序**：用户 defer 先于 ARC 注入的作用域末 release 执行，defer 里对象还活着（`dealloc` 最后打印）。
- `-eh checked` 零特判：defer pass 运行时 checked 的 throw 已是普通 `return`，自动被覆盖。

M1 限制（编译期强制）：`@defer` 必须直接位于块内；defer 体内不得出现 `return`/`break`/`continue`/`@throw`（`error: 'return' inside an '@defer' body is not supported (M1)`）。跨函数 `@throw`（sjlj longjmp 掠过中间帧）会跳过中间帧的 defer——与 ARC scope-end release 的既有已知限制相同，`-eh checked` 下无此问题。

实现：纯 desugar（`crates/defer`，pipeline Step 3.9——`-eh checked` 改写之后、ARC 之前）。codegen/checker/运行时看到的都是普通语句——下游零改动。Golden：`tests/golden/36_defer/`。

### `async T` —— async 方法修饰符

裸 `await` 单用有个软肋：头文件里看不出方法会挂起。`async` 修饰符把 async-ness 扶正为**签名级标记**：写在返回类型之前、直接修饰**业务类型**（`async int`），`.hje` 里可见。裸 `NPTask<T>` 返回则相反——只是返回任务对象的同步方法。

```jeti
@interface Fetcher : NPObject
- (async int)compute:(int)n;           // 会挂起，完成后给 int
+ (async void)runAll;                  // 入口方法
- (NPTask<int>)loadCached;             // 裸 NPTask<T> = 普通同步方法
- (int)plain:(int)n;                   // 不标 = 承诺不挂起
@end
```

体内的 `await` 决定事实，checker 双向对账：

| 声明 | 体内 | 判定 |
|------|------|------|
| `async T` | 有 `await` | ✅ |
| `async T` | 无 `await` | ✅ —— 合法（规则 1：`async` 是"允许挂起"的能力；缓存命中、碰巧同步的协议实现都正常） |
| 不标 | 有 `await` | **error** —— `'compute:' contains 'await' but is not declared 'async' — a suspending method must declare the async modifier` |
| 不标 | 无 `await` | ✅ |

- 修饰符是签名的一部分：`@interface` 与 `@implementation` 必须一致——`'async' modifier mismatch on 'compute:': the @interface and @implementation disagree — the modifier is part of the method signature` 报 error。仅头文件声明的 `@interface` 方法豁免（跨 TU 安全）。
- `async` 直接修饰业务类型（`async int` / `async NPString *` / `async void`）；旧的 `(async NPTask<T>)` 拼写报迁移 error（见 `tests/negative/async_nptask_*.jeti`）。
- 任务句柄在 v1 下不可被用户捕获：`NPTask<T> *t = [f compute];` 是 error（规则2 废除了"裸调用创建任务"）；句柄将只在显式 `+[NPTask spawn:…]` 诞生（阶段3，规划中）。`await` 仍是唯一取值通道，同一任务可多次 `await`（结果缓存）。
- `NPTask` 是保留类名。
- **一座桥，无入口位**：`[NPTask blockOn:async ^ void { … }]` 在调用点把 async 块驱动到完成（降为 `jeti_task_await`），是唯一同步→异步通道——`main`、同步方法、裸机同样适用。旧的语句位入口（`[f runAll];` 自动驱动、语句位 `[t start];` 驱动）已废除——语句位 `[t start];` 现在只入队，裸的任务产出语句是 checker error。`main` 退出处依旧不做兜底泵：ARC 的 scope-end release 比它更晚。

Golden：`tests/golden/37_async_modifier/`。

### 对象下标订阅（容器对象的 `a[0]`）

容器对象的 `recv[i]` 与 `recv[i] = v` 现在是语法糖。由 checker 改写——**类型感知**，判据是**符号表实证**（该类或其父类是否真声明了对应方法），不是"看起来像对象"：

| 源码 | 改写为 | 条件 |
|------|--------|------|
| `recv[i]` | `[recv objectAtIndex:i]` | 接收者的类声明了 `objectAtIndex:` |
| `recv[i] = v` | `[recv setObject:v atIndex:i]` | 类还声明了 `setObject:atIndex:` |

```jeti
NPArray *a = @[ @"x", @"y", @"z" ];
NPLog(@"%@", a[0]);            // → [a objectAtIndex:0]
NPMutableArray *m = [NPMutableArray array];
[m addObject:@"first"];
m[0] = @"hello";               // → [m setObject:@"hello" atIndex:0] —— 替换语义，不是追加
```

普通 C 零误伤：`int c[3]; c[1]`、`char *p; p[0]`、`const char *s; s[2]` 全部原样透传为 C 下标（探针验证，零误报）。改写落在 checker（parser 层拿不到变量类型，emit 阶段没有 vtable 元数据），下游 vtable 派发、nil 守卫、SEL 常量零特判。

字典下标（`d[@"k"]`）**有意不纳入**本改写：改写只映射到 `objectAtIndex:`，所以 `NPDictionary` 不声明 `objectForKeyedSubscript:`——声明它等于宣传一个会被发到错 selector 的写法。用 `[d objectForKey:@"k"]`。

### 泛型真检查（单态化 + 元素类型）

泛型容器**真单态化并做类型检查**。`NPArray<NPString *>` 与 `NPDictionary<NPString *, NPNumber *>` 会生成真正的特化 C（struct、vtable、类元数据、类型已代入的方法副本），checker 再把元素类型代入方法签名——所以元素类型是被强制的，不是被擦除的：

#### 真泛型跨 TU：写法像 Objective-C，实现不是轻量泛型

这里要特别区分两种模型。Objective-C 的 lightweight generics 主要是编译期注解：运行时仍然只有一个 `Factory` 类，`Factory<NPString *>` 不会生成一套新的方法和 ABI。Jeti 采用的是真泛型：每个具体参数列表都会生成独立的 C struct、方法副本、vtable、类元数据和调用 ABI；不做类型擦除，也不把特化对象退回成 `id`。

为了让写法仍然接近 Objective-C，跨 TU 编译时 jetic 会先扫描同一次构建中的全部 `.jeti` 输入，收集客户端实际使用的具体特化，再把需求传给各个 TU。包含泛型实现的 TU 负责生成特化；只有声明的客户端 TU 只引用同一个稳定的特化符号。因此不需要在库 TU 里写一个“假的变量”来触发实例化：

```jeti
// model.hje
@interface Factory<T> : NPObject
+ (T)make;
@end

// lib.jeti：真正拥有实现的 TU
#import "model.hje"
@implementation Factory
+ (T)make { return nil; }
@end

// main.jeti：客户端 TU
#import "model.hje"
int main(void) {
    NPString *s = [Factory<NPString *> make];
    return s == nil ? 0 : 1;
}
```

用一次命令把两个 TU 放进同一个构建：`jetic main.jeti lib.jeti -I . -o app`。泛型实现必须随输入源码或模块一起提供；只有 `.hje` 声明而没有实现体时，编译器无法凭空生成方法。预编译库可以携带一组已经生成的特化，但不能为发布后才出现、且库中没有实现模板的新参数类型发明方法体。若两个 TU 同时提供同一个特化，仍按 owner/strong-metadata 规则在链接期报重复定义，避免悄悄选出不一致的 ABI。

这就是 Jeti 的取舍：调用语法接近 Objective-C，泛型语义和代码生成更接近 C++ 模板；类型参数是真实类型，特化按使用生成，未使用的特化不会进入最终程序。

```jeti
NPMutableArray<NPString *> *m = [NPMutableArray array];
[m addObject:@"a"];
NPString *s = [m objectAtIndex:0];      // NPString *，不是 id

[m addObject:@42];                      // ✗ 报错：NPNumber* 放进 NPString* 容器
int bad = [m objectAtIndex:0];          // ✗ 报错：指针赋给标量
```

`@[...]` 与 `@{...}` 字面量在**所有元素同型**时会**推断**元素类型，所以 `NPArray<NPString *> *a = @[ @"x", @"y" ];` 无需标注；混合类型数组回退成裸 `NPArray`。

两种拼写并存：裸 `NPArray` 依然完整支持（零迁移），只是擦除成 `id`。把裸容器赋给特化变量是允许的，但会告警——此时元素类型未经验证：

```text
warning: assigning a bare 'NPArray *' to a specialization of it — the bare
container's element type is unchecked; add an explicit cast if the contents are known to match
```

`-Werror` 可升级拦截。`NPArray<A>` 与 `NPArray<B>` 之间互相赋值则不告警——与 ObjC lightweight generics 同样的宽松（你要回了 `id`，就给你 `id`）。

代价要说清楚：特化是编译期代码，不是免费的类型安全。同一程序改用泛型拼写而非裸拼写，生成的 C 多约 42 KB / +41%——全是重复的方法体与元数据，布局逐字节相同，故运行期收益为零。Golden：`tests/golden/40_nparray_generic/`。

### 泛型协议约束（`T : Proto`）

类型参数接受类级约束——ObjC 拼写（`T : id<Summable>`）、裸协议名（`T : Summable`）、类指针（`T : NSObject *`）三种都接受；存储与诊断统一用裸名：

```jeti
@protocol Greetable
- (NPString *)greeting;
@end

@interface Box<T : Greetable> : NPObject {
    T _value;
}
- (instancetype)initWithValue:(T)value;
@end

// 多参数：只有 V 被约束
@interface Pair<K, V : Comparable> : NPObject { ... }

// 子类必须重申继承的 bound（显式拼写——与共享 .hje 保留完整 ivar 布局
// 同一哲学），且不得弱化
@interface MutableBox<T : Greetable> : Box<T> { ... }
```

checker 在**显式特化的实例化点**（`Box<Dog *> *b = ...;`）强制约束：违约实报 error（一次报齐所有违约）。逃逸通道——`id`、`instancetype`、嵌套类型参数槽、forward 声明壳、无法解析的 bound 名——静默放行（漏报优于误报，与 checker 全局哲学一致）。裸拼写（`Box *`）永不触发：擦除兼容，今天的代码照常编译。bound 是纯编译期元数据——**零 codegen**，golden 输出逐字节不变；`-fno-checker` 关闭检查。不支持方法级约束（`where U : P`）。Golden：`tests/golden/46_generic_bounds/`。

### Jeti 语法宏（双轨 `#define`）

含 **jeti 语法**（`[recv msg]`、`@` 字面量、`^{}` block）的 `#define` 宏体此前原样透传给 C 编译器——直接语法错误。jetic 现在自行解析并在源级展开。纯 C 宏体照旧透传、由 C 编译器展开，行为零变化。

```jeti
#define TAG(o)      [o tag]                    // jeti 轨：jetic 展开
#define BUMP(o, n)  [o addTo:n times:1]
#define LOG(x)      NPLog(@"tag=%d", x)        // 宏体含 @literal
#define TWICE(x)    ((x) + (x))                // C 轨：clang 展开

int t = TAG(w);                                    // → [w tag]
BUMP(w, 3);
LOG(TAG(w));
```

展开语义遵循 ISO C §6.10.3（`crates/cpp` 独立实现，逐行对照 `clang -E` 交叉验证）：实参先完整展开再代入（`#`/`##` 操作数用 raw 文本）、`#param` 字符串化、`a ## b` 粘贴、`__VA_ARGS__` 逗号拼接、自递归冻结（蓝漆规则）、函数式宏裸名不展开、`\` 续行拼逻辑行。条件指令（`#if`/`#ifdef`/`#ifndef`/`#elif`/`#else`/`#endif`）也由 jetic 求值——`defined(X)` 操作数豁免展开、跳过的分组不定义宏、畸形条件指令报错而非静默吞文件。

限制（报清晰错误，不静默）：宏调用必须单行闭合（跨行用 `\` 续行）；宏体内不得出现 `_Pragma`。Golden：`tests/golden/38_macros/`。

### C99 指定初始化器

C99 指定初始化器的六种形态全部可用，包括 ObjC 的 C 子集从来不需要的那些：

```jeti
struct Point { int x; int y; };
struct Point p1 = { .x = 1, .y = 2 };      // 1. 完整指定
struct Point p2 = { .y = 5 };               // 2. 部分指定——未指定字段零填充
struct Point p3 = { .x = 1, 7 };           // 3. 指定与位置式混合

NPRange r = (NPRange){ .location = 3,      // 4. 复合字面量 + 指定
                        .length = 9 };

CGPoint pts[3] = { [0].wx = 1, [2].wy = 6 };  // 5. 数组元素

struct Outer o = { .in.a = 3, .tag = 9 }; // 6. 嵌套成员路径
```

位置式条目从最后一个被指定字段之后继续（形态 3 的 `7` 落在 `.y`），未指定字段零填充，`[1].wx` 这样的链式指定同样可用。它们按普通 C 初始化器原样透传——不引入新 IR。

### C99 `_Complex` 透传

`float _Complex` / `double _Complex` 的声明、typedef、形参全程原样透传，虚数后缀字面量（`2.0i`、`1e3j`）按 **raw 文本**发射——此前虚部被静默丢弃（`2.0i` → `2.0f`）。

```jeti
#include <complex.h>
typedef float _Complex cfloat;

double _Complex z = 1.0 + 2.0i;   // 实部 + 虚部混合
double _Complex w = 2.0i;         // 纯虚数字面量
cfloat f = 1.5;
printf("A=%.1f+%.1fi\n", creal(z), cimag(z));
```

已知限制：jeti checker 无复数类型推导（复数宽度窄化不告警，语义由生成的 C 交 C 编译器保证）。Golden：`tests/golden/39_complex/`。

---

## 编译与运行

### 命令行选项

```bash
jetic [options] <input.jeti>

模式:
  (无)              默认：转译 + 编译到二进制（需要 -o）
  run               转译 + 编译 + 运行（自动清理临时文件）

选项:
  -o <file>         指定输出文件（.o 输出对象文件，否则输出可执行文件）
  -I <dir>          添加头文件搜索路径
  -L <dir>          添加库搜索路径
  -v, --verbose     显示详细输出（包括 Clang 编译警告）
  --version         显示版本号
  --rewrite-jeti    只输出 C 代码（不编译）
  -fjeti-arc        启用 ARC（默认）
  -fno-jeti-arc     禁用 ARC（手动 MRC 模式）
  -fno-checker      跳过类型检查
  -eh <mode>        异常后端：checked（默认）或 legacy（别名 sjlj）
  -ffreestanding    裸机/freestanding 输出（无 libc、无 TLS）
  -backend <mode>   C 编译器后端：clang（默认）、portable、gcc
  -arch <target>    构建目标架构（如 -arch x86_64）
  -asm <file.s>     链接汇编文件（可重复）
  -gen-completions <shell>  生成 shell 补全脚本（zsh|bash|fish）
  -emit-bridge-header <file.h>  生成 C 桥接头，用于从 C 代码调用 Jeti 对象

引用计数追踪（调试辅助）:
  -trace-refcount              按源代码顺序打印每个存活对象的静态引用计数追踪
  -trace-max-iters <N>         追踪中模拟的循环迭代次数（默认 2）
  -trace-no-color              关闭追踪输出的颜色
```

### 构建系统集成

**cargo**:

```bash
cargo build
cargo test --workspace
```

---

## 代码示例

### 预编译 Foundation 库

不必在每个 TU 里内联 Foundation（`#import <Foundation/Foundation.jeti>`，自包含伞头），可以一次构建成静态库、各项目链接——单文件编译更快，实现只存一份：

```bash
./tools/build-foundation-lib.sh            # → target/foundation/libjetifoundation.a
```

脚本把每个 Foundation `.jeti` 作为**独立编译单元**转译（生成的 wrapper 前置完整声明面、再内联实现文本）、编译、归档。由于每个 `@implementation` 都落在其 TU 的主文件里，R2 ownership 自动把该类的元数据发射为 STRONG 符号——脚本 nm 校验全部九个，任何弱符号都会响亮失败。`-fstrong-metadata` 已不存在：ownership 由构造保证。

客户端随后只导入声明头：

```jeti
#import <Foundation/Foundation.hje>    // 纯声明——不内联任何实现

int main() {
    NPString *s = [NPString stringWithUTF8String:"hello"];
    NPLog(@"%@", s);
    return 0;
}
```

```bash
jetic app.jeti -I include -L target/foundation -ljetifoundation -o app   # 显式链接

# ……或者让 jetic 自己找到并链接库：
jetic app.jeti -o app
```

要点：

- **无需记任何旗标**——主文件持有 `@implementation` 自动为强；纯声明客户端保持弱，这正是正确的（库的真表在链接中胜出）。
- **自动链接**——jetic 找得到 `libjetifoundation.a`（二进制旁、`target/foundation`、`/opt/jeti/lib`、`/usr/local/lib/jeti` 或你的 `-L` 目录）就自动链。只对**纯声明客户端**生效：内联了 Foundation 实现（`Foundation.jeti`——直接或经导入的 `.hje` 传递）的 TU 会跳过，自包含程序与多 TU 构建永远见不到库的强 vtable。`-ffreestanding`、shared 模式、显式 `-ljetifoundation` 都会抑制自动链接。
- **`jeti_metaInit()`** 只对通过 `#import "*.jeti"` 获取实现的伞形构建（单 TU 构建、无任何 TU 持有元数据）**真正必需**。用预编译库——以及所有常规 `jetic` 工作流——元数据在加载期静态初始化，该调用是幂等空操作。
- 客户端重实现库类，对归档而言是标准 C 覆盖语义（库成员未被引用就保持休眠）——但未实现方法的槽位是 NULL，所以派发到的方法必须全部自己实现。

### Hello World

```jeti
#include <stdio.h>
#import <Foundation/Foundation.jeti>

@interface Greeter : NPObject
- (void)greet;
@end

@implementation Greeter
- (void)greet {
    printf("Hello, Jeti!\n");
}
@end

int main() {
    @autoreleasepool {
        Greeter *g = [[Greeter alloc] init];
        [g greet];
    }
    return 0;
}
```

### 多态

```jeti
@interface Animal : NPObject
- (void)speak;
@end

@interface Dog : Animal
@end

@interface Cat : Animal
@end

@implementation Animal
- (void)speak { printf("...\n"); }
@end

@implementation Dog
- (void)speak { printf("Woof!\n"); }
@end

@implementation Cat
- (void)speak { printf("Meow!\n"); }
@end

int main() {
    Animal *animals[2];
    animals[0] = [[Dog alloc] init];
    animals[1] = [[Cat alloc] init];
    for (int i = 0; i < 2; i++)
        [animals[i] speak];  // VTable 静态派发
    return 0;
}
```

### Block + ARC

```jeti
typedef void (^EventHandler)(int code, NPString *msg);

@interface Engine : NPObject
- (void)onEvent:(EventHandler)handler;
@end

int main() {
    @autoreleasepool {
        Engine *e = [[Engine alloc] init];
        int captured = 42;
        [e onEvent:^void(int code, NPString *msg) {
            printf("code=%d msg=%s captured=%d\n", code, msg, captured);
        }];
    }
    return 0;
}
```

### 静态泛型（Generics）

Jeti 通过**编译期单态化（monomorphization）**实现泛型——每个 `DataPack<QuantumToken *>` 都会生成独立的 C 结构体 `DataPack_QuantumToken_ptr`，类型参数被具体类型替换。没有类型擦除，没有装箱，没有运行时开销。

```jeti
@interface DataPack<T> : NPObject {
    @public
    int _count;
    T _storage[2];
}
- (void)pushItem:(T)item;
- (T)popItem;
@end

@implementation DataPack
- (void)pushItem:(T)item {
    if (_count < 2) {
        _storage[_count++] = item;
    }
}
- (T)popItem {
    if (_count > 0) {
        _count--;
        T item = _storage[_count];
        _storage[_count] = 0;
        return jeti_autorelease(item);
    }
    return 0;
}
@end

int main() {
    @autoreleasepool {
        // 每次实例化都会生成特化的 C 代码
        DataPack<QuantumToken *> *tokenPack = [[DataPack<QuantumToken *> alloc] init];
        DataPack<EncryptedMetric *> *metricPack = [[DataPack<EncryptedMetric *> alloc] init];
    }
    return 0;
}
```

**工作原理**：

- `DataPack<T>` 的泛型结构体不会被发射；只发射特化版本
- `DataPack<QuantumToken *>` → `struct DataPack_QuantumToken_ptr`，包含 `QuantumToken * _storage[2]`
- 方法会被克隆，返回类型、参数类型、方法体中的类型参数全部被替换
- VTable、元 VTable、类元数据均为每个实例化单独生成
- 类型名编码：`DataPack<QuantumToken *>` → `DataPack_QuantumToken_ptr`

**状态**：✅ 单类型参数完整实现。多参数（`<K, V>`）开发中。

---

## 设计原则

### 1. 静态 > 动态

ObjC 的运行时很强大，但我不想依赖它。把所有决策放在编译期，生成就是执行，没有意外。

- 方法派发 → VTable 偏移量
- 内存管理 → CFG 静态分析
- 协议一致性 → 编译期检查

### 2. 生成人能读的 C

Jeti 的"后端"是**人类可读的 C99**，不是 LLVM IR。这意味着：

- 可以用 Clang/LLDB 原生工具调试
- 生成的 C 可以审查、修改、嵌入到其他项目
- 没有 LLVM 后端绑定——Clang 能跑的地方 Jeti 就能跑

### 3. 渐进式

从一个类系统开始，慢慢加东西：

- ✅ 类/协议/类别/属性
- ✅ Block / @autoreleasepool
- ✅ 静态 ARC
- ✅ @selector / VTable 多态
- ✅ 异常处理（`@try`/`@catch`/`@finally`/`@throw`）——**默认后端是 `-eh checked`**（旗标 + 守卫降级，unwind-safe ARC：跨函数抛出会释放每一帧的 owned 局部；不用 `setjmp/longjmp`，故裸机同样可用）
  - `-eh legacy`（别名 `-eh sjlj`）切回旧的 `setjmp`/`longjmp` 后端。⚠️ 它的已知限制：跨函数抛出时**跨越作用域仍存活的对象会泄漏**（`longjmp` 跳过作用域末尾的 `jeti_release`）。该限制**不适用于默认后端**。
- ⏳ Foundation 标准库
- ⏳ 编译器自举

### 5. 可读性

生成的 C 代码应该像手写的 C 一样清晰：

- 使用 `struct` + `->` 访问 ivar
- 使用 `static const SEL` 常量
- 方法和变量命名一致且可预期
- 临时变量显式命名

---

## 路线图

### 阶段 1：基础设施 ✅

- [x] 词法分析器
- [x] 预处理器
- [x] 语法分析器
- [x] CST 验证与打印

### 阶段 2：语义分析 ✅

- [x] 符号表
- [x] 名称绑定
- [x] 类型检查
- [x] 属性展开
- [x] 协议一致性

### 阶段 3：VTable + 对象布局 ✅

- [x] VTable 布局
- [x] 对象内存布局
- [x] 类元数据

### 阶段 4：中间表示 ✅

- [x] Typed AST
- [x] CST → AST
- [x] CFG 构建

### 阶段 5：静态 ARC ✅

- [x] Ownership 推断
- [x] 局部 + 全局 ARC
- [x] Retain/Release 插入
- [x] ARC 验证

### 阶段 6：C99 代码生成 ✅

- [x] C99 AST
- [x] AST → C99 转换
- [x] 头文件生成
- [x] 编译器选项

### 阶段 7：运行时 ✅

- [x] 核心 retain/release/alloc/init
- [x] 自动释放池

### 阶段 8-10：进行中

- [ ] Foundation 标准库

- [x] Block 运行时

- [x] 弱引用

- [✅] 泛型（编译期单态化）

- [x] 异常处理

- [ ] 调试信息

- [x] VSCode/IDE 支持

---

## FAQ

### 它能用于生产环境吗？

还不能。但它是 **真实可用** 的 —— 它能编译、能运行，并且从一开始就是为成长而设计的。如果你觉得它的语法很对味，想给它贡献一下，那么非常欢迎。

### Jeti 能做什么？

写小游戏、写工具。项目里的贪吃蛇、Flappy Bird、太空射击、井字棋都是 Jeti 写的，跑在终端里。

### 和 ObjC 比少了什么？

- 没有 `objc_msgSend`——VTable 静态派发
- 没有运行时 Method Swizzling
- 没有 `forwardInvocation:`
- 选择器是编译期常量，不是运行时字符串

### 为什么用 C99 作为输出？

因为 C99 到处都能编译。生成人能读懂的 C，用 Clang 编译，用 lldb 调试。不需要绑定任何特定后端。

---

## 许可证

Jeti 采用 [Apache License 2.0](LICENSE)。

Copyright © 2026 3shine123

- 完整许可证文本：[`LICENSE`](LICENSE)；声明文件：[`NOTICE`](NOTICE)
- 用 Jeti 编译代码不给产物附加任何许可义务：`jetic` 生成的 C 文件及由其构建的二进制归你所有。
