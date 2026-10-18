# freestanding_features.jeti — 裸机语法独立性矩阵（2026-10-18 测量）

回答"脱离 Foundation/libc，语言本身能走多远"。探针：
`tests/stress/baremetal/freestanding_features.jeti`（host 构建、运行到 PASS），
与 ARM64 交叉构建的 `baremetal_test.jeti` 互为补充。

```sh
jetic -rewrite-jeti -ffreestanding -o /tmp/fsf.c tests/stress/baremetal/freestanding_features.jeti
clang -I include -include jeti/runtime.h -D_FORTIFY_SOURCE=0 \
    -Wno-unused-variable -c /tmp/fsf.c -o /tmp/fsf.o
clang -I include -D_FORTIFY_SOURCE=0 -c include/jeti/runtime_freestanding.c -o /tmp/fsfrt.o
clang /tmp/fsf.o /tmp/fsfrt.o tests/stress/baremetal/helpers.c -o /tmp/fsf -D_FORTIFY_SOURCE=0 -I include
/tmp/fsf   # exit 0
```

## ✓ freestanding 下完整可用

| 特性 | 说明 |
|------|------|
| 类/继承/多态 | `@interface Derived : Base`，基类链任意深，`[super msg]` 经父类 vtable 实例分派 |
| 自管 alloc/init | 每条根链自己声明 `+ (id)alloc { return jeti_alloc(self); }`、`- (id)init { return self; }`（bump allocator 由 runtime_freestanding.c 提供） |
| @namespace | 全名前缀 + `Class<T>` 实例化拼写 |
| 泛型（真单态化） | `Box<int> *b = [[Box<int>] alloc] initWithValue:…]`；每个实例化有独立 struct/vtable/克隆函数，`T` 按位替换。**值类型 T 的读取**（`[box value]` 返回 `int`）经直呼克隆函数（sentinel 门控：模板返回类型带 `/*T*/` 且自身实现才直呼，其余走统一 vtable 保持继承语义） |
| blocks | 字面量、byref 捕获（`__block`） |
| @defer | LIFO 退出（早期 return 也执行） |
| @autoreleasepool / @synchronized | runtime_freestanding.c 提供池与锁 |
| @try/@catch/@finally + **用户异常类** | `NPException` 是 Foundation——freestanding 抛/接自己的根类对象（探针用 `Cell`） |
| async/await + NPTask blockOn | `+ (async void) entry` + `[NPTask blockOn:async ^ void { await […]; }]`（canonical 模式，runtime_freestanding.c 提供任务 API） |
| C 结构体/联合/枚举、typedef、函数指针、static/inline | 文件级定义照常 |
| inline asm | `asm volatile(...)` 原样透传 |
| @selector / SEL、SEL.name/hash | intern 表由 runtime 提供 |
| 前向声明 extern C 函数 | `extern void kputs(const char *);`（控制台契约由调用方提供） |

## ✗ 明确不可用（设计使然或已知缺口）

| 特性 | 原因 |
|------|------|
| Foundation 类（NPString/NPArray/NPLog/@"" 字面量对象化…） | freestanding 不链 Foundation 库；`@""` 退化为 C 字符串 |
| `[super init]`（根链顶端） | `jeti_root` 无 init vtable 槽——所有 freestanding 代码手写 `- (id)init { return self; }` |
| 方法体内 **struct 定义** / **typedef 定义** | codegen 丢弃函数体内的 `struct X {…};` / `typedef …;`（生成的 C 只剩使用点）——记录缺口，定义移文件级可绕开 |
| `struct X {…} p;` 定义+声明一条语句 | parser 不接受该组合（声明落在 `}` 后即断）——记录缺口 |

## 修复记录（2026-10-18）

- `[super msg]` 从**类方法**发出时 meta-vtable 分派正确（`(&META_inst)->msg`）。
- `[[X alloc] init]` 的分路发射要求 `+alloc` 已声明：vtable 找不到 `alloc`
  时旧发射降级为 `ClassName->alloc`（非法 C，`->` 作用在类型名上）。
  现状仍是降级路径（clang 报错兜底），但 freestanding 纪律是**每类声明
  +alloc**（golden/25 的模式），探针已按此校准。
- 点对象 const 修复让 `(*p)++` 穿 `const unsigned char *p` 的既有非法
  测试源暴露——`baremetal_test.jeti` 已改为可写副本。
