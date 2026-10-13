# async/await 软关键字 + 异步代码块 设计定稿

> 状态:**设计定稿(2026-10-10 定案),未实施**。
> 本文是 `doc/async_nptask_plan.md` 的修正案:**取代**其三处定案——签名形态
> (第四代 `(async NPTask<T>)`)、`@await` 拼写、以及"不引入 try await +
> `@try` 跨 await 禁止"(§9:`try await` 引入,与 `@try` 共存,用户示例实证);
> 其余定案全部继续沿用:lazy 任务、语句位自动驱动(hosted)、机制/调度分层、
> freestanding 不自动驱动、blocking wrapper。机制层(runtime `jeti_task_*`)零改动。
> 核心原则不变:**异步语义属于语言,调度策略属于运行时或库。**
> 来源:用户 2026-10-10 定案 + AI 参考提案(Objective-C 异步代码块语法设计)。

## 1. 总览:相对现行设计的净变化

| 项 | 旧(现行实现) | 新(本文定案) |
|---|---|---|
| await 拼写 | `@await`(专用 token `AtAwait`) | `await`(**软关键字**,去 `@`) |
| async 语法位 | 仅方法返回类型位 `(async NPTask<T>)` | 方法位 + **顶层普通函数位** + **block 字面量前缀**(三语法位) |
| 签名形态 | `(async NPTask<int>)` —— async 后强制 `NPTask<T>` | `(async Result *)` —— async 后跟**业务结果类型 T**(`async T`) |
| 异步代码块 | 无 | `async Ret? ^ (params)? { body }`(完整形 `async void ^(...)`),体内可 `await` |
| await 关键字属性 | `@` 前缀硬 token | 软关键词:C 里 `await` 仍是合法标识符 |
| async 关键字属性 | raw-lookahead 上下文识别(仅方法位) | 软关键词,扩展到函数位与表达式位 |

## 2. 六种目标写法(验收基准)

> **类型拼写纪律(用户 2026-10-10 强调):库类一律 NP 前缀**——NPString、
> NPObject、NPTask、NPArray…(前缀来自项目原名 Nupa,与语言名解耦,见
> AGENTS.md)。参考提案里的 NSString/NSInteger/NSLog 等 NS 拼写**一律转写**
> 为 NP* 或 C 标量(NSInteger→int),禁止进入本文示例、实现与测试;
> `User`/`Result`/`Input` 为应用层假想类,不受库前缀约束。

```objc
// 1. Objective-C 异步方法
- (async Result *)performTask;

// 2. Objective-C 异步类方法
+ (async Result *)loadConfiguration;

// 3. 普通 C 异步函数
async Result *fetchResult(void);

// 4. 无返回值的异步函数
async void performTask(void);

// 5a. 异步代码块——完整形:返回类型写在 async 与 ^ 之间(可为 void)
async void ^(NPString *userID) {
    User *user = await [self fetchUser:userID];
    [self displayUser:user];
};

// 5b. 异步代码块——省略形:返回类型省略,由推导规则确定(规则未定,
//     实施前仅保证可解析,见 §8-①)
async ^{
    await [self performTask];
};

// 6. 等待异步结果
Result *result = await [service performTask];

// 6b. 等待可失败异步结果:失败按异步错误传播规则向外传(§9)
Result *result2 = try await [service performTask];
```

### 2.1 用户完整示例(第二批定案,2026-10-10;验收基准)

> 原例为 NS 拼写,按本节头注纪律转写:Foundation.h→Foundation.hje、
> NSObject→NPObject、NSString→NPString、NSArray→NPArray、NSLog→NPLog。
> 展示形态:语句位 IIFE 异步块(两例)、`try await` + 尾随 `throws`(§9)、
> async × 泛型返回(§9.2)、`try await` 与 `@try` 共存(例1)、`@[]` 数组
> 字面量(实现状态以探针为准)。例1 外层 `addAction:^{…}` 是**普通 block**
> (非异步上下文),await 只出现在异步块体内——与 §5.3-1 一致;例2 的
> 异步块未标 `throws`,其 `try await` 失败后的向外路径是 §9.3 待细化问题
> 的活样本。

```objc
// 例1:普通 block 内定义异步块并立即可调用(IIFE)
- (void)setupButton {

    [self.button addAction:^{
        async void ^(void) {

            [self.button setEnabled:NO];

            @try {
                User *user =
                    try await [self.service fetchUser:@"1001"];

                [self displayUser:user friends:@[]];
            }
            @finally {
                [self.button setEnabled:YES];
            }

        }();
    }];

}
```

