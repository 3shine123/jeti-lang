//! End-to-end: drive the real `jeti-lsp` binary over stdio the way an editor
//! does — initialize, open a buffer with a syntax error, expect
//! `publishDiagnostics`, fix it, then use the outline and definition requests
//! (including a jump into an imported Foundation header), and shut down.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};

use serde_json::{json, Value};

fn frame(message: &Value) -> String {
    let body = serde_json::to_string(message).unwrap();
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

/// Read one `Content-Length`-framed JSON message from the child's stdout.
fn read_frame(reader: &mut BufReader<ChildStdout>) -> Value {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read header");
        assert_ne!(n, 0, "server closed stdout before answering");
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse().expect("Content-Length"));
            }
        }
    }
    let mut body = vec![0u8; length.expect("Content-Length present")];
    std::io::Read::read_exact(reader, &mut body).expect("read body");
    serde_json::from_slice(&body).expect("json body")
}

/// The (0-based line, UTF-16 character) an editor would report for the first
/// `needle` in `text`, `extra` characters into it.
fn position_of(text: &str, needle: &str) -> Value {
    position_in(text, needle, 0)
}

/// [`position_of`], shifted `extra` characters (all test needles are ASCII).
fn position_in(text: &str, needle: &str, extra: usize) -> Value {
    let index = text.find(needle).expect("needle in text") + extra;
    let line = text[..index].matches('\n').count();
    let line_start = text[..index].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = text[line_start..index].encode_utf16().count();
    json!({ "line": line, "character": character })
}

fn repo_include_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("include")
}

/// A program with a class, a method, a send, and one imported Foundation type
/// (`NPString`) so definition has somewhere *outside* this file to point.
const PROGRAM: &str = "\
#import <Foundation/Foundation.hje>

@interface Greeter : NPObject
- (void)greet:(NPString *)name from:(NPString *)where;
@end

@implementation Greeter
- (void)greet:(NPString *)name from:(NPString *)where {
    [self greet:name from:name];
}
@end

int main(void) {
    NPString *s = @\"hi\";
    [s UTF8String];
    return 0;
}
";

