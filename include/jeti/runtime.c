#include "jeti/runtime.h"
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <time.h>

// ─── JETI_CLASS_$_jeti_root (defined weak; codegen's jeti_metaInit fills it) ──

__attribute__((weak)) NPClass JETI_CLASS_$_jeti_root;

// ─── Protocol conformance (D5.2) ─────────────────────────────────────────────

/* Does `set` of protocol `p` (recursively including parents) require every
   required method that `target` requires? Name comparison only, colon-stripped
   (the selector symbol convention). */
static int proto_covers(const struct NPProtocol *p, const struct NPProtocol *target);

static int proto_requires(const struct NPProtocol *p, const char *sel) {
    for (int i = 0; i < p->required_count; i++) {
        if (strcmp(p->required_methods[i].name, sel) == 0) return 1;
    }
    for (int i = 0; i < p->parent_count; i++) {
        if (p->parents[i] && proto_requires(p->parents[i], sel)) return 1;
    }
    return 0;
}

static int proto_covers(const struct NPProtocol *p, const struct NPProtocol *target) {
    if (!p || !target) return 0;
    if (p == target) return 1;
    if (p->name && target->name && strcmp(p->name, target->name) == 0) return 1;
    /* structural: p requires everything target requires (with parents) */
    for (int i = 0; i < target->required_count; i++) {
        if (!proto_requires(p, target->required_methods[i].name)) return 0;
    }
    return 1;
}

static int class_conforms(const NPClass *cls, const struct NPProtocol *proto, int depth) {
    if (!cls || !proto || depth > 32) return 0;
    for (int i = 0; i < cls->protocol_count; i++) {
        if (cls->protocols[i] && proto_covers(cls->protocols[i], proto)) return 1;
    }
    /* walk the superclass chain — conformance is inherited */
    return cls->superclass ? class_conforms(cls->superclass, proto, depth + 1) : 0;
}

int jeti_class_conformsToProtocol(NPClass *cls, struct NPProtocol *proto) {
    return class_conforms(cls, proto, 0);
}

void jeti_register_category_protocols(NPClass *cls, struct NPProtocol **protocols, int count) {
    if (!cls || !protocols || count <= 0) return;
    int old_count = cls->protocol_count;
    int add = 0;
    for (int i = 0; i < count; i++) {
        int duplicate = 0;
        for (int j = 0; j < old_count; j++) {
            if (cls->protocols && cls->protocols[j] == protocols[i]) { duplicate = 1; break; }
        }
        if (!duplicate) add++;
    }
    if (!add) return;
    struct NPProtocol **merged = malloc((size_t)(old_count + add) * sizeof(*merged));
    if (!merged) abort();
    for (int i = 0; i < old_count; i++) merged[i] = cls->protocols[i];
    int n = old_count;
    for (int i = 0; i < count; i++) {
        int duplicate = 0;
        for (int j = 0; j < n; j++) if (merged[j] == protocols[i]) { duplicate = 1; break; }
        if (!duplicate) merged[n++] = protocols[i];
    }
    cls->protocols = merged;
    cls->protocol_count = n;
}

// ─── Exception globals ────────────────────────────────────────────────────────

#ifdef __JETI_FREESTANDING
jmp_buf __jeti_exception_buf;
id      __jeti_exception_value;
#else
__thread jmp_buf __jeti_exception_buf;
__thread id     __jeti_exception_value;
#endif

// ─── Checked-exception (Swift-scheme) error flag ──────────────────────────────

#ifdef __JETI_FREESTANDING
int __jeti_eh_flag;
id   __jeti_eh_val;
#else
__thread int __jeti_eh_flag;
__thread id   __jeti_eh_val;
#endif

int __jeti_eh_isa(NPObject *obj, NPClass *cls) {
    if (!obj || !cls) return 0;
    return jeti_isKindOf(obj, cls) ? 1 : 0;
}

// Uncaught checked exception: an exception escaped `main` (flag still armed at
// the function-tail guard of main). Mirror ObjC's wording on stderr, then
// abort — the process MUST NOT exit 0 with an exception in flight.
void jeti_eh_uncaught(void) {
    const char *cls = (__jeti_eh_val && __jeti_eh_val->isa && __jeti_eh_val->isa->name)
        ? __jeti_eh_val->isa->name : "?";
    fprintf(stderr, "*** Terminating app due to uncaught exception of class '%s'\n", cls);
    abort();
}

