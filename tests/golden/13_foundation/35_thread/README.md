# 35_thread — NPThread

**NPThread** (`include/Foundation/NPThread.hje/.jeti`) is the `NSThread` port —
a **real thread** on top of the runtime thread layer
(`jeti_thread_*`, dual-backend Win32 / POSIX in `include/jeti/runtime.c`; the
archive links it with `-fblocks`, Apple clang enables blocks by default while
Linux needs the flag explicitly).

- `-initWithBlock:` — the body block is `_Block_copy`-ed (blocks arrive on the
  stack); the object keeps its copy for the thread's lifetime.
- `-start` — spawns a real OS thread (exactly once; a second call is NO). The
  trampoline retains `self` for the run and releases it at the end.
- `-join` — waits and reclaims the handle (ObjC's NSThread has no join; this
  port adds it because waiting on a thread you started is the common shape).
- `-startDetached` / `+detachNewThreadWithBlock:` — fire-and-forget; the runtime
  reclaims the handle and the trampoline releases the object.
- `-cancel` / `-isCancelled` — **cooperative** cancellation: only the flag is
  set; the body checks `-isCancelled` and returns early (NSThread semantics —
  nothing is ever killed).

Three compiler defects surfaced while building this class, all fixed:

1. **Block-typed ivars don't parse** (`void (^_body)(void)` never entered the
   ivar table → every `_body` use was "undeclared identifier"). The ivar stores
   `void *` and the users cast.
2. **A block literal in argument position emitted a bare `^void(){…}`**, and
   jetic's `-fblocks` detection greps for `(^` — so the exact shape that needed
   the flag was the one that never got it, and clang rejected the syntax.
   Block literals now emit parenthesized (`(^void(){…})`), which is legal C and
   lets the detection see every position.
3. **`#line` directives were flattened into the middle of inline-emitted lines**
   (`{ #line 12 "f.jeti" for …` → clang "expected expression"): the
   inline-statement fallback stripped newlines *after* the directive emitter had
   run. The fallback now emits under `LINE_SUPPRESS`.

Also: `NPThread -dealloc` reclaims its handle with **detach**, not join — the
last release can come from the worker's own trampoline, and joining your own
thread is `EDEADLK` (POSIX) or a hang (Win32).

Verified here (all deterministic — no scheduling order is ever observed):

| case | checked |
|------|---------|
| I1 | `join` semantics: locked `+1000` from the body, counter reads 1000 after join |
| I2 | lifecycle states before/running/finished; a second `-start` fails |
| I3 | cooperative cancel: the flag is set **before** start, so the body exits on its first check (no race) |
| I4 | detached thread reports completion through a condition-variable handshake (blocking wait, timing-free) |

Output is deterministic — GOLDEN-MATCH line by line.
