# Jeti → C 代码生成规范（CodeGen Spec）

> 版本：2026-08-08  
> 适用范围：`jetic` 编译器后端（`crates/codegen/src/codegen.rs`）

---

## 1. 总体原则

1. **生成的人类可读 C 代码是产物之一**，不是中间垃圾。变量名、缩进、结构必须清晰。

2. **后端选择驱动代码生成**。`--backend` 选项决定生成的 C 代码使用哪些编译器扩展：

   | 后端 | Block 字面量 | `__attribute__` | `__weak` cleanups | 编译目标 |
   |---|---|---|---|---|
   | `clang`（默认） | `^` 语法透传 | 全透传 | `__attribute__((cleanup))` | clang 编译器 |
   | `gcc` | jetic 展开为 struct+函数 | 全透传 | `__attribute__((cleanup))` | gcc 编译器 |
   | `portable` | 展开（同 gcc，gcc∩clang 交集内） | 仅 Common 集 | 退化为 `__unsafe_unretained` | gcc 与 clang 都能编译的 C |

3. **所有后端的行为必须一致**。无论是 clang 的 `^block` 透传还是 gcc/portable 的 struct 展开，Block 的语义（捕获、调用、`__block` 变量）完全相同。

4. **portable 模式 = gcc 与 clang 的语法交集**：生成代码必须能同时通过 `clang -std=c99 -pedantic -Werror` 和 `gcc -std=c99 -pedantic -Werror`，不使用两个前台中任一不认识的结构。Block 的 struct+invoke 展开只采用 struct、函数指针、静态函数等交集内的标准 C 构造，因此 portable 与 gcc 共用同一套展开逻辑，**不在 portable 模式下对 Block 报错**（若某插入代码依赖 clang/gcc 专属扩展，则不受此约束）。

---

## 2. 命名约定

### 2.1 命名空间

- 跨命名空间的类型名使用**双下划线（`__`）** 连接 namespace 段：`Engine__Math__Vector2D`。
- 宏、常量、临时符号使用 **`__jeti_` 前缀**。

### 2.2 方法名

- 方法名使用 **`ClassName_selector`** 格式：`Person_init`、`Person_setName_`。
- 选择器中的 `:` 替换为 `_`。
- 类方法同理：`Person_getDefaultGreeting`。

### 2.3 临时符号

- Block 展开函数：`__jeti_block_invoke_N`（N 为全局递增 ID）。
- Block 展开结构体：`__jeti_block_layout_N`。
- `__block` 变量结构体：`__jeti_byref_varName`。

---

## 3. 类型生成规则

### 3.1 基础类型映射

| Jeti 类型 | C 类型 |
|-----------|--------|
| `int` | `int` |
| `float` | `float` |
| `double` | `double` |
| `char` | `char` |
| `BOOL` | `_Bool` |
| `id` | `NPObject *` |
| `SEL` | `SEL`（结构体） |
| `instancetype` | `NPObject *` |
| `T`（泛型参数） | 具体类型（monomorphization） |

### 3.2 对象指针

- 所有对象类型生成 `ClassName *`，不省略 `struct` 关键字（除非 typedef 后）。
- 类元数据访问使用 `jeti_ClassName_class` 全局变量。

### 3.3 函数指针与 Block 类型

| 源码 | `--backend=clang` | `--backend=gcc` / `--backend=portable` |
   |---|---|---|
   | `int (^)(int)` | `int (^)(int)` | `int (*)(struct __jeti_block_layout_N *, int)` |
   | `void (^)(NPString *)` | `void (^)(NPString *)` | `void (*)(struct __jeti_block_layout_N *, NPString *)` |

---

## 4. 对象布局

### 4.1 NPObject 基础头

```c
struct NPObject {
    struct NPClass *isa;     // 类元数据指针
    uint32_t retain_count;   // 引用计数
};
```

### 4.2 子类

```c
struct Person {
    struct NPObject base;    // 父类字段必须为第一个成员
    int age;
    NPString *name;
};
```

### 4.3 类元数据（NPClass）

```c
typedef struct NPClass {
    const char *name;
    struct NPClass *superclass;
    size_t instance_size;
    struct jeti_vtable *vtable;
    struct jeti_meta_vtable *meta_vtable;
    struct jeti_vtable *meta_vtable_inst;
    void (*dealloc)(NPObject *, SEL);
} NPClass;
```

