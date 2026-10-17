//! A symbol index built from the elaborated AST, for the requests that ask
//! *about* the code instead of reporting problems in it:
//! `textDocument/documentSymbol` (the outline) and `textDocument/definition`.
//!
//! Two rules keep this honest:
//!
//! - Positions come from the shared model only. Every node's `(line, col)` /
//!   `(end_line, end_col)` goes through [`SourceMap::span_from_node`], and the
//!   resulting span is projected with [`SourceSpan::to_lsp_range`] — the same
//!   single crossing into UTF-16 the diagnostics use. Nothing here derives a
//!   column of its own.
//! - A declaration without a source origin is not indexed. Passes synthesize
//!   nodes (`line == 0`), and a symbol nobody can point at would force an
//!   invented range; those are dropped instead.
//!
//! Indexed today: classes, categories, protocols, methods, properties, ivars,
//! functions, file-scope variables, typedefs, structs/unions, enums and
//! namespaces (nested). Forward declarations are skipped — they are not
//! definitions. Enum members, protocol method lists and local variables are not
//! indexed: the AST drops their positions, and a jump target without a position
//! is worse than no jump target.
//!
//! Because `#import` is inlined, the AST of a file that imports Foundation also
//! contains Foundation's declarations. That is a feature for lookups (you can
//! jump from your code into a header) and is filtered out of the outline by
//! path.

use std::collections::{BTreeSet, HashMap};

use jeti_ast::ast::{AstDecl, AstDeclData, AstExpr, AstExprData, AstStmt, AstStmtData, AstType, AstUnit};
use jeti_cst::source_map::{LspPosition, LspRange, SourceMap};
use jeti_cst::{CstParam, CstType, TypePrim};
use serde_json::{json, Value};

use crate::same_file;

/// LSP `SymbolKind` numbers (the wire values of `vscode.SymbolKind`).
pub(crate) mod kind {
    pub const NAMESPACE: u8 = 3;
    /// Classes, categories and typedefs: LSP has no alias kind, so a typedef is
    /// a `Class` with a `typedef` detail (clangd does the same).
    pub const CLASS: u8 = 5;
    pub const METHOD: u8 = 6;
    pub const PROPERTY: u8 = 7;
    pub const FIELD: u8 = 8;
    pub const ENUM: u8 = 10;
    pub const INTERFACE: u8 = 11;
    pub const FUNCTION: u8 = 12;
    pub const VARIABLE: u8 = 13;
    pub const STRUCT: u8 = 23;
}

/// One indexed declaration. The tree answers the outline; the flattened map
/// answers definition lookups.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Name to display (namespace prefixes stripped when the container already
    /// shows them; a category reads `Dog (Tricks)`).
    pub name: String,
    /// Name a definition lookup answers to: the fully-qualified spelling for
    /// namespaced declarations, the selector for methods.
    pub lookup: String,
    /// LSP `SymbolKind` number.
    pub kind: u8,
    /// Short signature line, LSP `detail`.
    pub detail: String,
    /// Enclosing symbol name, LSP `containerName`.
    pub container: Option<String>,
    /// File the declaration lives in.
    pub path: String,
    /// LSP range covering the whole declaration.
    pub range: LspRange,
    /// Nested declarations (members of a class, decls of a namespace).
    pub children: Vec<Symbol>,
}

/// A declaration that can be jumped to, hovered, or completed with.
#[derive(Debug, Clone)]
pub struct Location {
    /// Declared name as the index keys it (a selector for methods).
    pub name: String,
    /// LSP `SymbolKind` number.
    pub kind: u8,
    /// Short signature line, LSP `detail`.
    pub detail: String,
    pub container: Option<String>,
    pub path: String,
    pub range: LspRange,
}

/// One completion candidate: a name the index knows, with what it is.
///
/// `kind` is an **LSP `CompletionItemKind`**, not the `SymbolKind` used by
/// out/definitions — the two enums number their entries differently, and the
/// editor picks its icon from the completion one.
#[derive(Debug, Clone)]
pub struct Completion {
    pub label: String,
    pub kind: u8,
    pub detail: String,
}

/// What the name under the cursor names, once a qualified name has been taken
/// apart the way a C++ editor does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A declaration the index has: [`SymbolIndex::definitions`] answers for it.
    Symbol(String),
    /// A namespace the index knows only from the qualified names of what it
    /// holds — a name that was never written as a `@namespace` block itself
    /// (a class named `A::B` by hand). There is no declaration to jump to; the
    /// name is real, and its members agreeing on the prefix is all the index
    /// has. A namespace that *was* declared resolves as
    /// [`Resolved::Symbol`] and carries the block's position.
    Namespace(String),
}

/// `SymbolKind` → `CompletionItemKind`, so completions carry the icon kind the
/// completion protocol expects.
fn completion_kind(symbol_kind: u8) -> u8 {
    match symbol_kind {
        kind::NAMESPACE => 9,  // Module
        kind::CLASS => 7,      // Class
        kind::METHOD => 2,     // Method
        kind::PROPERTY => 10,  // Property
        kind::FIELD => 5,      // Field
        kind::ENUM => 13,      // Enum
        kind::INTERFACE => 8,  // Interface
        kind::FUNCTION => 3,   // Function
        kind::VARIABLE => 6,   // Variable
        kind::STRUCT => 22,    // Struct
        _ => 1,                // Text
    }
}

/// Upper bound on a `textDocument/completion` answer. Real Jeti programs sit far
/// below it (the whole Foundation surface is a few hundred names); the cap only
/// stops a pathological index from producing a megabyte of JSON.
const COMPLETION_LIMIT: usize = 2000;

/// One `@namespace` block's extent, kept so a bare name written inside it can be
/// read the way the elaborator reads it — prefixed with the block's
/// fully-qualified name (see [`SymbolIndex::narrow`]).
#[derive(Debug, Clone)]
struct NamespaceBlock {
    /// Fully-qualified name of the block, `A::Inner` for a nested one.
    name: String,
    path: String,
    /// The block's extent, `@namespace` line through `@endnamespace`.
    range: LspRange,
}

/// One class as member completion needs it: its ancestors' names and its own
/// members, all keyed by the class's lookup name.
#[derive(Debug, Default)]
struct ClassInfo {
    super_name: Option<String>,
    members: Vec<MemberEntry>,
}

/// A callable's signature, as an editor wants it: the label to show, and the
/// parameter labels inside it (LSP matches them as substrings, so no offsets
/// have to be tracked).
#[derive(Debug, Clone)]
pub struct Signature {
    pub label: String,
    pub parameters: Vec<String>,
}

/// The answer to `textDocument/signatureHelp`: the candidates and which one and
/// which parameter the cursor is on.
#[derive(Debug, Clone)]
pub struct SignatureHelp {
    pub signatures: Vec<Signature>,
    pub active_signature: usize,
    pub active_parameter: usize,
}

/// A member offered after a receiver: a method selector, a property or an ivar.
#[derive(Debug, Clone)]
pub struct MemberEntry {
    pub label: String,
    /// LSP `SymbolKind` number (converted to `CompletionItemKind` on the way out).
    pub kind: u8,
    pub detail: String,
    /// Class methods answer to class receivers, instance members to objects.
    pub is_class_method: bool,
    /// Declared type, for resolving *this* member as a receiver in turn.
    pub type_name: Option<String>,
    /// Present for methods: what signature help shows.
    pub signature: Option<Signature>,
}

/// What is visible at a position: the enclosing method's parameters, `self`, the
/// enclosing class's ivars, and the locals declared in the body — plus, as a
/// fallback for any position, the file's globals.
///
/// Scopes are coarse on purpose: one per method or function rather than one per
/// block. Block-level shadowing inside a single method is therefore flattened
/// (later declarations win), which is what a completion list can live with.
#[derive(Debug, Default)]
pub struct Scope {
    /// Extent of the scope; `None` for file-scope names, which apply everywhere
    /// in the document.
    pub range: Option<LspRange>,
    /// Enclosing method or function name (for labels and debugging).
    pub owner: String,
    /// `self`'s class, inside a method.
    pub self_type: Option<String>,
    /// Visible names → their declared class name.
    pub locals: Vec<(String, String)>,
}

/// A message send in the edited document.
///
/// This exists because the *text* cannot tell a selector position from an
/// argument position (`[a do:x |]` versus `[a do:thing|]`), while the AST can:
/// the cursor is a selector position exactly when it is inside the send but
/// outside its receiver and every argument. The checker's `expr_type` also
/// types a receiver that is itself an expression (`[[a make] |]`), which no
/// amount of text scanning could do.
#[derive(Debug, Clone)]
struct SendSite {
    /// The whole `[...]` expression.
    send: LspRange,
    /// The receiver expression.
    receiver: LspRange,
    /// The receiver's name when it is a plain variable or `self` (so a local's
    /// declared type can answer when the checker left the node untyped).
    receiver_name: Option<String>,
    /// The receiver's static type, as the checker recorded it.
    receiver_type: Option<String>,
    /// The selector being sent, for signature help.
    selector: String,
    /// Every argument expression: a cursor inside one of these is not completing
    /// the selector, and its index is the active parameter.
    args: Vec<LspRange>,
}

/// A free-function call in the edited document (`name(args)`), for signature
/// help. Function calls are not message sends, so they get their own record.
#[derive(Debug, Clone)]
struct CallSite {
    call: LspRange,
    name: String,
    args: Vec<LspRange>,
}