// ─── Weak reference side table ───────────────────────────────────────────────

#define MAX_WEAK_ENTRIES 1024
#define INITIAL_SLOT_CAPACITY 4

typedef struct {
    NPObject *object;
    NPObject ***slots;
    int count;
    int capacity;
} WeakEntry;

static WeakEntry weak_table[MAX_WEAK_ENTRIES];
static int weak_entries = 0;

static WeakEntry *find_entry(NPObject *target) {
    for (int i = 0; i < weak_entries; i++) {
        if (weak_table[i].object == target)
            return &weak_table[i];
    }
    return NULL;
}

void jeti_weakRegister(NPObject **weak_loc, NPObject *target) {
    if (!target || !weak_loc) return;
    WeakEntry *entry = find_entry(target);
    if (!entry) {
        if (weak_entries >= MAX_WEAK_ENTRIES) return;
        entry = &weak_table[weak_entries++];
        entry->object = target;
        entry->slots = malloc(INITIAL_SLOT_CAPACITY * sizeof(NPObject **));
        entry->count = 0;
        entry->capacity = INITIAL_SLOT_CAPACITY;
    }
    if (entry->count >= entry->capacity) {
        entry->capacity *= 2;
        entry->slots = realloc(entry->slots, entry->capacity * sizeof(NPObject **));
    }
    entry->slots[entry->count++] = weak_loc;
}

void jeti_weakUnregister(NPObject **weak_loc) {
    if (!weak_loc) return;
    for (int i = 0; i < weak_entries; i++) {
        WeakEntry *entry = &weak_table[i];
        for (int j = 0; j < entry->count; j++) {
            if (entry->slots[j] == weak_loc) {
                entry->slots[j] = entry->slots[--entry->count];
                return;
            }
        }
    }
}

void jeti_weakClearAll(NPObject *target) {
    if (!target) return;
    for (int i = 0; i < weak_entries; i++) {
        WeakEntry *entry = &weak_table[i];
        if (entry->object == target) {
            for (int j = 0; j < entry->count; j++) {
                *entry->slots[j] = NULL;
            }
            free(entry->slots);
            weak_table[i] = weak_table[--weak_entries];
            return;
        }
    }
}

void jeti_weakAutoCleanup(void *ptr) {
    jeti_weakUnregister((NPObject **)ptr);
}

// ─── @synchronized monitors ──────────────────────────────────────────────────
//
// Global bucket array of spinlocks keyed by object address (see runtime.h).
// Hosted implementation: C11 atomics — `atomic_flag` test_and_set is a lock
// primitive on every platform clang targets here, no pthread dependency, no
// allocation, no table-growth ceiling. A thread that returns from
// jeti_syncLock owns bucket[b] until the matching jeti_syncUnlock.

#include <stdatomic.h>

#define JETI_SYNC_BUCKETS 256

static atomic_flag jeti_sync_flags[JETI_SYNC_BUCKETS];

static unsigned jeti_sync_hash(void *object) {
    unsigned long v = (unsigned long)(uintptr_t)object;
    unsigned hash = 0x811C9DC5u;
    for (unsigned i = 0; i < sizeof(void *); i++) {
        hash ^= (unsigned char)(v & 0xFFu);
        hash *= 0x01000193u;
        v >>= 8;
    }
    return hash % JETI_SYNC_BUCKETS;
}

long jeti_syncLock(void *object) {
    unsigned b = jeti_sync_hash(object);
    while (atomic_flag_test_and_set_explicit(&jeti_sync_flags[b], memory_order_acquire)) {
        // spin
    }
    return (long)b;
}

void jeti_syncUnlock(long bucket) {
    if (bucket < 0 || bucket >= JETI_SYNC_BUCKETS) return;
    atomic_flag_clear_explicit(&jeti_sync_flags[bucket], memory_order_release);
}

void jeti_syncAutoCleanup(void *ptr) {
    jeti_syncUnlock(*(long *)ptr);
}

// ─── Selectors ───────────────────────────────────────────────────────────────

SEL sel_registerName(const char *name) {
    unsigned hash = 0x811C9DC5;
    for (const char *p = name; *p; p++) {
        hash ^= (unsigned char)*p;
        hash *= 0x01000193;
    }
    SEL sel = { name, hash };
    return sel;
}