---

## 5. VTable 与消息派发

### 5.1 Uniform VTable

所有类的实例方法共享同一个函数指针枚举：

```c
enum jeti_vtable_index {
    jeti_sel_release = 0,
    jeti_sel_retain = 1,
    jeti_sel_init = 2,
    jeti_sel_dealloc = 3,
    // ... 全局排序
};
struct jeti_vtable {
    void (*methods[N])();
};
```

### 5.2 消息发送

```c
// [obj method:arg]
((RT(*)(id, SEL, int))((struct jeti_vtable *)obj->isa->vtable)->methods[INDEX])
    (obj, _cmd, arg)
```

### 5.3 类方法

```c
// [ClassName method:arg]
((RT(*)(NPClass *, SEL, int))((struct jeti_vtable *)meta_vtable_inst->methods[INDEX]))
    (meta_vtable_inst, _cmd, arg)
```

---

## 6. 继承与 super

### 6.1 父类方法调用

```c
// [super init]
struct jeti_vtable *super_vt = ((struct NPClass *)((NPObject *)self)->isa)->superclass->vtable;
((RT(*)(id, SEL))super_vt->methods[INDEX])(self, _cmd);
```

### 6.2 子类对象布局

子类结构体以 `struct ParentType base` 开头，保证指针可安全转换：

```c
struct Person {
    struct NPObject base;
    int age;
};
struct Employee {
    struct Person base;
    char *department;
};
```

---

## 7. 属性实现

### 7.1 @synthesize

```c
// @property int age;
// @synthesize age = _age;
int _age;

- (int)age {
    return _age;
}
- (void)setAge:(int)value {
    _age = value;
}
```

### 7.2 @dynamic

- 不生成 getter/setter，由 `@implementation` 提供。

---

## 8. Block 生成规则

### 8.1 `--backend=clang` 模式（默认）

Block 字面量直接透传为 `^return_type(params) { body }`，由 clang 的 `-fblocks` 编译。

```c
// ^int(int x) { return x * 2; }
^int(int x) {
    return (x * 2);
}
```

`__block` 变量生成 `struct __jeti_byref_varName` 结构体：

```c
struct __jeti_byref_counter {
    void *__isa;
    struct __jeti_byref_counter *__forwarding;
    int __flags;
    int __value;
};
struct __jeti_byref_counter counter = {
    .__forwarding = &counter,
    .__flags = 0,
    .__value = 0,
};
```

### 8.2 `--backend=gcc` / `--backend=portable` 模式（jetic 展开）

Block 字面量**必须**展开为静态函数 + 栈上结构体，展开形式只采用 struct、函数指针、静态函数等 gcc∩clang 交集内的构造。展开后的语义必须与 clang 的 `-fblocks` 完全一致，包括：

- `__block` 变量的捕获与修改
- Block 的调用语法
- 返回值传递

#### 8.2.1 展开模式总览

Block 字面量展开为三部分：

```
源代码:  ^int(int x) { return x * 2; }
         ↓
展开 1:  static int __jeti_block_invoke_N(struct __jeti_block_layout_N *cself, int x);
展开 2:  struct __jeti_block_layout_N { void *isa; int flags; void *reserved; int (*invoke)(...); /* captures */ };
展开 3:  使用处初始化 struct __jeti_block_layout_N 并调用 invoke
```

#### 8.2.2 展开结构体

```c
// Block 结构体定义（每个 Block 字面量一个）
struct __jeti_block_layout_N {
    void *isa;                                             // &_NSConcreteStackBlock
    int flags;                                             // BLOCK_HAS_COPY_DISPOSE (1<<25) 等
    int reserved;                                          // 0
    int (*invoke)(struct __jeti_block_layout_N *cself,     // 函数指针
                  /* 参数列表 */);
    /* 捕获的 __block 变量指针，按声明顺序排列 */
};
```

#### 8.2.3 Invoke 函数

```c
static int __jeti_block_invoke_N(
    struct __jeti_block_layout_N *cself,
    int x)
{
    // 通过 cself 访问捕获的 __block 变量
    // cself->counter 指向 struct __jeti_byref_counter
    // 通过 __forwarding 间接访问: cself->counter->__forwarding->__value
    return (x * 2);
}
```

