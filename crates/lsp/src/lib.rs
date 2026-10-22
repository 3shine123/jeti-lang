//! Minimal Language Server for Jeti — plan 阶段 6 of
//! `docs/plans/source_locations_debug_lsp_plan.md`.
//!
//! Scope of this skeleton, deliberately small and honest:
//!
//! - `initialize` / `initialized` / `shutdown` / `exit` lifecycle, plus
//!   `textDocument/didOpen|didChange|didClose` with **incremental** document
//!   sync. `didOpen` is answered by `textDocument/publishDiagnostics` right
//!   away; `didChange` only marks the buffer dirty — the re-analysis runs when
//!   the buffer has been quiet for the quiet period (150 ms by default) and the
//!   next message gives the server the opportunity to run it. High-frequency
//!   edits are therefore coalesced into one front-end run; the publish always
//!   carries the state the buffer settled on, never an intermediate one.
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jeti_cst::diagnostic::{Diagnostic, Severity};
use jeti_cst::source_map::{LspPosition, LspRange, SourceFile};
use jetic::pipeline::Pipeline;
use serde_json::{json, Value};

use crate::index::{typed_name_at, Resolved};

pub mod cheader;
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
    /// When (epoch) the last applied edit landed. `None` while the text is
    /// what the last analysis ran on.
    dirty_since: Option<Duration>,
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
    /// How long an edited buffer stays unanalyzed after its last edit, so a
    /// burst of keystrokes runs the front end once, not once per keystroke.
    quiet_period: Duration,
    /// The clock the quiet period is measured against (epoch). Injectable so
    /// tests can drive the window without sleeping.
    clock: Box<dyn Fn() -> Duration>,
    shutdown_received: bool,
}