/// The declaration index of one analysis run.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    /// Top-level declarations in source order, members nested inside.
    pub roots: Vec<Symbol>,
    /// Lookup key → declarations, in source order. Several declarations of one
    /// name are normal: a method implemented in two classes, a class split
    /// across headers.
    by_name: HashMap<String, Vec<Symbol>>,
    /// Class name → its members and ancestor, from every file in the
    /// compilation (a local's type is usually declared in a header).
    classes: HashMap<String, ClassInfo>,
    /// Scopes of the document being edited.
    scopes: Vec<Scope>,
    /// Message sends of the document being edited, in source order.
    sites: Vec<SendSite>,
    /// Free-function calls of the document being edited.
    calls: Vec<CallSite>,
    /// Free functions by name: what signature help shows for `name(...)`.
    functions: HashMap<String, Signature>,
    /// Namespace names seen in the qualified names of the index
    /// (`Game`, `Game::Entities`), including every declared block.
    ///
    /// Jeti's elaborator flattens `@namespace` blocks — members are lifted to
    /// the top level under their fully-qualified name — so a namespace's own
    /// declaration lives beside the flat stream (`AstUnit::namespaces`) and is
    /// rebuilt as a symbol here. This set is what answers for a namespace that
    /// has no block to point at, and what filters a completion after `Game::`.
    namespaces: BTreeSet<String>,
    /// `@namespace` blocks of the compilation, by extent, for reading a bare
    /// name the way the elaborator reads it.
    blocks: Vec<NamespaceBlock>,
    /// The document this index was built for: the one file whose positions
    /// `resolve_at` is asked about (an imported header's block must not claim a
    /// position in it).
    document: String,
}

impl SymbolIndex {
    /// Walk an analysis' AST into an index.
    ///
    /// `text_of` supplies a file's content by path (open editor buffers first,
    /// disk second). It is only used to project each declaration's span into
    /// UTF-16 columns, so a declaration in a file whose text is unavailable is
    /// skipped rather than guessed at.
    pub fn build(
        ast: &AstUnit,
        source_map: &SourceMap,
        text_of: &dyn Fn(&str) -> Option<String>,
        document: &str,
    ) -> SymbolIndex {
        let mut cache: HashMap<String, Option<String>> = HashMap::new();
        let mut lookup = |path: &str| -> Option<String> {
            cache
                .entry(path.to_string())
                .or_insert_with(|| text_of(path))
                .clone()
        };
        let mut index = SymbolIndex::default();
        index.document = document.to_string();
        index.roots = index.visit(&ast.decls, None, source_map, document, &mut lookup);
        index.attach_namespaces(ast, source_map, &mut lookup);
        index
    }

    /// Put each `@namespace` block back where it was written.
    ///
    /// The elaborated stream is flat: a namespace's members are top-level
    /// declarations named `A::B::C`. The block itself is the one fact flattening
    /// keeps for later (`AstUnit::namespaces`), because a wrapper node in
    /// `decls` would change what every lowering pass walks. Here it becomes a
    /// symbol of its own — which is what lands go-to-definition on the
    /// `@namespace` line — and adopts the declarations written inside it, which
    /// is what nests the outline and fills `containerName` (breadcrumbs).
    ///
    /// Blocks are adopted innermost first, so an inner namespace is already in
    /// the tree when the block containing it looks for its members.
    fn attach_namespaces(
        &mut self,
        ast: &AstUnit,
        source_map: &SourceMap,
        text_of: &mut impl FnMut(&str) -> Option<String>,
    ) {
        let mut blocks = Vec::new();
        for namespace in &ast.namespaces {
            let span = source_map.span_from_node(
                (namespace.line as u32, namespace.col as u32),
                (namespace.end_line as u32, namespace.end_col as u32),
            );
            if span.is_synthetic() {
                continue;
            }
            let path = source_map.registry().path_of(span.file).to_string();
            let Some(text) = text_of(&path) else { continue };
            let symbol = Symbol {
                name: namespace.name.clone(),
                lookup: namespace.name.clone(),
                kind: kind::NAMESPACE,
                // Same as an `AstDeclData::Namespace` node: the kind says it.
                detail: String::new(),
                container: None,
                path,
                range: span.to_lsp_range(&text),
                children: Vec::new(),
            };
            self.record(&symbol);
            self.namespaces.insert(symbol.lookup.clone());
            self.blocks.push(NamespaceBlock {
                name: symbol.lookup.clone(),
                path: symbol.path.clone(),
                range: symbol.range,
            });
            blocks.push(symbol);
        }
        for block in blocks.into_iter().rev() {
            self.roots = adopt_namespace(std::mem::take(&mut self.roots), block);
        }
    }

    /// The declarations of the document at `path`, ready to serialize as an LSP
    /// `DocumentSymbol[]`.
    pub fn document_symbols(&self, path: &str) -> Vec<Value> {
        self.roots
            .iter()
            .filter(|symbol| same_file(&symbol.path, path))
            .map(Symbol::to_json)
            .collect()
    }

    /// Every indexed declaration of `name` — a class, function, variable, or a
    /// whole selector such as `setObject:atIndex:`.
    pub fn definitions(&self, name: &str) -> Vec<Location> {
        let mut found: Vec<Location> = self
            .by_name
            .get(name)
            .map(|symbols| symbols.iter().map(Symbol::to_location).collect())
            .unwrap_or_default();
        found.sort_by(|a, b| (&a.path, a.range.start).cmp(&(&b.path, b.range.start)));
        found.dedup_by(|a, b| a.path == b.path && a.range.start == b.range.start);
        found
    }

    /// What the name at `position` names: the qualified name the cursor sits in
    /// is resolved segment by segment, the way clangd resolves a C++ one.
    ///
    /// `Game::Entities::Player` is three names, not one: `Game` and
    /// `Game::Entities` are namespaces, `Player` is the class. So the segment
    /// under the cursor decides — the qualified name *ending at that segment*
    /// is asked first, then the whole chain. Resolving a qualified name by its
    /// last segment is what made two `Player`s in different namespaces
    /// indistinguishable: every reference to either one answered with all of
    /// them.
    pub fn resolve_at(&self, text: &str, position: LspPosition) -> Option<Resolved> {
        for candidate in lookup_names(text, position) {
            // A declaration beats a namespace of the same name: it is the more
            // specific answer.
            if self.by_name.contains_key(&candidate) {
                return Some(Resolved::Symbol(self.narrow(&candidate, position)));
            }
            if self.namespaces.contains(&candidate) {
                return Some(Resolved::Namespace(candidate));
            }
        }
        None
    }

    /// The key a bare name at `position` stands for.
    ///
    /// `Player` written in three namespaces is one short name claiming three
    /// declarations, and answering with all three is a guess: inside
    /// `@namespace A` the elaborator reads it as `A::Player` (it prefixes a name
    /// written in a block with the block's fully-qualified name, walking outward
    /// when the inner name does not exist), so the block the cursor sits in
    /// decides. A name only one declaration claims, a name already written with
    /// a qualifier, and a name with no block around it all stay exactly as they
    /// were — the short form is indexed for them.
    fn narrow(&self, name: &str, position: LspPosition) -> String {
        if name.contains("::") {
            return name.to_string();
        }
        // Ambiguity means a *qualified* declaration shares this short name. A
        // name claimed only by bare declarations (the same class in two headers,
        // a selector implemented in two classes) has nothing to choose between.
        let ambiguous = self
            .by_name
            .get(name)
            .is_some_and(|symbols| symbols.iter().any(|symbol| symbol.lookup != name));
        if !ambiguous {
            return name.to_string();
        }
        for namespace in self.enclosing_namespaces(position) {
            let scoped = format!("{namespace}::{name}");
            if self.by_name.contains_key(&scoped) {
                return scoped;
            }
        }
        name.to_string()
    }

    /// The `@namespace` blocks containing `position`, innermost first.
    ///
    /// Blocks are per file: an imported header's block covers its own lines, so
    /// only the document's blocks can say anything about a position in it.
    fn enclosing_namespaces(&self, position: LspPosition) -> Vec<&str> {
        let mut blocks: Vec<&NamespaceBlock> = self
            .blocks
            .iter()
            .filter(|block| {
                same_file(&block.path, &self.document)
                    && block.range.start <= position
                    && position <= block.range.end
            })
            .collect();
        // Innermost first: of two blocks holding the cursor, the one that starts
        // later is written inside the other, so its name is the longer prefix.
        blocks.sort_by(|a, b| {
            b.range
                .start
                .cmp(&a.range.start)
                .then(a.range.end.cmp(&b.range.end))
        });
        blocks.into_iter().map(|block| block.name.as_str()).collect()
    }

    /// Declarations of the name at `position` — the editor's
    /// `textDocument/definition` answer: a declared namespace block answers
    /// with the block, a namespace the index only knows by name with nothing,
    /// an unknown name with nothing.
    pub fn definitions_at(&self, text: &str, position: LspPosition) -> Vec<Location> {
        match self.resolve_at(text, position) {
            Some(Resolved::Symbol(name)) => self.definitions(&name),
            _ => Vec::new(),
        }
    }

    /// Completion candidates whose name starts with `prefix` (case-sensitive, as
    /// the language is). An empty prefix offers everything the index knows.
    ///
    /// Names are deduplicated: a class declared in a header and implemented in
    /// this file is one entry, not two. Ordering is alphabetical so the editor's
    /// own ranking (and fuzzy matching) starts from a stable list.
    pub fn complete(&self, prefix: &str) -> Vec<Completion> {
        let mut seen: Vec<(&str, u8)> = Vec::new();
        let mut completions: Vec<Completion> = Vec::new();
        for symbol in self.by_name.values().flatten() {
            if !symbol.lookup.starts_with(prefix) {
                continue;
            }
            let key = (symbol.lookup.as_str(), symbol.kind);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            completions.push(Completion {
                label: symbol.lookup.clone(),
                kind: completion_kind(symbol.kind),
                detail: symbol.detail.clone(),
            });
            if completions.len() >= COMPLETION_LIMIT {
                break;
            }
        }
        // Namespaces are names too: after `Game::` the answers are the names
        // inside it, and a namespace is one of them (`Game::Entities` before
        // the class it holds).
        for namespace in self.namespaces.iter().filter(|name| name.starts_with(prefix)) {
            if completions.len() >= COMPLETION_LIMIT {
                break;
            }
            if completions.iter().any(|completion| completion.label == *namespace) {
                continue;
            }
            completions.push(Completion {
                label: namespace.clone(),
                kind: completion_kind(kind::NAMESPACE),
                detail: "namespace".to_string(),
            });
        }
        completions.sort_by(|a, b| a.label.cmp(&b.label));
        completions
    }

