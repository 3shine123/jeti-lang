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
fn same_file(a: &str, b: &str) -> bool {
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
    // The document's own directory resolves its sibling `#import`s; the
    // configured dirs (which default to the CLI's) follow.
    let mut dirs: Vec<String> = Vec::new();
    if let Some(parent) = Path::new(path).parent() {
        let dir = parent.to_string_lossy().to_string();
        if !dir.is_empty() {
            dirs.push(dir);
        }
    }
    dirs.extend(search_dirs.iter().cloned());

    let mut pipeline = Pipeline::new();
    pipeline.search_dirs = dirs;
    pipeline
        .analyze(text, path)
        .iter()
        .map(|d| diagnostic_to_json(d, path, text, text_of))
        .collect()
}

/// One diagnostic in LSP shape.
///
/// The byte→UTF-16 projection runs on the text of the file the diagnostic
/// points into, not on the document being edited — a diagnostic inside an
/// imported header carries that header's columns.
pub fn diagnostic_to_json(
    diagnostic: &Diagnostic,
    main_path: &str,
    main_text: &str,
    text_of: &dyn Fn(&str) -> Option<String>,
) -> Value {
    let ownership = if same_file(&diagnostic.file, main_path) {
        Some(main_text.to_string())
    } else {
        text_of(&diagnostic.file).or_else(|| std::fs::read_to_string(&diagnostic.file).ok())
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
    let pos = |p: LspPosition| json!({ "line": p.line, "character": p.character });
    json!({ "start": pos(range.start), "end": pos(range.end) })
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
                        vec![self.publish(uri)]
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
                if let (Some(doc), Some(text)) = (self.docs.get_mut(uri), text) {
                    doc.text = text.to_string();
                    return vec![self.publish(uri)];
                }
                Vec::new()
            }
            "textDocument/didClose" => {
                let uri = message
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.docs.remove(uri);
                // Diagnostics are owned by the server: clearing them is how the
                // client is told they are stale.
                vec![publish_empty(uri)]
            }
            "" => Vec::new(),
            other => match id {
                Some(id) => vec![error_response(id, -32601, &format!("unhandled method: {other}"))],
                None => Vec::new(),
            },
        }
    }

    fn publish(&self, uri: &str) -> Value {
        let Some(doc) = self.docs.get(uri) else {
            return publish_empty(uri);
        };
        let text_of = |path: &str| {
            self.docs
                .values()
                .find(|d| same_file(&d.path, path))
                .map(|d| d.text.clone())
        };
        let diagnostics = diagnostics_for(&doc.path, &doc.text, &self.search_dirs, &text_of);
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics },
        })
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
}
