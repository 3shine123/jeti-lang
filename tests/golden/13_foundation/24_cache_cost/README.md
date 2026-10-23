# 24_cache_cost — NPCache 的成本维度

P1-3：`NPCache` 补齐 ObjC `NSCache` 的成本维度 —— `setObject:forKey:cost:` 与
`totalCostLimit`（此前只有 `countLimit`）。

## 语义

- `setObject:forKey:cost:` 记录该键的成本；`setObject:forKey:` 等价于成本 0。
- 总成本超过 `totalCostLimit` 时，按 LRU 淘汰最久未使用者，直到不超限。
- `totalCostLimit` 为 0 = 不限（与 `countLimit` 同约定）。
- 两个维度各自独立生效：条数超 `countLimit` 与成本超 `totalCostLimit` 都会淘汰。

ObjC 的 NSCache 头文件对两个上限都注明 **"limits are imprecise/not strict"** ——
本实现按确定性的 LRU 淘汰到不超限为止，比 ObjC 严格，但不与其语义冲突（这正是
能写出确定期望输出的原因）。

## 为什么判据落在「哪些键被淘汰」

ObjC 的 `NSCache` **不公开** `totalCost`（头文件里没有该属性），所以本类同样只
提供 `totalCostLimit` 访问器，不额外造一个 ObjC 没有的 API。成本是否生效，一律
从 `objectForKey:` 是否为 nil 观察。

注意 `objectForKey:` **命中会提升 LRU**，因此每组断言都放在该组操作之后，不跨组
检查。

## 覆盖

| 组 | 覆盖 |
| --- | --- |
| A1 | `countLimit` 维度回归（原有行为不被成本维度破坏） |
| A2 | `totalCostLimit`：总成本超限淘汰最旧 |
| A3 | **同键覆盖是替换成本而非累加** |
| A4 | `removeObjectForKey:` 把成本扣回去 |
| A5 | `setObject:forKey:` 等价于成本 0 |
| A6 | `totalCostLimit=0` = 不限 |
| A7 | 单条自身就超限时保留（无从淘汰） |

A3 与 A4 是**区分性**判据：若覆盖时累加成本、或移除时不扣成本，这两组都会多淘汰
一条，输出与期望不符。A7 记录的是明确的取舍——单条自身超限时无处可淘汰，保留而
非拒绝插入（ObjC 的 limits 非强制，故不构成语义冲突）。
