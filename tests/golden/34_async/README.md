# 34 — async/await golden（Async Semantics v1）

> 设计正典：`docs/plans/async_semantics_v1.md`（Swift 模型六规则，2026-10-14 冻结）。
> 方法签名写 `- (async int)compute:(int)n`（`async` 属签名，修饰业务类型），体内
> `await [f …]` 是唯一挂起点形态。编译器内部仍归一化为任务句柄 `NPTask<int> *`
> （`crates/async` `normalize_async_signatures`）。

`async`/`await` 都是**软关键字**（C 超集：`int await = 1;`、`int async = 1;` 必须编过；
软关键字矩阵见 `49_soft_keywords/`）。

| 文件 | 期望 |
|------|------|
| `async_test.jeti` | 编译通过，stdout 见 `.out`（blockOn 桥内 await 跨调用链，`result=18`） |

```bash
./target/debug/jetic run tests/golden/34_async/async_test.jeti
```

## v1 规则速览（本目录钉住的）

1. **调用 async 方法只有一种形态：`await [f x]`**——裸调用（语句位/表达式位/cast 弃值/
   return 位/同步方法内）一律 error（负例 `tests/negative/async_call_must_await.jeti`）。
   旧的"语句位入口就地驱动 / `[t start]` 语句位驱动 / 同步调 async void 自动 wrapper"
   三套特殊行为已全部拆除。
2. **同步 → 异步唯一显式桥：`[NPTask blockOn:async T ^{ … }]`**——`main`/同步方法里
   只用它；块内 `await` 驱动子任务。
3. **async 体内无 await 合法**（规则 1：`async` = 允许挂起的能力；37 的 `compute:` 就是
   缓存命中形态）。反向（无标记含 `await`）仍是硬错误。
4. `await` 一个非 async 表达式 → **warning**（rule 6 lint；`-Werror` 升级）。
5. **`@try` 跨 await → error**（try/catch/finally 任一含 await 即报；仅 checked 后端
   支持跨挂起 EH，legacy 的 jmp_buf 活不过挂起点）。

## 实现形态（本目录覆盖）

desugar 在 AST 层把 async 方法体改写为 M2 状态机；签名 `(async T)` 归一化为
`NPTask<T> *` 后，生成 C 如下：

```c
/* 调用点：- (async int)compute:(int)n → 任务工厂（句柄只来自显式 spawn，阶段3） */
NPTask * compute_(NPObject * self, SEL _cmd, int n) {
    NPTask * __jeti_task = (NPTask *)(jeti_task_create(
        jeti_async_state_compute_, self, sizeof(struct compute__frame)));
    ((struct compute__frame *)__jeti_task->frame)->n = n;
    return __jeti_task;
}

/* 状态机：await 拆段；入口里 self 已强类型化 ((struct Cls *)__jeti_task->self_obj) */
__attribute__((weak)) int jeti_async_state_compute_(NPTask * __jeti_task) {
    switch (__jeti_task->state) {
      case 0: {
        struct compute__frame * __jeti_f = (struct compute__frame *)__jeti_task->frame;
        int raw = (__jeti_task->state = 2, (long)jeti_task_await(/* 子任务调用 */));
        __jeti_task->result = (void *)(unsigned)(raw * 2);  /* 体内 `return raw * 2` */
        __jeti_task->state = -1;
        return 1;
      }
    }
    __jeti_task->state = -1;
    return 1;
}
```

- `await e` → `(__jeti_task->state = N, jeti_task_await(<e>))`：Comma 求值——先落新状态
  （下次驱动从这里续），再取值；`e` 是 async 调用时走 `jeti_task_await`（未 start 自动
  start、结果缓存、可多 await）。
- **检查层**（`jeti_async::check_unit`，desugar 前跑原始 AST）：
  - 裸 async 调用 → error（规则 2，五形态全拦）
  - 无标记体内 `await` → error；有标记无 `await` → 合法（规则 1）
  - await 非 async 表达式 → warning（规则 6）
  - `@try` 跨 await → error
  - 旧拼法 `(async NPTask<…>)` → error + 迁移提示
- runtime（`include/jeti/runtime.{h,c}`）：`NPTask{state,finished,entry,self_obj,frame,
  result,parent}` + create/resume/finish/join；freestanding 下 blockOn 的阻塞 join
  同样可用（baremetal stress 14 号场景覆盖）。

## 里程碑路线（历史）

- **M1**（task 驱动 + await 钩子）：已完成，task-wrapper desugar 已随 v1 阶段2 拆除
- **M2**：真状态机——`await` 拆段进 `switch(t->state)`、break/continue → 状态跳转、
  ARC 在任务退出汇合点统一结算（现行）
- **M3**：显式 spawn / 任务所有权 / 父子取消（spec 阶段 3；`parent` 字段已预留，
  启用前须先拆调度队列对它的链表复用）

## 负例

`tests/negative/async_call_must_await.jeti`（规则 2 五形态）、
`tests/negative/async_nptask_*.jeti`（旧拼法迁移错误 / 上下文关键字冲突——
`int async = 3;` 必须编过，C 超集铁律）。