// ─── Type introspection ─────────────────────────────────────────────────────────

BOOL jeti_isKindOf(NPObject *obj, NPClass *cls) {
    if (!obj || !cls) return 0;
    NPClass *isa = obj->isa;
    while (isa) {
        if (isa == cls) return 1;
        isa = isa->superclass;
    }
    return 0;
}

/* Official ObjC spelling of isKindOf: (kept as a compatible alias).
 * Same isa-chain walk. */
BOOL jeti_isKindOfClass(NPObject *obj, NPClass *cls) {
    return jeti_isKindOf(obj, cls);
}

// ─── Autorelease pool ─────────────────────────────────────────────────────────

struct jeti_autoreleasepool {
    struct jeti_autoreleasepool *next;
    NPObject **objects;
    int count;
    int capacity;
};

static __thread jeti_autoreleasepool_t *current_pool = NULL;

jeti_autoreleasepool_t *jeti_autoreleasepoolPush(void) {
    jeti_autoreleasepool_t *pool = calloc(1, sizeof(jeti_autoreleasepool_t));
    if (!pool) return NULL;
    pool->next = current_pool;
    current_pool = pool;
    return pool;
}

void jeti_autoreleasepoolPop(jeti_autoreleasepool_t *pool) {
    if (!pool) return;
    for (int i = 0; i < pool->count; i++) {
        jeti_release(pool->objects[i]);
    }
    free(pool->objects);
    current_pool = pool->next;
    free(pool);
}

// ─── Lifecycle ───────────────────────────────────────────────────────────────

NPObject *jeti_alloc(NPClass *cls) {
    if (!cls) return NULL;
    NPObject *obj = (NPObject *)calloc(1, cls->instance_size);
    if (obj) {
        obj->isa = cls;
        obj->retain_count = 1;
    }
    return obj;
}

NPObject *jeti_init(NPObject *self) {
    return self;
}

// ─── Refcount debug trace (JETI_REFCOUNT_DEBUG) ─────────────────────────────
// Purely a debug aid: reads the env var once, then prints every retain /
// release event to stderr. Default OFF — zero behavior change when unset.
// Static language note: this is plain C control flow baked in at compile
// time; it adds no runtime dynamism to the language itself.
static int jeti_rc_debug = -1;   // -1 = not yet resolved

static int jeti_rc_debug_enabled(void) {
    if (jeti_rc_debug < 0)
        jeti_rc_debug = getenv("JETI_REFCOUNT_DEBUG") != NULL;
    return jeti_rc_debug;
}

static void jeti_rc_trace(const char *op, NPObject *obj, uint32_t rc_after, const char *note) {
    const char *cls = (obj->isa && obj->isa->name) ? obj->isa->name : "?";
    if (note)
        fprintf(stderr, "[rc] %s %p %s rc=%u (%s)\n", op, (void *)obj, cls, rc_after, note);
    else
        fprintf(stderr, "[rc] %s %p %s rc=%u\n", op, (void *)obj, cls, rc_after);
}

NPObject *jeti_retain(NPObject *obj) {
    if (!obj) return NULL;
    obj->retain_count++;
    if (jeti_rc_debug_enabled())
        jeti_rc_trace("retain", obj, obj->retain_count, NULL);
    return obj;
}

void jeti_release(NPObject *obj) {
    if (!obj) return;
    if (obj->retain_count > 0)
        obj->retain_count--;
    if (jeti_rc_debug_enabled()) {
        // Under-counting release (already at 0) is a bug worth flagging.
        const char *note = (obj->retain_count == 0 && obj->isa && obj->isa->dealloc)
            ? "dealloc" : NULL;
        jeti_rc_trace("release", obj, obj->retain_count, note);
    }
    if (obj->retain_count == 0) {
        // Zero weak references BEFORE dealloc: dealloc may free other objects
        // (strong ivars) whose memory holds a weak slot pointing back to us
        // (e.g. a child's `__weak parent`). Zeroing first avoids a use-after-free.
        jeti_weakClearAll(obj);
        // Call dealloc so ivar cleanup runs. dealloc's `[super dealloc]` calls
        // the parent's dealloc directly (not jeti_release), so no double-free.
        if (obj->isa && obj->isa->dealloc) {
            obj->isa->dealloc(obj, (SEL){ .name = "dealloc", .hash = 0xD9929EB3 });
        }
        free(obj);
    }
}

