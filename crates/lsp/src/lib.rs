//! Minimal Language Server for Jeti — plan 阶段 6 of
//! `docs/plans/source_locations_debug_lsp_plan.md`.
//!
//! Scope of this skeleton, deliberately small and honest:
//!
//! - `initialize` / `initialized` / `shutdown` / `exit` lifecycle, plus
//!   `textDocument/didOpen|didChange|didClose` with **full** document sync,
//!   each answered by `textDocument/publishDiagnostics`.
//! - Diagnostics come from the compiler's own front end
//!   ([`jetic::pipeline::Pipeline::analyze`]), so they are the same
//!   parser/binder/elaborator/checker/async findings the CLI reports — never a
//!   second, drifting linter.
//! - Positions cross the boundary exactly once, through the shared span model
//!   ([`Diagnostic::to_source_span`] → [`SourceSpan::to_lsp_range`]): the one
//!   place byte columns become LSP UTF-16 code units (plan §32). Nothing here
//!   re-derives a position in its own unit system.
//!
//! Not here yet (tracked in the plan): completion/hover/definition,
//! incremental sync, workspace folders, and ARC-only diagnostics (`analyze`
//! stops before the ARC pass).

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use jeti_cst::diagnostic::{Diagnostic, Severity};
use jeti_cst::source_map::{LspPosition, LspRange, SourceFile};
use jetic::pipeline::Pipeline;
use serde_json::{json, Value};

use crate::index::{typed_name_at, Resolved};

pub mod config;
pub mod index;
pub use index::{access_receiver_at, symbol_at, Location, Symbol, SymbolIndex};

// ---------------------------------------------------------------------------
// LSP wire framing (Content-Length delimited JSON-RPC over a byte stream)
// ---------------------------------------------------------------------------

/// Read one `Content-Length`-framed message body. `Ok(None)` at EOF or when a
/// header block carries no length; other header fields are ignored (only
/// `Content-Length` is mandatory in LSP).
pub fn read_message<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break; // blank line: headers end
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                len = value.trim().parse().ok();
            }
        }
    }
    let Some(len) = len else { return Ok(None) };
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

/// Write one message with its framing. UTF-8 byte length, as LSP requires.
pub fn write_message<W: Write>(writer: &mut W, message: &Value) -> std::io::Result<()> {
    let body = serde_json::to_string(message).unwrap_or_else(|_| "null".to_string());
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()
}

// ---------------------------------------------------------------------------
// Document URIs
// ---------------------------------------------------------------------------

/// `file:///abs/path` → `/abs/path`. Percent escapes are decoded (paths with
/// spaces are the normal case on this platform); non-`file` URIs are rejected.
pub fn uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    if !rest.starts_with('/') {
        return None; // `file://host/share` is not a local path
    }
    Some(percent_decode(rest))
}

/// `/abs/path` → `file:///abs/path`, percent-encoding everything that is not
/// an unreserved URI character or a path separator.
pub fn path_to_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