/// The wall clock, as [`Server::clock`] reads it.
fn system_now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO)
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
            quiet_period: Duration::from_millis(150),
            clock: Box::new(system_now),
            shutdown_received: false,
        }
    }

    /// A different coalescing window. Zero means "every message re-analyzes
    /// whatever is dirty" — the session tests run with that so they never
    /// depend on real elapsed time.
    pub fn with_quiet_period(mut self, quiet: Duration) -> Self {
        self.quiet_period = quiet;
        self
    }

    /// Now, the way the quiet period sees it.
    fn now(&self) -> Duration {
        (self.clock)()
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
        // The re-analysis an edit deferred becomes runnable the moment the
        // buffer has been quiet for the quiet period; this message is the
        // server's next opportunity to run it, so the settled diagnostics go
        // out ahead of this message's own answer. `shutdown`/`exit` skip it —
        // the client is leaving and the publish would reach no one.
        let mut out = if method == "shutdown" || method == "exit" {
            Vec::new()
        } else {
            self.flush_due()
        };
        out.extend(match method {
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
                        // 1 = Full: every change carries the whole buffer. The
                        // server re-analyzes from scratch on each flush anyway,
                        // so incremental ranges buy nothing here — but they can
                        // desync: a range whose position has drifted out of the
                        // server's text is dropped, and the diagnostics freeze
                        // on stale content until a whole-buffer edit re-syncs.
                        // A full replacement can never desync. `apply_change`
                        // still understands ranges, for other clients.
                        "textDocumentSync": 1,
                        "documentSymbolProvider": true,
                        "definitionProvider": true,
                        "referencesProvider": true,
                        "renameProvider": true,
                        "hoverProvider": true,
                        "documentLinkProvider": true,
                        // clangd-parity coloring: the server classifies what
                        // only the compiler knows (types, classes, fields,
                        // enum members, macros); the extension maps these
                        // types onto standard TextMate scopes via
                        // `semanticTokenScopes` so the theme's own
                        // bold/yellow rules apply unchanged.
                        "semanticTokensProvider": {
                            "legend": {
                                "tokenTypes": [
                                    "namespace", "class", "type", "enum",
                                    "function", "variable", "property", "macro", "enumMember",
                                    "keyword",
                                ],
                                "tokenModifiers": ["static", "defaultLibrary"],
                            },
                            "full": true,
                            // The server re-pushes this request after every
                            // analysis flush: a client that asked for tokens
                            // before the first analysis finished (a reloaded
                            // window reopening its tabs) cached an empty run,
                            // and only this request makes it fetch again.
                            "refreshSupport": true,
                        },
                        // No `resolveProvider`: everything a completion item
                        // needs (label, kind, detail) is in the same answer.
                        // Trigger characters mirror the extension's
                        // registration: `:` for selector parts, `(`/`,`
                        // for argument lists, `-` for method prefixes.
                        "completionProvider": {
                            "resolveProvider": false,
                            "triggerCharacters": [":", "(", ",", "-"],
                        },
                        // `:` is where a keyword selector's next parameter
                        // starts, so the popup appears exactly when it can say
                        // something.
                        "signatureHelpProvider": { "triggerCharacters": [":", "(", ",", "-"] },
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
                        self.docs.insert(
                            uri.to_string(),
                            Document { path, text: text.to_string(), dirty_since: None },
                        );
                        self.publish_and_index(uri)
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
                // the text the previous one produced. The edit only marks the
                // buffer dirty: the front end runs once the buffer has been
                // quiet for the quiet period and the next message arrives
                // (`flush_due`), so a burst of keystrokes is one analysis.
                let now = self.now();
                if let (Some(changes), Some(doc)) = (changes, self.docs.get_mut(uri)) {
                    let mut changed = false;
                    for change in changes {
                        if apply_change(&mut doc.text, change) {
                            changed = true;
                        }
                    }
                    if changed {
                        doc.dirty_since = Some(now);
                    }
                }
                Vec::new()
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
            // Every reference to the symbol under the cursor, in the edited
            // document. LSP answers this one as an array — possibly empty —
            // rather than `null`: an empty list is a complete answer.
            "textDocument/references" => {
                let uri = request_uri(message);
                let position = request_position(message);
                let locations = self.references(uri, position);
                vec![response(id, Value::Array(locations))]
            }
            // What a rename should preselect: the name's range, or `null` for
            // a name that cannot be renamed (not a symbol, a namespace in v1).
            "textDocument/prepareRename" => {
                let uri = request_uri(message);
                let position = request_position(message);
                let result = self
                    .rename_range(uri, position)
                    .map_or(Value::Null, range_json);
                vec![response(id, result)]
            }
            // The symbol under the cursor, renamed everywhere it occurs in the
            // edited document. The answer is a workspace edit; an invalid new
            // name is an empty edit, not a guess.
            "textDocument/rename" => {
                let uri = request_uri(message);
                let position = request_position(message);
                let new_name = message
                    .pointer("/params/newName")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                vec![response(id, self.rename(uri, position, new_name))]
            }
            // What the symbol under the cursor *is*, with its declaration text.
            "textDocument/hover" => {
                let position = request_position(message);
                let uri = request_uri(message);
                let mut hover = Value::Null;
                if let (Some(doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) {
                    let text_of = self.text_of();
                    let resolve = |path: &str| text_or_disk(&text_of, path);
                    // An include path under the cursor: not a symbol and not a
                    // field — the header card is the only answer.
                    if let Some(markdown) = header_hover_markdown(&doc.text, position) {
                        hover = json!({
                            "contents": { "kind": "markdown", "value": markdown },
                        });
                    }
                    // A struct/union (or class ivar) field under the cursor is
                    // not a symbol, so the declaration index has no entry for
                    // it — answer from the computed layout. It beats a
                    // same-named top-level declaration: inner scope wins, as
                    // in clangd.
                    if let Some(word) = word_at(&doc.text, position) {
                        if let Some(fref) = index.field_at(&word, position) {
                            if let Some(markdown) =
                                field_hover_markdown(index, fref, &word, &resolve)
                            {
                                hover = json!({
                                    "contents": { "kind": "markdown", "value": markdown },
                                });
                            }
                        }
                    }
                    if hover.is_null() {
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
                                // A struct/union/typedef declaration gets the
                                // clangd-parity layout card (Size/alignment +
                                // per-field offsets) in place of the generic
                                // declaration card.
                                let is_aggregate = first.kind == index::kind::STRUCT
                                    || first.detail == "typedef";
                                if is_aggregate {
                                    let layout = index.layouts.get(&name)
                                        .or_else(|| index.layouts.get(first.name.as_str()));
                                    let aliased = index.typedef_types.get(&name)
                                        .or_else(|| index.typedef_types.get(first.name.as_str()))
                                        .and_then(|t| t.name.clone());
                                    hover = json!({
                                        "contents": {
                                            "kind": "markdown",
                                            "value": hover_markdown_with_layout(first, layout, aliased.as_ref(), &resolve),
                                        },
                                    });
                                } else {
                                    let signature = index
                                        .free_function(&name)
                                        .or_else(|| index.free_function(first.name.as_str()));
                                    hover = json!({
                                        "contents": {
                                            "kind": "markdown",
                                            "value": hover_markdown(first, locations.len(), signature, &resolve),
                                        },
                                    });
                                }
                            }
                        }
                        None => {}
                    }
                    }
                    // The declaration index only answers for file-level
                    // symbols. Everything else the editor parks a cursor on —
                    // a local/parameter/ivar (`sp` in `sp.label = nil`), a
                    // builtin type keyword (`int`, `SEL`), a Jeti keyword
                    // (`@interface`), a C17 library function the passthrough
                    // headers declare (`printf`) — still deserves a card.
                    if hover.is_null() {
                        if let Some(markdown) = self.fallback_hover(index, &doc.text, position) {
                            hover = json!({
                                "contents": { "kind": "markdown", "value": markdown },
                            });
                        }
                    }
                }
                vec![response(id, hover)]
            }
            // Include paths become links: clangd-parity underline, a whole-path
            // hover highlight, and cmd+click to the resolved file.
            "textDocument/documentLink" => {
                let uri = request_uri(message);
                // The link target resolves through the same effective search
                // path the diagnostics ran with — a project's compile_flags.txt
                // `-I` dirs must reach the resolver too, or a header the
                // analysis accepts shows no underline (probe-verified:
                // `LibUI.hje` under `examples/03_LibUI/compile_flags.txt`).
                let links = self
                    .docs
                    .get(uri)
                    .map(|doc| (doc.path.clone(), doc.text.clone()))
                    .map(|(path, text)| {
                        let dirs = self.effective_options(&path).search_dirs;
                        self.document_links(uri, &text, &dirs)
                    })
                    .unwrap_or_default();
                vec![response(id, Value::Array(links))]
            }
            // The compiler's own coloring: identifiers classified through the
            // same run of analysis every other request answered from.
            "textDocument/semanticTokens/full" => {
                let uri = request_uri(message);
                let result = match (self.docs.get(uri), self.indexes.get(uri)) {
                    (Some(doc), Some(index)) => json!({ "data": index.semantic_tokens(&doc.text) }),
                    _ => json!({ "data": [] }),
                };
                vec![response(id, result)]
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
                    if let Some(receiver) = receiver {
                        for member in index.member_completion(
                            receiver.class.as_deref(),
                            &receiver.protocols,
                            receiver.is_class_receiver,
                            &prefix,
                        ) {
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
        });
        out
    }

    /// How long until the nearest dirty document's re-analysis is due —
    /// `None` while no buffer is waiting. The stdio loop polls stdin against
    /// this deadline, so the quiet period fires on the clock alone: the last
    /// edit of a burst sends no following message, and blocking indefinitely
    /// would leave its result unpublished forever (the editor keeps showing
    /// the old error, or never shows the new one).
    pub fn next_flush_in(&self) -> Option<Duration> {
        let now = self.now();
        self.docs
            .values()
            .filter_map(|doc| doc.dirty_since)
            .map(|since| since + self.quiet_period)
            .min()
            .map(|deadline| deadline.saturating_sub(now))
    }

    /// Re-analyze the documents whose last edit is older than the quiet
    /// period — the settled result of every edit since the last analysis.
    /// Intermediate states are never published; only what the buffer rests on.
    ///
    /// Public for the same reason as [`Self::next_flush_in`]: the stdio loop
    /// must call it when the clock alone says the buffer has settled.
    pub fn flush_due(&mut self) -> Vec<Value> {
        let now = self.now();
        let due: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, doc)| {
                doc.dirty_since.is_some_and(|since| since + self.quiet_period <= now)
            })
            .map(|(uri, _)| uri.clone())
            .collect();
        due.into_iter().flat_map(|uri| {
            let messages = self.publish_and_index(&uri);
            // That analysis is the one the edit waited for: clear the mark,
            // or every following message would re-run and re-publish it.
            if let Some(doc) = self.docs.get_mut(&uri) {
                doc.dirty_since = None;
            }
            messages
        }).collect()
    }

    /// Analyze `uri` and keep the index, so publishing is the only place the
    /// front end runs for a document. Returns the diagnostics message plus the
    /// semantic-tokens refresh request.
    fn publish_and_index(&mut self, uri: &str) -> Vec<Value> {
        let Some(path) = self.docs.get(uri).map(|doc| doc.path.clone()) else {
            return vec![publish_empty(uri)];
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
        // Ask the client to re-fetch the tokens it may have cached while this
        // document's first analysis was still running (a reloaded window
        // reopening its tabs): only this request makes VS Code fetch again.
        let diagnostics = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": analyzed.diagnostics },
        });
        let refresh = json!({
            "jsonrpc": "2.0",
            "id": format!("semantic-tokens-refresh:{uri}"),
            "method": "workspace/semanticTokens/refresh",
            "params": Value::Null,
        });
        vec![diagnostics, refresh]
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
    /// Hover answers the declaration index cannot give: locals/params/ivars
    /// (the scopes the completion machinery already walks), builtin type
    /// keywords, Jeti keywords, and the C17 library surface the passthrough
    /// headers declare. Order matters: a real declaration beats a local, a
    /// local beats a keyword card, a keyword beats the library guess.
    fn fallback_hover(
        &self,
        index: &index::SymbolIndex,
        text: &str,
        position: LspPosition,
    ) -> Option<String> {
        let word = word_at(text, position)?;
        if let Some((local, owner)) = index.local_hover_at(position, &word) {
            let mut markdown = format!("**local variable** `{}`", word);
            markdown.push_str(&format!("\n\nType: {}", local.type_spelling));
            if !owner.is_empty() && owner != "<file>" {
                markdown.push_str(&format!("\nin `{owner}`"));
            }
            if let Some(line) = local.decl_line {
                if let Some(decl) = text.lines().nth(line as usize - 1) {
                    let decl = decl.trim();
                    if !decl.is_empty() {
                        markdown.push_str(&format!("\n\n```jeti\n{decl}\n```"));
                    }
                }
                markdown.push_str(&format!("\n\n_Declared in this file, line {line}_"));
            }
            return Some(markdown);
        }
        if let Some(markdown) = builtin_type_markdown(&word) {
            return Some(markdown);
        }
        if let Some(markdown) = jeti_keyword_markdown(&word) {
            return Some(markdown);
        }
        // The passthrough C surface, extracted from the real toolchain —
        // clangd's card shape: return-type arrow, parameter list, full
        // prototype. The old "see the header for the signature" line is gone.
        if let Some(markdown) = self.c_library_markdown(&word, text, position) {
            return Some(markdown);
        }
        None
    }

    /// The clangd-shaped card for a C-library symbol from the `cc -E`-extracted
    /// table. Titles follow clangd's (`function`, `type-alias`, `macro`); a
    /// typedef's `Type:` is the resolved chain terminal; a macro card shows
    /// the real `#define` and the expansion with this use's arguments.
    fn c_library_markdown(
        &self,
        word: &str,
        text: &str,
        position: LspPosition,
    ) -> Option<String> {
        let table = cheader::signatures_for(&index::SymbolIndex::system_headers_in(text));
        // A real declaration beats a macro *spelling* (macOS fortify defines
        // `memset(...)` as a macro, but clangd's card shows the prototype);
        // macro-only names (`EOF`, `errno` on macOS) take the macro card.
        if let Some((kind, decl)) = table.decl(word) {
            let title = match kind {
                cheader::DeclKind::Function => "function",
                cheader::DeclKind::Variable => "variable",
                cheader::DeclKind::Typedef => "type-alias",
            };
            let mut markdown = format!("**{title}** `{word}`");
            markdown.push_str(&format!("\n\nprovided by `<{}>`", decl.header));
            match kind {
                cheader::DeclKind::Function => {
                    markdown.push_str(&format!("\n\n→ {}", decl.type_spelling));
                    if !decl.params.is_empty() {
                        markdown.push_str("\n\nParameters:");
                        for param in &decl.params {
                            markdown.push_str(&format!("\n• `{param}`"));
                        }
                    }
                }
                // clangd shows the resolved terminal, not the alias one hop
                // away (`va_list` → `char *`).
                cheader::DeclKind::Typedef => {
                    markdown
                        .push_str(&format!("\n\nType: {}", table.resolve_type(&decl.type_spelling)));
                }
                cheader::DeclKind::Variable => {
                    markdown.push_str(&format!("\n\nType: {}", decl.type_spelling));
                }
            }
            markdown.push_str(&format!("\n\n```jeti\n{}\n```", decl.label));
            return Some(markdown);
        }
        let macro_def = table.macro_def(word)?;
        let spelling = if macro_def.params.is_empty() {
            format!("#define {word} {}", macro_def.body)
        } else {
            format!("#define {word}({}) {}", macro_def.params.join(", "), macro_def.body)
        };
        let mut markdown = format!("**macro** `{word}`");
        markdown.push_str(&format!("\n\nprovided by `<{}>`", macro_def.header));
        markdown.push_str(&format!("\n\n```jeti\n{spelling}\n```"));
        // The expansion with this use's actual arguments — only when the
        // cursor sits on a call-shaped use (`va_start(ap, fmt)`), never on a
        // bare mention.
        let args = macro_args_at(text, position, word);
        if !args.is_empty() {
            let expanded = cheader::substitute_params(&macro_def.body, &macro_def.params, &args);
            markdown.push_str(&format!("\n\n```jeti\n// Expands to\n{expanded}\n```"));
        }
        Some(markdown)
    }

    fn text_of(&self) -> impl Fn(&str) -> Option<String> + '_ {        move |path: &str| {
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

    /// `#include <...>` / `#import "..."` lines become links: the whole
    /// bracketed path is the range (clangd highlights the brackets too), and
    /// the target is the resolved file when the search path — project dirs,
    /// then the C driver's system dirs — finds one. A path nothing resolves
    /// still gets its range, without a target.
    fn document_links(&self, uri: &str, text: &str, search_dirs: &[String]) -> Vec<Value> {
        let doc_dir = self
            .docs
            .get(uri)
            .and_then(|d| std::path::Path::new(&d.path).parent().map(|p| p.to_string_lossy().to_string()));
        let mut links = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let Some((open, end, raw)) = include_span(line) else {
                continue;
            };
            let mut link = json!({
                "range": range_json(LspRange {
                    start: LspPosition { line: i as u32, character: open as u32 },
                    end: LspPosition { line: i as u32, character: end as u32 },
                }),
            });
            if let Some(path) = self.resolve_header(&raw, doc_dir.as_deref(), search_dirs) {
                link["target"] = json!(file_uri(&path));
            }
            links.push(link);
        }
        links
    }

    /// First existing `dir / raw` across the document's own directory, the
    /// document's effective search path (server flags plus the project's
    /// configuration file — the same dirs the analysis ran with), and the C
    /// driver's system directories — project paths shadow system paths,
    /// clangd's precedence.
    fn resolve_header(&self, raw: &str, doc_dir: Option<&str>, search_dirs: &[String]) -> Option<String> {
        let mut dirs: Vec<String> = Vec::new();
        if let Some(d) = doc_dir {
            dirs.push(d.to_string());
        }
        dirs.extend(search_dirs.iter().cloned());
        dirs.extend(system_include_dirs().iter().cloned());
        for dir in &dirs {
            let candidate = std::path::Path::new(dir).join(raw);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
        None
    }

    fn references(&self, uri: &str, position: LspPosition) -> Vec<Value> {
        let (Some(doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) else {
            return Vec::new();
        };
        index
            .find_references(&doc.text, position)
            .iter()
            .map(|location| {
                json!({
                    "uri": self.uri_for(&location.path),
                    "range": range_json(location.range),
                })
            })
            .collect()
    }

    fn rename_range(&self, uri: &str, position: LspPosition) -> Option<LspRange> {
        let (Some(doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) else {
            return None;
        };
        index.rename_range_at(&doc.text, position)
    }

    fn rename(&self, uri: &str, position: LspPosition, new_name: &str) -> Value {
        // The compiler's identifier rules, minus the shape of a selector (the
        // client typed what it typed; the diagnostics will say if it is wrong).
        let valid_name = new_name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && new_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
        let (Some(doc), Some(index)) = (self.docs.get(uri), self.indexes.get(uri)) else {
            return json!({ "changes": {} });
        };
        if !valid_name {
            return json!({ "changes": {} });
        }
        let edits: Vec<Value> = index
            .rename_ranges(&doc.text, position)
            .iter()
            .map(|range| json!({ "range": range_json(*range), "newText": new_name }))
            .collect();
        json!({ "changes": { self.uri_for(&doc.path): edits } })
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

/// The identifier (or `@`-keyword) under the cursor. LSP positions are
/// UTF-16 code units; `SourceFile::lsp_to_offset` does the one conversion
/// the boundary rules allow, then the scan walks plain bytes.
fn word_at(text: &str, position: LspPosition) -> Option<String> {
    let offset = SourceFile::lsp_to_offset(text, position)?;
    let bytes = text.as_bytes();
    let mut start = offset.min(bytes.len());
    // Anchor one-past-the-end cursors to the word before them (same rule
    // the index's word_bounds uses).
    if start == bytes.len() || !is_word_byte(bytes[start]) {
        if start == 0 || !is_word_byte(bytes[start - 1]) {
            return None;
        }
        start -= 1;
    }
    let mut end = start + 1;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    // An `@` immediately before the word makes it a Jeti keyword
    // (`@interface`) — one token for hover purposes. The slice keeps the
    // word only; the `@` is prepended explicitly (shifting `start` would
    // double it — the slice would already carry the `@`).
    let at = start > 0 && bytes[start - 1] == b'@';
    let word = &text[start..end];
    Some(if at { format!("@{word}") } else { word.to_string() })
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Byte bounds of the word `position` anchors to — the same rule `word_at`
/// uses (a one-past-the-end cursor belongs to the word before it).
fn word_span_at(text: &str, position: LspPosition) -> Option<(usize, usize)> {
    let offset = SourceFile::lsp_to_offset(text, position)?;
    let bytes = text.as_bytes();
    let mut start = offset.min(bytes.len());
    if start == bytes.len() || !is_word_byte(bytes[start]) {
        if start == 0 || !is_word_byte(bytes[start - 1]) {
            return None;
        }
        start -= 1;
    }
    let mut end = start + 1;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    Some((start, end))
}

/// The actual arguments of a macro *use* at `position` — `va_start(ap, fmt)`
/// with the cursor on `va_start` yields `["ap", "fmt"]`. Empty unless the
/// word is the macro itself and a complete `(...)` call follows it: a bare
/// mention gets the definition card without an expansion.
fn macro_args_at(text: &str, position: LspPosition, word: &str) -> Vec<String> {
    let Some((start, end)) = word_span_at(text, position) else {
        return Vec::new();
    };
    if text[start..end] != *word || !text[end..].starts_with('(') {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut args = Vec::new();
    let mut current = String::new();
    for i in end..bytes.len() {
        match bytes[i] {
            b'(' => {
                depth += 1;
                if depth > 1 {
                    current.push('(');
                }
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    if !current.trim().is_empty() || !args.is_empty() {
                        args.push(current.trim().to_string());
                    }
                    return args;
                }
                current.push(')');
            }
            b',' if depth == 1 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => {
                let len = text[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                current.push_str(&text[i..i + len]);
            }
        }
    }
    Vec::new() // unpaired `(`: not a complete call
}

/// Builtin type keywords — the card clangd shows for `int`, terse and
/// factual. C17 6.2.5 spells the type system; ObjC adds the object kinds.
fn builtin_type_markdown(word: &str) -> Option<String> {
    let (name, desc) = match word {
        "void" => ("void", "no type (C17 6.2.5) — void-returning functions and `void *`"),
        "int" => ("int", "builtin integer type (C17 6.2.5p5)"),
        "char" => ("char", "character type, 1 byte (C17 6.2.5p15)"),
        "short" => ("short", "short integer (C17 6.2.5p5)"),
        "long" => ("long", "long integer (64 bits under LP64)"),
        "float" => ("float", "single-precision float (C17 6.2.5p10)"),
        "double" => ("double", "double-precision float (C17 6.2.5p10)"),
        "signed" => ("signed", "signed qualifier"),
        "unsigned" => ("unsigned", "unsigned qualifier"),
        "const" => ("const", "read-only qualifier (C17 6.7.3)"),
        "volatile" => ("volatile", "volatile qualifier (C17 6.7.3)"),
        "struct" => ("struct", "structure (C17 6.7.2.1) — hover a member name to see fields"),
        "union" => ("union", "union (C17 6.7.2.1)"),
        "enum" => ("enum", "enumeration (C17 6.7.2.2)"),
        "BOOL" => ("BOOL", "ObjC boolean (4-byte integer)"),
        "SEL" => ("SEL", "selector — the interned handle of a message name"),
        "id" => ("id", "object pointer wildcard — any object assigns to it"),
        "Class" => ("Class", "class-object pointer — the receiver of `+[… alloc]`"),
        "instancetype" => ("instancetype", "the instance type of the method's class (more precise than id)"),
        "self" => ("self", "the current receiver: the instance in an instance method, the class object in a class method"),
        "super" => ("super", "compile-time directive to message the superclass (still self)"),
        "nil" => ("nil", "the nil object constant (value 0)"),
        "NULL" => ("NULL", "the null pointer constant (value 0)"),
        _ => return None,
    };
    Some(format!("**`{name}`**\n\n{desc}"))
}

/// Jeti keyword cards for the `@` family (clangd shows nothing there; the
/// language server should at least say what the construct is).
fn jeti_keyword_markdown(word: &str) -> Option<String> {
    let (name, desc) = match word {
        "@interface" => ("@interface", "class/protocol declaration: methods, ivars, properties; members are implemented in @implementation"),
        "@implementation" => ("@implementation", "class implementation: method bodies live here; may be split from @interface across files (the owner TU holds the metadata)"),
        "@property" => ("@property", "declares a property — generates accessors and an ivar"),
        "@protocol" => ("@protocol", "protocol: a method contract; classes conform with <P>"),
        "@class" => ("@class", "forward declaration: names only, no layout"),
        "@end" => ("@end", "closes a class/protocol/implementation block"),
        "@try" => ("@try", "exception try block; pairs with @catch / @finally"),
        "@catch" => ("@catch", "catches an object thrown with @throw"),
        "@finally" => ("@finally", "cleanup that runs whether or not an exception was thrown"),
        "@throw" => ("@throw", "throws an exception object"),
        "@selector" => ("@selector", "compiles a message name into a SEL"),
        "@autoreleasepool" => ("@autoreleasepool", "autoreleasepool scope"),
        "@synchronized" => ("@synchronized", "mutex scope"),
        "@namespace" => ("@namespace", "namespace: declarations inside get a qualified prefix"),
        "@using" => ("@using", "imports a fully-qualified name under a short name"),
        "@public" => ("@public", "ivar visibility: accessible from anywhere"),
        "@private" => ("@private", "ivar visibility: this class only"),
        "@protected" => ("@protected", "ivar visibility: this class and subclasses"),
        _ => return None,
    };
    Some(format!("**`{name}`**\n\n{desc}"))
}

/// Markdown hover: what the declaration is, its own source text, and where it
/// lives. `declarations` is how many the name resolved to, so a selector with a
/// declaration and an implementation says so instead of hiding one.
fn hover_markdown(
    location: &Location,
    declarations: usize,
    signature: Option<&index::Signature>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> String {
    let mut markdown = format!("**{}** `{}`", kind_name(location.kind), location.name);
    if !location.detail.is_empty() && location.detail != location.name {
        markdown.push_str(&format!(" · {}", location.detail));
    }
    markdown.push('\n');
    // clangd spells the free function's return type and parameters out; the
    // signature already carries both (label `ret name(p1, p2)`).
    if let Some(sig) = signature {
        if let Some(ret) = sig
            .label
            .find(&format!(" {}(", location.name))
            .map(|i| sig.label[..i].trim())
            .filter(|ret| !ret.is_empty())
        {
            markdown.push_str(&format!("\n→ {ret}\n"));
        }
        if !sig.parameters.is_empty() {
            markdown.push_str("\nParameters:");
            for param in &sig.parameters {
                markdown.push_str(&format!("\n• `{param}`"));
            }
            markdown.push('\n');
        }
    }
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

/// The directory lines between cc's `#include <...>` banner and its end
/// marker; the `(framework directory)` annotation is stripped (the directory
/// is real, the suffix is cc's prose).
fn parse_cc_search_list(text: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("#include <...> search starts here:") {
            inside = true;
            continue;
        }
        if line.starts_with("End of search list.") {
            break;
        }
        if inside {
            let dir = line.trim();
            let dir = dir.strip_suffix(" (framework directory)").unwrap_or(dir);
            if !dir.is_empty() {
                dirs.push(dir.to_string());
            }
        }
    }
    dirs
}

/// The C driver's system include directories, probed once per process —
/// clangd's own trick for knowing where `stdio.h` lives. Display-layer only:
/// documentLink and the header hover resolve C headers through these, while
/// the analysis never parses passthrough C headers, so nothing here enters
/// the pipeline's search path.
fn system_include_dirs() -> &'static [String] {
    static DIRS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    DIRS.get_or_init(|| {
        let Ok(output) = std::process::Command::new("cc")
            .args(["-E", "-xc", "-", "-v"])
            .stdin(std::process::Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        parse_cc_search_list(&String::from_utf8_lossy(&output.stderr))
    })
}

/// The `<...>` / `"..."` span of an `#include`/`#import` line: byte offsets of
/// the opening bracket and one past the closer, plus the raw path. Include
/// lines are ASCII, so the offsets double as UTF-16 columns.
fn include_span(line: &str) -> Option<(usize, usize, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let after_hash = trimmed[1..].trim_start();
    let keyword: String = after_hash.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if keyword != "include" && keyword != "import" {
        return None;
    }
    let kw_start = line.len() - after_hash.len();
    let after_kw = &line[kw_start + keyword.len()..];
    let open_rel = after_kw.find(|c| c == '<' || c == '"')?;
    let open = kw_start + keyword.len() + open_rel;
    let close = open + 1 + line[open + 1..].find(|c| c == '>' || c == '"')?;
    Some((open, close + 1, line[open + 1..close].to_string()))
}

/// clangd-parity card for an `#include`/`#import` path: what kind of header it
/// is and how the build treats it. Nothing else answers on a preprocessor
/// line — the path is not a symbol — so hovering `stdio.h` used to say nothing.
fn header_hover_markdown(text: &str, position: LspPosition) -> Option<String> {
    let line = text.lines().nth(position.line as usize)?;
    let (open, end, path) = include_span(line)?;
    // The cursor may sit on the path or either bracket.
    let col = position.character as usize;
    if col < open || col >= end {
        return None;
    }
    if path.is_empty() {
        return None;
    }
    let kind_word = if path.ends_with(".hje") || path.ends_with(".jeti") {
        "Jeti header — inlined by the preprocessor from the `#import` search path"
    } else {
        "C header — passed through to the C compiler; jetic does not parse C headers"
    };
    let mut markdown = format!("**header** `{path}`\n\n{kind_word}");
    markdown.push_str(&format!("\n\n```jeti\n{}\n```", line.trim()));
    Some(markdown)
}

/// clangd-parity card for a struct/union (or class ivar) field under the
/// cursor: what it is, its type and computed size/alignment, and its own
/// declaration line. All English, matching the other hover cards.
fn field_hover_markdown(
    index: &index::SymbolIndex,
    fref: &index::FieldRef,
    name: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let layout = index.layouts.get(&fref.struct_name)?;
    let field = layout.fields.iter().find(|f| f.name == name)?;
    let kind_word = if fref.is_union { "union" } else { "struct" };
    let mut markdown = format!("**field** `{name}` · in `{kind_word} {}`", fref.struct_name);
    markdown.push_str(&format!("\n\nType: {}", field.type_spelling));
    if field.size > 0 {
        let mut line = format!("Size: {}", jeti_layout::field_size_text(field));
        if field.bit_width.is_none() && field.alignment > 0 {
            line.push_str(&format!(", alignment {} bytes", field.alignment));
        }
        markdown.push_str(&format!("\n{line}"));
    }
    if let Some(text) = resolve(&fref.path) {
        if let Some(decl) = text
            .lines()
            .nth(fref.line.saturating_sub(1) as usize)
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            markdown.push_str(&format!("\n\n```jeti\n{decl}\n```"));
        }
    }
    markdown.push_str(&format!(
        "\n\n_Declared in {}:{}_\n",
        display_path(&fref.path),
        fref.line
    ));
    Some(markdown)
}

/// clangd-parity layout card for a struct/union/typedef symbol: title line,
/// then "Size: N bytes, alignment M bytes" and the per-field offset table —
/// all English (the cards must read like clangd's, not carry prose). The
/// declaration snippet stays, because it is what makes the card recognizable.
fn hover_markdown_with_layout(
    location: &Location,
    layout: Option<&jeti_layout::StructLayout>,
    aliased: Option<&String>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> String {
    let kind_word = if location.detail == "union" { "union" } else { "struct" };
    let mut markdown = format!("**{kind_word}** `{}`", location.name);
    markdown.push_str("\n\n");
    if let Some(l) = layout {
        markdown.push_str(&jeti_layout::hover_body(l));
        markdown.push_str("\n\n");
    } else if let Some(target) = aliased {
        markdown.push_str(&format!("typedef — aliases `{target}`\n\n"));
    }
    if let Some(text) = resolve(&location.path) {
        if let Some(snippet) = declaration_text(&text, location.range) {
            markdown.push_str(&format!("```jeti\n{snippet}\n```\n"));
        }
    }
    markdown.push_str(&format!(
        "\n_Declared in {}:{}_",
        display_path(&location.path),
        location.range.start.line + 1
    ));
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
    use crate::index::ReceiverInfo;
    use crate::index::selector_part_matches;

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
        // Same program the CLI test uses: the missing `;` belongs between `0`
        // and `}`, and the caret sits at the INSERTION POINT (previous's
        // end, LSP character 25) — clang's `expected ';'` carets the spot
        // the fix-it would insert at (`1:26`, measured), not the offending
        // `}`. Same rule `f(1;` already followed, where the two coincide.
        let path = "/tmp/jeti_lsp_unit/parse_err.jeti";
        let text = "int main(void) { return 0 }\n";
        let diags = diagnostics_for(path, text, &[], &no_open_files());
        assert!(!diags.is_empty(), "expected a parse diagnostic, got none");
        let first = &diags[0];
        assert_eq!(first["severity"], 1);
        assert_eq!(first["range"]["start"], json!({"line": 0, "character": 25}));
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
        server.quiet_period = Duration::ZERO;
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let out = server.handle(&init);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[0]["result"]["capabilities"]["textDocumentSync"], 1);

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
        assert_eq!(out.len(), 2, "diagnostics plus the tokens refresh: {out:?}");
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[0]["params"]["uri"], uri);
        assert!(!out[0]["params"]["diagnostics"].as_array().unwrap().is_empty());
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh");

        // A clean edit clears them — the edit itself publishes nothing, the
        // next message runs the deferred analysis (full sync: whole buffer
        // replaces the old).
        let out = server.handle(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": "int main(void) { return 0; }\n" }],
            },
        }));
        assert!(out.is_empty(), "an edit alone publishes nothing: {out:?}");
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } },
        }));
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh");
        assert_eq!(out[2]["id"], 3);

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
        server.quiet_period = Duration::ZERO;
        let uri = path_to_uri("/tmp/jeti_lsp_unit/incremental.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1,
                "text": "int main(void) { return 0; }\n",
            }},
        }));

        // Delete the `;` (line 0, character 25). The edit alone publishes
        // nothing: the analysis of it runs when the buffer has been quiet and
        // the next message arrives.
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
        assert!(out.is_empty(), "an edit alone publishes nothing: {out:?}");

        // Put it back: the same range, an insertion this time. This message
        // runs the deferred analysis of the previous edit — the parse error
        // appeared at the insertion point where the `;` had been (previous's
        // end, LSP character 25 — clang's `expected ';'` placement).
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
        let diagnostics = out[0]["params"]["diagnostics"].as_array().expect("diagnostics");
        assert_eq!(diagnostics.len(), 1, "{out:?}");
        assert_eq!(diagnostics[0]["range"]["start"], json!({"line": 0, "character": 25}));
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh", "{out:?}");

        // And the next message settles the restored buffer: clean again.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } },
        }));
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh");
        assert_eq!(out[2]["id"], 1);
    }

    #[test]
    fn rapid_edits_coalesce_into_one_analysis() {
        // A burst of keystrokes inside the quiet period runs the front end
        // once, on the settled state — intermediate states are never analyzed.
        let t = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
        let clock = t.clone();
        let mut server = Server::new(AnalysisOptions::default());
        server.quiet_period = Duration::from_millis(100);
        server.clock = Box::new(move || clock.get());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/coalesce.jeti");

        // didOpen still publishes right away — coalescing is for edits.
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1,
                "text": "int main(void) { return 0; }\n",
            }},
        }));
        assert_eq!(out.len(), 2, "didOpen still publishes right away: {out:?}");
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh");

        // Edit A (t = 0): delete the `;`. Nothing runs yet.
        t.set(Duration::ZERO);
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
        assert!(out.is_empty(), "an edit inside the quiet period publishes nothing: {out:?}");

        // Edit B (t = 10 ms): put it back. Still inside A's quiet period, so
        // A's analysis never runs at all — only the settled state will.
        t.set(Duration::from_millis(10));
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
        assert!(out.is_empty(), "still inside the quiet period: {out:?}");

        // The next message after the quiet period (t = 150 ms) runs the one
        // coalesced analysis; the settled state is clean.
        t.set(Duration::from_millis(150));
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } },
        }));
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh");
        assert_eq!(out[2]["id"], 1);
    }

    #[test]
    fn the_clock_alone_flushes_the_settled_edit() {
        // The stdio loop polls stdin against `next_flush_in`, so the quiet
        // period fires on the clock alone: the last edit of a burst is
        // analyzed even when the client goes silent after it. Without this
        // the editor keeps showing the previous edit's diagnostics forever —
        // an error never clears once the fix is the final edit.
        let t = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
        let clock = t.clone();
        let mut server = Server::new(AnalysisOptions::default());
        server.quiet_period = Duration::from_millis(100);
        server.clock = Box::new(move || clock.get());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/timer.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1,
                "text": "int main(void) { return 0; }" } }
        }));
        // The only edit ever made: drop the semicolon at column 25. No
        // message follows — exactly what the editor does after the last
        // keystroke of a fix.
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": { "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [ {
                    "range": { "start": {"line": 0, "character": 25},
                               "end": {"line": 0, "character": 26} },
                    "text": "" } ] }
        }));
        // Inside the window: the loop would wait this long for input.
        t.set(Duration::from_millis(40));
        assert_eq!(server.next_flush_in(), Some(Duration::from_millis(60)));
        assert!(server.flush_due().is_empty(), "not due yet");
        // The clock alone crosses the deadline: now due, one flush settles it.
        t.set(Duration::from_millis(100));
        assert_eq!(server.next_flush_in(), Some(Duration::ZERO));
        let out = server.flush_due();
        assert_eq!(out.len(), 2, "diagnostics plus the tokens refresh: {out:?}");
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert!(!out[0]["params"]["diagnostics"].as_array().unwrap().is_empty());
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh");
        // Settled: nothing left to flush; the loop blocks indefinitely again.
        assert!(server.next_flush_in().is_none());
    }

    #[test]
    fn an_edit_inside_the_quiet_period_keeps_the_stale_index() {
        // Until the quiet period passes, requests are answered from the last
        // analysis — stale on purpose: running a front end per request would
        // be exactly the waste the quiet period exists to remove.
        let t = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
        let clock = t.clone();
        let mut server = Server::new(AnalysisOptions::default());
        server.quiet_period = Duration::from_millis(100);
        server.clock = Box::new(move || clock.get());
        let uri = path_to_uri("/tmp/jeti_lsp_unit/stale.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1,
                "text": "int main(void) { return 0 }\n",
            }},
        }));

        // A class appears in the buffer at t = 0.
        t.set(Duration::ZERO);
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{
                    "text": "@interface Late : jeti_root\n@end\nint main(void) { return 0; }\n",
                }],
            },
        }));

        // 10 ms later the quiet period has not passed: the outline still
        // comes from the open-time analysis, and no publish rides along.
        t.set(Duration::from_millis(10));
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } },
        }));
        assert_eq!(out.len(), 1, "no publish before the quiet period: {out:?}");
        assert_eq!(out[0]["id"], 1);
        let names: Vec<&str> = out[0]["result"].as_array().unwrap().iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect();
        assert!(names.iter().all(|name| *name != "Late"), "stale outline: {names:?}");

        // After the quiet period the same request triggers the analysis.
        t.set(Duration::from_millis(150));
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } },
        }));
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[1]["method"], "workspace/semanticTokens/refresh");
        let names: Vec<&str> = out[2]["result"].as_array().unwrap().iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"Late"), "{names:?}");
    }

    #[test]
    fn mrc_projects_can_turn_arc_off() {
        // The case a real editor session hit: legacy MRC code (`dealloc` with
        // manual releases) is an ARC error under the CLI's defaults, and the
        // editor must be able to say "this project builds with -fno-jeti-arc"
        // and stop reporting what the build accepts.
        let mrc = "\
@interface Node : jeti_root {
    NPObject *_next;
}
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
        // A method the server does not offer — the capability list in
        // `initialize` is the truth about what it does.
        let req = json!({"jsonrpc": "2.0", "id": 7, "method": "textDocument/formatting", "params": {}});
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

        // On a namespace segment the answer is the namespace — and since the
        // block is kept beside the flat declaration stream, there is a place to
        // jump to: the `@namespace` line itself.
        let on_namespace = position(NAMESPACES, "A::Inner::Player", 3);
        assert_eq!(
            analyzed.index.resolve_at(NAMESPACES, on_namespace),
            Some(index::Resolved::Symbol("A::Inner".to_string()))
        );
        let definitions = analyzed.index.definitions_at(NAMESPACES, on_namespace);
        assert_eq!(definitions.len(), 1, "{definitions:?}");
        assert_eq!(definitions[0].range.start.line, 12);
        // And the outermost segment is a namespace of its own.
        let outermost = position(NAMESPACES, "A::Player", 0);
        assert_eq!(
            analyzed.index.resolve_at(NAMESPACES, outermost),
            Some(index::Resolved::Symbol("A".to_string()))
        );
        assert_eq!(
            analyzed.index.definitions_at(NAMESPACES, outermost)[0].range.start.line,
            0
        );

        // The outline nests what was flattened: `A::Inner` holds its class, and
        // the class's name drops the prefix its container now spells.
        let symbols = analyzed.index.document_symbols("/tmp/jeti_lsp_unit/symbols.jeti");
        let names: Vec<&str> = symbols.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["A", "B", "A::Inner", "main"], "{symbols:#?}");
        let inner = &symbols[2]["children"];
        assert_eq!(inner[0]["name"], "Player");
        assert_eq!(inner[0]["detail"], "@interface:jeti_root");
        // Breadcrumbs come from `containerName`.
        assert_eq!(inner[0]["containerName"], "A::Inner");

        // A qualified name the index does not have is a miss: answering with
        // another namespace's class that happens to share the short name would
        // be a guess.
        let unknown = "    C::Player *p;";
        assert_eq!(analyzed.index.resolve_at(unknown, position(unknown, "C::Player", 3)), None);
        assert!(analyzed.index.definitions_at(unknown, position(unknown, "C::Player", 3)).is_empty());
    }

    /// References, like definitions, honor the segment under the cursor: on
    /// `Player` in `A::Player`, the answer is A's class and only A's — the
    /// two same-named `Player`s in the other namespaces do not appear.
    #[test]
    fn find_references_lists_one_symbol_s_occurrences_only() {
        let analyzed = analyze(NAMESPACES);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        // Cursor on the `Player` segment of `A::Player` in main.
        let on_player = position(NAMESPACES, "A::Player", 3);
        let references = analyzed.index.find_references(NAMESPACES, on_player);
        assert_eq!(references.len(), 2, "{references:?}");
        // The `@interface` declaration and the qualified use in main — the
        // scan's hit inside the declaration's extent dedupes into it.
        let lines: Vec<u32> = references.iter().map(|l| l.range.start.line).collect();
        assert_eq!(lines, vec![1, 19]);

        // The same answer from the other side: on B's qualified use, B's
        // declaration and only B's.
        let references = analyzed
            .index
            .find_references(NAMESPACES, position(NAMESPACES, "B::Player", 3));
        let lines: Vec<u32> = references.iter().map(|l| l.range.start.line).collect();
        assert_eq!(lines, vec![7, 20], "{references:?}");
    }

    /// A selector's references are found through its name without the colon,
    /// the form the text carries: declaration, implementation, and the send.
    #[test]
    fn selector_and_class_references_are_found_the_same_way() {
        let analyzed = analyze(PROGRAM);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        // Cursor on `run:` in the declaration.
        let references = analyzed
            .index
            .find_references(PROGRAM, position(PROGRAM, "- (void)run:", 8));
        let lines: Vec<u32> = references.iter().map(|l| l.range.start.line).collect();
        assert_eq!(lines, vec![3, 7, 8], "{references:?}");

        // A class: its `@interface` and `@implementation` names.
        let references = analyzed
            .index
            .find_references(PROGRAM, position(PROGRAM, "@interface Thing", 11));
        let lines: Vec<u32> = references.iter().map(|l| l.range.start.line).collect();
        assert_eq!(lines, vec![2, 6], "{references:?}");
    }

    /// The scan reads code, not text: a name inside a comment or a string
    /// literal is not a reference, however exactly it is spelled.
    #[test]
    fn names_in_comments_and_strings_are_not_references() {
        const SRC: &str = "\
@interface Thing : jeti_root
@end

int main() {
    // Thing does not count
    Thing *note;
    char *s = \"Thing\";
    return 0;
}
";
        let analyzed = analyze(SRC);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        let references = analyzed
            .index
            .find_references(SRC, position(SRC, "@interface Thing", 11));
        let lines: Vec<u32> = references.iter().map(|l| l.range.start.line).collect();
        assert_eq!(lines, vec![0, 5], "{references:?}");
    }

    /// A rename preselects the name, and a qualified name's qualifier
    /// survives: on the `Player` segment of `A::Player`, the range is the
    /// segment alone, and the occurrences are the declaration's name and the
    /// segment in main — not the whole qualified names, and not the other
    /// same-named classes.
    #[test]
    fn rename_replaces_the_name_not_the_qualifier() {
        let analyzed = analyze(NAMESPACES);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        // Cursor on the `Player` segment of `A::Player` in main
        // (`    A::Player *p;`: the segment is characters 7..13).
        let on_player = position(NAMESPACES, "A::Player", 5);
        let range = analyzed.index.rename_range_at(NAMESPACES, on_player).expect("renameable");
        assert_eq!((range.start.line, range.start.character), (19, 7));
        assert_eq!((range.end.line, range.end.character), (19, 13));

        let ranges = analyzed.index.rename_ranges(NAMESPACES, on_player);
        assert_eq!(ranges.len(), 2, "{ranges:?}");
        // The declaration's name (line 1, `@interface Player`) and the
        // segment in main.
        assert_eq!((ranges[0].start.line, ranges[0].start.character), (1, 11));
        assert_eq!((ranges[1].start.line, ranges[1].start.character), (19, 7));
    }

    /// A selector travels with its colon: renaming `run:` rewrites `run:` in
    /// the declaration, the implementation, and the send — four characters
    /// each time.
    #[test]
    fn rename_a_selector_keeps_its_colon() {
        let analyzed = analyze(PROGRAM);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        let on_run = position(PROGRAM, "- (void)run:", 8);
        let range = analyzed.index.rename_range_at(PROGRAM, on_run).expect("renameable");
        assert_eq!((range.start.line, range.start.character), (3, 8));
        assert_eq!((range.end.line, range.end.character), (3, 12));

        let ranges = analyzed.index.rename_ranges(PROGRAM, on_run);
        assert_eq!(ranges.len(), 3, "{ranges:?}");
        for range in &ranges {
            assert_eq!(range.end.character - range.start.character, 4, "{ranges:?}");
        }
    }

    /// A bare name written inside a `@namespace` block stands for that block's
    /// member, which is how the elaborator reads it: it prefixes the name with
    /// the block's fully-qualified name and walks outward when the inner name
    /// does not exist.
    const SCOPED: &str = "\
@namespace A
@interface Player : jeti_root
- (void)fromA;
@end

