# 23_notification — NPNotification 独立通知对象

P1-2：把通知从「中心内联的 (name, object) 回调」提升为**独立对象**
（对应 ObjC 的 `NSNotification`），并接进 `NPNotificationCenter`。

## 为什么块签名改成收 NPNotification

ObjC 的块式观察者 `addObserverForName:object:queue:usingBlock:` 收到的就是
`NSNotification *`，不是 name/object 两个裸参数。为对齐 ObjC，观察者块由
`^(NPString *name, id object)` 改为 `^(NPNotification *note)`，`name` /
`object` / `userInfo` 一律从通知对象上取。

改动面很小：全库真实调用点只有 `12_notify` 这一个 golden（3 处），已同步。

## 新增 API

| 来源 | 落地 |
| --- | --- |
| `+notificationWithName:object:` | 同名 |
| `+notificationWithName:object:userInfo:` | 同名 |
| `-initWithName:object:userInfo:` | 同名 |
| `-name` / `-object` / `-userInfo` | 同名 |
| `-postNotification:` | 同名（投递已构造的通知） |
| `-postNotificationName:object:userInfo:` | 同名 |

持有语义按 ObjC 的三个只读 property：`name`/`userInfo` 是 **copy**，
`object` 是 **retain**（发布者身份，不拷贝）。

## 覆盖

| 组 | 覆盖 |
| --- | --- |
| A1 | 三个属性 + 两个工厂（带/不带 userInfo，后者 `userInfo` 为 nil） |
| A2 | `name` 的 **copy** 语义：构造后改原 `NPMutableString`，通知里的名字不变 |
| A3 | `postNotification:` 直接投递，块读到 `userInfo` |
| A4 | `postNotificationName:object:userInfo:` 便捷入口等价 |
| A5 | `object` 维按**身份**过滤；注册时 `object` 为 NULL = 通配该维 |

A5 的两行：第一行 `object` 与注册对象同一实例，身份观察者与通配观察者都收到
（倒序投递：后注册先收到）；第二行换成另一个对象，只有通配观察者收到 —— 这就是
身份过滤生效的直接证据。

## 已知生成 C 警告（codegen 瑕疵，非本类缺陷）

`NPNotification` 的类方法工厂在**直接函数调用**路径上发射，该路径不按形参类型
转换实参，于是：

- 带类型的对象传给 `id` 形参（如 `object:sender`）→ clang
  `incompatible pointer types`（`id` 发射成 `NPObject *`，与 `NPString *` 是不同
  struct 指针）；
- 子类传给父类形参（如 `name:mname`，`NPMutableString *` → `NPString *`）→ 同样。

实例方法走 vtable 派发路径**不报**。ObjC 里这两种转换本就是隐式合法的，所以本
用例显式写出 `(id)` / `(NPString *)` 等价表达。这是 Jeti 的 C 发射不表达类层次
导致的普遍现象，值得单独立项修 codegen（直接调用点按形参类型补强转），不属
NPNotification 本身的问题。