    // -- building ------------------------------------------------------------

    fn visit<'a>(
        &mut self,
        decls: impl IntoIterator<Item = &'a AstDecl>,
        container: Option<&str>,
        source_map: &SourceMap,
        document: &str,
        text_of: &mut impl FnMut(&str) -> Option<String>,
    ) -> Vec<Symbol> {
        let mut out = Vec::new();
        for decl in decls {
            // Member tables and scopes are collected on the way down: both need
            // the declaration plus its enclosing container, which is exactly
            // what this loop has.
            if let (Some(name), AstDeclData::Class { .. }) = (&decl.name, &decl.data) {
                self.register_class(name, &decl.data);
            }
            if let (Some(name), AstDeclData::Function { return_type, params, has_variadic, .. }) =
                (&decl.name, &decl.data)
            {
                self.functions.entry(name.clone()).or_insert_with(|| {
                    function_signature(
                        name,
                        return_type.as_deref(),
                        params.as_deref(),
                        *has_variadic,
                    )
                });
            }
            if let Some(scope) = self.document_scope(decl, container, source_map, document, text_of) {
                self.scopes.push(scope);
            }
            // A declaration this index cannot name itself (a namespace the
            // elaborator left anonymous, a forward declaration) still has
            // children worth indexing: descend either way, and let a nameless
            // container's contents stand in its place rather than vanish.
            let symbol = self.symbol_for(decl, container, source_map, text_of);
            let inner_container = symbol
                .as_ref()
                .map(|symbol| symbol.lookup.clone())
                .or_else(|| container.map(str::to_string));
            let children = self.visit(
                nested_decls(decl),
                inner_container.as_deref(),
                source_map,
                document,
                text_of,
            );
            match symbol {
                Some(mut symbol) => {
                    symbol.children = children;
                    self.record(&symbol);
                    out.push(symbol);
                }
                None => out.extend(children),
            }
        }
        out
    }

    /// Record a class's members and ancestor, from any file (a local's type is
    /// usually declared in a header). Declaration and implementation merge into
    /// one entry; the first non-empty ancestor wins, since an `@implementation`
    /// records only the implicit root.
    fn register_class(&mut self, name: &str, data: &AstDeclData) {
        let AstDeclData::Class { super_name, methods, properties, ivars, impl_vars, .. } = data else {
            return;
        };
        let entry = self.classes.entry(name.to_string()).or_default();
        if entry.super_name.is_none() {
            entry.super_name = super_name.clone();
        }
        let members = methods
            .iter()
            .chain(properties.iter())
            .chain(ivars.iter())
            .chain(impl_vars.iter())
            .filter_map(class_member);
        for member in members {
            if !entry.members.iter().any(|existing| existing.label == member.label) {
                entry.members.push(member);
            }
        }
    }

    /// The scope a declaration opens in the document being edited (`None` for
    /// declarations elsewhere, and for kinds that open none).
    fn document_scope(
        &mut self,
        decl: &AstDecl,
        container: Option<&str>,
        source_map: &SourceMap,
        document: &str,
        text_of: &mut impl FnMut(&str) -> Option<String>,
    ) -> Option<Scope> {
        let (params, body, self_type) = match &decl.data {
            AstDeclData::Method { params, body, .. } => {
                (params.as_deref(), body.as_deref(), container)
            }
            AstDeclData::Function { params, body, .. } => (params.as_deref(), body.as_deref(), None),
            AstDeclData::Variable { .. } => {
                // A file-scope variable is visible everywhere in the document.
                let path = decl_path(decl, source_map)?;
                if !same_file(&path, document) || container.is_some() {
                    return None;
                }
                let name = decl.name.clone()?;
                let type_name = match &decl.data {
                    AstDeclData::Variable { var_type, .. } => var_type.as_deref().and_then(ast_type_name),
                    _ => None,
                }?;
                return Some(Scope {
                    range: None,
                    owner: "<file>".to_string(),
                    self_type: None,
                    locals: vec![(name, type_name)],
                });
            }
            _ => return None,
        };

        let path = decl_path(decl, source_map)?;
        if !same_file(&path, document) {
            return None;
        }
        let span = source_map.span_from_node(
            (decl.line as u32, decl.col as u32),
            (decl.end_line as u32, decl.end_col as u32),
        );
        let text = text_of(&path)?;
        let mut scope = Scope {
            range: Some(span.to_lsp_range(&text)),
            owner: decl.name.clone().unwrap_or_default(),
            self_type: self_type.map(str::to_string),
            locals: Vec::new(),
        };

        // Parameters.
        let mut param = params;
        while let Some(current) = param {
            if let (Some(name), Some(ty)) = (&current.name, current.par_type.as_deref()) {
                if let Some(class) = cst_type_name(ty) {
                    scope.locals.push((name.clone(), class));
                }
            }
            param = current.next.as_deref();
        }
        // The enclosing class's ivars are visible by name inside its methods.
        if let Some(class) = self_type {
            if let Some(info) = self.classes.get(class) {
                for member in &info.members {
                    if let (Some(class_name), false) = (&member.type_name, member.is_class_method) {
                        scope.locals.push((member.label.clone(), class_name.clone()));
                    }
                }
            }
        }
        // Locals declared in the body, and the sends they contain.
        if let Some(body) = body {
            let mut walk = BodyWalk {
                locals: &mut scope.locals,
                sites: &mut self.sites,
                calls: &mut self.calls,
                source_map,
                text: &text,
            };
            walk.stmt(body);
        }
        Some(scope)
    }

    /// The class whose members answer a completion at `position`, when the
    /// cursor sits in the *selector* position of a message send.
    ///
    /// `None` when the cursor is on the receiver or on an argument (those are
    /// not selector positions), when the send is unknown, or when the receiver's
    /// type cannot be determined (an `id`, a scalar, an expression the checker
    /// did not type) — completion then offers the global names instead of
    /// guessing.
    pub fn send_receiver(&self, position: LspPosition) -> Option<(String, bool)> {
        // Innermost send wins: it starts last of the sends containing the cursor.
        let site = self
            .sites
            .iter()
            .filter(|site| range_contains(site.send, position))
            .max_by_key(|site| (site.send.start.line, site.send.start.character))?;
        // The cursor sitting on the receiver is not a selector position. A
        // class-name receiver's node is rewritten over the whole send by the
        // elaborator (its span carries no receiver-only information), so that
        // check only applies when the recorded receiver is narrower than the
        // send.
        let receiver_is_precise = site.receiver != site.send;
        if (receiver_is_precise && range_contains(site.receiver, position))
            || site.args.iter().any(|arg| range_contains(*arg, position))
        {
            return None;
        }
        // A named receiver resolves through the scopes first: a local knows its
        // declared type, and a class receiver calls class methods rather than
        // instance ones.
        if let Some(name) = &site.receiver_name {
            if let Some(resolved) = self.receiver_class(name, position) {
                return Some(resolved);
            }
        }
        site.receiver_type.clone().map(|class| (class, false))
    }

    /// The signature the cursor is inside a call of, with the parameter it is
    /// filling in.
    ///
    /// Message sends answer from the receiver's class (the same resolution
    /// member completion uses), free calls from the function table. `None` when
    /// the cursor is not inside a call this index can type — the editor then
    /// shows nothing rather than a wrong signature.
    pub fn signature_help(&self, position: LspPosition) -> Option<SignatureHelp> {
        // A send: the innermost one containing the cursor.
        if let Some(site) = self
            .sites
            .iter()
            .filter(|site| range_contains(site.send, position))
            .max_by_key(|site| (site.send.start.line, site.send.start.character))
        {
            let receiver = self.receiver_for_site(site, position)?;
            let signature = self.method_signature_in(&receiver.0, &site.selector)?;
            // Several declarations of one selector can differ (a category
            // overriding it); the first is the one to show, and every candidate
            // is reported so the editor can offer the rest.
            let mut signatures = vec![signature];
            signatures.extend(
                self.method_signatures_in(&receiver.0, &site.selector)
                    .into_iter()
                    .skip(1),
            );
            return Some(SignatureHelp {
                active_parameter: active_parameter(&site.args, position, signatures[0].parameters.len()),
                active_signature: 0,
                signatures,
            });
        }
        // A free call: the innermost one containing the cursor.
        let site = self
            .calls
            .iter()
            .filter(|site| range_contains(site.call, position))
            .max_by_key(|site| (site.call.start.line, site.call.start.character))?;
        let signature = self.functions.get(&site.name)?.clone();
        Some(SignatureHelp {
            active_parameter: active_parameter(&site.args, position, signature.parameters.len()),
            active_signature: 0,
            signatures: vec![signature],
        })
    }

    /// The receiver class of a send site, without the "is the cursor in an
    /// argument" rule: signature help *wants* the argument positions.
    fn receiver_for_site(&self, site: &SendSite, position: LspPosition) -> Option<(String, bool)> {
        if let Some(name) = &site.receiver_name {
            if let Some(resolved) = self.receiver_class(name, position) {
                return Some(resolved);
            }
        }
        site.receiver_type.clone().map(|class| (class, false))
    }

    /// Every signature declared for `selector` in `class` or its ancestors.
    fn method_signatures_in(&self, class: &str, selector: &str) -> Vec<Signature> {
        let mut signatures: Vec<Signature> = Vec::new();
        let mut current = Some(class.to_string());
        let mut depth = 0usize;
        while let Some(name) = current {
            depth += 1;
            if depth > 32 {
                break;
            }
            let Some(info) = self.classes.get(&name) else { break };
            for member in &info.members {
                if member.label == selector {
                    if let Some(signature) = &member.signature {
                        if !signatures.iter().any(|existing| existing.label == signature.label) {
                            signatures.push(signature.clone());
                        }
                    }
                }
            }
            current = info.super_name.clone();
        }
        signatures
    }

    fn method_signature_in(&self, class: &str, selector: &str) -> Option<Signature> {
        self.method_signatures_in(class, selector).into_iter().next()
    }

    // -- member completion ---------------------------------------------------

    /// The scope whose extent contains `position`, smallest first, falling back
    /// to the file scope (locals of the enclosing method, then globals).
    pub fn scope_at(&self, position: LspPosition) -> Option<&Scope> {
        let containing = self
            .scopes
            .iter()
            .filter(|scope| {
                scope
                    .range
                    .is_some_and(|range| range_contains(range, position))
            })
            .min_by_key(|scope| scope.range.map(|range| range.end.line - range.start.line));
        containing.or_else(|| self.scopes.iter().find(|scope| scope.range.is_none()))
    }

    /// What a receiver name means at `position`: the class to complete members
    /// of, and whether the receiver is a class (so only class methods answer).
    ///
    /// `self` resolves through the enclosing method; a name resolves through the
    /// scopes or, failing that, through the classes themselves.
    pub fn receiver_class(&self, name: &str, position: LspPosition) -> Option<(String, bool)> {
        let scope = self.scope_at(position);
        if name == "self" {
            return scope.and_then(|scope| scope.self_type.clone()).map(|class| (class, false));
        }
        if !is_identifier(name) {
            return None;
        }
        if let Some(scope) = scope {
            // Later declarations win (block-level shadowing flattened).
            if let Some((_, class)) = scope.locals.iter().rev().find(|(local, _)| local == name) {
                return Some((class.clone(), false));
            }
        }
        self.classes.contains_key(name).then(|| (name.to_string(), true))
    }

    /// Members of `class`, walking up the ancestor chain, filtered to what a
    /// receiver of that kind can call (class methods for a class receiver,
    /// instance members for an object) and by `prefix`.
    pub fn member_completion(&self, class: &str, class_receiver: bool, prefix: &str) -> Vec<Completion> {
        let mut members: Vec<MemberEntry> = Vec::new();
        let mut current = Some(class.to_string());
        let mut depth = 0usize;
        while let Some(name) = current {
            // A malformed hierarchy must not loop forever.
            depth += 1;
            if depth > 32 {
                break;
            }
            let Some(info) = self.classes.get(&name) else { break };
            for member in &info.members {
                if member.is_class_method == class_receiver
                    && member.label.starts_with(prefix)
                    && !members.iter().any(|existing| existing.label == member.label)
                {
                    members.push(member.clone());
                }
            }
            current = info.super_name.clone();
        }
        members.sort_by(|a, b| a.label.cmp(&b.label));
        members
            .into_iter()
            .map(|member| Completion {
                label: member.label,
                kind: completion_kind(member.kind),
                detail: member.detail,
            })
            .collect()
    }

    // -- building ------------------------------------------------------------

    /// Index `symbol` under its lookup name, plus its short (unqualified) form
    /// so a reference written without the namespace prefix still resolves.
    ///
    /// Every namespace prefix on the way is remembered as a namespace name
    /// (`Game::Entities::Player` says `Game::Entities` and `Game` exist), since
    /// the flattened AST has no other trace of them.
    fn record(&mut self, symbol: &Symbol) {
        let mut names = vec![symbol.lookup.clone()];
        if let Some(short) = symbol.lookup.rsplit("::").next() {
            if short != symbol.lookup {
                names.push(short.to_string());
            }
        }
        for name in names {
            self.by_name.entry(name).or_default().push(symbol.clone());
        }
        let mut prefix = symbol.lookup.as_str();
        while let Some((outer, _)) = prefix.rsplit_once("::") {
            self.namespaces.insert(outer.to_string());
            prefix = outer;
        }
    }

    /// Turn one AST declaration into a [`Symbol`]; `None` when this index has
    /// nothing to say about it (forward declarations, unhandled kinds, or a
    /// declaration with no resolvable source position).
    fn symbol_for(
        &self,
        decl: &AstDecl,
        container: Option<&str>,
        source_map: &SourceMap,
        text_of: &mut impl FnMut(&str) -> Option<String>,
    ) -> Option<Symbol> {
        let lookup = decl.name.clone()?;
        let (kind, detail) = describe(decl)?;

        let span = source_map.span_from_node(
            (decl.line as u32, decl.col as u32),
            (decl.end_line as u32, decl.end_col as u32),
        );
        if span.is_synthetic() {
            return None;
        }
        let path = source_map.registry().path_of(span.file).to_string();
        let text = text_of(&path)?;
        let range = span.to_lsp_range(&text);

        // Display name: the container already spells the namespace, so strip the
        // prefix rather than repeating `NS::` in every line of the outline.
        let name = container
            .and_then(|container| lookup.strip_prefix(&format!("{container}::")))
            .unwrap_or(&lookup)
            .to_string();

        Some(Symbol {
            name,
            lookup,
            kind,
            detail,
            container: container.map(str::to_string),
            path,
            range,
            children: Vec::new(),
        })
    }
}