/// `file:` URI for a path the compiler reported as the home of a declaration.
///
/// Import paths come back relative when a search directory was relative, and a
/// relative URI would make the editor open the wrong file (or nothing), so such
/// a path is first resolved against the server's working directory — the same
/// directory the preprocessor resolved it from.
fn file_uri(path: &str) -> String {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let absolute = absolute.canonicalize().unwrap_or(absolute);
    path_to_uri(&absolute.to_string_lossy())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Do two paths name the same file? Exact and `Path`-normalized comparison
/// first (cheap, and covers `./x` vs `x`), then canonicalization so an editor's
/// path and the compiler's registered path still line up when one is relative.
pub(crate) fn same_file(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b || Path::new(a) == Path::new(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// What an analysis runs with.
///
/// The switches mirror the CLI's own flags, because the editor must report what
/// the user's build actually accepts: a project compiled with `-fno-jeti-arc`
/// (MRC) must not be told that `[_x release]` is an ARC error. Only switches
/// that change *diagnostics* are here — codegen and linking are not the editor's
/// business.
#[derive(Debug, Clone)]
pub struct AnalysisOptions {
    /// `#import` search path (`--include`).
    pub search_dirs: Vec<String>,
    /// `-fno-jeti-arc`: analyze in MRC mode.
    pub no_arc: bool,
    /// `-fno-checker`: parse, bind and elaborate, but skip the type checker.
    pub no_checker: bool,
    /// `-eh checked` (the default) vs `-eh legacy`/`-eh sjlj`.
    pub eh_checked: bool,
}

impl Default for AnalysisOptions {
    /// The CLI's defaults, with its default search path.
    fn default() -> Self {
        AnalysisOptions {
            search_dirs: Pipeline::new().search_dirs,
            no_arc: false,
            no_checker: false,
            eh_checked: jetic::pipeline::DEFAULT_EH_CHECKED,
        }
    }
}

/// Apply the diagnostic-relevant flags of a command line to `options`.
///
/// The server's own arguments and a project's `compile_commands.json` both go
/// through this, so a flag means one thing everywhere. `args[0]` is skipped as
/// the compiler, `source` (when given) is the file being compiled — not a flag
/// — and `base` resolves relative `-I` directories (a database entry's paths are
/// relative to its own `directory`; a command line's to the working directory,
/// which is why the server passes `None`).
///
/// Understood: `-I DIR` / `-IDIR` / `--include DIR`, `-eh <checked|legacy|sjlj>`,
/// `-fno-jeti-arc`, `-fno-checker`. Everything else is ignored — including the
/// value of `-o`, so an output path can never be mistaken for a directory.
pub fn apply_flags(
    args: &[String],
    base: Option<&std::path::Path>,
    source: Option<&str>,
    options: &mut AnalysisOptions,
) {
    let value = |index: usize| args.get(index + 1).map(String::as_str);
    let resolve = |dir: &str| match base {
        Some(base) => base.join(dir).to_string_lossy().to_string(),
        None => dir.to_string(),
    };

    let mut index = 1; // args[0] is the compiler
    while index < args.len() {
        let arg = args[index].as_str();
        if source.is_some_and(|source| names_the_same_file(arg, source)) {
            index += 1;
            continue;
        }
        match arg.trim_start_matches('-') {
            "I" | "include" => {
                if let Some(dir) = value(index) {
                    options.search_dirs.push(resolve(dir));
                    index += 2;
                    continue;
                }
            }
            "eh" => {
                if let Some(backend) = value(index) {
                    options.eh_checked = backend == "checked";
                    index += 2;
                    continue;
                }
            }
            "o" => {
                // An output path takes a value; skip it so it is not read as
                // anything else.
                index += 2;
                continue;
            }
            "fno-jeti-arc" | "no-arc" => options.no_arc = true,
            "fno-checker" | "no-checker" => options.no_checker = true,
            _ => {
                if let Some(dir) = arg.strip_prefix("-I") {
                    // A flags file holds one argument per line, so `-I include`
                    // arrives joined; the space is never intentional.
                    let dir = dir.trim();
                    if !dir.is_empty() {
                        options.search_dirs.push(resolve(dir));
                    }
                }
                // Any other flag, and any bare word (a file name), is not
                // something this server acts on.
            }
        }
        index += 1;
    }
}

/// Whether an argument names the file being compiled. Databases spell it
/// relative to the entry's directory while we hold the absolute path, so the
/// file name is compared as well — a command line names exactly one source file.
fn names_the_same_file(argument: &str, source: &str) -> bool {
    let (argument, source) = (std::path::Path::new(argument), std::path::Path::new(source));
    argument == source
        || argument.file_name().is_some_and(|name| Some(name) == source.file_name())
}

/// Diagnostics for `path` with `text` as its content, in LSP wire shape.
///
/// `text_of` supplies the content of any *other* file a diagnostic points into
/// (an imported header the editor has open, possibly unsaved); files it does
/// not know are read from disk, mirroring the CLI renderer.
pub fn diagnostics_for(
    path: &str,
    text: &str,
    search_dirs: &[String],
    text_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<Value> {
    let options = AnalysisOptions { search_dirs: search_dirs.to_vec(), ..Default::default() };
    analyze_for_editor(path, text, &options, text_of).diagnostics
}

/// One analysis run of an open document: what to publish, and what to answer
/// the requests that ask about the code from.
pub struct AnalyzedDocument {
    /// Diagnostics in LSP wire shape (for `textDocument/publishDiagnostics`).
    pub diagnostics: Vec<Value>,
    /// Symbol index for `textDocument/documentSymbol` and
    /// `textDocument/definition`. Empty when the front end produced no AST.
    pub index: SymbolIndex,
}

/// File content for positions: an open buffer first (the editor's version, even
/// unsaved), then the file on disk. A file neither source knows cannot be
/// projected, and callers skip rather than guess.
fn text_or_disk(text_of: &dyn Fn(&str) -> Option<String>, path: &str) -> Option<String> {
    text_of(path).or_else(|| std::fs::read_to_string(path).ok())
}

/// Analyze one document for the editor: run the compiler front end once, and
/// derive both the diagnostics and the symbol index from that single run.
pub fn analyze_for_editor(
    path: &str,
    text: &str,
    options: &AnalysisOptions,
    text_of: &dyn Fn(&str) -> Option<String>,
) -> AnalyzedDocument {
    let mut pipeline = Pipeline::new();
    pipeline.search_dirs = search_dirs_for(path, &options.search_dirs);
    pipeline.no_arc = options.no_arc;
    pipeline.no_checker = options.no_checker;
    pipeline.eh_checked = options.eh_checked;
    let analysis = pipeline.analyze_document(text, path);

    // Positions of imported files resolve from the open buffers or from disk —
    // a header the editor never opened still has to project its own columns.
    // The document being analyzed always resolves to its own text, buffer or
    // not: it is the buffer we were handed, and it may be unsaved.
    let resolve = |other: &str| {
        if same_file(other, path) {
            Some(text.to_string())
        } else {
            text_or_disk(text_of, other)
        }
    };

    let diagnostics = analysis
        .diagnostics
        .iter()
        .map(|d| diagnostic_to_json(d, path, text, &resolve))
        .collect();
    // The index is built from the same AST the diagnostics came from, so a
    // request never re-runs the front end; a document whose front end failed
    // simply has no symbols to offer.
    let index = match &analysis.ast {
        Some(ast) => SymbolIndex::build(ast, &analysis.source_map, &resolve, path),
        None => SymbolIndex::default(),
    };
    AnalyzedDocument { diagnostics, index }
}

/// Search path for `#import`: the document's own directory first — a sibling
/// import is the natural spelling — then the configured dirs.
fn search_dirs_for(path: &str, search_dirs: &[String]) -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    if let Some(parent) = Path::new(path).parent() {
        let dir = parent.to_string_lossy().to_string();
        if !dir.is_empty() {
            dirs.push(dir);
        }
    }
    dirs.extend(search_dirs.iter().cloned());
    dirs
}

/// Apply one `textDocument/didChange` entry to the document text.
///
/// Two shapes are legal and both are handled: a whole-buffer replacement
/// (`{text}` with no range, which is what an editor sends when it rewrites
/// everything) and a range replacement (`{range, text}`). A range's positions
/// are 0-based lines with **UTF-16** columns, exactly LSP's coordinates, so the
/// shared `lsp_to_offset` does the crossing — an edit after an emoji lands where
/// the editor meant, not where a byte count would put it.
///
/// Returns `false` when a range cannot be applied (a position past the end, or
/// inverted bounds). The caller keeps the previous text then; the alternative —
/// guessing — would silently diverge from what the editor shows.
fn apply_change(text: &mut String, change: &Value) -> bool {
    let Some(replacement) = change.get("text").and_then(Value::as_str) else {
        return false;
    };
    let Some(range) = change.get("range") else {
        // Whole-buffer replacement: no range at all.
        text.clear();
        text.push_str(replacement);
        return true;
    };
    let position = |name: &str| -> Option<LspPosition> {
        let point = range.get(name)?;
        Some(LspPosition {
            line: point.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
            character: point.get("character").and_then(Value::as_u64).unwrap_or(0) as u32,
        })
    };
    let (Some(start), Some(end)) = (position("start"), position("end")) else {
        return false;
    };
    let (Some(start), Some(end)) = (
        SourceFile::lsp_to_offset(text, start),
        SourceFile::lsp_to_offset(text, end),
    ) else {
        return false;
    };
    if start > end {
        return false;
    }
    text.replace_range(start..end, replacement);
    true
}

/// One diagnostic in LSP shape.
///
/// The byte→UTF-16 projection runs on the text of the file the diagnostic
/// points into, not on the document being edited — a diagnostic inside an
/// imported header carries that header's columns. `resolve` supplies that text
/// (open buffers first, disk second).
pub fn diagnostic_to_json(
    diagnostic: &Diagnostic,
    main_path: &str,
    main_text: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Value {
    let ownership = if same_file(&diagnostic.file, main_path) {
        Some(main_text.to_string())
    } else {
        resolve(&diagnostic.file)
    };
    // LSP cannot express "no position": a diagnostic whose file text is not
    // available keeps the file-wide default instead of inventing a column.
    let range: LspRange = match ownership.as_deref() {
        Some(text) => diagnostic.to_source_span().to_lsp_range(text),
        None => LspRange::default(),
    };
    json!({
        "range": range_json(range),
        "severity": severity_code(diagnostic.severity),
        "source": "jeti",
        "message": diagnostic.message,
    })
}

fn range_json(range: LspRange) -> Value {
    index::range_json(range)
}

fn severity_code(severity: Severity) -> u8 {
    match severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        // LSP numbers: 1 error, 2 warning, 3 information, 4 hint. Jeti's `note`
        // annotates a primary diagnostic, which is information, not a hint.
        Severity::Note => 3,
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// One open document, as the client last described it.
struct Document {
    path: String,
    text: String,
}

/// LSP state machine: open documents plus the search path diagnostics use.
/// Protocol I/O lives in `main.rs`, so this is driveable from tests.
pub struct Server {
    /// Keyed by the client's URI (the identity the protocol uses).
    docs: HashMap<String, Document>,
    /// Symbol index per open document, refreshed by the same analysis run that
    /// produced the last published diagnostics.
    indexes: HashMap<String, SymbolIndex>,
    /// What each analysis runs with: search path plus the compiler switches
    /// that change diagnostics.
    options: AnalysisOptions,
    /// Parsed project configuration (`compile_commands.json` /
    /// `compile_flags.txt`), cached per file.
    config: config::ConfigCache,
    /// Workspace root from `initialize`: the upward search for configuration
    /// files stops here instead of walking to the filesystem root.
    root: Option<String>,
    shutdown_received: bool,
}

impl Server {
    /// An empty [`AnalysisOptions`] is the CLI's default setup (its search path,
    /// ARC on, `-eh checked`, checker on), so a plain `jeti-lsp` behaves like
    /// `jetic` does in that directory.
    pub fn new(options: AnalysisOptions) -> Self {
        Server {
            docs: HashMap::new(),
            indexes: HashMap::new(),
            options,
            config: config::ConfigCache::default(),
            root: None,
            shutdown_received: false,
        }
    }

    /// True once `shutdown` arrived (the client then expects `exit`).
    pub fn shutdown_received(&self) -> bool {
        self.shutdown_received
    }

    /// Handle one incoming JSON-RPC message; returns the messages to send back,
    /// in order. Notifications (no `id`) never produce a response.
    pub fn handle(&mut self, message: &Value) -> Vec<Value> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let id = message.get("id").cloned();
        match method {
            "initialize" => {
                // Where the project starts: the upward search for configuration
                // files stops here. `workspaceFolders` is the modern spelling,
                // `rootUri` the one everything still sends.
                self.root = message
                    .pointer("/params/rootUri")
                    .and_then(Value::as_str)
                    .or_else(|| message.pointer("/params/workspaceFolders/0/uri").and_then(Value::as_str))
                    .and_then(uri_to_path);
                vec![response(
                id,
                json!({
                    "capabilities": {
                        // 2 = Incremental: the client sends the changed range,
                        // not the whole buffer. See `apply_change`.
                        "textDocumentSync": 2,
                        "documentSymbolProvider": true,
                        "definitionProvider": true,
                        "hoverProvider": true,
                        // No `resolveProvider`: everything a completion item
                        // needs (label, kind, detail) is in the same answer.
                        "completionProvider": { "resolveProvider": false },
                        // `:` is where a keyword selector's next parameter
                        // starts, so the popup appears exactly when it can say
                        // something.
                        "signatureHelpProvider": { "triggerCharacters": [":"] },
                    },
                    "serverInfo": {
                        "name": "jeti-lsp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
                )]
            }
            "initialized" => Vec::new(),
            "shutdown" => {
                self.shutdown_received = true;
                vec![response(id, Value::Null)]
            }
            "exit" => Vec::new(),
            "textDocument/didOpen" => {
                let doc = message.pointer("/params/textDocument");
                let uri = doc.and_then(|d| d.get("uri")).and_then(Value::as_str).unwrap_or("");
                let text = doc.and_then(|d| d.get("text")).and_then(Value::as_str).unwrap_or("");
                match uri_to_path(uri) {
                    Some(path) => {
                        self.docs.insert(uri.to_string(), Document { path, text: text.to_string() });
                        vec![self.publish_and_index(uri)]
                    }
                    None => Vec::new(),
                }
            }
            "textDocument/didChange" => {
                let uri = message
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let changes = message
                    .pointer("/params/contentChanges")
                    .and_then(Value::as_array);
                // LSP applies the changes of one notification in order, each to
                // the text the previous one produced.
                if let (Some(changes), Some(doc)) = (changes, self.docs.get_mut(uri)) {
                    for change in changes {
                        apply_change(&mut doc.text, change);
                    }
                }
                if self.docs.contains_key(uri) {
                    vec![self.publish_and_index(uri)]
                } else {
                    Vec::new()
                }
            }
            "textDocument/didClose" => {
                let uri = message
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.docs.remove(uri);
                self.indexes.remove(uri);
                // Diagnostics are owned by the server: clearing them is how the
                // client is told they are stale.
                vec![publish_empty(uri)]
            }
            // The outline of one document, from the index the last analysis
            // built — never a second parse of the buffer.
            "textDocument/documentSymbol" => {
                let uri = message
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let symbols = match (self.docs.get(uri), self.indexes.get(uri)) {
                    (Some(doc), Some(index)) => Value::Array(index.document_symbols(&doc.path)),
                    _ => Value::Array(Vec::new()),
                };
                vec![response(id, symbols)]
            }
            // The declarations of the symbol under the cursor. Several
            // locations are a normal answer (one selector, several classes), so
            // the client shows the list; no match is `null` rather than a
            // guess.
            "textDocument/definition" => {
                let uri = request_uri(message);
                let position = request_position(message);
                let locations = self.definitions(uri, position);
                let result = if locations.is_empty() {
                    Value::Null
                } else {
                    Value::Array(locations)
                };
                vec![response(id, result)]
            }
            // What the symbol under the cursor *is*, with its declaration text.
            "textDocument/hover" => {
                let position = request_position(message);
                let uri = request_uri(message);
                let mut hover = Value::Null;
                if let (Some(doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) {
                    match index.resolve_at(&doc.text, position) {
                        // A namespace is known by what it holds: there is no
                        // declaration of it to quote (see `namespace_markdown`).
                        Some(Resolved::Namespace(name)) => {
                            hover = json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": namespace_markdown(&name),
                                },
                            });
                        }
                        Some(Resolved::Symbol(name)) => {
                            let locations = index.definitions(&name);
                            if let Some(first) = locations.first() {
                                let text_of = self.text_of();
                                let resolve = |path: &str| text_or_disk(&text_of, path);
                                hover = json!({
                                    "contents": {
                                        "kind": "markdown",
                                        "value": hover_markdown(first, locations.len(), &resolve),
                                    },
                                });
                            }
                        }
                        None => {}
                    }
                }
                vec![response(id, hover)]
            }
            // Names the index knows, filtered by what has been typed.
            "textDocument/completion" => {
                let position = request_position(message);
                let uri = request_uri(message);
                let mut entries: Vec<(String, u8, String)> = Vec::new();
                if let (Some(doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) {
                    // What has been typed is the prefix — including a namespace
                    // qualifier, so `Game::En` completes to `Game::Enemies` and
                    // a bare `Game::` lists what the namespace holds instead of
                    // every name in the index.
                    let prefix = typed_name_at(&doc.text, position).unwrap_or_default();
                    // Members of the receiver come first: inside a send they are
                    // what the user is looking for, and the global names follow
                    // as a fallback (the same list, deduplicated by label).
                    let receiver = index.send_receiver(position).or_else(|| {
                        access_receiver_at(&doc.text, position)
                            .and_then(|name| index.receiver_class(&name, position))
                    });
                    if let Some((class, class_receiver)) = receiver {
                        for member in index.member_completion(&class, class_receiver, &prefix) {
                            entries.push((member.label, member.kind, member.detail));
                        }
                    }
                    for completion in index.complete(&prefix) {
                        if !entries.iter().any(|(label, _, _)| *label == completion.label) {
                            entries.push((completion.label, completion.kind, completion.detail));
                        }
                    }
                }
                let items: Vec<Value> = entries
                    .into_iter()
                    .map(|(label, kind, detail)| {
                        let mut item = json!({ "label": label, "kind": kind });
                        if !detail.is_empty() {
                            item["detail"] = json!(detail);
                        }
                        item
                    })
                    .collect();
                vec![response(id, json!({ "isIncomplete": false, "items": items }))]
            }
            // The signature of the call the cursor is inside, with the parameter
            // being filled in.
            "textDocument/signatureHelp" => {
                let position = request_position(message);
                let uri = request_uri(message);
                let mut help = Value::Null;
                if let (Some(_doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) {
                    if let Some(signature) = index.signature_help(position) {
                        let signatures: Vec<Value> = signature
                            .signatures
                            .iter()
                            .map(|candidate| {
                                json!({
                                    "label": candidate.label,
                                    "parameters": candidate
                                        .parameters
                                        .iter()
                                        .map(|parameter| json!({ "label": parameter }))
                                        .collect::<Vec<_>>(),
                                })
                            })
                            .collect();
                        help = json!({
                            "signatures": signatures,
                            "activeSignature": signature.active_signature,
                        });
                        // A signature with no parameters (a colon-less
                        // selector) has no parameter to be active in, and LSP
                        // wants that field omitted rather than out of range.
                        if !signature.signatures[0].parameters.is_empty() {
                            help["activeParameter"] = json!(signature.active_parameter);
                        }
                    }
                }
                vec![response(id, help)]
            }
            "" => Vec::new(),
            other => match id {
                Some(id) => vec![error_response(id, -32601, &format!("unhandled method: {other}"))],
                None => Vec::new(),
            },
        }
    }

    /// Analyze `uri` and keep the index, so publishing is the only place the
    /// front end runs for a document. Returns the diagnostics message.
    fn publish_and_index(&mut self, uri: &str) -> Value {
        let Some(path) = self.docs.get(uri).map(|doc| doc.path.clone()) else {
            return publish_empty(uri);
        };
        // The project's own configuration refines the server's setup, so this
        // needs the cache before the analysis borrows the documents.
        let options = self.effective_options(&path);
        // Scoped so the immutable borrows of `self` end before the index is
        // stored (the closure borrows `self.docs`).
        let analyzed = {
            let doc = self.docs.get(uri).expect("checked above");
            let text_of = self.text_of();
            analyze_for_editor(&doc.path, &doc.text, &options, &text_of)
        };
        self.indexes.insert(uri.to_string(), analyzed.index);
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": analyzed.diagnostics },
        })
    }

    /// The options one document is analyzed with: the server's own, refined by
    /// whatever the project's configuration file says about it.
    fn effective_options(&mut self, document: &str) -> AnalysisOptions {
        let mut options = self.options.clone();
        if let Some(flags) = self.config.flags_for(document, self.root.as_deref()) {
            flags.merge_into(&mut options);
        }
        options
    }

    /// Text of any open document, by path (open buffers shadow disk).
    fn text_of(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |path: &str| {
            self.docs
                .values()
                .find(|d| same_file(&d.path, path))
                .map(|d| d.text.clone())
        }
    }

    /// URI for a path the compiler reported.
    ///
    /// An open document keeps the URI the client gave us — the editor's own
    /// identity for that buffer (and canonicalizing it could make the editor
    /// open a second tab for the same file). Anything else (an imported header)
    /// is made absolute; see [`file_uri`].
    fn uri_for(&self, path: &str) -> String {
        if let Some((uri, _)) = self.docs.iter().find(|(_, doc)| same_file(&doc.path, path)) {
            return uri.clone();
        }
        file_uri(path)
    }

    /// Declarations of the symbol at `position` in `uri`.
    fn definitions(&self, uri: &str, position: LspPosition) -> Vec<Value> {
        let (Some(doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) else {
            return Vec::new();
        };
        index
            .definitions_at(&doc.text, position)
            .iter()
            .map(|location| {
                json!({
                    "uri": self.uri_for(&location.path),
                    "range": range_json(location.range),
                })
            })
            .collect()
    }
}

fn publish_empty(uri: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": { "uri": uri, "diagnostics": [] },
    })
}

fn response(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
}

/// `params.textDocument.uri` of a request, or `""` when absent.
fn request_uri(message: &Value) -> &str {
    message
        .pointer("/params/textDocument/uri")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// `params.position` of a request, or the document start when absent.
fn request_position(message: &Value) -> LspPosition {
    let position = message.pointer("/params/position");
    let coordinate = |name: &str| {
        position
            .and_then(|p| p.get(name))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32
    };
    LspPosition { line: coordinate("line"), character: coordinate("character") }
}

/// Hover for a name that is a namespace, not a declaration.
///
/// The elaborator lifts every `@namespace` member to the top level under its
/// qualified name, so the index learns a namespace from what it holds and has no
/// `@namespace` line to quote. Saying that plainly is better than showing a
/// member and pretending it is the namespace.
fn namespace_markdown(name: &str) -> String {
    format!(
        "**namespace** `{name}`\n\n_Contents are indexed; the `@namespace` block itself is flattened \
         by the compiler, so there is no declaration to show._"
    )
}

/// Markdown hover: what the declaration is, its own source text, and where it
/// lives. `declarations` is how many the name resolved to, so a selector with a
/// declaration and an implementation says so instead of hiding one.
fn hover_markdown(
    location: &Location,
    declarations: usize,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> String {
    let mut markdown = format!("**{}** `{}`", kind_name(location.kind), location.name);
    if !location.detail.is_empty() && location.detail != location.name {
        markdown.push_str(&format!(" · {}", location.detail));
    }
    markdown.push('\n');
    if let Some(container) = &location.container {
        markdown.push_str(&format!("in `{container}`\n"));
    }
    if let Some(text) = resolve(&location.path) {
        if let Some(snippet) = declaration_text(&text, location.range) {
            markdown.push_str(&format!("\n```jeti\n{snippet}\n```\n"));
        }
    }
    markdown.push_str(&format!(
        "\n_Declared in {}:{}_",
        display_path(&location.path),
        location.range.start.line + 1
    ));
    if declarations > 1 {
        markdown.push_str(&format!(" · _{declarations} declarations_"));
    }
    markdown
}

/// The declaration's own lines, so a hover shows the signature rather than only
/// a name. Capped so a whole class body cannot flood the popup.
fn declaration_text(text: &str, range: LspRange) -> Option<String> {
    const MAX_LINES: u32 = 10;
    let start = range.start.line;
    let end = range.end.line.min(start + MAX_LINES - 1);
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().skip(start as usize) {
        if lines.len() as u32 > end - start {
            break;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

/// Path as a reader wants it: relative to the server's working directory (the
/// workspace root) when it is inside it, absolute otherwise.
fn display_path(path: &str) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            Path::new(path)
                .strip_prefix(&cwd)
                .ok()
                .map(|relative| relative.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| path.to_string())
}

/// Human word for an LSP symbol kind (the numbers in [`index::kind`]).
fn kind_name(kind: u8) -> &'static str {
    match kind {
        index::kind::NAMESPACE => "namespace",
        index::kind::CLASS => "class",
        index::kind::METHOD => "method",
        index::kind::PROPERTY => "property",
        index::kind::FIELD => "instance variable",
        index::kind::ENUM => "enum",
        index::kind::INTERFACE => "protocol",
        index::kind::FUNCTION => "function",
        index::kind::VARIABLE => "variable",
        index::kind::STRUCT => "struct",
        _ => "declaration",
    }
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_open_files() -> impl Fn(&str) -> Option<String> {
        |_: &str| None
    }

    #[test]
    fn uri_round_trip_keeps_spaces() {
        let path = "/tmp/a b/ç.jeti";
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///tmp/a%20b/%C3%A7.jeti");
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
        assert!(uri_to_path("https://example.com/x").is_none());
    }

    #[test]
    fn diagnostic_columns_project_through_utf16() {
        // Bytes: `let a = ` 8, `"` 1, `h` 1, `é` 2, `llo ` 4, `中` 3, `文` 3,
        // `"` 1, `;` 1. The closing quote is 1-based byte column 23, which is
        // UTF-16 code unit 17 (é, 中 and 文 are one unit each despite 2–3 bytes).
        let text = "let a = \"héllo 中文\";";
        let mut d = Diagnostic::error("/x.jeti", 1, 23, "example");
        d.end_col = Some(24);
        let json = diagnostic_to_json(&d, "/x.jeti", text, &no_open_files());
        assert_eq!(json["range"]["start"], json!({"line": 0, "character": 17}));
        assert_eq!(json["range"]["end"], json!({"line": 0, "character": 18}));
        assert_eq!(json["severity"], 1);
        assert_eq!(json["source"], "jeti");
        assert_eq!(json["message"], "example");
    }

    #[test]
    fn file_wide_diagnostic_stays_at_the_origin() {
        // A preprocessor failure: no file, no line. LSP gets a zero-width range
        // at the start of the document rather than a fabricated column.
        let d = Diagnostic::error("", 0, 0, "boom");
        assert!(d.to_source_span().is_synthetic());
        let json = diagnostic_to_json(&d, "/x.jeti", "let x = 1;\n", &no_open_files());
        assert_eq!(json["range"]["start"], json!({"line": 0, "character": 0}));
        assert_eq!(json["range"]["end"], json!({"line": 0, "character": 0}));
    }

    #[test]
    fn a_parse_error_becomes_a_diagnostic_with_a_position() {
        // Same program the CLI test uses: the missing `;` is at column 25.
        let path = "/tmp/jeti_lsp_unit/parse_err.jeti";
        let text = "int main(void) { return 0 }\n";
        let diags = diagnostics_for(path, text, &[], &no_open_files());
        assert!(!diags.is_empty(), "expected a parse diagnostic, got none");
        let first = &diags[0];
        assert_eq!(first["severity"], 1);
        assert_eq!(first["range"]["start"], json!({"line": 0, "character": 24}));
        assert!(
            first["message"].as_str().unwrap().contains("expected ';'"),
            "unexpected message: {}",
            first["message"]
        );
    }

    #[test]
    fn a_clean_program_has_no_diagnostics() {
        let diags = diagnostics_for(
            "/tmp/jeti_lsp_unit/ok.jeti",
            "int main(void) { return 0; }\n",
            &[],
            &no_open_files(),
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn initialize_then_didopen_publishes_diagnostics() {
        let mut server = Server::new(AnalysisOptions::default());
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let out = server.handle(&init);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[0]["result"]["capabilities"]["textDocumentSync"], 2);

        let uri = path_to_uri("/tmp/jeti_lsp_unit/session.jeti");
        let open = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri,
                "languageId": "jeti",
                "version": 1,
                "text": "int main(void) { return 0 }\n",
            }},
        });
        let out = server.handle(&open);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[0]["params"]["uri"], uri);
        assert!(!out[0]["params"]["diagnostics"].as_array().unwrap().is_empty());

        // A clean edit clears them (full sync: whole buffer replaces the old).
        let change = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": "int main(void) { return 0; }\n" }],
            },
        });
        let out = server.handle(&change);
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));

        let shutdown = json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"});
        let out = server.handle(&shutdown);
        assert!(out[0]["result"].is_null());
        assert!(server.shutdown_received());

        let close = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } },
        });
        let out = server.handle(&close);
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
    }

    #[test]
    fn incremental_changes_apply_in_the_editors_coordinates() {
        // A range replacement keeps the rest of the buffer.
        let mut text = String::from("let a = 1;\nlet b = 2;\n");
        assert!(apply_change(
            &mut text,
            &json!({ "range": { "start": {"line": 1, "character": 8},
                                "end": {"line": 1, "character": 9} },
                     "text": "42" }),
        ));
        assert_eq!(text, "let a = 1;\nlet b = 42;\n");

        // An insertion: an empty range.
        assert!(apply_change(
            &mut text,
            &json!({ "range": { "start": {"line": 0, "character": 0},
                                "end": {"line": 0, "character": 0} },
                     "text": "// hi\n" }),
        ));
        assert_eq!(text, "// hi\nlet a = 1;\nlet b = 42;\n");

        // A deletion: an empty replacement. LSP columns stop at the line's end
        // (the newline is not addressable), so a whole-line delete ends at the
        // *start of the next* line.
        assert!(apply_change(
            &mut text,
            &json!({ "range": { "start": {"line": 0, "character": 0},
                                "end": {"line": 1, "character": 0} },
                     "text": "" }),
        ));
        assert_eq!(text, "let a = 1;\nlet b = 42;\n");

        // A whole-buffer replacement carries no range.
        assert!(apply_change(&mut text, &json!({ "text": "let c = 3;\n" })));
        assert_eq!(text, "let c = 3;\n");

        // Columns are UTF-16 code units, not bytes: `😀` is 2 units and 4
        // bytes, so `let s = "` (9 units) puts the emoji at 9..11 and `x` at 11.
        // Editing "the character after the emoji" must land on `x`, not four
        // bytes in.
        let mut text = String::from("let s = \"😀x\";\n");
        assert!(apply_change(
            &mut text,
            &json!({ "range": { "start": {"line": 0, "character": 11},
                                "end": {"line": 0, "character": 12} },
                     "text": "y" }),
        ));
        assert_eq!(text, "let s = \"😀y\";\n");

        // …and an edit before the emoji keeps it intact.
        let mut text = String::from("let s = \"😀x\";\n");
        assert!(apply_change(
            &mut text,
            &json!({ "range": { "start": {"line": 0, "character": 9},
                                "end": {"line": 0, "character": 9} },
                     "text": "-" }),
        ));
        assert_eq!(text, "let s = \"-😀x\";\n");

        // An impossible range is refused rather than guessed at.
        let mut text = String::from("short\n");
        assert!(!apply_change(
            &mut text,
            &json!({ "range": { "start": {"line": 9, "character": 0},
                                "end": {"line": 10, "character": 0} },
                     "text": "x" }),
        ));
        assert_eq!(text, "short\n");
    }

    #[test]
    fn a_range_change_moves_the_diagnostics() {
        let mut server = Server::new(AnalysisOptions::default());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/incremental.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1,
                "text": "int main(void) { return 0; }\n",
            }},
        }));

        // Delete the `;` (line 0, character 25) — the parse error appears there.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{
                    "range": { "start": {"line": 0, "character": 25},
                               "end": {"line": 0, "character": 26} },
                    "text": "",
                }],
            },
        }));
        let diagnostics = out[0]["params"]["diagnostics"].as_array().expect("diagnostics");
        assert_eq!(diagnostics.len(), 1, "{out:?}");
        assert_eq!(diagnostics[0]["range"]["start"], json!({"line": 0, "character": 24}));

        // Put it back: the same range, an insertion this time.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 3 },
                "contentChanges": [{
                    "range": { "start": {"line": 0, "character": 25},
                               "end": {"line": 0, "character": 25} },
                    "text": ";",
                }],
            },
        }));
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
    }

    #[test]
    fn mrc_projects_can_turn_arc_off() {
        // The case a real editor session hit: legacy MRC code (`dealloc` with
        // manual releases) is an ARC error under the CLI's defaults, and the
        // editor must be able to say "this project builds with -fno-jeti-arc"
        // and stop reporting what the build accepts.
        let mrc = "\
@interface Node : jeti_root
@end

@implementation Node
- (void)dealloc {
    [_next release];
    [super dealloc];
}
@end
";
        let arc = analyze_for_editor(
            "/tmp/jeti_lsp_unit/mrc.jeti",
            mrc,
            &AnalysisOptions::default(),
            &no_open_files(),
        );
        assert!(
            arc.diagnostics
                .iter()
                .any(|d| d["message"].as_str().unwrap().contains("not allowed in ARC mode")),
            "{:?}",
            arc.diagnostics
        );

        let options = AnalysisOptions { no_arc: true, ..Default::default() };
        let mrc_mode =
            analyze_for_editor("/tmp/jeti_lsp_unit/mrc.jeti", mrc, &options, &no_open_files());
        assert!(mrc_mode.diagnostics.is_empty(), "{:?}", mrc_mode.diagnostics);
        // Symbols still work in that mode (the index is independent of the
        // checker's ARC rules).
        let symbols = mrc_mode.index.document_symbols("/tmp/jeti_lsp_unit/mrc.jeti");
        assert_eq!(symbols.len(), 2, "{symbols:?}");
    }

    #[test]
    fn unhandled_request_gets_method_not_found() {
        let mut server = Server::new(AnalysisOptions::default());
        let req = json!({"jsonrpc": "2.0", "id": 7, "method": "textDocument/rename", "params": {}});
        let out = server.handle(&req);
        assert_eq!(out[0]["error"]["code"], -32601);
    }

    #[test]
    fn framed_messages_round_trip_through_the_codec() {
        let messages = vec![json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
                            json!({"jsonrpc": "2.0", "method": "exit"})];
        let mut wire = Vec::new();
        for m in &messages {
            write_message(&mut wire, m).unwrap();
        }
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(wire));
        for expected in &messages {
            let body = read_message(&mut reader).unwrap().expect("a frame");
            assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), *expected);
        }
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn content_length_header_is_case_insensitive() {
        let wire = b"content-length: 2\r\n\r\n{}".to_vec();
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(wire));
        assert_eq!(read_message(&mut reader).unwrap().as_deref(), Some("{}"));
    }

    /// A self-contained program (no `#import`, so the test needs no headers).
    const PROGRAM: &str = "\
typedef struct Point { int x; } Point;

