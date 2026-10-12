# NPTask 异步设计定稿

> 状态：**设计定稿（2026-10-09 定案）；入口语义已实施**——hosted 语句位自动驱动已落地
> （`crates/async`：`drive_entry_call` / `lower_task_starts_expr(drive=true)`），golden 37 绿；
> Foundation `NPTask.jth/.jeti` 薄壳与单态化接线尚未实施（见 §实施计划）。本文取代 `NPAsync<T>` 声明标记旧案（见
> `doc/archive/agents-history-2026-10.md` "NPAsync<T> 声明标记"节，该案被本设计反转）。
> `@await` 的表达式位语法（`@` 前缀、状态机 desugar、C++20 式"体内自证"判定）沿用既有
> M1/M2 定案，不在本文重复；本文只定义**声明侧标记、任务类型与调度语义**的新一轮反转。
>
> 核心原则（用户定案原话）：**异步语义属于语言，调度策略属于运行时或库。**

## 背景与动机

### 三代方案的演进

| 代 | 形态 | 问题 |
|---|---|---|
| 第一代 | `async - (int)fetch:` 修饰符顶在 `-` 前 | 路线图否决：Swift 词、C++ 没有的位置，哪儿都不像 |
| 第二代 | 无标记，体内 `@await` 自证（C++20 风格）+ 入口自动 blocking | 签名撒谎：头文件看不出方法会挂起；传染性不可见 |
| 第三代 | `NPAsync<T>` 声明标记（2026-09-29 定案，已实现） | 纯编译期擦除：标记只对人和 checker 说话，生成 C/桥接头/DWARF 全不可见；任务无句柄，调用方被迫 `@await`，没有"先拿走、稍后等"的自由 |
| **第四代（本定稿）** | `async` 返回类型前修饰符 + 真类型 `NPTask<T>` | — |

### 第四代相对第三代净增的能力

1. **签名彻底诚实**：`NPTask<int>` 是真类型——生成 C、vtable 签名、DWARF、桥接头全部可见。第三代擦除设计做不到。
2. **任务是一等公民**：句柄可存储、传递、手动驱动（`start`/`@await`）。第三代调用点被强制挂起，无自由度。
3. **双形态可区分**：`(async NPTask<int>)`（会挂起，完成后给 int）vs `(NPTask<int>)`（同步方法，只返回任务对象）。擦除设计表达不了。
4. **语法位置对**：`(async NPTask<int>)` 把 async 放在签名内返回类型前——正是 Swift `func fetch() async -> Int` 的位置；第一代被否决的"顶在 `-` 前"不适用。

### 代价（相对第三代的账单）

第三代是"codegen/runtime/ARC 零改动"；第四代返回类型真变了：

- vtable 签名、multi-TU `__sig`、跨 TU 元数据规则、golden 输出全部受影响；
- `NPTask<T>` 走既有泛型单态化，类型键命名规则要写进铁律（§单态化）；
- `crates/async` 的 desugar 从"内联状态机"改为"填 task 结构 + 返回句柄"；
- NPAsync 全套设施（parser 保留名、对账表、变量位拒绝）拆除迁移（§拆除清单）。

## 跨语言对照（定案依据）

| 决策点 | C++20 | Rust | C# | Swift | JS | Kotlin | Jeti 定案 |
|---|---|---|---|---|---|---|---|
| 调用即执行？ | Lazy | Lazy | Eager | Eager | Eager | Eager（默认） | **Lazy** |
| await 句柄 | 单次（future::get） | 可多次 | ✅ | ✅（task.value） | ✅ | ✅ | **✅ 可多次** |
| 机制/调度分层 | 语言+薄类型 / 不管执行 | core Future / 生态 executor | 全内建 | 全内建 | 全内建 | 全内建 | **runtime 机制 / Foundation 壳 / 用户泵** |

规律：有内建托管调度的语言选 eager；执行控制交给使用者的（C++/Rust）选 lazy。Jeti 裸机没有
"后台"可依托，属后者。C++23 把 `<coroutine>` 列入 freestanding、C++ 标准库托管设施
（`<thread>/<future>`）hosted-only——印证"机制层零 OS 依赖 + 调度策略可替换"的分层在工业上成立。

