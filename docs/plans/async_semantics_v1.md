# Jeti Async Semantics v1（冻结稿）

> 2026-10-14 定稿。决策来源：`Claude、GPT建议.txt` 两轮评审 + 实现接地核查。
> 本文件是 async 后续所有改动的**规则依据**：先冻结语义，再改编译器。
> 与本文件冲突的旧决策（见 §3 反转记录）以本文件为准。

## 1. 三个必须永远有明确答案的问题

1. 调用一个 async 方法时，到底发生什么？
2. 什么时候开始执行，什么时候让出控制权？
3. 任务失败、取消或被丢弃时，运行时究竟承诺什么？

语法、降级、调度器都服从这三问，不得各自长出规则。

## 2. 六条冻结规则（Swift 模型）

1. **`async` 是签名的一部分，语义是"允许挂起"（capability），不是"必须挂起"。**
   async 方法体没有 `await` 完全合法（缓存命中、协议实现碰巧同步都是正常形态），
   不警告。反向检查保持硬错误：非 async 上下文里出现 `await` → error（§5.3-1）。
2. **调用 async 方法只有一种形态：`await [f x]`。** 不带 `await` 的 async 调用
   是编译错误，不再悄悄创建任务句柄。语句位自动驱动、同步调用方的自动
   blocking wrapper 全部废除。
3. **同步 → 异步只有一座显式的桥：`+[NPTask blockOn:<async 块>]`**，用在程序
   边缘（main）。同步方法里调 async 方法 → 硬错误并指向 blockOn。
4. **并发是显式的：`+[NPTask spawn:<async 块>]` 是任务句柄（`NPTask<T> *`）
   唯一的诞生地。** await 一个句柄得到 T；同一句柄可重复 await（结果缓存，
   与现状一致）。
5. **消息发送内的 `await` 必须加小括号**（2026-10-13 禁令延续）：
   实参 `[a m:(await [b n])]`、接收者 `[(await [b n]) m]`。选择子位
   （`[task await]`）、C 标识符 `await`、cast/分组括号不受影响。
6. **错误：签名 `throws`（裸拼写，B′ 定案延续）+ `@try/@catch` 包住 `await`。**
   失败的 await 置 flag 由 EH call-site guard 传播；单纯挂起不触发 `@finally`。
   `await` 一个非 async 表达式 → **warning**（不阻断）。未来可加
   `-Wunhandled-throws`（第二阶段）。

**语法锚点（勿被评审稿笔误带偏）**：async 块字面量是 `async int ^{ … }`
—— `async` 在 `^` **之前**（parser 实测拼法）。评审示例里的 `^async int {…}`
是错误拼写，禁止采用。spawn/blockOn 的块实参写法：

```objc
NPTask<int> *t = [NPTask spawn:async int ^ {
    return await [f compute:1];
}];
return [NPTask blockOn:async int ^ {
    await [Demo run];
    return 0;
}];
```

## 3. 反转记录（供未来会话查证，勿拿旧决策当依据）

| 旧决策（2026-10-13 前） | v1 处置 |
|---|---|
| 裸 async 调用 = 创建任务句柄（C# Task 模型） | **废除**（规则 2） |
| 语句位裸调用 = async 入口、就地驱动 | **废除**（规则 2） |
| 同步调 async void 自动生成 blocking wrapper | **废除**（规则 3；full_syntax 5.7 的入口形态随之退役） |
| 惰性启动（冷启动）作为模型卖点 | **蒸发**：纯 await 模型下启动时机仅 spawn 一处可观察，不再是文档负担 |
| async 体内无 await → 硬错误 | **废除**（规则 1） |
| NPTask 入对象体系（hosted=ARC 对象） | **保留** |
| B′：裸 `throws` 统一注解、runtime failed 通道 | **保留** |
| M2 状态机降级（frame/state/驱动） | **保留**，await 的 async 调用作为子协程链接进去 |
| "C# 语义（除显式 task）"的自我描述 | **修正**：启动时机、签名进 async、async void 均与 C# 不同，文档按 Swift/Kotlin 系描述 |

## 4. 前置缺口（主切换会话开工前必须解决，按挡路程度排序）

1. ~~**async 帧内 `self->ivar` 访问编译不过**~~ **已修（阶段2 会话）**：类名穿进
   `desugar_method_m2`（AsyncCtx 挂 `self_cls`），`self` 重写为
   `((struct Cls *)__jeti_task->self_obj)` Cast 包裹，块 thunk 路径同样穿透。
2. **`parent` 字段被调度队列复用**（`sched_enqueue` 当链表 next）——spawn 的
   父子追踪开工前必须拆别名。
3. **await 一次泄漏 task+frame ~80B**（所有权未决）——spawn 前须定句柄所有权。
4. legacy 后端下 `throws` 注解不随降级传入 `jeti_async_state_*`（checker 误报
   escapes）——诊断措辞问题；`@try` 跨 await 仅 checked 支持（jmp_buf 语义），
   该报错本身正确，保留。
5. `typedef async void (^ClickHandler)(int);` 的 typedef 位拼法未验证——spawn
   会话验证；本文件不承诺。
6. 用户变量/typedef 叫 `await` 时与降级/EH 包裹的外围缺口（非语法问题，低优）。

## 5. 分阶段路线（GPT 顺序 + 前置）

- **阶段 0（速赢，本次）**：规则 1 落地（无 await 合法）+ 规则 6 的
  await-on-sync warning + 软关键字矩阵 golden（49）。✅ 完成。
- **阶段 1（spec 即本文件）**：冻结规则，先写反例不动调度器。✅ 本文件。
- **阶段 2（最小闭环主切换）**：先修前置 1；然后"async 调用必须 await +
  删语句位驱动与自动 wrapper + blockOn 入口"；golden 34/37/47 与
  full_syntax 5.7/§5.10 重定基线；M2 状态机保留。✅ 完成（含前置 1 修复、
  golden 48 基线、负例 `tests/negative/async_call_must_await.jeti`）。
- **阶段 3（显式并发）**：先拆前置 2/3，定任务所有权与句柄丢弃规则（未 await
  的 spawn 失败去哪、退出时是否等待）；再落 spawn / 句柄 await / 失败在
  await 时浮出。**cancel/whenAll 不在本阶段**——父任务关系与取消传播另立
  spec（规则未冻结前不实现）。
- **阶段 4（EH/组合）**：`@try` 跨 await 的挂起恢复正确性独立测试目标；
  `-Wunhandled-throws`；后端能力边界对诊断精确化。

## 6. 测试钉子计划

- golden 49：软关键字矩阵（`async/await/throws/when/in/nullable/nonnull`
  ×变量名/字段名/方法名 + 关键字位仍生效 + 消歧优先级表进 README）。
- golden 48 增补：无 await 的 async 方法正例；await-on-sync 的 warning 断言。
- 阶段 2 时：golden 34/37/47 基线重写；负例"async 调用不带 await"。
- 阶段 3 时：`@catch` 内 await（重试循环形态）作为独立编译器测试目标。
