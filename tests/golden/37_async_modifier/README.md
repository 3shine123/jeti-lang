# 37 — `async` 方法修饰符（软关键字设计）

> 设计正典：`docs/plans/async_soft_keywords_plan.md`（`async` 属签名，修饰**业务类型**）。
> 源码写 `- (async int)compute:(int)n`，编译器内部归一化为任务句柄 `NPTask<int> *`；
> 裸 `(NPTask<int>)`（不带 async）仍是普通同步方法的合法返回类型。

覆盖点：

- `- (async int)compute:(int)n` —— 体内 `return raw * 2` **直接返回 `T`**，`await` 解包得 `T`。
- `int x = await [self compute:21];` —— 形态 C（`await [调用]`）解包到 `T`。
- `- (NPTask<int>)loadCachedValue` —— **不带 `async`** 的普通同步方法，只是返回一个任务对象；
  `NPTask<int> *t = [self loadCachedValue];` 是形态 A（lazy：只创建，不执行）。
- `int y = await t;` —— 形态 B（`await` 一个任务变量）。
- `int z = [self plain:5];` —— 与任务无关的普通同步调用。
- `NPTask<void> *task = [f runAll]; [task start];` —— 语句位驱动：hosted 下就地驱动到完成
  （等价 `jeti_task_await(task)`），任务跑完时 receiver `f` 尚未被 ARC 释放。

| 文件 | 期望 |
|------|------|
| `async_modifier_test.jeti` | 编译通过，stdout 为 `runAll x=42 y=84 z=6` |
| `async_modifier_test.out` | 上面那行 stdout 的人工基线 |
| `async_modifier_test.c` | 生成 C 基线（人工参照；`test_all.py` 对该目录只判 rc） |

```bash
./target/debug/jetic run tests/golden/37_async_modifier/async_modifier_test.jeti
```

## 期望输出

```text
runAll x=42 y=84 z=6
```

## hosted 入口两条规则（定案）

调度策略属运行时，语言侧只认"语句位"这一个入口信号：

1. **语句位 async 调用**（`[f runAll];`，结果被丢弃）→ `jeti_task_await([f runAll])`：
   未 `start` 的自动 `start`，并在**调用点**泵到完成。
2. **语句位 `[t start];`** → `jeti_task_await(t)`：同样在调用点驱动到完成。

被接住的调用仍是 lazy（形态 A）：`NPTask<T> *t = [f run];` 只拿句柄，由调用方 `await` 或语句位 `[t start]` 驱动。
`-ffreestanding` 下两条一律不发射（裸机 `main` 自己泵：持有句柄 → `[t start]` → `jeti_sched_run()`）。

**为什么不在 `main` 退出处兜底泵一次**：ARC 的 scope-end release 排在函数收尾，兜底泵必然晚于它——
任务会在 receiver 已释放的堆内存上运行（实测 `jeti_async_state_runAll` EXC_BAD_ACCESS）。
语句位驱动天然发生在 release 之前，是唯一安全的入口点。

## 实现注记

- 状态机入口参数用**保留名** `__jeti_task`（不是 `t`）：用户局部 `NPTask<int> *t = …` 曾把参数 `t` 遮蔽，
  展开出的 `t->self_obj` 读到未初始化的新局部 → SIGSEGV（本目录是回归源）。
- 负例在 `tests/negative/async_nptask_*.jeti`（旧拼法 `async NPTask<…>` 迁移错误 / 上下文关键字冲突）。
