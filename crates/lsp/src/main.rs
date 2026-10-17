//! `jeti-lsp`: Jeti's language server over stdio.
//!
//! ```text
//! jeti-lsp [--include DIR]... [-fno-jeti-arc] [-fno-checker] [-eh checked|legacy]
//!          [--debounce-ms N]
//! ```
//!
//! The switches are the CLI's own, and only the ones that change *diagnostics*
//! are accepted — the editor must report what the user's build accepts. A
//! project compiled with `-fno-jeti-arc` (MRC) passes that here too, and stops
//! seeing "explicit 'release' not allowed in ARC mode" for code it builds.
//! `--debounce-ms` is the server's own: how long an edited buffer may stay
//! unanalyzed after its last edit (default 150); zero re-analyzes on every
//! message, which is how the tests stay free of real-time dependence.
//!
//! Protocol state and diagnostics live in the library so they are testable
//! without a subprocess; this file is only argument handling and the stdio loop.

use std::io::{self, BufReader, BufWriter};
use std::time::Duration;

use jeti_lsp::{apply_flags, read_message, write_message, AnalysisOptions, Server};

fn main() {
    // The very same parser a project's `compile_commands.json` goes through, so
    // a flag cannot mean one thing on the command line and another in the file.
    // Editors pass flags such as `--stdio`; unknown ones are ignored rather than
    // refusing to start.
    let args: Vec<String> = std::env::args().collect();
    let mut options = AnalysisOptions::default();
    apply_flags(&args, None, None, &mut options);
    let quiet = args
        .windows(2)
        .find(|pair| pair[0] == "--debounce-ms")
        .and_then(|pair| pair[1].parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(150));

    let mut server = Server::new(options).with_quiet_period(quiet);
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