## 语法

```objc
#import <Foundation/Foundation.jth>

@interface Fetcher : NPObject
- (async NPTask<int>)compute:(int)n;   // 异步方法：会挂起，完成后给 int
- (async NPTask<void>)run;             // void 变体：无结果任务，只等完成
- (NPTask<int>)loadCachedValue;        // 同步方法：只返回任务对象，承诺不挂起
- (int)plain:(int)n;                   // 普通同步方法
@end

@implementation Fetcher
- (async NPTask<int>)compute:(int)n {
    int raw = @await someOperation(n);  // 挂起点（沿用 M1/M2 的 @await 表达式）
    return raw * 2;
}
@end

int main(void) {
    Fetcher *f = [[Fetcher alloc] init];
    NPTask<void> *task = [f run];       // lazy：只创建，不执行
    [task start];                       // 语句位：hosted 下就地驱动到完成
    return 0;
}
```

| 规则 | 内容 |
|------|------|
| `async` 修饰符 | **正式定义为返回类型前的方法修饰符**，不是类型限定符：`(async NPTask<int>)`、`(async NPTask<void>)`。parser 在返回类型位识别裸关键字 `async`，要求其后恰为 `NPTask<T>`；`async int`、`async NPString *` 等一律 error（`'async' requires return type 'NPTask<T>'`）。interface 与 implementation 必须同时标或同时不标（签名一致性检查，沿用既有哲学） |
| `NPTask<T>` | 任务**句柄**的类型名（路线 A：**不是** Foundation 类，无单态化、无消息派发）；生成 C 统一 `NPTask *`，Foundation 侧只有 `NPTask.jth`/`NPTask.jeti` 薄别名壳 + 运行时 C API。`T` 允许 `int`/对象指针/`void`。变量、参数、ivar 位**合法**（与 NPAsync 旧案相反——任务句柄本来就是值） |
| 无 async 的 `NPTask<T>` 返回 | 合法同步方法，返回任务对象。其任务体**不得含 `@await`**：无 async 修饰却体内含 `@await` → **error**（修饰符是签名的一部分，撒谎直接 error——比 NPAsync 时代的 warning 更干净，因为现在签名有能力说真话） |
| async 方法体内无 `@await` | error：`'X' is declared 'async' but its body never suspends`（防修饰符撒谎；对齐旧案"必须真挂起"哲学） |
| 裸 `T` 返回 + 体内 `@await` | error：`'X' contains '@await' but is not declared 'async NPTask<T>'`（原 warning 升级——现在不是"忘了标"，是"签名在撒谎"） |
| 保留名 | 用户声明名为 `NPTask` 的类 → error（`'NPTask' is reserved`）。`async` 成关键字，用户标识符 `async` → error |

## 调用点语义

```objc
NPTask<int> *t1 = [f compute:21];   // 形态 A：裸调用 async 方法 → 创建任务（不执行）
int x = @await t1;                  // 形态 B：@await task 变量 → 未 start 则先 start，挂起等待
int y = @await [f compute:2];       // 形态 C：@await 调用表达式 → 糖：create + start + 挂起等待
```