NPObject *jeti_autorelease(NPObject *obj) {
    if (!obj) return obj;
    jeti_autoreleasepool_t *pool = current_pool;
    if (!pool) return obj;
    if (pool->count >= pool->capacity) {
        pool->capacity = pool->capacity ? pool->capacity * 2 : 16;
        pool->objects = realloc(pool->objects, pool->capacity * sizeof(NPObject *));
        if (!pool->objects) return obj;
    }
    pool->objects[pool->count++] = obj;
    return obj;
}

// ─── String literals ──────────────────────────────────────────────────────────
// jeti_stringFromCstr is emitted by the codegen in the generated C code.
// The runtime.h declaration is used by the generated code to call it.
// When NPString is not present, @"..." falls back to a regular C string literal.

// ─── Async tasks (route map item #4) ────────────────────────────────────────

// Cooperative single-thread async: the desugared method pumps its own task
// to completion, so a task's lifetime is fully contained in the call that
// created it (milestone 1). `parent` exists for milestone 2 (task graphs
// where a suspended task resumes its awaiter).

/* Allocator injection: NULL = malloc/free defaults (jeti_task_set_allocator
 * swaps in a static pool on bare metal). Read through function pointers so
 * the switch can happen before the first task is created. */
static jeti_task_alloc_fn task_alloc = NULL;   /* set lazily to malloc */
static jeti_task_free_fn  task_free_fn = NULL; /* set lazily to free */

static void ensure_default_allocator(void) {
    if (!task_alloc) task_alloc = malloc;
    if (!task_free_fn) task_free_fn = free;
}

void jeti_task_set_allocator(jeti_task_alloc_fn alloc, jeti_task_free_fn free_fn) {
    task_alloc = alloc;
    task_free_fn = free_fn;
}

NPTask *jeti_task_create(jeti_task_entry_fn entry, NPObject *self_obj, size_t frame_size) {
    ensure_default_allocator();
    NPTask *t = (NPTask *)task_alloc(sizeof(NPTask));
    if (!t) return NULL;
    t->state = 1;   /* state 1 = the entry's first case; 0 means "not started" */
    t->finished = 0;
    t->failed = 0;  /* §9 try-await channel: garbage here makes every await
                       look failed (ASan/scribble exposed exactly that) */
    t->entry = entry;
    t->self_obj = self_obj;
    t->frame = frame_size ? task_alloc(frame_size) : NULL;
    t->result = NULL;
    t->parent = NULL;
    t->queued = 0;  /* scheduler idempotency flag reads this before enqueue */
    return t;
}

int jeti_task_resume(NPTask *task) {
    if (!task || task->finished) return 1;
    if (task->entry) {
        if (task->entry(task) != 0) {
            task->finished = 1;
        }
    } else {
        task->finished = 1;
    }
    return task->finished ? 1 : 0;
}

void jeti_task_finish(NPTask *task) {
    if (task) task->finished = 1;
}

/* try-await error propagation (§9): mark the task failed. `result` carries
 * the error object; the awaiting `try await` site re-arms __jeti_eh_flag
 * from `failed` (checked EH), so the failure propagates outward through the
 * ordinary EH machinery — including @finally. A PLAIN await ignores `failed`
 * and keeps returning `result` (the checker already forces `try` on failable
 * methods, so this path only exists for defense in depth). */
void jeti_task_fail(NPTask *task, NPObject *err) {
    if (!task) return;
    task->failed = 1;
    task->result = (void *)err;
    task->finished = 1;
}

void *jeti_task_join(NPTask *task) {
    if (!task) return NULL;
    while (!task->finished) {
        (void)jeti_task_resume(task);
    }
    void *result = task->result;
    if (task->frame) free(task->frame);
    free(task);
    return result;
}

// ─── NPTask scheduling layer (doc/async_nptask_plan.md, stage C) ────────────
// Lazy tasks + a cooperative ready queue. Mechanism only — the pump is never
// invoked implicitly here; hosted entry wrappers (stage D) call
// jeti_sched_run explicitly, bare-metal main loops own their own pump.

