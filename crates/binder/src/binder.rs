use jeti_cst::*;
use jeti_symbol::symbol::*;

// Strip trailing `*` from a type expression, return (stripped, ptr_level)
fn strip_ptr(fqn: &str) -> (String, usize) {
    let trimmed = fqn.trim();
    let mut ptr_level = 0;
    let mut s = trimmed;
    while s.ends_with('*') {
        ptr_level += 1;
        s = s[..s.len()-1].trim();
    }
    (s.to_string(), ptr_level)
}

// Extract the base class name from a type expression (remove `<...>`)
fn base_class_name(fqn: &str) -> String {
    let s = fqn.trim();
    if let Some(pos) = s.find('<') {
        s[..pos].trim().to_string()
    } else {
        s.to_string()
    }
}

pub struct Binder {
    pub symtab: SymbolTable,
    pub current_class: Option<String>,
    pub has_error: bool,
    pub err_msg: String,
    pub ns_prefix: String,
}

impl Binder {
    pub fn new(symtab: SymbolTable) -> Self {
        let mut binder = Binder { symtab, current_class: None, has_error: false, err_msg: String::new(), ns_prefix: String::new() };
        // Register built-in implicit root class jeti_root
        binder.symtab.declare(Symbol::new(SymbolKind::Class, "jeti_root"));
        binder
    }

    pub fn has_error(&self) -> bool { self.has_error }
    pub fn last_error(&self) -> &str { &self.err_msg }

    fn error(&mut self, line: usize, col: usize, msg: &str) {
        self.has_error = true;
        let entry = format!("{}:{}: {}", line, col, msg);
        if self.err_msg.is_empty() {
            self.err_msg = entry;
        } else {
            self.err_msg = format!("{}\n{}", self.err_msg, entry);
        }
    }

