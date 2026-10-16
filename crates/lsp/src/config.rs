//! Project configuration files, read the way clangd reads them.
//!
//! - **`compile_commands.json`** — the compilation database: an array of
//!   `{directory, file, arguments | command}`. The entry whose `file` is the
//!   document wins; otherwise the entry whose `directory` is the closest
//!   ancestor of the document (clangd's inference), so a project with one entry
//!   still configures every file.
//! - **`compile_flags.txt`** — one flag per line, applied to every document.
//!   The simpler file, for projects whose flags do not vary per file.
//!
//! Both are searched from the document's directory upwards, stopping at the
//! workspace root the client gave us (so a nested project does not pick up its
//! parent's file by accident). `compile_commands.json` wins in a directory that
//! has both.
//!
//! Flags are read with [`crate::apply_flags`] — the same parser as the server's
//! own command line — so `-fno-jeti-arc` means one thing everywhere. A database
//! entry is a full command line (`jetic -I include -fno-jeti-arc src/a.jeti`),
//! so a single parser covers it, and relative `-I` paths resolve against the
//! entry's own `directory`, as the format requires.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::{apply_flags, AnalysisOptions};

/// The flags a project's configuration asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFlags {
    pub search_dirs: Vec<String>,
    pub no_arc: bool,
    pub no_checker: bool,
    /// `Some(true)` = `-eh checked`, `Some(false)` = a legacy backend.
    pub eh_checked: Option<bool>,
}

impl ConfigFlags {
    /// Fold these flags into the options an analysis will use.
    ///
    /// A project file *refines* the server's setup: directories add, a switch
    /// that turns something off stays off, and an explicit exception backend
    /// replaces the server's choice (the file is the more specific statement).
    pub fn merge_into(&self, options: &mut AnalysisOptions) {
        options.search_dirs.extend(self.search_dirs.iter().cloned());
        options.no_arc |= self.no_arc;
        options.no_checker |= self.no_checker;
        if let Some(eh_checked) = self.eh_checked {
            options.eh_checked = eh_checked;
        }
    }
}

/// One `compile_commands.json` entry, with paths already resolved.
#[derive(Debug, Clone)]
struct DbEntry {
    directory: PathBuf,
    file: PathBuf,
    arguments: Vec<String>,
}

/// Parsed configuration, kept between analyses.
///
/// A real compilation database can be megabytes, and an analysis runs on every
/// keystroke, so parsed databases are cached per file and only re-read when the
/// file's modification time changes. `compile_flags.txt` is small enough to read
/// each time.
#[derive(Default)]
pub struct ConfigCache {
    databases: HashMap<PathBuf, (SystemTime, Vec<DbEntry>)>,
}

impl ConfigCache {
    /// The flags that apply to `document`, from the nearest configuration file,
    /// or `None` when the project has none.
    pub fn flags_for(&mut self, document: &str, root: Option<&str>) -> Option<ConfigFlags> {
        let document = canonical(document);
        let start = document.parent()?;
        let root = root.map(canonical);
        for directory in ancestors(start, root.as_deref()) {
            let database = directory.join("compile_commands.json");
            if let Some(entries) = self.database(&database) {
                // A database is authoritative for the tree it was found in: if
                // it has no entry for this file, the closest entry's flags are
                // used (clangd's inference) rather than falling through to
                // another directory's file.
                if let Some(entry) = closest_entry(&entries, &document) {
                    let mut options = AnalysisOptions::default();
                    options.search_dirs.clear();
                    apply_flags(
                        &entry.arguments,
                        Some(&entry.directory),
                        Some(&entry.file.to_string_lossy()),
                        &mut options,
                    );
                    return Some(ConfigFlags {
                        search_dirs: options.search_dirs,
                        no_arc: options.no_arc,
                        no_checker: options.no_checker,
                        eh_checked: Some(options.eh_checked),
                    });
                }
            }
            let flags_file = directory.join("compile_flags.txt");
            if let Some(flags) = self.flags_file(&flags_file) {
                return Some(flags);
            }
        }
        None
    }

