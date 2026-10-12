# Nullability 实施方案（M1 + M2）

> 状态：**规划已定，M1 待实施**。本文是实现的单一事实来源。
> 背景：ObjC 2015 年最大的语言演进（`nullable` / `nonnull` / `NS_ASSUME_NONNULL_BEGIN`）在 Jeti 完全缺失，
> 而 Jeti 的 `nil` 安全性是核心卖点（所有消息派发都有 nil 守卫）——这是与现代 ObjC 最大的显著差距。

## 0. 为什么抄 ObjC 的三态设计，而不是布尔

ObjC 故意用**三态**而非布尔，这决定了一切：

| 态 | 拼写（方法/属性位） | 拼写（C 指针位） | 含义 |
|---|---|---|---|
| nonnull | `nonnull` | `_Nonnull` | 承诺非 nil，传 nil 是 bug |
| nullable | `nullable` | `_Nullable` | 显式可为 nil，调用方**必须**处理 |
| unspecified | （不写） | `_Null_unspecified` | **"我没表态"** —— 与"显式 unspecified"是两件事 |

第三态是整套设计的支点：

- **审计可行**：ObjC 的 `NS_ASSUME_NONNULL_BEGIN/END`（Jeti 拼写 `NP_ASSUME_NONNULL_BEGIN` / `NP_ASSUME_NONNULL_END`）把整段指针**自动**变 nonnull，区域内想例外才写 `_Nullable`。
  审计成本从 **O(声明数)** 降到 **O(1)** —— 新加声明自动获正确默认值。
- **兼容性**：老代码（无标注）默认 unspecified，零改动不报新错。这是 ObjC 能在存量代码库上增量落地的关键。
- **不撒谎**：`nullable` 的代价是调用方要处理 nil，所以它必须**显式写出来**；`unspecified` 不承担这个义务。

Jeti 处境**比 ObjC 更好**：ObjC 至少能用 `NS_ASSUME_NONNULL` 表达"这里不该传 nil"，Jeti 现在连表达这个意图的语法都没有。

## 1. 语法落点

### 1.1 拼写：接受两套，行为一致

```objc
// ObjC 风格（方法/返回值/参数位，主推）
- (nullable NPString *)find:(nonnull NPString *)key;
- (NPString * _Nonnull)name;              // C 指针位同款拼写也接受

// 属性
@property (nonatomic, copy, nullable) NPString *title;
@property (nonatomic, strong, nonnull) NPString *name;   // 标注期许 Promise 名字都不该是 nil

// 区域默认（M1 就要，见 §5）
NP_ASSUME_NONNULL_BEGIN
...声明...
NP_ASSUME_NONNULL_END
```

### 1.2 关键约束：**必须是上下文关键词**

ObjC 里 `nullable` / `nonnull` 是宏（展开成 `_Nullable`），所以 `int nullable = 5;` 在 ObjC 里能编译。
但 Jeti 若把它们登记进 `KW_TABLE` 当硬关键词，`int nullable = 5;` 就会炸 —— **违反 C 超集铁律**。

→ 复用项目既有先例（`in` / `when` / `copy` / `retain` 全部是上下文关键词，lexer 不硬编码，只在类型位识别）。
这是 Jeti 相对 ObjC 的一个真实优势：**同一语义，两套拼写，且不破坏 C 标识符可用性**。

### 1.3 落点清单

| 位 | 例子 | M1 | 承载字段 |
|---|---|---|---|
| 方法返回类型 | `- (nullable NPString *)f;` | ✅ | `CstType.nulls` |
| 方法参数 | `- (void)f:(nonnull id)x;` | ✅ | `CstType.nulls`（复用 `CstParam.par_type`） |
| 顶层函数返回/参数 | `nullable id *f(nonnull id *x);` | ✅ | 同上 |
| 属性 | `@property (nullable) NPString *t;` | ✅ | `AstDeclData::Property` 加 `nulls` |
| ivar | `@public nullable NPString *t;` | ✅ | `CstType.nulls` |
| `id` / `instancetype` 的 nullability | `nullable id` | ✅ | `CstType.nulls` |
| block 参数/返回 | `void (^)(nullable id)` | ⏭ M2 | `CstType.block_params` 递归 |
| out-param（**ObjC 的 `NSError **`，Jeti 对应类尚不存在**） | `- (BOOL)f:(NSError **)e;` | ⏭ M2 | 已知难点，见 §6 |

## 2. 三态表示

```rust
// crates/cst/src/cst.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Nullability {
    #[default]
    Unspecified,   // 未标注 —— 与"显式 unspecified"不同，见 §0
    Nonnull,
    Nullable,
    NullUnspecified, // _Null_unspecified：显式"我不管"
}
```

