//! End-to-end: drive the real `jeti-lsp` binary over stdio the way an editor
//! does — initialize, open a buffer with a syntax error, expect
//! `publishDiagnostics`, fix it, then use the outline and definition requests
//! (including a jump into an imported Foundation header), and shut down.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};

use serde_json::{json, Value};

/// Spawn the server under test with `args`.
fn spawn(args: &[&str]) -> (std::process::Child, std::process::ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_jeti-lsp"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn jeti-lsp");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = BufReader::new(child.stdout.take().expect("stdout"));
    (child, stdin, stdout)
}

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

/// The flag a project that builds MRC passes: the analysis then matches the
/// build, so legacy `dealloc` code is not reported as an ARC error.
#[test]
fn the_arc_switch_reaches_the_analysis() {
    let dir = std::env::temp_dir().join("jeti_lsp_session_test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("mrc.jeti");
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
    std::fs::write(&path, mrc).expect("write probe file");
    let uri = format!("file://{}", path.display());

    let (mut child, mut stdin, mut stdout) = spawn(&["-fno-jeti-arc"]);
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {},
        })).as_bytes())
        .expect("write initialize");
    stdin.flush().expect("flush");
    read_frame(&mut stdout);

    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": mrc,
            }},
        })).as_bytes())
        .expect("write didOpen");
    stdin.flush().expect("flush");
    let published = read_frame(&mut stdout);
    assert_eq!(
        published["params"]["diagnostics"],
        json!([]),
        "MRC mode must accept manual releases: {published}"
    );

    stdin
        .write_all(frame(&json!({"jsonrpc": "2.0", "id": 9, "method": "shutdown"})).as_bytes())
        .expect("write shutdown");
    stdin.write_all(frame(&json!({"jsonrpc": "2.0", "method": "exit"})).as_bytes())
        .expect("write exit");
    stdin.flush().expect("flush");
    drop(stdin);
    assert_eq!(child.wait().expect("wait").code(), Some(0));
}

/// A syntax error must not hide the diagnostics *after* it.
///
/// One forgotten `;` used to stop the descent at the parser: the type errors
/// further down stayed invisible until the `;` was fixed (fix one, see one), and
/// because no AST came back the outline, definition, hover and completion went
/// blank for the whole file too. The editor path (`Pipeline::analyze_document`)
/// hands the recovered CST to the later stages; `transpile` still refuses to
/// emit code for such a file.
#[test]
fn a_syntax_error_does_not_hide_later_diagnostics() {
    let dir = std::env::temp_dir().join("jeti_lsp_session_test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("mixed.jeti");
    let mixed = "\
int main(void) {
    int a = 1
    [self release];
    return 0;
}
";
    std::fs::write(&path, mixed).expect("write probe file");
    let uri = format!("file://{}", path.display());

    let (mut child, mut stdin, mut stdout) = spawn(&[]);
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {},
        })).as_bytes())
        .expect("write initialize");
    stdin.flush().expect("flush");
    read_frame(&mut stdout);

    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": mixed,
            }},
        })).as_bytes())
        .expect("write didOpen");
    stdin.flush().expect("flush");
    let published = read_frame(&mut stdout);
    let diagnostics = published["params"]["diagnostics"].as_array().expect("diagnostics").clone();
    let messages: Vec<&str> =
        diagnostics.iter().map(|d| d["message"].as_str().unwrap()).collect();
    assert!(
        messages.iter().any(|m| m.contains("expected ';'")),
        "the syntax error must be reported: {published}"
    );
    assert!(
        messages.iter().any(|m| m.contains("release") && m.contains("ARC")),
        "the ARC error below it must not be swallowed: {published}"
    );

    // The recovered AST still answers what an editor asks about the code.
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
    assert_eq!(names, vec!["main"], "the outline must survive a syntax error: {symbols}");

    stdin
        .write_all(frame(&json!({"jsonrpc": "2.0", "id": 9, "method": "shutdown"})).as_bytes())
        .expect("write shutdown");
    stdin.write_all(frame(&json!({"jsonrpc": "2.0", "method": "exit"})).as_bytes())
        .expect("write exit");
    stdin.flush().expect("flush");
    drop(stdin);
    assert_eq!(child.wait().expect("wait").code(), Some(0));
}

/// A project's own configuration file (`compile_flags.txt`) is found by the
/// server, with no editor setting and no command-line flag: the editor analyzes
/// the way the project builds.
#[test]
fn a_compile_flags_file_configures_the_analysis() {
    let root = std::env::temp_dir().join("jeti_lsp_session_config");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("project dirs");
    std::fs::write(root.join("compile_flags.txt"), "-fno-jeti-arc\n").expect("flags file");
    let path = root.join("src/legacy.jeti");
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
    std::fs::write(&path, mrc).expect("write source");
    let uri = format!("file://{}", path.display());
    let root_uri = format!("file://{}", root.display());

    let (mut child, mut stdin, mut stdout) = spawn(&[]);
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "rootUri": root_uri },
        })).as_bytes())
        .expect("write initialize");
    stdin.flush().expect("flush");
    read_frame(&mut stdout);

    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "jeti", "version": 1, "text": mrc,
            }},
        })).as_bytes())
        .expect("write didOpen");
    stdin.flush().expect("flush");
    let published = read_frame(&mut stdout);
    assert_eq!(
        published["params"]["diagnostics"],
        json!([]),
        "the project's compile_flags.txt must reach the analysis: {published}"
    );

    stdin
        .write_all(frame(&json!({"jsonrpc": "2.0", "id": 9, "method": "shutdown"})).as_bytes())
        .expect("write shutdown");
    stdin.write_all(frame(&json!({"jsonrpc": "2.0", "method": "exit"})).as_bytes())
        .expect("write exit");
    stdin.flush().expect("flush");
    drop(stdin);
    assert_eq!(child.wait().expect("wait").code(), Some(0));
    let _ = std::fs::remove_dir_all(&root);
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

    let include = repo_include_dir().to_string_lossy().to_string();
    let (mut child, mut stdin, mut stdout) = spawn(&["--include", &include]);

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
    assert_eq!(response["result"]["capabilities"]["textDocumentSync"], 2);
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

    // An incremental change: delete the `;` of `return 0;` (line 16), expect the
    // parse error there, then put it back with a zero-width insertion.
    let mut semicolon = position_of(PROGRAM, "return 0;");
    semicolon["character"] = json!(semicolon["character"].as_u64().unwrap() + 8);
    let mut after = semicolon.clone();
    after["character"] = json!(semicolon["character"].as_u64().unwrap() + 1);
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 3 },
                "contentChanges": [{ "range": { "start": semicolon, "end": after }, "text": "" }],
            },
        })).as_bytes())
        .expect("write didChange");
    stdin.flush().expect("flush");
    let published = read_frame(&mut stdout);
    let diagnostics = published["params"]["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(diagnostics.len(), 1, "{published}");
    assert_eq!(diagnostics[0]["range"]["start"]["line"], semicolon["line"]);

    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 4 },
                "contentChanges": [{ "range": { "start": semicolon, "end": semicolon }, "text": ";" }],
            },
        })).as_bytes())
        .expect("write didChange");
    stdin.flush().expect("flush");
    let published = read_frame(&mut stdout);
    assert_eq!(published["params"]["diagnostics"], json!([]));

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
