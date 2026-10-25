// Dead code is a build failure, not a warning: a function nobody calls is
// either a bug or a leftover, and both should surface at compile time rather
// than rot unnoticed. Mark intentional exceptions with #[allow(dead_code)]
// and a comment saying who will use it.
#![deny(dead_code)]
//! Static reference-count trace for Jeti.
//!
//! Runs on the AST *after* ARC analysis has inserted its `jeti_release(x)`
//! calls, so the trace shows exactly where ARC releases objects. For each
//! retained object the tracer prints a chronological, color-coded line:
//!
//! ```text
//!   3:7   [[Cat alloc] init]          Cat#1: 1
//!   4:9   jeti_release(cat)           Cat#1: 0
//! ```
//!
//! Color rule (per object, compared to that object's previous printed count):
//! increased → green, decreased (still alive) → blue, freed (reached 0) →
//! cyan, error (over-released / leak) → red, first print / unchanged → plain.

use std::collections::HashMap;
use jeti_ast::{AstUnit, AstDecl, AstDeclData, AstStmt, AstStmtData, AstStmtKind, AstExpr, AstExprData};
use jeti_cst::{CstParam, CstType, TypePrim};
use jeti_cst::source_map::SourceMap;
use jeti_cfg::cfg_build;
use std::collections::{HashSet, VecDeque};

const GREEN: &str = "\x1b[32m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

#[derive(Clone)]
pub struct TraceOptions {
    /// Loop bodies are simulated this many times (default 2).
    pub max_iters: usize,
    /// Emit ANSI colors (default true; disable when stdout is piped).
    pub color: bool,
}

impl Default for TraceOptions {
    fn default() -> Self {
        TraceOptions { max_iters: 2, color: true }
    }
}

#[derive(Clone, PartialEq)]
struct TraceObj {
    label: String,
    /// Alive-count ceiling across the paths joined so far (`count` IS the
    /// ceiling after a join, and the exact value while tracing one path).
    count: i32,
    /// Alive-count floor across the paths joined so far. `count > 0` with
    /// `min_count <= 0` means "alive on some paths only" — the distinction
    /// between a definite and a possible leak.
    min_count: i32,
    last_count: Option<i32>,
    line: usize,
    col: usize,
    is_param: bool,
    escaped: bool,
}

#[derive(Clone, PartialEq)]
struct SimState {
    objects: Vec<TraceObj>,
    class_counters: HashMap<String, usize>,
    bindings: Vec<HashMap<String, usize>>,
    pools: Vec<Vec<usize>>,
}

impl SimState {
    fn new() -> Self {
        SimState {
            objects: Vec::new(),
            class_counters: HashMap::new(),
            bindings: vec![HashMap::new()],
            pools: Vec::new(),
        }
    }
    fn resolve(&self, name: &str) -> Option<usize> {
        for scope in self.bindings.iter().rev() {
            if let Some(&id) = scope.get(name) {
                return Some(id);
            }
        }
        None
    }
    fn bind(&mut self, name: &str, id: usize) {
        if let Some(scope) = self.bindings.last_mut() {
            scope.insert(name.to_string(), id);
        }
    }
}

pub struct Tracer<'a> {
    opts: TraceOptions,
    st: SimState,
    out: String,
    scope_line: usize,
    scope_col: usize,
    /// The pipeline's shared SourceMap.
    ///
    /// Node positions in the AST are FLATTENED post-preprocessing line numbers
    /// (`SourceMap::span_from_node` documents that convention), so printing
    /// them raw produced positions the reader cannot find in their own source —
    /// and that shifted by one for every line added to any imported header.
    /// Resolving here is also what the project's source-location rule asks for:
    /// one projection at the boundary, never a parallel line-number mechanism.
    sm: Option<&'a SourceMap>,
    /// Same-TU method ownership table (P2-1), keyed by colon-stripped
    /// selector. Built once per unit in `trace_unit`.
    own: HashMap<String, MethodOwn>,
    /// Names captured by block literals in the body currently being traced
    /// (P2-2), harvested per body in `trace_body_with_params`.
    captured: Vec<String>,
    /// True while a CFG revisit re-executes a block purely to refine its
    /// state: bookkeeping continues, printing stops, so the fixpoint never
    /// duplicates events (P2-3).
    mute: bool,
    /// Allocation-site abstraction (P2-3): one abstract object per
    /// (line, col, class) creation site. The CFG fixpoint re-runs loop bodies
    /// — without this memo every silent pass minted a fresh `Cat#N` phantom
    /// and the state never converged. Object identity per SITE is also the
    /// semantically right model: loop iterations share one abstract object,
    /// so "released every iteration" comes out freed, not [0, N].
    site_objects: HashMap<(usize, usize, String), usize>,
}

impl<'a> Tracer<'a> {
    pub fn new(opts: TraceOptions, sm: Option<&'a SourceMap>) -> Self {
        Tracer {
            opts,
            st: SimState::new(),
            out: String::new(),
            scope_line: 0,
            scope_col: 0,
            sm,
            own: HashMap::new(),
            captured: Vec::new(),
            mute: false,
            site_objects: HashMap::new(),
        }
    }

    // ─── top-level entry ────────────────────────────────────────────────────

    pub fn trace_unit(&mut self, unit: &AstUnit) {
        self.own = build_own_table(unit);
        self.header(&unit.filename);
        for d in &unit.decls {
            self.trace_decl(d);
        }
        self.summary();
    }

    fn header(&mut self, filename: &str) {
        let dim = if self.opts.color { DIM } else { "" };
        let reset = if self.opts.color { RESET } else { "" };
        self.out.push_str(&format!(
            "{}== Refcount trace (static simulation): {}{}\n",
            dim, filename, reset
        ));
        self.out.push_str(&format!(
            "{}   green=count increased · blue=count decreased · cyan=freed{}\n",
            dim, reset
        ));
        self.out.push('\n');
    }

    // ─── declarations ───────────────────────────────────────────────────────

    fn trace_decl(&mut self, d: &AstDecl) {
        match &d.data {
            AstDeclData::Function { params, body, .. } => {
                self.trace_body_with_params(params.as_deref(), body.as_deref());
            }
            AstDeclData::Method { params, body, .. } => {
                self.trace_body_with_params(params.as_deref(), body.as_deref());
            }
            AstDeclData::Class { methods, .. } => {
                for m in methods {
                    self.trace_decl(m);
                }
            }
            AstDeclData::Namespace(decls) => {
                for x in decls {
                    self.trace_decl(x);
                }
            }
            _ => {}
        }
    }

    fn trace_body_with_params(&mut self, params: Option<&CstParam>, body: Option<&AstStmt>) {
        // P2-2: mirror the ARC pass's block-capture harvest so captured
        // locals get the same lifetime treatment the compiler gave them.
        let saved = std::mem::take(&mut self.captured);
        if let Some(b) = body {
            self.captured = harvest_captures(b);
        }
        self.st.bindings.push(HashMap::new());
        self.bind_params(params);
        if let Some(b) = body {
            self.trace_body_cfg(b);
        }
        self.st.bindings.pop();
        self.captured = saved;
    }