#[test]
fn server_answers_editors_requests_end_to_end() {
    let dir = std::env::temp_dir().join("jeti_lsp_session_test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("session.jeti");
    std::fs::write(&path, "int main(void) { return 0 }\n").expect("write probe file");
    let uri = format!("file://{}", path.display());

    let mut child = Command::new(env!("CARGO_BIN_EXE_jeti-lsp"))
        .arg("--include")
        .arg(repo_include_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn jeti-lsp");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    // initialize → response with the capabilities we advertise
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "processId": null, "rootUri": null, "capabilities": {} },
        })).as_bytes())
        .expect("write initialize");
    stdin.write_all(frame(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})).as_bytes())
        .expect("write initialized");
    stdin.flush().expect("flush");
    let response = read_frame(&mut stdout);
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["capabilities"]["textDocumentSync"], 1);
    assert_eq!(response["result"]["capabilities"]["documentSymbolProvider"], true);
    assert_eq!(response["result"]["capabilities"]["definitionProvider"], true);
    assert_eq!(response["result"]["serverInfo"]["name"], "jeti-lsp");

    // didOpen → publishDiagnostics for the syntax error on line 0
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1,
                "text": "int main(void) { return 0 }\n",
            }},
        })).as_bytes())
        .expect("write didOpen");
    stdin.flush().expect("flush");

    let published = read_frame(&mut stdout);
    assert_eq!(published["method"], "textDocument/publishDiagnostics");
    assert_eq!(published["params"]["uri"], uri);
    let diagnostics = published["params"]["diagnostics"].as_array().expect("diagnostics").clone();
    assert!(!diagnostics.is_empty(), "expected a syntax diagnostic: {published}");
    assert_eq!(diagnostics[0]["severity"], 1);
    assert_eq!(diagnostics[0]["range"]["start"], json!({"line": 0, "character": 24}));

    // didChange → the real program: diagnostics clear, and the index is built.
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": PROGRAM }],
            },
        })).as_bytes())
        .expect("write didChange");
    stdin.flush().expect("flush");
    let cleared = read_frame(&mut stdout);
    assert_eq!(cleared["params"]["diagnostics"], json!([]));

    // documentSymbol → the outline of *this* file (imported Foundation decls are
    // not the editor's outline).
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } },
        })).as_bytes())
        .expect("write documentSymbol");
    stdin.flush().expect("flush");
    let symbols = read_frame(&mut stdout);
    let names: Vec<&str> = symbols["result"]
        .as_array()
        .expect("symbol array")
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Greeter", "Greeter", "main"], "{symbols}");
    let methods = symbols["result"][1]["children"].as_array().expect("class members");
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0]["name"], "greet:from:");
    assert_eq!(methods[0]["detail"], "- greet:from:");
    assert_eq!(methods[0]["range"]["start"], position_of(PROGRAM, "- (void)greet:(NPString *)name from:(NPString *)where {"));

    // definition on `greet:` inside the send → the declaration and the
    // implementation, both in this file.
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "textDocument/definition",
            "params": { "textDocument": { "uri": uri }, "position": position_of(PROGRAM, "greet:name from") },
        })).as_bytes())
        .expect("write definition");
    stdin.flush().expect("flush");
    let definition = read_frame(&mut stdout);
    let locations = definition["result"].as_array().expect("locations");
    assert_eq!(locations.len(), 2, "{definition}");
    assert_eq!(locations[0]["uri"], uri);
    assert_eq!(locations[0]["range"]["start"], position_of(PROGRAM, "- (void)greet:(NPString *)name from:(NPString *)where;"));

    // definition on the imported type → a header outside this document, with an
    // absolute `file:` URI the editor can open.
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "textDocument/definition",
            "params": { "textDocument": { "uri": uri }, "position": position_of(PROGRAM, "NPString *s") },
        })).as_bytes())
        .expect("write definition");
    stdin.flush().expect("flush");
    let definition = read_frame(&mut stdout);
    let locations = definition["result"].as_array().expect("locations");
    assert_eq!(locations.len(), 1, "{definition}");
    let target = locations[0]["uri"].as_str().expect("uri");
    assert!(target.starts_with("file:///"), "absolute uri expected, got {target}");
    assert!(target.ends_with("include/Foundation/NPString.hje"), "got {target}");

    // A position with no symbol is `null`, never an invented target.
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "textDocument/definition",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 0 } },
        })).as_bytes())
        .expect("write definition");
    stdin.flush().expect("flush");
    let definition = read_frame(&mut stdout);
    assert!(definition["result"].is_null(), "{definition}");

    // hover on the imported type → markdown with the declaration and its file.
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "textDocument/hover",
            "params": { "textDocument": { "uri": uri }, "position": position_of(PROGRAM, "NPString *s") },
        })).as_bytes())
        .expect("write hover");
    stdin.flush().expect("flush");
    let hover = read_frame(&mut stdout);
    let markdown = hover["result"]["contents"]["value"].as_str().expect("markdown hover");
    assert!(markdown.starts_with("**class** `NPString`"), "{markdown}");
    assert!(markdown.contains("```jeti"), "{markdown}");
    assert!(markdown.contains("NPString.hje"), "{markdown}");

    // completion with a prefix typed inside a word: `NPS…` offers the class.
    let mut position = position_of(PROGRAM, "NPString *s");
    position["character"] = json!(position["character"].as_u64().unwrap() + 3);
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 7, "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": position },
        })).as_bytes())
        .expect("write completion");
    stdin.flush().expect("flush");
    let completion = read_frame(&mut stdout);
    let items = completion["result"]["items"].as_array().expect("items");
    let labels: Vec<&str> = items.iter().map(|item| item["label"].as_str().unwrap()).collect();
    assert!(labels.contains(&"NPString"), "{labels:?}");
    assert!(labels.iter().all(|label| label.starts_with("NPS")), "{labels:?}");
    let npstring = items.iter().find(|item| item["label"] == "NPString").expect("NPString item");
    // CompletionItemKind.Class = 7 (SymbolKind numbers are different).
    assert_eq!(npstring["kind"], 7);

    // Member completion: inside a send, the receiver's own class answers. The
    // cursor sits at `[s UTF8S`, so either the receiver's members or nothing.
    let mut position = position_of(PROGRAM, "[s UTF8S");
    position["character"] = json!(position["character"].as_u64().unwrap() + 8);
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 8, "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": position },
        })).as_bytes())
        .expect("write completion");
    stdin.flush().expect("flush");
    let completion = read_frame(&mut stdout);
    let items = completion["result"]["items"].as_array().expect("items");
    let labels: Vec<&str> = items.iter().map(|item| item["label"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["UTF8String"], "{labels:?}");
    assert_eq!(items[0]["kind"], 2, "CompletionItemKind.Method");

    // signatureHelp inside the second argument of the two-part selector.
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 8, "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": uri },
                "position": position_in(PROGRAM, "[self greet:name from:name", 22),
            },
        })).as_bytes())
        .expect("write signatureHelp");
    stdin.flush().expect("flush");
    let help = read_frame(&mut stdout);
    let signature = &help["result"]["signatures"][0];
    assert_eq!(help["result"]["activeParameter"], 1, "{help}");
    assert_eq!(signature["label"], "- (void)greet:(NPString *)name from:(NPString *)where");
    assert_eq!(signature["parameters"][1]["label"], "(NPString *)where");

    // shutdown → response, exit → process ends with status 0
    stdin
        .write_all(frame(&json!({"jsonrpc": "2.0", "id": 9, "method": "shutdown"})).as_bytes())
        .expect("write shutdown");
    stdin.flush().expect("flush");
    let shutdown = read_frame(&mut stdout);
    assert_eq!(shutdown["id"], 9);
    assert!(shutdown["result"].is_null());

    stdin
        .write_all(frame(&json!({"jsonrpc": "2.0", "method": "exit"})).as_bytes())
        .expect("write exit");
    drop(stdin);

    let status = child.wait().expect("wait for jeti-lsp");
    assert_eq!(status.code(), Some(0), "clean shutdown exits 0");
}