#### 8.2.4 使用处初始化

```c
// 使用处的代码
struct __jeti_block_layout_N __jeti_blk_N = {
    .isa = &_NSConcreteStackBlock,
    .flags = 0,
    .reserved = 0,
    .invoke = __jeti_block_invoke_N,
    /* 捕获的 __block 变量指针赋值 */
};
// 调用: blk(x)
((int (*)(struct __jeti_block_layout_N *, int))__jeti_blk_N.invoke)(&__jeti_blk_N, x);
```

#### 8.2.5 `__block` 变量展开

`__block` 变量必须展开为 `struct __jeti_byref_name`，与 clang 模式的布局保持一致：

```c
// __block int counter = 0;
struct __jeti_byref_counter {
    void *__isa;
    struct __jeti_byref_counter *__forwarding;
    int __flags;
    int __value;
};
// 初始化
struct __jeti_byref_counter __jeti_byref_counter = {
    .__forwarding = &__jeti_byref_counter,
    .__flags = 0,
    .__value = 0,
};
```

Block 展开的 invoke 函数通过 `cself->counter` 访问该结构体，并通过 `cself->counter->__forwarding->__value` 读写值（`__block` 变量可能被拷贝到堆上，`__forwarding` 保证栈上与堆上的指针最终指向堆上的值）。

#### 8.2.6 捕获的 `__block` 变量传递

展开结构体中，捕获的 `__block` 变量存储为**指向 byref 结构体的指针**：

```c
struct __jeti_block_layout_N {
    void *isa;
    int flags;
    int reserved;
    int (*invoke)(struct __jeti_block_layout_N *, int, int);
    struct __jeti_byref_counter *counter;  // 捕获的 __block 变量
    struct __jeti_byref_other *other;      // 捕获的 __block 变量
};
```

构造时赋值：

```c
struct __jeti_block_layout_N __jeti_blk_N = {
    .isa = &_NSConcreteStackBlock,
    .flags = 0,
    .invoke = __jeti_block_invoke_N,
    .counter = &__jeti_byref_counter,
    .other = &__jeti_byref_other,
};
```

### 8.3 `--backend=portable` 模式

Block 字面量**与 gcc 后端共用同一套 struct+invoke 展开**（见 §8.2）——展开形式只使用 struct、函数指针、静态函数等 gcc∩clang 交集内的标准 C 构造，因此可同时通过 `clang -std=c99 -pedantic` 与 `gcc -std=c99 -pedantic`。

⚠️ 例外：若 Block 体或捕获中使用了 clang/gcc 专属扩展（如 `__attribute__` 的专有项），则按 §1 的 `__attribute__` 后端规则处理（portable 只允许 Common 集），不因 Block 本身报错。

---

## 9. 泛型（Monomorphization）规则

- 泛型类（如 `DataPack<T>`）**不生成**通用结构体，只为每个实例化生成特化版本。
- 特化结构体命名：`DataPack_QuantumToken_ptr`。
- 方法同样克隆并替换所有 `T` 为具体类型。
- VTable、Meta VTable、类元数据均按特化版本独立生成。

---

## 10. 内存管理相关生成

### 10.1 MRC 模式（`-fno-jeti-arc`）

- 用户手动写 `retain` / `release` / `autorelease`，代码生成器**原样转译**为对应的 C 函数调用。

### 10.2 ARC 模式（默认）

- 编译器在 AST/CFG 层分析所有权，**自动插入** `jeti_retain()` / `jeti_release()`。
- 生成的 C 代码中**不应出现**用户手写的 `retain` / `release`（除非在 `-fno-jeti-arc` 模式）。
- `dealloc` 方法中 `[super dealloc]` 必须保留，且生成为父类 vtable 的 `dealloc` 调用。

### 10.3 `dealloc` 调用链