```objc
// 例2:async 方法尾随 throws;异步块定义于同步方法体内,定义即调用
#import <Foundation/Foundation.hje>

@interface User : NPObject
@property (nonatomic, copy) NPString *name;
@property (nonatomic, copy) NPString *userID;
@end

@interface UserService : NPObject

- (async User *)fetchUser:(NPString *)userID throws;

- (async NPArray<NPString *> *)fetchFriends:(NPString *)userID throws;

@end

@interface ProfileController : NPObject

@property (nonatomic, strong) UserService *service;

- (void)displayUser:(User *)user
            friends:(NPArray<NPString *> *)friends;

@end

@implementation ProfileController

- (void)loadProfile:(NPString *)userID {

    // 定义一个无返回值的异步 Block
    async void ^(NPString *userID) {

        // 等待异步请求,并获取用户
        User *user = try await [self.service fetchUser:userID];

        // 获取好友列表
        NPArray<NPString *> *friends =
            try await [self.service fetchFriends:userID];

        // 更新用户界面
        [self displayUser:user friends:friends];

    }(userID);

}

- (void)displayUser:(User *)user
            friends:(NPArray<NPString *> *)friends {

    NPLog(@"用户:%@", user.name);
    NPLog(@"好友数量:%lu", (unsigned long)friends.count);

}

@end
```

## 3. await:软关键字定案

### 3.1 词法层

`await` 保持 `Identifier` token,**不新增** `KeywordKind`;全部判定在 parser
lookahead(与 `at_async_modifier` 的 raw-source lookahead 同款手法)。
现有 `AtAwait` 专用 token 随本设计废除,旧 `@await` 拼写不再接受(§6)。

### 3.2 判定规则(表达式前缀位)

`await` 处于**表达式前缀位**、且其后紧跟 token ∈ 一元表达式起始集时,是 await
运算符;否则按普通 C 标识符处理。起始集:

```
{ Identifier, Int/Float/Char/String 字面量, (, [, ^, !, ~, ++, --, sizeof, @( }
```

- **刻意排除 `+ - * &`**(一元/二元同形算符):保住 `await + 1`、`await * p`、
  `await & x` 中 `await` 作为变量名的 C 兼容。代价:`await -t`、`await *t`
  须写成 `await (-t)`、`await (*t)`。
- `await (` 判定为运算符:函数名恰好叫 `await` 的存量 C 代码(`await(x)`)
  需改名——与 C++20 把 `co_await` 定为硬关键字的取舍同款,记录为已接受破坏面,
  负测试锁定。

### 3.3 `await await` 必须正确解析

`await await e` 解析为 `await (await e)`:外层 `await` 后跟 Identifier
(`await` 在起始集内)⇒ 运算符;操作数按一元前缀递归,内层同理。
类型语义沿用既有 checker 规则(`await` 对 `NPTask<T> *` 得 `T`,对非任务表达式
保持原类型):`await await t`(t: `NPTask<NPTask<int>> *`)⇒ `int`,链式双解包。

必测用例:`await await [f m]`、`int await = 1;`、`await = 5;`、`return await;`、
`await + 1`(标识符)、`await [w m]`(运算符)。

### 3.4 C 兼容边界表(定案)

| 写法 | 判定 | 依据 |
|---|---|---|
| `int await = 1;` | 标识符 | 类型位,不处于表达式前缀位 |
| `await = 5;` | 标识符 | 后跟 `=`(不在起始集) |
| `return await;` | 标识符 | 后跟 `;` |
| `await(x)` | **运算符** | `(` ∈ 起始集;函数名 await 需改名(接受) |
| `await + 1` / `await * p` | 标识符 | `+ - * &` 被排除 |
| `await await e` | 双运算符 | Identifier ∈ 起始集 |
| `await [w m]` | 运算符 | `[` ∈ 起始集 |

## 4. async:三个语法位

### 4.1 方法位(现状扩展)

`at_async_modifier` 保留;**`require_nptask_async` 删除**——`async` 后跟任意
合法返回类型(含 `void`),不再强制 `NPTask<T>`。interface/impl 签名对账、
`@protocol` 方法位沿用既有检查(对账比较"async 标志 + 业务类型")。

### 4.2 顶层函数位(新增)

声明/定义位的标识符 `async`:

- 后跟**类型起始**(`void`、内建类型、typename 标识符、struct/union/enum)
  ⇒ 函数 async 修饰符:`async Result *fetchResult(void);`、
  `async void performTask(void);`(声明与定义均合法)。
- 后跟 `^` ⇒ 异步 block 字面量(表达式位,§5)。
- 后跟 `= ; ) .` 等 ⇒ 普通 C 标识符。

