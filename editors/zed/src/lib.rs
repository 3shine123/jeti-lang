use zed_extension_api as zed;

struct JetiExtension;

/// Resolve the `jeti-lsp` binary for the workspace, mirroring the VS Code
/// extension: the workspace's own `target/debug` / `target/release` builds
/// win, then `$PATH`.
///
/// Two sandbox rules shape this function:
///
/// * `std::fs` is unusable — the WIT surface has no `stat`, and
///   `Path::exists()` silently returns `false` for a file that does exist on
///   the host. That made an earlier version fall back to a bare `jeti-lsp`,
///   which Zed resolved inside its own extension work directory
///   (`extensions/work/jeti/jeti-lsp`), where nothing is installed.
/// * `Worktree::read_text_file` accepts only paths *relative to the worktree
///   root* — an absolute path raises `absolute path not allowed`. So the
///   probes below are relative, and the absolute command path is built from
///   `root_path()` afterwards.
///
/// The probe target is the empty `.cargo-lock` cargo writes into every
/// profile directory (text, so readable); `Cargo.toml` is the fallback for a
/// worktree whose ignore rules hide `target/`, pointing at the standard
/// `cargo build -p jeti-lsp` output.
fn resolve_server(worktree: &zed::Worktree) -> String {
    let root = worktree.root_path();
    for profile in ["debug", "release"] {
        if worktree
            .read_text_file(&format!("target/{profile}/.cargo-lock"))
            .is_ok()
        {
            return format!("{root}/target/{profile}/jeti-lsp");
        }
    }
    if worktree.read_text_file("Cargo.toml").is_ok() {
        return format!("{root}/target/debug/jeti-lsp");
    }
    // A globally installed server, or Zed's own work-directory convention
    // (which at least reports the missing binary accurately).
    worktree
        .which("jeti-lsp")
        .unwrap_or_else(|| "jeti-lsp".to_string())
}

impl zed::Extension for JetiExtension {
    fn new() -> Self {
        JetiExtension
    }

    /// Launch `jeti-lsp` with the workspace's include paths, matching the VS
    /// Code client's defaults (`include`, then the workspace root). The server
    /// also reads `compile_flags.txt` itself, so per-project flags need no
    /// editor-specific wiring.
    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command, String> {
        let root = worktree.root_path();
        Ok(zed::Command {
            command: resolve_server(worktree),
            args: vec![
                "--include".into(),
                format!("{root}/include"),
                "--include".into(),
                root,
            ],
            env: Default::default(),
        })
    }
}

zed::register_extension!(JetiExtension);
