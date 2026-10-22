# Foundation Gap Plan — ObjC 对照缺口清单

> 对照 ObjC Foundation，列出 Jeti Foundation 尚未实现的类与能力。**Jeti 库前缀是
> `NP-`**（源自 Nupa，与语言名解耦，见 AGENTS.md 命名约定）；表内 ObjC 类名只作
> 对照来源，落地时一律取 NP- 拼写。
>
> 现有基线（27 个类）：`NPObject`/`jeti_root`、`NPString`/`NPMutableString`、
> `NPArray`/`NPMutableArray`、`NPDictionary`/`NPMutableDictionary`、
> `NPSet`/`NPMutableSet`/`NPOrderedSet`/`NPCountedSet`、`NPData`/`NPMutableData`、
> `NPDate`、`NPEnumerator`、`NPError`、`NPFileManager`、`NPPredicate`、
> `NPProcessInfo`、`NPCache`、`NPNotificationCenter`、`NPNull`、`NPValue`、
> `NPTask`、`NPTimer`、`NPRunLoop`。
>
> 新增类必须遵守：NP- 前缀；`.hje` 声明 + `.jeti` 实现成对；两个伞头同步接入；
> 低碰撞选择子不再是硬约束（同名选择子按接收者类解析），但仍避开与高频用户
> 方法名同形同签名的组合；行为对齐 ObjC，除需 ObjC 运行时动态性的部分。

## 一、集合与容器（扩展现有家族）

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSMutableOrderedSet` | `NPMutableOrderedSet<T>` | 有序集合可变版；现只有不可变 `NPOrderedSet` |
| `NSIndexSet` | `NPIndexSet` | 整数集合（区间运算：`indexesInRange:`、`enumerate:`） |
| `NSMutableIndexSet` | `NPMutableIndexSet` | 同上可变版 |
| `NSIndexPath` | `NSIndexPath` | 树/表格路径（组件序列 + hashable） |
| `NSMapTable` | `NPMapTable<K, V>` | 可配内存语义（弱 key/弱 value）的字典 |
| `NSHashTable` | `NPHashTable<T>` | 可配内存语义（弱元素）的集合 |
| `NSPointerArray` | `NPPointerArray` | 可含 NULL 的对象数组（弱引用模式） |
| `NSCache` 补齐 | `NPCache` 补齐 | 若未实现 `totalCostLimit`/`countLimit`/逐出策略则为半成品，需核对 |

## 二、字符串处理

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSCharacterSet` | `NPCharacterSet` | 字符集（扫描器/分词/trim 依赖；先做内置类别：字母、数字、空白等） |
| `NSMutableCharacterSet` | `NPMutableCharacterSet` | 同上可变版 |
| `NSScanner` | `NPScanner` | 定向扫描解析（`scanInt`/`scanUpToCharactersFromSet:` …） |
| `NSRegularExpression` | `NPRegularExpression` | 正则；无 ICU 依赖可先做简化引擎（明确不支持的语言特性写进 README） |
| `NSAttributedString` | `NPAttributedString` | 带属性字符串；属性表可先做纯数据层，GUI 渲染留给 libui |
| `NSMutableAttributedString` | `NPMutableAttributedString` | 同上可变版 |
| `NSStringTokenizer` | `NPStringTokenizer` | 分词（可先做 Unicode 空白/标点切分） |
| `NSString` 编码 API 补齐 | `NPString` 补齐 | 编码转换（UTF-8/16/32、Latin-1）、`dataUsingEncoding:` |

## 三、数值与格式化

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSDecimalNumber` | `NPDecimalNumber` | 十进制精确运算（避免二进制浮点误差；金融场景刚需） |
| `NSDecimalNumberHandler` | `NPDecimalNumberHandler` | 舍入/精度策略 |
| `NSNumberFormatter` | `NPNumberFormatter` | 数字格式化/解析（千分位、小数位、百分比） |
| `NSDateFormatter` | `NPDateFormatter` | 日期格式化/解析（先做 ISO 8601 与常见格式） |
| `NSISO8601DateFormatter` | `NPIso8601DateFormatter` | 或并入 `NPDateFormatter` |
| `NSCalendar` | `NPCalendar` | 日历运算（年/月/日拆分与构造） |
| `NSDateComponents` | `NPDateComponents` | 日期分量 |
| `NSTimeZone` | `NPTimeZone` | 时区（UTC 与本地偏移起步，完整 tzdata 后置） |
| `NSLocale` | `NPLocale` | 区域设置（起步可只暴露语言/地区标识） |
| `NSMeasurement` / `NSUnit*` | `NPMeasurement` / `NPUnit*` | 物理量纲（后置，价值密度低） |

## 四、谓词 / 表达式 / 排序

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSExpression` | `NPExpression` | 谓词的表达式层；若 `NPPredicate` 只做了比较算子则为半成品，需核对补齐 |
| `NSSortDescriptor` | `NPSortDescriptor` | 键排序描述符（`sortedArrayUsingDescriptors:`） |
| `NSComparator` 惯用法 | block 形态 | `sortedArrayUsingComparator:` 等排序 API 补进 `NPArray`/`NPMutableArray` |

