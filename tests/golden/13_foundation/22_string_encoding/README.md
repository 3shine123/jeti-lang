# 22_string_encoding — NPString 编码转换 API

`NPString` 的编码 API 补齐（P1）：`dataUsingEncoding:`、
`lengthOfBytesUsingEncoding:`、`canBeConvertedToEncoding:`、
`initWithData:encoding:`、`initWithBytes:length:encoding:`，以及
`NPStringEncoding` 类型（取值逐值对齐 ObjC `NSStringEncoding`）。

内部存储一律是 UTF-8（`_cstr`）。转换走「UTF-8 ↔ 码点序列 ↔ 目标编码」两段式，
逐字节直转在 UTF-16/32 上必然错位（代理项、端序、BOM 都处理不了）。

## 语义来源：clang ObjC 基准，不是推测

`probes/x17_string_encoding/` 下有同题的 ObjC 基准（`objc_baseline.m`）与 Jeti
探针（`main.jeti`），输出格式完全相同，**逐字节 diff 一致**。本用例是这些语义的
可回归固化。已锁定的几条反直觉事实：

| 事实 | 实测值 |
| --- | --- |
| 主机端序变体（`UTF16`=10 / `UTF32`）**输出 BOM** | UTF16 → `FF FE 41 00 …`（8 字节） |
| 显式端序变体（BE/LE）**不输出 BOM** | UTF16BE → `00 41 00 E9 20 AC`（6 字节） |
| `lengthOfBytesUsingEncoding:` **不计 BOM** | UTF16 = 6，而 data 长 8 |
| 转换不了 → `dataUsingEncoding:` nil、长度 0、canConvert NO | ASCII/Latin-1 遇 `€` |
| 解码：显式端序**不剥 BOM**（BOM 变成 U+FEFF 字符） | `bom-explicit=￾䄀갠` |
| 解码：主机端序剥 BOM 并用它定序 | `bom-host=Aé€` |
| 解码：主机端序**无 BOM 时按大端** | `nobom-be=Aé€`、`nobom-le=䄀갠`（乱码即证据） |
| 非法字节序列 → nil | 孤立续字节 `FF 41` |

后两行输出里的乱码是**正确行为**：它们正是「按大端解读小端载荷」的结果，与 ObjC
逐字节相同。这是对齐的证据，不是 bug。

## 覆盖

| 组 | 覆盖 |
| --- | --- |
| A1 | UTF-8 编码：原始字节、无 BOM |
| A2 | ASCII / Latin-1 表示不了 → nil（不静默降级、不产出替换字符） |
| A3 | 可表示的 ASCII / Latin-1：单字节直出 |
| A4 | UTF-16：主机端序带 BOM，BE/LE 不带 |
| A5 | UTF-32：同上 |
| A6 | 代理项对：U+1F600 → UTF-16 `3D D8 00 DE` |
| A7 | `lengthOfBytesUsingEncoding:` 只算载荷、不计 BOM；转换不了 0 |
| A8 | `canBeConvertedToEncoding:` |
| A9 | 往返：UTF16BE 编码再解码回原 UTF-8 字节 |
| A10 | 解码端序四条：无 BOM 大端 / 主机端序剥 BOM / 显式端序不剥 |
| A11 | 非法字节序列 → nil |

## 诚实边界

`-length` **不在**本用例的判据里：ObjC 的 `NSString -length` 是 **UTF-16 码元数**，
而 `NPString -length` 是 **UTF-8 字节数**（`characterAtIndex:` 同样是字节下标）。
这是 NPString 既有的语义分歧，独立于编码转换，尚未对齐——探针的往返判据因此比
UTF-8 字节而不是比 `-length`，免得把两件事混在一个闸门里。
