# NPPredicate 完整实现规划

> 状态：设计定稿，未实现。目标：对齐 ObjC NSPredicate 的**完整语义**——
> 格式串 DSL、键路径求值、可组合谓词对象、容器宿主 API——不做 block 简化版。
> 前置矛盾（上轮评估已确认）：jeti 无运行时反射，按键名字符串找方法/ivar
> 的动作在现有架构里不存在。本规划用 **编译期 KVC 访问器表** 解决它：
> 派发仍是静态 vtable，字符串只做**查表**，不做反射。

## 0. 总体架构

```
@“age > 18 AND name LIKE 'A*'”
  → [NPPredicate predicateWithFormat:]  (运行时, libjetifoundation)
      │  DSL parser（Foundation 内，C 实现）
      ▼
  NPPredicate 对象（AST: 节点数组）
      │  evaluateWithObject:
      │    每个键路径节点 → jeti_kvc_<Class>_<keypath 段> 查表
      ▼
  BOOL 结果
```

三条支柱，各自独立可测：

1. **DSL parser + 求值引擎**（纯 Foundation C，~600 行）；
2. **KVC 访问器表**（codegen 生成——编译期把「类 + 键」映射到 getter fn-ptr）；
3. **宿主 API**（NPArray/NPMutableSet 的 filteredArrayUsingPredicate: 等）。

## 1. 支柱一：DSL 语法与求值（Foundation C）

`NPPredicate.jth/.jeti`，语义取 ObjC NSPredicate 格式串的常用全集：

```jeti
@interface NPPredicate : NPObject
+ (NPPredicate *)predicateWithFormat:(NPString *)format, ...;  // %@ 占位
- (BOOL)evaluateWithObject:(id)object;
- (NPPredicate *)predicateWithSubstitutionVariables:(NPDictionary *)vars;
- (NPString *)predicateFormat;   // 规范化回读（parser 规范化的往返证明）
@end
```

| 类别 | 运算符 | 备注 |
|---|---|---|
| 比较 | `= == != <> > < >= <=` | 数值走 NPNumber 通道，对象走 isEqual: |
| 逻辑 | `AND && OR \|\| NOT !` | 短路求值 |
| 字符串 | `BEGINSWITH ENDSWITH CONTAINS LIKE MATCHES` | `[c]` `[d]` 修饰符；LIKE 的 `*`/`?` 通配；MATCHES 只支持 POSIX 基础正则（无 regex 库依赖，自写小引擎或用 C 字符串扫描——边界见 §6） |
| 集合 | `IN BETWEEN` | 右操作数为集合或区间字面量 |
| 键路径 | `address.city`、`ANY tags` | 聚合 `ANY/ALL/NONE` 对 NPArray 元素逐个求值 |
| 字面量 | `'str'` `"str"` 数字 `TRUE FALSE nil` | `%@` 占位在 parse 前替换（参数数组随谓词对象保存） |

实现位置：`NPPredicate.jeti` 内一个递归下降 parser（token: 标识符/字符串/数字/
运算符），产出节点数组（union tag + payload，手写 tagged union——正是 §7 的
@sum 的 C 形态先例）。求值是树行走，叶子「键路径求值」调 KVC 表（支柱二）。
**parser 完全在 Foundation 库里，编译器（jetic）零参与 DSL 解析**——格式串
对 jetic 只是普通 NPString。

## 2. 支柱二：KVC 访问器表（codegen 生成）

这是规划的核心发明。`[p valueForKey:@"age"]` 要工作，需要 `age → fn-ptr`
的映射。jeti 没有 objc_msgSend 的 selector 注册表，但 codegen **编译期知道
每个类的全部方法名**——所以生成静态访问器表：

### 2.1 生成规则

对每个 `@implementation` 的类 X，codegen 额外发射（仅在「本 TU 可见
NPPredicate」时发射，见 §2.4）：

```c
/* 键 → getter。getter 形态统一为 id (*)(id self)：
   返回包装对象（int → NPNumber），无对应 ivar/property 的键缺席。 */
static const struct jeti_kvc_entry jeti_kvc_entries_$_Person[] = {
    { .key = "name", .get = jeti_kvc_wrap_Person_name },
    { .key = "age",  .get = jeti_kvc_wrap_Person_age  },
    { .key = NULL,   .get = NULL },                       /* 哨兵 */
};
```

包装函数由 codegen 合成（用户不写）：`- (int)age` 生成
`jeti_kvc_wrap_Person_age` = `return [NPNumber numberWithInt:[(Person *)self age]]`——
**方法体就是一条已有的静态 vtable 派发消息发送**，无新派发机制。

### 2.2 键的来源（收录规则）

按 ObjC KVC 语义：`@property` 名、ivar 名、以及**无参实例方法**（ObjC 的
getter 惯例）。收录顺序：property > ivar > 无参方法；同键先到先得。
键名按 selector 同款纪律处理（`UTF8String` 这类无参方法自动成为键）。

### 2.3 查表与类链

