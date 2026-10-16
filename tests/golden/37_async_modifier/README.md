# 37 — `async` 方法修饰符（软关键字设计 × Async Semantics v1）

> 设计正典：`docs/plans/async_semantics_v1.md`（`async` 属签名，修饰**业务类型**）。
> 源码写 `- (async int)compute:(int)n`，编译器内部归一化为任务句柄 `NPTask<int> *`；
> 裸 `(NPTask<int>)`（不带 async）仍是普通同步方法的合法返回类型。

覆盖点：

- `- (async int)compute:(int)n` —— 体内 `return raw * 2` **直接返回 `T`**，且**无
  `await` 也合法**（v1 规则 1：`async` = 允许挂起的能力，缓存命中形态；旧的假
  `await n` 脚手架正是 rule-6 lint 存在要抓的东西）。
- `int x = await [self compute:21];` —— `await [调用]` 解包到 `T`。
- `int y = await [self compute:42];` —— 第二次 await 同一 async 方法（结果独立）。
  旧模型的"句柄捕获（形态 A）+ `await t`（形态 B）"已随规则 2 废除——句柄将只来自
  显式 spawn（阶段 3）。
- `int z = [self plain:5];` —— 与任务无关的普通同步调用。
- `[NPTask blockOn:async ^ void { … }]` —— main 里唯一的同步 → 异步桥（规则 3）；
  旧的 `NPTask<void> *task = [f runAll]; [task start];` 语句位驱动形态已废除。

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

## 入口规则（v1 定案）

- **语句位 async 调用与语句位 `[t start]` 驱动已废除**（规则 2）：裸 async 调用是
  checker error， lowering 不再按调用位置改变行为。
- **唯一同步 → 异步桥：`[NPTask blockOn:async ^ void { … }]`**（规则 3）：阻塞 join 在
  调用点泵到完成。它在 receiver 仍 ARC 存活时运行——与旧语句位驱动相同的安全性论证
  （ARC 的 scope-end release 排在函数收尾，`main` 退出处兜底泵必然晚于它，故 v1 同样
  不设兜底泵）。
- `-ffreestanding`：同一条 blockOn 桥可用（baremetal stress 14 号场景覆盖）；旧的
  "持有句柄 → `[t start]` → `jeti_sched_run()`" 手动泵形态已废除。

## 实现注记

- 状态机入口参数用**保留名** `__jeti_task`（不是 `t`）：用户局部 `NPTask<int> *t = …`
  曾把参数 `t` 遮蔽，展开出的 `t->self_obj` 读到未初始化的新局部 → SIGSEGV（本目录是
  回归源）。
- 入口里的 `self` 已强类型化：`((struct Cls *)__jeti_task->self_obj)`——async 体内
  `self->ivar` / 裸 ivar 访问由此编译通过（2026-10-14 修复）。
- 负例在 `tests/negative/async_nptask_*.jeti`（旧拼法 `async NPTask<…>` 迁移错误 /
  上下文关键字冲突）与 `tests/negative/async_call_must_await.jeti`（规则 2）。