| 规则 | 内容 |
|------|------|
| Lazy 启动 | 裸调用只创建任务并入待命态，**不执行**；`[task start]` 入就绪队列。依据 Kotlin `CoroutineStart.LAZY` 先例；忘 start 由 checker 静态拦截（§静态分析） |
| hosted 语句位入口 | **语句位就是入口信号**：语句位的 async 调用（`[f compute:2];`，结果被丢弃）在 desugar 时降为 `jeti_task_await([f compute:2])`；语句位 `[t start];` 同样降为 `jeti_task_await(t)`——两者都在**调用点**驱动到完成（receiver 此时必然存活）。被接住的调用（`NPTask<T> *t = [f compute:2];`）仍 lazy，由调用方 `@await` 或语句位 `[t start]` 驱动。`-ffreestanding` 一律不驱动（裸机 `main` 自己泵） |
| `@await` 统一词汇 | 操作数二选一：async 调用表达式（糖）或 `NPTask<T> *` 表达式（变量/ivar/返回值）。await 本身就是"我要结果"，对未 start 的 task 自动先 start——不需要用户写两行 |
| 多次 await | **合法**。任务只生产一次，但结果缓存在任务状态里，二次 `@await` 直接取缓存（C#/Swift/JS/Java 的 promise/future 盒子语义；单次性属于"生产动作"，不属于"观察结果"）。C++ `future::get()` 的单次是其类型设计产物，不采纳 |
| `@await` 非 task | `@await` 作用于非 `NPTask` 表达式 → error（`'@await' requires an async call or an 'NPTask'`） |
| 环等待 | task 体内 `@await` 自身句柄（直接或经引用环）→ 运行时检测 abort（`jeti_task_await` 入口查环）；checker 只拦直接自等 |
| `NPTask<void>` | `@await t` 合法（等完成，无值可取）；`[task start]` 同普通任务 |
| 跨 TU | `async` 修饰符是签名一部分：`.jth` 声明与 `.jeti` 实现必须一致，不一致 → 链接期由 `__sig` 捕获（返回类型进签名哈希）+ 编译期对账 error |

## 静态分析（checker 新增）

1. **未使用任务警告**：`NPTask` 创建后既未 `start` 也未 `@await`、也未逃逸（存 ivar/传参/返回）就离开作用域 → warning：`task created but never started or awaited`（lazy 语义下忘 start = 静默不执行，这是 lazy 方案唯一真实风险，必须静态兜住）。逃逸判定复用 ARC 静态分析既有路径。
2. **体内 await 对账**：上表三条 error（async 无 await / await 无 async / 无 async 修饰的 NPTask 方法含 await），在 `jeti_async::check_unit` 原对账表位置实现——它独占"体内是否含 @await"分析且跑在 desugar 之前。
3. **调度泵归属**：hosted 模式下"语句位"是唯一入口信号——语句位的 async 调用与语句位 `[t start];` 在 desugar 期降为 `jeti_task_await(...)`（`crates/async`：`drive_entry_call` 与 `lower_task_starts_expr(drive=true)`），在**调用点**驱动到完成；**`-ffreestanding` 下绝不隐式驱动**——见 §runtime 层。
   **明确不做"main 退出兜底泵"**：ARC 的 scope-end release 排在函数收尾，兜底泵必然晚于它——任务会在已释放的 receiver 上运行（实测 `jeti_async_state_runAll` EXC_BAD_ACCESS）。语句位驱动天然早于 release，是唯一安全的入口位置。

## runtime 机制层（`include/jeti/runtime.h` / `runtime.c` / `runtime_freestanding.c`）

语言（parser/checker/async crate）认 `async` 修饰符 + `NPTask<T>` 类型 + `@await`；**策略（单线程协作泵、未来多线程/优先级）全在本层可替换**。机制层零 libc 依赖，`runtime_freestanding.c` 同签名提供，裸机完整可用。

```c
/* 任务状态机由编译器 desugar 生成；runtime 只管生命周期与就绪队列。 */
typedef struct jeti_task jeti_task;

typedef enum {
    JETI_TASK_READY,     /* 已创建未 start（lazy 待命） */
    JETI_TASK_RUNNING,   /* 在就绪队列中 / 正在执行到下一挂起点 */
    JETI_TASK_SUSPENDED, /* 挂起，等外部事件标记 ready */
    JETI_TASK_DONE,      /* 完成，结果已缓存 */
    JETI_TASK_FAILED     /* 环等待等致命错误 */
} jeti_task_state;

/* alloc 注入点：默认 malloc/free；裸机可换静态池（C++ promise_type 自定义
 * operator new 的先例）。NULL 参数 = 回落默认。必须在第一次 task 创建前设置。 */
void jeti_task_set_allocator(void *(*alloc)(size_t), void (*free_fn)(void *));

jeti_task *jeti_task_create(void *frame, void (*step_fn)(jeti_task *)); /* OOM 返回 NULL */
int   jeti_task_start(jeti_task *t);   /* READY → 入就绪队列；已 start 则幂等 no-op */
void *jeti_task_await(jeti_task *t);   /* 挂起当前任务直至 t 完成；未 start 自动 start；
                                          结果缓存，可多次调用；查环，环则 abort */
void  jeti_task_mark_ready(jeti_task *t); /* 挂起点事件源（中断/DMA 回调）调用 */
void  jeti_sched_run(void);            /* 泵：跑完就绪队列中所有可推进任务后返回 */
```

