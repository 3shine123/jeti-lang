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
//! The quiet period is fired by the clock, not by the next message: stdin is
//! polled against the nearest re-analysis deadline, so the last edit of a
//! burst is analyzed even when the client goes silent after it — blocking
//! indefinitely there would leave the editor showing the previous edit's
//! diagnostics forever (old error never cleared, new error never reported).
//!
//! Protocol state and diagnostics live in the library so they are testable
//! without a subprocess; this file is only argument handling and the stdio loop.

use std::io::{self, BufReader, BufWriter};
use std::time::Duration;

use jeti_lsp::{apply_flags, read_message, write_message, AnalysisOptions, Server};

/// Is stdin readable now, or within `timeout`? `None` waits forever.
/// `false` is a bare timeout — the moment the loop uses to run the
/// due re-analyses itself.
#[cfg(unix)]
mod wait {
    use std::time::Duration;

    #[repr(C)]
    struct PollFd {
        fd: std::os::unix::io::RawFd,
        events: i16,
        revents: i16,
    }

    const POLLIN: i16 = 0x0001;
    const EINTR: i32 = 4; // BSD and Linux agree on this one

    extern "C" {
        fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
    }

    pub fn ready(timeout: Option<Duration>) -> bool {
        let mut fd = PollFd { fd: 0, events: POLLIN, revents: 0 };
        let millis = match timeout {
            None => -1,
            Some(d) => i32::try_from(d.as_millis()).unwrap_or(i32::MAX),
        };
        loop {
            let n = unsafe { poll(&mut fd, 1, millis) };
            if n > 0 {
                // EOF also reports readable (the read that follows returns
                // zero); an invalid fd must not be waited on again either.
                return true;
            }
            if n == 0 {
                return false;
            }
            if std::io::Error::last_os_error().raw_os_error() == Some(EINTR) {
                continue; // interrupted, not a result
            }
            return true; // abnormal: let the read surface it
        }
    }
}

/// The Windows twin of the unix poll: `WSAPoll` on the stdin handle.
#[cfg(windows)]
mod wait {
    use std::time::Duration;

    const STD_INPUT_HANDLE: i32 = -10;
    const FD_READ: i16 = 0x0001;
    const WSAEINTR: i32 = 10036;

    #[repr(C)]
    struct WsaFd {
        socket: u32,
        events: i16,
        revents: i16,
    }

    #[link(name = "ws2_32")]
    extern "system" {
        fn GetStdHandle(nStdHandle: i32) -> *mut std::ffi::c_void;
        fn WSAPoll(fds: *mut WsaFd, nfds: u32, timeout: i32) -> i32;
        fn WSAGetLastError() -> i32;
    }

    pub fn ready(timeout: Option<Duration>) -> bool {
        let socket = unsafe { GetStdHandle(STD_INPUT_HANDLE) as u32 };
        let mut fd = WsaFd { socket, events: FD_READ, revents: 0 };
        let millis = match timeout {
            None => -1,
            Some(d) => i32::try_from(d.as_millis()).unwrap_or(i32::MAX),
        };
        loop {
            let n = unsafe { WSAPoll(&mut fd, 1, millis) };
            if n > 0 {
                return true;
            }
            if n == 0 {
                return false;
            }
            if unsafe { WSAGetLastError() } == WSAEINTR {
                continue; // interrupted, not a result
            }
            return true; // abnormal: let the read surface it
        }
    }
}

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
        // Bytes the last `read_message` already pulled into the reader are
        // invisible to `poll`: the OS pipe can look empty while a request
        // sits inside the process. Waiting on the fd alone would strand that
        // request forever — the editor waits for an answer that already
        // arrived — so buffered input counts as ready.
        let buffered = !reader.buffer().is_empty();
        if !buffered && !wait::ready(server.next_flush_in()) {
            for outgoing in server.flush_due() {
                if write_message(&mut writer, &outgoing).is_err() {
                    return;
                }
            }
            continue;
        }
        // A request is at hand. Settle a due edit before answering it, so
        // the response reflects the buffer the editor actually holds.
        for outgoing in server.flush_due() {
            if write_message(&mut writer, &outgoing).is_err() {
                return;
            }
        }
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