体内自证对账与方法的 checker 规则同款:async 函数体内无挂起点 = error
(`'async' ... body never suspends`)。`async void` 函数即入口形态
(blocking wrapper / 语句位驱动语义与 async void 方法一致)。

### 4.3 类型模型:`async T` 的调用语义

声明位 `async T`(T = 业务结果类型),调用语义三条全部沿用已实施机制,仅签名
拼写在 parser/对账层换形态:

1. async 调用(方法/函数/block)表达式类型 = 任务句柄 `NPTask<T> *`——lazy,
   调用不执行;
2. `await` 解包句柄得 `T`;
3. 语句位调用 = 自动驱动(hosted;`-ffreestanding` 一律不驱动)。

`async void` ⇒ 句柄 `NPTask<void>`(裸 NPTask),`await` 得 `void`。
生成 C 侧返回类型仍是擦除的 `NPTask *`;`NPTask<T>` 保持真类型(可作变量/参数/
ivar 位),任务句柄一等公民属性不变。vtable `__sig` 需把 async 性与业务类型
编码进签名串(编码方式见 §8-④;R3 只覆盖公共段的铁律不变)。

## 5. 异步代码块(新增语法位)

### 5.1 语法(定案)

```
AsyncBlockExpression ::= 'async' ReturnType? '^' Parameters? BlockBody
```

**返回类型写在 `async` 与 `^` 之间**(用户 2026-10-10 修正;参考提案的
`^NSInteger(...)` 后置形不采纳,避免双拼写)。参数表与块体沿用 block 既有结构。

```objc
// 完整形:显式 void 返回(用户原例,NSString→NPString)
async void ^(NPString *userID) {
    User *user = await [self fetchUser:userID];
    [self displayUser:user];
};

// 完整形:显式对象返回类型
async Result *^(NPString *key) {
    return await [self.api fetchValueForKey:key];
};

// 完整形:显式标量返回类型
async int ^(int a, int b) {
    return a + b;
};

// 省略形:返回类型省略 → 由推导规则确定(规则未定,§8-①;实施前仅保证可解析)
async ^{
    await [self performTask];
};

// IIFE(定义即调用):块表达式后直接跟调用括号 `(args)`;
// 语句位求值+调用 = 自动驱动(hosted)
async void ^(void) {
    await [self performTask];
}();
```

- 参数表可整体省略(如省略形);完整形无参写作 `async void ^{ … }` 或
  `async void ^(void) { … }`(实施时统一,倾向两者都收)。
- **与普通 block 的拼写对比**:普通 block 字面量返回类型在 `^` 之后
  (`^int(int x){…}`,既有语法一字不动);异步 block 在 `async` 与 `^` 之间,
  `^` 后只接参数表/块体。`async` 前缀是两形的唯一判据。
- **语句位 IIFE(用户示例定案)**:异步块表达式后直接跟调用括号——
  `async void ^(NPString *userID) { … }(userID);`——定义即调用,产生句柄并
  按语句位语义自动驱动(hosted)。外层可以是普通方法体/普通 block 体
  (用户两例均如此),异步上下文只是块体本身(§5.3-1)。

### 5.2 类型与调用语义

- `async T ^…` 的**类型** = 异步块指针(概念拼写 `async T (^)(params)`);
  **求值/调用**产生任务句柄 `NPTask<T> *`,lazy 不执行——与方法/函数同款
  三语义(§4.3)。
- `async void ^…` ⇒ 句柄 `NPTask<void>`,`await` 得 `void`。
- **变量声明须显式写异步块类型**:类型系统必须保留同步/异步之别(提案 §5);
  `auto` 推导不做(用户既有定案:"不然会乱")。声明位的正式拼写是遗留项
  §8-②,语义先行:调用块变量得 `NPTask<T> *`,`await` 解包得 `T`。
- **参数位**(方法收异步块,提案 §6):async 修饰块指针类型的返回段——

  ```objc
  - (void)performTask:(async void (^)(void))block;
  - (Result *)processValue:(async Result *(^)(Input *input))block;
  ```

  传入的异步块不自动降级为同步块,编译器不悄悄丢弃任务结果。
- **async × 泛型(用户示例定案)**:`async NPArray<NPString *> *` 合法——
  async 后跟任意合法返回类型,含单态化泛型实例;句柄
  `NPTask<NPArray<NPString *> *> *`。
- **可失败标注**:async 方法可用 `throws(ErrType *)` 尾随注解声明可失败
  (B′ 统一拼写,文档性,见 §9)。