| 约束 | 内容 |
|------|------|
| 单线程协作 | 就绪队列 = 单链表；无锁无线程原语。`jeti_sched_run` 跑到队列空即返回——泵不泵、何时泵归用户：hosted 由语句位入口自动驱动；裸机用户主循环 `while (1) { jeti_sched_run(); __WFI(); }` |
| hosted-only 自动驱动 | 语句位自动驱动**必须**挂在"hosted + 语句位"双条件下；`-ffreestanding` 下 async 链顶端只是普通函数，`main` 是用户的地盘。这是与 hosted 唯一的行为分叉，诊断文档必须显式标注。**不得**在 `main` 退出处补兜底泵（ARC release 在前，见 §静态分析 3） |
| 堆分配 | task 帧需堆。裸机现状已有堆（ARC 依赖 allocator），非新依赖；OOM → `create` 返回 NULL，`start`/`await` 入口 NULL 检查 abort。可用注入点换静态池/预分配 |
| 零 libc | `runtime_freestanding.c` 不引 `<stdlib.h>`/`<pthread.h>`；`<thread>/<future>` 类托管设施明确**不做**（C++ 标准库 hosted-only 的教训：future 绑条件变量即失去裸机） |
| 挂起事件源 | 裸机无阻塞 I/O；`@await` 的真实来源是中断/DMA 回调调 `jeti_task_mark_ready`。协作式单线程下无抢占 = 无数据竞争 |

## Foundation 壳层（`include/Foundation/NPTask.jth` / `NPTask.jeti`）——路线 A：薄别名

**`NPTask<T>` 不是 Foundation 类，而是任务句柄的类型名。** 语言侧 `async NPTask<int>`、
变量、参数、ivar 位合法；生成 C 里统一渲染为运行时句柄 `NPTask *`（`include/jeti/runtime.h`）——
**无单态化类**（不存在 `NPTask_int`）、无消息派发、不进 vtable。

- `NPTask.jth` 是声明面（说明 + 落点，**不声明任何符号**），`NPTask.jeti` 是空壳；伞头分别导入
  （声明伞头 `Foundation.jth` 导入 `.jth`；自包含伞头 `Foundation.jeti` 导入两个）。
- `tools/build-foundation-lib.sh` 按与跳过 `Foundation.jeti` 同一理由跳过 `NPTask.jeti`——没有 vtable 可验。
- `[t start]` / `@await` **不是消息发送**，desugar 期直接降为 `jeti_task_*` C API：
  `[t start];`（hosted 语句位）→ `jeti_task_await(t)`；`@await t` → `jeti_task_await(t)`（未 start 自动 start）；
  `@await [f compute:1]` → create + start + await。
- 机制层 C API 在 hosted 与裸机都完整可用——句柄本来就只是 C 类型，与「裸机不链 Foundation」无关。
- 注意 KVC 门控教训：新增 .jth 的 import 闭包改动后必须复跑 tests/ 全量基线。

> 存档（未采纳）：路线 B = `NPTask<T> : NPObject` 真泛型类 + 单态化类型键，壳持 `jeti_task *`、
> 暴露 `- (void)start`。不采纳原因：任务句柄是**值**，而裸机不链 Foundation——做成类会把
> `-ffreestanding` 下的 async 返回类型判死；且要把 vtable / `__sig` / multi-TU 元数据拖进 async
> 返回类型；类名 `NPTask` 还与运行时 `struct NPTask` 同名（须先做运行时 struct 改名）。
> 复活条件：需要把任务放进 NPArray 等集合，或需要反射 / 动态派发。

