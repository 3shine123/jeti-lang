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
use jeti_ast::{AstUnit, AstDecl, AstDeclData, AstStmt, AstStmtData, AstExpr, AstExprData};
use jeti_cst::{CstParam, CstType, TypePrim};

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

#[derive(Clone)]
struct TraceObj {
    label: String,
    count: i32,
    last_count: Option<i32>,
    line: usize,
    col: usize,
    is_param: bool,
    escaped: bool,
}

#[derive(Clone)]
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

pub struct Tracer {
    opts: TraceOptions,
    st: SimState,
    out: String,
    scope_line: usize,
    scope_col: usize,
}

impl Tracer {
    pub fn new(opts: TraceOptions) -> Self {
        Tracer {
            opts,
            st: SimState::new(),
            out: String::new(),
            scope_line: 0,
            scope_col: 0,
        }
    }

    // ─── top-level entry ────────────────────────────────────────────────────

    pub fn trace_unit(&mut self, unit: &AstUnit) {
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
        self.st.bindings.push(HashMap::new());
        self.bind_params(params);
        if let Some(b) = body {
            self.trace_stmt(b);
        }
        self.st.bindings.pop();
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
                // keep the `then` path's state as the post-if state when there
                // is no `else`, so releases and var bindings in the branch
                // persist (matches real control flow for e.g. statics assigned
                // inside a bare `if`).
                if let Some(el) = else_ {
                    self.st = snap.clone();
                    self.marker(el.line, el.col, "else");
                    self.trace_stmt_persist(el);
                    // final state = else branch result (last traced path)
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

    fn trace_loop(&mut self, body: &AstStmt, kind: &str) {
        let n = self.opts.max_iters.max(1);
        for i in 0..n {
            self.marker(0, 0, &format!("{} iter {}/{}", kind, i + 1, n));
            let snap = self.st.clone();
            self.trace_stmt(body);
            // Restore state, but keep the class counters monotonic so each
            // iteration's allocations get fresh Cat#N identities.
            let counters = self.st.class_counters.clone();
            self.st = snap;
            self.st.class_counters = counters;
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
                        self.st.bind(n, id);
                    }
                } else if let Some(id) = self.refop_if_any(cd, init) {
                    // e.g. `Cat *other = [cat retain];` — retain traced, new
                    // name aliases the same object.
                    if let Some(n) = &cd.name {
                        self.st.bind(n, id);
                    }
                } else if let Some(v) = self.var_expr_id(init) {
                    if let Some(n) = &cd.name {
                        self.st.bind(n, v);
                    }
                }
                cur = next.as_deref();
                continue;
            }
            break;
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
                self.out.push_str(&format!(
                    "  {:>4}:{:<3}  ! {}{} on untracked {}{}\n",
                    line,
                    col,
                    yellow,
                    op,
                    render_expr(target),
                    reset
                ));
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
        if self.st.objects[id].count < 0 {
            // Every negative count is shown inline in red (double-release /
            // over-release), not just the final value in the summary.
            let red = if self.opts.color { RED } else { "" };
            let reset = if self.opts.color { RESET } else { "" };
            self.out.push_str(&format!(
                "  {:>4}:{:<3}  ! {}double-release / over-released {}: {}{}\n",
                line, col, red, self.st.objects[id].label, self.st.objects[id].count, reset
            ));
            return;
        }
        self.emit(line, col, id, action);
    }

    fn apply_retain(&mut self, line: usize, col: usize, id: usize, action: &str) {
        self.st.objects[id].count += 1;
        self.emit(line, col, id, action);
    }

    fn apply_autorelease(&mut self, line: usize, col: usize, id: usize, action: &str) {
        if let Some(pool) = self.st.pools.last_mut() {
            if !pool.contains(&id) {
                pool.push(id);
            }
        }
        self.emit(line, col, id, action);
    }

    // ─── creation ───────────────────────────────────────────────────────────

    fn create(&mut self, e: &AstExpr, _is_temp: bool) -> usize {
        let class = class_of(e);
        let n = self.st.class_counters.entry(class.clone()).or_insert(0);
        *n += 1;
        let id = self.st.objects.len();
        // `@"..."` and `@[...]` literals are immutable constants / autoreleased
        // runtime objects in Foundation — they are never manually released, so
        // they are not leak candidates (same as `escaped`).
        let literal = matches!(e.data, AstExprData::AtString(_) | AstExprData::ArrayLit(_));
        self.st.objects.push(TraceObj {
            label: format!("{}#{}", class, *n),
            count: 1,
            last_count: None,
            line: e.line,
            col: e.col,
            is_param: false,
            escaped: literal,
        });
        self.emit(e.line, e.col, id, &render_expr(e));
        id
    }

    // ─── printing ───────────────────────────────────────────────────────────

    fn emit(&mut self, line: usize, col: usize, id: usize, action: &str) {
        let count = self.st.objects[id].count;
        let color = self.count_color(id);
        let reset = if self.opts.color { RESET } else { "" };
        let label = self.st.objects[id].label.clone();
        self.out.push_str(&format!(
            "  {:>4}:{:<3}  {:<30}  {}: {}{}{}\n",
            line, col, action, label, color, count, reset
        ));
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
        if !self.opts.color {
            self.out.push_str(&format!("── {} ──\n", text));
            return;
        }
        if line > 0 {
            self.out.push_str(&format!(
                "{}  ── {} ──{}  ({:>4}:{})\n",
                DIM, text, RESET, line, col
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
            if o.count > 0 {
                problems += 1;
                self.out.push_str(&format!(
                    "  {}{} (allocated {}:{}) — {} (still alive — possible leak){}\n",
                    err, o.label, o.line, o.col, o.count, reset_c
                ));
            } else if o.count < 0 {
                problems += 1;
                self.out.push_str(&format!(
                    "  {}{} (allocated {}:{}) — {} (over-released — count negative){}\n",
                    err, o.label, o.line, o.col, o.count, reset_c
                ));
            } else {
                self.out.push_str(&format!(
                    "  {} (allocated {}:{}) — 0 (freed)\n",
                    o.label, o.line, o.col
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

pub fn trace_refcounts(unit: &AstUnit, opts: &TraceOptions) -> String {
    let mut t = Tracer::new(opts.clone());
    t.trace_unit(unit);
    t.out
}

// ─── expression helpers ─────────────────────────────────────────────────────

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
        AstExprData::AtString(_) => "NSString".to_string(),
        AstExprData::ArrayLit(_) => "NSArray".to_string(),
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
        let unit = AstUnit { decls: vec![function(stmts)], filename: "test.jeti".to_string(), namespaces: Vec::new() };
        trace_refcounts(&unit, &TraceOptions { max_iters: 2, color: false })
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
    fn loop_iterations_get_distinct_ids() {
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
        assert!(out.contains("Cat#2"), "out:\n{}", out);
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
}
