# 34 — async/await golden（当前设计 = async 软关键字）

> 现行设计 = `async` 返回类型前修饰符 + 业务类型（`docs/plans/async_soft_keywords_plan.md`）：
> 方法签名写 `- (async int)compute:(int)n`，体内 `await n`，编译器内部仍归一化为任务句柄
> `NPTask<int> *`（归一化 pass：`crates/async` `normalize_async_signatures`）。
> 金字塔基线：`34_async/` 与 `37_async_modifier/` 都用**新拼法**；两者生成 C 逐字节同形
> （归一化保证新拼法与旧拼法产物一致），区别只在源码注释/覆盖面。
> 本目录原记录的 M1/M2 里程碑形态与"同步上下文调非 void async 报错"检查已随第四代设计移除。

方法体内 `await` 是挂起点；`async`/`await` 都是**软关键字**（C 超集：`int await = 1;`、`int async = 1;` 必须编过）。

| 文件 | 期望 |
|------|------|
| `async_test.jeti` | 编译通过，stdout 见 `.out`（async void 入口 + 跨调用链双 await，`result=18`） |

```bash
./target/debug/jetic run tests/golden/34_async/async_test.jeti
```

## 已定设计（用户拍板，2026-09）

1. **入口 = 语句位的 async 调用**：同步上下文调用 async void 合法。参照 C# `async void` vs `async Task`（无死锁风险：单线程协作驱动到完成必有进展）。**语句位**的裸调用（`[f runAll];`）与语句位 `[t start];` 在 hosted 下就地驱动到完成——不是自动生成的 blocking wrapper，也不在 `main` 退出处兜底泵（ARC 的 scope-end release 会先落地）。（被接住的裸调用只创建任务（形态 A，lazy），由 `await t` 或语句位 `[t start]` 驱动。）
2. **语句位 async 调用 = 入口**：hosted 下就地驱动到完成（降为 `jeti_task_await`）；`-ffreestanding` 下只创建任务、结果丢弃（入队由用户主循环 `jeti_sched_run` 驱动）。
3. **break/continue 跨 await 放行**（M2 状态跳转）；**@noarc 跨 await 放行**（无 jmp_buf）；**@try 跨 await 报错**（try/catch/finally 不能跨挂起点，替代：try 包在 await 之后）。
4. **bridge header**：async void 发 wrapper 声明；非 void async 跳过（M2）。

## 实现形态（本目录覆盖）

**不拆函数签名**（vtable 槽位不变、跨 TU 安全、bridge header 零特判）。desugar 在 AST 层改写 async 方法体；签名 `(async T)` 归一化为 `NPTask<T> *` 后，生成 C 如下（M2 状态机）：

```c
/* 调用点：- (async int)compute:(int)n → 只建任务（形态 A，lazy） */
NPTask * compute_(NPObject * self, SEL _cmd, int n) {
    NPTask * __jeti_task = (NPTask *)(jeti_task_create(
        jeti_async_state_compute_, self, sizeof(struct compute__frame)));
    ((struct compute__frame *)__jeti_task->frame)->n = n;
    return __jeti_task;
}

/* 状态机：await 拆段；活过挂起点的局部提升进 frame */
__attribute__((weak)) int jeti_async_state_compute_(NPTask * __jeti_task) {
    switch (__jeti_task->state) {
      case 0: {
        struct compute__frame * __jeti_f = (struct compute__frame *)__jeti_task->frame;
        int raw = (__jeti_task->state = 2, __jeti_f->n);    /* `await n`：先落新状态再取值 */
        __jeti_task->result = (void *)(unsigned)(raw * 2);  /* 体内 `return raw * 2` */
        __jeti_task->state = -1;
        return 1;
      }
    }
    __jeti_task->state = -1;
    return 1;
}

/* `await [self compute:n]` → 建任务 + 就地驱动取 T */
int a = (__jeti_task->state = 2, (long)jeti_task_await(/* vtable 调 compute_，返回新任务 */));
```

- `await e` → `(__jeti_task->state = N, <e 或 jeti_task_await(子任务)>)`：Comma 求值——先落新状态（下次驱动从这里续），再取值；`e` 是任务表达式时走 `jeti_task_await`（未 start 自动 start、结果缓存、可多 await）。
- 任务由**调用点**创建（`jeti_task_create`：分配任务本体 + 每方法一个 frame 结构体），方法体拆进 `switch (__jeti_task->state)`；`[t start]` / 语句位裸调用在 hosted 下就地驱动到完成，裸机只入队等用户主循环泵。
- **检查层**（`jeti_async::check_unit`，desugar 前跑原始 AST）：
  - `@try` 跨 await → error（try/catch/finally 任一含 await 即报）
  - `async` 修饰符与体内 `await` 双向对账（声明 async 无 await → error；体内 await 无声明 → error）
  - 旧拼法 `(async NPTask<…>)` → error + 迁移提示（§6）
- runtime（`include/jeti/runtime.{h,c}`）：`NPTask{state,finished,entry,self_obj,frame,result,parent}` + create/resume/finish/join 四个 API（host 用 calloc/free；freestanding 用户提供分配器后可用）。

## 里程碑路线（历史）

- **M1**：task 驱动 + await 钩子 + 检查层，行为=同步执行，API 契约就位
- **M2**：真状态机——`await` 拆段进 `switch(t->state)`、活过挂起点的局部提升进 frame（仿 @try 的 TRY_LIFT 提升先例）、break/continue → 状态跳转、ARC 在任务退出汇合点统一结算、bridge header wrapper
- **M3**：调度器 `jeti_sched_run` + 任务图（`parent` 字段已预留）/ I/O

## 负例

现行负例在 `tests/negative/async_nptask_*.jeti`：旧拼法 `async NPTask<…>`（迁移错误）与上下文关键字冲突（`int async = 3;` 必须编过——C 超集铁律）。M1 时代的"同步上下文调非 void async"负例已随第四代设计取消（裸调用 async 方法合法，形态 A，lazy）。
