//! End-to-end: drive the real `jeti-lsp` binary over stdio the way an editor
//! does — initialize, open a buffer with a syntax error, expect
//! `publishDiagnostics`, then shut down and exit.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn frame(message: &Value) -> String {
    let body = serde_json::to_string(message).unwrap();
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

/// Read one `Content-Length`-framed JSON message from the child's stdout.
fn read_frame(reader: &mut BufReader<std::process::ChildStdout>) -> Value {
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

#[test]
fn server_publishes_diagnostics_for_an_open_buffer() {
    let dir = std::env::temp_dir().join("jeti_lsp_session_test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("session.jeti");
    std::fs::write(&path, "int main(void) { return 0 }\n").expect("write probe file");
    let uri = format!("file://{}", path.display());

    let mut child = Command::new(env!("CARGO_BIN_EXE_jeti-lsp"))
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

    // A clean edit clears the diagnostics on the same buffer.
    stdin
        .write_all(frame(&json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": "int main(void) { return 0; }\n" }],
            },
        })).as_bytes())
        .expect("write didChange");
    stdin.flush().expect("flush");
    let cleared = read_frame(&mut stdout);
    assert_eq!(cleared["params"]["diagnostics"], json!([]));

    // shutdown → response, exit → process ends with status 0
    stdin
        .write_all(frame(&json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"})).as_bytes())
        .expect("write shutdown");
    stdin.flush().expect("flush");
    let shutdown = read_frame(&mut stdout);
    assert_eq!(shutdown["id"], 2);
    assert!(shutdown["result"].is_null());

    stdin
        .write_all(frame(&json!({"jsonrpc": "2.0", "method": "exit"})).as_bytes())
        .expect("write exit");
    drop(stdin);

    let status = child.wait().expect("wait for jeti-lsp");
    assert_eq!(status.code(), Some(0), "clean shutdown exits 0");
}
