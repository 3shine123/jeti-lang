# x18_arc_audit — ARC 正确性审计探针组

针对评审列出的 ARC 风险面（对象所有权、返回值所有权、跨函数生命周期、异常路径、
闭包捕获、复杂控制流、C 接口转换）各一个探针，全部经 `-trace-refcount` 判决 +
生成 C 人工核对 + `jetic run` 运行时行为三重验证。

## 结论（2026-10-24）

**未发现新的编译器缺陷。** 本轮之前修的三项规则经受住了全部探针：

| 审计面 | 文件 | 结论 |
| --- | --- | --- |
| 返回值所有权 | `01_returns.jeti` | returned-object 规则四情形全对：+1 赋局部 freed；+1 当临时经 return 处 autorelease 平衡；分支内两个 return 都平衡；字面量方法 +0 不平衡 |
| 异常路径 | `02_exceptions.jeti` | 活局部跨 @throw 存活、循环内反复抛、finally 均平衡 |
| 闭包捕获 | `03_blocks.jeti` | 被捕获变量按 ARC 行为 escaped，不误报 |
| 复杂控制流 | `04_control.jeti` | 嵌套分支合并、循环多轮、早退、switch（结构化回退）全对；C3 的条件释放如实报 `[0,1] possibly leaked` |
| C 接口 | `05_c_interop.jeti` | 对象 ↔ void* 往返、char* ↔ NPString 互转均 freed |

## 两个已解释的显示现象（非缺陷，C 已人工核对）

1. **summary 里的 `[-1, 0] (freed)`**：区间数学在重执行路径上的精度损失——
   循环/分支合并把入口状态加宽成 `[0,1]` 后，路径上的 release 对区间整体减一
   得 `[-1,0]`。生成 C 逐一核对过：`early_ret` 的 `o` 每条路径恰好释放一次，
   循环抛出每轮 create/autorelease/retain/release/池排空平衡。**判决（freed）
   可信；下界数字是伪影。** 真正的双重释放走具体路径、count 变负，以内联
   红字 `! double-release` 报出——那才是需要立刻看的信号。按「误报不可」
   原则不把区间伪影升级成疑点。
2. **`! jeti_retain on untracked __jeti_eh_val` 噪声**：EH 机制对抛出值的
   retain/autorelease 落在合成临时名（`__jeti_eh_val`）上，追踪器解析不到；
   其后的 release 能剥回创建点。纯外观，平衡性由 C 核对保障。

## 覆盖边界

- **跨编译单元所有权约定**：选择子身份全局、签名按类解析（AGENTS.md 铁律），
  元数据/布局由 `tests/multi_tu` 套件覆盖；所有权约定是编译期约定，不随 TU 变。
- **循环引用**：强引用环需 weak 语义参与，属语言运行期特性，超出静态追踪器
  职责；`@weak` 变量的 cleanup 行为由既有测试覆盖。