impl Symbol {
    fn to_location(&self) -> Location {
        Location {
            name: self.lookup.clone(),
            kind: self.kind,
            detail: self.detail.clone(),
            container: self.container.clone(),
            path: self.path.clone(),
            range: self.range,
        }
    }

    fn to_json(&self) -> Value {
        let mut value = json!({
            "name": self.name,
            "kind": self.kind,
            "range": range_json(self.range),
            // The whole declaration is also the "selected" part: Jeti's AST
            // carries one extent per node, and pointing `selectionRange` at a
            // sub-token we did not track would be an invented position.
            "selectionRange": range_json(self.range),
        });
        if !self.detail.is_empty() {
            value["detail"] = json!(self.detail);
        }
        if let Some(container) = &self.container {
            value["containerName"] = json!(container);
        }
        if !self.children.is_empty() {
            value["children"] = Value::Array(self.children.iter().map(Symbol::to_json).collect());
        }
        value
    }
}

/// Kind + detail for a declaration, or `None` for kinds this index skips.
fn describe(decl: &AstDecl) -> Option<(u8, String)> {
    Some(match &decl.data {
        AstDeclData::Class { is_implementation, is_category, super_name, .. } => {
            let mut detail = match super_name {
                Some(super_name) => format!(":{super_name}"),
                None => String::new(),
            };
            if *is_category {
                detail = format!("category{detail}");
            } else if *is_implementation {
                detail = format!("@implementation{detail}");
            } else if !detail.is_empty() {
                detail = format!("@interface{detail}");
            }
            (kind::CLASS, detail)
        }
        AstDeclData::Protocol { parents, .. } => {
            let detail = if parents.is_empty() {
                String::new()
            } else {
                format!("<{}>", parents.join(", "))
            };
            (kind::INTERFACE, detail)
        }
        AstDeclData::Method { is_class_method, .. } => (
            kind::METHOD,
            format!("{} {}", if *is_class_method { "+" } else { "-" }, decl.name.clone().unwrap_or_default()),
        ),
        AstDeclData::Property { .. } => (kind::PROPERTY, String::new()),
        AstDeclData::Ivar { .. } => (kind::FIELD, String::new()),
        AstDeclData::Function { .. } => (kind::FUNCTION, decl.name.clone().unwrap_or_default()),
        AstDeclData::Variable { .. } => (kind::VARIABLE, String::new()),
        AstDeclData::Typedef { .. } => (kind::CLASS, "typedef".to_string()),
        AstDeclData::Aggregate { is_union, .. } => (
            kind::STRUCT,
            if *is_union { "union".to_string() } else { "struct".to_string() },
        ),
        AstDeclData::Enum { .. } => (kind::ENUM, String::new()),
        AstDeclData::Namespace(_) => (kind::NAMESPACE, String::new()),
        // Forward declarations are not definitions, and the remaining kinds
        // carry no name a user could click through.
        _ => return None,
    })
}

/// Move every declaration a `@namespace` block contains under it, and put the
/// block back where its members were (the list stays in source order).
///
/// A block's extent covers exactly the declarations written inside it, so
/// containment — not the `::` in the names — is what decides who is a member.
/// An adopted symbol's display name loses the prefix its container now spells,
/// the way a class member's does.
fn adopt_namespace(roots: Vec<Symbol>, mut block: Symbol) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::with_capacity(roots.len() + 1);
    let mut at: Option<usize> = None;
    for symbol in roots {
        if !encloses(&block, &symbol) {
            out.push(symbol);
            continue;
        }
        at.get_or_insert(out.len());
        let mut child = symbol;
        child.container = Some(block.lookup.clone());
        if let Some(short) = child.lookup.strip_prefix(&format!("{}::", block.lookup)) {
            child.name = short.to_string();
        }
        block.children.push(child);
    }
    match at {
        Some(index) => out.insert(index, block),
        None => {
            // Nothing of this file is inside it (an empty block, or members
            // declared elsewhere): place it where its extent says it belongs,
            // ahead of the first declaration that starts after it.
            let index = out
                .iter()
                .position(|symbol| {
                    same_file(&symbol.path, &block.path) && block.range.start <= symbol.range.start
                })
                .unwrap_or(out.len());
            out.insert(index, block);
        }
    }
    out
}

