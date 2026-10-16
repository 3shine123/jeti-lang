# 11_vtable_private_slots — 稳定槽位（3b/R3）的验收用例

## 这个用例在证明什么

两个 TU 分工完全合法：

* `lib.jeti` 实现 `Widget`，外加一个**头文件没声明**的私有方法 `privateHelper`；
* `main.jeti` 只通过 `model.hje` 的声明调用 `Widget`，另外定义自己的类 `App`。

R1/R2/R3 合力下的预期行为：

* 公共段（`show` 等）槽位两侧一致（R1）→ sig 相同（R3 只吃公共段）→ 运行期**不**中止；
* `privateHelper` 的派发只发生在 lib（R2：lib 拥有 `Widget`，实例是强符号）；
* `App` 的派发只发生在 main（R2：main 拥有 `App`）。

## 期望行为

```text
run=42
```

退出码 0（`expected.txt` 逐字比对）。

## 注意与 09_layout_mismatch 的区别

`09` 里两个 TU **各自在主文件实现同一个类**——那是真正的非法用法，R2 在链接期以
`duplicate symbol` 拒绝。本用例是**不同类 + 公共声明一致**的合法场景。

> 状态：**正向 PASS**（R1+R2+R3 全链验证通过，2026-10-03）。原 `EXPECT_FAIL` /
> `EXPECT_FAIL_MATCH` 标记已删除，输出按 `expected.txt` 逐字比对。
> 摘牌过程暴露并修掉的两个缺口见 `docs/plans/stable_slots_plan.md` §10"验证链连带修复"。