/* READY → RUNNING → SUSPENDED → … → DONE, as a tiny explicit queue. */
enum { TASK_READY = 0, TASK_QUEUED = 1, TASK_DONE = 2 };

static NPTask *sched_head = NULL;
static NPTask *sched_tail = NULL;
static NPTask *sched_current = NULL;

NPTask *jeti_task_current(void) { return sched_current; }

/* try-await propagation helper (§9), emitted around every `try await` point by
 * the async desugar pass. The runtime's jeti_task_await has already armed
 * __jeti_eh_flag when the child failed; this marks the CURRENT task failed
 * with the same error so an enclosing task's await sees the failure too. The
 * flag is deliberately left armed — the EH pass's call-site guards after this
 * statement (`if (__jeti_eh_flag) return <zero>;`) and the @finally exit
 * splices route the failure outward. When the flag is clear this is a no-op
 * (a mere suspension must not fail the task). The value passthrough around
 * this call is the `__jeti_try_await_passthrough` macro in runtime.h. */
void __jeti_try_await_propagate(void) {
    if (!__jeti_eh_flag) return;
    NPTask *cur = sched_current;
    if (!cur) return; /* top-level drive: no task to mark; flag stays armed */
    cur->failed = 1;
    cur->result = (void *)__jeti_eh_val;
}

static void sched_enqueue(NPTask *t) {
    t->parent = NULL;          /* reuse `parent` as the queue link */
    if (sched_tail) sched_tail->parent = t;
    else sched_head = t;
    sched_tail = t;
    t->queued = 1;
}

int jeti_task_start(NPTask *task) {
    if (!task || task->finished) return task != NULL;
    if (task->queued) return 1;                 /* idempotent */
    sched_enqueue(task);
    return 1;
}

void jeti_task_mark_ready(NPTask *task) {
    (void)jeti_task_start(task);
}

void jeti_sched_run(void) {
    /* due timers first: a timer callback may enqueue tasks, and the queue
     * below is what carries them; timers added during that queue run wait
     * for the next pump */
    jeti_sched_poll();
    while (sched_head) {
        NPTask *t = sched_head;
        sched_head = t->parent;
        if (!sched_head) sched_tail = NULL;
        t->queued = 0;
        if (t->finished) continue;
        sched_current = t;
        (void)jeti_task_resume(t);
        sched_current = NULL;
        /* finished tasks are dropped (frame/task freed by stage-D desugar at
         * the join point; legacy join still owns its own lifetime) */
    }
}

void *jeti_task_await(NPTask *task) {
    if (!task) return NULL;
    if (task == sched_current) {
        /* self-await cycle: fatal by design (no thread to make progress) */
        fprintf(stderr, "fatal: task awaits itself\n");
        abort();
    }
    (void)jeti_task_start(task);
    /* Blocking drive, top-level or in-task alike (M2 synchronous-drive
     * model): cooperative single-thread — an in-task await on an independent
     * nested task can drive it to completion inline; only a cycle deadlocks,
     * and direct self-await is checked above. */
    while (!task->finished) {
        if (sched_head) { jeti_sched_run(); continue; }
        (void)jeti_task_resume(task);
    }
    /* try-await error propagation (§9): a failed child re-arms the checked-EH
     * flag with its error value. The awaiting site's ordinary EH call-site
     * guard then routes the failure outward (@catch / @finally / further
     * try-await frames) — exactly like a synchronous @throw, and a SUSPENDED
     * await (child merely unfinished when a segment returns) never arms it,
     * so @finally does not fire on suspension (§9.4). */
    if (task->failed) {
        __jeti_eh_flag = 1;
        __jeti_eh_val = (id)task->result;
        return NULL;
    }
    return task->result;
}

// ─── Timer scheduling layer (Foundation/NPTimer) ─────────────────────────────
// Deadlined callbacks beside the ready queue: a sorted intrusive singly-linked
// list, no locks, no allocation beyond the timer nodes. The clock defaults to
// the host's monotonic clock; jeti_sched_set_clock(NULL) restores it, any
// other function replaces it (tests use a virtual clock this way).

static uint64_t (*timer_clock)(void) = NULL;

static uint64_t jeti_host_clock_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

void jeti_sched_set_clock(uint64_t (*fn)(void)) {
    timer_clock = fn ? fn : jeti_host_clock_ms;
}