/// Whether `namespace`'s extent contains `symbol`: same file, and strictly
/// wider than the symbol it holds.
fn encloses(namespace: &Symbol, symbol: &Symbol) -> bool {
    same_file(&namespace.path, &symbol.path)
        && namespace.range != symbol.range
        && namespace.range.start <= symbol.range.start
        && symbol.range.end <= namespace.range.end
}

/// The declarations nested inside `decl` (a class body, a namespace).
fn nested_decls<'a>(decl: &'a AstDecl) -> Vec<&'a AstDecl> {
    match &decl.data {
        AstDeclData::Class { methods, ivars, properties, impl_vars, .. } => methods
            .iter()
            .chain(properties.iter())
            .chain(ivars.iter())
            .chain(impl_vars.iter())
            .collect(),
        AstDeclData::Namespace(decls) => decls.iter().collect(),
        _ => Vec::new(),
    }
}

/// The file a declaration lives in, or `None` when it has no source origin.
fn decl_path(decl: &AstDecl, source_map: &SourceMap) -> Option<String> {
    let span = source_map.span_from_node(
        (decl.line as u32, decl.col as u32),
        (decl.end_line as u32, decl.end_col as u32),
    );
    (!span.is_synthetic()).then(|| source_map.registry().path_of(span.file).to_string())
}

/// The class name a type denotes, when it denotes one. Scalars, `void`, `id`
/// and `instancetype` have no members to complete (protocol members would need
/// the protocol side of the index, which is not there yet).
fn ast_type_name(ty: &AstType) -> Option<String> {
    match ty.prim {
        TypePrim::Named => ty.name.clone().or_else(|| ty.class_ref.clone()),
        TypePrim::Class => ty.class_ref.clone().or_else(|| ty.name.clone()),
        TypePrim::Void
        | TypePrim::Char
        | TypePrim::Short
        | TypePrim::Int
        | TypePrim::Long
        | TypePrim::LongLong
        | TypePrim::Float
        | TypePrim::Double
        | TypePrim::Bool
        | TypePrim::Signed
        | TypePrim::Unsigned
        | TypePrim::Id
        | TypePrim::Instancetype
        | TypePrim::Param
        | TypePrim::Sel => None,
    }
}

/// [`ast_type_name`] for a parameter, which keeps its **CST** type node
/// (`AstDeclData::Method` stores `CstParam`s verbatim).
fn cst_type_name(ty: &CstType) -> Option<String> {
    match ty.prim {
        TypePrim::Named | TypePrim::Class => ty.name.clone(),
        TypePrim::Void
        | TypePrim::Char
        | TypePrim::Short
        | TypePrim::Int
        | TypePrim::Long
        | TypePrim::LongLong
        | TypePrim::Float
        | TypePrim::Double
        | TypePrim::Bool
        | TypePrim::Signed
        | TypePrim::Unsigned
        | TypePrim::Id
        | TypePrim::Instancetype
        | TypePrim::Param
        | TypePrim::Sel => None,
    }
}

/// One class member (method, property, ivar) as member completion needs it.
fn class_member(decl: &AstDecl) -> Option<MemberEntry> {
    let label = decl.name.clone()?;
    let (kind, detail, is_class_method, type_name) = match &decl.data {
        AstDeclData::Method { is_class_method, .. } => (
            kind::METHOD,
            format!("{} {label}", if *is_class_method { "+" } else { "-" }),
            *is_class_method,
            None,
        ),
        AstDeclData::Property { prop_type, .. } => (
            kind::PROPERTY,
            String::new(),
            false,
            prop_type.as_deref().and_then(ast_type_name),
        ),
        AstDeclData::Ivar { ivar_type, .. } => (
            kind::FIELD,
            String::new(),
            false,
            ivar_type.as_deref().and_then(ast_type_name),
        ),
        AstDeclData::Variable { var_type, .. } => (
            kind::FIELD,
            String::new(),
            false,
            var_type.as_deref().and_then(ast_type_name),
        ),
        _ => return None,
    };
    let signature = match (&decl.data, &label) {
        (AstDeclData::Method { is_class_method, return_type, params, has_variadic, .. }, name) => {
            Some(method_signature(
                *is_class_method,
                name,
                return_type.as_deref(),
                params.as_deref(),
                *has_variadic,
            ))
        }
        _ => None,
    };
    Some(MemberEntry { label, kind, detail, is_class_method, type_name, signature })
}

/// The signature of a method declaration: `- (void)greet:(NPString *)name`.
///
/// The label is synthesized here (the AST keeps a method's parameters as CST
/// nodes with no source extent, so the declaration's text cannot be sliced apart
/// into parameter labels). Parameter labels are substrings of the label, which
/// is what LSP matches on.
fn method_signature(
    is_class_method: bool,
    name: &str,
    return_type: Option<&AstType>,
    params: Option<&CstParam>,
    has_variadic: bool,
) -> Signature {
    let mut label = format!(
        "{} ({})",
        if is_class_method { "+" } else { "-" },
        return_type.map(render_ast_type).unwrap_or_else(|| "id".to_string())
    );
    let mut parameters: Vec<String> = Vec::new();
    let mut param = params;
    while let Some(current) = param {
        let part = current.external_name.clone().unwrap_or_default();
        let label_text = param_label(current);
        // Keyword parts are space-separated in the source spelling,
        // `greet:(NPString *)name from:(NPString *)where`.
        label.push_str(&format!("{part}:{label_text}"));
        parameters.push(label_text);
        param = current.next.as_deref();
        if param.is_some() {
            label.push(' ');
        }
    }
    // A colon-less method carries its selector in the declaration's name.
    if parameters.is_empty() {
        label.push_str(name);
    }
    if has_variadic {
        label.push_str(", ...");
    }
    Signature { label, parameters }
}

/// `(NPString *)name` — the parameter as it reads in a signature.
fn param_label(param: &CstParam) -> String {
    let ty = param
        .par_type
        .as_deref()
        .map(render_cst_type)
        .unwrap_or_else(|| "id".to_string());
    match &param.name {
        Some(name) => format!("({ty}){name}"),
        None => format!("({ty})"),
    }
}

/// The signature of a free function declaration: `int helper(int value)`.
fn function_signature(
    name: &str,
    return_type: Option<&AstType>,
    params: Option<&CstParam>,
    has_variadic: bool,
) -> Signature {
    let mut parameters: Vec<String> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut param = params;
    while let Some(current) = param {
        let ty = current
            .par_type
            .as_deref()
            .map(render_cst_type)
            .unwrap_or_else(|| "int".to_string());
        let label_text = match &current.name {
            Some(name) => format!("{ty} {name}"),
            None => ty,
        };
        parts.push(label_text.clone());
        parameters.push(label_text);
        param = current.next.as_deref();
    }
    if has_variadic {
        parts.push("...".to_string());
    }
    let label = format!(
        "{} {name}({})",
        return_type.map(render_ast_type).unwrap_or_else(|| "void".to_string()),
        parts.join(", ")
    );
    Signature { label, parameters }
}

/// A type as it reads in a signature. Display-only text: the checker's printer
/// is private and takes only AST types, while a method's parameters keep their
/// CST type nodes, so both spellings are rendered here from the same rules.
fn render_ast_type(ty: &AstType) -> String {
    render_type(TypeParts {
        prim: ty.prim,
        name: ty.name.as_deref(),
        is_pointer: ty.is_pointer,
        is_struct: ty.is_struct,
        type_args: ty.type_args.iter().map(render_ast_type).collect(),
    })
}

fn render_cst_type(ty: &CstType) -> String {
    render_type(TypeParts {
        prim: ty.prim,
        name: ty.name.as_deref(),
        is_pointer: ty.is_pointer,
        is_struct: ty.is_struct,
        type_args: ty.type_args.iter().map(render_cst_type).collect(),
    })
}

struct TypeParts<'a> {
    prim: TypePrim,
    name: Option<&'a str>,
    is_pointer: bool,
    is_struct: bool,
    type_args: Vec<String>,
}

fn render_type(parts: TypeParts<'_>) -> String {
    let mut text = match parts.prim {
        TypePrim::Void => "void".to_string(),
        TypePrim::Char => "char".to_string(),
        TypePrim::Short => "short".to_string(),
        TypePrim::Int => "int".to_string(),
        TypePrim::Long => "long".to_string(),
        TypePrim::LongLong => "long long".to_string(),
        TypePrim::Float => "float".to_string(),
        TypePrim::Double => "double".to_string(),
        TypePrim::Bool => "BOOL".to_string(),
        TypePrim::Signed => "signed".to_string(),
        TypePrim::Unsigned => "unsigned".to_string(),
        TypePrim::Id => "id".to_string(),
        TypePrim::Class => "Class".to_string(),
        TypePrim::Sel => "SEL".to_string(),
        TypePrim::Instancetype => "instancetype".to_string(),
        TypePrim::Named | TypePrim::Param => {
            parts.name.unwrap_or("?").to_string()
        }
    };
    if parts.is_struct {
        text = format!("struct {text}");
    }
    if !parts.type_args.is_empty() {
        text.push('<');
        text.push_str(&parts.type_args.join(", "));
        text.push('>');
    }
    if parts.is_pointer {
        text.push_str(" *");
    }
    text
}

/// A walk over one method or function body that collects the two things member
/// completion needs from a body: the locals it declares, and the message sends
/// in it (see [`SendSite`]).
///
/// Nodes are resolved through [`SourceMap::span_from_node`] + `to_lsp_range`, so
/// the recorded extents are in the editor's own coordinates; a node the
/// compiler synthesized has no source origin and is skipped.
struct BodyWalk<'a> {
    locals: &'a mut Vec<(String, String)>,
    sites: &'a mut Vec<SendSite>,
    calls: &'a mut Vec<CallSite>,
    source_map: &'a SourceMap,
    text: &'a str,
}