## 实施计划（四阶段，每阶段独立可验证）

### 阶段 A：parser + AST/CST（语法落地）

- 返回类型位识别 `(async NPTask<...>)`：`async` 成保留关键字（用户标识符冲突 → error）；要求后随恰为 `NPTask<单 type_arg>`，否则 error。
- `CstDeclData::Method`/`Function` 的 `async_marker: bool` 复用（NPAsync 旧案已铺好，字段保留、语义从"标记解包"改为"修饰符"）；AST 方法签名携带 `async: bool`。
- parser 教训回炉：consume 失败不 advance；批量改动不用行号定位。
- 验收：新 parser 单测 + `tests/negative/`（async 非 NPTask、async 帧数错、保留名冲突）。

### 阶段 B：checker + jeti_async 对账（语义落地）

- `jeti_async::check_unit` 对账表三条 error（§语法表）+ 跨 TU interface/impl 修饰符一致；warnings 出口沿用 NPAsync 案已加的通道。
- `NPTask` 保留名检查；`@await` 操作数类型检查（async 调用 / `NPTask<T> *`，否则 error）。
- 未使用任务警告（逃逸判定复用 ARC 静态分析路径）。
- 验收：`tests/negative/` 四例（async 无 await / await 无 async / 未 start 弃任务 / @await 非 task）+ golden 逐字节核对既有 async 用例的迁移。

### 阶段 C：runtime 机制层（C API）

- `runtime.h`/`runtime.c`/`runtime_freestanding.c` 实现 §runtime 层 API（状态机 struct、就绪队列、注入分配器、环检测）。
- 验收：C 层单测（create/start/await/mark_ready/环 abort）；baremetal stress 跑通 `jeti_sched_run` 主循环形态；hosted 与 freestanding 双编译。

### 阶段 D：codegen + async crate + Foundation（接线落地）

- `crates/async` desugar 改造：async 方法编译为 step_fn 状态机（活过 await 的局部变量提升进 task 帧，释放结算归 ARC 汇合点——M3 既有思路）；裸调用 → `jeti_task_create`（lazy，不 start）；`@await task` → `jeti_task_await`；`@await 调用` → create+start+await 糖。
- 返回类型从 `T` 改为 `NPTask<T>*`：vtable、`__sig`（返回类型进签名哈希）、multi-TU 元数据联动；`-emit-bridge-header` 对 async 方法特判（跳过或按 task 签名发 wrapper）。
- Foundation 增 `NPTask.jth`/`NPTask.jeti` **薄别名壳**（路线 A，非单态化类——见 §Foundation 壳层）；改动后复跑 tests/ 全量。
- hosted 语句位入口自动驱动保留（hosted-only；无 main 退出兜底泵）；裸机不泵。
- 验收：`tests/golden/` 既有 async golden 更新 + 新增 lazy/双 await/NPTask 变量传递用例；`./test_all.sh`、`multi_tu`、`cargo test --workspace` 全绿。

## NPAsync 拆除清单（阶段 B/D 同步执行）

| 触点 | 动作 |
|------|------|
| parser `NPAsync` 保留名 + 返回类型位解包 | 删；换 `async` 关键字识别（阶段 A） |
| checker 变量/参数位拒绝 NPAsync | 删（变量位合法是本设计反转点） |
| `check_unit` NPAsync 对账表 + warning | 改写为 async 修饰符对账（阶段 B，位置不变） |
| archive 文档 NPAsync 节 | 标注"已被 NPTask 定案反转（2026-10-09）→ doc/async_nptask_plan.md"，按惯例保留原文 |
| `doc/architecture.md`、`ROADMAP.md` async 节 | 阶段 D 完成后同步更新 |
| golden 既有 NPAsync 用例 | 阶段 D 改写为 async 修饰符形态，逐字节重录 |

## 非目标

- 不做多线程调度器（策略层未来工作，机制层已预留）；不做取消（cancel）语义，首版任务不可取消；不做表达式级单步调试（归属 source_locations 计划阶段 5）。
- 桥接头对 async 方法的完整 wrapper 支持留待需要时做（M1 限制沿用）。
