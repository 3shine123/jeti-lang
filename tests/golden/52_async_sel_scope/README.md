# 52_async_sel_scope — async selectors resolve per receiver class

The async pass used to collect async method **names** globally
(`async_sels`), so when two classes shared a selector — `AsyncA` with
`- (async int)run`, `SyncB` with `- (int)run` — ANY `await [b run]` with a
`SyncB *` receiver was driven as a task handle: a scalar return was a hard
clang error (`jeti_task_await(int)`), and an object-returning sync method
was silent UB at runtime (the object pointer handed to `jeti_task_await`
as a `NPTask *`).

Fixed to the same discipline as the checker's `method_kinds_by_class` and
codegen's per-class dispatch casts: the receiver's own static class chain
decides (`self` / locals / params, including locals declared inside async
block bodies and thunks); the name-keyed global set survives only as the
fallback for receivers whose class is not statically known (`id`).

| case | checked |
|------|---------|
| A1 | `SyncB *b` local + awaited `[b run]` — NOT driven (rule 6 warning is expected), value correct |
| A2 | `[self run]` inside `Probe` (sync, same-named async elsewhere) — no rule-2 error, Probe's own signature used |
| A3 | real async `[x run]` — still driven, value correct |
| A4 | plain sync `[y run]` without await — value correct |

Output deterministic — GOLDEN-MATCH line by line. Probe:
`probes/x10_async_sel_scope/`.