impl BodyWalk<'_> {
    fn range(&self, line: usize, col: usize, end_line: usize, end_col: usize) -> Option<LspRange> {
        let span = self.source_map.span_from_node(
            (line as u32, col as u32),
            (end_line as u32, end_col as u32),
        );
        (!span.is_synthetic()).then(|| span.to_lsp_range(self.text))
    }

    fn expr_range(&self, expr: &AstExpr) -> Option<LspRange> {
        self.range(expr.line, expr.col, expr.end_line, expr.end_col)
    }

    /// Locals declared by one declaration (including a `T *a, *b;` chain) and by
    /// anything declared inside its initializer.
    fn decl(&mut self, decl: &AstDecl) {
        let mut current = Some(decl);
        while let Some(declaration) = current {
            let AstDeclData::Variable { var_type, init, next, .. } = &declaration.data else {
                return;
            };
            if let (Some(name), Some(ty)) = (&declaration.name, var_type.as_deref()) {
                if let Some(class) = ast_type_name(ty) {
                    self.locals.push((name.clone(), class));
                }
            }
            if let Some(init) = init {
                self.expr(init);
            }
            current = next.as_deref();
        }
    }

    fn param(&mut self, param: &CstParam) {
        // A parameter's `external_name` is a selector part; the body refers to
        // it by `name`.
        if let (Some(name), Some(ty)) = (&param.name, param.par_type.as_deref()) {
            if let Some(class) = cst_type_name(ty) {
                self.locals.push((name.clone(), class));
            }
        }
    }

    fn stmt(&mut self, stmt: &AstStmt) {
        match &stmt.data {
            AstStmtData::Compound(statements) => {
                for statement in statements {
                    self.stmt(statement);
                }
            }
            AstStmtData::Decl(decl) => self.decl(decl),
            AstStmtData::Expr(expr) => self.expr(expr),
            AstStmtData::If { cond, then, else_ } => {
                self.expr(cond);
                self.stmt(then);
                if let Some(otherwise) = else_ {
                    self.stmt(otherwise);
                }
            }
            AstStmtData::Switch { expr, body } => {
                self.expr(expr);
                self.stmt(body);
            }
            AstStmtData::SwitchPat { expr, arms, default_body, .. } => {
                self.expr(expr);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.stmt(&arm.body);
                }
                if let Some(body) = default_body {
                    self.stmt(body);
                }
            }
            AstStmtData::Case { value, body } => {
                self.expr(value);
                self.stmt(body);
            }
            AstStmtData::Default(body)
            | AstStmtData::Autoreleasepool(body)
            | AstStmtData::NoArc(body)
            | AstStmtData::Defer(body)
            | AstStmtData::Finally(body) => self.stmt(body),
            AstStmtData::While { cond, body } => {
                self.expr(cond);
                self.stmt(body);
            }
            AstStmtData::Do { body, cond } => {
                self.stmt(body);
                self.expr(cond);
            }
            AstStmtData::For { init, cond, incr, body } => {
                if let Some(init) = init {
                    self.stmt(init);
                }
                if let Some(cond) = cond {
                    self.expr(cond);
                }
                if let Some(incr) = incr {
                    self.expr(incr);
                }
                self.stmt(body);
            }
            AstStmtData::ForIn { var, collection, body } => {
                self.expr(var);
                self.expr(collection);
                self.stmt(body);
            }
            AstStmtData::Try { try_block, catches, finally_block } => {
                self.stmt(try_block);
                for catch in catches {
                    self.stmt(catch);
                }
                if let Some(finally) = finally_block {
                    self.stmt(finally);
                }
            }
            AstStmtData::Catch { param, body } => {
                self.param(param);
                self.stmt(body);
            }
            AstStmtData::Synchronized { lock, body } => {
                self.expr(lock);
                self.stmt(body);
            }
            AstStmtData::Return(Some(expr)) | AstStmtData::Throw(Some(expr)) => self.expr(expr),
            AstStmtData::Asm { outputs, inputs, .. } => {
                for operand in outputs.iter().chain(inputs.iter()) {
                    self.expr(&operand.expr);
                }
            }
            AstStmtData::Return(None)
            | AstStmtData::Throw(None)
            | AstStmtData::Goto(_)
            | AstStmtData::Label(_) => {}
        }
    }

    /// Walk an expression far enough to find the sends in it and any
    /// declarations inside its block literals. Every variant is listed, so a
    /// new one fails the build instead of silently narrowing what is seen.
    fn expr(&mut self, expr: &AstExpr) {
        match &expr.data {
            AstExprData::MsgSend { receiver, args, selector, .. } => {
                if let (Some(send), Some(receiver_range)) =
                    (self.expr_range(expr), self.expr_range(receiver))
                {
                    self.sites.push(SendSite {
                        send,
                        receiver: receiver_range,
                        receiver_name: match &receiver.data {
                            AstExprData::VarRef { name, .. } => Some(name.clone()),
                            _ => None,
                        },
                        receiver_type: receiver.expr_type.as_deref().and_then(ast_type_name),
                        selector: selector.clone(),
                        args: args.iter().filter_map(|arg| self.expr_range(arg)).collect(),
                    });
                }
                self.expr(receiver);
                for arg in args {
                    self.expr(arg);
                }
            }
            AstExprData::FuncCall { name, callee, args, .. } => {
                if let Some(range) = self.expr_range(expr) {
                    self.calls.push(CallSite {
                        call: range,
                        name: name.clone(),
                        args: args.iter().filter_map(|arg| self.expr_range(arg)).collect(),
                    });
                }
                if let Some(callee) = callee {
                    self.expr(callee);
                }
                for arg in args {
                    self.expr(arg);
                }
            }
            AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => self.expr(obj),
            AstExprData::Unary { operand, .. } => self.expr(operand),
            AstExprData::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            AstExprData::Assign { target, value } => {
                self.expr(target);
                self.expr(value);
            }
            AstExprData::Cast { expr, .. }
            | AstExprData::Boxed(expr)
            | AstExprData::Paren(expr)
            | AstExprData::Await { inner: expr } => self.expr(expr),
            AstExprData::ArrayLit(items)
            | AstExprData::InitList(items)
            | AstExprData::Comma(items) => {
                for item in items {
                    self.expr(item);
                }
            }
            AstExprData::DesignatedInit { expr, .. } => self.expr(expr),
            AstExprData::DictLit { keys, values } => {
                for entry in keys.iter().chain(values.iter()) {
                    self.expr(entry);
                }
            }
            AstExprData::Subscript { object, key } => {
                self.expr(object);
                self.expr(key);
            }
            AstExprData::Sizeof { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            }
            AstExprData::Ternary { cond, then, else_ } => {
                self.expr(cond);
                self.expr(then);
                self.expr(else_);
            }
            AstExprData::Block { params, body, .. } => {
                for (ty, name) in params {
                    if let Some(class) = ast_type_name(ty) {
                        self.locals.push((name.clone(), class));
                    }
                }
                if let Some(body) = body {
                    self.stmt(body);
                }
            }
            AstExprData::Int(_)
            | AstExprData::Float(_)
            | AstExprData::FloatRaw(_)
            | AstExprData::Char(_)
            | AstExprData::String(_)
            | AstExprData::AtString(_)
            | AstExprData::Bool(_)
            | AstExprData::VarRef { .. }
            | AstExprData::Selector(_)
            | AstExprData::TypeLiteral(_)
            | AstExprData::Alignof(_) => {}
        }
    }
}

fn range_contains(range: LspRange, position: LspPosition) -> bool {
    let point = (position.line, position.character);
    (range.start.line, range.start.character) <= point && point <= (range.end.line, range.end.character)
}

/// Which parameter the cursor is filling in: the argument it sits inside, or how
/// many arguments start before it (the one being typed), clamped to the
/// signature's parameter count.
fn active_parameter(args: &[LspRange], position: LspPosition, parameters: usize) -> usize {
    if let Some(index) = args.iter().position(|arg| range_contains(*arg, position)) {
        return index.min(parameters.saturating_sub(1));
    }
    let point = (position.line, position.character);
    let before = args
        .iter()
        .filter(|arg| (arg.start.line, arg.start.character) < point)
        .count();
    before.min(parameters.saturating_sub(1))
}

/// True for a plain identifier (so `self.x`, `[a b]` or `1` never reach the
/// scope/class lookups as if they were a name).
fn is_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_alphabetic() || first == b'_' || first == b'$' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

pub(crate) fn range_json(range: LspRange) -> Value {
    let pos = |p: LspPosition| json!({ "line": p.line, "character": p.character });
    json!({ "start": pos(range.start), "end": pos(range.end) })
}
// ---------------------------------------------------------------------------
// Cursor → symbol text
// ---------------------------------------------------------------------------