    /// The name a declaration is filed under: what was written, under the
    /// `@namespace` block it sits in.
    ///
    /// Deliberately not [`Self::ns_fqn`], which resolves a *use* of a name and
    /// therefore consults the `@using` list first. A declaration is not named by
    /// the spellings that can refer to it, and naming one through that list
    /// renamed it: with `@using namespace A;` written above, `@namespace B {
    /// @interface Player }` bound its class as `A::Player` — one class's methods
    /// spread over the other's, and a single `A__Player` emitted.
    ///
    /// Mirrors the elaborator's `decl_name`; the two must agree, since the
    /// elaborator reads the name back from the symbol this registered.
    fn decl_name(&self, name: &str) -> String {
        if name.contains("::") || self.ns_prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}{}", self.ns_prefix, name)
        }
    }

    fn ns_fqn(&self, name: &str) -> String {
        if name.contains("::") { return name.to_string(); }
        // Check @using namespace entries — try <ns>::<name>
        for entry in &self.symtab.using_list {
            if entry.is_namespace {
                let prefixed = format!("{}::{}", entry.fqn, name);
                if self.symtab.find_class(&prefixed).is_some() || self.symtab.find_protocol(&prefixed).is_some() {
                    return prefixed;
                }
            }
        }
        if self.ns_prefix.is_empty() { name.to_string() }
        else { format!("{}{}", self.ns_prefix, name) }
    }

    fn build_selector_name(&self, d: &CstDecl) -> String {
        if let CstDeclData::Method { ref params, .. } = d.data {
            if let Some(ref p) = params {
                let mut sel = String::new();
                let mut cur = Some(p.as_ref());
                while let Some(param) = cur {
                    if let Some(ref ext) = param.external_name {
                        sel.push_str(ext);
                    }
                    cur = param.next.as_ref().map(|n| n.as_ref());
                }
                if !sel.is_empty() { return sel; }
            }
        }
        d.name.clone().unwrap_or_default()
    }

    fn bind_type(&mut self, ct: &mut CstType) {
        if ct.prim == TypePrim::Named {
            if let Some(ref name) = ct.name.clone() {
                let cls = self.symtab.find_class(name)
                    .or_else(|| {
                        let fqn = self.ns_fqn(name);
                        self.symtab.find_class(&fqn)
                    });
                if let Some(c) = cls {
                    if c.name != *name {
                        let new_name = Some(c.name.clone());
                        ct.name = new_name.clone();
                        if let Some(ref mut sub) = ct.subtype {
                            if sub.name == Some(name.clone()) {
                                sub.name = new_name;
                            }
                        }
                    }
                    return;
                }
                if self.symtab.find_protocol(name).is_some() { return; }
                let type_sym = self.symtab.lookup(name)
                    .and_then(|s| if s.kind == SymbolKind::Type { Some(s) } else { None });
                if let Some(s) = type_sym {
                    if s.name != *name { ct.name = Some(s.name.clone()); }
                    return;
                }
            }
        }
        if let Some(ref mut sub) = ct.subtype { self.bind_type(sub); }
        if let Some(ref mut n) = ct.next { self.bind_type(n); }
        for arg in &mut ct.type_args { self.bind_type(arg); }
        if let Some(ref mut bp) = ct.block_params { self.bind_type(bp); }
    }

    fn bind_type_opt(&mut self, ct: &mut Option<Box<CstType>>) {
        if let Some(ref mut t) = ct { self.bind_type(t); }
    }

    fn bind_params(&mut self, params: &mut Option<Box<CstParam>>) {
        if let Some(ref mut param) = params {
            self.bind_type_opt(&mut param.par_type);
            self.bind_params(&mut param.next);
        }
    }

    fn bind_expr(&mut self, e: &mut CstExpr) {
        match &mut e.data {
            CstExprData::Ident(name) => {
                if name.starts_with("__") {
                    return;
                }
                if self.symtab.lookup(name).is_none() {
                    if let Some(ref cls_name) = self.current_class {
                        if let Some(cls_sym) = self.symtab.find_class(cls_name) {
                            if let SymbolData::Class { ref ivars, .. } = cls_sym.data {
                                if ivars.contains(name) { return; }
                            }
                        }
                    }
                    self.symtab.declare(Symbol::new(SymbolKind::Variable, name));
                }
            }
            CstExprData::Message { receiver, args, .. } => {
                self.bind_expr(receiver);
                for a in args.iter_mut() { self.bind_expr(a); }
            }
            CstExprData::Dot { object, .. } => { self.bind_expr(object); }
            CstExprData::Arrow { object, .. } => { self.bind_expr(object); }
            CstExprData::Subscript { object, key } => {
                self.bind_expr(object);
                self.bind_expr(key);
            }
            CstExprData::Call { callee, args } => {
                self.bind_expr(callee);
                for a in args.iter_mut() { self.bind_expr(a); }
            }
            CstExprData::Unary { operand, .. } => { self.bind_expr(operand); }
            CstExprData::Binary { left, right, .. } => {
                self.bind_expr(left);
                self.bind_expr(right);
            }
            CstExprData::Ternary { cond, true_expr, false_expr } => {
                self.bind_expr(cond);
                self.bind_expr(true_expr);
                self.bind_expr(false_expr);
            }
            CstExprData::Assign { target, value } => {
                self.bind_expr(target);
                self.bind_expr(value);
            }
            CstExprData::Cast { ref mut target_type, expr } => {
                self.bind_type(target_type);
                self.bind_expr(expr);
            }
            CstExprData::Sizeof { ref mut type_expr, expr } => {
                self.bind_type(type_expr);
                if let Some(ref mut ex) = expr { self.bind_expr(ex); }
            }
            CstExprData::Typeof(ref mut ct) => { self.bind_type(ct); }
            CstExprData::Comma(exprs) => { for ex in exprs.iter_mut() { self.bind_expr(ex); } }
            CstExprData::Block { ref mut params, return_type: _, ref mut body, .. } => {
                let mut cur = params.as_mut().map(|p| p.as_mut());
                while let Some(param) = cur {
                    self.bind_type_opt(&mut param.par_type);
                    if let Some(ref pname) = param.name.clone() {
                        self.symtab.declare(Symbol::new(SymbolKind::Variable, &pname));
                    }
                    cur = param.next.as_mut().map(|n| n.as_mut());
                }
                if let Some(ref mut b) = body { self.bind_stmt(b); }
            }
            CstExprData::ArrayLit(elements) => { for ex in elements.iter_mut() { self.bind_expr(ex); } }
            CstExprData::DictLit { ref mut keys, ref mut values } => {
                for k in keys.iter_mut() { self.bind_expr(k); }
                for v in values.iter_mut() { self.bind_expr(v); }
            }
            CstExprData::NumberLit(val) => { self.bind_expr(val); }
            CstExprData::Selector(ref sel_name) => {
                self.symtab.register_selector(sel_name);
            }
            CstExprData::Encode(ref mut ct) => { self.bind_type(ct); }
            CstExprData::InitList(exprs) => { for ex in exprs.iter_mut() { self.bind_expr(ex); } }
            CstExprData::Paren(inner) => { self.bind_expr(inner); }
            _ => {}
        }
    }

    fn bind_stmt(&mut self, s: &mut CstStmt) {
        match &mut s.data {
            CstStmtData::Expr(e) => { self.bind_expr(e); }
            CstStmtData::Compound(stmts) => {
                for stmt in stmts.iter_mut() { self.bind_stmt(stmt); }
            }
            CstStmtData::If { cond, then_branch, else_branch } => {
                self.bind_expr(cond);
                self.bind_stmt(then_branch);
                if let Some(ref mut eb) = else_branch { self.bind_stmt(eb); }
            }
            CstStmtData::Switch { expr, body } => {
                self.bind_expr(expr);
                self.bind_stmt(body);
            }
            CstStmtData::SwitchPat { expr, arms, default_body, .. } => {
                self.bind_expr(expr);
                for arm in arms {
                    match &mut arm.pattern {
                        CstPattern::Const(e) | CstPattern::Cond(e) => self.bind_expr(e),
                        CstPattern::Bind { .. } => {}
                    }
                    if let Some(ref mut g) = arm.guard { self.bind_expr(g); }
                    self.bind_stmt(&mut arm.body);
                }
                if let Some(b) = default_body { self.bind_stmt(b); }
            }
            CstStmtData::Case { value, body } => {
                self.bind_expr(value);
                self.bind_stmt(body);
            }
            CstStmtData::Default(body) => { self.bind_stmt(body); }
            CstStmtData::While { cond, body } => {
                self.bind_expr(cond);
                self.bind_stmt(body);
            }
            CstStmtData::Do { body, cond } => {
                self.bind_stmt(body);
                self.bind_expr(cond);
            }
            CstStmtData::For { init, cond, incr, body } => {
                if let Some(ref mut i) = init { self.bind_stmt(i); }
                if let Some(ref mut c) = cond { self.bind_expr(c); }
                if let Some(ref mut i) = incr { self.bind_expr(i); }
                self.bind_stmt(body);
            }
            CstStmtData::ForIn { var, collection, body } => {
                self.bind_expr(var);
                self.bind_expr(collection);
                self.bind_stmt(body);
            }
            CstStmtData::Return(val) => { if let Some(ref mut v) = val { self.bind_expr(v); } }
            CstStmtData::Throw(val) => { if let Some(ref mut v) = val { self.bind_expr(v); } }
            CstStmtData::Try { try_block, catches, finally_block } => {
                self.bind_stmt(try_block);
                for c in catches.iter_mut() { self.bind_stmt(c); }
                if let Some(ref mut f) = finally_block { self.bind_stmt(f); }
            }
            CstStmtData::Catch { ref mut param, body } => {
                self.bind_type_opt(&mut param.par_type);
                if let Some(ref pname) = param.name.clone() {
                    self.symtab.declare(Symbol::new(SymbolKind::Variable, &pname));
                }
                self.bind_stmt(body);
            }
            CstStmtData::Finally(body) => { self.bind_stmt(body); }
            CstStmtData::Synchronized { lock, body } => {
                self.bind_expr(lock);
                self.bind_stmt(body);
            }
            CstStmtData::Autoreleasepool(body) => { self.bind_stmt(body); }
            CstStmtData::NoArc(body) => { self.bind_stmt(body); }
            // Defer bodies must bind normally: the defer pass (pipeline
            // Step 3.9) runs AFTER binding and splices the body verbatim into
            // exit points, so declarations inside it need their symtab
            // entries now.
            CstStmtData::Defer(body) => { self.bind_stmt(body); }
            CstStmtData::Decl(ref mut d) => { self.bind_decl(d); }
            _ => {}
        }
    }

    fn bind_method_body(&mut self, d: &mut CstDecl) {
        if let CstDeclData::Method { ref mut body, ref params, .. } = d.data {
            if let Some(ref mut b) = body {
                let mut cur = params.as_ref().map(|p| p.as_ref());
                while let Some(p) = cur {
                    if let Some(ref pname) = p.name {
                        self.symtab.declare(Symbol::new(SymbolKind::Variable, pname));
                    }
                    cur = p.next.as_ref().map(|n| n.as_ref());
                }
                self.bind_stmt(b);
            }
        }
    }

    fn bind_decl(&mut self, d: &mut CstDecl) {
        let kind = d.kind;
        match kind {
            CstDeclKind::ClassInterface => {
                let cls_name = self.decl_name(d.name.as_deref().unwrap_or(""));
                let old_class = self.current_class.clone();

                let is_category = matches!(d.data, CstDeclData::Class { ref category_name, .. } if category_name.is_some());
                // Capture superclass from the CST so the elaborator's ivar resolver
                // can walk the superclass chain (subclass methods referencing an
                // ivar declared in the parent, e.g. `_nodeType` declared in
                // NPJsonNode used inside NPJsonStringNode's init).
                let superclass_from_cst = match &d.data {
                    CstDeclData::Class { ref superclass, .. } => superclass.clone(),
                    _ => None,
                };
                // Generic classes must record their type parameters, or the
                // checker cannot tell `Box<T>` (which monomorphizes) from a
                // non-generic class (whose `<...>` is silently erased).
                let type_params_from_cst = match &d.data {
                    CstDeclData::Class { ref type_params, .. } => type_params.clone(),
                    _ => Vec::new(),
                };
                // Bounds ride along the same way (which param carries which
                // protocol/class constraint); the checker is their consumer.
                let type_bounds_from_cst = match &d.data {
                    CstDeclData::Class { ref type_bounds, .. } => type_bounds.clone(),
                    _ => Vec::new(),
                };
                // Protocol conformances (`@interface Dog <Greetable>`) — the
                // checker's bound enforcement resolves them via the class
                // symbol, so they must be recorded here (names resolved like
                // protocol parents below: known protocol kept as written,
                // otherwise namespace-qualified).
                let protocols_from_cst = match &d.data {
                    CstDeclData::Class { ref protocols, .. } => protocols.clone(),
                    _ => Vec::new(),
                };
                if self.symtab.find_class(&cls_name).is_none() {
                    self.symtab.declare(Symbol::new(SymbolKind::Class, &cls_name));
                }
                // Record superclass on the class symbol (overwrite if already set,
                // e.g. a forward @class declaration left it None).
                if let Some(ref sup) = superclass_from_cst {
                    let sup_fqn = if self.symtab.find_class(sup).is_some() {
                        sup.clone()
                    } else {
                        self.ns_fqn(sup)
                    };
                    for sym in self.symtab.global.symbols.iter_mut() {
                        if sym.name == cls_name && sym.kind == SymbolKind::Class {
                            if let SymbolData::Class { ref mut superclass, .. } = sym.data {
                                *superclass = Some(sup_fqn);
                            }
                            break;
                        }
                    }
                }
                self.current_class = Some(cls_name.clone());

                // Collect ivar/property/method names from CST (for class symbol data)
                let mut ivar_names = Vec::new();
                let mut prop_names = Vec::new();
                let mut method_names = Vec::new();

                if !is_category {
                    if let CstDeclData::Class { ref mut ivars, ref mut properties, .. } = d.data {
                        for ivar in ivars.iter_mut() {
                            if let Some(ref n) = ivar.name { ivar_names.push(n.clone()); }
                            self.bind_decl(ivar);
                        }
                        for prop in properties.iter_mut() {
                            if let Some(ref n) = prop.name { prop_names.push(n.clone()); }
                            self.bind_decl(prop);
                            // Follow the next chain (comma-separated properties)
                            let mut cur = prop.next.as_mut().map(|n| n.as_mut());
                            while let Some(next_prop) = cur {
                                if let Some(ref n) = next_prop.name { prop_names.push(n.clone()); }
                                self.bind_decl(next_prop);
                                cur = next_prop.next.as_mut().map(|n| n.as_mut());
                            }
                        }
                    }
                }
                if let CstDeclData::Class { ref mut methods, .. } = d.data {
                    for method in methods.iter_mut() {
                        let sel = self.build_selector_name(method);
                        method_names.push(sel);
                        self.bind_decl(method);
                    }
                }

                // Update class symbol data with ivar/property/method names
                // Protocol names are resolved BEFORE the mutable loop:
                // find_protocol needs an immutable borrow of symtab.
                let protocols_resolved: Vec<String> = protocols_from_cst.iter().map(|p| {
                    if self.symtab.find_protocol(p).is_some() {
                        p.clone()
                    } else if !cls_name.contains("::") {
                        p.clone()
                    } else {
                        format!("{}::{}", cls_name.split("::").next().unwrap_or(""), p)
                    }
                }).collect();
                for sym in self.symtab.global.symbols.iter_mut() {
                    if sym.name == cls_name && sym.kind == SymbolKind::Class {
                        if let SymbolData::Class { ref mut ivars, ref mut properties, ref mut methods, ref mut type_params, ref mut type_bounds, ref mut protocols, .. } = sym.data {
                            ivars.extend(ivar_names);
                            properties.extend(prop_names);
                            methods.extend(method_names);
                            // Overwrite (not extend): a class may be bound more
                            // than once in a TU (e.g. re-bound after a `@class`
                            // forward declaration), and a re-binding without type
                            // params must not leave stale ones behind.
                            if !type_params_from_cst.is_empty() {
                                *type_params = type_params_from_cst.clone();
                            }
                            if !type_bounds_from_cst.is_empty() {
                                *type_bounds = type_bounds_from_cst.clone();
                            }
                            if !protocols_from_cst.is_empty() {
                                protocols.clear();
                                for r in &protocols_resolved {
                                    protocols.push(r.clone());
                                }
                            }
                        }
                        break;
                    }
                }

                self.current_class = old_class;
            }

            CstDeclKind::ClassImplementation => {
                // An `@implementation` (like a category) names a class that must
                // already exist, so it resolves like a use of that name — the
                // block it sits in first, so the class the surrounding
                // `@namespace` declared wins over an import, and only then the
                // `@using` list (`@implementation GP` for `@using GP = …;`).
                // Mirrors the elaborator's rule for the same declaration.
                let written = d.name.as_deref().unwrap_or("");
                let scoped = self.decl_name(written);
                let cls_name = if self.symtab.find_class(&scoped).is_some() {
                    scoped
                } else {
                    self.ns_fqn(written)
                };
                let old_class = self.current_class.clone();
                if self.symtab.find_class(&cls_name).is_none() {
                    self.error(d.line, d.column, &format!("cannot find class '{}' for @implementation", cls_name));
                    return;
                }
                self.current_class = Some(cls_name);

                let mut mcount = 0;
                if let CstDeclData::Class { ref methods, .. } = d.data {
                    mcount = methods.len();
                }
                let mut indices = Vec::new();
                for i in 0..mcount {
                    if let CstDeclData::Class { ref methods, .. } = d.data {
                        if i < methods.len() && methods[i].kind == CstDeclKind::Method {
                            indices.push(i);
                        }
                    }
                }
                for i in indices {
                    if let CstDeclData::Class { ref mut methods, .. } = d.data {
                        if i < methods.len() {
                            self.bind_decl(&mut methods[i]);
                        }
                    }
                }
                // Bind C-level declarations inside @implementation (e.g. static
                // helper functions and variables stored in impl_vars) so that
                // namespace-qualified types (e.g. `Table` → `TOML::Table`) are
                // resolved via ns_prefix.
                if let CstDeclData::Class { ref mut impl_vars, .. } = d.data {
                    for v in impl_vars.iter_mut() {
                        self.bind_decl(v);
                    }
                }
                self.current_class = old_class;
            }

            CstDeclKind::Method => {
                if let CstDeclData::Method { ref mut return_type, ref mut params, .. } = d.data {
                    self.bind_type_opt(return_type);
                    self.bind_params(params);
                }
                self.bind_method_body(d);
            }

            CstDeclKind::Variable => {
                if let CstDeclData::Variable { ref mut var_type, ref mut initializer, is_block_qual, is_weak, .. } = d.data {
                    self.bind_type_opt(var_type);
                    if let Some(ref mut init) = initializer { self.bind_expr(init); }
                    // Register the variable with its type so the elaborator's
                    // convert_dot_expr can resolve `obj.field` access to the
                    // correct ObjC class (forcing `->` for pointer-typed
                    // instances). Without this, `s.grade` on `Student *s`
                    // falls through to plain C `.` and fails to compile.
                    if let Some(ref name) = d.name {
                        let var_t = var_type.as_ref().map(|t| Box::new(NpType::from_cst(t)));
                        if self.symtab.lookup(name).is_none() {
                            let mut sym = Symbol::new(SymbolKind::Variable, name);
                            if let SymbolData::Variable { ref mut var_type, is_static: _, is_extern: _, is_const: _, is_weak: ref mut w, is_block: ref mut b } = sym.data {
                                *var_type = var_t;
                                *w = is_weak;
                                *b = is_block_qual;
                            }
                            self.symtab.declare(sym);
                        } else if let Some(existing) = self.symtab.current.symbols.iter_mut().find(|s| s.name == *name) {
                            // Update existing variable's type if missing
                            if let SymbolData::Variable { ref mut var_type, .. } = existing.data {
                                if var_type.is_none() { *var_type = var_t; }
                            }
                        }
                    }
                }
            }

            CstDeclKind::Typedef => {
                if let CstDeclData::Typedef { ref mut alias_type, .. } = d.data {
                    self.bind_type_opt(alias_type);
                }
                if let Some(ref name) = d.name {
                    if self.symtab.lookup(name).is_none() {
                        self.symtab.declare(Symbol::new(SymbolKind::Type, &self.decl_name(name)));
                    }
                }
            }

            CstDeclKind::Struct | CstDeclKind::Union => {
                if let Some(ref name) = d.name {
                    if self.symtab.find_class(name).is_none() && self.symtab.lookup(name).is_none() {
                        self.symtab.declare(Symbol::new(SymbolKind::Class, name));
                    }
                }
                if let CstDeclData::Aggregate { ref mut fields, .. } = d.data {
                    for f in fields.iter_mut() { self.bind_decl(f); }
                }
            }

            CstDeclKind::Enum => {
                if let Some(ref name) = d.name {
                    if self.symtab.lookup(name).is_none() {
                        self.symtab.declare(Symbol::new(SymbolKind::Type, name));
                    }
                }
                if let CstDeclData::Enum { ref members, ref mut values } = d.data {
                    for (i, member) in members.iter().enumerate() {
                        if self.symtab.lookup(member).is_none() {
                            self.symtab.declare(Symbol::new(SymbolKind::Variable, member));
                        }
                        if i < values.len() { self.bind_expr(&mut values[i]); }
                    }
                }
            }

            CstDeclKind::Ivar => {
                if let CstDeclData::Ivar { ref mut ivar_type, .. } = d.data {
                    self.bind_type_opt(ivar_type);
                }
            }

            CstDeclKind::Property => {
                // Snapshot the property data we need by cloning the
                // non-mutable fields. We bind the type separately.
                let (prop_type_clone, getter_c, setter_c, is_readonly, is_weak, is_assign, is_retain, is_copy, is_dynamic, is_nonatomic) =
                    if let CstDeclData::Property { ref prop_type, ref getter, ref setter, ref is_readonly, ref is_weak, ref is_assign, ref is_retain, ref is_copy, ref is_dynamic, ref is_nonatomic, .. } = d.data {
                        (prop_type.clone(), getter.clone(), setter.clone(), *is_readonly, *is_weak, *is_assign, *is_retain, *is_copy, *is_dynamic, *is_nonatomic)
                    } else { (None, None, None, false, false, false, false, false, false, false) };
                // Bind the type in place so namespace qualifiers resolve.
                if let CstDeclData::Property { ref mut prop_type, .. } = d.data {
                    self.bind_type_opt(prop_type);
                }
                // Register the property symbol in the global scope so the
                // elaborator's elaborate_class() can synthesize the backing
                // ivar (e.g. `_name` for `name`) and getter/setter methods.
                // Without this, `@synthesize name = _name` looks up a missing
                // property symbol and the ivar list stays empty, leaving bare
                // `_name` references undeclared in method bodies.
                if let Some(ref name) = d.name {
                    // Register by the original (possibly FQN'd) name, matching
                    // the class symbol's `properties` list which uses the same
                    // name form. elaborate_class() looks up properties by the
                    // same name stored in the class's `properties` field.
                    // Property symbols go directly into the global scope
                    // (declare() only auto-globals Class/Protocol/Type/Function).
                    if self.symtab.global.symbols.iter().find(|s| s.name == *name && s.kind == SymbolKind::Property).is_none() {
                        let mut sym = Symbol::new(SymbolKind::Property, name);
                        sym.data = SymbolData::Property {
                            prop_type: prop_type_clone.as_ref().map(|t| Box::new(NpType::from_cst(t))),
                            ivar_sym: None,
                            getter: getter_c,
                            setter: setter_c,
                            is_readonly,
                            is_weak,
                            is_assign,
                            is_retain,
                            is_copy,
                            is_dynamic,
                            is_nonatomic,
                        };
                        self.symtab.global.add(sym.clone());
                        self.symtab.current.add(sym);
                    }
                }
            }

            CstDeclKind::Function => {
                if let CstDeclData::Function { ref mut return_type, ref mut params, ref mut body, .. } = d.data {
                    self.bind_type_opt(return_type);
                    self.bind_params(params);
                    if let Some(ref mut b) = body { self.bind_stmt(b); }
                }
                if let Some(ref name) = d.name {
                    if self.symtab.lookup(name).is_none() {
                        self.symtab.declare(Symbol::new(SymbolKind::Function, name));
                    }
                }
            }

            CstDeclKind::Protocol => {
                if let Some(ref name) = d.name {
                    if self.symtab.find_protocol(name).is_none() {
                        self.symtab.declare(Symbol::new(SymbolKind::Protocol, name));
                    }
                }
                if let CstDeclData::ProtocolData { ref mut methods, ref protocols, is_optional } = d.data {
                    for m in methods.iter_mut() { self.bind_decl(m); }
                    // Record the protocol's method names (and its parents) on the
                    // symbol. Without this the protocol's requirements are lost
                    // after binding, and a class that conforms to the protocol
                    // never gets vtable slots for them — which shows up as a
                    // cross-TU vtable layout mismatch when the implementation
                    // lives in a different translation unit than the header.
                    //
                    // `is_optional` is a per-protocol switch (the parser flips it
                    // at `@optional` / `@required` and applies to every method
                    // that follows), not a per-method flag.
                    if let Some(ref name) = d.name {
                        let fqn = self.decl_name(name);
                        let pname = if self.symtab.find_protocol(name).is_some() {
                            name.clone()
                        } else {
                            fqn
                        };
                        let sels: Vec<String> = methods
                            .iter()
                            .filter(|m| m.kind == CstDeclKind::Method)
                            .map(|m| self.build_selector_name(m))
                            .filter(|s| !s.is_empty())
                            .collect();
                        let mut parents: Vec<String> = Vec::new();
                        for p in protocols {
                            let pf = self.ns_fqn(p);
                            parents.push(if self.symtab.find_protocol(p).is_some() { p.clone() } else { pf });
                        }
                        for sym in self.symtab.global.symbols.iter_mut() {
                            if sym.name == pname && sym.kind == SymbolKind::Protocol {
                                if let SymbolData::Protocol { required_methods, optional_methods, parents: ps } = &mut sym.data {
                                    for m in sels {
                                        let target = if is_optional { &mut *optional_methods } else { &mut *required_methods };
                                        if !target.contains(&m) { target.push(m); }
                                    }
                                    for m in parents.drain(..) { if !ps.contains(&m) { ps.push(m); } }
                                }
                                break;
                            }
                        }
                    }
                }
            }

            CstDeclKind::ForwardClass => {
                if let CstDeclData::Forward(ref names) = d.data {
                    for name in names.iter() {
                        let fqn = self.decl_name(name);
                        if self.symtab.find_class(&fqn).is_none() {
                            self.symtab.declare(Symbol::new(SymbolKind::Class, &fqn));
                        }
                    }
                }
            }

            CstDeclKind::Namespace => {
                if let CstDeclData::Namespace(ref mut decls) = d.data {
                    if let Some(ref name) = d.name {
                        let old = self.ns_prefix.clone();
                        self.ns_prefix = format!("{}{}::", old, name);
                        for decl in decls.iter_mut() { self.bind_decl(decl); }
                        self.ns_prefix = old;
                    }
                }
            }

            CstDeclKind::Using => {
                if let CstDeclData::Using { ref fqn, ref alias } = d.data {
                    let line = d.line; let col = d.column;
                    // Determine the short name that will become visible.
                    let short = if let Some(ref a) = alias {
                        Some(a.clone())
                    } else if fqn.contains("::") {
                        Some(fqn.rsplit("::").next().unwrap_or(fqn).to_string())
                    } else {
                        None // @using namespace X; — no short name introduced
                    };
                    if let Some(ref sname) = short {
                        // Conflict with an existing @using short name (ambiguity).
                        if let Some(existing) = self.symtab.find_using(sname) {
                            self.error(line, col, &format!("ambiguous import: '{}' imported from both '{}' and '{}'", sname, existing.fqn, fqn));
                            return;
                        }
                        // Conflict with an existing symbol in the current scope.
                        if self.symtab.find_class(sname).is_some()
                            || self.symtab.find_protocol(sname).is_some()
                            || self.symtab.find_type(sname).is_some() {
                            self.error(line, col, &format!("'{}' conflicts with an existing symbol", sname));
                            return;
                        }
                    }
                    if let Some(ref a) = alias {
                        // @using Alias = TypeExpr*;
                        // Strip trailing `*` and type args for the find_class check,
                        // but store the base FQN (with type args) for later resolution.
                        let (base_fqn, ptr_level) = strip_ptr(fqn);
                        let base_name = base_class_name(&base_fqn);
                        if base_name == "id" || self.symtab.find_class(&base_name).is_some() || self.symtab.find_protocol(&base_name).is_some() {
                            self.symtab.add_using(&base_fqn, a, ptr_level, false);
                        }
                    } else if fqn.contains("::") {
                        let short = fqn.rsplit("::").next().unwrap_or(fqn);
                        if self.symtab.find_class(fqn).is_some() || self.symtab.find_protocol(fqn).is_some() {
                            self.symtab.add_using(fqn, short, 0, false);
                        } else {
                            // @using namespace Engine::Physics; — register as namespace prefix
                            self.symtab.add_using(fqn, fqn, 0, true);
                        }
                    } else {
                        // @using namespace Render; — register as namespace prefix
                        self.symtab.add_using(fqn, fqn, 0, true);
                    }
                }
            }

            _ => {}
        }
    }

    /// Collect every selector a protocol requires, walking its parents.
    /// Returns (required, optional); an optional method does not oblige the
    /// conforming class to declare it.
    fn protocol_selectors(&self, proto: &str, seen: &mut Vec<String>) -> (Vec<String>, Vec<String>) {
        if seen.iter().any(|s| s == proto) { return (Vec::new(), Vec::new()); }
        seen.push(proto.to_string());
        let sym = match self.symtab.find_protocol(proto) {
            Some(s) => s,
            None => return (Vec::new(), Vec::new()),
        };
        let (required, optional, parents) = match &sym.data {
            SymbolData::Protocol { required_methods, optional_methods, parents } =>
                (required_methods.clone(), optional_methods.clone(), parents.clone()),
            _ => return (Vec::new(), Vec::new()),
        };
        let mut req = required;
        let mut opt = optional;
        for p in parents {
            let (pr, po) = self.protocol_selectors(&p, seen);
            for m in pr { if !req.contains(&m) { req.push(m); } }
            for m in po { if !opt.contains(&m) { opt.push(m); } }
        }
        (req, opt)
    }

    /// After binding, copy each conforming class's protocol-required METHOD
    /// DECLARATIONS into the class's own `@interface`, so downstream stages
    /// (elaborator, codegen) see a single, complete set of slots for the class
    /// regardless of which translation unit it came from.
    ///
    /// This must be a CST-level injection, not a symbol-table one: codegen
    /// builds each class's vtable slots from the AST method list, so a class
    /// that only *conforms* to a protocol (without redeclaring the methods)
    /// would otherwise compile a layout with fewer slots than a translation
    /// unit that does see the methods — a cross-TU vtable mismatch.
    fn propagate_protocol_methods(&mut self, decls: &mut [CstDecl], ns: &str) {
        // Snapshot what each class needs, keyed by (class_fqn, protocol_fqn).
        let mut conformances: Vec<(String, String)> = Vec::new();
        collect_conformance_pairs(decls, ns, &mut conformances);
        if conformances.is_empty() { return; }

        // Protocol method declarations, keyed by protocol fqn. Cloned from the
        // CST so the injected declaration carries the real signature.
        let mut proto_methods: Vec<(String, Vec<CstDecl>)> = Vec::new();
        collect_protocol_method_decls(decls, ns, &mut proto_methods);

        for (cls_fqn, proto_fqn) in conformances {
            let mut seen = Vec::new();
            let (req, _opt) = self.protocol_selectors(&proto_fqn, &mut seen);
            if req.is_empty() { continue; }
            // Resolve each required selector to its declaring CstDecl in the
            // protocol (or an ancestor protocol), so we can clone it in.
            let mut to_add: Vec<CstDecl> = Vec::new();
            for sel in req {
                if let Some(decl) = find_protocol_method_decl(&proto_methods, &proto_fqn, &sel, &mut Vec::new()) {
                    let already = to_add.iter().any(|m| selector_of(m) == sel);
                    if !already { to_add.push(decl); }
                }
            }
            if to_add.is_empty() { continue; }
            inject_methods_into_class(decls, ns, &cls_fqn, to_add);
        }
    }

    pub fn bind(&mut self, unit: &mut TranslationUnit) -> i32 {
        for decl in unit.decls.iter_mut() { self.bind_decl(decl); }
        if self.has_error { return -1; }
        // Protocol requirements must reach the class regardless of declaration
        // order (`@protocol` after the `@interface`, or in a different file), so
        // this runs as a post-pass over the finished tree.
        let decls = &mut unit.decls;
        self.propagate_protocol_methods(decls, "");
        if self.has_error { -1 } else { 0 }
    }
}