@interface Thing : jeti_root
- (void)run:(Point)where;
@end

@implementation Thing
- (void)run:(Point)where {
    [self run:where];
}
@end

int helper(int value) { return value; }
";

    fn analyze(source: &str) -> AnalyzedDocument {
        analyze_for_editor("/tmp/jeti_lsp_unit/symbols.jeti", source, &AnalysisOptions::default(), &no_open_files())
    }

    #[test]
    fn outline_comes_from_the_index_with_exact_ranges() {
        let analyzed = analyze(PROGRAM);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        let symbols = analyzed.index.document_symbols("/tmp/jeti_lsp_unit/symbols.jeti");
        let names: Vec<&str> = symbols.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Point", "Thing", "Thing", "helper"], "{symbols:#?}");

        // The typedef is a `typedef` detail; the class carries its method.
        assert_eq!(symbols[0]["kind"], 5);
        assert_eq!(symbols[0]["detail"], "typedef");
        assert_eq!(symbols[1]["detail"], "@interface:jeti_root");
        let methods = symbols[1]["children"].as_array().unwrap();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["name"], "run:");
        assert_eq!(methods[0]["kind"], 6);
        assert_eq!(methods[0]["detail"], "- run:");
        // The declaration extent is the whole `- (void)run:(Point)where;` line,
        // 0-based line 3 and one past its last character.
        assert_eq!(methods[0]["range"]["start"], json!({"line": 3, "character": 0}));
        assert_eq!(methods[0]["range"]["end"], json!({"line": 3, "character": 25}));
    }

    #[test]
    fn definition_resolves_the_name_under_the_cursor() {
        let analyzed = analyze(PROGRAM);
        let path = "/tmp/jeti_lsp_unit/symbols.jeti";

        // The class name on the `@interface` line, and its use in a send.
        let position = |line, character| LspPosition { line, character };
        let name = symbol_at(PROGRAM, position(7, 9)).expect("a symbol under the cursor");
        assert_eq!(name, "run:");
        let definitions = analyzed.index.definitions(&name);
        assert_eq!(definitions.len(), 2, "declaration + implementation");

        let name = symbol_at(PROGRAM, position(2, 11)).expect("a symbol under the cursor");
        assert_eq!(name, "Thing");
        let definitions = analyzed.index.definitions(&name);
        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0].range.start.line, 2);
        assert_eq!(definitions[0].range.start.character, 0);

        // A name the index does not know is a miss, not a guess.
        assert!(analyzed.index.definitions("value").is_empty());
        assert!(analyzed.index.definitions("where").is_empty());
        assert!(!analyzed.index.definitions(&name).is_empty());
        let _ = path;
    }

    /// The same class name in three namespaces: before the cursor's qualified
    /// name was taken apart, all three collapsed into the short name `Player`
    /// and every reference resolved to all of them.
    const NAMESPACES: &str = "\
@namespace A
@interface Player : jeti_root
- (void)fromA;
@end
@endnamespace

