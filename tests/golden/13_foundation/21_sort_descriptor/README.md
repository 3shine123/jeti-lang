# 21_sort_descriptor — NPSortDescriptor + 多键排序

`NPSortDescriptor` 是 ObjC `NSSortDescriptor` 的**无 KVC 子集**：Jeti 静态派发
没有 `valueForKey:` 运行期查表，所以描述符不按 key 反射取值，而是持有调用方给的
**比较块** `int (^)(id, id)`（语义同 `NPArray sortedArrayUsingComparator:`）。
`key` 只是标签：进 description、帮调用方对齐多键次序，不参与取值。

多键排序：描述符数组前面的优先；前级平局落次键；全级平局保持原相对顺序（稳定）。
比较走 `-compareObject:toObject:`，升降序在那里折算。

| 组 | 覆盖 |
| --- | --- |
| A1 | 单键升序 `sortedArrayUsingDescriptors:` |
| A2 | `ascending:0` 降序；`-reversedSortDescriptor` 与之等价；`-key`/`-ascending` 访问器 |
| A3 | 稳定性：首键全平局时保原相对顺序 |
| A4 | 多键决胜：主键平局落次键，次键也平局保原序 |
| A5 | 原地 `sortUsingDescriptors:`；nil/空描述符数组 no-op（原数组不动） |
| A6 | 无比较块的描述符：身份退化（同对象 0，否则地址序）+ 访问器 |

**诚实边界**（与 ObjC 的差异，非降级）：无 KVC，`+sortDescriptorWithKey:ascending:`
只存标签；ObjC 的 `compare:` 选择子路径（运行期按名反射）不存在。

实现要点：key 是 alloc-init 持有的 `NPString`（`jeti_release` 释放）；比较块按
NPTimer 先例存 `id`（`_Block_copy` 持有），调用点强转。