### 5.3 await 约束(采纳提案 §7)

1. **只有异步上下文可用 `await`**:async 方法体、async 函数体、`async ^`
   块体。普通 block 体(包括嵌在异步方法里的普通 block)出现 `await` =
   **error**。——这同时了结上一轮探针发现的"block 体内 `@await` 被静默
   吞掉"bug:新设计下它是显式诊断,不再静默。
2. 异步上下文内可自由调用同步方法;同步调用不构成挂起点,也不需要 await。
3. 异步块可嵌套;**创建 ≠ 执行**,外层是否等待由显式调用/`await` 决定,
   不因嵌套自动等待。
4. 生命周期:沿用 block 捕获规则;异步块被 spawn 后由任务持有块结构(捕获
   环境),原始作用域结束不影响运行。

## 6. 兼容与破坏面(定案)

| 项 | 处理 |
|---|---|
| `@await` 拼写 | 废除,不再接受;专用 token `AtAwait` 删除,负测试锁定 |
| 旧方法签名 `(async NPTask<T>)` | **语义改变**:新模型 `async T` 的 T 是业务类型,`async NPTask<int>` 会变成"业务类型 = 任务句柄"。存量用点(`tests/full_syntax_test.jeti`、`tests/eh_matrix/samples/async_eh` 等)须迁移到 `(async T)`;迁移完成前 checker 对 `async NPTask<…>` 形态发 error(不静默) |
| 存量 C 代码函数名 `await` | `await(x)` 判定为运算符,需改名(§3.2 已接受) |
| `require_nptask_async` 检查 | 删除(async 后不限 NPTask) |
| 普通 block 体内 await | 由"静默吞掉"(降级缺失)改为 checker **error**(§5.3-1) |

## 7. checker 规则(新增/调整)

- **await 上下文判定**:async 方法体 / async 函数体 / `async ^` 块体 = 异步
  上下文;普通函数体、普通 block 体 = 非异步。非异步上下文 `await` = error。
- **体自证**:async 方法/函数/异步块体内必须有挂起点(`await` 或语句位
  async 调用),否则 error(`'async' … body never suspends`)——方法位既有
  规则扩展到函数位与块体。
- **签名对账**:interface/impl、@protocol、函数声明/定义均比较
  "async 标志 + 业务返回类型"。
- **`__sig`**:公共段签名串把 async 性与业务类型编码进去(格式 §8-④);
  R1/R2/R3 铁律不变。

## 8. 实施切分与遗留问题

实施顺序(机制层 runtime `jeti_task_*` **零改动**):

1. **词法/parser**:删 `AtAwait`;`await` 软关键字判定表(§3);`async` 函数位
   lookahead(§4.2);`async Ret? ^` 块字面量解析(省略形先可解析)。
2. **checker**:§7 全部;旧 `async NPTask<…>` 迁移诊断。
3. **async 降级 pass**:函数位降级;`async ^` 块 = 块结构(captures)+ 隐藏
   thunk 函数(体内 await 先降级)+ spawn 包装;普通块 await 交 checker 报错,
   pass 不再处理普通块(取代"补 Block 下钻"的旧修法)。
4. **codegen**:`await` 泵调用发射;异步块三件套发射;`Await` 漏网 fallback
   改硬 error(上一轮定下的防御,保留);`__sig` 编码。
5. **测试**:§3.3/§3.4 await 判定正负用例、§2 验收六形、块三形、
   `(async NPTask<T>)` 迁移用例、multi_tu async 方法公共段 `__sig` 一致性;
   `cargo test --workspace` + `test_all.sh` 基线。

遗留(明确**暂不做**,实施前逐项请用户拍板):

- ① **省略返回类型的推导规则**(用户定案"由编译器按规定的推导规则确定",
  规则本身未定;此前省略形仅保证可解析)。
- ② 异步块**类型**的声明位正式拼写(变量/参数/返回值处 `async` 放哪一段;
  语义模型已定,拼写未定稿)。
- ③ 错误传播:**已定案(2026-10-12 第三批修订 B′)**——`try await` 强制层
  删除,异步失败与同步异常完全同构(`@try/@catch` 处理、无人接顶层
  abort),拼写统一为 `throws(ErrType *)`(文档性,同步/异步同一标记),
  见 §9;第二批强制设计由 §9.5 清算作废。
- ④ `__sig` 对 async 业务类型的编码格式。

## 9. 异步错误传播(B′ 定案,2026-10-12 第三批修订)

