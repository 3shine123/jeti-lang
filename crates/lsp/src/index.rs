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

use std::collections::HashMap;

use jeti_ast::ast::{AstDecl, AstDeclData, AstUnit};
use jeti_cst::source_map::{LspPosition, LspRange, SourceMap};
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

/// A declaration that can be jumped to.
#[derive(Debug, Clone)]
pub struct Location {
    pub path: String,
    pub range: LspRange,
    pub kind: u8,
    pub container: Option<String>,
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
    ) -> SymbolIndex {
        let mut cache: HashMap<String, Option<String>> = HashMap::new();
        let mut lookup = |path: &str| -> Option<String> {
            cache
                .entry(path.to_string())
                .or_insert_with(|| text_of(path))
                .clone()
        };
        let mut index = SymbolIndex::default();
        index.roots = index.visit(&ast.decls, None, source_map, &mut lookup);
        index
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
            .map(|symbols| {
                symbols
                    .iter()
                    .map(|symbol| Location {
                        path: symbol.path.clone(),
                        range: symbol.range,
                        kind: symbol.kind,
                        container: symbol.container.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        found.sort_by(|a, b| (&a.path, a.range.start).cmp(&(&b.path, b.range.start)));
        found.dedup_by(|a, b| a.path == b.path && a.range.start == b.range.start);
        found
    }

    // -- building ------------------------------------------------------------

    fn visit<'a>(
        &mut self,
        decls: impl IntoIterator<Item = &'a AstDecl>,
        container: Option<&str>,
        source_map: &SourceMap,
        text_of: &mut impl FnMut(&str) -> Option<String>,
    ) -> Vec<Symbol> {
        let mut out = Vec::new();
        for decl in decls {
            let Some(mut symbol) = self.symbol_for(decl, container, source_map, text_of) else {
                continue;
            };
            symbol.children = self.visit(nested_decls(decl), Some(&symbol.name), source_map, text_of);
            self.record(&symbol);
            out.push(symbol);
        }
        out
    }

    /// Index `symbol` under its lookup name, plus its short (unqualified) form
    /// so a reference written without the namespace prefix still resolves.
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
    let (word_start, word_end) = word_bounds(bytes, offset)?;
    let word = &text[word_start..word_end];

    if let Some(selector) = selector_at(bytes, offset, word_start, word_end) {
        return Some(selector);
    }
    // A declaration spells a keyword part as `foo:` (`- (void)foo:(int)bar`),
    // and that is how the index keys it — keep the colon the user typed.
    if bytes.get(word_end) == Some(&b':') {
        return Some(format!("{word}:"));
    }
    Some(word.to_string())
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

/// Identifier *start* byte.
fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Position of the `inside`-th UTF-16 unit of the first `needle` occurrence.
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
}