    /// P2-3: abstract interpretation over the CFG the ARC pass already builds
    /// (`cfg_build`) — a worklist over blocks, interval states joined at every
    /// merge point. Events print on a block's FIRST visit only; revisits
    /// re-run silently and only refine the verdict (the fixpoint).
    ///
    /// Bodies using `goto`/`switch` keep the structured walker: the builder
    /// wires no edges for them, so interpreting that graph would silently
    /// drop code and mint false leaks.
    fn trace_body_cfg(&mut self, body: &AstStmt) {
        if body_has_unwired_shape(body) {
            self.trace_stmt(body);
            return;
        }
        let cfg = cfg_build(body);
        let Some(entry) = cfg.entry else {
            self.trace_stmt(body);
            return;
        };
        let saved = self.st.clone();
        let mut in_state: HashMap<i32, SimState> = HashMap::new();
        let mut out_state: HashMap<i32, SimState> = HashMap::new();
        let mut processed_in: HashMap<i32, SimState> = HashMap::new();
        let mut visited: HashSet<i32> = HashSet::new();
        let mut queue: VecDeque<i32> = VecDeque::new();
        in_state.insert(entry, saved.clone());
        queue.push_back(entry);
        // Safety valve: interval merges only widen, so the fixpoint
        // terminates on its own; the budget just caps a pathological graph.
        let mut budget = 4096_i32;
        while let Some(bid) = queue.pop_front() {
            budget -= 1;
            if budget <= 0 {
                break;
            }
            let Some(block) = cfg.blocks.get(bid as usize) else { continue };
            // A block can sit in the queue several times (one enqueue per
            // refining predecessor). The in-state entry must SURVIVE the first
            // visit — removing it made every later pop run on an EMPTY state
            // whose garbage output then overwrote the good one. Skip instead
            // when nothing refined since the last run.
            let Some(entry_st) = in_state.get(&bid).cloned() else { continue };
            if processed_in.get(&bid) == Some(&entry_st) {
                continue;
            }
            processed_in.insert(bid, entry_st.clone());
            let first_visit = visited.insert(bid);
            self.st = entry_st;
            self.mute = !first_visit;
            for s in &block.stmts {
                self.cfg_exec_stmt(s);
            }
            self.mute = false;
            let out = std::mem::replace(&mut self.st, SimState::new());
            out_state.insert(bid, out.clone());
            for succ in [block.true_succ, block.false_succ].into_iter().flatten() {
                let merged = match in_state.get(&succ) {
                    Some(prev) => {
                        let mut m = out.clone();
                        merge_states(&mut m, prev);
                        m
                    }
                    None => out.clone(),
                };
                let refine = in_state.get(&succ) != Some(&merged);
                in_state.insert(succ, merged);
                if refine || !visited.contains(&succ) {
                    queue.push_back(succ);
                }
            }
        }
        // Program end = every reachable terminal block (no successors): the
        // exit block, `return`/`@throw` blocks, dead-end branches. Their
        // out-states merge into the summary state — which is how early
        // returns now stop later code from being counted on their path.
        let mut final_st: Option<SimState> = None;
        for b in &cfg.blocks {
            if b.true_succ.is_none() && b.false_succ.is_none() {
                if let Some(o) = out_state.get(&b.id) {
                    match &mut final_st {
                        None => final_st = Some(o.clone()),
                        Some(f) => merge_states(f, o),
                    }
                }
            }
        }
        self.st = final_st.unwrap_or(saved);
    }

    /// Execute one statement inside a CFG block. Statements the builder
    /// unrolled into separate blocks (branches and loops) print as markers
    /// only — walking them here would double-trace their bodies, which the
    /// worklist reaches through the graph edges. Everything else (plain
    /// statements, `return`/`@throw`, `@autoreleasepool`, `try`, `@noarc`) is
    /// atomic in the graph and runs through the structured handlers.
    fn cfg_exec_stmt(&mut self, s: &AstStmt) {
        match s.kind {
            AstStmtKind::If
            | AstStmtKind::While
            | AstStmtKind::Do
            | AstStmtKind::For
            | AstStmtKind::ForIn
            | AstStmtKind::Switch => self.marker(s.line, s.col, "branch"),
            _ => self.trace_stmt(s),
        }
    }

    fn bind_params(&mut self, params: Option<&CstParam>) {
        let mut p = params;
        while let Some(cp) = p {
            if let Some(name) = &cp.name {
                if is_obj_cst_type(cp.par_type.as_deref()) {
                    let class = cp
                        .par_type
                        .as_ref()
                        .and_then(|t| t.name.clone())
                        .unwrap_or_else(|| "id".into());
                    let id = self.st.objects.len();
                    self.st.objects.push(TraceObj {
                        label: format!("{} (param)", class),
                        count: 1,
                        min_count: 1,
                        last_count: None,
                        line: 0,
                        col: 0,
                        is_param: true,
                        escaped: false,
                    });
                    self.st.bind(&name.clone(), id);
                }
            }
            p = cp.next.as_deref();
        }
    }

    // ─── statements ─────────────────────────────────────────────────────────

    fn trace_stmt(&mut self, s: &AstStmt) {
        match &s.data {
            AstStmtData::Compound(stmts) => {
                let saved_line = self.scope_line;
                let saved_col = self.scope_col;
                if s.line > 0 {
                    self.scope_line = s.line;
                    self.scope_col = s.col;
                }
                self.st.bindings.push(HashMap::new());
                for st in stmts {
                    self.trace_stmt(st);
                }
                self.st.bindings.pop();
                self.scope_line = saved_line;
                self.scope_col = saved_col;
            }
            AstStmtData::Expr(e) => self.trace_expr_stmt(e),
            AstStmtData::Decl(d) => self.trace_decl_stmt(d),
            AstStmtData::If { then, else_, .. } => {
                self.marker(s.line, s.col, "if");
                let snap = self.st.clone();
                self.trace_stmt_persist(then);
                match else_ {
                    Some(el) => {
                        // Join both branch results; the old code reset to the
                        // snapshot, traced `else`, and kept ONLY its state, so
                        // a release in the `then` branch vanished from the
                        // verdict.
                        let then_state = std::mem::replace(&mut self.st, snap);
                        self.marker(el.line, el.col, "else");
                        self.trace_stmt_persist(el);
                        self.merge_from(&then_state);
                    }
                    None => {
                        // No `else`: the fall-through path leaves the entry
                        // state untouched, so join with it.
                        self.merge_from(&snap);
                    }
                }
            }
            AstStmtData::While { body, .. } => self.trace_loop(body, "while"),
            AstStmtData::Do { body, .. } => self.trace_loop(body, "do"),
            AstStmtData::For { init, body, .. } => {
                if let Some(i) = init {
                    self.trace_stmt(i);
                }
                self.trace_loop(body, "for");
            }
            AstStmtData::ForIn { body, .. } => self.trace_loop(body, "for-in"),
            AstStmtData::Autoreleasepool(body) => {
                self.marker(s.line, s.col, "@autoreleasepool");
                self.st.pools.push(Vec::new());
                self.trace_stmt(body);
                let pool = self.st.pools.pop().unwrap_or_default();
                for id in pool {
                    self.apply_release(s.line, s.col, id, "pool pop");
                }
            }
            AstStmtData::NoArc(body) => self.trace_stmt(body),
            AstStmtData::Synchronized { body, .. } => self.trace_stmt(body),
            AstStmtData::Try { try_block, catches, finally_block } => {
                self.trace_stmt(try_block);
                for c in catches {
                    self.trace_stmt(c);
                }
                if let Some(f) = finally_block {
                    self.trace_stmt(f);
                }
            }
            AstStmtData::Catch { param, body } => {
                self.bind_catch_param(param);
                self.trace_stmt(body);
            }
            AstStmtData::Return(expr) => {
                if let Some(e) = expr {
                    self.trace_escape(e);
                }
            }
            AstStmtData::Throw(expr) => {
                if let Some(e) = expr {
                    self.trace_escape(e);
                }
            }
            AstStmtData::Switch { body, .. } => self.trace_stmt(body),
            AstStmtData::Case { body, .. } => self.trace_stmt(body),
            AstStmtData::Default(b) => self.trace_stmt(b),
            _ => {}
        }
    }

    /// Trace a branch body so that bindings made inside it (e.g. in the `then`
    /// of a bare `if`) survive past the branch. Unlike `trace_stmt`, a leading
    /// `Compound` is inlined into the current scope instead of pushing a frame
    /// that is then discarded.
    fn trace_stmt_persist(&mut self, s: &AstStmt) {
        if let AstStmtData::Compound(stmts) = &s.data {
            for st in stmts {
                self.trace_stmt(st);
            }
        } else {
            self.trace_stmt(s);
        }
    }

