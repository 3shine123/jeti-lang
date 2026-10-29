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

use std::collections::{BTreeSet, HashMap, HashSet};

use jeti_ast::ast::{AstDecl, AstDeclData, AstDesignator, AstExpr, AstExprData, AstStmt, AstStmtData, AstType, AstUnit, AstUsing};
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

/// A struct/union field at its declaration site. Fields are not indexed as
/// symbols (the symbol tree carries aggregates, not their members), so hover
/// locates the cursor inside the **host aggregate's** extent and answers from
/// the computed layout keyed by `struct_name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRef {
    /// The aggregate's name as written — the key into [`SymbolIndex::layouts`].
    pub struct_name: String,
    pub is_union: bool,
    pub path: String,
    /// The host aggregate's extent: what the cursor must sit inside. A field's
    /// own span is unreliable across declarator shapes (a fn-pointer's
    /// line/col lands on the trailing token, not the name), so matching is by
    /// name within the host range instead.
    pub range: LspRange,
    /// The field's own declaration line (1-based, document-local), for the
    /// card's snippet and provenance.
    pub line: u32,
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
    /// The class's own generic type parameters, in declaration order
    /// (`@interface Box<T, U>` → `["T", "U"]`): the keys a concrete receiver's
    /// type arguments are substituted against.
    type_params: Vec<String>,
    /// Declared outside the document being edited (an imported header) — the
    /// `defaultLibrary` modifier's signal, clangd's system-surface rule.
    external: bool,
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

/// One inlay hint: the parameter name the argument at `position` fills in.
/// The editor draws these as gray, hover-less text over the source (clangd's
/// parameter hints) — they add a word the file does not contain.
#[derive(Debug, Clone)]
pub struct InlayHint {
    pub position: LspPosition,
    pub label: String,
    /// LSP `InlayHintKind`: 1 = Type, 2 = Parameter — the spec's numbering.
    /// Getting it backwards files the hints under the wrong editor setting
    /// (`show_type_hints` / `show_parameter_hints`), which is how they vanish.
    pub kind: u8,
}