- 加在 `CstType.nulls` 与 `AstType.nulls`（两个类型结构体都已有 `is_weak_qual` / `is_complex` 这类附加位，加一个 enum 成本极低）
- **codegen 完全不读这个字段** —— 纯编译期，零运行时成本（与 ObjC 同构）
- 指针语义：`Nullable` 蕴含 `is_pointer`；`Nonnull` 用在非指针位（`int`）→ checker 报 `nonnull_on_non_pointer`（M1 报 warning，M2 升 error）

## 3. 诊断清单

全部接进既有 `check_warning` / `check_error` + `-Werror` 升级体系。命名对齐 clang 的 `nullable-*` 族：

| 诊断 | 触发 | 档位 | 对应 clang |
|---|---|---|---|
| `nullable_to_nonnull_conversion` | 把 `nullable` 实参传给 `nonnull` 形参 | **error** | `-Wnullable-to-nonnull-conversion` |
| `nonnull_missing_nonnull` | `NP_ASSUME_NONNULL` 区内声明了 nullable 却**没标注**（即该标 nonnull 却是 unspecified） | warning | `-Wnullability-completeness` |
| `nullable_to_nonnull_scalar` | `nullable` 值参与指针上下文但目标为 nonnull 标量 | error | 同族 |
| `nonnull_on_non_pointer` | `nonnull` 用在非指针类型 | warning | `-Wnullability-completeness` |
| `unspecified_to_nonnull` | unspecified 实参传给 nonnull 形参 | **不报**（渐进采用的核心，见 §4） | clang 默认也不报 |
| `nullability_completeness_missing` | 声明级：方法/属性在 nonnull 区内未标注 | warning | `-Wnullability-completeness` |

**不做的诊断**（避免误报洪水）：
- 不检查"函数返回 nullable 值但调用方直接解引用" —— 那是 `-Wnullable-to-nonnull-conversion` 的值域版本，ObjC 至今也不做（`NSError **` 审计争议的根源）
- 不对 block 参数/返回做流敏感检查（M2 再议）

## 4. 流敏感 narrow（M1 做最简版）

M1 只做**分支内 narrow**，这是 90% 的实际用法：

```objc
- (void)f:(nullable NPString *)s {
    if (s) {
        [s length];          // ✅ narrow 后当 nonnull
    }
    [s length];              // ⚠️ warning nullable_to_nonnull_conversion
}
```

实现：checker 现有 `scope_vars` 增一个 `narrowed: HashSet<String>`，在 `If` 的 then 分支 push/pop。
- `if (x)` / `if ([x isKindOfClass:...])` / `if (x != nil)` → narrow
- **`if (x)` 的 else 分支 narrow 成 nonnull**（`if (!x) return;` 后 x 必 nonnull）—— 这个模式极常见，M1 必做

M1 **不做**：循环内 narrow、`__autoreleasing` 标注、复杂控制流合并（保守取交集，冲突则放弃 narrow —— 与 ARC `RefVal` 的 `Unknown` 合并哲学一致）。

## 5. `NP_ASSUME_NONNULL_BEGIN` / `_END` 区域

M1 就要，因为它是 ObjC 方案的核心价值（审计成本 O(1)）。

### 5.1 拼写定案（2026-09-30 用户拍板）

```objc
NP_ASSUME_NONNULL_BEGIN
...声明...
NP_ASSUME_NONNULL_END
```

**这是真语法，不是宏** —— 这是本节最重要的事实，理由见 §5.2。它沿用 ObjC 的 `NS_ASSUME_NONNULL_*` 拼写（仅把 `NS_` 换成 Jeti 的 `NP_`），让 ObjC 开发者零学习成本认出，迁移时也便于对照苹果文档。

### 5.2 为什么不能是宏（ObjC 的实现方式在 Jeti 不可用）

ObjC 那两个标记是**宏**，内部是 `_Pragma("clang assume_nonnull begin")`。**jetic 明确拒绝宏体内的 `_Pragma`**：

```
// crates/cpp/src/lib.rs:358
"macro '{}' has '_Pragma' in its body, which jeti cannot expand — '_Pragma'
 (§6.10.9) takes effect at the invocation site during preprocessing, and a
 source-level expander has no position to place it"
```

即 ObjC 的实现机制在 Jeti 里**根本不可用**，这个标记只能是语法。负例见 `tests/negative/pragma_in_macro_body.jeti`。

### 5.3 已知代价（如实记录，勿当 bug 修）

**名字长得像宏，行为却是语法。** 后果是用户可能：

| 会尝试的动作 | 应当的行为 |
|---|---|
| 找 `#define NP_ASSUME_NONNULL_BEGIN` 的定义 | 找不到 —— 它不是宏 |
| `#undef NP_ASSUME_NONNULL_BEGIN` | 应报错/无效，不是"取消区域" |
| 把它放进 `#if` 条件 | 不参与预处理器求值 |
| 在宏体里使用 | 报错（宏体内无位置安放，见 §5.2） |