struct jeti_timer_s {
    uint64_t deadline;              /* absolute ms in the clock's domain */
    uint64_t period;                /* 0 = one-shot */
    jeti_timer_fn fn;
    void *user;
    int valid;                      /* 0 once fired (one-shot) or cancelled */
    struct jeti_timer_s *next;
};

static struct jeti_timer_s *timer_head = NULL;

static void timer_insert(struct jeti_timer_s *t) {
    /* sorted by deadline; equal deadlines keep insertion order (FIFO) */
    struct jeti_timer_s **link = &timer_head;
    while (*link && (*link)->deadline <= t->deadline) link = &(*link)->next;
    t->next = *link;
    *link = t;
    t->valid = 1;
}

jeti_timer jeti_sched_add_timer(uint64_t delay_ms, uint64_t period_ms,
                                jeti_timer_fn fn, void *user) {
    if (!fn) return NULL;
    struct jeti_timer_s *t = malloc(sizeof(*t));
    if (!t) return NULL;
    t->deadline = timer_clock() + delay_ms;
    t->period = period_ms;
    t->fn = fn;
    t->user = user;
    t->next = NULL;
    timer_insert(t);
    return t;
}

void jeti_sched_cancel_timer(jeti_timer timer) {
    if (!timer || !timer->valid) return;
    struct jeti_timer_s **link = &timer_head;
    while (*link && *link != timer) link = &(*link)->next;
    if (*link) {
        *link = timer->next;
        timer->next = NULL;
    }
    timer->valid = 0;   /* the node outlives the schedule: isValid stays answerable */
}

int jeti_sched_timer_valid(jeti_timer timer) {
    return timer ? timer->valid : 0;
}

void jeti_sched_poll(void) {
    if (!timer_clock) return;   /* no time source: nothing is ever due */
    uint64_t now = timer_clock();
    while (timer_head && timer_head->deadline <= now) {
        /* detach BEFORE firing: the callback may add or cancel freely, and
         * cancelling itself must be a no-op on the firing timer */
        struct jeti_timer_s *t = timer_head;
        timer_head = t->next;
        t->next = NULL;
        t->valid = 0;
        t->fn(t->user);
        if (t->period) {
            /* repeating: re-arm on the ORIGINAL cadence — missed beats
             * coalesce to the one late fire just delivered, and the schedule
             * does not drift (NSTimer's model: never burst-compensate) */
            uint64_t now2 = timer_clock();
            do { t->deadline += t->period; } while (t->deadline <= now2);
            timer_insert(t);
        }
    }
}

int jeti_sched_has_work(void) {
    return sched_head != NULL || timer_head != NULL;
}

/* HOSTED ONLY: the NSRunLoop-run shape. Sleeps until the nearest deadline
 * (bounded so a wake-up race still re-polls promptly), pumps timers and the
 * ready queue, exits when both drain. Freestanding has no definition — the
 * bare-metal owner owns the wait. */
void jeti_sched_run_loop(void) {
    for (;;) {
        jeti_sched_poll();
        jeti_sched_run();
        if (!jeti_sched_has_work()) return;
        uint64_t now = timer_clock();
        uint64_t deadline = timer_head ? timer_head->deadline : now;
        if (deadline > now) {
            uint64_t sleep_ms = deadline - now;
            if (sleep_ms > 100u) sleep_ms = 100u;   /* new timers still get seen */
            struct timespec ts = {
                .tv_sec = (time_t)(sleep_ms / 1000u),
                .tv_nsec = (long)(sleep_ms % 1000u) * 1000000L,
            };
            nanosleep(&ts, NULL);
        }
    }
}

// ─── Logging ─────────────────────────────────────────────────────────────────

// NPLog takes a Foundation `NPString *` format. The `struct NPString` layout is
// generated by the transpiler (from NPString.hje's `@public` ivars), so runtime.c
// mirrors that layout here to reach `_cstr` without depending on the generated
// header. Keep in sync with NPString.hje (isa/retain_count base + _cstr/_length/
// _hash/_hashIsValid).
struct __jeti_npstring_layout {
    void *isa;
    uint32_t retain_count;
    char *_cstr;
    size_t _length;
    uint32_t _hash;
    int _hashIsValid;
};