/// A member access *use* in a body (`r.lo`, `self->_handle`) whose host
/// aggregate resolved: the hover surface for fields outside their
/// declaration region (declarations answer through `field_positions`).
#[derive(Debug, Clone)]
pub struct MemberUse {
    /// The whole chain's extent — any cursor inside it that no narrower
    /// answer covers reads as this member.
    pub range: LspRange,
    pub member: String,
    /// The host aggregate's key into [`SymbolIndex::layouts`].
    pub struct_name: String,
    pub is_union: bool,
    /// The field's type as the layout spells it, shown by both the hover card
    /// and the use-site hint.
    pub type_spelling: String,
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

/// A name visible in a scope: what class it declares (when its type names a
/// class) and what protocols an `id<P>` / `id<P1, P2>` receiver conforms to.
///
/// A local can be both at once is impossible, but keeping two fields rather
/// than one string keeps `id<P>` first-class: the protocol is the type, not a
/// detail of it.
#[derive(Debug, Clone)]
pub struct ScopeLocal {
    pub name: String,
    /// The declared type's class name, when the type names a class.
    pub class: Option<String>,
    /// Protocols the declared type conforms to, for `id` receivers.
    pub protocols: Vec<String>,
    /// The declared type's concrete generic arguments, when the type instantiates
    /// a generic class (`NPArray<NPString> *a` → `["NPString"]`): what the class's
    /// type parameters are substituted with for a member of this local.
    pub type_args: Vec<String>,
    /// Full declared-type spelling for the hover card (`const char *`,
    /// `NPString *`, `struct S`) — completion never reads it; hover does.
    pub type_spelling: String,
    /// Source line (1-based, document-local) of the declaration, when the
    /// declaration sits in this document — the hover card's provenance.
    pub decl_line: Option<u32>,
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
    /// Visible names and what their declared types resolve to.
    pub locals: Vec<ScopeLocal>,
    /// Every visible name with its spelled type — the hover surface. `locals`
    /// filters to receiver-shaped entries (class/protocols) for completion;
    /// hover answers for scalars and pointers too, so this list skips the
    /// filter.
    pub hover_locals: Vec<ScopeLocal>,
}

/// What a receiver resolves to at a position: the class whose members answer
/// (when its type names a class), the protocols an `id` receiver conforms to,
/// and whether the receiver is a class itself (class methods only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceiverInfo {
    pub class: Option<String>,
    pub protocols: Vec<String>,
    pub is_class_receiver: bool,
    /// The receiver's concrete generic arguments, when its type instantiates a
    /// generic class: what the class's type parameters are substituted with for
    /// the receiver's members. Empty for a bare (non-instantiated) class, an
    /// `id`, or a class receiver.
    pub type_args: Vec<String>,
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
    /// Protocols the receiver conforms to, for `id<P>` receivers: the
    /// checker's type name is class-only, so the protocols ride along
    /// separately (the same split `ScopeLocal` keeps).
    receiver_protocols: Vec<String>,
    /// The receiver's concrete generic arguments, from the same source as
    /// `receiver_type` (the checker's `expr_type`): what the class's type
    /// parameters are substituted with for the receiver's members.
    receiver_type_args: Vec<String>,
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
    /// Protocol name → the selectors it declares, after parent protocols are
    /// merged in (`build` resolves the inheritance). Member completion for an
    /// `id<P>` receiver answers from here: the protocols are the type, and a
    /// protocol's method list is all it has (no bodies, no parameter types).
    protocols: HashMap<String, Vec<MemberEntry>>,
    /// Protocol name → the protocols it inherits, as written, until
    /// `resolve_protocol_inheritance` folds them into [`Self::protocols`].
    protocol_parents: HashMap<String, Vec<String>>,
    /// Scopes of the document being edited.
    scopes: Vec<Scope>,
    /// Aggregates' computed layout (C17 6.7.2.1) keyed by the name as
    /// written, and the typedef alias → aliased-type map. Built during
    /// `build()` from the same AST every other answer derives from; the
    /// hover cards read these for clangd-parity size/offset lines.
    pub layouts: HashMap<String, jeti_layout::StructLayout>,
    pub typedef_types: HashMap<String, AstType>,
    /// Field declaration positions keyed by field name: the hover surface for
    /// struct/union members, which the symbol tree does not index.
    pub field_positions: HashMap<String, Vec<FieldRef>>,
    /// Member accesses in bodies whose host aggregate resolved, in source
    /// order: the hover surface for a field *use* (`r.lo`) away from its
    /// declaration.
    member_uses: Vec<MemberUse>,
    /// Names the semantic-token scan colors, collected on the same build walk:
    /// enum names and every enum member (flat — member names are matched
    /// wherever written), and file-scope variables.
    pub enum_names: HashSet<String>,
    pub enum_member_names: HashSet<String>,
    pub global_vars: HashSet<String>,
    /// Message sends of the document being edited, in source order.
    sites: Vec<SendSite>,
    /// Free-function calls of the document being edited.
    calls: Vec<CallSite>,
    /// Type inlays collected on the same body walk: at each variable *use*
    /// (outside its own declaration) the declared type is shown as a gray
    /// `: T`, clangd-style.
    type_hints: Vec<InlayHint>,
    /// Byte offset of each line's start in the document. Anchoring a hint per
    /// declaration needs its line, and `text.lines().nth(n)` per declaration is
    /// quadratic. Shared (an `Arc`) so the body walk can hold it beside the
    /// mutable hint vector.
    line_offsets: std::sync::Arc<Vec<usize>>,
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
    /// Names each `@using` makes visible, in declaration order: the name it
    /// introduces → the declaration it stands for (see [`Self::read_usings`]).
    ///
    /// A `Vec`, not a map: the elaborator resolves a name through these in the
    /// order the binder recorded them, so the first entry that matches wins.
    aliases: Vec<(String, String)>,
    /// Namespace prefixes each `@using` makes visible, in declaration order.
    using_namespaces: Vec<String>,
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
        index.line_offsets = std::sync::Arc::new(line_offsets(
            lookup(document).as_deref().unwrap_or(""),
        ));
        let (layouts, typedef_types) = jeti_layout::collect_layouts(&ast.decls);
        index.layouts = layouts;
        index.typedef_types = typedef_types;
        index.extend_layouts_with_typedef_aggregates(&ast.decls);
        index.roots = index.visit(&ast.decls, None, source_map, document, &mut lookup);
        index.collect_field_positions(&ast.decls, source_map, &mut lookup);
        index.attach_namespaces(ast, source_map, &mut lookup);
        index.read_usings(&ast.usings);
        index.resolve_protocol_inheritance();
        index
    }

    /// `typedef struct { … } np_u128;` files its body under the typedef, not
    /// under a named aggregate, so `collect_layouts` has no entry for the alias
    /// and the hover/member answers would have no table to read. Synthesize the
    /// aggregate the C tag would have had, keyed by the alias — and by the tag
    /// name when the typedef spells one (`typedef struct Tag { … } Alias;`),
    /// because `struct Tag t;` names the same layout.
    ///
    /// The parser drops `union` for an anonymous body (the CST keeps only
    /// `struct_fields`), so an anonymous typedef is taken as a struct — the
    /// shape the Foundation headers use. A typedef over a *named* tag keeps its
    /// `TagKind`, `union` included.
    fn extend_layouts_with_typedef_aggregates(&mut self, decls: &[AstDecl]) {
        for decl in decls {
            let AstDeclData::Typedef { struct_fields, aliased_type } = &decl.data else {
                continue;
            };
            self.extend_layouts_with_typedef_aggregates(struct_fields);
            let (Some(alias), false) = (&decl.name, struct_fields.is_empty()) else {
                continue;
            };
            let tag = aliased_type.as_deref().map(|t| t.tag);
            let synth = |name: &str| AstDecl {
                kind: jeti_ast::AstDeclKind::Struct,
                line: decl.line,
                col: decl.col,
                name: Some(name.to_string()),
                data: AstDeclData::Aggregate {
                    fields: struct_fields.clone(),
                    is_union: matches!(tag, Some(jeti_cst::TagKind::Union)),
                },
                attributes: Vec::new(),
                end_line: decl.end_line,
                end_col: decl.end_col,
            };
            // The alias first: a bare `np_u128 r;` spells the alias, and the
            // tag (if any) is what `struct Tag r;` spells.
            let tag_name = aliased_type.as_deref().and_then(|t| t.name.clone());
            let mut names = vec![alias.clone()];
            if let Some(tag_name) = tag_name.filter(|n| n != alias) {
                names.push(tag_name);
            }
            for name in names {
                if self.layouts.contains_key(&name) {
                    continue;
                }
                if let Some(layout) =
                    jeti_layout::layout_of(&synth(&name), &self.layouts, &self.typedef_types)
                {
                    if !layout.name.is_empty() {
                        self.layouts.insert(name, layout);
                    }
                }
            }
        }
    }

    /// Parent protocols' selectors become members of the child, ObjC-style:
    /// conforming to a protocol means seeing everything its parents declared.
    /// One pass per child is enough for the shapes programs use; a protocol
    /// cycle would at worst miss a level, never spin.
    fn resolve_protocol_inheritance(&mut self) {
        let entries: Vec<(String, Vec<String>)> = self.protocol_parents.clone().into_iter().collect();
        for (name, parents) in entries {
            let mut inherited: Vec<MemberEntry> = Vec::new();
            for parent in parents {
                if let Some(parent_members) = self.protocols.get(&parent).cloned() {
                    inherited.extend(parent_members);
                }
            }
            if inherited.is_empty() {
                continue;
            }
            let members = self.protocols.entry(name).or_default();
            for member in inherited {
                if !members.iter().any(|existing| existing.label == member.label) {
                    members.insert(0, member.clone());
                }
            }
        }
    }

    /// Record what each `@using` makes visible.
    ///
    /// The binder resolves these into its symbol table and the elaborator drops
    /// them, so a name reached through one (`Alias` in `@using Alias = A::Player;`)
    /// had nothing to answer from. The *policy* stays the binder's; this mirrors
    /// its effect against the index instead of re-deciding it: a name is
    /// imported when the index knows the fully-qualified name as a declaration,
    /// and that name doubles as a namespace prefix. The binder imports only a
    /// class or a protocol (anything else becomes a bare prefix) and rejects an
    /// import that shadows an existing declaration (`ambiguous import`), so in a
    /// file it accepts the two rules agree; where they differ — a typedef,
    /// struct or enum target — the compiler resolves nothing at all, while the
    /// index answers the declaration the `@using` names.
    fn read_usings(&mut self, usings: &[AstUsing]) {
        for using in usings {
            match &using.alias {
                Some(alias) => {
                    if let Some(target) = self.known_type(&using.fqn) {
                        self.aliases.push((alias.clone(), target));
                    }
                }
                None => {
                    // `@using A::B;` brings `B` in; `@using A;` brings in no
                    // short name of its own (the binder's `short` is `None`).
                    if let Some((_, last)) = using.fqn.rsplit_once("::") {
                        if self.by_name.contains_key(&using.fqn) {
                            self.aliases.push((last.to_string(), using.fqn.clone()));
                        }
                    }
                    self.using_namespaces.push(using.fqn.clone());
                }
            }
        }
    }

    /// The declaration a `@using` right-hand side names, if the index knows it.
    ///
    /// The spelling keeps what the binder strips before storing a name: a
    /// trailing `*` (`@using Alias = A::Player*;`) and an `id<Protocol>`
    /// qualifier. The qualifier is read as the protocol it names — that is the
    /// declaration go-to-definition on the alias should reach — and a generic
    /// argument list is dropped, since it is not part of the name the
    /// declaration is indexed under.
    fn known_type(&self, spelling: &str) -> Option<String> {
        let spelling = spelling.trim_end_matches('*').trim();
        if let Some(inner) = spelling.strip_prefix("id<") {
            let protocol = inner.trim_end_matches('>').trim_end_matches('*').trim();
            return self.by_name.contains_key(protocol).then(|| protocol.to_string());
        }
        let base = spelling.split('<').next().unwrap_or(spelling).trim();
        self.by_name.contains_key(base).then(|| base.to_string())
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
                range: source_map.to_lsp_range(&span, &text),
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
        // The @property attribute list holds no symbols: a bare `copy` there
        // used to hover as "method copy in NPArray" because the member search
        // matched any class's `- copy`. clangd answers nothing in the list —
        // clang's grammar colors it as keyword.other.property.attribute.
        if position_inside_property_attrs(text, position) {
            return None;
        }
        lookup_names(text, position)
            .into_iter()
            .find_map(|candidate| self.resolve_candidate(&candidate, position))
    }

    /// The local/parameter/ivar spelling visible at `position` under `name`,
    /// for the hover card. The declaration index only carries file-level
    /// symbols, so `sp` in `sp.label = nil` answered nothing while `label`
    /// (a class member) did — this is the missing half. Hover-only:
    /// references and definition keep the declaration-index semantics.
    pub fn local_at(&self, position: LspPosition, name: &str) -> Option<ScopeLocal> {
        for scope in &self.scopes {
            if let Some(range) = &scope.range {
                if !(range.start <= position && position <= range.end) {
                    continue;
                }
            }
            if let Some(local) = scope.hover_locals.iter().find(|l| l.name == name) {
                return Some(local.clone());
            }
        }
        None
    }

    /// The field whose host aggregate the cursor sits inside, or `None`. The
    /// narrowest containing range wins, so a field name shared by an inner and
    /// an outer aggregate answers for the inner one.
    pub fn field_at(&self, name: &str, position: LspPosition) -> Option<&FieldRef> {
        self.field_positions
            .get(name)?
            .iter()
            .filter(|f| f.range.start <= position && position <= f.range.end)
            .min_by_key(|f| {
                (
                    f.range.end.line.saturating_sub(f.range.start.line),
                    f.range.end.character.saturating_sub(f.range.start.character),
                )
            })
    }

    /// The member access at `position` whose host aggregate resolved — the
    /// hover answer for a field *use* (`r.lo` in a body), which
    /// `field_positions` cannot give because the declaration is elsewhere. The
    /// narrowest match wins: an inner ring's own extent beats the whole
    /// chain's.
    pub fn member_use_at(&self, name: &str, position: LspPosition) -> Option<&MemberUse> {
        self.member_uses
            .iter()
            .filter(|u| u.member == name && u.range.start <= position && position <= u.range.end)
            .min_by_key(|u| {
                (
                    u.range.end.line.saturating_sub(u.range.start.line),
                    u.range.end.character.saturating_sub(u.range.start.character),
                )
            })
    }

    /// A free function's signature, for the hover card's return-type arrow and
    /// parameter list. Methods answer through `Location.detail` instead.
    pub fn free_function(&self, name: &str) -> Option<&Signature> {
        self.functions.get(name)
    }

    /// The local under the cursor plus its enclosing function — the hover
    /// card's "In" line. `local_at` answers completion-shaped questions; this
    /// one carries the scope owner the card spells.
    pub fn local_hover_at(&self, position: LspPosition, name: &str) -> Option<(ScopeLocal, String)> {
        for scope in &self.scopes {
            if let Some(range) = &scope.range {
                if !(range.start <= position && position <= range.end) {
                    continue;
                }
            }
            if let Some(local) = scope.hover_locals.iter().find(|l| l.name == name) {
                return Some((local.clone(), scope.owner.clone()));
            }
        }
        None
    }

    /// The C headers a document pulls in — the passthrough surface the toolchain
    /// probe (`cc -E`) extracts symbols from. Every `#include <...>` qualifies;
    /// a quoted `#include "..."` qualifies only when it names a C header (`.h`),
    /// which is how the runtime reaches generated code (`"jeti/runtime.h"`).
    /// Quoted `.hje`/`.jeti` are jeti sources and stay out of this table.
    pub fn c_headers_in(text: &str) -> Vec<String> {
        let mut headers = Vec::new();
        for line in text.lines() {
            let rest = line.trim_start();
            let Some(rest) = rest.strip_prefix('#') else {
                continue;
            };
            let rest = rest.trim_start();
            let Some(rest) = rest.strip_prefix("include") else {
                continue;
            };
            let rest = rest.trim_start();
            let name = if let Some(rest) = rest.strip_prefix('<') {
                let Some(end) = rest.find('>') else { continue };
                rest[..end].to_string()
            } else if let Some(rest) = rest.strip_prefix('"') {
                let Some(end) = rest.find('"') else { continue };
                let quoted = &rest[..end];
                if !quoted.ends_with(".h") {
                    continue;
                }
                quoted.to_string()
            } else {
                continue;
            };
            if !name.is_empty() && !headers.contains(&name) {
                headers.push(name);
            }
        }
        headers
    }

    /// Semantic tokens for the whole document: every identifier scanned, then
    /// classified through the same index the hover answers from (locals beat
    /// globals, declarations beat keywords). Grammar keeps the keywords and
    /// punctuation; this adds what only the compiler knows — types, classes,
    /// fields, enum members, macros.
    pub fn semantic_tokens(&self, text: &str) -> Vec<u32> {
        const T_NAMESPACE: u8 = 0;
        const T_CLASS: u8 = 1;
        const T_TYPE: u8 = 2;
        const T_ENUM: u8 = 3;
        const T_FUNCTION: u8 = 4;
        const T_VARIABLE: u8 = 5;
        const T_PROPERTY: u8 = 6;
        const T_MACRO: u8 = 7;
        const T_ENUM_MEMBER: u8 = 8;

        // `#define`d names: the definition site colors as macro, and so does
        // every later use (clangd does the same from its macro table).
        let mut macros: HashSet<String> = HashSet::new();
        let mut tokens: Vec<(u32, u32, u32, u8, u8)> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim_start();
            let Some(rest) = line.strip_prefix('#') else { continue };
            let rest = rest.trim_start();
            let Some(rest) = rest.strip_prefix("define") else { continue };
            let Some(rest) = rest.strip_prefix(|c: char| c.is_whitespace()) else { continue };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let name_len = name.chars().map(|c| c.len_utf16() as u32).sum();
            macros.insert(name);
            let char = (line.len() - rest.len()) as u32;
            tokens.push((i as u32, char, name_len, T_MACRO, 0));
        }

        // Identifier scan: classify each word that is actually code —
        // `code_mask` blanks comments, string/char literals, and preprocessor
        // lines (the same mask find-references scans through), so a keyword
        // mentioned inside a comment keeps the comment's single color.
        let mask = code_mask(text);
        // The passthrough C surface comes from the real toolchain for exactly
        // the system headers this document includes.
        let c_table = crate::cheader::signatures_for(&Self::c_headers_in(text), &[]);
        // Byte offset of each line's start in the full text, so line-local
        // `char_indices` can index the global mask (works for LF and CRLF).
        let line_starts: Vec<usize> = std::iter::once(0usize)
            .chain(
                text.bytes()
                    .enumerate()
                    .filter(|(_, b)| *b == b'\n')
                    .map(|(i, _)| i + 1),
            )
            .collect();
        for (i, line) in text.lines().enumerate() {
            let bytes = line.as_bytes();
            let mut start = None;
            let mut start16 = 0u32;
            let mut col16 = 0u32;
            // The previous word ended right before a `.`/`->` whose tail this
            // word follows: an access, so the word colors as a property (or a
            // dispatching method), never as a scope-shadowed variable.
            let mut access_tail = false;
            for (col, ch) in line.char_indices() {
                let at = line_starts[i] + col;
                if is_ident_byte(bytes.get(col).copied().unwrap_or(b' ')) {
                    if mask.get(at).copied().unwrap_or(true) {
                        if start.is_none() {
                            start = Some(col);
                            start16 = col16;
                        }
                    } else if start.take().is_some() {
                        // A comment cut the word: what came before is not a
                        // token; what follows is not an access tail either.
                        access_tail = false;
                    }
                } else if let Some(from) = start.take() {
                    let word = &line[from..col];
                    let pos = LspPosition { line: i as u32, character: start16 };
                    let rest = &text[line_starts[i] + col..];
                    // Attribute-list words get no semantic token — clang's
                    // grammar colors them (keyword.other.property.attribute);
                    // emitting one turned `copy` into a method (it matched
                    // any class's `- copy` member).
                    let attr_pos = inside_property_attrs(line, from);
                    if !attr_pos {
                        if let Some((t, mods)) = self.classify_token(word, pos, &macros, &c_table, rest, access_tail) {
                            let len = word.chars().map(|c| c.len_utf16() as u32).sum();
                            tokens.push((i as u32, start16, len, t, mods));
                        }
                    }
                    // `.` or `->` between words: what follows is a member.
                    // The `>` of `->` must keep the tail alive, not reset it.
                    access_tail = match ch {
                        '.' => true,
                        '-' => line.as_bytes().get(col + 1) == Some(&b'>'),
                        '>' => access_tail,
                        _ => false,
                    };
                } else {
                    access_tail = match ch {
                        '.' => true,
                        '-' => line.as_bytes().get(col + 1) == Some(&b'>'),
                        '>' => access_tail,
                        _ => false,
                    };
                }
                col16 += ch.len_utf16() as u32;
            }
            if let Some(from) = start {
                let word = &line[from..];
                let pos = LspPosition { line: i as u32, character: start16 };
                let rest = &text[line_starts[i] + line.len()..];
                if !inside_property_attrs(line, from) {
                    if let Some((t, mods)) = self.classify_token(word, pos, &macros, &c_table, rest, access_tail) {
                        let len = word.chars().map(|c| c.len_utf16() as u32).sum();
                        tokens.push((i as u32, start16, len, t, mods));
                    }
                }
            }
        }

        tokens.sort_by_key(|&(line, char, _, _, _)| (line, char));
        // LSP delta encoding.
        let mut out = Vec::with_capacity(tokens.len() * 5);
        let mut prev_line = 0u32;
        let mut prev_char = 0u32;
        for (line, char, len, t, mods) in tokens {
            let (dline, dchar) = if line == prev_line {
                (0, char - prev_char)
            } else {
                (line - prev_line, char)
            };
            out.extend([dline, dchar, len, t as u32, mods as u32]);
            prev_line = line;
            prev_char = char;
        }
        out
    }

    fn classify_token(
        &self,
        word: &str,
        pos: LspPosition,
        macros: &HashSet<String>,
        c_table: &crate::cheader::CSignatures,
        rest: &str,
        access: bool,
    ) -> Option<(u8, u8)> {
        if BUILTIN_AND_KEYWORDS.contains(&word) {
            return None;
        }
        // Nullability qualifiers: clangd emits no semantic token for these —
        // they are type qualifiers, never declarations, and clang's TextMate
        // grammar colors them via storage.modifier.protocol. Without this,
        // the bare spellings fell through to the member search below and
        // words like `nullable` could pick up a `method` color from any
        // class that happened to declare a member of that name. The `__`
        // spellings join the same suppression.
        if matches!(
            word,
            "nonnull" | "nullable" | "null_unspecified" | "null_resettable"
                | "__nullable" | "__nonnull" | "__unsafe_unretained"
        ) {
            return None;
        }
        // A soft keyword in keyword position colors as a keyword; the same
        // spelling in identifier position falls through to the usual rules.
        if let Some(t) = soft_keyword_token(word, rest) {
            return Some((t, 0));
        }
        // Numbers never take a semantic color (`1.5` would otherwise make the
        // `5` a "member" — the digit-start guard is what keeps floats whole).
        if word.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        if macros.contains(word) {
            return Some((7, 0));
        }
        // `obj.member` / `ptr->member`: a class method dispatches (function
        // color, clangd's method-call yellow), anything else reads as a
        // property — known field or not, it sits left of `=`. Selector parts
        // match per colon segment (`addObject:` answers to `addObject`) — the
        // whole-word compare is what left multi-part selectors colorless. A
        // class method carries `static`, clangd's modifier for `+` methods.
        let find_member =
            |info: &ClassInfo| {
                info.members
                    .iter()
                    .find(|m| selector_part_matches(word, &m.label))
                    .map(|m| (m.kind, m.is_class_method))
            };
        let method_pair = |kind: u8, class_method: bool| -> (u8, u8) {
            if kind == kind::METHOD {
                (4, if class_method { MOD_STATIC } else { 0 })
            } else {
                (6, 0)
            }
        };
        if access {
            for info in self.classes.values() {
                if let Some((member_kind, class_method)) = find_member(info) {
                    return Some(method_pair(member_kind, class_method));
                }
            }
            return Some((6, 0));
        }
        // Innermost scope wins: a local shadows anything global.
        if self.local_at(pos, word).is_some() {
            return Some((5, 0));
        }
        if self.functions.contains_key(word) {
            return Some((4, 0));
        }
        for info in self.classes.values() {
            if let Some((member_kind, class_method)) = find_member(info) {
                return Some(method_pair(member_kind, class_method));
            }
        }
        if self.enum_member_names.contains(word) {
            return Some((8, 0));
        }
        if self.enum_names.contains(word) {
            return Some((3, 0));
        }
        // A class name used as a type stays class-colored (clangd's rule for
        // ObjC interfaces): a Foundation class also carries a struct layout
        // under the same name, and hitting the `type` branch below first is
        // what made class uses render in the alias color. A class declared in
        // an imported header carries `defaultLibrary` — the theme's cue to
        // style the system surface (clangd's italics) apart from project
        // classes (the underline).
        if let Some(info) = self.classes.get(word) {
            let mods = if info.external { MOD_DEFAULT_LIBRARY } else { 0 };
            return Some((1, mods));
        }
        if self.protocols.contains_key(word)
            || self.layouts.contains_key(word)
            || self.typedef_types.contains_key(word)
        {
            return Some((2, 0));
        }
        // Passthrough C surface, extracted from the real toolchain for the
        // headers this document includes — no name tables in the source.
        // User declarations above already had their chance, so a project
        // function named `printf` wins over the library's. A real prototype
        // also beats a macro *spelling*: macOS fortify defines `memset(...)`
        // as a macro, but the use in code still reads as the declared call.
        // Everything the toolchain table holds came from system headers, so
        // each shape carries `defaultLibrary` the way clangd's does.
        match c_table.decl(word) {
            Some((crate::cheader::DeclKind::Function, _)) => return Some((4, MOD_DEFAULT_LIBRARY)),
            Some((crate::cheader::DeclKind::Variable, _)) => return Some((5, MOD_DEFAULT_LIBRARY)),
            Some((crate::cheader::DeclKind::Typedef, _)) => return Some((2, MOD_DEFAULT_LIBRARY)),
            None => {}
        }
        // The toolchain's macro-only surface (`EOF`, `NULL`, `errno` on
        // macOS) colors as a macro wherever it appears.
        if c_table.macro_name(word).is_some() {
            return Some((7, MOD_DEFAULT_LIBRARY));
        }
        if self.global_vars.contains(word) {
            return Some((5, 0));
        }
        if self.namespaces.contains(word) {
            return Some((0, 0));
        }
        if self.field_at(word, pos).is_some() {
            return Some((6, 0));
        }
        None
    }

    /// The answer for one raw name at one position: `@using` first, a
    /// declaration next (the more specific answer), a namespace last. Shared by
    /// `resolve_at` (cursor) and `find_references` (every name occurrence), so
    /// the two can never disagree about what a name means.
    fn resolve_candidate(&self, candidate: &str, position: LspPosition) -> Option<Resolved> {
        // A name a `@using` introduces is checked first, the order the
        // elaborator resolves in: the binder has already rejected a file where
        // one would shadow a declaration, so a name import is the answer
        // wherever it applies.
        if let Some((_, target)) = self.aliases.iter().find(|(name, _)| *name == candidate) {
            return Some(Resolved::Symbol(target.clone()));
        }
        // A declaration beats a namespace of the same name: it is the more
        // specific answer.
        if self.by_name.contains_key(candidate) {
            return Some(Resolved::Symbol(self.narrow(candidate, position)));
        }
        if self.namespaces.contains(candidate) {
            return Some(Resolved::Namespace(candidate.to_string()));
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
        // `@using A;` first, then the block walk outward: the same order the
        // elaborator's `ns_fqn` tries a name, and the same reason the innermost
        // block wins — the nearer name is the one the compiler resolves to.
        let walk = self
            .using_namespaces
            .iter()
            .map(String::as_str)
            .chain(self.enclosing_namespaces(position));
        for prefix in walk {
            let scoped = format!("{prefix}::{name}");
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

    /// Every reference to the symbol under `position`, in the edited document.
    ///
    /// The answer is a text scan confirmed by the same resolution the cursor
    /// uses: `find_references` walks the document's maximal name tokens —
    /// type references have no source origin of their own in the AST
    /// (`AstType` carries no position), so the text is the only place that sees
    /// every occurrence — and keeps a token only when resolving it at its own
    /// position yields the same symbol the cursor resolved to. That last step
    /// is what excludes a same-named symbol in another namespace: the bare
    /// `Player` written in `@namespace B` resolves to `B::Player` and drops out
    /// of `A::Player`'s references.
    ///
    /// v1 scope: the edited document only, declaration-name symbols plus
    /// selector references (`token:` is tried when `token` alone does not
    /// resolve); local-variable reads are not in the declaration index, so a
    /// reference to one is answered by its declaration alone.
    pub fn find_references(&self, text: &str, position: LspPosition) -> Vec<Location> {
        let Some(Resolved::Symbol(target)) = self.resolve_at(text, position) else {
            return Vec::new();
        };
        let ranges = self
            .scan_target_tokens(text, &target)
            .into_iter()
            .map(|(_, start, end)| LspRange {
                start: offset_to_position(text, start),
                end: offset_to_position(text, end),
            })
            .collect::<Vec<_>>();

        // The declarations themselves are references too (LSP's
        // `includeDeclaration` defaults to true), and they are the answer for a
        // symbol the scan cannot see (a local, for instance). A scan range that
        // sits inside a declaration's own extent is the same reference, not a
        // second one: the name is on the declaration's first line, and the
        // extent starts where the declaration starts (the `@interface`
        // keyword), so equality would never hold.
        let mut locations: Vec<Location> = self.definitions(&target);
        // The scan only saw the edited document: every range it produced is a
        // position in this index's own file.
        for range in ranges {
            let duplicate = locations.iter().any(|l| {
                l.path == self.document
                    && l.range.start.line == range.start.line
                    && l.range.start.character <= range.start.character
                    && (l.range.end.line > range.end.line
                        || (l.range.end.line == range.end.line
                            && l.range.end.character >= range.end.character))
            });
            if !duplicate {
                locations.push(Location {
                    name: target.clone(),
                    kind: 1,
                    detail: String::new(),
                    container: None,
                    path: self.document.clone(),
                    range,
                });
            }
        }
        locations.sort_by(|a, b| (&a.path, a.range.start).cmp(&(&b.path, b.range.start)));
        locations.dedup_by(|a, b| a.path == b.path && a.range.start == b.range.start);
        locations
    }

    /// Every maximal name token of the document that resolves to `target`,
    /// with its byte extent — the scan shared by `find_references` and the
    /// rename.
    ///
    /// The forms a usage may be written in: the full name, its short form, and
    /// — for a selector, which the token scan reads without its trailing colon
    /// — the colon-stripped form. A token is kept only when resolving it at
    /// its own position yields `target`: the step that excludes a same-named
    /// symbol in another namespace (the bare `Player` written in
    /// `@namespace B` resolves to `B::Player` and drops out of
    /// `A::Player`'s occurrences).
    fn scan_target_tokens(&self, text: &str, target: &str) -> Vec<(String, usize, usize)> {
        let mut forms: Vec<String> = vec![target.to_string()];
        if let Some(short) = target.rsplit("::").next() {
            if short != target && !forms.iter().any(|f| f == short) {
                forms.push(short.to_string());
            }
        }
        let stripped = target.trim_end_matches(':');
        if !forms.iter().any(|f| f == stripped) {
            forms.push(stripped.to_string());
        }
        let mask = code_mask(text);
        name_tokens(text, &mask)
            .into_iter()
            .filter(|(token, _, _)| forms.iter().any(|f| f == token))
            .filter(|(token, start, _)| {
                // A selector occurrence is written `name:`; try the colon
                // when the bare name does not resolve (methods are keyed by
                // their selector).
                let at = offset_to_position(text, *start);
                self.resolve_candidate(token, at)
                    .or_else(|| self.resolve_candidate(&format!("{token}:"), at))
                    .is_some_and(|resolved| {
                        matches!(&resolved, Resolved::Symbol(name) if name == target)
                    })
            })
            .collect()
    }

    /// The range rename should preselect: the name under the cursor, and —
    /// for a qualified name — its last segment only, so renaming `A::Player`
    /// keeps the qualifier. A selector's range carries its trailing colon (the
    /// spelling the text has). A namespace declines: in v1 there is no
    /// consistent set of occurrences for one, and a partial answer is worse
    /// than none.
    pub fn rename_range_at(&self, text: &str, position: LspPosition) -> Option<LspRange> {
        let Some(Resolved::Symbol(target)) = self.resolve_at(text, position) else {
            return None;
        };
        if self.namespaces.contains(&target) {
            return None;
        }
        let offset = jeti_cst::source_map::SourceFile::lsp_to_offset(text, position)?;
        let is_selector = target.ends_with(':');
        for (token, start, end) in self.scan_target_tokens(text, &target) {
            let (seg_start, seg_end) = rename_segment(&token, start, end, is_selector, text);
            // The end is inclusive: an editor reports the position after the
            // last character when a word is selected.
            if offset >= seg_start && offset <= seg_end {
                return Some(LspRange {
                    start: offset_to_position(text, seg_start),
                    end: offset_to_position(text, seg_end),
                });
            }
        }
        None
    }

    /// Every range rename must replace for the symbol under `position`, in the
    /// edited document: the same occurrences `find_references` scans, each
    /// shrunk to the segment a rename rewrites.
    pub fn rename_ranges(&self, text: &str, position: LspPosition) -> Vec<LspRange> {
        let Some(Resolved::Symbol(target)) = self.resolve_at(text, position) else {
            return Vec::new();
        };
        if self.namespaces.contains(&target) {
            return Vec::new();
        }
        let is_selector = target.ends_with(':');
        self.scan_target_tokens(text, &target)
            .into_iter()
            .map(|(token, start, end)| {
                let (seg_start, seg_end) = rename_segment(&token, start, end, is_selector, text);
                LspRange {
                    start: offset_to_position(text, seg_start),
                    end: offset_to_position(text, seg_end),
                }
            })
            .collect()
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
                let external = decl_path(decl, source_map)
                    .map(|path| !same_file(&path, document))
                    .unwrap_or(false);
                self.register_class(name, &decl.data, external);
            }
            if let (Some(name), AstDeclData::Protocol { parents, required_methods, optional_methods }) =
                (&decl.name, &decl.data)
            {
                // A protocol's whole API surface is its selector list — no
                // parameter types come with it, so members carry no signature.
                self.protocol_parents.insert(name.clone(), parents.clone());
                let members = self.protocols.entry(name.clone()).or_default();
                for selector in required_methods.iter().chain(optional_methods.iter()) {
                    if !members.iter().any(|existing| existing.label == *selector) {
                        members.push(MemberEntry {
                            label: selector.clone(),
                            kind: kind::METHOD,
                            detail: "-".to_string(),
                            is_class_method: false,
                            type_name: None,
                            signature: None,
                        });
                    }
                }
            }
            if let (Some(name), AstDeclData::Enum { members, .. }) = (&decl.name, &decl.data) {
                self.enum_names.insert(name.clone());
                for member in members {
                    self.enum_member_names.insert(member.clone());
                }
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
                    if symbol.kind == kind::VARIABLE {
                        self.global_vars.insert(symbol.lookup.clone());
                    }
                    out.push(symbol);
                }
                None => out.extend(children),
            }
        }
        out
    }

    /// Record every aggregate field's declaration position, mirroring the
    /// layout engine's walk (same aggregates, same synthesized class
    /// aggregate) so the field hover and the struct card agree on what exists.
    fn collect_field_positions(
        &mut self,
        decls: &[AstDecl],
        source_map: &SourceMap,
        lookup: &mut impl FnMut(&str) -> Option<String>,
    ) {
        for decl in decls {
            match &decl.data {
                AstDeclData::Aggregate { fields, is_union } => {
                    if let Some(name) = &decl.name {
                        self.record_fields(fields.iter(), name, *is_union, decl, source_map, lookup);
                    }
                    self.collect_field_positions(fields, source_map, lookup);
                }
                AstDeclData::Typedef { struct_fields, aliased_type } => {
                    // The layout engine keys a typedef'd aggregate by its alias
                    // (`extend_layouts_with_typedef_aggregates` installs that
                    // entry), so the fields answer under the same name.
                    if let (Some(name), false) = (&decl.name, struct_fields.is_empty()) {
                        let is_union = matches!(
                            aliased_type.as_deref().map(|t| t.tag),
                            Some(jeti_cst::TagKind::Union)
                        );
                        self.record_fields(
                            struct_fields.iter(),
                            name,
                            is_union,
                            decl,
                            source_map,
                            lookup,
                        );
                    }
                    self.collect_field_positions(struct_fields, source_map, lookup);
                }
                AstDeclData::Class { methods, ivars, impl_vars, .. } => {
                    // The layout engine keys a class's implicit ivar aggregate
                    // by the class name; mirror that so ivar hover works too.
                    if let Some(name) = &decl.name {
                        let ivars = ivars.iter().chain(impl_vars.iter());
                        self.record_fields(ivars, name, false, decl, source_map, lookup);
                    }
                    self.collect_field_positions(methods, source_map, lookup);
                }
                AstDeclData::Namespace(inner) => {
                    self.collect_field_positions(inner, source_map, lookup);
                }
                _ => {}
            }
        }
    }

    /// Record `fields` against `owner`'s extent: the cursor must sit inside
    /// the aggregate's body, and the field is found by name — a field's own
    /// span is unreliable across declarator shapes (fn-pointers, arrays,
    /// bit-fields land their line/col on a trailing token, not the name).
    fn record_fields<'a>(
        &mut self,
        fields: impl Iterator<Item = &'a AstDecl>,
        struct_name: &str,
        is_union: bool,
        owner: &AstDecl,
        source_map: &SourceMap,
        lookup: &mut impl FnMut(&str) -> Option<String>,
    ) {
        let span = source_map.span_from_node(
            (owner.line as u32, owner.col as u32),
            (owner.end_line as u32, owner.end_col as u32),
        );
        if span.is_synthetic() {
            return;
        }
        let path = source_map.registry().path_of(span.file).to_string();
        let Some(text) = lookup(&path) else { return };
        let range = source_map.to_lsp_range(&span, &text);
        for field in fields {
            let (Some(name), AstDeclData::Ivar { .. }) = (&field.name, &field.data) else {
                continue;
            };
            self.field_positions
                .entry(name.clone())
                .or_default()
                .push(FieldRef {
                    struct_name: struct_name.to_string(),
                    is_union,
                    path: path.clone(),
                    range,
                    line: mapped_line(source_map, field.line),
                });
        }
    }

    /// Record a class's members and ancestor, from any file (a local's type is
    /// usually declared in a header). Declaration and implementation merge into
    /// one entry; the first non-empty ancestor wins, since an `@implementation`
    /// records only the implicit root.
    fn register_class(&mut self, name: &str, data: &AstDeclData, external: bool) {
        let AstDeclData::Class { super_name, methods, properties, ivars, impl_vars, type_params, .. } = data else {
            return;
        };
        let entry = self.classes.entry(name.to_string()).or_default();
        if entry.super_name.is_none() {
            entry.super_name = super_name.clone();
        }
        if entry.type_params.is_empty() {
            entry.type_params = type_params.clone();
        }
        entry.external |= external;
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
            AstDeclData::Function { params, body, .. } => {
                (params.as_deref(), body.as_deref(), None)
            }
            AstDeclData::Variable { .. } => {
                // A file-scope variable is visible everywhere in the document.
                let path = decl_path(decl, source_map)?;
                if !same_file(&path, document) || container.is_some() {
                    return None;
                }
                let name = decl.name.clone()?;
                let (class, protocols, type_args) = match &decl.data {
                    AstDeclData::Variable { var_type, .. } => var_type
                        .as_deref()
                        .map(|ty| (ast_type_name(ty), ast_id_protocols(ty), ast_type_args(ty)))
                        .unwrap_or_default(),
                    _ => (None, Vec::new(), Vec::new()),
                };
                // A name only enters the scope when its type says something:
                // a class to complete against or a protocol an id conforms to.
                if class.is_none() && protocols.is_empty() {
                    return None;
                }
                let spelling = match &decl.data {
                    AstDeclData::Variable { var_type, .. } => var_type
                        .as_deref()
                        .map(|t| ast_type_spell(t))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                return Some(Scope {
                    range: None,
                    owner: "<file>".to_string(),
                    self_type: None,
                    locals: vec![ScopeLocal {
                        name: name.clone(), class, protocols, type_args,
                        type_spelling: spelling,
                        decl_line: Some(mapped_line(source_map, decl.line)),
                    }],
                    hover_locals: vec![],
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
            range: Some(source_map.to_lsp_range(&span, &text)),
            owner: decl.name.clone().unwrap_or_default(),
            self_type: self_type.map(str::to_string),
            locals: Vec::new(),
            hover_locals: Vec::new(),
        };

        // Parameters.
        let mut param = params;
        while let Some(current) = param {
            if let (Some(name), Some(ty)) = (&current.name, current.par_type.as_deref()) {
                let class = cst_type_name(ty);
                let protocols = cst_id_protocols(ty);
                let type_args = cst_type_args(ty);
                let spelling = cst_type_spell(ty);
                if class.is_some() || !protocols.is_empty() {
                    scope.locals.push(ScopeLocal {
                        name: name.clone(), class: class.clone(), protocols: protocols.clone(),
                        type_args: type_args.clone(),
                        type_spelling: spelling.clone(),
                        decl_line: None,
                    });
                }
                scope.hover_locals.push(ScopeLocal {
                    name: name.clone(), class, protocols, type_args,
                    type_spelling: spelling.clone(),
                    decl_line: None,
                });
                // No type hint at a DECLARATION site: the type is spelled right
                // before the name, so `int value` reading `value -> int` is
                // noise (clangd does not emit it either). USE sites keep their
                // hint — see the `VarRef` arm of the body walk.
            }
            param = current.next.as_deref();
        }
        // No return-type hint after the parameter list: `(int a) -> int`
        // repeats the type already spelled at the start of the same line.
        // The enclosing class's ivars are visible by name inside its methods.
        if let Some(class) = self_type {
            if let Some(info) = self.classes.get(class) {
                for member in &info.members {
                    if let (Some(class_name), false) = (&member.type_name, member.is_class_method) {
                        scope.locals.push(ScopeLocal {
                            name: member.label.clone(),
                            class: Some(class_name.clone()),
                            protocols: Vec::new(),
                            // A member's declared type is recorded by name
                            // alone: no type arguments to carry.
                            type_args: Vec::new(),
                            type_spelling: format!("{class_name} *"),
                            decl_line: None,
                        });
                        scope.hover_locals.push(ScopeLocal {
                            name: member.label.clone(),
                            class: Some(class_name.clone()),
                            protocols: Vec::new(),
                            type_args: Vec::new(),
                            type_spelling: format!("{class_name} *"),
                            decl_line: None,
                        });
                    }
                }
            }
        }
        // Locals declared in the body, and the sends they contain.
        if let Some(body) = body {
            let mut walk = BodyWalk {
                locals: &mut scope.locals,
                hover_locals: &mut scope.hover_locals,
                sites: &mut self.sites,
                calls: &mut self.calls,
                type_hints: &mut self.type_hints,
                member_uses: &mut self.member_uses,
                line_offsets: self.line_offsets.clone(),
                layouts: &self.layouts,
                source_map,
                text: &text,
                chain_bases: HashSet::new(),
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
    pub fn send_receiver(&self, position: LspPosition) -> Option<ReceiverInfo> {
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
        self.receiver_from_type(&site.receiver_type, &site.receiver_protocols, &site.receiver_type_args)
    }

    /// A receiver the checker typed but nobody named: its type is the answer,
    /// and an `id<P>` type is a protocol, not a class.
    fn receiver_from_type(
        &self,
        receiver_type: &Option<String>,
        receiver_protocols: &[String],
        receiver_type_args: &[String],
    ) -> Option<ReceiverInfo> {
        if receiver_type.is_some() || !receiver_protocols.is_empty() {
            Some(ReceiverInfo {
                class: receiver_type.clone(),
                protocols: receiver_protocols.to_vec(),
                is_class_receiver: false,
                type_args: receiver_type_args.to_vec(),
            })
        } else {
            None
        }
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
            let Some(class) = receiver.class else {
                // An `id<P>` receiver has no class, so no declared signature to
                // show: signature help is class-member-shaped, and a protocol's
                // selectors carry no parameter list.
                return None;
            };
            let signature = self.method_signature_in(&class, &site.selector)?;
            // Several declarations of one selector can differ (a category
            // overriding it); the first is the one to show, and every candidate
            // is reported so the editor can offer the rest.
            let mut signatures = vec![signature];
            signatures.extend(
                self.method_signatures_in(&class, &site.selector)
                    .into_iter()
                    .skip(1),
            );
            // A concrete receiver instantiates its class: the parameters in
            // the signature it is filling in are the receiver's arguments.
            let type_params = self
                .classes
                .get(&class)
                .map(|info| info.type_params.clone())
                .unwrap_or_default();
            if !receiver.type_args.is_empty() && !type_params.is_empty() {
                signatures = signatures
                    .into_iter()
                    .map(|signature| substitute_signature(&signature, &type_params, &receiver.type_args))
                    .collect();
            }
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

    /// The parameter-name inlays for every free-function call this index can
    /// type: each argument is labeled with the declared name of the parameter
    /// it fills in (clangd's gray, hover-less parameter hints).
    ///
    /// Message sends get none by construction — they live in `sites`, not
    /// `calls`, and a selector already names its arguments. A call no
    /// declaration describes (the document's own table first, then the
    /// system-header table) gets none: a missing hint is acceptable, a wrong
    /// label is not.
    pub fn inlay_hints(&self, c_table: &crate::cheader::CSignatures) -> Vec<InlayHint> {
        let mut hints: Vec<InlayHint> = Vec::new();
        for call in &self.calls {
            let parameter_names: Vec<Option<String>> = match self.functions.get(&call.name) {
                Some(signature) => signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter_name(parameter))
                    .collect(),
                None => match c_table.decl(&call.name) {
                    Some((crate::cheader::DeclKind::Function, decl)) => decl
                        .params
                        .iter()
                        .map(|parameter| parameter_name(parameter))
                        .collect(),
                    _ => continue,
                },
            };
            for (arg, name) in call.args.iter().zip(&parameter_names) {
                if let Some(name) = name {
                    hints.push(InlayHint {
                        position: arg.start,
                        label: format!("{name}: "),
                        kind: 2,
                    });
                }
            }
        }
        // Variable-use type hints, collected on the same analysis. Source order
        // so the array reads the way the file does.
        hints.extend(self.type_hints.iter().cloned());
        hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
        hints
    }

    /// The receiver class of a send site, without the "is the cursor in an
    /// argument" rule: signature help *wants* the argument positions.
    fn receiver_for_site(&self, site: &SendSite, position: LspPosition) -> Option<ReceiverInfo> {
        if let Some(name) = &site.receiver_name {
            if let Some(resolved) = self.receiver_class(name, position) {
                return Some(resolved);
            }
        }
        self.receiver_from_type(&site.receiver_type, &site.receiver_protocols, &site.receiver_type_args)
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
    pub fn receiver_class(&self, name: &str, position: LspPosition) -> Option<ReceiverInfo> {
        let scope = self.scope_at(position);
        if name == "self" {
            let Some(class) = scope.and_then(|scope| scope.self_type.clone()) else {
                return None;
            };
            // `self` is the generic class itself: its "arguments" are the
            // declared parameters, so the substitution is the identity.
            let type_args = self
                .classes
                .get(&class)
                .map(|info| info.type_params.clone())
                .unwrap_or_default();
            return Some(ReceiverInfo {
                class: Some(class),
                protocols: Vec::new(),
                is_class_receiver: false,
                type_args,
            });
        }
        if !is_identifier(name) {
            return None;
        }
        if let Some(scope) = scope {
            // Later declarations win (block-level shadowing flattened).
            if let Some(local) = scope.locals.iter().rev().find(|local| local.name == name) {
                return Some(ReceiverInfo {
                    class: local.class.clone(),
                    protocols: local.protocols.clone(),
                    is_class_receiver: false,
                    type_args: local.type_args.clone(),
                });
            }
        }
        self.classes.contains_key(name).then(|| ReceiverInfo {
            class: Some(name.to_string()),
            protocols: Vec::new(),
            is_class_receiver: true,
            type_args: Vec::new(),
        })
    }

    /// Members of `class` (walking up the ancestor chain) and of the
    /// `protocols` an `id` receiver conforms to, filtered to what a receiver of
    /// that kind can call (class methods for a class receiver, instance members
    /// for an object) and by `prefix`. A concrete class was collected first, so
    /// it wins on a label conflict with a protocol member.
    pub fn member_completion(
        &self,
        class: Option<&str>,
        protocols: &[String],
        class_receiver: bool,
        prefix: &str,
    ) -> Vec<Completion> {
        let mut members: Vec<MemberEntry> = Vec::new();
        let mut current = class.map(str::to_string);
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
        for protocol in protocols {
            if let Some(protocol_members) = self.protocols.get(protocol) {
                for member in protocol_members {
                    if member.is_class_method == class_receiver
                        && member.label.starts_with(prefix)
                        && !members.iter().any(|existing| existing.label == member.label)
                    {
                        members.push(member.clone());
                    }
                }
            }
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
        let range = source_map.to_lsp_range(&span, &text);

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
/// Source line (1-based) a raw AST line maps to. Scopes only exist for
/// same-file declarations, so the provenance is the document's own line.
fn mapped_line(source_map: &SourceMap, line: usize) -> u32 {
    let (_, real_line) = source_map.locate(line);
    real_line as u32
}

/// Readable type spelling for the hover card: keyword spelling, pointer
/// stars, struct/union/enum tags, generic arguments, and const/volatile
/// qualifiers where C puts them. Mirrors clangd's terse rendering — this is
/// what the editor shows as the type, not what codegen emits.
/// `__auto_type` — GNU C's inference marker (the EH desugar's hoisted temp).
/// The declared type is a placeholder; the initializer's type is the real one.
fn is_auto_type(ty: &AstType) -> bool {
    ty.prim == TypePrim::Named && ty.name.as_deref() == Some("__auto_type")
}

/// Byte offset of each line's start (line 1 at offset 0).
fn line_offsets(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// One line's text (1-based line) in O(1). `text.lines().nth(n)` is O(n) per
/// call, which turned the whole build quadratic once a hint was anchored per
/// declaration.
fn line_slice<'a>(text: &'a str, offsets: &[usize], line: usize) -> Option<&'a str> {
    let start = *offsets.get(line.checked_sub(1)?)?;
    let end = offsets.get(line).copied().unwrap_or(text.len());
    text.get(start..end)
}

/// The position just past `name` on `line` (1-based), searching from `from_col`
/// (1-based byte column): the first whole-word match. Where a type inlay that
/// sits after a declarator/parameter name is anchored. The span is handed to
/// the source map, so the one position projection stays in `to_lsp_range`.
fn name_end_after(
    text: &str,
    offsets: &[usize],
    source_map: &SourceMap,
    line: usize,
    from_col: usize,
    name: &str,
) -> Option<LspPosition> {
    // `line` is the AST's flattened line (post-`#import` expansion): searching
    // the editor text on the raw number read a *different* line and anchored
    // hints mid-identifier (`np_u12⟨ -> T⟩8` — the user's screenshot). Project
    // it to the document's own line first; columns survive the projection,
    // since an import replaces whole lines. The span below still passes the
    // flattened line to `span_from_node`, which owns the same mapping.
    let (_, real_line) = source_map.locate(line);
    let line_text = line_slice(text, offsets, real_line as usize)?;
    let bytes = line_text.as_bytes();
    let mut search = from_col.saturating_sub(1);
    while let Some(rel) = line_text.get(search..)?.find(name) {
        let start = search + rel;
        let end = start + name.len();
        let before_ok = start == 0
            || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let after_ok = end >= line_text.len()
            || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
        if before_ok && after_ok {
            let span = source_map.span_from_node(
                (line as u32, (end + 1) as u32),
                (line as u32, (end + 1) as u32),
            );
            return Some(source_map.to_lsp_range(&span, text).start);
        }
        search = end;
    }
    None
}

/// The position just past a function/method's parameter list on its own line —
/// where a return-type inlay sits (`(int a) -> int`). The `(` is the first one
/// after the name; its matching `)` closes the list.
/// The aggregate name behind a spelled type: `np_u128 *`, `struct Point` and
/// `const Foo` all name the same layout. Anything else (a block type, a
/// function pointer, an array) is not an aggregate — `None`, so callers stay
/// silent instead of resolving a member against the wrong table.
fn strip_to_aggregate_name(spelling: &str) -> Option<String> {
    let mut name = spelling.trim();
    loop {
        let mut changed = false;
        for keyword in ["const ", "volatile ", "struct ", "union ", "enum "] {
            if let Some(rest) = name.strip_prefix(keyword) {
                name = rest.trim();
                changed = true;
            }
        }
        if let Some(rest) = name.strip_suffix('*') {
            name = rest.trim();
            changed = true;
        }
        if !changed {
            break;
        }
    }
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| name.to_string())
}

fn ast_type_spell(ty: &AstType) -> String {
    let mut s = String::new();
    if ty.is_const && !ty.is_pointer { s.push_str("const "); }
    match ty.prim {
        TypePrim::Named => {
            let name = ty.name.clone().unwrap_or_default();
            if ty.is_struct {
                s.push_str(&format!("{} {}", ty.tag.keyword(), name));
            } else {
                s.push_str(&name);
            }
            if !ty.type_args.is_empty() {
                let args = ty.type_args.iter().map(ast_type_spell).collect::<Vec<_>>().join(", ");
                s.push_str(&format!("<{args}>"));
            }
        }
        TypePrim::Param => s.push_str(ty.name.as_deref().unwrap_or("T")),
        other => {
            // `unsigned int`, `unsigned char`, … — the qualifier rides on the
            // integer base as `is_unsigned`, not on a prim of its own; spell
            // it out (clangd shows `unsigned int`, never a bare `int`).
            if ty.is_unsigned && !matches!(other, TypePrim::Unsigned | TypePrim::Signed) {
                s.push_str("unsigned ");
            }
            s.push_str(match other {
                TypePrim::Void => "void",
                TypePrim::Char => "char",
                TypePrim::Short => "short",
                TypePrim::Int => "int",
                TypePrim::Long => "long",
                TypePrim::LongLong => "long long",
                TypePrim::Float => "float",
                TypePrim::Double => "double",
                TypePrim::Bool => "BOOL",
                TypePrim::Signed => "signed",
                TypePrim::Unsigned => "unsigned",
                TypePrim::Id => "id",
                TypePrim::Class => "Class",
                TypePrim::Sel => "SEL",
                TypePrim::Instancetype => "instancetype",
                _ => "int",
            });
        }
    }
    if ty.is_pointer {
        s.push_str(" *");
        if ty.is_const { s.push_str(" const"); }
    }
    s
}

/// Same spelling for a CST type (parameters spell their types in CST).
fn cst_type_spell(ty: &CstType) -> String {
    let mut s = String::new();
    if ty.is_const && !ty.is_pointer { s.push_str("const "); }
    if ty.is_volatile && !ty.is_pointer { s.push_str("volatile "); }
    match ty.prim {
        TypePrim::Named => {
            let name = ty.name.clone().unwrap_or_default();
            if ty.is_struct {
                s.push_str(&format!("{} {}", ty.tag.keyword(), name));
            } else {
                s.push_str(&name);
            }
            if !ty.type_args.is_empty() {
                let args = ty.type_args.iter().map(|t| cst_type_spell(t)).collect::<Vec<_>>().join(", ");
                s.push_str(&format!("<{args}>"));
            }
        }
        TypePrim::Param => s.push_str(ty.name.as_deref().unwrap_or("T")),
        other => s.push_str(match other {
            TypePrim::Void => "void",
            TypePrim::Char => "char",
            TypePrim::Short => "short",
            TypePrim::Int => "int",
            TypePrim::Long => "long",
            TypePrim::LongLong => "long long",
            TypePrim::Float => "float",
            TypePrim::Double => "double",
            TypePrim::Bool => "BOOL",
            TypePrim::Signed => "signed",
            TypePrim::Unsigned => "unsigned",
            TypePrim::Id => "id",
            TypePrim::Class => "Class",
            TypePrim::Sel => "SEL",
            TypePrim::Instancetype => "instancetype",
            _ => "int",
        }),
    }
    if ty.is_pointer {
        s.push_str(" *");
        if ty.is_const { s.push_str(" const"); }
    }
    s
}

fn ast_type_name(ty: &AstType) -> Option<String> {    match ty.prim {
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

/// The protocols an `id<P>` / `id<P1, P2>` type conforms to — the only
/// members an untyped `id` receiver can have.
fn ast_id_protocols(ty: &AstType) -> Vec<String> {
    (ty.prim == TypePrim::Id)
        .then(|| {
            if ty.protocol_refs.is_empty() {
                ty.protocol_ref.clone().into_iter().collect()
            } else {
                ty.protocol_refs.clone()
            }
        })
        .unwrap_or_default()
}

/// The CST-side twin of [`ast_id_protocols`], for parameter types, which stay
/// in CST form in the elaborated AST.
fn cst_id_protocols(ty: &CstType) -> Vec<String> {
    (ty.prim == TypePrim::Id).then(|| ty.protocols.clone()).unwrap_or_default()
}

/// The rendered names of a type's generic arguments, in order
/// (`NPArray<NPString>` → `["NPString"]`): the values a class's type
/// parameters are substituted with for this type's members.
fn ast_type_args(ty: &AstType) -> Vec<String> {
    ty.type_args.iter().filter_map(ast_type_name).collect()
}

/// The CST-side twin of [`ast_type_args`], for parameter types.
fn cst_type_args(ty: &CstType) -> Vec<String> {
    ty.type_args.iter().filter_map(cst_type_name).collect()
}

/// A signature with a receiver's concrete arguments substituted for the
/// class's type parameters, in the label and in every parameter label (the
/// parameters are substrings of the label, but LSP matches on them
/// separately).
fn substitute_signature(signature: &Signature, type_params: &[String], type_args: &[String]) -> Signature {
    Signature {
        label: substitute_generic_params(&signature.label, type_params, type_args),
        parameters: signature
            .parameters
            .iter()
            .map(|parameter| substitute_generic_params(parameter, type_params, type_args))
            .collect(),
    }
}

/// Each whole-word occurrence of a class's type parameter in a rendered
/// signature, rewritten as the receiver's corresponding argument. A
/// parameter with no corresponding argument (the receiver was declared with
/// fewer arguments than the class declares) stays as written: showing the
/// parameter beats showing nothing.
fn substitute_generic_params(text: &str, type_params: &[String], type_args: &[String]) -> String {
    if type_args.is_empty() {
        return text.to_string();
    }
    let mut result = text.to_string();
    for (param, arg) in type_params.iter().zip(type_args.iter()) {
        if param == arg {
            continue;
        }
        result = replace_whole_word(&result, param, arg);
    }
    result
}

/// Whole-word occurrences of `from` rewritten as `to`; an occurrence
/// identifier-adjacent is left alone, so `T` never rewrites `Trait`.
fn replace_whole_word(text: &str, from: &str, to: &str) -> String {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let chars: Vec<char> = text.chars().collect();
    let from_chars: Vec<char> = from.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        let hit = i + from_chars.len() <= chars.len()
            && chars[i..i + from_chars.len()] == from_chars[..]
            && (i == 0 || !is_ident(chars[i - 1]))
            && (i + from_chars.len() == chars.len() || !is_ident(chars[i + from_chars.len()]));
        if hit {
            out.push_str(to);
            i += from_chars.len();
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
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

/// The declared name inside a parameter spelling: `int value` → `value`,
/// `const char *restrict` → `restrict`. An unnamed spelling — `int`,
/// `char *`, or a type whose last word is an attribute (`__restrict`,
/// `_Nullable`, …) — has no name to show, and labeling an argument with a
/// type word would be a wrong hint: a miss is acceptable, a misspelled name
/// is not.
fn parameter_name(spelling: &str) -> Option<String> {
    let words: Vec<&str> = spelling.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }
    // The declarator may hug the name (`char *fmt`, `NPObject *obj`, `char **argv`):
    // the star(s) belong to the type, the identifier after them is the name.
    let last = words[words.len() - 1].trim_start_matches('*');
    if last.is_empty() {
        return None;
    }
    let is_identifier = last
        .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && last
            .chars()
            .all(|c: char| c.is_ascii_alphanumeric() || c == '_');
    if !is_identifier {
        return None;
    }
    const NOT_NAMES: &[&str] = &[
        "const", "volatile", "restrict", "__restrict", "unsigned", "signed",
        "_Nullable", "_Nonnull", "_Null_unspecified", "_Null_resettable",
    ];
    if NOT_NAMES.contains(&last) {
        return None;
    }
    Some(last.to_string())
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
    locals: &'a mut Vec<ScopeLocal>,
    /// The hover surface: every visible name with its spelled type, no
    /// receiver-shaped filter (a scalar local still deserves a hover card).
    hover_locals: &'a mut Vec<ScopeLocal>,
    sites: &'a mut Vec<SendSite>,
    calls: &'a mut Vec<CallSite>,
    /// Use-site type inlays, collected here (see [`SymbolIndex::type_hints`]).
    type_hints: &'a mut Vec<InlayHint>,
    /// Member accesses whose host aggregate resolved, in source order: the
    /// hover surface for a field *use* away from its declaration.
    member_uses: &'a mut Vec<MemberUse>,
    /// Line-start offsets, shared with the index (see [`SymbolIndex::line_offsets`]).
    line_offsets: std::sync::Arc<Vec<usize>>,
    /// Struct/union layouts, for the field names a designator inlay spells.
    layouts: &'a HashMap<String, jeti_layout::StructLayout>,
    source_map: &'a SourceMap,
    text: &'a str,
    /// Expressions that must stay hint-free: the direct base of a member
    /// access (`r.hi`), a subscript (`arr[i]`) or a prefix operator (`*p`). A
    /// use-site hint after the base splits the chain visually
    /// (`r⟨ -> T⟩.hi`) and says nothing the declaration doesn't already spell.
    chain_bases: HashSet<(usize, usize)>,
}

impl BodyWalk<'_> {
    fn range(&self, line: usize, col: usize, end_line: usize, end_col: usize) -> Option<LspRange> {
        let span = self.source_map.span_from_node(
            (line as u32, col as u32),
            (end_line as u32, end_col as u32),
        );
        (!span.is_synthetic()).then(|| self.source_map.to_lsp_range(&span, self.text))
    }

    fn expr_range(&self, expr: &AstExpr) -> Option<LspRange> {
        self.range(expr.line, expr.col, expr.end_line, expr.end_col)
    }

    /// Remember a direct chain base — the `r` of `r.hi`, the `arr` of
    /// `arr[i]`, the `p` of `*p`, a send's named receiver, the `a.b` of
    /// `a.b.c` — so no hint is inserted between the base and its operator.
    fn record_chain_base(&mut self, expr: &AstExpr) {
        self.chain_bases.insert((expr.line, expr.col));
    }

    /// A member access whose host aggregate resolved: a [`MemberUse`] so hover
    /// answers away from the declaration, plus — only for the outermost ring of
    /// a chain — the clangd-style use-site type hint. An inner ring (`a.b` in
    /// `a.b.c`, the base of `x.arr[i]`) says nothing: the hint belongs after the
    /// whole chain, never inside it.
    fn member_access(
        &mut self,
        expr: &AstExpr,
        member: Option<&str>,
        cls: Option<&str>,
        obj: &AstExpr,
    ) {
        let Some(member) = member.filter(|m| !m.is_empty()) else {
            return;
        };
        let Some(host) = self.member_host(cls, obj) else {
            return;
        };
        let Some(layout) = self.layouts.get(&host) else {
            return;
        };
        let Some(field) = layout.fields.iter().find(|f| f.name == member) else {
            return;
        };
        let is_union = layout.is_union;
        let spelling = field.type_spelling.trim().to_string();
        let Some(range) = self.expr_range(expr) else {
            return;
        };
        self.member_uses.push(MemberUse {
            range,
            member: member.to_string(),
            struct_name: host,
            is_union,
            type_spelling: spelling.clone(),
        });
        if !spelling.is_empty() && !self.chain_bases.contains(&(expr.line, expr.col)) {
            self.type_hints.push(InlayHint {
                position: range.end,
                label: format!(" -> {spelling}"),
                kind: 1,
            });
        }
    }

    /// The aggregate a member access's base has, when the base's type is
    /// spelled out (a local, an ivar, another member access). The elaborator
    /// resolves a class member's class itself, which beats re-deriving it.
    fn member_host(&self, cls: Option<&str>, obj: &AstExpr) -> Option<String> {
        match cls {
            Some(cls) => Some(cls.to_string()),
            None => self.expr_aggregate(obj),
        }
    }

    /// The aggregate name behind an expression's type: `np_u128 *` and
    /// `struct Point` name layouts too. Anything the walk cannot name (a send
    /// result, an array element) stays unresolved — the member then says
    /// nothing rather than guessing.
    fn expr_aggregate(&self, expr: &AstExpr) -> Option<String> {
        if let AstExprData::IvarRef { cls: Some(cls), .. }
        | AstExprData::PropRef { cls: Some(cls), .. } = &expr.data
        {
            return Some(cls.clone());
        }
        strip_to_aggregate_name(&self.expr_type_spelling(expr)?)
    }

    /// The spelled type of an expression the walk can name: locals answer from
    /// the declaration table, member accesses chain through the layouts table.
    fn expr_type_spelling(&self, expr: &AstExpr) -> Option<String> {
        match &expr.data {
            AstExprData::VarRef { name, .. } => self
                .hover_locals
                .iter()
                .find(|l| &l.name == name)
                .map(|l| l.type_spelling.clone())
                .filter(|s| !s.is_empty()),
            AstExprData::IvarRef { ivar, cls, obj } => {
                let host = self.member_host(cls.as_deref(), obj)?;
                self.field_spelling(&host, ivar.as_deref()?)
            }
            AstExprData::PropRef { prop, name, cls, obj, .. } => {
                let host = self.member_host(cls.as_deref(), obj)?;
                self.field_spelling(&host, prop.as_deref().unwrap_or(name.as_str()))
            }
            AstExprData::Paren(inner) | AstExprData::Boxed(inner) => self.expr_type_spelling(inner),
            // `(*p).x` / `p->x`: the pointer's spelling already names the
            // aggregate once the stars are stripped.
            AstExprData::Unary { operand, .. } => self.expr_type_spelling(operand),
            _ => None,
        }
    }

    fn field_spelling(&self, host: &str, member: &str) -> Option<String> {
        self.layouts
            .get(host)?
            .fields
            .iter()
            .find(|f| f.name == member)
            .map(|f| f.type_spelling.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// The position just past the declarator `name` on the declaration's first
    /// line — where a definition-site type inlay sits.
    fn declarator_end(&self, decl: &AstDecl, name: &str) -> Option<LspPosition> {
        name_end_after(self.text, &self.line_offsets, self.source_map, decl.line, decl.col, name)
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
                let class = ast_type_name(ty);
                let protocols = ast_id_protocols(ty);
                let type_args = ast_type_args(ty);
                // `__auto_type x = expr;` (GNU C's inference marker, the shape
                // the EH desugar emits for `__jeti_eh_tmp_N`): the declared
                // type is a placeholder. The real type is the initializer's,
                // stamped by the checker — record and show that, never the
                // marker.
                let inferred = if is_auto_type(ty) {
                    init.as_deref().and_then(|i| i.expr_type.as_deref())
                } else {
                    None
                };
                let spelling = match inferred {
                    Some(inferred_ty) => ast_type_spell(inferred_ty),
                    None => ast_type_spell(ty),
                };
                let line = Some(mapped_line(self.source_map, declaration.line));
                // At the definition the inferred type is the whole point:
                // `__auto_type tmp = …;` reads `tmp: T = …`.
                if let (Some(inferred_ty), Some(position)) =
                    (inferred, self.declarator_end(declaration, name))
                {
                    self.type_hints.push(InlayHint {
                        position,
                        label: format!(" -> {}", ast_type_spell(inferred_ty)),
                        kind: 1,
                    });
                }
                if class.is_some() || !protocols.is_empty() {
                    self.locals.push(ScopeLocal {
                        name: name.clone(), class, protocols, type_args,
                        type_spelling: spelling.clone(), decl_line: line,
                    });
                }
                self.hover_locals.push(ScopeLocal {
                    name: name.clone(),
                    class: ast_type_name(ty),
                    protocols: ast_id_protocols(ty),
                    type_args: ast_type_args(ty),
                    type_spelling: spelling,
                    decl_line: line,
                });
            }
            // Designator inlays: a positional initializer list spells no member
            // names, so show them — `int a[] = {1, 2}` reads `{[0]=1, [1]=2}`
            // and `Point p = {1, 2}` reads `{.x=1, .y=2}`.
            //
            // An element that already spells its own designator (`.x = 1`) gets
            // NOTHING: the source names that member right there, so a hint would
            // duplicate it. The positional elements around it still do, and they
            // are labelled by where C actually puts them — the member after the
            // LAST designator (wrapping for a struct, the next index for an
            // array) — never by their position in the list.
            if let (Some(ty), Some(init_expr)) = (var_type.as_deref(), init.as_deref()) {
                if let AstExprData::InitList(elems) | AstExprData::ArrayLit(elems) = &init_expr.data {
                    // Field names, copied out so the loop below does not hold a
                    // borrow of `self.layouts` across the hint pushes.
                    let fields: Vec<String> = if ty.is_struct {
                        ty.name
                            .as_deref()
                            .and_then(|n| self.layouts.get(n))
                            .map(|layout| layout.fields.iter().map(|f| f.name.clone()).collect())
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    // `None` = the next position can no longer be derived (an
                    // index designator this walk cannot evaluate, or more
                    // elements than the layout has fields): emit nothing for the
                    // rest rather than guess — a missing hint is acceptable, a
                    // wrong label is not.
                    let mut cursor: Option<usize> = Some(0);
                    for elem in elems.iter() {
                        if let AstExprData::DesignatedInit { designators, .. } = &elem.data {
                            cursor = designators.last().and_then(|d| match d {
                                AstDesignator::Member(name) => fields
                                    .iter()
                                    .position(|f| f == name)
                                    .map(|i| (i + 1) % fields.len()),
                                AstDesignator::Index(_) => None,
                            });
                            continue;
                        }
                        let Some(i) = cursor else { continue };
                        let designator = if ty.is_array {
                            format!("[{i}]=")
                        } else if ty.is_struct {
                            match fields.get(i) {
                                Some(name) => format!(".{name}="),
                                None => {
                                    cursor = None;
                                    continue;
                                }
                            }
                        } else {
                            continue;
                        };
                        if let Some(range) = self.expr_range(elem) {
                            self.type_hints.push(InlayHint {
                                position: range.start,
                                label: designator,
                                kind: 1,
                            });
                        }
                        cursor = Some(i + 1);
                    }
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
            let class = cst_type_name(ty);
            let protocols = cst_id_protocols(ty);
            let type_args = cst_type_args(ty);
            if class.is_some() || !protocols.is_empty() {
                self.locals.push(ScopeLocal {
                    name: name.clone(), class: class.clone(),
                    protocols: protocols.clone(), type_args: type_args.clone(),
                    type_spelling: cst_type_spell(ty), decl_line: None,
                });
            }
            self.hover_locals.push(ScopeLocal {
                name: name.clone(), class, protocols, type_args,
                type_spelling: cst_type_spell(ty), decl_line: None,
            });
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
                        receiver_protocols: receiver
                            .expr_type
                            .as_deref()
                            .map(ast_id_protocols)
                            .unwrap_or_default(),
                        receiver_type_args: receiver
                            .expr_type
                            .as_deref()
                            .map(ast_type_args)
                            .unwrap_or_default(),
                        selector: selector.clone(),
                        args: args.iter().filter_map(|arg| self.expr_range(arg)).collect(),
                    });
                }
                self.record_chain_base(receiver);
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
            AstExprData::IvarRef { ivar, cls, obj } => {
                self.member_access(expr, ivar.as_deref(), cls.as_deref(), obj);
                self.record_chain_base(obj);
                self.expr(obj);
            }
            AstExprData::PropRef { prop, name, cls, obj, .. } => {
                let member = prop.as_deref().unwrap_or(name.as_str());
                self.member_access(expr, Some(member), cls.as_deref(), obj);
                self.record_chain_base(obj);
                self.expr(obj);
            }
            AstExprData::Unary { operand, is_postfix, .. } => {
                // An operator glues to its operand, prefix (`*carry`, `&x`) and
                // postfix (`i++`) alike: a hint inserted between them splits the
                // pair (`i« -> int»++`). Every tool that ships expression type
                // hints anchors them at the END of the expression instead —
                // gopls uses `Position: e.End()`, rust-analyzer moved its hints
                // from before the item to after it — so the operand is recorded
                // as a chain base and the hint is emitted after the whole
                // expression below.
                self.record_chain_base(operand);
                self.expr(operand);
                // No "is this expression itself a chain base" guard here: a
                // postfix expression STARTS where its operand starts (the `++`
                // comes after), so `chain_bases` would always look like a hit.
                // The anchor below is the end of the whole expression either
                // way, which is also the end of any enclosing prefix operator.
                if *is_postfix {
                    let spelling = self.expr_type_spelling(operand).unwrap_or_default();
                    if !spelling.is_empty() {
                        if let Some(range) = self.expr_range(expr) {
                            self.type_hints.push(InlayHint {
                                position: range.end,
                                label: format!(" -> {spelling}"),
                                kind: 1,
                            });
                        }
                    }
                }
            }
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
                self.record_chain_base(object);
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
                    let class = ast_type_name(ty);
                    let protocols = ast_id_protocols(ty);
                    let type_args = ast_type_args(ty);
                    if class.is_some() || !protocols.is_empty() {
                        self.locals.push(ScopeLocal {
                            name: name.clone(), class, protocols, type_args,
                            type_spelling: String::new(), decl_line: None,
                        });
                    }
                }
                if let Some(body) = body {
                    self.stmt(body);
                }
            }
            // Statement expression `({ ... })`: its body declarations and the
            // calls/sends inside must be indexed like any block body.
            AstExprData::StmtExpr(body) => {
                for s in body {
                    self.stmt(s);
                }
            }
            // A variable *use*: show its declared type as a gray `: T`. The
            // declaration's own name is a decl field, never a VarRef, so this
            // cannot duplicate the definition.
            AstExprData::VarRef { name, .. } => {
                if let Some(local) = self.hover_locals.iter().find(|l| &l.name == name) {
                    // A chain base (`r` in `r.hi`) stays hint-free: the hint
                    // would land inside the chain and read as the *chain
                    // result's* type.
                    if !local.type_spelling.is_empty()
                        && !self.chain_bases.contains(&(expr.line, expr.col))
                    {
                        if let Some(range) = self.expr_range(expr) {
                            self.type_hints.push(InlayHint {
                                position: range.end,
                                label: format!(" -> {}", local.type_spelling),
                                kind: 1,
                            });
                        }
                    }
                }
            }
            AstExprData::Int(_)
            | AstExprData::Float(_)
            | AstExprData::FloatRaw(_)
            | AstExprData::Char(_)
            | AstExprData::String(_)
            | AstExprData::AtString(_)
            | AstExprData::Bool(_)
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
/// Per-byte mask of one document: `true` where the byte is code, `false`
/// inside a comment, a string, a character literal, or a preprocessor line —
/// the places a reference to a name cannot occur. References are found in the
/// text, so the same pass that reads the text has to say where it is not code.
fn code_mask(text: &str) -> Vec<bool> {
    let bytes = text.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // String or character literal (`@"` is the same literal; the `@` stays
        // code, it cannot start a name anyway).
        if b == b'"' || b == b'\'' {
            let quote = b;
            mask[i] = false;
            i += 1;
            while i < bytes.len() {
                mask[i] = false;
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    mask[i + 1] = false;
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                // An unterminated literal ends where the lexer ends it: at the
                // newline (the parser reports the error).
                if bytes[i] == b'\n' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // Line comment: through the newline, which stays code.
            while i < bytes.len() && bytes[i] != b'\n' {
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // Block comment: through the closing `*/`, which is not code either.
            mask[i] = false;
            mask[i + 1] = false;
            i += 2;
            while i < bytes.len() {
                if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    mask[i] = false;
                    mask[i + 1] = false;
                    i += 2;
                    break;
                }
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    // Preprocessor lines name the file, not a symbol: a header path's
    // components are tokens that could fake a reference, so the whole line is
    // not code.
    let mut line_start = 0usize;
    let mut directive_lines: Vec<(usize, usize)> = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            if is_directive_line(bytes, line_start, i) {
                directive_lines.push((line_start, i));
            }
            line_start = i + 1;
        }
    }
    if is_directive_line(bytes, line_start, bytes.len()) {
        directive_lines.push((line_start, bytes.len()));
    }
    for (start, end) in directive_lines {
        for k in start..end {
            mask[k] = false;
        }
    }
    mask
}

/// A preprocessor line: first non-blank character is `#`.
fn is_directive_line(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut j = start;
    while j < end && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    j < end && bytes[j] == b'#'
}

/// The maximal name tokens of one document at code positions: runs of
/// identifier characters, extended across `::` (a qualified name is one name),
/// with their byte extents. Selectors are read without their trailing colon —
/// a colon is not an identifier character — and callers asking about a method
/// retry with the colon appended.
fn name_tokens(text: &str, mask: &[bool]) -> Vec<(String, usize, usize)> {
    let bytes = text.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if mask[i] && is_ident(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            // A qualified name extends across `::` to the next identifier.
            while i + 1 < bytes.len() && bytes[i] == b':' && bytes[i + 1] == b':' {
                if i + 2 < bytes.len() && is_ident(bytes[i + 2]) {
                    i += 2;
                    while i < bytes.len() && is_ident(bytes[i]) {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            let end = i;
            tokens.push((text[start..end].to_string(), start, end));
        } else {
            i += 1;
        }
    }
    tokens
}

/// A byte offset as an `LspPosition`: the one crossing from raw offsets to
/// LSP's line + UTF-16 character coordinates (the same conversion
/// `SourceSpan::to_lsp_range` makes on the way out).
fn offset_to_position(text: &str, offset: usize) -> LspPosition {
    let offset = offset.min(text.len());
    let line = text[..offset].matches('\n').count();
    let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = text[line_start..offset].encode_utf16().count();
    LspPosition { line: line as u32, character: character as u32 }
}

/// The byte range one rename replaces in one occurrence: the last segment of
/// a qualified name, the whole bare token, a selector's trailing colon
/// included (the spelling the text has, when it is there).
fn rename_segment(token: &str, start: usize, end: usize, is_selector: bool, text: &str) -> (usize, usize) {
    let seg_start = token.rfind("::").map(|i| start + i + 2).unwrap_or(start);
    let mut seg_end = end;
    if is_selector && text.as_bytes().get(seg_end) == Some(&b':') {
        seg_end += 1;
    }
    (seg_start, seg_end)
}

/// Is `col` (a BYTE index into `line`) inside the attribute list of an
/// `@property ( ... )` declaration on that line? Jeti property declarations
/// are single-line, so a line-local scan is exact.
fn inside_property_attrs(line: &str, col: usize) -> bool {
    let Some(prop) = line.find("@property") else {
        return false;
    };
    let rest = &line[prop..];
    let Some(open) = rest.find('(') else {
        return false;
    };
    match rest[open..].find(')') {
        Some(close_rel) => col > prop + open && col < prop + open + close_rel,
        // Unterminated list: the attribute position runs to end of line.
        None => col > prop + open,
    }
}

/// `inside_property_attrs` over a full document + LSP position.
fn position_inside_property_attrs(text: &str, position: LspPosition) -> bool {
    let Some(offset) = jeti_cst::source_map::SourceFile::lsp_to_offset(text, position) else {
        return false;
    };
    let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[offset..].find('\n').map(|i| offset + i).unwrap_or(text.len());
    inside_property_attrs(&text[line_start..line_end], offset - line_start)
}

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

/// Words the semantic-token scan never colors: the grammar already spells
/// keywords and builtin types, and semantic tokens would override its colors.
/// The soft keywords `async`/`await` color as keywords only where the parser
/// reads them as keywords — the same raw-source judgment, mirrored from
/// `Parser::at_async_modifier` / `at_await_operator`, so a user identifier
/// spelled `async` anywhere else keeps its identifier color (C superset iron
/// law). `rest` is the source right after the word.
fn soft_keyword_token(word: &str, rest: &str) -> Option<u8> {
    let after = rest.trim_start();
    match word {
        "async" => {
            let c = after.chars().next()?;
            // `async ^…` block literal, or `async` + a type start.
            if c == '^' {
                return Some(9);
            }
            for kw in [
                "void", "int", "char", "float", "double", "long", "short",
                "signed", "unsigned", "bool", "struct", "union", "enum",
                "const", "volatile", "id", "instancetype",
            ] {
                if after.starts_with(kw) {
                    let after_kw = &after[kw.len()..];
                    if !after_kw
                        .chars()
                        .next()
                        .map_or(false, |a| a.is_alphanumeric() || a == '_')
                    {
                        return Some(9);
                    }
                }
            }
            // Typename identifier: `async Result *f(void)`.
            (c.is_alphabetic() || c == '_').then_some(9)
        }
        "await" => {
            let c = after.chars().next()?;
            // Expression-prefix suspension operator: any unary-expression
            // start EXCEPT the bare `+ - * &` operators (`await + 1` keeps
            // `await` an identifier; `++`/`--` are in the start set).
            let ok = match c {
                '(' | '[' | '^' | '!' | '~' | '@' | '"' | '\'' => true,
                '0'..='9' => true,
                '+' => after[1..].starts_with('+'),
                '-' => after[1..].starts_with('-'),
                c => c.is_alphabetic() || c == '_',
            };
            ok.then_some(9)
        }
        _ => None,
    }
}

/// A scanned word matches a member when it equals any colon-separated part
/// of the stored selector: `addObject:` answers to `addObject`,
/// `setObject:atIndex:` to both `setObject` and `atIndex` — clangd colors
/// every selector part, and a whole-word scan never sees the colons.
pub(crate) fn selector_part_matches(word: &str, label: &str) -> bool {
    label.split(':').any(|part| part == word)
}

/// Modifier bits, matching the legend order in the capabilities
/// (`["static", "defaultLibrary"]`). clangd marks class methods `static` and
/// system-header surface `defaultLibrary`; themes style the combinations
/// apart (the italic/underline distinctions), so the server must carry them.
const MOD_STATIC: u8 = 1;
const MOD_DEFAULT_LIBRARY: u8 = 2;

const BUILTIN_AND_KEYWORDS: &[&str] = &[
    "asm", "auto", "bool", "BOOL", "break", "case", "char", "Class", "const", "continue",
    "default", "do", "double", "else", "enum", "extern", "float", "for", "goto", "id",
    "if", "inline", "instancetype", "int", "long", "nil", "NO", "register", "restrict",
    "return", "self", "SEL", "short", "signed", "sizeof", "static", "struct", "super",
    "switch", "typedef", "union", "unsigned", "void", "volatile", "while", "YES", "_Bool",
    "__attribute__", "_Nullable", "_Nonnull", "_Complex",
];

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
    fn a_postfix_operator_keeps_its_hint_after_the_whole_expression() {
        // `i++` is a glued pair like `*p`, so the hint goes AFTER it — never
        // between `i` and `++` (`i« -> int»++`). That trailing anchor is what
        // every tool shipping expression type hints does: gopls uses
        // `Position: e.End()`, rust-analyzer moved its hints from before the
        // item to after it. A prefix operator's operand stays unlabelled
        // entirely (`-i`, `!i`, `*p`, `++i`), which is why only the lines below
        // carry a hint.
        let src = "\
int f(int *p, int i, int j) {
    i++;
    ++i;
    j = i + j;
    int v = *p;
    v = -i;
    return v;
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints: Vec<(u32, u32, String)> = index
            .inlay_hints(&crate::cheader::CSignatures::default())
            .into_iter()
            .filter(|h| h.kind == 1)
            .map(|h| (h.position.line, h.position.character, h.label))
            .collect();
        assert_eq!(
            hints,
            vec![
                (1, 7, " -> int".to_string()),  // `i++` — after the `++`
                (3, 5, " -> int".to_string()),  // `j =`
                (3, 9, " -> int".to_string()),  // `i` in `i + j`
                (3, 13, " -> int".to_string()), // `j` in `i + j`
                (5, 5, " -> int".to_string()),  // `v =` (`-i` stays unlabelled)
                (6, 12, " -> int".to_string()), // `return v;`
            ],
            "{hints:?}"
        );
    }

    #[test]
    fn an_ivar_declaration_carries_no_hint_but_an_argument_use_does() {
        // The ivar's declaration already spells `int _value`, so it gets no
        // hint; the USE of the parameter `v` is what is worth labelling
        // (`_value = v;` reads `_value = v -> int;`). `_value` itself — the
        // declaration and both uses — stays unlabelled.
        let src = "\
@interface Holder : NPObject {
    int _value;
}
@end
@implementation Holder
- (void)setV:(int)v {
    _value = v;
    _value = _value + v;
}
@end
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let labels: Vec<String> = index
            .inlay_hints(&crate::cheader::CSignatures::default())
            .into_iter()
            .filter(|h| h.kind == 1)
            .map(|h| h.label)
            .collect();
        assert_eq!(labels, vec![" -> int", " -> int"], "{labels:?}");
    }

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

    /// Parameter hints only (kind 2). The type hints (kind 1) are exercised by
    /// their own tests; these cases are about argument labeling.
    fn parameter_hints(hints: Vec<InlayHint>) -> Vec<InlayHint> {
        hints.into_iter().filter(|h| h.kind == 2).collect()
    }

    #[test]
    fn variable_uses_carry_their_declared_type() {
        // At every *use* of a variable — never its own declaration — the type
        // is shown: `a = 8;` reads `a: int = 8;`, `i != 2345` reads
        // `i: long != 2345`.
        let src = "\
int f(void) {
    int a = 1;
    long i = 2;
    a = 8;
    if (i != 2345) { return a; }
    return 0;
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints: Vec<InlayHint> = index
            .inlay_hints(&crate::cheader::CSignatures::default())
            .into_iter()
            .filter(|h| h.kind == 1)
            .collect();
        let labels: Vec<&str> = hints.iter().map(|h| h.label.as_str()).collect();
        // The three uses only: the `int a` / `long i` declarations are not
        // uses, and the signature spells its own types (no declaration-site
        // hints).
        assert_eq!(labels, vec![" -> int", " -> long", " -> int"], "{hints:?}");
    }

    #[test]
    fn auto_type_variables_show_their_inferred_type() {
        // `__auto_type` (GNU C; the shape the EH desugar emits for its hoisted
        // `__jeti_eh_tmp_N`) declares no type — the initializer decides. Both
        // the definition and every use must show the inferred type, never the
        // `__auto_type` marker.
        let src = "\
int f(void) {
    __auto_type x = 5;
    return x + 1;
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints: Vec<InlayHint> = index
            .inlay_hints(&crate::cheader::CSignatures::default())
            .into_iter()
            .filter(|h| h.kind == 1)
            .collect();
        let labels: Vec<&str> = hints.iter().map(|h| h.label.as_str()).collect();
        // The `__auto_type` definition and the use — both inferred as `int`,
        // never the `__auto_type` marker. (The definition IS a declaration, but
        // the type is not spelled there at all, so the hint is the only way to
        // see it.)
        assert_eq!(labels, vec![" -> int", " -> int"], "{hints:?}");
    }

    #[test]
    fn signature_declarations_carry_no_type_hints() {
        // A declaration spells its own type, so repeating it after the name
        // (`int a` reading `a -> int`) or after the parameter list
        // (`(int a) -> int`) is noise — the same choice clangd's inlay hints
        // make. Only USE sites get a hint.
        let src = "int add(int a, int b) { return 0; }\n";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints: Vec<InlayHint> = index
            .inlay_hints(&crate::cheader::CSignatures::default())
            .into_iter()
            .filter(|h| h.kind == 1)
            .collect();
        assert!(hints.is_empty(), "{hints:?}");
    }

    #[test]
    fn positional_initializers_show_their_designators() {
        // A positional initializer spells no member names: an array reads
        // `{[0]=1, [1]=2}`, a struct `{.x=1, .y=2}` — the `[i]=` / `.field=`
        // parts are gray, non-editable hints.
        let src = "\
struct Point { int x; int y; };
int use(void) {
    int a[3] = {1, 2, 3};
    struct Point p = {4, 5};
    return a[0] + p.x;
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let labels: Vec<String> = index
            .inlay_hints(&crate::cheader::CSignatures::default())
            .into_iter()
            .map(|h| h.label)
            .filter(|l| l.ends_with('='))
            .collect();
        assert_eq!(labels, vec!["[0]=", "[1]=", "[2]=", ".x=", ".y="], "{labels:?}");
    }

    #[test]
    fn an_explicit_designator_is_not_repeated_by_a_hint() {
        // `.x = 1` already names its member in the source, so a hint there would
        // duplicate it — that element gets NOTHING. The positional element after
        // it still does, labelled by where C actually puts it (the member after
        // the last designator, wrapping for a struct), not by its index in the
        // list.
        let src = "\
struct FsPoint { int x; int y; };
int use(void) {
    struct FsPoint p1 = { .x = 1, 7 };
    struct FsPoint p2 = { .y = 1, 7 };
    return 0;
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let labels: Vec<String> = index
            .inlay_hints(&crate::cheader::CSignatures::default())
            .into_iter()
            .map(|h| h.label)
            .filter(|l| l.ends_with('='))
            .collect();
        // p1: `.x` is spelled, so only the trailing `7` is hinted — as `.y`.
        // p2: `.y` is spelled, and C wraps the next positional element back to
        // `x`.
        assert_eq!(labels, vec![".y=", ".x="], "{labels:?}");
    }

    #[test]
    fn parameter_inlays_label_the_arguments_of_a_known_call() {
        let src = "int add(int a, int b) { return a + b; } int use_it() { return add(1, 2); }";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints = parameter_hints(index.inlay_hints(&crate::cheader::CSignatures::default()));
        assert_eq!(hints.len(), 2, "{hints:?}");
        assert_eq!(hints[0].position, at(src, "add(1", 4));
        assert_eq!(hints[0].label, "a: ");
        assert_eq!(hints[1].position, at(src, "2);", 0));
        assert_eq!(hints[1].label, "b: ");
    }

    #[test]
    fn message_sends_contribute_no_parameter_inlays() {
        // The send's arguments sit under their own selector, which already
        // names them: only the free call in the same document is hinted.
        let src = "\
@interface Thing : jeti_root
@end
@implementation Thing
- (int)take:(int)one other:(int)two { return one + two; }
@end
int add(int a, int b) { return a + b; }
int main() {
    Thing *thing;
    [thing take:1 other:2];
    return add(1, 2);
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints = parameter_hints(index.inlay_hints(&crate::cheader::CSignatures::default()));
        // Multi-line document, so the line is computed, not assumed.
        let pos_of = |needle: &str, inside: usize| -> LspPosition {
            let offset = src.find(needle).expect("needle") + inside;
            let before = &src[..offset];
            let line = before.matches('\n').count() as u32;
            let column = (offset - before.rfind('\n').map_or(0, |i| i + 1)) as u32;
            LspPosition { line, character: column }
        };
        assert_eq!(hints.len(), 2, "{hints:?}");
        assert_eq!(hints[0].position, pos_of("add(1", 4));
        assert_eq!(hints[0].label, "a: ");
        assert_eq!(hints[1].position, pos_of("2);", 0));
        assert_eq!(hints[1].label, "b: ");
    }

    #[test]
    fn unknown_calls_and_unnamed_parameters_get_no_inlays() {
        // A call no declaration describes gets no hint at all — the index
        // would rather miss than label an argument with a guess. The named
        // call in the same document proves the pipeline ran.
        let src = "int known(int x) { return x; } int main() { return known(1) + mystery(3, 4); }";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints = parameter_hints(index.inlay_hints(&crate::cheader::CSignatures::default()));
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].position, at(src, "known(1)", 6));
        assert_eq!(hints[0].label, "x: ");

        // An unnamed parameter has no name to show, so its argument stays
        // bare (the named call proves the pipeline ran again).
        let src = "int sub(int, int) { return 0; } int known(int x) { return x; } int main() { return sub(1, 2) + known(3); }";
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints = parameter_hints(index.inlay_hints(&crate::cheader::CSignatures::default()));
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].position, at(src, "known(3)", 6));
        assert_eq!(hints[0].label, "x: ");
    }

    #[test]
    fn variadic_calls_hint_only_the_named_parameters() {
        let src = "int logv(const char *fmt, ...) { return 0; } int main() { return logv(\"%d\", 1, 2); }";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints = parameter_hints(index.inlay_hints(&crate::cheader::CSignatures::default()));
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].position, at(src, "logv(\"", 5));
        assert_eq!(hints[0].label, "fmt: ");
    }

    /// The chain base stays hint-free (that was #11's fix), but the member
    /// access itself must still say what it is — once, after the *outermost*
    /// ring (clangd's place). The host aggregate is a `typedef struct { … } T;`
    /// whose layout the plain collector does not key, so this also pins the
    /// synthesized entry.
    #[test]
    fn member_access_hints_land_after_the_whole_chain() {
        let src = "\
typedef struct { int hi; int lo; } np_u128;
typedef struct { np_u128 full; } Holder;
int main() {
    np_u128 r;
    r.hi = 1;
    Holder h;
    h.full.hi = 2;
    return r.lo + h.full.lo;
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/member.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/member.jeti",
        );
        let hints = index.inlay_hints(&crate::cheader::CSignatures::default());
        let pos_of = |needle: &str, inside: usize| -> LspPosition {
            let offset = src.find(needle).expect("needle") + inside;
            let before = &src[..offset];
            let line = before.matches('\n').count() as u32;
            let column = (offset - before.rfind('\n').map_or(0, |i| i + 1)) as u32;
            LspPosition { line, character: column }
        };
        let labels_at = |needle: &str| -> Vec<&str> {
            let position = pos_of(needle, needle.len());
            hints
                .iter()
                .filter(|h| h.position == position)
                .map(|h| h.label.as_str())
                .collect()
        };
        // One hint per statement, sitting past the whole member expression.
        assert_eq!(labels_at("r.hi"), vec![" -> int"], "{hints:?}");
        assert_eq!(labels_at("h.full.hi"), vec![" -> int"], "{hints:?}");
        assert_eq!(labels_at("h.full.lo"), vec![" -> int"], "{hints:?}");
        // Inner rings stay silent: a hint between `h` and `.full`, or between
        // `h.full` and `.hi`, would split the chain.
        assert!(
            hints.iter().all(|h| h.label != " -> np_u128" && h.label != " -> Holder"),
            "{hints:?}"
        );

        // The hover surface for a *use*: the declaration is on another line, so
        // `field_at` cannot answer — the member walk recorded the host and type.
        let use_hi = index
            .member_use_at("hi", pos_of("h.full.hi", 7))
            .expect("outermost ring resolves");
        assert_eq!(use_hi.struct_name, "np_u128");
        assert_eq!(use_hi.type_spelling, "int");
        let use_full = index
            .member_use_at("full", pos_of("h.full.hi", 3))
            .expect("inner ring resolves too");
        assert_eq!(use_full.struct_name, "Holder");

        // The typedef'd aggregate's own fields answer at their declaration.
        let declared = index
            .field_at("lo", pos_of("int lo", 4))
            .expect("typedef field declaration");
        assert_eq!(declared.struct_name, "np_u128");
    }

    #[test]
    fn external_c_functions_hint_through_the_header_table() {
        // Note: a string-literal argument gets no hint today — the AST gives
        // it a synthetic span, so it is absent from the call's argument list.
        // That is a miss, and misses are acceptable; int literals keep theirs.
        let src = "int main() { return ext(1, 2); }";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let table = crate::cheader::CSignatures::with_function(
            "ext",
            vec!["int x".into(), "int y".into()],
        );
        let hints = parameter_hints(index.inlay_hints(&table));
        assert_eq!(hints.len(), 2, "{hints:?}");
        assert_eq!(hints[0].position, at(src, "ext(1", 4));
        assert_eq!(hints[0].label, "x: ");
        assert_eq!(hints[1].position, at(src, "2);", 0));
        assert_eq!(hints[1].label, "y: ");
    }

    #[test]
    fn an_attribute_tail_is_not_a_parameter_name() {
        // `const char * __restrict` names no parameter: the attribute sits
        // where a name could, and a type word is never an acceptable label.
        let src = "int main() { return attr(1); }";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let table = crate::cheader::CSignatures::with_function(
            "attr",
            vec!["const char * __restrict".into(), "int".into()],
        );
        let hints = parameter_hints(index.inlay_hints(&table));
        assert!(hints.is_empty(), "{hints:?}");
    }

    #[test]
    fn a_declarator_that_hugs_the_star_keeps_its_name() {
        // The runtime headers spell pointers with the star hugging the name
        // (`NPObject *obj`), so the last whitespace-separated word is `*obj`.
        // The star belongs to the type; the identifier after it is the name.
        assert_eq!(parameter_name("int value").as_deref(), Some("value"));
        assert_eq!(parameter_name("NPObject *obj").as_deref(), Some("obj"));
        assert_eq!(parameter_name("char *fmt").as_deref(), Some("fmt"));
        assert_eq!(parameter_name("const char *cstr").as_deref(), Some("cstr"));
        assert_eq!(parameter_name("char **argv").as_deref(), Some("argv"));
        // An unnamed pointer or a qualifier-only tail still yields nothing.
        assert_eq!(parameter_name("char *"), None);
        assert_eq!(parameter_name("const char *restrict"), None);
        assert_eq!(parameter_name("char *const"), None);
    }

    #[test]
    fn designators_and_compound_literals_contribute_no_inlays() {
        // A compound literal and a designated initializer are not calls, so the
        // only hint in the document labels the free call's argument — a
        // designator field (`.location`) must never be read as a parameter.
        let src = "\
typedef struct NPRange { int location; int length; } NPRange;
int known(int x) { return x; }
int use(void) {
    NPRange r = (NPRange){ .location = 3, .length = 9 };
    return known(r.location);
}
";
        let mut pipeline = jetic::pipeline::Pipeline::new();
        let analysis = pipeline.analyze_document(src, "/tmp/y.jeti");
        let index = SymbolIndex::build(
            analysis.ast.as_ref().expect("ast"),
            &analysis.source_map,
            &|_| Some(src.to_string()),
            "/tmp/y.jeti",
        );
        let hints = parameter_hints(index.inlay_hints(&crate::cheader::CSignatures::default()));
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].label, "x: ");
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

#[cfg(test)]
mod inlay_anchors {
    use super::*;
    use jetic::pipeline::Pipeline;

    /// The user's screenshot, reproduced end to end: hints inserted
    /// mid-identifier in signatures (`np_u12⟨ -> T⟩8`) and mid-member-chain
    /// (`r⟨ -> T⟩.hi`) once `#import` expansion shifted the AST's flattened
    /// line numbers off the document's own. Anchoring must search the
    /// document's line (after projection) and never split a token or a chain.
    #[test]
    fn hints_never_split_a_token_or_a_member_chain() {
        let real_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../include/Foundation/NPDecimalNumber.jeti"
        );
        let real_path = std::fs::canonicalize(real_path).expect("canonicalize");
        let real_path = real_path.to_str().expect("utf8 path");
        let src = std::fs::read_to_string(real_path).expect("read NPDecimalNumber.jeti");
        let mut pipeline = Pipeline::new();
        // Mirror the editor: compile_flags.txt gives `-I include` from the
        // workspace root — absolute so the test is cwd-independent.
        pipeline.search_dirs =
            vec![concat!(env!("CARGO_MANIFEST_DIR"), "/../../include").to_string()];
        let analysis = pipeline.analyze_document(&src, real_path);
        let ast = analysis.ast.as_ref().expect("imports resolve to an AST");
        let index = SymbolIndex::build(
            ast,
            &analysis.source_map,
            // Faithful to the server: the document's own text for its own
            // path, disk for imported headers.
            &|p: &str| {
                if p == real_path {
                    Some(src.clone())
                } else {
                    std::fs::read_to_string(p).ok()
                }
            },
            real_path,
        );
        let hints = index.inlay_hints(&crate::cheader::CSignatures::default());
        assert!(
            hints.len() > 500,
            "the real file yields a full hint set, got {}",
            hints.len()
        );

        let offsets = line_offsets(&src);
        // The member accesses in the real file resolve through the synthesized
        // `np_u128` layout: a hint after the whole chain, and a hover surface
        // for the use. `u128_make` spells both shapes on one line.
        let doc_line = src
            .lines()
            .position(|l| l.contains("r.hi = hi"))
            .expect("u128_make body");
        let character = src
            .lines()
            .nth(doc_line)
            .and_then(|l| l.find("r.hi"))
            .expect("r.hi") as u32
            + 4;
        let member = index
            .member_use_at("hi", LspPosition { line: doc_line as u32, character })
            .expect("r.hi resolves");
        assert_eq!(member.struct_name, "np_u128");
        assert_eq!(member.type_spelling, "uint64_t");
        assert!(
            hints
                .iter()
                .any(|h| h.position.line == doc_line as u32 && h.label == " -> uint64_t"),
            "the member chain says its type: {hints:?}"
        );
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let bad: Vec<String> = hints
            .iter()
            .filter_map(|h| {
                // `line_slice` is 1-based; a hint's LSP line is 0-based.
                let line = line_slice(&src, &offsets, h.position.line as usize + 1)?;
                let mut units = 0usize;
                let mut before: Option<char> = None;
                let mut at: Option<char> = None;
                for (bi, ch) in line.char_indices() {
                    if units >= h.position.character as usize {
                        at = Some(ch);
                        let _ = bi;
                        break;
                    }
                    if units + ch.len_utf16() > h.position.character as usize {
                        // The anchor falls inside one multi-unit character.
                        return Some(format!("line {} splits a character", h.position.line));
                    }
                    before = Some(ch);
                    units += ch.len_utf16();
                }
                let at = match at {
                    Some(c) => c,
                    // End of line: nothing to split.
                    None => return None,
                };
                let before = before.unwrap_or(' ');
                if is_ident(before) && is_ident(at) {
                    return Some(format!("line {}: …{before}⟨{}⟩{at}…", h.position.line, h.label));
                }
                if is_ident(before) && (at == '.' || at == '[') {
                    return Some(format!(
                        "line {}: splits a chain: …{before}⟨{}⟩{at}…",
                        h.position.line, h.label
                    ));
                }
                None
            })
            .collect();
        assert!(bad.is_empty(), "mis-anchored hints:\n{}", bad.join("\n"));
    }
}