**若日后收到"这个宏找不到定义""为什么不能 undef"之类的报障，答案在本节，不是缺陷。** 若确因用户困惑需要改成语法形态（如 `@nonnull_begin`），那是 breaking change，需重新拍板。

### 5.4 无字符串歧义（已核实）

`@end` / `@endnamespace` 能共存，是因为 lexer 是**全串精确匹配**而非前缀匹配：

```rust
// crates/lexer/src/lexer.rs:144
for (kstr, kw) in KW_TABLE { if *kstr == s { return *kw; } }
```

本标记是标识符（`NP_ASSUME_NONNULL_BEGIN`），无 `@` 前缀，**根本不存在字符串层面的歧义**。

### 5.5 实施要点

- **lexer**：作为**标识符**识别（不是关键词，守 C 超集铁律）—— 两条路径都要在，且**必须原样透传进生成的 C**（clang 需要它们生效），故走既有 `RawLine` 通道
- **parser**：声明列表维护 `nonnull_region: bool`；在区内，每个**指针类型缺省标注**改为 `Nonnull`
- **区域状态按文件作用域**：进入新文件（`#import` 边界）清零，`Foundation.jth` 内部不受用户的区域状态影响
- **未闭合即 `#import`**：报 warning（`assume_nonnull region left open at #import`）

## 6. 已知难点（如实记录，不假装简单）

1. **out-param（ObjC 的 `NSError **`）** —— 这是 ObjC nullability 至今只部分检查的经典难点，也是"该特性不该一次做对"的真实刻度。Jeti Foundation 目前**没有错误类**（无 `NPError`），M1 明确不碰。
2. **block 参数/返回的 nullability** —— `CstType.block_params` 是递归结构，M1 跳过。
3. **`nullable` 与 ARC 的交互** —— ARC 的 `RefVal` 状态机把对象局部标 `Owned`；`nullable` 意味着可能为 nil，注入的 `jeti_release` 需要 nil 守卫。**必须核实 ARC 注入路径已有该守卫**，否则 `nullable` 会引入新的 over-release。
4. **跨 TU** —— 纯编译期，不影响 vtable 布局，`__sig` 不变。✅ 无风险。

## 7. 测试矩阵

| 类别 | 文件 | 覆盖 |
|---|---|---|
| 正例 | `tests/golden/45_nullability/nullability.jeti` + `.out` | 三态标注、区域默认、流敏感 narrow、block 外全部落点 |
| 负例 | `tests/negative/null_to_nonnull.jeti` | nullable → nonnull 传参，报错 |
| 负例 | `tests/negative/nonnull_on_int.jeti` | `nonnull int` 误用 |
| 负例 | `tests/negative/assume_unclosed.jeti` | 区域未闭合就 `#import` |
| C 超集 | `tests/nullable_c_superset_test.jeti` | `int nullable = 5;` / `int nonnull = 6;` / `struct { int nullable; }` 全部照常编译（**守护上下文关键词决策不被回退**） |
| 回归 | — | `cargo test` + `test_all` + multi_tu + stress 四套基线 |

## 8. 实施切分

| 阶段 | 内容 | 工作量 | 依赖 |
|---|---|---|---|
| **M1** | 三态语法（上下文关键词）+ 三处类型位 + 6 条诊断 + 分支内 narrow + `NP_ASSUME_NONNULL` 区域 | 中 | 需先核实 §6.3 的 ARC 守卫 |
| **M2** | block 参数/返回 nullability + out-param 基础检查（届时需先给 Foundation 加错误类）+ 循环内 narrow + `_Null_unspecified` 显式拼写 | 中 | M1 |
| **不做** | 完整流敏感（跨循环/复杂合并）、值域解引用检查 | — | ObjC 也没做，见 §6.1 |

## 9. 实施前置核实（✅ 已完成 2026-09-30）

**§6.3 的疑问已解答**：`nullable` 不会引入 over-release。

```c
// include/jeti/runtime.c:222
void jeti_release(NPObject *obj) {
    if (!obj) return;              // ← nil 安全，ARC 注入的 release 天然安全
    if (obj->retain_count > 0) obj->retain_count--;
    ...
}
```

`jeti_release` 自身就是 nil 安全的，所以 ARC 注入的 `jeti_release(local)` 在 `nullable` 场景下（局部为 nil）**不会**崩、不会 over-release。

⚠️ **但有一处仍需在 M1 实施时确认**（本次未核）：`crates/arc/src/arc.rs:35` 构造的 release 调用是
`FuncCall { name: "jeti_release", args: [target] }` —— 需确认这条路径**不会**被 checker 的
`nullable` 检查拦下（`jeti_release` 的参数类型是 `NPObject *`，若标为 `nonnull` 则注入调用会误报）。
**实施 M1 时第一件事**：把 runtime 内部注入的 release 调用列入诊断豁免名单（仿既有
`&JETI_CLASS_$_X` 内部引用的 `__` 前缀放行规则，见 AGENTS.md 记载的 eh desugar 豁免）。

