# 32 — Foundation 类型分发三件套（isKindOfClass: / respondsToSelector: / isEqual:）

ObjC 多路分发的标准词汇补齐（**Sep 2026**，路线图待做 #1）。

| 文件 | 期望 |
|------|------|
| `foundation_dispatch_test.jeti` | 编译通过，stdout 见 `.out` |

```bash
./target/debug/jetic run tests/golden/32_foundation_dispatch/foundation_dispatch_test.jeti
```

## 三个方法，三种实现路径

| 方法 | 声明位置 | 实现路径 | 改动层 |
|------|----------|----------|--------|
| `isKindOfClass:` | `NPObject.hje` 的 `jeti_root`（全类继承） | runtime helper `jeti_isKindOfClass` 沿 `isa->superclass` 链比较 | runtime.c + Foundation |
| `respondsToSelector:` | **无声明**（编译器伪方法） | codegen MsgSend 特判 → `jeti_resp_<member>(recv)`：`__o && ((struct jeti_vtable *)__o->isa->vtable)->member != 0` | codegen |
| `isEqual:` | `NPObject.hje` 的 `jeti_root` | 根类默认指针相等（同 NSObject）；`NPString`/`NPNumber` 按内容重写 | Foundation |

## respondsToSelector: 设计要点（编译器伪方法）

- **不声明、不占 vtable 槽位**：声明它反而会生成一个 `NPObject_respondsToSelector_` 函数并被派发；正确形态是仿 `[obj class]` 的 codegen 特判，接收者只求值一次。
- **统一 vtable 是前提**：selector 与 vtable 成员名 1:1（冒号转下划线，`sanitize_sel_name`），槽位检查是编译期已知的成员访问。
- **NULL 槽位语义**：`propagate_protocol_methods` 注入的协议声明存根、声明于 `@interface` 但实现于另一 TU 的方法 → 槽位为 NULL → NO。这与 ObjC 运行时行为一致。
- **未知 selector → 常量 0**：selector 未命名本 TU 任何实例方法时，它不是 `struct jeti_vtable` 的成员，helper 无从引用；静态派发下答案是确定地 NO，直接发射 `0`。
- **nil 接收者安全**：`__o && ...` 短路，返回 NO。
- **按需发射**：helper（static）只在本 TU 真的出现对应 send 时才发射，无 `-Wunused-function` 噪音。成员集在 `convert_expr` 期间登记（`RESP_HELPERS`），体在 `emit_unit_with_headers` 的 vtable struct 定义之后发射（彼时成员才存在）。
- ⚠️ **转换顺序坑**：`convert_expr` 跑在 `METHOD_METADATA.set` 之前，特判不能用 `METHOD_METADATA` 判断 selector 是否存在——要用已构建的 `class_infos`。

## conformsToProtocol: — 未实现（评估结论）

`NPClass` 已有 `protocols/protocol_count` 字段但 codegen 恒写 0（`jeti_metaInit`）。填充需要 codegen 拿到每个类的协议 required 方法表，而协议表在 symtab、`ast_to_cg_unit` 不携带 symtab——需先把协议元数据导进 AST/CgClassMeta（涉跨 TU 布局，因为协议方法已通过 `propagate_protocol_methods` 进槽位）。**单独评估，暂不做**；编译期等价物是 checker 的协议一致性检查（golden/31）。

## isEqual: 说明

`jeti_root` 给的默认实现是**指针身份**（同 NSObject）；`NPString` 与 `NPNumber` 各自重写为**值相等**（`isEqualToString:` / `isEqualToNumber:`，ObjC 的 NSString/NSNumber 正是如此）。值语义在这里仍是语义保证（2026-09-30 更新：字面量 interning 已落地，同内容字面量现为同一对象，但跨类的值相等仍靠 `isEqual:` 重写）：`[a isEqual:b]` 现在返回 1（本条 golden 的期望据此从 `eq other=0` 改为 `eq other=1`）。

这个改动是 `@{...}` 字典键查找的前提——按身份查键的话，用新写的 `@"key"` 字面量去查永远落空。子类仍可按需重写 `isEqual:`（重写时同时考虑 `hash` 一致性）。

## 命名纪律

`isKindOf:`（非标准历史拼写）保留作兼容别名；新代码一律用 ObjC 官方拼写 `isKindOfClass:`。