`jeti_kvc_lookup(id obj, const char *key)`：沿 `obj->isa` 的类链查每层的
entries 表（`NPClass` 需新增 `const struct jeti_kvc_entry *kvc_entries;`
字段——runtime.h 唯一改动点，追加尾部字段不破既有静态初始化的编译期常量
语义，字段缺省 NULL）。未命中返回 NULL，求值引擎报
`key not key-value coding compliant`（对齐 ObjC 的
`NSUndefinedKeyException`，但以 abort 呈现——jeti 无运行时异常注入）。

### 2.4 跨 TU 与元数据铁律核对

- **表符号链接性**：entries 表与包装函数是**类元数据的伴生符号**，linkage
  跟随 R2——owner TU 强符号，声明客户端弱/不引用。多 TU 时库的强表胜出，
  与 vtable 同一合并模型。
- **何时发射**：`emit_unit_with_headers` 检测到本 TU 的 `#import` 展开含
  `NPPredicate` 声明（与 auto-link 的 `#import` 扫描判据同源）且本 TU 拥有
  类实现时发射。**不 import NPPredicate 的 TU 零发射**——不付没用的表。
- **`__sig`（R3）零影响**：KVC 表不是 vtable 成员，签名段不变。
- **manifest 模式**：KVC 表不进 slots manifest（它不参与跨 TU 槽位一致性
  ——每个 owner 的表只描述自己的类）。

## 3. 支柱三：宿主 API

```jeti
// NPArray.jth 增补
- (NPArray *)filteredArrayUsingPredicate:(NPPredicate *)pred;
- (size_t)indexOfObjectMatchingPredicate:(NPPredicate *)pred;   // 首个命中
// NPMutableArray.jth 增补
- (void)filterUsingPredicate:(NPPredicate *)pred;               // 原地
// NPSet/NPMutableSet 增补
- (NPArray *)filteredArrayUsingPredicate:(NPPredicate *)pred;
```

实现为 Foundation 内的普通循环：逐元素 `evaluateWithObject:`，命中收集。
不改变容器存储布局，不新增 ivar，vtable 追加公共段方法——**会改 `__sig`**，
 Foundation 全量重建后客户端重编即可（与 NPSet 落地时同一流程，test_all
守护）。

## 4. `@{}` 字面量与谓词的组合

不做谓词字面量语法（`@(...)` 已被 boxed expr 占用）。组合用方法：

```jeti
NPPredicate *both = [p1 ANDPredicate:p2];   // 逻辑组合 API
```

## 5. 与 jeti 现有机制的交互核对（评审预演）

| 机制 | 影响 |
|---|---|
| ARC | 谓词对象是普通 NPObject，容器保留结果数组元素照常 retain——零特殊 |
| checker | 格式串是运行时字符串，**不做**格式串静态校验（校验=重写 DSL parser 进 checker，收益低）；占位符数量不匹配是运行时错误 |
| eh checked | `evaluateWithObject:` 键失败 abort 不 throw——不产生新的 throw 语义 |
| pattern pass / defer / async | 无交互（纯运行时库功能） |
| -ffreestanding | **排除**：KVC 表 + DSL 需要字符串与堆；裸机不发射表、Foundation 本就不可用 |
| 泛型单态化 | 特化类克隆时 KVC 表随类一起克隆（键名不变，getter 调特化方法——`substitute_cg_stmt` 同款替换） |
| `respondsToSelector:` 先例 | 同为「编译器合成的伪方法基础设施」，KVC 表是它的推广——codegen 已有按类合成静态辅助的成熟路径 |

## 6. 已知边界（如实）

- **MATCHES 正则**：POSIX ERE 子集自实现（`* + ? [] ^ $ . () |`）；PCRE
  回溯语义不承诺。ObjC 的 ICU 语法不兼容项记录在 Foundation 头注释。
- **无反射的代价**：键必须在编译期存在于某个 `@implementation`——运行时
  动态构造的类（jeti 也没有）自然不支持。
- **占位符**：`%@`/`%d` 等有限集合（对齐 format 检查的 FormatArgKind），
  复杂对象参数按 `isEqual:` 参与。
- **性能**：线性查表 + 每元素整棵谓词树求值。对 jeti 现有容器（本就线性
  扫描）不引入新的复杂度台阶；哈希加速是容器层课题，不归谓词。

## 7. 实施顺序

1. **KVC 先行**（可独立交付）：runtime.h 字段 + codegen 访问器表 +
   `valueForKey:`（挂在 NPObject）——即使没有谓词，KVC 本身就是完整特性；
   golden 基线核对（未 import NPPredicate 的 TU 输出必须逐字节不变）。
2. DSL parser + 求值 + `predicateWithFormat:`（纯库，编译器不参与）。
3. 宿主 API 三个方法 + Foundation 重建。
4. 测试：DSL 各运算符往返（`predicateFormat` 回读）、KVC 类链/未命中、
   多 TU（客户端弱表合并）、占位符替换、ANY/ALL 聚合；
   回归基线全套 + golden。
5. 文档：architecture.md 增 §12（KVC/谓词）、README 特性列表、ROADMAP 删行。