    /// The value leaving `return expr` / `@throw expr` transfers its ownership
    /// to the caller or to the runtime (`escaped = true`), so it is no longer a
    /// candidate for a leak inside the traced scope. Trace any ref-op inside the
    /// expression first (e.g. `return [x autorelease]`).
    fn trace_escape(&mut self, e: &AstExpr) {
        let produced = match &e.data {
            AstExprData::VarRef { name, .. } => self.st.resolve(name),
            AstExprData::MsgSend { receiver, selector, .. } => {
                let s = selector.as_str();
                match s {
                    "retain" | "autorelease" => {
                        // The send's value is the receiver object.
                        let id = self.refop(e.line, e.col, selector, receiver);
                        id
                    }
                    "release" => {
                        self.refop(e.line, e.col, s, receiver);
                        None
                    }
                    _ if is_creation(e) => Some(self.create(e, false)),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(id) = produced {
            if id < self.st.objects.len() {
                self.st.objects[id].escaped = true;
            }
        }
    }

    fn bind_catch_param(&mut self, param: &CstParam) {
        if let Some(name) = &param.name {
            if is_obj_cst_type(param.par_type.as_deref()) {
                let class = param
                    .par_type
                    .as_ref()
                    .and_then(|t| t.name.clone())
                    .unwrap_or_else(|| "id".into());
                let id = self.st.objects.len();
                self.st.objects.push(TraceObj {
                    label: format!("{} (catch)", class),
                    count: 1,
                    min_count: 1,
                    last_count: None,
                    line: 0,
                    col: 0,
                    is_param: true,
                    escaped: true,
                });
                self.st.bind(name, id);
            }
        }
    }

    /// One pass of the loop body, joined with the entry state.
    ///
    /// The old model restored the snapshot after every iteration, discarding
    /// the body's net effect — a release inside the loop looked like it never
    /// happened, so the object stayed "possibly alive" at exit. A `while`/`for`
    /// exit state is "ran zero or more times", i.e. entry ∪ one pass; a `do`
    /// body always runs at least once, so it keeps the pass state unjoined.
    /// (`max_iters` no longer multiplies passes: re-tracing from a restored
    /// snapshot only duplicated events, it never changed the verdict.)
    fn trace_loop(&mut self, body: &AstStmt, kind: &str) {
        self.marker(0, 0, kind);
        if kind == "do" {
            self.trace_stmt(body);
        } else {
            let snap = self.st.clone();
            self.trace_stmt(body);
            self.merge_from(&snap);
        }
    }

    fn trace_decl_stmt(&mut self, d: &AstDecl) {
        let mut cur = Some(d);
        while let Some(cd) = cur {
            if let AstDeclData::Variable { init: Some(init), is_static, next, .. } = &cd.data {
                if is_creation(init) {
                    let id = self.create(init, false);
                    if *is_static {
                        // `static` singletons live for the whole process; their
                        // owner is the global state, not this scope.
                        if id < self.st.objects.len() {
                            self.st.objects[id].escaped = true;
                        }
                    }
                    if let Some(n) = &cd.name {
                        self.bind_var(n, id);
                    }
                } else if let Some(id) = self.refop_if_any(cd, init) {
                    // e.g. `Cat *other = [cat retain];` — retain traced, new
                    // name aliases the same object.
                    if let Some(n) = &cd.name {
                        self.bind_var(n, id);
                    }
                } else if self.bind_owned_result(cd, init) {
                    // Cross-function ownership (P2-1): the call's +1/+0
                    // convention is known, so the result is tracked instead
                    // of degrading to "untracked" at scope end.
                } else if let Some(v) = self.var_expr_id(init) {
                    if let Some(n) = &cd.name {
                        self.bind_var(n, v);
                    }
                }
                cur = next.as_deref();
                continue;
            }
            break;
        }
    }

    /// Bind a local and apply capture semantics (P2-2): a variable referenced
    /// from a block literal is NOT released at scope exit — the ARC pass's
    /// `insert_exit_cleanup` deliberately skips captured names (the block may
    /// run after the scope dies), so the block owns the lifetime. The trace
    /// mirrors that: a captured binding is escaped, not a leak candidate.
    fn bind_var(&mut self, name: &str, id: usize) {
        self.st.bind(name, id);
        if self.captured.iter().any(|c| c == name) {
            if let Some(o) = self.st.objects.get_mut(id) {
                o.escaped = true;
            }
        }
    }

    fn refop_if_any(&mut self, cd: &AstDecl, init: &AstExpr) -> Option<usize> {
        if let AstExprData::MsgSend { receiver, selector, .. } = &init.data {
            match selector.as_str() {
                "retain" | "autorelease" => {
                    return self.refop(cd.line, cd.col, selector, receiver);
                }
                _ => {}
            }
        }
        None
    }

    /// Cross-function ownership (P2-1): the selector's +1/+0 convention is
    /// known from the unit's own method bodies, so the result is tracked.
    /// +0 results are pool-owned (never a caller-leak candidate); +1 results
    /// are caller-owned and MUST be released — tracked like creations.
    fn bind_owned_result(&mut self, cd: &AstDecl, init: &AstExpr) -> bool {
        let AstExprData::MsgSend { selector, .. } = &init.data else { return false };
        let Some(mo) = self.lookup_own(selector) else { return false };
        // Clone out of the table first: push_obj needs &mut self, and holding
        // the table borrow across it is exactly the E0502 the naive form hits.
        let (class, escaped) = (mo.class.clone(), !mo.plus1);
        let id = self.push_obj(&class, init, escaped);
        if let Some(n) = &cd.name {
            self.bind_var(n, id);
        }
        true
    }

    fn lookup_own(&self, selector: &str) -> Option<&MethodOwn> {
        let key: String = selector.chars().filter(|c| *c != ':').collect();
        self.own.get(&key)
    }

    fn trace_expr_stmt(&mut self, e: &AstExpr) {
        match &e.data {
            AstExprData::FuncCall { name, args, .. } => {
                match name.as_str() {
                    "jeti_release" | "jeti_retain" | "jeti_autorelease" => {
                        if let Some(a) = args.first() {
                            let _ = self.refop(e.line, e.col, name, a);
                        }
                    }
                    _ => {}
                }
            }
            AstExprData::MsgSend { receiver, selector, args: _, .. } => {
                match selector.as_str() {
                    "release" | "retain" | "autorelease" => {
                        let _ = self.refop(e.line, e.col, selector, receiver);
                    }
                    _ => {
                        // v1: no temp-object scanning inside message args
                    }
                }
            }
            AstExprData::Assign { target, value } => {
                self.trace_assign(e.line, e.col, target, value);
            }
            _ => {}
        }
    }

    fn trace_assign(&mut self, _line: usize, _col: usize, target: &AstExpr, value: &AstExpr) {
        if let AstExprData::VarRef { name, .. } = &target.data {
            if is_creation(value) {
                let id = self.create(value, false);
                self.st.bind(name, id);
            } else if let Some(v) = self.var_expr_id(value) {
                self.st.bind(name, v);
            }
        }
    }

    // ─── ref-count operations ───────────────────────────────────────────────

    fn refop(&mut self, line: usize, col: usize, op: &str, target: &AstExpr) -> Option<usize> {
        let (line, col) = if line == 0 {
            (self.scope_line, self.scope_col)
        } else {
            (line, col)
        };
        // ARC wraps the target (`jeti_release((NPObject *)x)`); look through the
        // wrappers so the release lands on the object ARC is actually talking
        // about instead of degrading to "untracked ?".
        let target = peel(target);
        let id = match &target.data {
            AstExprData::VarRef { name, .. } => self.st.resolve(name),
            // A ref-op whose receiver is a fresh creation (e.g.
            // `[[[X alloc] init] autorelease]`) creates the object first, then
            // applies the op — returning the created object's id.
            _ if is_creation(target) => Some(self.create(target, false)),
            _ => None,
        };
        let id = match id {
            Some(id) => id,
            None => {
                let yellow = if self.opts.color { YELLOW } else { "" };
                let reset = if self.opts.color { RESET } else { "" };
                let pos = self.pos(line, col);
                if !self.mute {
                    self.out.push_str(&format!(
                        "  {:<26}  ! {}{} on untracked {}{}\n",
                        pos,
                        yellow,
                        op,
                        render_expr(target),
                        reset
                    ));
                }
                return None;
            }
        };
        match op {
            "release" | "jeti_release" => self.apply_release(line, col, id, op),
            "retain" | "jeti_retain" => self.apply_retain(line, col, id, op),
            "autorelease" | "jeti_autorelease" => self.apply_autorelease(line, col, id, op),
            _ => {}
        }
        Some(id)
    }

    fn apply_release(&mut self, line: usize, col: usize, id: usize, action: &str) {
        self.st.objects[id].count -= 1;
        self.st.objects[id].min_count -= 1;
        if self.st.objects[id].count < 0 {
            // Every negative count is shown inline in red (double-release /
            // over-release), not just the final value in the summary.
            let red = if self.opts.color { RED } else { "" };
            let reset = if self.opts.color { RESET } else { "" };
            let pos = self.pos(line, col);
            if !self.mute {
                self.out.push_str(&format!(
                    "  {:<26}  ! {}double-release / over-released {}: {}{}\n",
                    pos, red, self.st.objects[id].label, self.st.objects[id].count, reset
                ));
            }
            return;
        }
        self.emit(line, col, id, action);
    }

    fn apply_retain(&mut self, line: usize, col: usize, id: usize, action: &str) {
        self.st.objects[id].count += 1;
        self.st.objects[id].min_count += 1;
        self.emit(line, col, id, action);
    }

    fn apply_autorelease(&mut self, line: usize, col: usize, id: usize, action: &str) {
        match self.st.pools.last_mut() {
            Some(pool) => {
                if !pool.contains(&id) {
                    pool.push(id);
                }
            }
            None => {
                // No active pool: the implicit main pool owns the value (the
                // runtime drains it at exit). Mark escaped so the method's own
                // trace doesn't flag its ARC-balanced return as a leak.
                if let Some(o) = self.st.objects.get_mut(id) {
                    o.escaped = true;
                }
            }
        }
        self.emit(line, col, id, action);
    }

    /// Join the current path's end state with `other` (a sibling path's):
    /// each object's alive-interval becomes [min, max] across the two paths.
    ///
    /// This replaces the old "last traced path wins" join, which threw away
    /// the `then` branch's releases whenever an `else` existed, and discarded
    /// a loop body's net effect after every iteration — so a release on one
    /// path looked like it never happened.
    fn merge_from(&mut self, other: &SimState) {
        merge_states(&mut self.st, other);
    }

    // ─── creation ───────────────────────────────────────────────────────────

    fn push_obj(&mut self, class: &str, e: &AstExpr, escaped: bool) -> usize {
        // Allocation-site memo (see the field doc): the same source creation
        // site always denotes the same abstract object, so a fixpoint revisit
        // of a loop body reuses it instead of minting a phantom.
        let site = (e.line, e.col, class.to_string());
        if let Some(&id) = self.site_objects.get(&site) {
            return id;
        }
        let counter = self.st.class_counters.entry(class.to_string()).or_insert(0);
        *counter += 1;
        let n = *counter;
        let id = self.st.objects.len();
        self.site_objects.insert(site, id);
        self.st.objects.push(TraceObj {
            label: format!("{}#{}", class, n),
            count: 1,
            min_count: 1,
            last_count: None,
            line: e.line,
            col: e.col,
            is_param: false,
            escaped,
        });
        id
    }

    fn create(&mut self, e: &AstExpr, _is_temp: bool) -> usize {
        let class = class_of(e);
        // `@"..."` and `@[...]` literals are immutable constants / autoreleased
        // runtime objects in Foundation — they are never manually released, so
        // they are not leak candidates (same as `escaped`).
        let literal = matches!(e.data, AstExprData::AtString(_) | AstExprData::ArrayLit(_));
        let id = self.push_obj(&class, e, literal);
        self.emit(e.line, e.col, id, &render_expr(e));
        id
    }

    // ─── printing ───────────────────────────────────────────────────────────

    /// Resolve a flattened post-preprocessing position to `file:line:col` in
    /// the file the reader wrote. Falls back to the raw flattened position when
    /// there is no map (unit tests) or the line sits outside every region.
    ///
    /// Node lines are FLATTENED post-preprocessing lines while `col` is already
    /// the source column (see `SourceMap::span_from_node`); printing them raw
    /// gave positions that cannot be found in the user's source and that
    /// shifted by one for every line added to any imported header.
    fn pos(&self, line: usize, col: usize) -> String {
        if line == 0 {
            return format!("{}:{}", line, col);
        }
        let Some(sm) = self.sm else {
            return format!("{}:{}", line, col);
        };
        let (path, src_line) = sm.locate(line);
        if path.is_empty() {
            return format!("{}:{}", line, col);
        }
        let base = path.rsplit(['/', '\\']).next().unwrap_or(&path);
        format!("{}:{}:{}", base, src_line, col)
    }

    fn emit(&mut self, line: usize, col: usize, id: usize, action: &str) {
        let count = self.st.objects[id].count;
        let color = self.count_color(id);
        let reset = if self.opts.color { RESET } else { "" };
        let label = self.st.objects[id].label.clone();
        let pos = self.pos(line, col);
        if !self.mute {
            self.out.push_str(&format!(
                "  {:<26}  {:<30}  {}: {}{}{}\n",
                pos, action, label, color, count, reset
            ));
        }
        self.st.objects[id].last_count = Some(count);
    }

    fn count_color(&self, id: usize) -> &'static str {
        if !self.opts.color {
            return "";
        }
        let o = &self.st.objects[id];
        match o.last_count {
            None => "",                               // first print: plain
            Some(prev) if o.count > prev => GREEN,    // increased
            Some(prev) if prev > 0 && o.count <= 0 => CYAN,  // freed (reached 0)
            Some(prev) if o.count < prev => BLUE,     // decreased (still alive)
            _ => "",                                  // unchanged
        }
    }

    fn marker(&mut self, line: usize, col: usize, text: &str) {
        if self.mute {
            return;
        }
        if !self.opts.color {
            self.out.push_str(&format!("── {} ──\n", text));
            return;
        }
        if line > 0 {
            let pos = self.pos(line, col);
            self.out.push_str(&format!(
                "{}  ── {} ──{}  ({})\n",
                DIM, text, RESET, pos
            ));
        } else {
            self.out.push_str(&format!("{}  ── {} ──{}\n", DIM, text, RESET));
        }
    }

    // ─── summary ────────────────────────────────────────────────────────────

    fn summary(&mut self) {
        let dim = if self.opts.color { DIM } else { "" };
        let reset = if self.opts.color { RESET } else { "" };
        self.out.push('\n');
        self.out.push_str(&format!("{}== Summary =={}\n", dim, reset));
        let mut problems = 0;
        for o in &self.st.objects {
            if o.is_param || o.escaped {
                continue;
            }
            let err = if self.opts.color { RED } else { "" };
            let reset_c = if self.opts.color { RESET } else { "" };
            let val = if o.min_count == o.count {
                format!("{}", o.count)
            } else {
                format!("[{}, {}]", o.min_count, o.count)
            };
            if o.count > 0 && o.min_count > 0 {
                problems += 1;
                let pos = self.pos(o.line, o.col);
                self.out.push_str(&format!(
                    "  {}{} (allocated {}) — {} (leaked — alive on every path){}\n",
                    err, o.label, pos, val, reset_c
                ));
            } else if o.count > 0 {
                problems += 1;
                let pos = self.pos(o.line, o.col);
                self.out.push_str(&format!(
                    "  {}{} (allocated {}) — {} (possibly leaked — alive on some path){}\n",
                    err, o.label, pos, val, reset_c
                ));
            } else if o.count < 0 {
                problems += 1;
                let pos = self.pos(o.line, o.col);
                self.out.push_str(&format!(
                    "  {}{} (allocated {}) — {} (over-released — count negative){}\n",
                    err, o.label, pos, val, reset_c
                ));
            } else {
                let pos = self.pos(o.line, o.col);
                self.out.push_str(&format!(
                    "  {} (allocated {}) — {} (freed)\n",
                    o.label, pos, val
                ));
            }
        }
        if problems == 0 {
            self.out.push_str("  no live objects — all freed\n");
        }
    }

    // ─── helpers ────────────────────────────────────────────────────────────

    fn var_expr_id(&self, e: &AstExpr) -> Option<usize> {
        match &e.data {
            AstExprData::VarRef { name, .. } => self.st.resolve(name),
            _ => None,
        }
    }
}

pub fn trace_refcounts(unit: &AstUnit, opts: &TraceOptions, sm: Option<&SourceMap>) -> String {
    let mut t = Tracer::new(opts.clone(), sm);
    t.trace_unit(unit);
    t.out
}

// ─── expression helpers ─────────────────────────────────────────────────────

/// Join `cur` (the current path's end state) with `other` (a sibling path's):
/// each object's alive-interval becomes [min, max] across the two paths.
///
/// A free function so the CFG worklist (P2-3) can merge states without a
/// `Tracer` in hand; the structured walker reaches it via
/// `Tracer::merge_from`.
fn merge_states(cur: &mut SimState, other: &SimState) {
    // Objects that exist only on the sibling path (created after the paths
    // split): append them with a 0 floor — they were never alive here. The
    // old in-place merge silently dropped them, so a leak that existed only
    // on the first-traced branch vanished at the join.
    while cur.objects.len() < other.objects.len() {
        let mut o = other.objects[cur.objects.len()].clone();
        o.min_count = o.min_count.min(0);
        cur.objects.push(o);
    }
    for id in 0..cur.objects.len() {
        match other.objects.get(id) {
            Some(b) => {
                let o = &mut cur.objects[id];
                o.min_count = o.min_count.min(b.min_count);
                o.count = o.count.max(b.count);
                o.escaped |= b.escaped;
            }
            None => {
                // Exists only on the current path (created inside the
                // branch/loop body): on the sibling path it was never
                // alive, so the interval floor is 0.
                cur.objects[id].min_count = cur.objects[id].min_count.min(0);
            }
        }
    }
    // Rebind names only the sibling path bound, so a later release on the
    // joined path still resolves instead of printing "untracked".
    for scope in &other.bindings {
        for (name, &id) in scope {
            if cur.resolve(name).is_none() {
                if let Some(top) = cur.bindings.last_mut() {
                    top.insert(name.clone(), id);
                }
            }
        }
    }
    // Union the innermost autorelease pools.
    if let Some(b) = other.pools.last() {
        if let Some(a) = cur.pools.last_mut() {
            for &id in b {
                if !a.contains(&id) {
                    a.push(id);
                }
            }
        }
    }
}

/// True when the body uses a shape the CFG builder does not wire edges for:
/// `goto` (sets no edge — the label block is unreachable) and `switch` (no
/// successor from the dispatch block, so its arms AND everything after it are
/// unreachable). Interpreting such a graph would silently drop code and mint
/// false leaks, so those bodies keep the structured walker (missed
/// path-sensitivity is acceptable; fabricated verdicts are not).
fn body_has_unwired_shape(s: &AstStmt) -> bool {
    if s.kind == AstStmtKind::Goto || s.kind == AstStmtKind::Switch {
        return true;
    }
    match &s.data {
        AstStmtData::Compound(v) => v.iter().any(body_has_unwired_shape),
        AstStmtData::If { then, else_, .. } => {
            body_has_unwired_shape(then)
                || else_.as_deref().map_or(false, body_has_unwired_shape)
        }
        AstStmtData::While { body, .. }
        | AstStmtData::Do { body, .. }
        | AstStmtData::ForIn { body, .. }
        | AstStmtData::Autoreleasepool(body)
        | AstStmtData::NoArc(body)
        | AstStmtData::Defer(body)
        | AstStmtData::Synchronized { body, .. } => body_has_unwired_shape(body),
        AstStmtData::For { init, body, .. } => {
            init.as_deref().map_or(false, body_has_unwired_shape)
                || body_has_unwired_shape(body)
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            body_has_unwired_shape(try_block)
                || catches.iter().any(body_has_unwired_shape)
                || finally_block.as_deref().map_or(false, body_has_unwired_shape)
        }
        AstStmtData::Catch { body, .. } => body_has_unwired_shape(body),
        _ => false,
    }
}

/// Peel the wrappers ARC (and the passes before it) leave around a ref-op
/// target.
///
/// `make_release_stmt` spells the upcast in the AST so the emitted call is
/// warning-clean on every backend (`jeti_release((NPObject *)x)`), and
/// parentheses/boxing stack the same way. Without peeling, *every*
/// ARC-inserted release resolved to nothing and printed as
/// `! jeti_release on untracked ?` — the exact opposite of what this tool
/// exists to show, since attributing ARC's own releases is its whole point.
fn peel(e: &AstExpr) -> &AstExpr {
    match &e.data {
        AstExprData::Cast { expr, .. }
        | AstExprData::Paren(expr)
        | AstExprData::Boxed(expr) => peel(expr),
        _ => e,
    }
}

fn is_creation(e: &AstExpr) -> bool {
    match &e.data {
        AstExprData::MsgSend { receiver, selector, .. } => {
            let s = selector.as_str();
            if s.starts_with("alloc") || s.starts_with("new") || s.starts_with("copy") || s == "mutableCopy"
            {
                return true;
            }
            if s.starts_with("init") {
                return is_creation(receiver);
            }
            false
        }
        AstExprData::AtString(_) | AstExprData::ArrayLit(_) => true,
        _ => false,
    }
}

/// Ownership of a method's return value: `plus1` means the CALLER owns the
/// result and must release it; `!plus1` (+0) means it is pool-owned and
/// releasing it is the pool's job.
struct MethodOwn {
    class: String,
    plus1: bool,
}

/// The alloc/new/copy/mutableCopy prefixes hand ownership to the caller by
/// convention, with or without a visible body.
fn selector_is_owning(sel: &str) -> bool {
    sel.starts_with("alloc")
        || sel.starts_with("new")
        || sel.starts_with("copy")
        || sel.starts_with("mutableCopy")
}

/// Does any `return` in this body hand back a freshly created (+1) value?
fn body_returns_creation(body: &AstStmt) -> bool {
    match &body.data {
        AstStmtData::Return(Some(e)) => {
            // A literal is a constant: returning one carries no ownership to
            // balance, so `return @"x"` is +0 even though is_creation tracks
            // literals for scope-release purposes. Counting them here made
            // produce/maybeC-style methods look +1 and their call sites
            // "leak" — a tracer false positive.
            is_creation(e)
                && !matches!(e.data, AstExprData::AtString(_) | AstExprData::ArrayLit(_))
        }
        AstStmtData::Compound(v) => v.iter().any(body_returns_creation),
        AstStmtData::If { then, else_, .. } => {
            body_returns_creation(then)
                || else_.as_deref().map_or(false, body_returns_creation)
        }
        AstStmtData::While { body, .. }
        | AstStmtData::Do { body, .. }
        | AstStmtData::ForIn { body, .. }
        | AstStmtData::Autoreleasepool(body)
        | AstStmtData::NoArc(body)
        | AstStmtData::Synchronized { body, .. } => body_returns_creation(body),
        AstStmtData::For { init, body, .. } => {
            init.as_deref().map_or(false, body_returns_creation)
                || body_returns_creation(body)
        }
        AstStmtData::Try { try_block, catches, finally_block } => {
            body_returns_creation(try_block)
                || catches.iter().any(body_returns_creation)
                || finally_block.as_deref().map_or(false, body_returns_creation)
        }
        _ => false,
    }
}

/// Build the selector → ownership table from the unit's own classes.
///
/// Keys are colon-stripped (selector identity is global; AGENTS.md). A
/// selector whose implementations disagree on ownership is DROPPED: guessing
/// either way mints false verdicts, and this tool's value is that its
/// "leaked" lines can be trusted.
fn build_own_table(unit: &AstUnit) -> HashMap<String, MethodOwn> {
    let mut table: HashMap<String, MethodOwn> = HashMap::new();
    for d in &unit.decls {
        let AstDeclData::Class { cls_sym, methods, .. } = &d.data else { continue };
        let Some(class) = cls_sym.as_deref() else { continue };
        for m in methods {
            let Some(sel) = m.name.as_deref() else { continue };
            let AstDeclData::Method { body: Some(body), .. } = &m.data else { continue };
            let key: String = sel.chars().filter(|c| *c != ':').collect();
            let plus1 = selector_is_owning(sel) || body_returns_creation(body);
            let conflict = table.get(&key).map_or(false, |p: &MethodOwn| p.plus1 != plus1);
            if conflict {
                table.remove(&key);
            } else if !table.contains_key(&key) {
                table.insert(key, MethodOwn { class: class.to_string(), plus1 });
            }
        }
    }
    table
}

/// Collect the names a function's block literals capture (P2-2): every
/// variable referenced inside a block body.
///
/// Mirrors the ARC pass's own block-capture harvest (`collect_captures_*` in
/// arc.rs): a block body may run after its enclosing scope dies, so captured
/// locals get different lifetime treatment — the trace must mirror that
/// treatment instead of demanding a release the compiler will never insert.
fn harvest_captures(body: &AstStmt) -> Vec<String> {
    let mut out = Vec::new();
    walk_stmt(body, &mut out, false);
    out
}

/// Depth-first walk collecting variable references made inside block bodies.
/// `in_block` flips on when entering a block literal and never turns off —
/// nested blocks capture too. Unknown shapes are skipped (misses are
/// acceptable; false captures are not).
fn walk_stmt(s: &AstStmt, out: &mut Vec<String>, in_block: bool) {
    match &s.data {
        AstStmtData::Compound(v) => v.iter().for_each(|x| walk_stmt(x, out, in_block)),
        AstStmtData::Expr(e) => walk_expr(e, out, in_block),
        AstStmtData::Decl(d) => {
            let mut cur = Some(d);
            while let Some(cd) = cur {
                if let AstDeclData::Variable { init: Some(i), next, .. } = &cd.data {
                    walk_expr(i, out, in_block);
                    cur = next.as_deref();
                } else {
                    break;
                }
            }
        }
        AstStmtData::Return(Some(e)) | AstStmtData::Throw(Some(e)) => walk_expr(e, out, in_block),
        AstStmtData::If { cond, then, else_, .. } => {
            walk_expr(cond, out, in_block);
            walk_stmt(then, out, in_block);
            if let Some(el) = else_ {
                walk_stmt(el, out, in_block);
            }
        }
        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. } => {
            walk_stmt(body, out, in_block)
        }
        AstStmtData::For { init, body, .. } => {
            if let Some(i) = init {
                walk_stmt(i, out, in_block);
            }
            walk_stmt(body, out, in_block);
        }
        AstStmtData::ForIn { body, .. } => walk_stmt(body, out, in_block),
        AstStmtData::Try { try_block, catches, finally_block } => {
            walk_stmt(try_block, out, in_block);
            catches.iter().for_each(|c| walk_stmt(c, out, in_block));
            if let Some(f) = finally_block {
                walk_stmt(f, out, in_block);
            }
        }
        AstStmtData::Catch { body, .. } => walk_stmt(body, out, in_block),
        AstStmtData::Finally(b) => walk_stmt(b, out, in_block),
        AstStmtData::Synchronized { body, .. } => walk_stmt(body, out, in_block),
        AstStmtData::Autoreleasepool(b) | AstStmtData::NoArc(b) | AstStmtData::Defer(b) => {
            walk_stmt(b, out, in_block)
        }
        AstStmtData::Switch { body, .. } => walk_stmt(body, out, in_block),
        AstStmtData::Case { body, .. } => walk_stmt(body, out, in_block),
        AstStmtData::Default(b) => walk_stmt(b, out, in_block),
        _ => {}
    }
}

fn walk_expr(e: &AstExpr, out: &mut Vec<String>, in_block: bool) {
    match &e.data {
        AstExprData::VarRef { name, .. } if in_block => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        AstExprData::Block { body: Some(b), .. } => walk_stmt(b, out, true),
        AstExprData::MsgSend { receiver, args, .. } => {
            walk_expr(receiver, out, in_block);
            args.iter().for_each(|a| walk_expr(a, out, in_block));
        }
        AstExprData::FuncCall { args, .. } => {
            args.iter().for_each(|a| walk_expr(a, out, in_block));
        }
        AstExprData::Paren(i)
        | AstExprData::Boxed(i)
        | AstExprData::Await { inner: i, .. }
        | AstExprData::Cast { expr: i, .. } => walk_expr(i, out, in_block),
        AstExprData::Unary { operand, .. } => walk_expr(operand, out, in_block),
        AstExprData::Binary { left, right, .. } => {
            walk_expr(left, out, in_block);
            walk_expr(right, out, in_block);
        }
        AstExprData::Assign { target, value } => {
            walk_expr(target, out, in_block);
            walk_expr(value, out, in_block);
        }
        AstExprData::Ternary { cond, then, else_ } => {
            walk_expr(cond, out, in_block);
            walk_expr(then, out, in_block);
            walk_expr(else_, out, in_block);
        }
        _ => {}
    }
}

fn class_of(e: &AstExpr) -> String {
    match &e.data {
        AstExprData::MsgSend { receiver, selector, .. } => {
            let s = selector.as_str();
            if s.starts_with("alloc") || s.starts_with("new") || s.starts_with("init") {
                class_of(receiver)
            } else if s.starts_with("copy") || s == "mutableCopy" {
                "copy".to_string()
            } else {
                selector.clone()
            }
        }
        AstExprData::VarRef { name, .. } => name.clone(),
        // Jeti's own classes: `@"..."` is an NPString and an array literal an
        // NPArray. These used to print the ObjC spellings (`NSString` /
        // `NSArray`), which are not classes in this language at all — the NP-
        // prefix is deliberately decoupled from ObjC (see AGENTS.md).
        AstExprData::AtString(_) => "NPString".to_string(),
        AstExprData::ArrayLit(_) => "NPArray".to_string(),
        _ => "obj".to_string(),
    }
}

fn render_expr(e: &AstExpr) -> String {
    match &e.data {
        AstExprData::VarRef { name, .. } => name.clone(),
        AstExprData::MsgSend { receiver, selector, args, .. } => {
            let mut s = format!("[{} {}", render_expr(receiver), selector);
            if !args.is_empty() {
                s.push_str(&format!(":{}", args.iter().map(render_expr).collect::<Vec<_>>().join(":")));
            }
            s.push(']');
            s
        }
        AstExprData::FuncCall { name, args, .. } => {
            format!("{}({})", name, args.iter().map(render_expr).collect::<Vec<_>>().join(", "))
        }
        AstExprData::Int(v) => v.to_string(),
        AstExprData::Float(f) => format!("{}", f),
        AstExprData::String(s) => format!("\"{}\"", s),
        AstExprData::AtString(s) => format!("@\"{}\"", s),
        AstExprData::Bool(b) => b.to_string(),
        _ => "?".to_string(),
    }
}

fn is_obj_cst_type(t: Option<&CstType>) -> bool {
    match t {
        None => false,
        Some(t) => {
            t.is_pointer
                || t.prim == TypePrim::Id
                || t.prim == TypePrim::Instancetype
                || t.prim == TypePrim::Class
        }
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use jeti_ast::{AstDeclKind, AstExprKind, AstStmtKind};
    use jeti_cst::{CstParam, CstType, TypePrim};

    fn var_ref(name: &str) -> AstExpr {
        AstExpr {
            kind: AstExprKind::VarRef,
            expr_type: None,
            line: 1,
            col: 1,
            data: AstExprData::VarRef { sym: None, name: name.to_string() },
            end_line: 0,
            end_col: 0,
        }
    }

    fn msg_send(receiver: AstExpr, selector: &str) -> AstExpr {
        AstExpr {
            kind: AstExprKind::MsgSend,
            expr_type: None,
            line: 1,
            col: 1,
            data: AstExprData::MsgSend {
                receiver: Box::new(receiver),
                method: None,
                vtable_index: -1,
                is_class_method: true,
                is_super: false,
                super_name: None,
                selector: selector.to_string(),
                args: vec![],
            },
            end_line: 0,
            end_col: 0,
        }
    }

    fn alloc_init() -> AstExpr {
        msg_send(msg_send(var_ref("Cat"), "alloc"), "init")
    }

    fn expr_stmt(e: AstExpr) -> AstStmt {
        AstStmt { kind: AstStmtKind::Expr, line: 1, col: 1, data: AstStmtData::Expr(e), end_line: 0, end_col: 0 }
    }

    fn decl_stmt(d: AstDecl) -> AstStmt {
        AstStmt { kind: AstStmtKind::Decl, line: 1, col: 1, data: AstStmtData::Decl(d), end_line: 0, end_col: 0 }
    }

    fn func_call(name: &str, args: Vec<AstExpr>) -> AstExpr {
        AstExpr {
            kind: AstExprKind::FuncCall,
            expr_type: None,
            line: 1,
            col: 1,
            data: AstExprData::FuncCall {
                func: Some(name.to_string()),
                name: name.to_string(),
                callee: None,
                args,
            },
            end_line: 0,
            end_col: 0,
        }
    }

    fn decl(name: &str, init: AstExpr) -> AstDecl {
        AstDecl {
            kind: AstDeclKind::Variable,
            name: Some(name.to_string()),
            line: 1,
            col: 1,
            data: AstDeclData::Variable {
                var_type: None,
                init: Some(Box::new(init)),
                is_static: false,
                is_extern: false,
                is_const: false,
                is_block_qual: false,
                is_weak: false,
                next: None,
            },
            attributes: vec![],
            end_line: 0,
            end_col: 0,
        }
    }

    fn function(body: Vec<AstStmt>) -> AstDecl {
        AstDecl {
            kind: AstDeclKind::Function,
            name: Some("main".to_string()),
            line: 1,
            col: 1,
            data: AstDeclData::Function {
                func_sym: None,
                return_type: None,
                params: None,
                body: Some(Box::new(AstStmt {
                    kind: AstStmtKind::Compound,
                    line: 1,
                    col: 1,
                    data: AstStmtData::Compound(body),
                    end_line: 0,
                    end_col: 0,
                })),
                has_variadic: false,
                throws: None,
                async_marker: false,
            },
            attributes: vec![],
            end_line: 0,
            end_col: 0,
        }
    }

    fn trace(stmts: Vec<AstStmt>) -> String {
        let unit = AstUnit { decls: vec![function(stmts)], filename: "test.jeti".to_string(), namespaces: Vec::new(), usings: Vec::new() };
        trace_refcounts(&unit, &TraceOptions { max_iters: 2, color: false }, None)
    }

    #[test]
    fn alloc_release_chain() {
        let out = trace(vec![
            decl_stmt(decl("cat", alloc_init())),
            expr_stmt(func_call("jeti_release", vec![var_ref("cat")])),
        ]);
        assert!(out.contains("Cat#1: 1"), "out:\n{}", out);
        assert!(out.contains("Cat#1: 0"), "out:\n{}", out);
        assert!(out.contains("all freed"), "out:\n{}", out);
    }

    #[test]
    fn retain_increases_release_decreases() {
        let out = trace(vec![
            decl_stmt(decl("cat", alloc_init())),
            expr_stmt(func_call("jeti_retain", vec![var_ref("cat")])),
            expr_stmt(func_call("jeti_release", vec![var_ref("cat")])),
        ]);
        assert!(out.contains("Cat#1: 2"), "out:\n{}", out);
        assert!(out.contains("Cat#1: 1"), "out:\n{}", out);
    }

    #[test]
    fn message_send_release_traced() {
        let out = trace(vec![
            decl_stmt(decl("cat", alloc_init())),
            expr_stmt(msg_send(var_ref("cat"), "release")),
        ]);
        assert!(out.contains("Cat#1: 0"), "out:\n{}", out);
    }

    #[test]
    fn double_release_warns() {
        let out = trace(vec![
            decl_stmt(decl("cat", alloc_init())),
            expr_stmt(func_call("jeti_release", vec![var_ref("cat")])),
            expr_stmt(func_call("jeti_release", vec![var_ref("cat")])),
        ]);
        assert!(out.contains("double-release"), "out:\n{}", out);
    }

    #[test]
    fn over_release_goes_negative_and_warns_in_summary() {
        let out = trace(vec![
            decl_stmt(decl("cat", alloc_init())),
            expr_stmt(func_call("jeti_release", vec![var_ref("cat")])),
            expr_stmt(func_call("jeti_release", vec![var_ref("cat")])),
        ]);
        assert!(out.contains("-1"), "out:\n{}", out);
        assert!(out.contains("over-released"), "out:\n{}", out);
        assert!(!out.contains("all freed"), "out:\n{}", out);
    }

    #[test]
    fn autoreleasepool_batch_releases() {
        let out = trace(vec![
            AstStmt {
                kind: AstStmtKind::Autoreleasepool,
                line: 1,
                col: 1,
                data: AstStmtData::Autoreleasepool(Box::new(AstStmt {
                    kind: AstStmtKind::Compound,
                    line: 1,
                    col: 1,
                    data: AstStmtData::Compound(vec![
                        decl_stmt(decl("tmp", alloc_init())),
                        expr_stmt(func_call("jeti_autorelease", vec![var_ref("tmp")])),
                    ]),
                    end_line: 0,
                    end_col: 0,
                })),
                end_line: 0,
                end_col: 0,
            },
        ]);
        assert!(out.contains("pool pop"), "out:\n{}", out);
        assert!(out.contains("Cat#1: 0"), "out:\n{}", out);
    }

    #[test]
    fn alias_binding_shares_object() {
        let out = trace(vec![
            decl_stmt(decl("cat", alloc_init())),
            decl_stmt(decl("other", var_ref("cat"))),
            expr_stmt(func_call("jeti_release", vec![var_ref("other")])),
        ]);
        assert!(out.contains("Cat#1: 0"), "out:\n{}", out);
        assert!(!out.contains("untracked"), "out:\n{}", out);
    }

    #[test]
    fn release_on_untracked_warns() {
        let out = trace(vec![
            expr_stmt(func_call("jeti_release", vec![var_ref("ghost")])),
        ]);
        assert!(out.contains("untracked"), "out:\n{}", out);
    }

    #[test]
    fn loop_body_create_and_release_comes_out_freed() {
        // One pass of the body is traced and joined with the entry state: an
        // object created AND released inside the loop is freed, and no phantom
        // second-iteration identity appears (the old snapshot-restore model
        // re-created Cat#N every iteration while discarding the release).
        let out = trace(vec![AstStmt {
            kind: AstStmtKind::For,
            line: 1,
            col: 1,
            data: AstStmtData::For {
                init: None,
                cond: None,
                incr: None,
                body: Box::new(AstStmt {
                    kind: AstStmtKind::Compound,
                    line: 1,
                    col: 1,
                    data: AstStmtData::Compound(vec![
                        decl_stmt(decl("c", alloc_init())),
                        expr_stmt(func_call("jeti_release", vec![var_ref("c")])),
                    ]),
                    end_line: 0,
                    end_col: 0,
                }),
            },
            end_line: 0,
            end_col: 0,
        }]);
        assert!(out.contains("Cat#1"), "out:\n{}", out);
        assert!(!out.contains("Cat#2"), "one pass is traced; no phantom iteration\nout:\n{}", out);
        assert!(out.contains("freed"), "created+released in the body must be freed\nout:\n{}", out);
    }

    fn ret_stmt(e: AstExpr) -> AstStmt {
        AstStmt {
            kind: AstStmtKind::Return,
            line: 1,
            col: 1,
            data: AstStmtData::Return(Some(Box::new(e))),
            end_line: 0,
            end_col: 0,
        }
    }

    fn throw_stmt(e: AstExpr) -> AstStmt {
        AstStmt {
            kind: AstStmtKind::Throw,
            line: 1,
            col: 1,
            data: AstStmtData::Throw(Some(Box::new(e))),
            end_line: 0,
            end_col: 0,
        }
    }

    #[test]
    fn returned_object_escapes_leak_summary() {
        // `Cat *c = [Cat alloc+init]; return c;` — ownership of `c` leaves the
        // scope, so it must not be reported as a possible leak.
        let out = trace(vec![
            decl_stmt(decl("c", alloc_init())),
            ret_stmt(var_ref("c")),
        ]);
        assert!(!out.contains("possible leak"), "out:\n{}", out);
        assert!(out.contains("all freed"), "out:\n{}", out);
    }

    #[test]
    fn thrown_object_escapes_leak_summary() {
        // `@throw err;` transfers ownership to the runtime/caller.
        let out = trace(vec![
            decl_stmt(decl("err", alloc_init())),
            throw_stmt(var_ref("err")),
        ]);
        assert!(!out.contains("possible leak"), "out:\n{}", out);
    }

    #[test]
    fn catch_param_bound_and_releasable() {
        let mut t = CstType::new(TypePrim::Named);
        t.name = Some("Error".to_string());
        t.is_pointer = true;
        let catch = AstStmt {
            kind: AstStmtKind::Catch,
            line: 1,
            col: 1,
            data: AstStmtData::Catch {
                param: CstParam {
                    par_type: Some(Box::new(t)),
                    name: Some("e".to_string()),
                    external_name: None,
                    next: None,
                    attributes: vec![],
                },
                body: Box::new(AstStmt {
                    kind: AstStmtKind::Compound,
                    line: 1,
                    col: 1,
                    data: AstStmtData::Compound(vec![
                        expr_stmt(func_call("jeti_release", vec![var_ref("e")])),
                    ]),
                    end_line: 0,
                    end_col: 0,
                }),
            },
            end_line: 0,
            end_col: 0,
        };
        let out = trace(vec![catch]);
        assert!(!out.contains("untracked e"), "out:\n{}", out);
        assert!(out.contains("Error (catch)"), "out:\n{}", out);
    }

    #[test]
    fn refop_on_creation_receiver_traced() {
        // `[[[Cat alloc] init] autorelease];` — the receiver is itself a
        // creation; the tracer must create the object first, then apply the op.
        let out = trace(vec![expr_stmt(msg_send(alloc_init(), "autorelease"))]);
        assert!(!out.contains("untracked"), "out:\n{}", out);
        assert!(out.contains("autorelease"), "out:\n{}", out);
    }

    #[test]
    fn static_decl_creation_escapes_summary() {
        // `static Cat *c = [Cat alloc+init];` — a static owns its object for
        // the whole process, not the current scope.
        let mut d = decl("c", alloc_init());
        if let AstDeclData::Variable { is_static, .. } = &mut d.data {
            *is_static = true;
        }
        let out = trace(vec![decl_stmt(d)]);
        assert!(!out.contains("possible leak"), "out:\n{}", out);
        assert!(out.contains("all freed"), "out:\n{}", out);
    }

    #[test]
    fn bare_if_assignment_persists_for_return() {
        // `if (x) s = [[Cat alloc] init]; return s;` — the binding made inside
        // the bare `if` must survive so `return s` can mark the object escaped.
        let assign = AstStmt {
            kind: AstStmtKind::Expr,
            line: 1,
            col: 1,
            data: AstStmtData::Expr(AstExpr {
                kind: AstExprKind::Assign,
                expr_type: None,
                line: 1,
                col: 1,
                data: AstExprData::Assign {
                    target: Box::new(var_ref("s")),
                    value: Box::new(alloc_init()),
                },
                end_line: 0,
                end_col: 0,
            }),
            end_line: 0,
            end_col: 0,
        };
        let if_stmt = AstStmt {
            kind: AstStmtKind::If,
            line: 1,
            col: 1,
            data: AstStmtData::If {
                cond: Box::new(var_ref("x")),
                then: Box::new(AstStmt {
                    kind: AstStmtKind::Compound,
                    line: 1,
                    col: 1,
                    data: AstStmtData::Compound(vec![assign]),
                    end_line: 0,
                    end_col: 0,
                }),
                else_: None,
            },
            end_line: 0,
            end_col: 0,
        };
        let out = trace(vec![if_stmt, ret_stmt(var_ref("s"))]);
        assert!(!out.contains("possible leak"), "out:\n{}", out);
        assert!(out.contains("all freed"), "out:\n{}", out);
    }

    #[test]
    fn cfg_branch_join_counts_both_paths() {
        // The CFG worklist joins branch states at the merge point: an object
        // released on BOTH branches is freed, one released on a single branch
        // only is "possibly leaked [0, 1]". The old "last traced path wins"
        // kept just one side's releases.
        let rel = |name: &str| expr_stmt(func_call("jeti_release", vec![var_ref(name)]));
        let branch = |then: AstStmt, else_: Option<AstStmt>| AstStmt {
            kind: AstStmtKind::If,
            line: 1,
            col: 1,
            data: AstStmtData::If {
                cond: Box::new(var_ref("x")),
                then: Box::new(AstStmt {
                    kind: AstStmtKind::Compound,
                    line: 1,
                    col: 1,
                    data: AstStmtData::Compound(vec![then]),
                    end_line: 0,
                    end_col: 0,
                }),
                else_: else_.map(Box::new),
            },
            end_line: 0,
            end_col: 0,
        };
        // Distinct source positions for the two creation sites: the
        // allocation-site memo keys on (line, col, class), and real code
        // never puts two sites at one position.
        let mut a_init = alloc_init();
        a_init.line = 3;
        let mut b_init = alloc_init();
        b_init.line = 7;
        let out = trace(vec![
            decl_stmt(decl("a", a_init)),
            branch(rel("a"), Some(rel("a"))),
            decl_stmt(decl("b", b_init)),
            branch(rel("b"), None),
        ]);
        assert!(out.contains("possibly leaked"), "out:\n{}", out);
        assert!(!out.contains("alive on every path"), "out:\n{}", out);
        assert!(out.contains("Cat#2"), "out:\n{}", out);
    }
}