> **本节整体取代第二批 `try await` 强制设计**。用户 2026-10-12 拍板"那就
> B":异步失败处理与同步异常完全同构,不引入任何强制级编译期检查。动因是
> 三探针实证:(a)同步 `throws` 本就是文档性标注——Jeti 从未有编译期错误
> 强制,异步单侧强制制造不对称;(b)纯 `@try/@catch` 的运行时语义当时已
> 完整成立;(c)"两套 throws"拼写怪异(裸 `throws` vs `throws`)。

### 9.1 定案内容

- **`try` 软关键字与强制规则删除**:解析器不再定义 `try await` 组合;async
  pass 删除旧 §9.2 检查(可失败须 try、failable_sels、iface_throws 对账、
  failable-await 位点收集)。
- **拼写统一为一个标记 `throws(ErrType *)`**:同步函数与异步方法同一写法、
  同一尾随位置(既有注解解析已兼容方法位,无需新语法位);裸 `throws` 拼写
  废除。`throws` 回归**文档性**——记录失败类型,与同步侧一致,无调用点
  强制。
- **运行时零改动**:失败通道(`failed` flag、flag 重武装、错误值走 `result`
  槽)对标记完全无感,全部保留;`__jeti_try_await_propagate`/passthrough 宏
  留存但不再被生成代码引用。
- **无处理失败 = 顶层 loud abort**(带异常类名,不静默)——与同步 `@throw`
  无人接的行为同构。旧 §9.3 唯一锁定点"失败不允许静默吞掉"保持成立。

### 9.2 三探针实证(定案依据,2026-10-12)

| 探针 | 测试 | 结果 |
|---|---|---|
| hole.jeti | throws-less 方法裸 `@throw`,无 try 无 @try 地 await | 编译零拦截;运行时 abort(类名正确,无静默垃圾) |
| hole2.jeti | **无 try 关键字**,纯 `@try` 包 await | `@catch` 接住——运行时标记无关成立 |
| sync_throws.jeti | 同步 `throws` 函数无 `@try` 调用 | 编译零拦截——同步侧本就无强制 |

### 9.3 统一语法(定稿形态,已运行验证)

```objc
// ── 同步函数:既有约定(golden 15 原样,一字未动)──
static int divide(int a, int b) throws(MathError *) {
    if (b == 0) { @throw [[MathError alloc] init]; }
    return a / b;
}

// ── 异步方法:同一个标记,同一个位置语义 ──
- (async int)fetchScore:(int)uid throws(ApiError *);
```

调用点三模式(全合法,零新关键字):

1. **正常路径不处理**——失败向上传播,无人接 = 顶层 abort(与同步 `@throw`
   同构);
2. **处理异步失败 = 普通 `@try/@catch`**——运行时已实证(§9.2 hole2);
3. **同步失败同理**(既有行为,一字未动)。

完整示例见 `probes/try_await/bprime_demo.jeti`(同步 + 异步并排,三种模式
一次跑通:`a=7 / api error caught / math error caught / main done`)。

### 9.4 实现差量(全部为删除或改拼写)

| 位置 | 动作 |
|---|---|
| parser | `try` 软关键字块与 `next_is_await_operator` 删除;裸 `throws` 尾解析删除 |
| async pass | 旧 §9.2 的 failable_sels / iface_throws / check_failable_awaits / collect_failable_awaits 全删;await 降级的 `__jeti_try_await_passthrough` 分支删(may_fail 恒 false) |
| cst/ast/elaborator | `Await.may_fail`、`Method.async_throws` 字段删除 |
| checker | 合成 Method 的 async_throws 填充删除 |
| codegen | `async_sig_marks` 的 `!` 标记删除(golden 34/37 无 throws 方法,指纹不变) |
| runtime | **零改动** |
| 测试 | probes/try_await 两探针迁移到 §9.3 形态;bprime_demo 收编入 probes |

### 9.5 对旧定案的清算

- 第二批 §9.1(`try` 可失败标记语义)、§9.2(尾随裸 `throws` + 强制 `try`
  + 签名 throws 对账)、§9.3("无 try = error 还是 warning"问题随强制删除
  而消失)**全部作废**。
- 仍成立的两条事实:checked 后端下**挂起返回不得触发 `@finally`/`@catch`**
  (旧 §9.4 实现注意——flag 区分挂起与错误,`jeti_task_await` 实现不变);
  异步失败传播已在 checked 后端完整实证,sjlj 后端仍不在本轮范围。
- 历史清算链:旧遗留 ③"不引入 try await"作废(第二批)→ 第二批强制设计
  作废(B′)——等价于以更彻底的方式回到最初立场,但**保留**了第二批的
  runtime 遗产(失败通道/传播)与 `throws` 文档性标注。