/// The selector a method declaration defines: the concatenation of its
/// parameter external names (the ObjC selector), falling back to its name.
fn selector_of(m: &CstDecl) -> String {
    if let CstDeclData::Method { ref params, .. } = m.data {
        if let Some(ref p) = params {
            let mut sel = String::new();
            let mut cur = Some(p.as_ref());
            while let Some(param) = cur {
                if let Some(ref ext) = param.external_name { sel.push_str(ext); }
                cur = param.next.as_ref().map(|n| n.as_ref());
            }
            if !sel.is_empty() { return sel; }
        }
    }
    m.name.clone().unwrap_or_default()
}

/// Find the declaration of `sel` in `proto` or any of its ancestor protocols.
fn find_protocol_method_decl(
    table: &[(String, Vec<CstDecl>)],
    proto: &str,
    sel: &str,
    seen: &mut Vec<String>,
) -> Option<CstDecl> {
    if seen.iter().any(|s| s == proto) { return None; }
    seen.push(proto.to_string());
    let (_, methods) = table.iter().find(|(p, _)| p == proto)?;
    if let Some(m) = methods.iter().find(|m| selector_of(m) == sel) {
        return Some(m.clone());
    }
    // Walk ancestors recorded on the protocol symbol.
    None.or_else(|| None)
}