- `jeti_release` 在 `retain_count` 到达 0 时，**必须**先通过 `obj->isa->dealloc` 调用 `dealloc`，再 `free(obj)`。
- `dealloc` 方法体中 `[super dealloc]` 必须保留，生成为父类 vtable 的 `dealloc` 调用。
- `dealloc` 调用期间**不再**走 `jeti_release` 路径，避免死循环/双重释放。
- `NPClass` 结构体新增 `void (*dealloc)(NPObject *, SEL)` 字段，由 `jeti_meta_init()` 填充。

### 10.4 Autorelease Pool 实现约束

- `jeti_autoreleasepool_push()` / `pop()` 必须管理一个对象数组栈。
- `jeti_autorelease(obj)` 将 `obj` 加入当前栈顶 pool。
- `pop()` 时遍历 pool 内所有对象，逐个调用 `jeti_release()`。
- 裸机模式下不使用 `__thread`，使用普通全局变量（单核假设）。

### 10.5 `__weak` 变量（按 backend 区别）

| 模式 | 行为 |
|---|---|
| `--backend=clang` | `__attribute__((cleanup(jeti_weak_auto_cleanup)))` + `jeti_weak_register` |
| `--backend=gcc` | 同上（`__attribute__((cleanup))` 在 GCC 中也支持） |
| `--backend=portable` | 退化为 `__unsafe_unretained`（无 zeroing 语义），不生成任何清理代码 |

---

## 11. 禁止事项清单

| # | 禁止行为 | 原因 |
|---|---|---|
| 1 | 将方法签名中的 `NPObject *self` 改为具体子类指针 | 破坏 Uniform VTable 和对象头一致性 |
| 2 | 将 `struct NPClass *isa` 改为 `NPClass *isa` | 类型不一致，依赖 typedef 可见性 |
| 3 | 生成重复的 `#include` | 冗余，可能引发宏重定义问题 |
| 4 | Block typedef 不带命名空间前缀 | 全局命名空间污染 |
| 5 | `NPObject *` → 子类指针赋值时不加显式强制转换 | `clang -Werror` 报 incompatible pointer types |
| 6 | 为每个类生成独立的 vtable struct | 破坏 Uniform VTable 设计 |
| 7 | `@selector` 直接输出 hash 值作为无符号整数 | `SEL` 是结构体，不是 `unsigned` |
| 8 | 在 `@try` body 中生成空块或 `/* stub */` | 异常处理必须实际生效 |
| 9 | `jeti_release` 直接 `free(obj)` 而不调用 `dealloc` | 导致 ivar 内存泄漏 |
| 10 | `@autoreleasepool` 生成空壳 push/pop 而不管理对象 | 对象永不释放 |
| 11 | `.hje` 头文件中包含 `@implementation` | 多重导入导致链接期符号重复定义 |
| 12 | `--backend=portable` 模式下发出 `^` Block 字面量 | `^` 是 clang 专属语法（`-fblocks`），portable 必须展开为 struct 形式 |
| 13 | gcc 后端发出 `^` block 语法 | GCC 不支持 `-fblocks`，必须展开为 struct |
| 14 | clang 后端展开 Block 为 struct | 浪费性能，clang 自己的 `-fblocks` 已经处理了 |

---

## 12. 验收测试用例

修改 codegen 后，必须验证以下文件能正确转译并通过对应后端的编译：

### 12.1 clang 后端

```
clang -std=c99 -fblocks -Werror <output.c>
```

1. `absolute_qualified_hardcore_test.jeti` —— 深层命名空间、跨空间继承、Block、@try/@catch
2. `grand_integrated_epic_test.jeti` —— 命名空间、VTable 多态、属性、@autoreleasepool
3. `log_analyzer.jeti` —— 协议、泛型容器、双 Block 回调、统计报告

### 12.2 gcc 后端

```
gcc -std=c99 -Werror <output.c>
```

1. 同上三组测试文件，但 Block 部分必须展开为 struct 模式
2. `mega_fusion/` —— 多文件、C/asm 混合、Block、@try/@catch

### 12.3 portable 后端

```
clang -std=c99 -pedantic -Werror <output.c>
gcc -std=c99 -pedantic -Werror <output.c>
```

1. 含 Block 的测试文件：Block 展开为 struct 模式，**必须同时通过 clang 与 gcc 的 `-std=c99 -pedantic -Werror`**（验证展开形式在 gcc∩clang 交集内）。
2. 不含 Block 的测试文件全部通过。