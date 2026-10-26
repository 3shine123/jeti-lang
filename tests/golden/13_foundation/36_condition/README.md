# 36_condition — NPCondition

**NPCondition** (`include/Foundation/NPCondition.hje/.jeti`) is the `NSCondition`
port: a built-in mutex and condition variable as one object, on the runtime
thread layer (`jeti_mutex_*` / `jeti_cond_*`, dual-backend Win32 / POSIX).

- `-lock` / `-unlock` / `-tryLock` — the built-in lock (tryLock succeeds when
  free, fails when held).
- `-wait` — releases the built-in lock, sleeps, and re-acquires before
  returning. Callers must hold the lock first and re-check their predicate in a
  `while` loop (spurious wakeups are platform-legal, same as ObjC).
- `-signal` wakes at least one waiter; `-broadcast` wakes them all.

Verified here (all handshake-shaped or timing-convergent — the result never
depends on which scheduling path interleaves):

| case | checked |
|------|---------|
| J1 | `tryLock` two states: free succeeds, held fails |
| J2 | signal handshake: worker sets state then signals; the main thread's `wait` (in a re-checking `while`) observes it |
| J3 | broadcast releases both waiters. Two arrival timings exist (worker already waiting → woken by broadcast; worker not yet waiting → sees `state==1` after locking) and both converge to `w1=w2=1` |

Honest boundaries (see the header): no `-waitUntilDate:` (bounded waits need
platform timeout semantics, v1 scope), and `-ffreestanding` builds have no
concurrency — the methods compile but `wait` is never woken, so the class is
meaningless on bare metal.

Output is deterministic — GOLDEN-MATCH line by line.