/// Walk the tree, recording `(class_fqn, protocol_fqn)` for every
/// `@interface X : Super <Proto...>`.
fn collect_conformance_pairs(decls: &[CstDecl], ns: &str, out: &mut Vec<(String, String)>) {
    for d in decls {
        if let CstDeclData::Namespace(ref inner) = d.data {
            if let Some(ref name) = d.name {
                let nested = if ns.is_empty() { name.clone() } else { format!("{}::{}", ns, name) };
                collect_conformance_pairs(inner, &nested, out);
            }
            continue;
        }
        if let CstDeclData::Class { ref protocols, .. } = d.data {
            if let Some(ref cname) = d.name {
                let fqn = if ns.is_empty() { cname.clone() } else { format!("{}::{}", ns, cname) };
                for p in protocols {
                    let pf = if p.contains("::") { p.clone() } else if ns.is_empty() { p.clone() } else { format!("{}::{}", ns, p) };
                    if !out.iter().any(|(c, q)| *c == fqn && *q == pf) {
                        out.push((fqn.clone(), pf));
                    }
                }
            }
        }
    }
}

/// Walk the tree, recording each `@protocol`'s fqn and its method declarations.
fn collect_protocol_method_decls(decls: &[CstDecl], ns: &str, out: &mut Vec<(String, Vec<CstDecl>)>) {
    for d in decls {
        if let CstDeclData::Namespace(ref inner) = d.data {
            if let Some(ref name) = d.name {
                let nested = if ns.is_empty() { name.clone() } else { format!("{}::{}", ns, name) };
                collect_protocol_method_decls(inner, &nested, out);
            }
            continue;
        }
        if let CstDeclData::ProtocolData { ref methods, .. } = d.data {
            if let Some(ref name) = d.name {
                let fqn = if ns.is_empty() { name.clone() } else { format!("{}::{}", ns, name) };
                out.push((fqn, methods.clone()));
            }
        }
    }
}

/// Append `adds` to the `@interface` named `cls_fqn`, skipping any selector it
/// already declares (directly or via the class's own list).
fn inject_methods_into_class(decls: &mut [CstDecl], ns: &str, cls_fqn: &str, adds: Vec<CstDecl>) {
    for d in decls.iter_mut() {
        if let CstDeclData::Namespace(ref mut inner) = d.data {
            if let Some(ref name) = d.name {
                let nested = if ns.is_empty() { name.clone() } else { format!("{}::{}", ns, name) };
                inject_methods_into_class(inner, &nested, cls_fqn, adds.clone());
            }
            continue;
        }
        let name_matches = match d.name {
            Some(ref n) => {
                let fqn = if ns.is_empty() { n.clone() } else { format!("{}::{}", ns, n) };
                fqn == cls_fqn
            }
            None => false,
        };
        if !name_matches { continue; }
        if let CstDeclData::Class { ref mut methods, .. } = d.data {
            for a in adds {
                if methods.iter().any(|m| selector_of(m) == selector_of(&a)) { continue; }
                methods.push(a);
            }
        }
        return;
    }
}