@namespace B
@interface Player : jeti_root
- (void)fromB;
@end
@endnamespace

@namespace A::Inner
@interface Player : jeti_root
- (void)deep;
@end
@endnamespace

int main() {
    A::Player *p;
    B::Player *q;
    A::Inner::Player *r;
    return 0;
}
";

    #[test]
    fn a_qualified_name_resolves_one_segment_at_a_time() {
        let analyzed = analyze(NAMESPACES);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        // On `Player` in `A::Player`: that class, and only it.
        let on_player = position(NAMESPACES, "A::Player", 3);
        assert_eq!(
            analyzed.index.resolve_at(NAMESPACES, on_player),
            Some(index::Resolved::Symbol("A::Player".to_string()))
        );
        let definitions = analyzed.index.definitions_at(NAMESPACES, on_player);
        assert_eq!(definitions.len(), 1, "{definitions:?}");
        assert_eq!(definitions[0].range.start.line, 1);

        // The namespaces are told apart too: `B::Player` is B's, and
        // `A::Inner::Player` is the deep one.
        let definitions = analyzed
            .index
            .definitions_at(NAMESPACES, position(NAMESPACES, "B::Player", 3));
        assert_eq!(definitions.len(), 1, "{definitions:?}");
        assert_eq!(definitions[0].range.start.line, 7);
        let definitions = analyzed
            .index
            .definitions_at(NAMESPACES, position(NAMESPACES, "A::Inner::Player", 10));
        assert_eq!(definitions.len(), 1, "{definitions:?}");
        assert_eq!(definitions[0].range.start.line, 13);

        // On a namespace segment the answer is the namespace. The elaborator
        // flattens `@namespace` blocks, so there is no declaration to jump to
        // — an honest empty answer, not the class it happens to hold.
        let on_namespace = position(NAMESPACES, "A::Inner::Player", 3);
        assert_eq!(
            analyzed.index.resolve_at(NAMESPACES, on_namespace),
            Some(index::Resolved::Namespace("A::Inner".to_string()))
        );
        assert!(analyzed.index.definitions_at(NAMESPACES, on_namespace).is_empty());
        // And the outermost segment is a namespace of its own.
        assert_eq!(
            analyzed.index.resolve_at(NAMESPACES, position(NAMESPACES, "A::Player", 0)),
            Some(index::Resolved::Namespace("A".to_string()))
        );

        // A qualified name the index does not have is a miss: answering with
        // another namespace's class that happens to share the short name would
        // be a guess.
        let unknown = "    C::Player *p;";
        assert_eq!(analyzed.index.resolve_at(unknown, position(unknown, "C::Player", 3)), None);
        assert!(analyzed.index.definitions_at(unknown, position(unknown, "C::Player", 3)).is_empty());
        // Written without a qualifier, a name the index knows still resolves —
        // the short form is indexed for exactly that.
        let short = "    Player *p;";
        assert_eq!(
            analyzed.index.resolve_at(short, position(short, "Player", 0)),
            Some(index::Resolved::Symbol("Player".to_string()))
        );
    }

    #[test]
    fn hover_and_completion_speak_qualified_names() {
        let mut server = Server::new(AnalysisOptions::default());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/namespaces.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": NAMESPACES,
            }},
        }));

        // Hovering a namespace segment names the namespace.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 20, "method": "textDocument/hover",
            "params": { "textDocument": { "uri": uri },
                        "position": position_json(position(NAMESPACES, "A::Inner::Player", 3)) },
        }));
        let value = out[0]["result"]["contents"]["value"].as_str().unwrap();
        assert!(value.starts_with("**namespace** `A::Inner`"), "{value}");

        // Hovering the class segment describes the class, as before.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 21, "method": "textDocument/hover",
            "params": { "textDocument": { "uri": uri },
                        "position": position_json(position(NAMESPACES, "A::Inner::Player", 10)) },
        }));
        let value = out[0]["result"]["contents"]["value"].as_str().unwrap();
        assert!(value.starts_with("**class** `A::Inner::Player`"), "{value}");

        // Definition on the class is one place, not one per same-named class.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 22, "method": "textDocument/definition",
            "params": { "textDocument": { "uri": uri },
                        "position": position_json(position(NAMESPACES, "B::Player", 3)) },
        }));
        assert_eq!(out[0]["result"].as_array().unwrap().len(), 1, "{}", out[0]["result"]);

        // Completion is filtered by the qualified name in progress: with the
        // cursor on `Inner` in `A::Inner::Player` the candidates are the
        // namespace and what it holds, not every name in the index.
        let mut items = |position: LspPosition| -> Vec<String> {
            let out = server.handle(&json!({
                "jsonrpc": "2.0", "id": 23, "method": "textDocument/completion",
                "params": { "textDocument": { "uri": uri }, "position": position_json(position) },
            }));
            out[0]["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["label"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            items(position(NAMESPACES, "A::Inner::Player", 3)),
            vec!["A::Inner", "A::Inner::Player"]
        );
        assert_eq!(items(position(NAMESPACES, "A::Player", 3)), vec!["A::Player"]);
    }

    #[test]
    fn completion_lists_a_namespace_while_its_name_is_still_being_typed() {
        // What an editor sends mid-keystroke: `A::` with the right-hand side
        // not written yet. The file does not parse, and the recovered tree still
        // indexes the declarations above — which is what makes the namespace
        // completable rather than just unknown.
        let source = format!("{NAMESPACES}\nint pending() {{\n    A::\n}}\n");
        let mut server = Server::new(AnalysisOptions::default());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/namespaces.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": source,
            }},
        }));
        // One past the separator: `A::` has nothing on its right-hand side yet.
        let position = position(&source, "    A::\n", 7);
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 24, "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": position_json(position) },
        }));
        let labels: Vec<&str> = out[0]["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["A::Inner", "A::Inner::Player", "A::Player"], "{labels:?}");
    }

    #[test]
    fn completion_offers_names_by_prefix() {
        let analyzed = analyze(PROGRAM);
        let label = |completions: Vec<index::Completion>| -> Vec<String> {
            completions.into_iter().map(|c| c.label).collect()
        };
        assert_eq!(label(analyzed.index.complete("Thing")), vec!["Thing"]);
        // A selector with its colon, so a keyword send can be completed.
        assert_eq!(label(analyzed.index.complete("run")), vec!["run:"]);
        // Everything the index knows, when nothing has been typed.
        let all = label(analyzed.index.complete(""));
        assert!(all.contains(&"Point".to_string()), "{all:?}");
        assert!(all.contains(&"helper".to_string()), "{all:?}");
        assert!(all.contains(&"run:".to_string()), "{all:?}");
        // A class is offered as a Class completion item (the completion enum
        // numbers Class 7, not the SymbolKind's 5).
        let thing = analyzed.index.complete("Thing");
        assert_eq!(thing[0].kind, 7);
        assert_eq!(thing[0].detail, "@interface:jeti_root");
        // A class declared twice is one candidate, not two.
        assert_eq!(label(analyzed.index.complete("Thing")).len(), 1);
    }

    #[test]
    fn hover_describes_the_declaration() {
        let mut server = Server::new(AnalysisOptions::default());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/hover.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": PROGRAM,
            }},
        }));

        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "textDocument/hover",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 2, "character": 11 } },
        }));
        let contents = &out[0]["result"]["contents"];
        assert_eq!(contents["kind"], "markdown");
        let value = contents["value"].as_str().unwrap();
        assert!(value.starts_with("**class** `Thing`"), "{value}");
        assert!(value.contains("@interface:jeti_root"), "{value}");
        assert!(value.contains("```jeti\n@interface Thing : jeti_root"), "{value}");
        assert!(value.contains("_Declared in "), "{value}");
        assert!(value.contains("_2 declarations_"), "{value}");

        // A position with no symbol has no hover, rather than an empty one.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 7, "method": "textDocument/hover",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 0 } },
        }));
        assert!(out[0]["result"].is_null());

        // Completion answers from the same cached index, and carries the
        // completion kind (Class = 7).
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 8, "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 2, "character": 11 } },
        }));
        let items = out[0]["result"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0]["label"], "Thing");
        assert_eq!(items[0]["kind"], 7);
    }

    /// Class hierarchy + a local + a class-method send + a scalar local.
    const MEMBERS: &str = "\
@interface Base : jeti_root
- (void)baseMethod;
@end