/// The symbol under an LSP position, written the way the index keys it:
///
/// - a **selector** when the cursor sits on one of its parts inside a message
///   send (`setObject:atIndex:` — every keyword part is one symbol),
/// - a keyword part of a *declaration* keeps its colon (`run:` in
///   `- (void)run:(Point)where`), or
/// - the **qualified name** the cursor sits in, when it is one
///   (`Game::Entities::Player`, whichever segment the cursor is on), or
/// - the plain identifier otherwise.
///
/// A position just past the end of a word counts as being on it, which is where
/// an editor leaves the cursor after you finish typing. `None` when there is no
/// word to speak of (whitespace, punctuation).
///
/// Selector detection reads the source text: the send is located by matching
/// brackets backwards from the cursor, then the identifier runs *directly*
/// inside it are classified — runs followed by `:` are keyword parts, the last
/// run of a colon-less send is the selector, and everything else in there is the
/// receiver or an argument, so the cursor gets that plain identifier instead.
/// This is deliberately local (no AST walk): the only thing it decides is which
/// *name* to look up, and every range that comes back still comes from the
/// shared span model. A mis-scan can therefore only cause a miss, never a jump
/// to an invented position.
pub fn symbol_at(text: &str, position: LspPosition) -> Option<String> {
    let bytes = text.as_bytes();
    let offset = jeti_cst::source_map::SourceFile::lsp_to_offset(text, position)?;
    if let Some(name) = standalone_name(bytes, text, offset) {
        return Some(name);
    }
    qualified_at(text, position)
        .filter(|name| name.on_name)
        .map(|name| name.text)
}

/// A word that is a name *by itself*, spelled the way the index keys it: a
/// message send's selector (`greet:from:` — several words, one name) or a
/// declared keyword part (`run:` in `- (void)run:(Point)where`).
///
/// `None` means the word is a piece of a bigger name — a plain identifier, or a
/// segment of a qualified one — and the `::` scan decides what to look up.
fn standalone_name(bytes: &[u8], text: &str, offset: usize) -> Option<String> {
    let (word_start, word_end) = word_bounds(bytes, offset)?;
    if let Some(selector) = selector_at(bytes, offset, word_start, word_end) {
        return Some(selector);
    }
    // A colon that opens a separator (`foo::`) is qualification, not a keyword.
    if bytes.get(word_end) == Some(&b':') && bytes.get(word_end + 1) != Some(&b':') {
        return Some(format!("{}:", &text[word_start..word_end]));
    }
    None
}

/// The name as written under the cursor, `::` qualification included.
///
/// The AST already speaks this way: the elaborator lifts every `@namespace`
/// member to the top level under its fully-qualified name, so the index keys
/// `Game::Entities::Player` and its segments are what tell the qualifiers apart.
/// `text` is that whole name; `segment_end` is where the segment the cursor is
/// on ends inside it, so `text[..segment_end]` is the qualified name *ending at
/// that segment* — `Game::Entities` when the cursor is on `Entities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    /// The whole name as written: `Game::Entities::Player`. When the cursor has
    /// just typed a separator (`Game::|`) this is what is written so far,
    /// `Game::`.
    pub text: String,
    /// Byte length of the part of `text` up to and including the segment the
    /// cursor is on; `text.len()` on the last segment or after a separator.
    pub segment_end: usize,
    /// Whether the cursor is inside an identifier. False when it has only typed
    /// a separator, which is not a name to answer for — only a prefix to
    /// complete.
    pub on_name: bool,
}

impl QualifiedName {
    /// The name spelled up to and including the cursor's own segment: what the
    /// segment literally names (`Game::Entities` on `Entities`).
    pub fn prefix(&self) -> &str {
        &self.text[..self.segment_end.min(self.text.len())]
    }
}

/// Scan the `::`-qualified name the cursor touches.
///
/// Spaces are allowed around a separator because the parser allows them
/// (`Game :: Player`); a newline ends the scan, since a chain is not written
/// across lines in practice. `None` when the cursor is on punctuation or
/// whitespace that no name reaches over.
fn qualified_at(text: &str, position: LspPosition) -> Option<QualifiedName> {
    let bytes = text.as_bytes();
    let offset = jeti_cst::source_map::SourceFile::lsp_to_offset(text, position)?;
    let (segment_start, segment_end, on_name) = match word_bounds(bytes, offset) {
        Some((word_start, word_end)) => (word_start, word_end, true),
        None => {
            // `Game::|` — a separator with nothing after it yet: still the name
            // `Game::`, so completion can offer what lives inside it.
            let mut separator = offset;
            while separator > 0 && is_blank(bytes[separator - 1]) {
                separator -= 1;
            }
            if separator < 2 || &bytes[separator - 2..separator] != b"::" {
                return None;
            }
            (separator, separator, false)
        }
    };
    let start = chain_start(bytes, segment_start);
    let end = chain_end(bytes, segment_end);
    if end <= start {
        return None;
    }
    // Blanks are only ever crossed at a separator (`Game :: Player`), and the
    // index keys the name without them — hence the compacted spelling.
    Some(QualifiedName {
        text: compact(&text[start..end]),
        segment_end: compact(&text[start..segment_end]).len(),
        on_name,
    })
}

/// Drop the blanks a name may be written with around its separators, so
/// `Game :: Entities :: Player` looks up as `Game::Entities::Player`.
fn compact(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, ' ' | '\t')).collect()
}

/// What has been typed of the name under the cursor, for filtering completion
/// candidates: `Game::En` while the segment is being written, and `Game::`
/// right after the separator, so a namespace completes to its contents instead
/// of offering every name in the index.
pub fn typed_name_at(text: &str, position: LspPosition) -> Option<String> {
    let name = qualified_at(text, position)?;
    Some(name.prefix().to_string())
}

/// Walk left across `name ::` pairs, so the scan starts at the outermost
/// segment. Returns `from` when there is no qualifier to cross.
fn chain_start(bytes: &[u8], from: usize) -> usize {
    let mut start = from;
    loop {
        let mut separator = start;
        while separator > 0 && is_blank(bytes[separator - 1]) {
            separator -= 1;
        }
        if separator < 2 || &bytes[separator - 2..separator] != b"::" {
            return start;
        }
        let mut name_end = separator - 2;
        while name_end > 0 && is_blank(bytes[name_end - 1]) {
            name_end -= 1;
        }
        let ident_end = name_end;
        while name_end > 0 && is_ident_byte(bytes[name_end - 1]) {
            name_end -= 1;
        }
        if name_end == ident_end {
            return start; // `::` with nothing before it
        }
        start = name_end;
    }
}

/// Walk right across `:: name` pairs. Returns `from` when the name does not
/// continue, which is also the case for a trailing separator (`Game::`).
fn chain_end(bytes: &[u8], from: usize) -> usize {
    let mut end = from;
    loop {
        let mut separator = end;
        while separator < bytes.len() && is_blank(bytes[separator]) {
            separator += 1;
        }
        if separator + 2 > bytes.len() || &bytes[separator..separator + 2] != b"::" {
            return end;
        }
        let mut name_start = separator + 2;
        while name_start < bytes.len() && is_blank(bytes[name_start]) {
            name_start += 1;
        }
        let ident_start = name_start;
        while name_start < bytes.len() && is_ident_byte(bytes[name_start]) {
            name_start += 1;
        }
        if name_start == ident_start {
            return end; // `::` with nothing after it
        }
        end = name_start;
    }
}

