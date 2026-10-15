# 48_async_throws_await — 2026-10-13 定案语法的钉子

一个用例钉住两条当日定案：**裸 `throws` 注解**（同步+异步同一拼写）与
**消息发送内的强制括号 `await`**。

## 判据表

| 输出行 | 判据 |
|---|---|
| `ok=30` / `fail=0` / `caught-neg` | async 方法 `throws(NetErr *)`：成功路径 `await` 得 30；失败路径失败 await 归 0 + EH flag，call-site guard 传播给外层 `@catch`（B′ 语义） |
| `probe=2` / `fail99=0` / `caught-99` | 裸 `throws`（无括号形态）与带类型形态行为一致：体内真抛必须、失败传播同上 |
| `msgarg=41` | 消息实参位强制括号形态 `[api wrap:(await [api load:4])]` → 30+1+10=41 |
| `msgrecv=7` | 消息接收者位强制括号形态 `[(await [api box:2]) value]` → 7 |

## 为什么裸 `throws`（位置消歧规则）

`throws` 是**软关键字**，靠位置而非保留字消歧（C 超集铁律：用户标识符
`throws` 不受影响）：

- `throws:`（尾随冒号）→ 选择子分段（`- (int)throws:(int)x`、`[t throws:1]`）
- `- (int)throws;` → 方法名叫 `throws`（返回类型后首个 token 即方法名）
- 参数表之后、`;`/`{` 之前 → 异常注解（`throws` / `throws(T *)`）

`@throws` 已废除（parser 三处报迁移诊断，无 deprecated 窗口）；`@throw`
是语句，保留 @。

## 为什么消息内的 `await` 要加小括号

**禁令而非优先级规则**：裸写 `[a m:await [a n]]` 会被读成 await 盖住整条
消息链（Swift `await foo.bar()` 语义），但 Jeti 的 `await` 只作用于紧跟的
一元表达式——两种读法语义不同，与其发明优先级不如强制读者写出唯一读法：

- 实参位：`[a m:(await [a n])]` —— `(` 把 await 推深一层，禁令只在
  await 恰好位于消息 `[` 的内部深度时触发，天然豁免
- 接收者位：`[(await [a n]) m]`
- 选择子位不受影响：`[task await]` 是选择子（软关键字，位置决定含义）
- C 变量叫 `await` 不受影响：`int await = 5;`、`[self add:(int)await]`
  （await 后跟 `]`/标识符时不是运算符）

## 与 cast 的交互

- `(int)(await [x])` —— cast 在外：先驱动任务再转换，合法
- `[a m:(await (int)[x])]` —— cast 包住的是任务句柄（NPTask*→int 无意义
  转换），await 吃到 int 后由 **C 编译器硬错误**兜底，无静默垃圾
- `(await)tv`（用户 typedef 叫 await）—— 运算符判定表保证是 cast 不是
  await 表达式

## 已知边界（不在此钉住）

- 顶层 async 函数路径的两个既有缺口：`static async void` 不被认作 async
  上下文；裸 `async void` 函数体内不被认作 async 上下文。入口走类方法
  `+ (async void)`（full_syntax §5.10 同型）。
- 块不捕获任意局部变量（只有参数 + 隐藏 self）——任意捕获留给阶段3
  spawn 会话。
