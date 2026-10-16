//! `jeti-lsp`: Jeti's language server over stdio.
//!
//! ```text
//! jeti-lsp [--include DIR]...        # DIRs resolve `#import` (the document's
//!                                    # own directory always comes first)
//! ```
//!
//! Protocol state and diagnostics live in the library so they are testable
//! without a subprocess; this file is only the stdio loop.

use std::io::{self, BufReader, BufWriter};

use jeti_lsp::{read_message, write_message, Server};

fn main() {
    let mut search_dirs: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--include" | "-I" => {
                if let Some(dir) = args.next() {
                    search_dirs.push(dir);
                }
            }
            // Editors pass flags such as `--stdio`; ignore what we do not use
            // rather than refusing to start.
            other if other.starts_with('-') => {}
            other => search_dirs.push(other.to_string()),
        }
    }

    let mut server = Server::new(search_dirs);
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    loop {
        let body = match read_message(&mut reader) {
            Ok(Some(body)) => body,
            // EOF or a truncated frame: the client went away.
            Ok(None) | Err(_) => break,
        };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue; // malformed JSON: not answerable, keep the session alive
        };
        let is_exit = message.get("method").and_then(|m| m.as_str()) == Some("exit");
        for outgoing in server.handle(&message) {
            if write_message(&mut writer, &outgoing).is_err() {
                return;
            }
        }
        if is_exit {
            // LSP: 0 when the client asked for shutdown first, 1 otherwise.
            std::process::exit(if server.shutdown_received() { 0 } else { 1 });
        }
    }
    // The pipe just closed without an `exit` notification.
    std::process::exit(1);
}
