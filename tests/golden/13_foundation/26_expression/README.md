# 26_expression — NPExpression 表达式对象层

P1-5：补上 `NPPredicate` 缺的 **表达式对象层**（对应 ObjC 的 `NSExpression`）。

## 核对结论：NPPredicate 本身不是半成品

计划原文是「若 `NPPredicate` 只做了比较算子则为半成品，需核对补齐」。核对结果：
它**不止**比较算子 —— 已有 `AND`/`OR`/`NOT`、字符串算子（`BEGINSWITH`
`ENDSWITH` `CONTAINS` `LIKE` `MATCHES`，带 `[c]`/`[d]` 修饰）、`BETWEEN`、`IN`、
`ANY` 聚合，实现里也没有任何 TODO/未实现标记。所以缺的不是比较算子，而是 ObjC 那
层**可编程构造的表达式对象**。

## 落地范围：只做可静态表达的四类

`NSExpressionType` 取值逐值对齐系统头文件：

| ObjC 来源 | Jeti 落地名（NP- 前缀） | 值 | 落地 |
| --- | --- | --- | --- |
| `NSConstantValueExpressionType` | `NPConstantValueExpressionType` | 0 | ✅ `+expressionForConstantValue:` |
| `NSEvaluatedObjectExpressionType` | `NPEvaluatedObjectExpressionType` | 1 | ✅ `+expressionForEvaluatedObject` |
| `NSVariableExpressionType` | `NPVariableExpressionType` | 2 | ✅ `+expressionForVariable:` |
| `NSKeyPathExpressionType` | `NPKeyPathExpressionType` | 3 | ✅ `+expressionForKeyPath:` |
| 函数 / 集合运算 | — | 4–7 | ❌ 需运行期按名解析符号 |
| 子查询 / 聚合 / anyKey / 块 / 条件 | — | 13/14/15/19/20 | ❌ 同上 |

**不做**的类型是明确边界而非静默降级：它们都要运行期按名解析符号，与 Jeti 的静态
派发模型冲突（与 KVO、`NSInvocation` 同一条理由）。

## 键解析与 NPPredicate 同源

键路径经 `jeti_kvc_value` 走**编译期生成的 KVC 访问器表**
（`JETI_KVC_$_<Class>`），不做运行期反射。键不满足 KVC 时**响亮中止**，与
`NPPredicate` 同款取舍 —— 静态派发下没有可注入异常的运行期，静默返回 nil 会把
「键写错」变成难以察觉的空值。

## 覆盖

| 组 | 覆盖 |
| --- | --- |
| A1 | 常量表达式：`type=0`、`constantValue`、`predicateFormat`（字符串常量带单引号） |
| A2 | 键路径：`type=3`，经 KVC 表取到 int 键（`age`）与字符串键（`name`） |
| A3 | 变量：`type=2`，从 context 绑定字典取值；format 形如 `$x` |
| A4 | 被求值对象：`type=1`，求值即返回传入对象本身 |

## 写作约定

- **类方法调用点的实参强转已由 codegen 修好**（本轮）。此前给 `NPString *` 形参
  传 `@"..."`（发射成 `jeti_stringFromCstr()`，返回 `NPObject *`）、传带类型对象、
  传子类都会报 incompatible pointer types，只能靠 `[NPString stringWithUTF8String:]`
  之类的写法绕开。根因是 `CLASS_METHOD_METADATA` 只登记**实例**方法，类方法查不到
  形参类型；现已补 `CLASS_METHOD_PARAMS` 表，类方法调用点与实例路径一样按形参类型
  强转。本用例保留 `[NPString stringWithUTF8String:]` 只是因为它同时是更明确的
  写法，不再是必需。
- 释放带类型的 ivar / 局部对象时先落到局部 `id`：直接写 `(NPObject *)ivar` 会触发
  checker 的 "cast from smaller integer type 'int'" 警告（局部 `id` 不会）。
- **初始化赋值的强转也已修好**（本轮）。此前 `id x = [obj stringReturningMethod]`
  会报 incompatible pointer types：checker 解析不出 `[obj stringReturningMethod]`
  的返回类型时回落到 `id`（即 `NPObject *`），而 codegen 的派发按 vtable 元数据发出
  真实返回类型（`NPString * (*)(...)`）的函数指针强转，于是语句表达式的 C 类型是
  `NPString *`、局部是 `NPObject *`，两边对不上。修法是把「对象指针声明 + Call
  初始化」一律强转到声明类型（checker 已校验赋值合法，故强转恒安全）。