@namespace Inner
@interface Player : jeti_root
- (void)deep;
@end

int useInner(void) {
    Player *p;
    return 0;
}
@endnamespace

int useOuter(void) {
    Player *q;
    return 0;
}
@endnamespace

@namespace B
@interface Player : jeti_root
- (void)fromB;
@end
@endnamespace

int useTop(void) {
    Player *r;
    return 0;
}
";

    /// The same short name in three namespaces used to answer with all three
    /// wherever it was written. Inside a block it now answers with that block's
    /// declaration — and only at the top level, where the source qualifies
    /// nothing, does the name stay the three-way one it looks like.
    #[test]
    fn a_bare_name_resolves_to_the_block_it_is_written_in() {
        let analyzed = analyze(SCOPED);
        let on = |needle: &str| {
            analyzed
                .index
                .definitions_at(SCOPED, position(SCOPED, needle, 0))
        };

        // The innermost block the cursor is in decides first.
        let inner = on("Player *p;");
        assert_eq!(inner.len(), 1, "{inner:?}");
        assert_eq!(inner[0].name, "A::Inner::Player");
        assert_eq!(inner[0].range.start.line, 6);

        // Outside the inner block the walk goes outward, to `A`.
        let outer = on("Player *q;");
        assert_eq!(outer.len(), 1, "{outer:?}");
        assert_eq!(outer[0].name, "A::Player");
        assert_eq!(outer[0].range.start.line, 1);

        // At the top level no block holds it, so nothing is narrowed: all three
        // declarations are the honest answer, in source order — the short form
        // is indexed for exactly this case.
        let top = on("Player *r;");
        let names: Vec<&str> = top.iter().map(|location| location.name.as_str()).collect();
        assert_eq!(names, vec!["A::Player", "A::Inner::Player", "B::Player"], "{names:?}");
    }

    /// The `@using` spellings the binder resolves and the AST used to drop: an
    /// alias, an imported short name, and a namespace prefix.
    ///
    /// The two classes have deliberately different short names: a `@using` whose
    /// name would also be a *declaration's* short name is miscompiled today
    /// (the elaborator names declarations through the using list, so two
    /// classes can collapse into one), and a test built on that would pin the
    /// bug instead of the resolution.
    const USINGS: &str = "\
@namespace A
@interface Player : jeti_root
- (void)fromA;
@end
@endnamespace

