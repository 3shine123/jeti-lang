# 37_operation_queue — NPOperationQueue

**NPOperationQueue** (`include/Foundation/NPOperationQueue.hje/.jeti`) is the
`NSOperationQueue` port: a resident worker-thread pool draining a FIFO block
queue, built on `NPThread` + `NPCondition` (runtime thread layer, dual-backend
Win32 / POSIX). `maxConcurrentOperationCount` is the pool width — 1 is a serial
queue.

- `+mainQueue` (serial) / `+queue` (default width 4).
- `-addOperationWithBlock:` — enqueues (FIFO), signals one worker, and lazily
  grows the pool while `spawned < maxConcurrentOperationCount`. A full queue
  blocks the caller (fixed capacity 256 — backpressure, honest boundary).
- `-waitUntilAllOperationsAreFinished` — returns when the queue is empty **and**
  no worker is mid-operation; operations enqueued while waiting still run to
  completion (ObjC semantics).
- `-setSuspended:` gates **dispatch** — a suspended worker never takes a block
  (already-running ones finish; enqueuing still works), matching ObjC.
- `-cancelAllOperations` — cooperative: drops not-yet-started blocks and sets
  the flag; running blocks must check `-isCancelled` themselves.
- Blocks are `_Block_copy`-ed on enqueue and released after execution
  (Foundation MRC).

The probe caught one real scheduling bug before it shipped: the worker loop
checked only "queue non-empty" before taking a block, so `-setSuspended:YES`
was decorative — the enqueue `signal` handed freshly added blocks straight to
workers. The suspension predicate now gates the take itself
(`while (!quit && (count == 0 || suspended)) wait`).

Verified here (all deterministic — the result never depends on interleaving):

| case | checked |
|------|---------|
| K1 | serial FIFO: 10 × `+1`, counter is 10 after `waitUntilAllOperationsAreFinished` |
| K2 | concurrent (max 4): 100 operations, total deterministic after the wait |
| K3 | FIFO order: single worker logs exactly `0..7` |
| K4 | suspension gate: enqueue-while-suspended → `operationCount` stays 3; resume drains |
| K5 | cancel: 5 queued-while-suspended blocks are dropped, count 0, sum unchanged |

Honest boundaries (see the header): bare blocks only (no `NSOperation`
dependency graph, no per-operation completion), no priorities/QoS, fixed
capacity (a full queue blocks the enqueuer), and `-ffreestanding` has no thread
layer so the class is meaningless on bare metal.

Output is deterministic — GOLDEN-MATCH line by line.