void NPLog(NPString *format, ...) {
    const char *cstr = format ? ((struct __jeti_npstring_layout *)format)->_cstr : "";
    va_list args;
    va_start(args, format);
    vfprintf(stderr, cstr ? cstr : "", args);
    va_end(args);
    fprintf(stderr, "\n");
}

void __NPLogv(NPString *format, va_list args) {
    const char *cstr = format ? ((struct __jeti_npstring_layout *)format)->_cstr : "";
    vfprintf(stderr, cstr ? cstr : "", args);
    fprintf(stderr, "\n");
}

// ─── KVC lookup (NPPredicate support) ───────────────────────────────────────

// Walk obj's isa chain and scan each class's JETI_KVC_$_<Class> table for
// `key`. Codegen emits the tables (see doc/nppredicate_plan.md §2); classes
// compiled without KVC emission leave the field NULL and the walk continues
// to the superclass. The getter itself is an ordinary static vtable dispatch,
// so this is table-driven lookup, never reflection.
id jeti_kvc_value(id obj, const char *key) {
    if (!obj || !key) return NULL;
    // Every object (NPObject or jeti_root) starts with the isa pointer.
    NPClass *cls = *(NPClass **)obj;
    for (int depth = 0; cls && depth < 64; cls = cls->superclass, depth++) {
        const jeti_kvc_entry *e = cls->kvc_entries;
        if (!e) continue;
        for (; e->key; e++) {
            if (strcmp(e->key, key) == 0) {
                return e->get(obj);
            }
        }
    }
    return NULL;
}

// ─── Process arguments (NPProcessInfo -arguments) ───────────────────────────
// Read from the platform, not from `main`: jetic never synthesizes main, so a
// library cannot otherwise see the program's arguments. Index 0 is the program
// path, matching ObjC's NSProcessInfo.arguments.
#if defined(__APPLE__)
#include <crt_externs.h>
int jeti_argc(void) {
    return *_NSGetArgc();
}
const char *jeti_argv(int index) {
    int n = *_NSGetArgc();
    if (index < 0 || index >= n) return NULL;
    return (*_NSGetArgv())[index];
}
#elif defined(__linux__)
// /proc/self/cmdline holds the arguments NUL-separated. Split once into a
// cached array; absent or unreadable (no procfs) leaves argc 0.
static char  *jeti_argv_blob = NULL;
static char **jeti_argv_ptrs = NULL;
static int    jeti_argc_cache = 0;

static void jeti_argv_load(void) {
    if (jeti_argv_blob) return;
    FILE *f = fopen("/proc/self/cmdline", "rb");
    if (!f) return;
    size_t cap = 4096, len = 0;
    char *buf = (char *)malloc(cap);
    if (!buf) { fclose(f); return; }
    for (;;) {
        if (len + 1024 > cap) {
            size_t ncap = cap * 2;
            char *nb = (char *)realloc(buf, ncap);
            if (!nb) break;
            buf = nb;
            cap = ncap;
        }
        size_t got = fread(buf + len, 1, cap - len, f);
        if (got == 0) break;
        len += got;
    }
    fclose(f);
    if (len == 0) { free(buf); return; }
    int n = 0;
    for (size_t i = 0; i < len; i++) if (buf[i] == '\0') n++;
    if (buf[len - 1] != '\0') n++;          // last entry unterminated
    if (n == 0) { free(buf); return; }
    char **ptrs = (char **)malloc(sizeof(char *) * (size_t)(n + 1));
    if (!ptrs) { free(buf); return; }
    int k = 0;
    ptrs[k++] = buf;
    for (size_t i = 0; i + 1 < len; i++) {
        if (buf[i] == '\0') ptrs[k++] = buf + i + 1;
    }
    jeti_argv_blob = buf;
    jeti_argv_ptrs = ptrs;
    jeti_argc_cache = k;
}

int jeti_argc(void) {
    jeti_argv_load();
    return jeti_argc_cache;
}

const char *jeti_argv(int index) {
    jeti_argv_load();
    if (index < 0 || index >= jeti_argc_cache) return NULL;
    return jeti_argv_ptrs[index];
}
#else
// Documented platform gap (see the header): no capture path is wired up here,
// so callers see an empty argument list rather than a guess.
int jeti_argc(void) { return 0; }
const char *jeti_argv(int index) { (void)index; return NULL; }
#endif
