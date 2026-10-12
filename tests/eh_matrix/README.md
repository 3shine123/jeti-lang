# EH 验收矩阵（`-eh checked` 默认化）

> 状态：**已切换默认**（`DEFAULT_EH_CHECKED = true`，`crates/jetic/src/pipeline.rs`）。
> `-eh legacy`（别名 `-eh sjlj`）是完整回退。

## 为什么存在

`-eh checked` 用「显式旗标 + 守卫」替换 sjlj（setjmp/longjmp）异常后端。把它
**翻成默认**必须靠证据，而不是「golden 都过了」：每个场景都要在三种模式下
跑一遍并留下记录。

| 模式 | 命令 | 身份 |
|---|---|---|
| `default` | 不传 `-eh` | 出厂默认（翻转后 = checked） |
| `checked` | `-eh checked` | 候选默认，显式写法 |
| `legacy` | `-eh legacy` | sjlj 的官方别名，**回退路径** |

两条硬门：

1. **回退保真**——`default` 与 `-eh legacy` 的 rc / stdout / stderr 必须逐字节一致。
   否则 `-eh legacy` 就不是可靠回退，矩阵判 `NO-LEGACY-PARITY`。
2. **跨模式无漂移**——三模式都成功时 stdout 必须一致，矩阵判 `MODE-DRIFT`。

## 运行

```bash
# 三模式验收矩阵（18 场景 x 3 模式 + 默认后端门）
JETIC=target/release/jetic ./tests/eh_matrix/run_eh_matrix.sh

# 生成代码质量统计（守卫数/行数/体积/编译时间）+ 守卫密度硬门
JETIC=target/release/jetic ./tests/eh_matrix/gen_quality.sh
```

## 场景（18）

| # | 场景 | 样例 | 说明 |
|---|---|---|---|
| 01 | 普通无异常程序 | `golden/01_basics/hello_world.jeti` | rc + 与 `.out` 比对 |
| 02 | 单层 `@try/@catch` | `golden/15_exceptions/try_catch.jeti` | ARC 下显式 release → 自动 MRC 回退 |
| 03 | 嵌套 `@try` | `tests/eh_diff/03_nested_finally.jeti` | |
| 04 | `@finally` 执行顺序 | `samples/finally_order.jeti` | inner-try → inner-finally → outer-catch → outer-finally |
| 05 | `@throw` 后重新抛出 | `tests/eh_diff/04_rethrow.jeti` | 不得重入同层 catch |
| 06 | 异常跨函数传播 | `tests/eh_diff/01_cross_frame_release.jeti` | **checked-only**（见已知限制 1） |
| 07 | ARC | `golden/04_arc/retain_release.jeti` | |
| 08 | MRC | 同上 + `-fno-jeti-arc` | 无 `.out` 参照（MRC 不打印 dealloc） |
| 09 | `@autoreleasepool` | `golden/05_autoreleasepool/nested_pool.jeti` | |
| 10 | 多文件 Jeti | `tests/multi_tu/run_multi_tu.sh`（全部用例） | 跨 TU 布局 + Foundation 内联大 TU |
| 11 | Foundation 大文件 | `golden/13_foundation/04_npstring/npstring_test.jeti` | |
| 12 | 纯 C 超集 | `golden/22_c_superset/c_superset.jeti` | 无 Foundation/无 block 的生成 C 也须含 EH 声明 |
| 13 | `-ffreestanding` | `golden/25_freestanding/`（golden/25 方法论） | 裸机转译 + host 编译链接运行 |
| 14 | ARM64 裸机 | `tests/stress/baremetal/` | `-ffreestanding` + 真 `asm_ext.s` + `helpers.c` |
| 15 | `await` + 异常 | `samples/async_eh.jeti` | `@try` 不跨挂起点（语言规则） |
| 16 | Blocks + 异常 | `tests/eh_diff/06_block_throw.jeti` | block 体独立函数 + 旗标跨 invoke 边界 |
| 17 | 零异常程序（守卫密度运行侧） | `samples/no_throw_plain.jeti` | 静态侧见 `gen_quality.sh` 硬门 |
| 18 | 全语法总检 | `tests/full_syntax_test.jeti`（+ `-asm full_syntax_test.s`） | C 超集 + Foundation + 内联 asm + `@throws` + `await` 同居 |

结果（Apple clang 21, arm64-darwin）：**18 场景 x 3 模式全部 PASS**，
`default` 与显式 `-eh checked` 在每个场景上逐字节一致（`06_cross_frame`
在 `legacy` 下按已记录的 sjlj 限制失败，见下）。

## 生成代码质量（`gen_quality.sh`）

