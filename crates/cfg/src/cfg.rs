use jeti_ast::ast::*;

// ─── Basic Block ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CfgBlock {
    pub id: i32,
    pub stmts: Vec<AstStmt>,
    pub true_succ: Option<i32>,    // block id
    pub false_succ: Option<i32>,   // block id (for if/while conditions)
    pub visited: bool,
}

impl CfgBlock {
    pub fn new(id: i32) -> Self {
        CfgBlock { id, stmts: Vec::new(), true_succ: None, false_succ: None, visited: false }
    }

    pub fn add_stmt(&mut self, s: AstStmt) {
        self.stmts.push(s);
    }
}

// ─── CFG ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Cfg {
    pub blocks: Vec<CfgBlock>,
    pub entry: Option<i32>,  // block id
    pub exit: Option<i32>,   // block id
}

impl Cfg {
    pub fn new() -> Self {
        Cfg { blocks: Vec::new(), entry: None, exit: None }
    }

    pub fn add_block(&mut self, bb: CfgBlock) -> i32 {
        let id = bb.id;
        self.blocks.push(bb);
        id
    }

    pub fn find_block(&self, id: i32) -> Option<&CfgBlock> {
        self.blocks.iter().find(|b| b.id == id)
    }

    pub fn find_block_mut(&mut self, id: i32) -> Option<&mut CfgBlock> {
        self.blocks.iter_mut().find(|b| b.id == id)
    }
}

// ─── CFG Builder ─────────────────────────────────────────────────────────────

pub struct CfgBuilder {
    blocks: Vec<CfgBlock>,
    next_id: i32,
    current: Option<i32>,  // block id being filled
}

impl CfgBuilder {
    pub fn new() -> Self {
        CfgBuilder { blocks: Vec::new(), next_id: 0, current: None }
    }

    fn alloc_block(&mut self) -> CfgBlock {
        let bb = CfgBlock::new(self.next_id);
        self.next_id += 1;
        bb
    }

    fn new_block(&mut self) -> i32 {
        let bb = self.alloc_block();
        let id = bb.id;
        // Fall-through: a block still being filled with no successor yet is
        // continued by the new block. Without this edge, every loop header
        // (and the blocks after it) was unreachable from anything before it —
        // the graph only ever had branch/back edges, so a body like
        // `decl a; while (…) {…} decl b;` lost `decl b` entirely. Explicit
        // edges (branches, back edges, break/continue) are set by their arms
        // and win; `flush()`ed blocks (post-branch merges, post-`return`)
        // have `current == None` here and get their edges where they belong.
        if let Some(cur) = self.current {
            if let Some(prev) = self.blocks.iter_mut().find(|b| b.id == cur) {
                if prev.true_succ.is_none() && prev.false_succ.is_none() {
                    prev.true_succ = Some(id);
                }
            }
        }
        self.blocks.push(bb);
        // Take over `current`. Every caller does `let b = new_block()` and
        // then builds statements meant to LAND in `b` — but with `current`
        // left unset (a preceding `flush()` clears it), the first
        // `ensure_block()` inside that build silently allocated ANOTHER
        // block: the branch/loop body ended up in an orphan nobody points
        // to, while the successor edge targeted the empty block. Both the
        // refcount trace and the ARC pass walked a half-disconnected graph.
        self.current = Some(id);
        id
    }

    fn flush(&mut self) {
        self.current = None;
    }

    fn ensure_block(&mut self) -> i32 {
        if self.current.is_none() {
            let id = self.new_block();
            self.current = Some(id);
        }
        self.current.unwrap()
    }

    fn set_succ(&mut self, from: i32, to: i32) {
        if let Some(bb) = self.blocks.iter_mut().find(|b| b.id == from) {
            if bb.true_succ.is_none() {
                bb.true_succ = Some(to);
            }
        }
    }

    fn build_sequence(&mut self, stmts: &[AstStmt], break_target: Option<i32>, continue_target: Option<i32>) {
        for s in stmts {
            self.build_stmt(s, break_target, continue_target);
        }
    }