@namespace B
@interface Hero : jeti_root
- (void)fromB;
@end
@endnamespace

@using A::Player;
@using Alias = B::Hero;
@using A;

void runUse(void) {
    Player *p;
    Alias *q;
}
";

    /// A name introduced by `@using` answers where the compiler resolves it: the
    /// alias is the class it renames — the one case the index could not answer
    /// at all before, since an alias introduces no declaration of its own — and
    /// the imported short name is that one class.
    #[test]
    fn a_using_name_resolves_to_what_it_imports() {
        let analyzed = analyze(USINGS);
        let at = |needle: &str| position(USINGS, needle, 0);

        let alias = analyzed.index.definitions_at(USINGS, at("Alias *q;"));
        assert_eq!(alias.len(), 1, "{alias:?}");
        assert_eq!(alias[0].name, "B::Hero");

        let imported = analyzed.index.definitions_at(USINGS, at("Player *p;"));
        assert_eq!(imported.len(), 1, "{imported:?}");
        assert_eq!(imported[0].name, "A::Player");

        // On the alias inside the declaration itself, go-to-definition reaches
        // the class it renames — as clangd's does.
        assert_eq!(
            analyzed.index.resolve_at(USINGS, at("Alias = B::Hero")),
            Some(index::Resolved::Symbol("B::Hero".to_string()))
        );

        // The namespace form introduces no name of its own; the name it spells
        // is the block, which the index already knows.
        assert_eq!(
            analyzed.index.resolve_at(USINGS, position(USINGS, "@using A;", 7)),
            Some(index::Resolved::Symbol("A".to_string()))
        );
    }

    #[test]
    /// Hover answers for everything the declaration index cannot: locals and
    /// receivers (`sp` in `sp.label = nil`), builtin type keywords, Jeti
    /// keywords, and the C17 library surface the passthrough headers bring.
    fn hover_covers_locals_builtins_keywords_and_c_stdlib() {
        let src = "\
#include <stdio.h>
#include <errno.h>

@interface Runner : jeti_root
@property int label;
@end

@implementation Runner
- (void)run {
    Runner *sp = self;
    sp.label = 1;
    int ticks = 0;
    printf(\"%d\\n\", ticks + errno);
}
@end

int main(void) { return 0; }
";
        let hover_at = |needle: &str, extra: usize| {
            let pos = position(src, needle, extra);
            let mut server = Server::new(AnalysisOptions::default());
            let uri = path_to_uri("/tmp/jeti_lsp_unit/hover.jeti");
            server.handle(&json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": uri, "languageId": "jeti", "version": 1, "text": src,
                }},
            }));
            let out = server.handle(&json!({
                "jsonrpc": "2.0", "id": 30, "method": "textDocument/hover",
                "params": { "textDocument": { "uri": uri },
                            "position": position_json(pos) },
            }));
            out[0]["result"]["contents"]["value"].as_str().map(String::from)
        };

        // The receiver `sp` in `sp.label = nil` — clangd shows the receiver's
        // type; the index-only hover used to answer nothing.
        let value = hover_at("sp.label", 0).expect("sp hover");
        assert!(value.contains("local variable") && value.contains("Runner"), "{value}");

        // A scalar local: `int ticks`.
        let value = hover_at("ticks", 0).expect("ticks hover");
        assert!(value.contains("int"), "{value}");

        // A passthrough C function — the clangd-shaped card, extracted from
        // the real toolchain (`cc -E` over the included system headers).
        let value = hover_at("printf", 0).expect("printf hover");
        assert!(value.contains("**function**") && value.contains("stdio.h"), "{value}");
        assert!(value.contains("→ int") && value.contains("Parameters:"), "{value}");
        assert!(value.contains("const char *restrict"), "{value}");

        // A passthrough binding — the *use* in `ticks + errno`, not the
        // `#include <errno.h>` line. macOS spells errno as a macro, Linux as
        // an extern variable; both come from the toolchain extraction.
        let value = hover_at("+ errno", 2).expect("errno hover");
        assert!(value.contains("provided by") && value.contains("errno.h"), "{value}");

        // Builtin type keyword.
        let value = hover_at("int main", 0).expect("int hover");
        assert!(value.contains("builtin integer type"), "{value}");

        // Jeti keyword.
        let value = hover_at("@interface", 1).expect("keyword hover");
        assert!(value.contains("@interface"), "{value}");
    }

    #[test]
    /// A cursor parked on a field name inside a struct/union body (or a class
    /// ivar) answers from the computed layout: kind, host aggregate, type and
    /// size/alignment — the card clangd shows, which used to be nothing.
    fn hover_answers_for_struct_union_and_ivar_fields() {
        let src = "\
struct FsWidget { int wid; float ratio; };
union FsMix { int i; float f; char bytes[4]; };
@interface Runner : NPObject
{
  int laps;
}
@end
int main(void) { return 0; }
";
        let hover_at = |needle: &str, extra: usize| {
            let pos = position(src, needle, extra);
            let mut server = Server::new(AnalysisOptions::default());
            let uri = path_to_uri("/tmp/jeti_lsp_unit/fields.jeti");
            server.handle(&json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": uri, "languageId": "jeti", "version": 1, "text": src,
                }},
            }));
            let out = server.handle(&json!({
                "jsonrpc": "2.0", "id": 40, "method": "textDocument/hover",
                "params": { "textDocument": { "uri": uri },
                            "position": position_json(pos) },
            }));
            out[0]["result"]["contents"]["value"].as_str().map(str::to_string)
        };

        // A union member: clangd spells kind, host, type, size+alignment.
        let value = hover_at("int i", 4).expect("union field hover");
        assert!(value.contains("**field** `i`"), "{value}");
        assert!(value.contains("in `union FsMix`"), "{value}");
        assert!(value.contains("Type: int"), "{value}");
        assert!(value.contains("Size: 4 bytes, alignment 4 bytes"), "{value}");

        // A struct member after padding: same shape, host struct named.
        let value = hover_at("float ratio", 6).expect("struct field hover");
        assert!(value.contains("**field** `ratio`"), "{value}");
        assert!(value.contains("in `struct FsWidget`"), "{value}");

        // A class ivar: the layout engine's synthesized aggregate backs it.
        let value = hover_at("int laps", 4).expect("ivar hover");
        assert!(value.contains("**field** `laps`"), "{value}");
        assert!(value.contains("Runner"), "{value}");
    }

    #[test]
    /// Free-function cards spell the clangd shape (return-type arrow plus a
    /// parameter list), and local-variable cards carry the type, the enclosing
    /// function, and the declaration line instead of a bare spelling.
    fn hover_enriches_function_and_local_cards() {
        let src = "\
static int fs_double_it(int v) { return v * 2; }
int main(void) {
  unsigned int ali = 4;
  return fs_double_it(ali);
}
";
        let hover_at = |needle: &str, extra: usize| {
            let pos = position(src, needle, extra);
            let mut server = Server::new(AnalysisOptions::default());
            let uri = path_to_uri("/tmp/jeti_lsp_unit/enriched.jeti");
            server.handle(&json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": uri, "languageId": "jeti", "version": 1, "text": src,
                }},
            }));
            let out = server.handle(&json!({
                "jsonrpc": "2.0", "id": 40, "method": "textDocument/hover",
                "params": { "textDocument": { "uri": uri },
                            "position": position_json(pos) },
            }));
            out[0]["result"]["contents"]["value"].as_str().map(str::to_string)
        };

        let value = hover_at("int fs_double_it", 12).expect("function hover");
        assert!(value.contains("**function** `fs_double_it`"), "{value}");
        assert!(value.contains("→ int"), "{value}");
        assert!(value.contains("Parameters:"), "{value}");
        assert!(value.contains("• `int v`"), "{value}");

        let value = hover_at("unsigned int ali", 16).expect("local hover");
        assert!(value.contains("**local variable** `ali`"), "{value}");
        assert!(value.contains("Type: unsigned int"), "{value}");
        assert!(value.contains("in `main`"), "{value}");
        assert!(value.contains("unsigned int ali = 4"), "{value}");
    }

    #[test]
    /// Every declarator shape answers, not just the plain ones: a fn-pointer,
    /// a multi-dim array, and a bit-field field all used to hover as nothing
    /// because their own spans land on a trailing token — matching now goes by
    /// name inside the host aggregate's extent.
    fn hover_covers_fnptr_array_and_bitfield_fields() {
        let src = "\
struct FsWidget {
    int wid;
    int (*cb)(int);
    int cells[2][3];
    unsigned flags : 3;
    int in;
};
int main(void) { return 0; }
";
        let hover_at = |needle: &str, extra: usize| {
            let pos = position(src, needle, extra);
            let mut server = Server::new(AnalysisOptions::default());
            let uri = path_to_uri("/tmp/jeti_lsp_unit/shapes.jeti");
            server.handle(&json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": uri, "languageId": "jeti", "version": 1, "text": src,
                }},
            }));
            let out = server.handle(&json!({
                "jsonrpc": "2.0", "id": 40, "method": "textDocument/hover",
                "params": { "textDocument": { "uri": uri },
                            "position": position_json(pos) },
            }));
            out[0]["result"]["contents"]["value"].as_str().map(str::to_string)
        };

        let value = hover_at("int (*cb)", 7).expect("fn-ptr field hover");
        assert!(value.contains("**field** `cb`"), "{value}");
        assert!(value.contains("Type: int (*cb)(int)"), "{value}");
        assert!(value.contains("Size: 8 bytes, alignment 8 bytes"), "{value}");

        let value = hover_at("int cells", 4).expect("array field hover");
        assert!(value.contains("**field** `cells`"), "{value}");
        assert!(value.contains("Type: int[2][3]"), "{value}");
        assert!(value.contains("Size: 24 bytes"), "{value}");

        let value = hover_at("unsigned flags", 9).expect("bit-field hover");
        assert!(value.contains("**field** `flags`"), "{value}");
        assert!(value.contains("3 bits (+29 bits padding)"), "{value}");

        let value = hover_at("int wid", 4).expect("plain field hover");
        assert!(value.contains("**field** `wid`"), "{value}");
    }

    #[test]
    /// Hovering an include path answers with a header card — the C-passthrough
    /// kind for C headers, the inlined kind for `.hje` — instead of nothing.
    fn hover_answers_on_include_paths() {
        let src = "\
#include <stdio.h>
#import <Foundation/Foundation.hje>
int main(void) { return 0; }
";
        let hover_at = |line: u32, character: u32| {
            let mut server = Server::new(AnalysisOptions::default());
            let uri = path_to_uri("/tmp/jeti_lsp_unit/headers.jeti");
            server.handle(&json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": uri, "languageId": "jeti", "version": 1, "text": src,
                }},
            }));
            let out = server.handle(&json!({
                "jsonrpc": "2.0", "id": 40, "method": "textDocument/hover",
                "params": { "textDocument": { "uri": uri },
                            "position": position_json(LspPosition { line, character }) },
            }));
            out[0]["result"]["contents"]["value"].as_str().map(str::to_string)
        };

        // On the path, and on either bracket.
        let value = hover_at(0, 12).expect("C header hover");
        assert!(value.contains("**header** `stdio.h`"), "{value}");
        assert!(value.contains("C header"), "{value}");
        let value = hover_at(0, 9).expect("opening bracket hover");
        assert!(value.contains("**header** `stdio.h`"), "{value}");
        let value = hover_at(1, 12).expect("jeti header hover");
        assert!(value.contains("**header** `Foundation/Foundation.hje`"), "{value}");
        assert!(value.contains("Jeti header"), "{value}");
        // Off the path: no card, the line's keyword stays grammar-only. (The
        // document's `#import` cannot resolve in the unit harness — no include
        // search path — so later lines have no analysis to hover with; the
        // `#include` keyword itself needs none.)
        let value = hover_at(0, 3);
        assert!(!value.as_deref().is_some_and(|v| v.contains("**header**")), "{value:?}");
    }

    #[test]
    /// Include paths come back as links over the whole bracketed span: a path
    /// the search directory resolves carries a file target, a system header
    /// nothing resolves keeps its range without one.
    fn include_paths_link_to_resolved_files() {
        let dir = std::env::temp_dir().join("jeti_lsp_unit_links");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("helper.hje"), "// helper\n").unwrap();
        let src = "#import <helper.hje>\n#include <jeti_no_such_header_xyz.h>\n";
        let mut server = Server::new(AnalysisOptions {
            search_dirs: vec![dir.to_string_lossy().to_string()],
            ..AnalysisOptions::default()
        });
        let uri = path_to_uri("/tmp/jeti_lsp_unit/links.jeti");
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": src,
            }},
        }));
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 50, "method": "textDocument/documentLink",
            "params": { "textDocument": { "uri": uri } },
        }));
        let links = out[0]["result"].as_array().expect("links array");
        assert_eq!(links.len(), 2, "{links:?}");

        // The whole `<helper.hje>` — brackets included — is one link range
        // (end is exclusive, so it stops one past the `>`).
        let resolved = &links[0];
        assert_eq!(resolved["range"]["start"]["character"], 8, "{resolved:?}");
        assert_eq!(resolved["range"]["end"]["character"], 20, "{resolved:?}");
        let target = resolved["target"].as_str().expect("resolved target");
        assert!(target.ends_with("helper.hje"), "{target}");

        // A header no search dir and no system dir has: range, no target.
        // (stdio.h *does* resolve once the cc probe lists the SDK — that is
        // the clangd-parity behavior — so the miss case needs a real miss.)
        let unresolved = &links[1];
        assert!(unresolved.get("target").is_none(), "{unresolved:?}");
    }

    /// The project's `compile_flags.txt` reaches the link resolver, not just
    /// the diagnostics: a header found only through the flags file's `-I` dir
    /// still gets a target (the LibUI-demo regression — the import analyzed
    /// clean while the underline was missing).
    #[test]
    fn a_flags_file_include_dir_reaches_document_links() {
        let dir = std::env::temp_dir().join("jeti_lsp_unit_links_flags");
        std::fs::create_dir_all(dir.join("include")).unwrap();
        std::fs::write(dir.join("include/helper.hje"), "// helper\n").unwrap();
        // One flag per line; the relative dir resolves against this file's
        // own directory, clangd's rule.
        std::fs::write(dir.join("compile_flags.txt"), "-I include\n").unwrap();
        let src = "#import <helper.hje>\n";
        let mut server = Server::new(AnalysisOptions::default());
        let uri = path_to_uri(&dir.join("links.jeti").to_string_lossy());
        server.handle(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": src,
            }},
        }));
        let out = server.handle(&json!({
            "jsonrpc": "2.0", "id": 51, "method": "textDocument/documentLink",
            "params": { "textDocument": { "uri": uri } },
        }));
        let links = out[0]["result"].as_array().expect("links array");
        assert_eq!(links.len(), 1, "{links:?}");
        let target = links[0]["target"].as_str().expect("target via flags file");
        assert!(target.ends_with("include/helper.hje"), "{target}");
    }

    #[test]
    /// The cc probe parser reads the driver's search-list block and nothing
    /// else — framework annotations stripped, prose after the list ignored.
    fn cc_search_list_parses_to_directories() {
        let text = "\
#include <...> search starts here:
 /usr/local/include
 /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include
 /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks (framework directory)
End of search list.
noise after the list
";
        let dirs = parse_cc_search_list(text);
        assert_eq!(dirs.len(), 3, "{dirs:?}");
        assert!(dirs[0].ends_with("/usr/local/include"), "{dirs:?}");
        assert!(
            !dirs[2].ends_with("(framework directory)"),
            "annotation must be stripped: {dirs:?}"
        );
    }

    #[test]
    #[ignore] // probe: decode semantic tokens for the screenshot's white shapes
    fn probe_semantic_shapes() {
        let src = "\
#define FSQ_MAX(a, b) ((a) > (b) ? (a) : (b))
static int fs_double_it(int v) { return v * 2; }
static void sec1_c_superset(void) {
    int x1 = 10, x2 = 20;
    int *pa = &x1, *pb = &x2;
    printf(\"1.1 %d\\n\", *pa, *pb);
    int (*fp)(int) = fs_double_it;
    memset(&x1, 0, sizeof(x1));
    struct FsWidget { int wid; int (*cb)(int); };
    struct FsWidget w;
    w.cb = fs_double_it;
    w.wid = 7;
    w.cells[0] = 1;
    int r = FSQ_MAX(x1, x2);
}
int main(void) { return 0; }
";
        let names = ["namespace", "class", "type", "enum", "function", "variable", "property", "macro", "enumMember"];
        let analyzed = analyze(src);
        let data = analyzed.index.semantic_tokens(src);
        let mut i = 0;
        let mut line = 0;
        let mut character = 0;
        while i < data.len() {
            if data[i] > 0 { line += data[i]; character = data[i + 1]; } else { character += data[i + 1]; }
            let (len, ty) = (data[i + 2], data[i + 3]);
            let text: String = src.lines().nth(line as usize).unwrap()
                [character as usize..(character + len) as usize].to_string();
            println!("L{line:>3} c{character:>3} {:<10} {ty:<2} `{text}`", names[ty as usize]);
            i += 5;
        }
    }

    #[test]
    /// The shapes the screenshots showed as plain white all classify: C
    /// stdlib calls (`printf`, `memset` — headers the analysis never parses),
    /// member accesses after `.`/`->` (`w.cb`, `w.wid`, `w.cells`), locals in
    /// comma declaration chains, and macro uses. Keywords like `sizeof` stay
    /// grammar-colored (no semantic token).
    fn semantic_tokens_cover_stdlib_member_and_macro_shapes() {
        // The include is what arms the C surface: symbols come from the
        // toolchain for exactly the headers the document includes (a document
        // without the include has no `printf` to color — clangd's semantics).
        let src = "\
#include <stdio.h>
#include <string.h>
#define FSQ_MAX(a, b) ((a) > (b) ? (a) : (b))
static int fs_double_it(int v) { return v * 2; }
static void sec1_c_superset(void) {
    int x1 = 10, x2 = 20;
    printf(\"1.1 %d\\n\", x1, x2);
    memset(&x1, 0, sizeof(x1));
    struct FsWidget { int wid; int (*cb)(int); };
    struct FsWidget w;
    w.cb = fs_double_it;
    w.wid = 7;
    int r = FSQ_MAX(x1, x2);
}
int main(void) { return 0; }
";
        // Decode the delta stream into (line, character, type) triples.
        let decoded = |data: &[u32]| -> Vec<(u32, u32, u32)> {
            let mut out = Vec::new();
            let (mut line, mut character) = (0u32, 0u32);
            let mut i = 0;
            while i + 4 < data.len() {
                if data[i] > 0 {
                    line += data[i];
                    character = data[i + 1];
                } else {
                    character += data[i + 1];
                }
                out.push((line, character, data[i + 3]));
                i += 5;
            }
            out
        };
        let at = |decoded: &[(u32, u32, u32)], line: u32, character: u32| -> u32 {
            decoded
                .iter()
                .find(|(l, c, _)| *l == line && *c == character)
                .map(|(_, _, t)| *t)
                .unwrap_or(u32::MAX)
        };
        const FUNCTION: u32 = 4;
        const VARIABLE: u32 = 5;
        const PROPERTY: u32 = 6;
        const MACRO: u32 = 7;

        let tokens = decoded(&analyze(src).index.semantic_tokens(src));

        // C stdlib calls color as functions (lines 0–1 are the includes; a
        // header the document does not include arms no symbols).
        assert_eq!(at(&tokens, 6, 4), FUNCTION, "printf");
        assert_eq!(at(&tokens, 7, 4), FUNCTION, "memset");
        // Member accesses after `.` color as properties.
        assert_eq!(at(&tokens, 10, 6), PROPERTY, "w.cb");
        assert_eq!(at(&tokens, 11, 6), PROPERTY, "w.wid");
        // Locals — comma chains included — color as variables.
        assert_eq!(at(&tokens, 5, 8), VARIABLE, "x1");
        assert_eq!(at(&tokens, 5, 17), VARIABLE, "x2");
        // The macro use colors as a macro.
        assert_eq!(at(&tokens, 12, 12), MACRO, "FSQ_MAX use");
        // sizeof stays grammar-colored: no token at its position.
        assert_ne!(at(&tokens, 7, 15), VARIABLE, "sizeof must not tokenize");
    }

    #[test]
    /// Words inside comments and string literals keep their container's single
    /// color: the semantic scan runs through the same code mask find-references
    /// uses, so a keyword *mentioned* (`struct` in a comment, `int` in a
    /// string) never becomes a token.
    fn comments_and_strings_never_tokenize() {
        // Line 0's include arms the C table, so the `printf` *mentioned* in
        // the comments would classify as a function if the mask ever slipped.
        let src = "\
#include <stdio.h>
struct FsWidget { int wid; };
int main(void) {
    // mention struct and int in a comment
    /* block comment: FsWidget, printf */
    const char *s = \"int and printf\";
    struct FsWidget w;
    printf(\"%s\", s);
    return 0;
}
";
        // Decode the delta stream into (line, character) positions only.
        let decoded = |data: &[u32]| -> Vec<(u32, u32)> {
            let mut out = Vec::new();
            let (mut line, mut character) = (0u32, 0u32);
            let mut i = 0;
            while i + 4 < data.len() {
                if data[i] > 0 {
                    line += data[i];
                    character = data[i + 1];
                } else {
                    character += data[i + 1];
                }
                out.push((line, character));
                i += 5;
            }
            out
        };
        let tokens = decoded(&analyze(src).index.semantic_tokens(src));

        // Comment lines (3, 4) and the string literal on line 5: no token may
        // start inside them. (The `FsWidget`/`printf` mentioned there and the
        // `int` in the string would otherwise classify as type/function —
        // `main` on line 2 and the variable `s` on line 5 at col 16 are the
        // real code and must stay.) Lines 0–9:
        // 0 include, 1 struct decl, 2 main, 3 line comment, 4 block comment,
        // 5 string literal, 6 local w, 7 printf, 8 return, 9 }.
        for line in [3u32, 4, 5] {
            assert!(
                !tokens.iter().any(|(l, c)| *l == line && *c >= 4 && (*l, *c) != (5, 16)),
                "line {line} must carry no semantic tokens: {tokens:?}"
            );
        }
        // The real code around them still classifies.
        assert!(tokens.iter().any(|(l, c)| *l == 7 && *c == 4), "printf call: {tokens:?}");
        assert!(tokens.iter().any(|(l, c)| *l == 5 && *c == 16), "variable s: {tokens:?}");
    }

    #[test]
    /// `async`/`await` color as keywords only in keyword position — the same
    /// raw-source judgment the parser applies. An identifier spelled `async`
    /// (a parameter here) keeps its variable color, and `await + 1` stays an
    /// identifier (C compatibility: bare `+` is not an expression start).
    fn soft_keywords_color_only_in_keyword_position() {
        let src = "\
#include <stdio.h>
async int fetch(int n) {
    return n;
}
int main(void) {
    int async = 3;
    async = async + 1;
    printf(\"%d\", async);
    return 0;
}
";
        let decoded = |data: &[u32]| -> Vec<(u32, u32, u32)> {
            let mut out = Vec::new();
            let (mut line, mut character) = (0u32, 0u32);
            let mut i = 0;
            while i + 4 < data.len() {
                if data[i] > 0 {
                    line += data[i];
                    character = data[i + 1];
                } else {
                    character += data[i + 1];
                }
                out.push((line, character, data[i + 3]));
                i += 5;
            }
            out
        };
        let tokens = decoded(&analyze(src).index.semantic_tokens(src));
        let at = |l: u32, c: u32| tokens.iter().find(|(tl, tc, _)| *tl == l && *tc == c).map(|(_, _, t)| *t);
        const KEYWORD: u32 = 9;

        // Line 1 `async int fetch`: keyword position (followed by a type).
        assert_eq!(at(1, 0), Some(KEYWORD), "async modifier: {tokens:?}");
        // Lines 5–7: a local variable named `async` — identifier position
        // (followed by `=`, `+`, `)`), never a keyword token.
        for (line, col) in [(5u32, 8u32), (6, 4), (6, 12), (7, 17)] {
            assert_ne!(at(line, col), Some(KEYWORD), "{line}:{col} is an identifier: {tokens:?}");
        }
    }

    /// Typing `-` inside an argument list must not blank the editor's
    /// answers: the completion provider fires (trigger character) and the
    /// prefix degenerates to the number being typed or to empty — the full
    /// symbol list is the honest fallback, never the file-name suggestions
    /// VS Code shows when a provider answers nothing.
    #[test]
    fn completion_after_minus_keeps_the_symbol_list() {
        let src = "\
@interface Adder : jeti_root
- (int)add:(int)x to:(int)y;
@end

@implementation Adder
- (int)add:(int)x to:(int)y { return x + y; }
@end

int main(void) {
    Adder *a = 0;
    int r = [a add:-1 to:-2];
    return r;
}
";
        let analyzed = analyze(src);
        // Right after the `-` of `-1`: the send site is alive and the
        // signature help still answers with the active parameter.
        let pos = position(src, "-1", 1);
        let help = analyzed.index.signature_help(pos).expect("signature through the minus");
        assert_eq!(help.signatures.len(), 1, "{help:?}");
        // The completion prefix at that spot filters to the digit or empty;
        // both paths return the server's own list, never nothing-then-files.
        let prefix = crate::index::typed_name_at(src, pos).unwrap_or_default();
        assert!(prefix.is_empty() || prefix == "1", "unexpected prefix {prefix:?}");
    }

    #[test]
    /// Every selector *part* colors even though the stored label carries the
    /// colons (`addObject:`), and a class method's use carries the `static`
    /// modifier bit — the theme metadata clangd also sends.
    fn selector_parts_color_and_class_methods_carry_static() {
        let src = "\
@interface Token : NPObject
+ (id)alloc;
- (int)value;
@end
int main(void) {
    id t = [Token alloc];
    return 0;
}
";
        let data = analyze(src).index.semantic_tokens(src);
        let mut toks = Vec::new();
        let (mut line, mut character) = (0u32, 0u32);
        let mut i = 0;
        while i + 4 < data.len() {
            if data[i] > 0 {
                line += data[i];
                character = data[i + 1];
            } else {
                character += data[i + 1];
            }
            toks.push((line, character, data[i + 3], data[i + 4]));
            i += 5;
        }
        let at = |l: u32, c: u32| {
            toks.iter()
                .find(|(tl, tc, _, _)| *tl == l && *tc == c)
                .map(|(_, _, t, m)| (*t, *m))
        };
        // Line 5: `    id t = [Token alloc];` — receiver class first, then
        // the class-method selector with bit 0 (static) set.
        assert_eq!(at(5, 12), Some((1, 0)), "receiver class: {toks:?}");
        assert_eq!(at(5, 18), Some((4, 1)), "class method carries static: {toks:?}");
        // A multi-part selector's parts match per colon segment.
        assert!(selector_part_matches("addObject", "addObject:"));
        assert!(selector_part_matches("atIndex", "setObject:atIndex:"));
        assert!(!selector_part_matches("Object", "addObject:"));
    }

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

    /// Namespaces written as blocks inside one another. Flattening loses the
    /// most here — the inner block has to be put back before the outer one can
    /// adopt it — and the qualified-name-only shape above never exercises that.
    const NESTED_NAMESPACES: &str = "\
@namespace Game
    @namespace Entities
        @interface Player : jeti_root
        - (void)deep;
        @end
    @endnamespace
@endnamespace

int main() {
    Game::Entities::Player *p;
    return 0;
}
";

    #[test]
    fn nested_namespace_blocks_nest_in_the_outline_and_resolve_to_their_line() {
        let analyzed = analyze(NESTED_NAMESPACES);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);
        let text = NESTED_NAMESPACES;

        // Each segment answers with the block that declared it, clangd-style:
        // `Game` line 1, `Entities` line 2, `Player` the class.
        for (inside, line) in [(0, 0), (7, 1), (16, 2)] {
            let definitions =
                analyzed.index.definitions_at(text, position(text, "Game::Entities::Player *p", inside));
            assert_eq!(definitions.len(), 1, "byte {inside}: {definitions:?}");
            assert_eq!(definitions[0].range.start.line, line, "byte {inside}");
        }

        // The outline is the nesting the source had, with each container named
        // for breadcrumbs.
        let symbols = analyzed.index.document_symbols("/tmp/jeti_lsp_unit/symbols.jeti");
        let names: Vec<&str> = symbols.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Game", "main"], "{symbols:#?}");
        let entities = &symbols[0]["children"][0];
        assert_eq!(entities["name"], "Entities");
        assert_eq!(entities["containerName"], "Game");
        assert_eq!(entities["range"]["start"], json!({"line": 1, "character": 4}));
        let player = &entities["children"][0];
        assert_eq!(player["name"], "Player");
        assert_eq!(player["containerName"], "Game::Entities");
        assert_eq!(player["children"][0]["name"], "deep");

        // Hover says which namespace a block sits in, the same way breadcrumbs
        // are read.
        let hover = analyzed
            .index
            .resolve_at(text, position(text, "Game::Entities::Player *p", 7));
        assert_eq!(hover, Some(index::Resolved::Symbol("Game::Entities".to_string())));
    }

    #[test]
    fn a_namespace_reopened_twice_keeps_its_blocks_apart() {
        // Two blocks of one name, and one with no members at all: adoption is by
        // extent, so each holds its own declarations and the empty one still
        // appears in the outline.
        const SRC: &str = "\
@namespace Empty
@endnamespace

@namespace Reopened
@interface One : jeti_root
@end
@endnamespace

@namespace Reopened
@interface Two : jeti_root
@end
@endnamespace

int main() {
    Reopened::One *a;
    Reopened::Two *b;
    return 0;
}
";
        let analyzed = analyze(SRC);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);
        let text = SRC;

        let symbols = analyzed.index.document_symbols("/tmp/jeti_lsp_unit/symbols.jeti");
        let names: Vec<&str> = symbols.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Empty", "Reopened", "Reopened", "main"], "{symbols:#?}");
        assert!(symbols[0].get("children").is_none(), "an empty block, {symbols:#?}");
        assert_eq!(symbols[1]["children"][0]["name"], "One");
        assert_eq!(symbols[2]["children"][0]["name"], "Two");

        // The name of a reopened namespace answers with both declarations, the
        // way a class split across a header and an implementation does.
        let definitions =
            analyzed.index.definitions_at(text, position(text, "Reopened::One", 11));
        assert_eq!(definitions.len(), 1, "{definitions:?}");
        assert_eq!(definitions[0].range.start.line, 4);
        let reopenings = analyzed
            .index
            .definitions_at(text, position(text, "Reopened::One", 0));
        assert_eq!(
            reopenings.iter().map(|d| d.range.start.line).collect::<Vec<_>>(),
            vec![3, 8],
            "{reopenings:?}"
        );
    }

    #[test]
    fn a_namespace_declared_in_an_imported_header_still_has_a_target() {
        // The block is in another file: the index points into it, and the
        // outline of the file being edited leaves it out — the header shows it
        // when the header itself is open.
        let dir = std::env::temp_dir().join("jeti_lsp_ns_import");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let header = dir.join("shapes.hje");
        std::fs::write(
            &header,
            "@namespace Shapes\n@interface Circle : jeti_root\n- (void)draw;\n@end\n@endnamespace\n",
        )
        .expect("write header");
        let source = "#import \"shapes.hje\"\n\nint main() {\n    Shapes::Circle *c;\n    return 0;\n}\n";
        let path = dir.join("main.jeti");
        let analyzed = analyze_for_editor(
            path.to_str().unwrap(),
            source,
            &AnalysisOptions::default(),
            &no_open_files(),
        );
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        let definitions = analyzed
            .index
            .definitions_at(source, position(source, "Shapes::Circle", 3));
        assert_eq!(definitions.len(), 1, "{definitions:?}");
        assert!(definitions[0].path.ends_with("shapes.hje"), "{definitions:?}");
        assert_eq!(definitions[0].range.start.line, 0, "{definitions:?}");

        let symbols = analyzed.index.document_symbols(path.to_str().unwrap());
        let names: Vec<&str> = symbols.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["main"], "{symbols:#?}");
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
    fn an_id_receiver_completes_its_protocols() {
        const SRC: &str = r#"
@protocol Root
- (void)rootAction:;
@end

@protocol Task <Root>
- (void)taskAction:;
@end

@interface TaskWorker : jeti_root
- (void)worker;
@end

@implementation TaskWorker
- (void)worker {
    id<Task> t = nil;
    [t taskAction:nil];
    [t rootAction:nil];
    id u = nil;
    [u worker];
}
@end
"#;
        let analyzed = analyze(SRC);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        // An `id<Task>` local: the receiver is a protocol, not a class.
        let receiver = analyzed.index.send_receiver(position(SRC, "[t taskAction", 3));
        assert_eq!(
            receiver,
            Some(ReceiverInfo {
                class: None,
                protocols: vec!["Task".to_string()],
                is_class_receiver: false,
                type_args: Vec::new(),
            })
        );

        // Members: Task's own selectors plus the ones inherited from Root.
        // Labels are the binder's canonical selector form (the same
        // colon-stripped spelling the conformance checker compares with).
        let protocols = receiver.map(|r| r.protocols).unwrap_or_default();
        let members = analyzed
            .index
            .member_completion(None, &protocols, false, "");
        let labels = members
            .into_iter()
            .map(|member| member.label)
            .collect::<Vec<_>>();
        assert!(labels.contains(&"taskAction".to_string()), "{labels:?}");
        assert!(labels.contains(&"rootAction".to_string()), "{labels:?}");

        // An untyped id has no protocol to complete against: no members.
        let receiver = analyzed.index.send_receiver(position(SRC, "[u worker", 3));
        assert_eq!(receiver, None);
    }

    /// A concrete receiver instantiates its class: the signature it is
    /// filling in shows the receiver's arguments where the class declares
    /// its parameters — in the return type and in the parameter labels.
    #[test]
    fn a_generic_receivers_signature_shows_its_arguments() {
        const SRC: &str = "\
@interface Thing : jeti_root
@end

@interface NPBox<T> : jeti_root
- (T *)value;
- (void)store:(T *)thing;
@end

@implementation NPBox
@end

int main() {
    Thing *thing;
    NPBox<Thing> *box;
    [box value];
    [box store:thing];
    return 0;
}
";
        let analyzed = analyze(SRC);
        assert!(analyzed.diagnostics.is_empty(), "{:?}", analyzed.diagnostics);

        // Inside `[box value]`: the declared `T *` return is the receiver's
        // argument.
        let help = analyzed
            .index
            .signature_help(position(SRC, "[box value]", 6))
            .expect("a signature under the cursor");
        assert_eq!(help.signatures.len(), 1, "{help:?}");
        assert!(help.signatures[0].label.contains("Thing"), "{help:?}");
        assert!(!help.signatures[0].label.contains("T *"), "{help:?}");

        // And the parameter labels carry it too (LSP matches them as
        // substrings of the label).
        let help = analyzed
            .index
            .signature_help(position(SRC, "[box store", 5))
            .expect("a signature under the cursor");
        assert_eq!(help.signatures[0].parameters, vec!["(Thing *)thing".to_string()], "{help:?}");
        assert!(help.signatures[0].label.contains("(Thing *)thing"), "{help:?}");
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
        let instance = labels(analyzed.index.member_completion(Some("Widget"), &[], false, ""));
        assert_eq!(instance, vec!["baseMethod", "count", "widgetMethod:"], "{instance:?}");
        // A class receiver gets the class methods only.
        let class_methods = labels(analyzed.index.member_completion(Some("Widget"), &[], true, ""));
        assert_eq!(class_methods, vec!["make"]);
        // The prefix filters, and the completion kind is the completion enum's
        // (Method = 2).
        let filtered = analyzed.index.member_completion(Some("Widget"), &[], false, "widget");
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
        assert_eq!(receiver, Some(ReceiverInfo { class: Some("Widget".to_string()), protocols: Vec::new(), is_class_receiver: false, type_args: Vec::new() }));

        // `[w …` — a local, resolved through the scope.
        let receiver = analyzed.index.send_receiver(position(MEMBERS, "[w widgetMethod", 3));
        assert_eq!(receiver, Some(ReceiverInfo { class: Some("Widget".to_string()), protocols: Vec::new(), is_class_receiver: false, type_args: Vec::new() }));

        // `[Widget …` — a class receiver, so class methods answer.
        let receiver = analyzed.index.send_receiver(position(MEMBERS, "[Widget make", 8));
        assert_eq!(receiver, Some(ReceiverInfo { class: Some("Widget".to_string()), protocols: Vec::new(), is_class_receiver: true, type_args: Vec::new() }));

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

    // ─── C17 6.5.x / 6.8.4.2 diagnostic families ────────────────────────
    //
    // clang-frozen wording/severity (tests/negative/c_std/README.md matrix).
    // These run the same analyze_for_editor path the editor sees — the whole
    // point of the families is that the terminal and the editor agree.

    fn diag_messages(src: &str) -> Vec<String> {
        let doc = analyze_for_editor(
            "/tmp/jeti_lsp_unit/family.jeti",
            src,
            &AnalysisOptions::default(),
            &no_open_files(),
        );
        doc.diagnostics.iter()
            .filter_map(|d| d["message"].as_str().map(String::from))
            .collect()
    }

    #[test]
    fn undeclared_identifier_family_matches_clang() {
        let msgs = diag_messages("\
int known(int v) { return v; }
int main(void) {
    int y = x + 1;      // ① undeclared identifier → error
    ghost_fn(1);        // ② implicit function declaration → error
    int z = known(2);   // ③ declared → silent
    return y + z;
}
");
        assert!(msgs.iter().any(|m| m.contains("use of undeclared identifier 'x'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "call to undeclared function 'ghost_fn'; ISO C99 and later do not support implicit function declarations")),
            "{msgs:?}");
        assert_eq!(msgs.len(), 2, "declared call must stay silent: {msgs:?}");
    }

    #[test]
    fn call_family_matches_clang() {
        let msgs = diag_messages("\
int two_args(int a, int b) { return a + b; }
int one_arg(int a) { return a; }
int main(void) {
    int r = two_args(1);      // too few → error
    r = one_arg(1, 2);        // too many (single param names it) → error
    int not_fn = 0;
    not_fn(1);                // called object type 'int' → error
    r = two_args(1, 2);       // exact → silent
    r = one_arg(3);           // exact → silent
    return r;
}
");
        assert!(msgs.iter().any(|m| m.contains(
            "too few arguments to function call, expected 2, have 1")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "too many arguments to function call, expected single argument 'a', have 2 arguments")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "called object type 'int' is not a function or function pointer")), "{msgs:?}");
        assert_eq!(msgs.len(), 3, "exact-arity calls must stay silent: {msgs:?}");
    }

    #[test]
    fn assign_family_matches_clang() {
        let msgs = diag_messages("\
struct S { int a; };
struct T { int b; };
int main(void) {
    int v = 0;
    int *p = &v;
    char *c = 0;
    struct S s;
    struct T t;
    p = c;        // ptr ↔ ptr → warning (clang -Wincompatible-pointer-types)
    v = p;        // ptr → int → error 'dereference with *'
    v = &v;       // &value → error 'remove &'
    p = v;        // int → ptr → error 'take the address with &'
    s = v;        // struct ← scalar → error
    s = t;        // struct ← foreign struct → error
    p = 0;        // zero literal → silent
    v = 2;        // int ← int → silent
    return 0;
}
");
        assert!(msgs.iter().any(|m| m.contains(
            "incompatible pointer types assigning to 'int *' from 'char *'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "incompatible pointer to integer conversion assigning to 'int' from 'int *'; dereference with *")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "incompatible pointer to integer conversion assigning to 'int' from 'int *'; remove &")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "incompatible integer to pointer conversion assigning to 'int *' from 'int'; take the address with &")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "assigning to 'struct S' from incompatible type 'int'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "assigning to 'struct S' from incompatible type 'struct T'")), "{msgs:?}");
        // Zero/int literals stay silent: exactly the six shapes above.
        let loud = msgs.iter().filter(|m| m.contains("assigning")
            || m.contains("conversion")).count();
        assert_eq!(loud, 6, "silent shapes must not fire: {msgs:?}");
    }

    #[test]
    fn const_assign_family_matches_clang_after_declarator_fix() {
        let msgs = diag_messages("int main(void) {
    const int z = 1;
    int const w = 2;
    int *const cp = 0;
    int *q = 0;
    z = 2;        // leading const → error
    w = 3;        // declarator const → error
    cp = 0;       // pointer-level const → error 'int *const'
    q = cp;       // top-level const read → silent (C)
    return 0;
}
");
        assert!(msgs.iter().any(|m| m.contains(
            "cannot assign to variable 'z' with const-qualified type 'int'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "cannot assign to variable 'w' with const-qualified type 'int'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "cannot assign to variable 'cp' with const-qualified type 'int *const'")), "{msgs:?}");
        assert_eq!(msgs.len(), 3, "reading a const pointer must stay silent: {msgs:?}");
    }

    #[test]
    fn enum_member_case_labels_fold_for_duplicates() {
        let msgs = diag_messages("enum Color { RED = 1, BLUE };
int main(void) {
    int v = 1;
    switch (v) {
        case 1: break;
        case RED: break;   // both equal 1 → error
        case BLUE: break;  // distinct → silent
    }
    return 0;
}
");
        assert!(msgs.iter().any(|m| m.contains(
            "duplicate case value: '1' and 'RED' both equal '1'")), "{msgs:?}");
        assert_eq!(msgs.len(), 1, "distinct members must stay silent: {msgs:?}");
    }

    #[test]
    fn nested_calls_in_assignment_values_reach_the_call_family() {
        let msgs = diag_messages("int one(int a) { return a; }
int wrap(int v) { return v; }
int main(void) {
    int r = wrap(one(1));    // nested correct → silent
    r = wrap(one(1, 2));     // nested too-many → error
    r = wrap(one());         // nested too-few → error
    return r;
}
");
        assert!(msgs.iter().any(|m| m.contains(
            "too many arguments to function call, expected single argument 'a', have 2 arguments")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "too few arguments to function call, expected 1, have 0")), "{msgs:?}");
        assert_eq!(msgs.len(), 2, "{msgs:?}");
    }

    #[test]
    fn object_pointer_upcast_is_silent_downcast_warns() {
        let msgs = diag_messages("\
@interface Foo : jeti_root
@end
@interface Sub : Foo
@end
@interface Other : jeti_root
@end

@implementation Foo
-(void) m {
    Foo *f = 0;
    Sub *s = 0;
    Other *o = 0;
    f = s;   // child → parent: silent (probe_objc ①)
    s = f;   // parent → child: warning
    f = o;   // unrelated: warning
    return;
}
@end
");
        let warns: Vec<&String> = msgs.iter().filter(|m| m.contains("incompatible pointer types assigning")).collect();
        assert_eq!(warns.len(), 2, "upcast must be silent: {msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "incompatible pointer types assigning to 'Sub *' from 'Foo *'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "incompatible pointer types assigning to 'Foo *' from 'Other *'")), "{msgs:?}");
    }

    #[test]
    fn switch_family_matches_clang() {
        let msgs = diag_messages("\
enum Color { RED = 1 };
int main(void) {
    int v = 1;
    int *pv = &v;
    switch (pv) {          // pointer subject → error
        case 1: break;
    }
    switch (v) {
        case 1: break;
        case 1: break;     // duplicate → error
        case 1.5: break;   // non-integer constant → error
        default: break;
        default: break;    // multiple default → error
    }
    switch (v) {
        case 1: break;
        default: break;
        case 2: break;     // case after default → silent (measured legal)
    }
    return 0;
}
");
        assert!(msgs.iter().any(|m| m.contains(
            "statement requires expression of integer type ('int *' invalid)")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("duplicate case value '1'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(
            "integer constant expression must have integer type, not 'double'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("multiple default labels in one switch")), "{msgs:?}");
        assert_eq!(msgs.len(), 4, "case-after-default must stay silent: {msgs:?}");
    }

    #[test]
    fn stdlib_calls_and_bindings_stay_silent_under_includes() {
        // Passthrough headers gate the whitelists: stdlib functions and
        // header-declared bindings (errno, STDIN_FILENO…) must not trip the
        // undeclared checks, while a genuinely undeclared name still does.
        let msgs = diag_messages("\
#include <stdio.h>
#include <errno.h>
int main(void) {
    printf(\"%d\\n\", errno);
    ghost_fn(1);
    return 0;
}
");
        assert!(msgs.iter().any(|m| m.contains("call to undeclared function 'ghost_fn'")), "{msgs:?}");
        assert!(!msgs.iter().any(|m| m.contains("'printf'")), "{msgs:?}");
        assert!(!msgs.iter().any(|m| m.contains("'errno'")), "{msgs:?}");
    }
}