@interface Widget : Base
- (void)widgetMethod:(int)n;
+ (Widget *)make;
@property int count;
@end

@implementation Widget
- (void)widgetMethod:(int)n {
    Widget *w;
    int scalar;
    [self widgetMethod:0];
    [w widgetMethod:1];
    [Widget make];
    [scalar widgetMethod:2];
}
@end
";

    /// 0-based line + UTF-16 character `extra` characters into the first
    /// occurrence of `needle`.
    fn position(text: &str, needle: &str, extra: usize) -> LspPosition {
        let index = text.find(needle).expect("needle") + extra;
        let line = text[..index].matches('\n').count();
        let line_start = text[..index].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let character = text[line_start..index].encode_utf16().count();
        LspPosition { line: line as u32, character: character as u32 }
    }

    /// A `LspPosition` as the wire spells it (`LspPosition` itself is a
    /// compiler-side type, not a serializable one).
    fn position_json(position: LspPosition) -> Value {
        json!({ "line": position.line, "character": position.character })
    }

    #[test]
    fn member_completion_follows_the_class_chain() {
        let analyzed = analyze(MEMBERS);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);
        let labels = |members: Vec<index::Completion>| -> Vec<String> {
            members.into_iter().map(|m| m.label).collect()
        };

        // An object receiver gets its class's instance members *and* the
        // inherited ones, but never the class methods.
        let instance = labels(analyzed.index.member_completion("Widget", false, ""));
        assert_eq!(instance, vec!["baseMethod", "count", "widgetMethod:"], "{instance:?}");
        // A class receiver gets the class methods only.
        let class_methods = labels(analyzed.index.member_completion("Widget", true, ""));
        assert_eq!(class_methods, vec!["make"]);
        // The prefix filters, and the completion kind is the completion enum's
        // (Method = 2).
        let filtered = analyzed.index.member_completion("Widget", false, "widget");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].label, "widgetMethod:");
        assert_eq!(filtered[0].kind, 2);
        assert_eq!(filtered[0].detail, "- widgetMethod:");
    }

    #[test]
    fn a_send_completes_the_receivers_members_only_in_selector_position() {
        let analyzed = analyze(MEMBERS);

        // `[self …` — the enclosing method's class, for a selector position.
        let receiver = analyzed.index.send_receiver(position(MEMBERS, "[self widgetMethod", 6));
        assert_eq!(receiver, Some(("Widget".to_string(), false)));

        // `[w …` — a local, resolved through the scope.
        let receiver = analyzed.index.send_receiver(position(MEMBERS, "[w widgetMethod", 3));
        assert_eq!(receiver, Some(("Widget".to_string(), false)));

        // `[Widget …` — a class receiver, so class methods answer.
        let receiver = analyzed.index.send_receiver(position(MEMBERS, "[Widget make", 8));
        assert_eq!(receiver, Some(("Widget".to_string(), true)));

        // On the receiver itself, and inside an argument: not selector
        // positions, so no members (completion falls back to global names).
        assert_eq!(analyzed.index.send_receiver(position(MEMBERS, "[w widgetMethod", 1)), None);
        assert_eq!(analyzed.index.send_receiver(position(MEMBERS, "[w widgetMethod:1", 16)), None);
        // A scalar receiver has no members to offer, and neither does an
        // unknown name.
        assert_eq!(analyzed.index.send_receiver(position(MEMBERS, "[scalar widgetMethod", 8)), None);
    }

    #[test]
    fn dot_access_resolves_its_receiver_from_the_text() {
        // The AST has no node for `w.` until the member name is typed, so this
        // one case reads the text.
        let text = "    w.count;\n";
        assert_eq!(access_receiver_at(text, LspPosition { line: 0, character: 6 }).as_deref(), Some("w"));
        let text = "    node->next;\n";
        assert_eq!(access_receiver_at(text, LspPosition { line: 0, character: 10 }).as_deref(), Some("node"));
        // Not an access position, and not an identifier.
        assert_eq!(access_receiver_at("w.count", LspPosition { line: 0, character: 4 }), None);
        assert_eq!(access_receiver_at("1.", LspPosition { line: 0, character: 2 }), None);
    }

    /// A hierarchy plus a two-part selector, a class method and a free function,
    /// for signature help.
    const SIGNATURES: &str = "\
@interface Base : jeti_root
- (void)baseMethod;
@end