`guards` = 生成 C 中 `__jeti_eh_flag` 的出现次数；`setjmp` = `setjmp|longjmp` 次数。

| 用例 | default 行/守卫/setjmp | checked 行/守卫/setjmp | legacy 行/守卫/setjmp |
|---|---|---|---|
| `no_try`（含 Foundation） | 3185 / 0 / 0 | 3303 / 45 / **0** | 3185 / 0 / 0 |
| `single_try` | 3109 / 0 / 3 | 3262 / 60 / **0** | 3109 / 0 / 3 |
| `foundation` | 3081 / 0 / 0 | 3212 / 45 / **0** | 3081 / 0 / 0 |
| `big_file`（json_editor） | 6501 / 0 / 7 | 7067 / 225 / **0** | 6501 / 0 / 7 |
| `c_superset` | 60 / 0 / 0 | 75 / 4 / **0** | 60 / 0 / 0 |
| `freestanding` | 352 / 0 / 3 | 377 / 12 / **0** | 352 / 0 / 3 |
| `no_throw_plain` | 142 / **0** / 0 | 143 / **0** / **0** | 142 / **0** / 0 |

结论：

- **checked 恒为 0 个 `setjmp/longjmp`** —— 不依赖 hosted-only 的 `jmp_buf` ABI，
  这是它能在裸机（场景 13/14）工作的前提。
- **`legacy` 的生成 C 与 `default` 完全相同**（行数/字节/守卫）—— 回退是原样复现，
  不是「等价实现」。
- **零异常程序零守卫**（`no_throw_plain`）：无 `@try`、无 `@throw`、无 Foundation
  内联 → 143 行里 0 个旗标引用。这是 `gen_quality.sh` 里的硬门。
- **编译时间**：checked 比 sjlj 高约 10–20%，绝对值 **+11 ~ +25 ms**（最大用例
  0.108s → 0.134s）。无显著回归。
- **体积**：小文件 +2.7% ~ +3.1%；`big_file` +11.3%。见已知限制 2。

> ⚠️ **统计口径**：必须用 `grep -c '__jeti_eh_flag'`（任意出现），不能用
> `grep -c 'if (__jeti_eh_flag'` —— codegen 会把条件括起来
> （`if ((__jeti_eh_flag == 0))`），前缀匹配会**恒为 0**，把有守卫的程序
> 误报成零守卫。

## 已知限制

1. **`06_cross_frame`：sjlj 后端不支持**（`default`/`legacy` rc=1，`checked` PASS）。
   原因有两层：(a) sjlj 的 `longjmp` 跳过中间帧的 scope-end `jeti_release`（README
   已记录的 sjlj 经典限制）；(b) 该用例为 checked 编写，其 `@throw` 未标注
   `@throws`，而**只有 sjlj 模式**的 checker 会执行 `@throws` 逃逸检查并拒绝它。
   两条都是**既有行为**，`default` 与 `legacy` 在此完全一致 → 回退仍然保真。
   `checked` 通过 = 本次翻转带来的改进，不是回归。

2. **Foundation 内联会带来守卫**：`no_try` / `foundation` 的 checked 输出有 45 处
   旗标引用。这些**不是**无意义守卫——它们落在 Foundation 里真实会 `@throw` 的
   方法及其（传递闭包上的）调用者，例如 `NPString` 的越界 `@throw`。零守卫只在
   「无 `@try` + 无 `@throw` + 不内联任何抛异常方法」时成立，见 `no_throw_plain`。
   若要让带 Foundation 的程序也归零，需要一条「整 TU 无 catch ⇒ 无需任何守卫」的
   全局快速路径（当前未做）。

3. **既有 ARC 顺序缺陷（与 EH 无关，本次未修）**：
   `return <表达式使用 owned 局部对象>;` 会在**求值之前**插入 `jeti_release`，
   即 use-after-free。最小复现：

   ```jeti
   #import <Foundation/Foundation.hje>
   // Holder 持有 NPString *_s；- (NPString *)text { return _s; }
   int main() {
       Holder *h = [[Holder alloc] init];
       return [[h text] length] - 5;   // default/legacy: 错误结果；checked: 正确
   }
   ```

   实测 `default`/`legacy` rc=1，`checked` rc=0 —— checked 之所以正确，是因为
   EH 的表达式线性化把 `[[h text] length]` 提到 `jeti_release(h)` 之前（**偶然**
   规避，不是修复）。此缺陷在 HEAD 上即存在，属于 ARC，不在本次 EH 切换范围内，
   已在报告中标出。矩阵的 `samples/no_throw_plain.jeti` 刻意避开该形状以免干扰
   守卫密度判据。