    fn build_stmt(&mut self, s: &AstStmt, break_target: Option<i32>, continue_target: Option<i32>) {
        match s.kind {
            AstStmtKind::Expr | AstStmtKind::Decl => {
                let bb = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                }
            }
            AstStmtKind::Compound => {
                if let AstStmtData::Compound(ref stmts) = s.data {
                    self.build_sequence(stmts, break_target, continue_target);
                }
            }
            AstStmtKind::If => {
                let entry = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == entry) {
                    block.add_stmt(s.clone());
                }
                self.flush();

                // then branch
                let then_entry = self.new_block();
                let then_s = match &s.data { AstStmtData::If { ref then, .. } => Some(&**then), _ => None };
                if let Some(ref then_s) = then_s {
                    self.build_stmt(then_s, break_target, continue_target);
                }
                let then_exit = self.current.unwrap_or(then_entry);
                self.flush();

                // else branch
                let mut else_entry = None;
                let mut else_exit = None;
                let else_s = match &s.data { AstStmtData::If { ref else_, .. } => else_.as_ref(), _ => None };
                if let Some(ref else_s) = else_s {
                    else_entry = Some(self.new_block());
                    self.build_stmt(else_s, break_target, continue_target);
                    else_exit = self.current;
                    self.flush();
                }

                // merge point
                let merge = self.new_block();

                // Set successors
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == entry) {
                    block.true_succ = Some(then_entry);
                    block.false_succ = Some(else_entry.unwrap_or(merge));
                }
                self.set_succ(then_exit, merge);
                if let Some(ee) = else_exit {
                    self.set_succ(ee, merge);
                }
                self.current = Some(merge);
            }
            AstStmtKind::While => {
                // header block: condition
                let header = self.new_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.add_stmt(s.clone());
                }
                self.flush();

                // body
                let body_entry = self.new_block();
                if let AstStmtData::While { ref body, .. } = s.data {
                    self.build_stmt(body, Some(header), Some(header));
                }
                let body_exit = self.current.unwrap_or(body_entry);
                self.flush();

                // after loop
                let after = self.new_block();

                // Set successors
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.true_succ = Some(body_entry);
                    block.false_succ = Some(after);
                }
                self.set_succ(body_exit, header);

                self.current = Some(after);
            }
            AstStmtKind::Do => {
                // body first
                let body_entry = self.new_block();
                if let AstStmtData::Do { ref body, .. } = s.data {
                    self.build_stmt(body, break_target, continue_target);
                }
                let body_exit = self.current.unwrap_or(body_entry);
                self.flush();

                // condition
                let header = self.new_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.add_stmt(s.clone());
                }
                self.flush();

                // after
                let after = self.new_block();

                // Set successors
                self.set_succ(body_entry, header);
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.true_succ = Some(body_entry);
                    block.false_succ = Some(after);
                }
                self.set_succ(body_exit, header);

                self.current = Some(after);
            }
            AstStmtKind::For => {
                // init runs in current block
                let init_s = match &s.data { AstStmtData::For { ref init, .. } => init.as_ref().map(|i| &**i), _ => None };
                if let Some(ref init_s) = init_s {
                    self.build_stmt(init_s, None, None);
                }

                // header (condition)
                let header = self.new_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.add_stmt(s.clone());
                }
                self.flush();

                // body
                let body_entry = self.new_block();
                let body_s = match &s.data { AstStmtData::For { ref body, .. } => Some(&**body), _ => None };
                if let Some(ref body_s) = body_s {
                    self.build_stmt(body_s, Some(header), Some(header));
                }
                let body_exit = self.current.unwrap_or(body_entry);
                self.flush();

                // after
                let after = self.new_block();

                // Set successors
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.true_succ = Some(body_entry);
                    block.false_succ = Some(after);
                }
                self.set_succ(body_exit, header);

                self.current = Some(after);
            }
            AstStmtKind::ForIn => {
                // header
                let header = self.new_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.add_stmt(s.clone());
                }
                self.flush();

                // body
                let body_entry = self.new_block();
                if let AstStmtData::ForIn { ref body, .. } = s.data {
                    self.build_stmt(body, Some(header), Some(header));
                }
                let body_exit = self.current.unwrap_or(body_entry);
                self.flush();

                // after
                let after = self.new_block();

                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == header) {
                    block.true_succ = Some(body_entry);
                    block.false_succ = Some(after);
                }
                self.set_succ(body_exit, header);

                self.current = Some(after);
            }
            AstStmtKind::Break => {
                let bb = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                    if let Some(bt) = break_target {
                        block.true_succ = Some(bt);
                    }
                }
                self.flush();
            }
            AstStmtKind::Continue => {
                let bb = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                    if let Some(ct) = continue_target {
                        block.true_succ = Some(ct);
                    }
                }
                self.flush();
            }
            AstStmtKind::Return => {
                let bb = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                }
                self.flush();
            }
            AstStmtKind::Goto => {
                let bb = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                }
                self.flush();
            }
            AstStmtKind::Label => {
                // Labels are targets — start a new block
                self.flush();
                let bb = self.new_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                }
            }
            AstStmtKind::Switch => {
                let entry = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == entry) {
                    block.add_stmt(s.clone());
                }
                self.flush();
                // Treat switch body as straight-line code
                if let AstStmtData::Switch { ref body, .. } = s.data {
                    self.build_stmt(body, None, None);
                }
                let after = self.new_block();
                self.current = Some(after);
            }
            AstStmtKind::Case | AstStmtKind::Default => {
                // These get their own block as they're jump targets
                let bb = self.new_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                }
                match s.data {
                    AstStmtData::Case { ref body, .. } => {
                        self.build_stmt(body, break_target, continue_target);
                    }
                    AstStmtData::Default(ref body) => {
                        self.build_stmt(body, break_target, continue_target);
                    }
                    _ => {}
                }
            }
            _ => {
                // Default: add to current block
                let bb = self.ensure_block();
                if let Some(block) = self.blocks.iter_mut().find(|b| b.id == bb) {
                    block.add_stmt(s.clone());
                }
            }
        }
    }

    pub fn build(body: &AstStmt) -> Cfg {
        let mut builder = CfgBuilder::new();

        // Create the entry block first and point `current` at it BEFORE any
        // other block exists: with `new_block` now taking over `current` and
        // wiring fall-through, creating the exit here would steal the entry's
        // continuation edge (entry → exit, body unreachable).
        let entry_id = builder.new_block();
        builder.current = Some(entry_id);

        // Build CFG from body
        match body.data {
            AstStmtData::Compound(ref stmts) => {
                builder.build_sequence(stmts, None, None);
            }
            _ => {
                builder.build_stmt(body, None, None);
            }
        }

        // Exit block last: the fall-through in `new_block` wires the trailing
        // block to it. No extra `set_succ(current, exit)` here — `current` IS
        // the exit now, and re-wiring it would give the exit a self-loop.
        let exit_id = builder.new_block();

        // Build final CFG
        let mut cfg = Cfg::new();
        cfg.blocks = builder.blocks;
        cfg.entry = Some(entry_id);
        cfg.exit = Some(exit_id);

        cfg
    }
}

// Build CFG from a method/function body (a compound statement)
pub fn cfg_build(body: &AstStmt) -> Cfg {
    CfgBuilder::build(body)
}

pub fn cfg_print(cfg: &Cfg) {
    println!("CFG ({} blocks):", cfg.blocks.len());
    if let Some(entry) = cfg.entry {
        println!("  entry: B{}", entry);
    }
    if let Some(exit) = cfg.exit {
        println!("  exit: B{}", exit);
    }
    for bb in &cfg.blocks {
        print!("  B{}:", bb.id);
        if cfg.entry == Some(bb.id) { print!(" (entry)"); }
        if cfg.exit == Some(bb.id) { print!(" (exit)"); }
        println!(" {} stmts", bb.stmts.len());
        if let Some(ts) = bb.true_succ { println!("    -> B{}", ts); }
        if let Some(fs) = bb.false_succ { println!("    ->else B{}", fs); }
    }
}