/// A space or a tab: whitespace a separator may be written across. A newline is
/// deliberately not one of them.
fn is_blank(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

/// The names the cursor's word could be, most specific first:
///
/// 1. the qualified name ending at the segment under the cursor
///    (`Game::Entities` on `Entities`),
/// 2. the whole chain the segment is part of (`Game::Entities::Player`).
///
/// Step 1 is what makes the qualifiers resolvable — on `Entities` the answer is
/// the namespace, not the class it holds. A name written with no qualifier is
/// simply itself, and the index holds the short form of every qualified name it
/// knows, so an unqualified reference still resolves.
///
/// A name that is whole by itself — a selector (`greet:from:`) or a declared
/// keyword part (`run:`) — has one candidate, itself: it is never taken apart.
fn lookup_names(text: &str, position: LspPosition) -> Vec<String> {
    let bytes = text.as_bytes();
    let Some(offset) = jeti_cst::source_map::SourceFile::lsp_to_offset(text, position) else {
        return Vec::new();
    };
    // A selector or a declared keyword part is one name however many words it
    // is written with, so it is never taken apart.
    if let Some(name) = standalone_name(bytes, text, offset) {
        return vec![name];
    }
    let Some(name) = qualified_at(text, position).filter(|name| name.on_name) else {
        return Vec::new();
    };
    let mut candidates = vec![name.prefix().to_string()];
    if name.text != name.prefix() {
        candidates.push(name.text.clone());
    }
    candidates
}


/// Bounds of the identifier the cursor is on, or `None` when there is none. A
/// cursor one past the end of a word anchors to that word.
fn word_bounds(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    if bytes.is_empty() || offset > bytes.len() {
        return None;
    }
    let mut anchor = offset;
    if anchor == bytes.len() || !is_ident_byte(bytes[anchor]) {
        if anchor == 0 || !is_ident_byte(bytes[anchor - 1]) {
            return None;
        }
        anchor -= 1;
    }
    let mut start = anchor;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = anchor + 1;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    Some((start, end))
}

/// The selector the cursor's word belongs to, when the cursor is inside a
/// message send *and* its word is part of that send's selector.
fn selector_at(
    bytes: &[u8],
    offset: usize,
    word_start: usize,
    word_end: usize,
) -> Option<String> {
    let open = opening_bracket_before(bytes, offset)?;
    let runs = send_identifier_runs(bytes, open);
    let keyword_parts: Vec<&(usize, usize, bool)> = runs.iter().filter(|run| run.2).collect();
    let in_word = |run: &&(usize, usize, bool)| run.0 <= word_start && word_end <= run.1;

    if !keyword_parts.is_empty() {
        if !keyword_parts.iter().any(in_word) {
            return None; // the cursor is on the receiver or an argument
        }
        let mut selector = String::new();
        for run in keyword_parts {
            selector.push_str(&String::from_utf8_lossy(&bytes[run.0..run.1]));
            selector.push(':');
        }
        return Some(selector);
    }

    // A colon-less send (`[str UTF8String]`): its selector is the last
    // identifier written directly inside the brackets, the receiver or a
    // subscript coming before it.
    let last = runs.last()?;
    in_word(&last).then(|| String::from_utf8_lossy(&bytes[last.0..last.1]).into_owned())
}

/// The `[` that opens the innermost bracket group containing `offset`.
fn opening_bracket_before(bytes: &[u8], offset: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = offset.min(bytes.len());
    // A bounded scan keeps the worst case linear; a send longer than this is
    // pathological, and falling back to the plain identifier is harmless.
    let floor = i.saturating_sub(4096);
    while i > floor {
        i -= 1;
        match bytes[i] {
            b']' => depth += 1,
            b'[' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The identifier runs written directly inside the bracket group that starts at
/// `open`, as `(start, end, followed_by_colon)`. Nested groups (a send inside an
/// argument, a subscript, an array literal) are skipped, and string and
/// character literals are stepped over so their contents cannot be mistaken for
/// code.
fn send_identifier_runs(bytes: &[u8], open: usize) -> Vec<(usize, usize, bool)> {
    let mut runs = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = open;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(open_quote) = quote {
            if byte == b'\\' {
                i += 2;
                continue;
            }
            if byte == open_quote {
                quote = None;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                i += 1;
            }
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return runs;
                }
                i += 1;
            }
            _ if depth == 1 && is_ident_start(byte) => {
                let start = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                let keyword = i < bytes.len() && bytes[i] == b':';
                runs.push((start, i, keyword));
            }
            _ => i += 1,
        }
    }
    runs
}

/// Identifier *continuation* byte (digits included, digits only start a number).
fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// The receiver name of a `.` / `->` access the cursor sits right after, while
/// the member name is still being typed (`obj.` and `obj->`).
///
/// This is the one member position the AST cannot answer: the property/ivar
/// node does not exist until its name has been written. Message sends go through
/// [`SymbolIndex::send_receiver`] instead, which is exact.
pub fn access_receiver_at(text: &str, position: LspPosition) -> Option<String> {
    let offset = jeti_cst::source_map::SourceFile::lsp_to_offset(text, position)?;
    let bytes = text.as_bytes();
    let mut cursor = offset.min(bytes.len());
    if cursor >= 2 && &bytes[cursor - 2..cursor] == b"->" {
        cursor -= 2;
    } else if cursor >= 1 && bytes[cursor - 1] == b'.' {
        cursor -= 1;
    } else {
        return None;
    }
    let mut start = cursor;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == cursor {
        return None;
    }
    let name = &text[start..cursor];
    is_identifier(name).then(|| name.to_string())
}

/// Identifier *start* byte.
fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_calls() {
        let src = "\
int helper(int value, int other) { return helper(1, 2); }
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        println!("diagnostics: {:?}", analysis.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>());
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        println!("calls: {:?}", index.calls);
        println!("functions: {:?}", index.functions);
    }

    fn at(text: &str, needle: &str, inside: usize) -> LspPosition {
        let column = text.find(needle).expect("needle") + inside;
        LspPosition { line: 0, character: column as u32 }
    }

    fn symbol(text: &str, needle: &str, inside: usize) -> Option<String> {
        symbol_at(text, at(text, needle, inside))
    }

    #[test]
    fn plain_identifiers_resolve_to_themselves() {
        let text = "NPString *s = NPStringCreate(@\"hi\");";
        assert_eq!(symbol(text, "NPString *", 0).as_deref(), Some("NPString"));
        // Cursor one past the last character still belongs to the word.
        assert_eq!(symbol(text, "NPString *", 8).as_deref(), Some("NPString"));
        assert_eq!(symbol(text, "s =", 0).as_deref(), Some("s"));
        assert_eq!(symbol(text, "NPStringCreate", 3).as_deref(), Some("NPStringCreate"));
    }

    #[test]
    fn keyword_selector_is_one_symbol_from_every_part() {
        let text = "[dict setObject:value atIndex:0];";
        for (needle, inside) in [("setObject", 0), ("setObject", 4), ("atIndex", 2), ("atIndex", 6)] {
            assert_eq!(
                symbol(text, needle, inside).as_deref(),
                Some("setObject:atIndex:"),
                "cursor inside {needle}"
            );
        }
        // The receiver and the arguments are not the selector.
        assert_eq!(symbol(text, "dict setObject", 0).as_deref(), Some("dict"));
        assert_eq!(symbol(text, "value atIndex", 0).as_deref(), Some("value"));
    }

    #[test]
    fn colon_less_send_uses_its_last_identifier() {
        let text = "[str UTF8String];";
        assert_eq!(symbol(text, "UTF8String", 0).as_deref(), Some("UTF8String"));
        // The receiver is not promoted to a selector.
        assert_eq!(symbol(text, "str UTF8String", 0).as_deref(), Some("str"));
        // A nested receiver: the outer send's selector is still the last run.
        let text = "[[a b] describe];";
        assert_eq!(symbol(text, "describe", 0).as_deref(), Some("describe"));
        assert_eq!(symbol(text, "a b]", 0).as_deref(), Some("a"));
        // An argument that is itself a send resolves inside that inner send.
        let text = "[outer foo:[inner bar:baz]];";
        assert_eq!(symbol(text, "foo:", 0).as_deref(), Some("foo:"));
        assert_eq!(symbol(text, "inner bar", 6).as_deref(), Some("bar:"));
        assert_eq!(symbol(text, "baz", 0).as_deref(), Some("baz"));
    }

    #[test]
    fn literals_and_punctuation_have_no_symbol() {
        assert_eq!(symbol("return (x + 1);", "+ 1", 0), None);
        assert_eq!(symbol("int y = a ? b : c;", "b :", 0).as_deref(), Some("b"));
        assert_eq!(symbol("int y = a ? b : c;", "c;", 0).as_deref(), Some("c"));
        // A bracket inside a string literal must not confuse the scan.
        let text = "[log write:@\"] not a send\"];";
        assert_eq!(symbol(text, "write:", 0).as_deref(), Some("write:"));
        assert_eq!(symbol_at(text, LspPosition { line: 99, character: 0 }), None);
    }

    #[test]
    fn utf16_columns_are_understood() {
        // `@"中文"` is five UTF-16 units (`@`, `"`, 中, 文, `"`), so `NPString`
        // starts at unit 6.
        let text = "@\"中文\" NPString *s;";
        assert_eq!(
            symbol_at(text, LspPosition { line: 0, character: 6 }).as_deref(),
            Some("NPString")
        );
    }

    #[test]
    fn a_qualified_name_is_one_symbol_from_every_segment() {
        let text = "Game::Entities::Player *p;";
        // A segment is not a name on its own: wherever the cursor sits in the
        // chain, the name is the whole qualified one.
        for inside in [0, 4, 13, 16, 22] {
            assert_eq!(
                symbol(text, "Game::Entities::Player", inside).as_deref(),
                Some("Game::Entities::Player"),
                "cursor {inside} bytes into the name"
            );
        }
        // The first colon after a segment belongs to it (the cursor one past
        // the end of a word is on that word); the second colon belongs to
        // nobody, exactly as in clangd.
        assert_eq!(symbol(text, "Game::", 4).as_deref(), Some("Game::Entities::Player"));
        assert_eq!(symbol_at(text, at(text, "Game::", 5)), None);
    }

    #[test]
    fn the_segment_under_the_cursor_is_where_the_name_stops() {
        let text = "Game::Entities::Player *p;";
        let name = qualified_at(text, at(text, "Game::Entities::Player", 16)).expect("a name");
        assert_eq!(name.text, "Game::Entities::Player");
        // The cursor is on `Player` (16 bytes in), so the name *it* spells ends
        // after `Player` — the whole chain.
        assert_eq!(name.prefix(), "Game::Entities::Player");
        assert!(name.on_name);

        let name = qualified_at(text, at(text, "Game::Entities::Player", 6)).expect("a name");
        // On `Entities` the segment's own name stops after `Entities`.
        assert_eq!(name.prefix(), "Game::Entities");
    }

    #[test]
    fn a_trailing_separator_is_a_prefix_to_complete_not_a_name() {
        let text = "    Game::";
        // Nothing typed after the separator: no name to hover or jump to...
        assert_eq!(symbol_at(text, at(text, "Game::", 6)), None);
        // ...but the separator is what the user has typed, so completion
        // filters by it.
        assert_eq!(typed_name_at(text, at(text, "Game::", 6)).as_deref(), Some("Game::"));
        // A single colon is a keyword colon, not a qualifier.
        let call = "[recv setObject:|";
        assert_eq!(typed_name_at(call, at(call, "setObject:", 10)), None);
    }

    #[test]
    fn a_separator_at_the_end_of_a_line_is_still_a_prefix() {
        // Mid-keystroke: the right-hand side of `A::` is not written yet, and
        // the line ends there. The scan stops at the newline, so the name is the
        // part that exists — a prefix to complete, not a name to answer for.
        let text = "int pending() {\n    A::\n}\n";
        let end_of_line = LspPosition { line: 1, character: 7 };
        assert_eq!(typed_name_at(text, end_of_line).as_deref(), Some("A::"));
        assert_eq!(symbol_at(text, end_of_line), None);
    }

    #[test]
    fn blanks_around_a_separator_do_not_change_the_name() {
        let text = "Game :: Entities :: Player *p;";
        assert_eq!(
            symbol(text, "Player", 0).as_deref(),
            Some("Game::Entities::Player"),
            "the name looks up the way the index keys it"
        );
        let name = qualified_at(text, at(text, "Player", 0)).expect("a name");
        assert_eq!(name.prefix(), "Game::Entities::Player");
        // A newline ends the scan: a chain is not written across lines.
        let text = "Game::\nPlayer *p;";
        assert_eq!(
            symbol_at(text, LspPosition { line: 1, character: 0 }).as_deref(),
            Some("Player")
        );
    }
}