## 五、并发与调度

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSLock` | `NPLock` | 互斥锁；依赖真实线程层（运行期目前单线程事件泵） |
| `NSRecursiveLock` | `NPRecursiveLock` | 同上 |
| `NSCondition` / `NSConditionLock` | `NPCondition` | 条件变量 |
| `NSThread` | `NPThread` | 线程对象 |
| `NSOperation` | `NPOperation` | 依赖图任务单元 |
| `NSOperationQueue` | `NPOperationQueue` | 队列调度（可与 NPTask/NPRunLoop 的就绪队列整合） |
| `NSInvocation` / `NSMethodSignature` | （诚实边界） | 反射调用与静态派发模型冲突；倾向明确不做，或以 block 装包形态替代 |
| `NSTimer` tolerance | `NPTimer` 补齐 | `-setTolerance:`（宽松调度提示） |

## 六、网络 / IO / 进程

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSURL` | `NPURL` | URL 解析（scheme/host/path/query 拆装；纯数据，零网络依赖，优先） |
| `NSURLComponents` | `NPURLComponents` | 结构化拆装 |
| `NSURLRequest` / `NSURLResponse` | `NPURLRequest` / `NPURLResponse` | 网络栈前置类型 |
| `NSURLSession` / `NSURLConnection` | `NPURLSession` | 真网络栈；需先引入 socket 层，量大后置 |
| `NSInputStream` / `NSOutputStream` | `NPInputStream` / `NPOutputStream` | 流式 IO |
| `NSStream` | `NPStream` | 上述两类的共同基类 |
| `NSTask`（进程版） | `NPProcess`（改名避撞） | 子进程执行；**注意** ObjC 的 `NSTask` 是子进程，Jeti 的 `NPTask` 已被 async 任务占用——落地名用 `NPProcess` |
| `NSPipe` | `NPPipe` | 进程管道 |
| `NSUUID` | `NPUUID` | UUID 生成/解析（v4 随机即可起步） |
| `NSProcessInfo` 补齐 | `NPProcessInfo` 补齐 | 环境变量、`globallyUniqueString`、操作系统版本细节核对 |

## 七、通知与 KVO/KVC

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSNotification` | `NPNotification` | 独立通知对象（name/object/userInfo）；核对 `NPNotificationCenter` 是否已内联 |
| `NSNotificationQueue` | `NPNotificationQueue` | 合并/去重投递（coalescing） |
| `NSDistributedNotificationCenter` | （诚实边界） | 跨进程通知，倾向明确不做 |
| KVO / KVC | （诚实边界） | KVO 依赖运行期 isa 摆动，与静态派发冲突，倾向不做；KVC 可做有限版（按 ivar/property 名读写） |

## 八、归档与序列化

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSCoding` / `NSSecureCoding` | `NPCoding` 协议 | 归档协议（`encodeWithCoder:` / `initWithCoder:`） |
| `NSKeyedArchiver` / `NSKeyedUnarchiver` | `NPKeyedArchiver` / `NPKeyedUnarchiver` | 键控归档 |
| `NSJSONSerialization` | `NPJSONSerialization` | JSON ↔ Foundation 对象（`NPArray`/`NPDictionary`/`NPNumber`/`NPString`/`NPNull`） |
| `NSPropertyListSerialization` | `NPPropertyListSerialization` | plist（可后置于 JSON 之后） |

## 九、其他

| ObjC 来源 | Jeti 落地名 | 说明 |
|---|---|---|
| `NSBundle` | `NPBundle` | 资源包定位（可执行文件所在目录、资源查找） |
| `NSException` | 核对现状 | Jeti 已有 throw/catch 机制；确认是否有独立异常类型，无则评估是否需要 ObjC 形态的 `NSException` |
| `NSGarbageCollector` | （不做） | ObjC 已废弃 |
| `NSDebug*` | （后置） | 调试设施 |

## 优先级（按依赖与价值密度）

1. **P0 纯数据结构、零外部依赖**：`NPIndexSet`/`NPMutableIndexSet`、
   `NPMutableOrderedSet`、`NPSortDescriptor`、`NPCharacterSet`、`NPScanner`、
   `NPUUID`、`NPURL`、`NPJSONSerialization`、数组排序 comparator API。
2. **P1 核对补齐已有半成品**：`NPCache` 逐出策略、`NPPredicate` 的
   `NPExpression` 层、`NPNotificationCenter` 的独立 `NPNotification` 对象与
   合并队列、`NPString` 编码 API。
3. **P2 需运行期能力**：`NPLock`/`NPThread`/`NPOperationQueue`（需真实线程）、
   `NPRegularExpression`（需引擎）、`NPDateFormatter`/`NPCalendar`（需日历算法）、
   `NPDecimalNumber`（需大数十进制）。
4. **P3 大工程**：`NPURLSession`（socket 层）、`NPKeyedArchiver`、
   `NPAttributedString`（联动 libui）。
5. **明确不做（诚实边界，写进 README）**：KVO、`NSInvocation` 反射、
   `NSDistributedNotificationCenter`、`NSGarbageCollector`。

## 落地纪律（每个新类都要过一遍）

- NP- 前缀；`.hje`/`.jeti` 成对；两个伞头（`Foundation.hje`/`Foundation.jeti`）接入。
- 重建 `libjetifoundation.a` 后 `.slots` 冻结清单随之增长；客户端需重编。
- golden 归入 `tests/golden/13_foundation/<NN>_<name>/`，不外层散开。
- 行为对齐 ObjC；做不到的（动态性依赖）写进"诚实边界"，不静默降级。
- 探针先行（`probes/`），实证后再写实现。
