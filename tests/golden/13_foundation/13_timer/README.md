# 13_timer — NPTimer / NPRunLoop + the runtime timer layer

**NPTimer** (`include/Foundation/NPTimer.hje/.jeti`) is the block-shaped
`NSTimer` port: `+scheduledTimerWithTimeInterval:repeats:block:`,
`+timerWithTimeInterval:...`, `-fire`, `-invalidate`, `-isValid`,
`-timeInterval` (seconds — `NSTimeInterval` parity). It sits on a new
**timer scheduling layer** in the runtime (`include/jeti/runtime.h`):

- a sorted intrusive list of deadlined callbacks beside the task ready queue
  — mechanism only, no locks, no libc;
- an **injectable monotonic millisecond clock** (`jeti_sched_set_clock`);
  hosted installs one by default, freestanding none (until injected, no timer
  is ever due — the bare-metal owner owns the tick source like they own the
  pump);
- `jeti_sched_poll()` fires due timers (non-blocking), `jeti_sched_run()`
  drains timers first, then the ready queue, `jeti_sched_has_work()` is the
  run-loop exit condition. **The user-facing pump is `NPRunLoop`** —
  `[[NPRunLoop mainRunLoop] run]` (hosted only) blocks and sleeps
  between deadlines until the registry and queue both drain (the
  `[[NSRunLoop mainRunLoop] run]` shape); `-poll` is the GUI/embedded
  host's entry. Same-name selectors on user classes
  (`- (async void)run`, `+ (void)run`) coexist legally: the `+`/`-`
  kind and the dispatch-cast signature resolve along the receiver's
  own class chain. `jeti_sched_*` never appears in application code.

Semantics verified here, all ObjC-aligned:

| case | checked |
|------|---------|
| A1 | one-shot: not early, fires once, `-isValid` 1→0, never re-fires |
| A2 | repeating: one tick per period on the original cadence; `-invalidate` stops it |
| A3 | `-fire`: triggers the block without touching the schedule |
| A4 | unscheduled (`+timerWithTimeInterval:`): `-isValid` 0, `-fire` a no-op |
| B | `[[NPRunLoop mainRunLoop] run]` drains a real-clock timer and returns |

Repeating timers re-arm on their original cadence — missed beats coalesce to
a single late fire, the schedule never drifts (NSTimer's model, never
burst-compensated).

Determinism: parts A use a **virtual clock** injected through
`jeti_sched_set_clock` (bridged through file-level C functions — a bare
function name passed from Jeti codegen calls it instead of taking its
address), part B uses the real clock via the blocking pump. The probe that
pinned these values first: `probes/x7_nptimer/`. Built and run through the
precompiled-Foundation multi-TU flow (`-ljetifoundation`).