    /// The parsed database at `path`, re-read when it changes.
    fn database(&mut self, path: &Path) -> Option<Vec<DbEntry>> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        if let Some((cached_at, entries)) = self.databases.get(path) {
            if *cached_at == modified {
                return Some(entries.clone());
            }
        }
        let text = std::fs::read_to_string(path).ok()?;
        let entries = parse_database(&text)?;
        self.databases.insert(path.to_path_buf(), (modified, entries.clone()));
        Some(entries)
    }

    /// Read a `compile_flags.txt`: one flag per line, the whole line being one
    /// argument (clangd's rule — `-I src` on one line is a single argument, which
    /// is why the joined form is trimmed).
    ///
    /// `#` starts a comment. That is a jetic extension: clangd reads the same
    /// file and passes every line to the compiler, so a comment line would make
    /// *its* command unusable — a directory shared with clangd should keep
    /// comments out of this file.
    fn flags_file(&self, path: &Path) -> Option<ConfigFlags> {
        let text = std::fs::read_to_string(path).ok()?;
        let arguments: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect();
        // Nothing but comments and blank lines: the project has said nothing.
        if arguments.is_empty() {
            return None;
        }
        // A synthetic argv: the first entry is the "compiler", which
        // `apply_flags` skips.
        let mut argv = vec!["jetic".to_string()];
        argv.extend(arguments);
        let mut options = AnalysisOptions::default();
        options.search_dirs.clear();
        apply_flags(&argv, path.parent(), None, &mut options);
        Some(ConfigFlags {
            search_dirs: options.search_dirs,
            no_arc: options.no_arc,
            no_checker: options.no_checker,
            eh_checked: Some(options.eh_checked),
        })
    }
}

/// The directories from `start` up to `root` (inclusive), or to the filesystem
/// root when the client gave no root.
fn ancestors(start: &Path, root: Option<&Path>) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut current = Some(start.to_path_buf());
    while let Some(directory) = current {
        directories.push(directory.clone());
        if root.is_some_and(|root| root == directory) {
            break;
        }
        current = directory.parent().map(Path::to_path_buf);
    }
    directories
}

/// The entry to use for `document`: its own, or the closest ancestor directory
/// that has one.
///
/// When a directory holds several entries, the *first* of the deepest wins — a
/// deterministic choice, and the same fallback clangd documents ("the first
/// entry"), which matters because a project's first entry usually carries the
/// project-wide flags.
fn closest_entry(entries: &[DbEntry], document: &Path) -> Option<DbEntry> {
    if let Some(entry) = entries.iter().find(|entry| entry.file == document) {
        return Some(entry.clone());
    }
    let mut best: Option<&DbEntry> = None;
    let mut best_depth = 0;
    for entry in entries.iter().filter(|entry| document.starts_with(&entry.directory)) {
        let depth = entry.directory.components().count();
        if best.is_none() || depth > best_depth {
            best = Some(entry);
            best_depth = depth;
        }
    }
    best.cloned()
}

/// Parse a compilation database. `None` when the file is not a JSON array of
/// entries (a hand-written file with the wrong shape is reported as absent
/// rather than half-applied).
fn parse_database(text: &str) -> Option<Vec<DbEntry>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let entries = value.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                let file = entry.get("file").and_then(Value::as_str)?;
                let directory = entry
                    .get("directory")
                    .and_then(Value::as_str)
                    .map(canonical)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let arguments: Vec<String> = match entry.get("arguments") {
                    Some(Value::Array(items)) => {
                        items.iter().filter_map(Value::as_str).map(str::to_string).collect()
                    }
                    // Either `command` is the spelling used, or `arguments` is
                    // absent: `command` is a shell line, and splitting on
                    // whitespace is what clangd does (quoting is rare enough in
                    // a Jeti build to not warrant a shell parser).
                    _ => match entry.get("command").and_then(Value::as_str) {
                        Some(command) => command.split_whitespace().map(str::to_string).collect(),
                        None => return None,
                    },
                };
                // A relative `file` is relative to `directory`, like a relative
                // `-I` path.
                let path = Path::new(file);
                Some(DbEntry {
                    file: if path.is_absolute() { path.to_path_buf() } else { directory.join(path) },
                    directory,
                    arguments,
                })
            })
            .collect(),
    )
}

