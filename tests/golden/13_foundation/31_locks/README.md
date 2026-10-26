# 31_locks — NPLock + NPRecursiveLock

**NPLock** / **NPRecursiveLock**
(`include/Foundation/NPLock.hje/.jeti`, `NPRecursiveLock.hje/.jeti`) are the
`NSLock` / `NSRecursiveLock` ports.

They reuse the runtime's own primitive family — **C11 atomics**, the same
mechanism `jeti_syncLock` uses in `include/jeti/runtime.c` — through the
clang/gcc `__atomic_*` builtins, so no pthread dependency is introduced
(macOS ships no C11 `<threads.h>`, and pthread would add a link requirement for
Linux clients). `NPRecursiveLock` identifies its owner **without pthread**: the
address of a `__thread` variable is unique per thread, and the runtime already
uses `__thread` for exception state.

- `NPLock`: `-lock` / `-unlock` / `-tryLock` / `-isLocked`, non-recursive.
- `NPRecursiveLock`: same surface, but the same thread re-entering `-lock` only
  increments a depth counter; the lock is released when the depth reaches zero.

Honest boundaries (see the headers): these are **real locks** (atomic
operations, correct under real threads), but Jeti's runtime is **cooperative
single-threaded** (async work is pumped by `jeti_task_resume`), so in a normal
program the lock is never contended. `-lock` spins rather than blocks, and
there is no `-lockBeforeDate:` (that needs a blocking primitive the runtime
does not have).

Verified here:

| case | checked |
|------|---------|
| E1 | `NPLock`: initially free, `tryLock` succeeds when free / fails when held, unlock frees it |
| E2 | `NPRecursiveLock`: three re-entrant locks hold; depth zero releases |
| E3 | `isLocked` / `tryLock` agree in both states |

Output is deterministic — GOLDEN-MATCH line by line.
