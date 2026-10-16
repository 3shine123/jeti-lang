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
use jeti_cst::source_map::{LspPosition, LspRange};
use jetic::pipeline::Pipeline;
use serde_json::{json, Value};

pub mod index;
pub use index::{symbol_at, Location, Symbol, SymbolIndex};

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
    analyze_for_editor(path, text, search_dirs, text_of).diagnostics
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
    search_dirs: &[String],
    text_of: &dyn Fn(&str) -> Option<String>,
) -> AnalyzedDocument {
    let mut pipeline = Pipeline::new();
    pipeline.search_dirs = search_dirs_for(path, search_dirs);
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
        Some(ast) => SymbolIndex::build(ast, &analysis.source_map, &resolve),
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
    /// Extra `#import` search dirs, after the document's own directory.
    search_dirs: Vec<String>,
    shutdown_received: bool,
}

impl Server {
    /// `search_dirs` empty → the CLI's defaults (`.`, `include`,
    /// `include/Foundation`), so a plain `jeti-lsp` in a project root behaves
    /// like `jetic` does there.
    pub fn new(search_dirs: Vec<String>) -> Self {
        let search_dirs = if search_dirs.is_empty() {
            Pipeline::new().search_dirs
        } else {
            search_dirs
        };
        Server {
            docs: HashMap::new(),
            indexes: HashMap::new(),
            search_dirs,
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
            "initialize" => vec![response(
                id,
                json!({
                    "capabilities": {
                        // 1 = Full: the client sends the whole buffer on every
                        // change. Incremental sync needs range-diff handling and
                        // is a later stage.
                        "textDocumentSync": 1,
                        "documentSymbolProvider": true,
                        "definitionProvider": true,
                    },
                    "serverInfo": {
                        "name": "jeti-lsp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )],
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
                // Full sync: the last content change is the whole new buffer.
                let text = message
                    .pointer("/params/contentChanges")
                    .and_then(Value::as_array)
                    .and_then(|c| c.last())
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str);
                if let Some(text) = text {
                    if let Some(doc) = self.docs.get_mut(uri) {
                        doc.text = text.to_string();
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
                let uri = message
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let position = message.pointer("/params/position");
                let position = LspPosition {
                    line: position
                        .and_then(|p| p.get("line"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                    character: position
                        .and_then(|p| p.get("character"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                };
                let locations = self.definitions(uri, position);
                let result = if locations.is_empty() {
                    Value::Null
                } else {
                    Value::Array(locations)
                };
                vec![response(id, result)]
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
        // Scoped so the immutable borrows of `self` end before the index is
        // stored (the closure borrows `self.docs`).
        let analyzed = {
            let Some(doc) = self.docs.get(uri) else {
                return publish_empty(uri);
            };
            let text_of = self.text_of();
            analyze_for_editor(&doc.path, &doc.text, &self.search_dirs, &text_of)
        };
        self.indexes.insert(uri.to_string(), analyzed.index);
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": analyzed.diagnostics },
        })
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
        let Some(name) = symbol_at(&doc.text, position) else {
            return Vec::new();
        };
        index
            .definitions(&name)
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
        let mut server = Server::new(Vec::new());
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
    fn unhandled_request_gets_method_not_found() {
        let mut server = Server::new(Vec::new());
        let req = json!({"jsonrpc": "2.0", "id": 7, "method": "textDocument/hover", "params": {}});
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
        analyze_for_editor("/tmp/jeti_lsp_unit/symbols.jeti", source, &[], &no_open_files())
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

    #[test]
    fn a_failed_front_end_offers_no_symbols() {
        // A parse error stops before elaboration: diagnostics yes, symbols no.
        let analyzed = analyze("int main(void) { return 0 }\n");
        assert!(!analyzed.diagnostics.is_empty());
        assert!(analyzed.index.document_symbols("/tmp/jeti_lsp_unit/broken.jeti").is_empty());
        assert!(analyzed.index.definitions("main").is_empty());
    }

    #[test]
    fn definition_and_outline_requests_answer_from_the_cache() {
        let mut server = Server::new(Vec::new());
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
