# 泛型协议约束（`T : Proto`）规划

> 状态：设计定稿，未实现。探针（`probes/probe_generic_bound*.jeti`）证实现状：
> parser 在 `T :` 处报 `expected '>' after type params`——bound 语法完全不存在。
> 本规划对齐 ObjC lightweight generics（clang 3.7，有语法无强制）并升级为
> **编译期强制**——jeti 单态化的实例化点是天然检查点，ObjC 做不到的这里做得到。

## 1. 语法

```jeti
// bound = 协议名（裸用，不加 id<> —— jeti 协议类型本就是编译期标签）
@interface Box<T : Greetable> : NPObject {
    T _value;
}
- (instancetype)initWithValue:(T)value;
@end

// 多参数混合：有的有 bound，有的没有
@interface Pair<K, V : Comparable> : NPObject { ... }

// 继承传播：子类约束不得弱于父类
@interface MutableBox<T : Greetable> : Box<T> { ... }   // 必须重申（见 §5）
```

决策记录：

- **接受 ObjC 拼写 `T : id<Summable>`，剥掉 `id<>` 后按裸协议名存储**。
  设计原则：ObjC 程序员写 jeti 必须零词典成本——jeti 的生存策略是「ObjC
  语义 + 静态实现」，拼写偏离 ObjC 等于给迁移者加税。同时**也接受裸协议名
  `T : Summable`**（更简洁的本地习惯拼写）；两种拼法等价，存储与诊断统一
  用裸名。类指针 bound（`T : NSObject *`）同样接受，验证规则见 §3。
- **不支持方法级约束**（`- (void)foo:(U)u where U : P`）。那是 HKT 门口的
  开始，与「ObjC 语义 + C 超集」定位冲突；类级 bound + 协议本身已覆盖
  总量约束场景。

## 2. 数据流（各层改动点）

| 层 | 改动 | 形态 |
|---|---|---|
| lexer | 无 | `:` 已是 token |
| parser | `parse_generic_params`（现 4656 行附近的 type_params 收集） | `type_params: Vec<String>` → `Vec<(String, Option<String>)>`（参数名, bound 协议名）；parser 的扁平 `type_params` 表保持只存名字（`is_type_param` 判定逻辑不变），bound 随 CstDecl 走 |
| CST | `CstDeclData::Class` 加 `type_bounds: Vec<(String, String)>`（参数名 → 协议 FQN） | 独立字段，不改 type_args |
| binder | `SymbolData::Class.type_params: Vec<String>` 旁加 `type_bounds` | 符号表是 bound 的唯一权威（checker 靠它查，codegen 不需要） |
| elaborator | 透传 bound 到 AST `AstDeclData::Class.type_bounds` | checker 消费 |
| checker | 新增 `check_generic_bounds`（见 §4） | **全部强制逻辑在此** |
| codegen | **零改动** | bound 不产生任何 C |

## 3. 检查点：单态化入口

唯一需要触发的位置是**显式特化的实例化点**：

```jeti
Box<NPString *> *b = ...;    // 检查：NPString 声明了 <Greetable>？
Box<id> *e = ...;            // 放行（逃逸通道，见下）
Box *bare = ...;             // 裸拼写：无 type_args，不触发（擦除兼容）
```

- **检查时机**：checker 的 MsgSend/变量声明路径拿到带 type_args 的接收者/变量
  类型时，按 base 类名查符号表的 `type_bounds`，对每个 (参数, bound) 用
  `type_args` 对应位置的实参类验证协议一致性。一致性判定复用现有的协议
  required-method 检查（`Circle` 缺 `draw` 的那条路径），沿协议父链递归。
- **逃逸通道（迁移安全阀）**：实参为 `id`、`instancetype`、裸泛型名、或
  无法解析的 forward-declared 类时**放行**——与 checker 全局哲学一致
  （"无法证明就不报"），保证现有代码零迁移：今天能编译的特化，加 bound
  之前就全部合法，加了之后照旧。
- **@using 别名**：泛型性可能藏在别名展开后的名字里（`init_generic_erasure_warning`
  注释记载的先例），bound 检查取 base 名时走同一剥离逻辑。

## 4. 诊断

```
error: 'NPNumber' does not conform to protocol 'Greetable' required by
       'Box<T : Greetable>' — add <Greetable> to NPNumber's declaration or
       use a conforming type argument
```

一次性报齐所有违约实参（收集后统一报，不首错即停——与 `@throws` 调和的
多错收集习惯一致）。`-fno-checker` 关闭（bound 是 checker 功能，不进 codegen，
关闭后生成 C 与无 bound 完全一致——这就是"零 codegen"的另一面：关掉检查
不会留下半成品）。

## 5. 继承与传播规则

1. **子类必须重申 bound**：`MutableBox<T> : Box<T>` 且父类有 bound 时，
   子类声明若不写 bound 报 error（不隐式继承——显式拼写让 `.jth` 读者一眼
   看到约束，这正是 `.jth` 必须保留完整布局的同一哲学：客户端要看到全部事实）。
2. **子类 bound 不得弱于父类**：`MutableBox<T : Specific>` : `Box<T : General>`
   要求 Specific 在 General 的协议继承链上，否则 error。
3. **实例化时沿类链收集全部 bound 一并检查**（父类约束对子类特化同样生效），
   复用 `find_class` 的 superclass walk。

## 6. 跨 TU 约束

bound 信息活在 `.jth`（共享声明面）里，与协议声明同轨——纯声明客户端 import
`.jth` 即拿到完整 bound 表，检查在客户端 TU 本地完成，**不依赖链接期、不碰
vtable 布局（R1/R3 零影响）、不产生新符号（R2 零影响）**。`.jth` 过期（客户端
看到旧版无 bound 的头）的后果是检查静默放行——与所有 checker 功能的降级
方向一致（漏报优于误报）。

## 7. 测试计划

- 正例：bound 声明 + 合规实例化（probe 升级为正式测试）；
- 反例：违约实例化（`NPNumber` 进 `Box<T : Greetable>`）报 §4 诊断；
- 逃逸通道：`Box<id>`、裸 `Box` 放行；
- 继承：子类未重申 bound 报错；弱化 bound 报错；实例化沿链检查生效；
- 多参数：`Pair<K, V : Comparable>` 只约束 V；
- 回归基线：`cargo test --workspace`、`./test_all.sh`、`tests/multi_tu/`、
  `tests/golden/`（golden 输出必须逐字节不变——bound 不进 C 的硬证据）。

## 8. 实施顺序

1. parser/CST/symbol/elaborator 数据管道（语法可解析，checker 忽略 → 全绿）；
2. checker `check_generic_bounds` + 诊断；
3. 测试 + golden 基线核对；
4. 文档：architecture.md §8（泛型）追加 bound 段落，ROADMAP 删行。