/// Canonicalize as much of `path` as exists, so a file that is opened before it
/// is saved still resolves like the directory it will live in (`/tmp` is
/// `/private/tmp` on macOS, and a database's `directory` is canonical).
fn canonical(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent().map(Path::to_path_buf) {
        if let Some(name) = current.file_name() {
            missing.push(PathBuf::from(name));
        }
        if let Ok(canonical) = parent.canonicalize() {
            let mut rebuilt = canonical;
            for name in missing.iter().rev() {
                rebuilt.push(name);
            }
            return rebuilt;
        }
        current = parent;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dir");
        }
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn a_flags_file_applies_to_every_document_below_it() {
        let root = std::env::temp_dir().join("jeti_lsp_config_flags");
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("compile_flags.txt"), "-I include\n-fno-jeti-arc\n\n-g\n");
        write(&root.join("src/deep/a.jeti"), "");

        let mut cache = ConfigCache::default();
        let document = root.join("src/deep/a.jeti").to_string_lossy().to_string();
        let flags = cache.flags_for(&document, None).expect("flags");
        assert!(flags.no_arc);
        assert!(!flags.no_checker);
        // `-g` is not a diagnostic switch: ignored, not treated as a directory.
        assert_eq!(flags.search_dirs.len(), 1);
        assert!(flags.search_dirs[0].ends_with("include"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn comments_let_a_flag_sit_there_until_it_is_wanted() {
        let root = std::env::temp_dir().join("jeti_lsp_config_comments");
        let _ = std::fs::remove_dir_all(&root);
        write(
            &root.join("compile_flags.txt"),
            "# kept here until the project moves to MRC\n# -fno-jeti-arc\n",
        );
        write(&root.join("src/a.jeti"), "");

        let mut cache = ConfigCache::default();
        let document = root.join("src/a.jeti").to_string_lossy().to_string();
        // Nothing but comments: the project has said nothing, so the editor's
        // own settings stand.
        assert!(cache.flags_for(&document, None).is_none());

        // Uncommenting is all it takes.
        write(&root.join("compile_flags.txt"), "# -fno-jeti-arc\n-fno-jeti-arc\n");
        let flags = cache.flags_for(&document, None).expect("flags");
        assert!(flags.no_arc);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_joined_include_path_is_trimmed() {
        let root = std::env::temp_dir().join("jeti_lsp_config_joined");
        let _ = std::fs::remove_dir_all(&root);
        // One argument per line, so a space after `-I` is part of the same line.
        write(&root.join("compile_flags.txt"), "-I include\n-Isrc\n");
        write(&root.join("src/a.jeti"), "");

        let mut cache = ConfigCache::default();
        let flags = cache
            .flags_for(&root.join("src/a.jeti").to_string_lossy(), None)
            .expect("flags");
        assert_eq!(flags.search_dirs.len(), 2, "{:?}", flags.search_dirs);
        assert!(flags.search_dirs[0].ends_with("include"), "{:?}", flags.search_dirs);
        assert!(flags.search_dirs[1].ends_with("src"), "{:?}", flags.search_dirs);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_database_entry_wins_for_its_own_file() {
        let root = std::env::temp_dir().join("jeti_lsp_config_db");
        let _ = std::fs::remove_dir_all(&root);
        let database = format!(
            r#"[
              {{ "directory": "{root}", "file": "src/a.jeti",
                 "arguments": ["jetic", "-I", "include", "-fno-jeti-arc", "src/a.jeti", "-o", "a"] }},
              {{ "directory": "{root}", "file": "src/b.jeti",
                 "command": "jetic -I other -eh legacy src/b.jeti" }}
            ]"#,
            root = root.display()
        );
        write(&root.join("compile_commands.json"), &database);
        write(&root.join("src/a.jeti"), "");
        write(&root.join("src/b.jeti"), "");

        let mut cache = ConfigCache::default();
        let a = cache
            .flags_for(&root.join("src/a.jeti").to_string_lossy(), None)
            .expect("a's flags");
        assert!(a.no_arc);
        assert_eq!(a.search_dirs.len(), 1);
        assert!(a.search_dirs[0].ends_with("include"), "{:?}", a.search_dirs);
        assert_eq!(a.eh_checked, Some(true), "no -eh means the default backend");

        let b = cache
            .flags_for(&root.join("src/b.jeti").to_string_lossy(), None)
            .expect("b's flags");
        assert!(!b.no_arc);
        assert_eq!(b.eh_checked, Some(false), "-eh legacy");
        assert!(b.search_dirs[0].ends_with("other"), "{:?}", b.search_dirs);

        // A file the database does not name uses its closest entry's flags --
        // and `-o`'s value is never mistaken for a directory.
        let c = cache
            .flags_for(&root.join("src/new.jeti").to_string_lossy(), None)
            .expect("inferred flags");
        assert!(c.no_arc);
        assert_eq!(c.search_dirs.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_search_stops_at_the_workspace_root() {
        let root = std::env::temp_dir().join("jeti_lsp_config_root");
        let _ = std::fs::remove_dir_all(&root);
        // Outside the workspace: the flags file above the root must not apply.
        write(&root.join("compile_flags.txt"), "-fno-jeti-arc\n");
        write(&root.join("project/src/a.jeti"), "");

        let mut cache = ConfigCache::default();
        let document = root.join("project/src/a.jeti").to_string_lossy().to_string();
        let workspace = root.join("project");
        assert!(cache.flags_for(&document, Some(&workspace.to_string_lossy())).is_none());
        assert!(cache.flags_for(&document, None).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_database_is_treated_as_absent() {
        assert!(parse_database("{ not json").is_none());
        assert!(parse_database("{}").is_none());
        assert!(parse_database("[]").unwrap().is_empty());
    }
}
