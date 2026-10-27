# 38_attributed_string_runs — 区间属性（run）写入

**NPMutableAttributedString** (`include/Foundation/NPMutableAttributedString.hje/.jeti`)
是 `NSMutableAttributedString` 的移植：同一串的不同区间可以带不同属性。
基类 `NPAttributedString` 只读，写入全部走这个子类。

存储形状：两个等长并行数组——run 起点（升序，首项恒 0）与每 run 的属性字典；
run i 覆盖 `[starts[i], starts[i+1])`，最后一个 run 的终点是字符串长度。空串
保留一个长度 0 的 run，因此写入路径不需要特判。

覆盖点：

| 断言 | 内容 |
|---|---|
| F1 | 下标/区间按 **UTF-8 字节**计（`ab中文cd` = 10 字节）；无属性时 `attributesAtIndex:` 返回 nil（沿用 v1 语义） |
| F2 | `setAttributes:range:` 在区间两端切分 run，区间内外属性互不串味 |
| F3 | `addAttribute:value:range:` 叠加单个属性，保留该区间原有键 |
| F4 | `removeAttribute:range:` 只删指定键，其他键与区间外不受影响 |
| F5 | copy 隔离：写入后改源字典，不会回写已生效的 run |
| F6 | `replaceCharactersInRange:withString:` 覆盖区间内的边界删除、尾部边界按长度差平移，插入文本继承插入点所在 run 的属性 |
| F7 | 越界与空区间：写属性是 no-op；越界读取返回 nil / 长度 0 |

与 ObjC 的差异：下标按 UTF-8 字节而非 UTF-16 码元；写入空字典等价于清掉该
区间属性（ObjC 传 nil 会抛异常）；不做 `-appendAttributedString:` /
`-insertAttributedString:atIndex:`（跨对象拼接的属性合并策略待需求）。
渲染（NSParagraphStyle 等）仍不在本类范围内——GUI 归 libui。