@interface Widget : Base
- (void)place:(Widget *)w at:(int)index;
+ (Widget *)makeWith:(int)n;
@end

@implementation Widget
- (void)place:(Widget *)w at:(int)index {
    [self place:w at:index];
    [self baseMethod];
    [Widget makeWith:3];
}
@end

int helper(int value, int other) {
    return helper(1, 2);
}
";

    #[test]
    fn signature_help_reports_the_signature_and_the_active_parameter() {
        let analyzed = analyze(SIGNATURES);

        // Inside the first argument of a two-part selector.
        let help = analyzed
            .index
            .signature_help(position(SIGNATURES, "[self place:w at:index", 12))
            .expect("a signature");
        assert_eq!(help.active_parameter, 0);
        assert_eq!(help.active_signature, 0);
        assert_eq!(help.signatures[0].label, "- (void)place:(Widget *)w at:(int)index");
        assert_eq!(help.signatures[0].parameters, vec!["(Widget *)w", "(int)index"]);

        // Inside the second argument.
        let help = analyzed
            .index
            .signature_help(position(SIGNATURES, "[self place:w at:index", 20))
            .expect("a signature");
        assert_eq!(help.active_parameter, 1);

        // A class method, and an inherited colon-less one.
        let help = analyzed
            .index
            .signature_help(position(SIGNATURES, "[Widget makeWith:3", 16))
            .expect("a signature");
        assert_eq!(help.signatures[0].label, "+ (Widget *)makeWith:(int)n");
        let help = analyzed
            .index
            .signature_help(position(SIGNATURES, "[self place:", 0))
            .expect("a signature");
        assert_eq!(help.signatures[0].label, "- (void)place:(Widget *)w at:(int)index");

        // A free call, with the argument index following the commas.
        let help = analyzed
            .index
            .signature_help(position(SIGNATURES, "helper(1, 2", 8))
            .expect("a signature");
        assert_eq!(help.active_parameter, 0);
        assert_eq!(help.signatures[0].label, "int helper(int value, int other)");
        assert_eq!(help.signatures[0].parameters, vec!["int value", "int other"]);
        let help = analyzed
            .index
            .signature_help(position(SIGNATURES, "helper(1, 2", 12))
            .expect("a signature");
        assert_eq!(help.active_parameter, 1);

        // A declaration is not a call: no signature there.
        assert!(analyzed.index.signature_help(position(SIGNATURES, "int helper", 4)).is_none());
        // Neither is a nullary send with nothing to fill in.
        let help = analyzed.index.signature_help(position(SIGNATURES, "[self baseMethod", 6)).expect("a signature");
        assert!(help.signatures[0].parameters.is_empty());
        assert_eq!(help.signatures[0].label, "- (void)baseMethod");
    }

    #[test]
    fn the_signature_request_answers_from_the_cache() {
        let mut server = Server::new(AnalysisOptions::default());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/signature.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": SIGNATURES,
            }},
        }));

        // Inside the second argument of `[self place:… at:…]`.
        let cursor = position(SIGNATURES, "[self place:w at:index", 20);
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 11, "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": cursor.line, "character": cursor.character },
            },
        }));
        let help = &out[0]["result"];
        assert_eq!(help["activeParameter"], 1);
        assert_eq!(help["activeSignature"], 0);
        assert_eq!(help["signatures"][0]["label"], "- (void)place:(Widget *)w at:(int)index");
        assert_eq!(help["signatures"][0]["parameters"][1]["label"], "(int)index");

        // Outside any call the answer is `null`, so the editor shows nothing.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 12, "method": "textDocument/signatureHelp",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 0 } },
        }));
        assert!(out[0]["result"].is_null());

        // A colon-less selector has no parameter, so the field is omitted.
        let cursor = position(SIGNATURES, "[self baseMethod", 6);
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 13, "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": cursor.line, "character": cursor.character },
            },
        }));
        assert_eq!(out[0]["result"]["signatures"][0]["label"], "- (void)baseMethod");
        assert!(out[0]["result"].get("activeParameter").is_none(), "{}", out[0]["result"]);
    }

    #[test]
    fn a_parse_error_still_offers_the_recovered_symbols() {
        // The editor path continues past a parse error (see
        // `Pipeline::finish_analysis`): the syntax mistake is reported *and* the
        // outline of what the parser recovered is still there, so one forgotten
        // `;` no longer blanks every request about the file.
        let analyzed = analyze("int main(void) { return 0 }\n");
        assert!(!analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);
        let symbols = analyzed.index.document_symbols("/tmp/jeti_lsp_unit/symbols.jeti");
        let names: Vec<&str> = symbols.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["main"], "{symbols:#?}");
        assert!(!analyzed.index.definitions("main").is_empty());
    }

    #[test]
    fn a_front_end_that_cannot_run_offers_no_symbols() {
        // A failure with no positions behind it (here: an import that cannot be
        // opened) yields diagnostics and nothing to answer requests from.
        let analyzed = analyze("#import \"definitely_missing.hje\"\n");
        assert!(!analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);
        assert!(analyzed.index.document_symbols("/tmp/jeti_lsp_unit/symbols.jeti").is_empty());
    }

    #[test]
    fn definition_and_outline_requests_answer_from_the_cache() {
        let mut server = Server::new(AnalysisOptions::default());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/request.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": PROGRAM,
            }},
        }));

        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } },
        }));
        let names: Vec<&str> = out[0]["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Point", "Thing", "Thing", "helper"]);

        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "textDocument/definition",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 2, "character": 11 } },
        }));
        let locations = out[0]["result"].as_array().unwrap();
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0]["uri"], uri);
        assert_eq!(locations[0]["range"]["start"], json!({"line": 2, "character": 0}));

        // No symbol under the cursor is `null`, never a fabricated target.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/definition",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 0, "character": 0 } },
        }));
        assert!(out[0]["result"].is_null());
    }
}
