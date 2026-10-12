//! Sidecar source-map file (`.jeti.map`) — the versioned JSON schema of 阶段 4
//! (`doc/source_locations_debug_lsp_plan.md` lines 66-74).
//!
//! The file maps **generated C lines** back to **Jeti source lines**. Its
//! ground truth is the `#line` directive stream in the generated C, which is
//! exactly what the C preprocessor consumes, so the map can never disagree
//! with what clang diagnostics show. Within one mapping the source line
//! increments by one per generated C line (CPP `#line` semantics), so a
//! mapping covering N C lines starting at `src_line_start` spans source lines
//! `src_line_start ..= src_line_start + N - 1` (stored as `src_line_end`).
//!
//! Schema (version 1):
//!
//! ```json
//! {
//!   "version": 1,
//!   "generator": "jetic",
//!   "primary_source": "main.jeti",
//!   "generated": { "path": "main.c", "hash": "fnv1a64:…" },
//!   "sources": [ { "id": 0, "path": "main.jeti", "hash": "fnv1a64:…" } ],
//!   "mappings": [
//!     { "c_start": 120, "c_end": 480,
//!       "c_start_line": 9, "c_start_col": 1, "c_end_line": 11, "c_end_col": 14,
//!       "src_id": 0, "src_line_start": 3, "src_line_end": 5,
//!       "kind": "source", "synthetic": false }
//!   ]
//! }
//! ```
//!
//! - `sources[]` — every file named by a `#line` directive (a TU may inline
//!   many). `id` is this file's own stable identifier: sources are sorted by
//!   path so the same build always assigns the same ids.
//! - `generated.hash` / `sources[].hash` — FNV-1a 64 content hashes. A
//!   consumer compares the generated-C hash before trusting the map; a stale
//!   map next to a new `.c` is thereby detected (plan: build identity).
//!   `hash_algo` is spelled inside the value (`fnv1a64:<hex>`) so a future
//!   stronger hash can coexist.
//! - Synthetic regions (compiler-generated code attributed to no source
//!   file) omit `src_id` / `src_line_start` / `src_line_end` entirely and
//!   carry `"synthetic": true`, `"kind": "synthetic"`.
//! - Lines before the first `#line` directive are not mapped at all.
//! - `kind` names the generation category; version 1 only ever emits
//!   `"source"` / `"synthetic"` (later phases add ARC/EH/pattern/async kinds
//!   — the field is a plain string so consumers must treat unknown kinds as
//!   synthetic-ish and keep parsing).
//!
//! JSON is written by hand: `jeti-cst` is intentionally dependency-free and
//! this schema is write-only for the compiler (the reader lives in the
//! future LSP/debugger-adapter consumers, 阶段 6).
//!
//! Paths are stored exactly as emitted into the `#line` directives; on
//! Windows a backslash is JSON-escaped (`\\`), which is valid JSON and
//! round-trips.

use std::fmt::Write as _;

/// Bumped on any schema change; consumers must refuse unknown versions.
pub const SOURCE_MAP_VERSION: u64 = 1;

/// One `#include`d / `#import`ed source file named by the `#line` stream.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceEntry {
    pub id: u64,
    pub path: String,
    /// Content hash, spelled `fnv1a64:<16 hex>`; empty string = unknown
    /// (file could not be re-read at map-assembly time).
    pub hash: String,
}

/// Identity of the generated C artifact the mappings refer to.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedArtifact {
    pub path: String,
    pub hash: String,
}

/// One contiguous generated-C line range attributed to one source position.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    /// Byte offset of the region's first character in the generated C.
    pub c_start: u64,
    /// Byte offset one past the region's last character (end of the last
    /// mapped line, including its newline).
    pub c_end: u64,
    /// 1-based generated-C line/column where the region starts (always
    /// column 1: regions are line-aligned by construction).
    pub c_start_line: u32,
    pub c_start_col: u32,
    /// 1-based generated-C line of the region's last mapped line, and the
    /// column ONE PAST its last character (exclusive end, mirroring `c_end`;
    /// 1 for an empty last line).
    pub c_end_line: u32,
    pub c_end_col: u32,
    /// Index into `sources[]`, or `None` for synthetic regions.
    pub src_id: Option<u64>,
    /// 1-based source line the region starts at (the `#line` value).
    pub src_line_start: u32,
    /// 1-based source line the region ends at (start + C line count - 1).
    pub src_line_end: u32,
    /// Generation category; see the module docs for version-1 values.
    pub kind: String,
    pub synthetic: bool,
}

/// The complete sidecar document.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceMapFile {
    pub version: u64,
    pub generator: String,
    /// The main `.jeti` translation unit this build compiled.
    pub primary_source: String,
    pub generated: GeneratedArtifact,
    /// Sorted by `path` at serialization; `id` follows the sorted order.
    pub sources: Vec<SourceEntry>,
    /// Sorted by `c_start` at serialization (stable: equal starts keep order).
    pub mappings: Vec<Mapping>,
}

impl SourceMapFile {
    pub fn new(generator: &str, primary_source: &str, generated: GeneratedArtifact) -> Self {
        SourceMapFile {
            version: SOURCE_MAP_VERSION,
            generator: generator.to_string(),
            primary_source: primary_source.to_string(),
            generated,
            sources: Vec::new(),
            mappings: Vec::new(),
        }
    }

    /// Serialize to pretty JSON (2-space indent). Ordering is enforced here so
    /// the same build always produces byte-identical output.
    pub fn to_json(&self) -> String {
        let mut sources: Vec<&SourceEntry> = self.sources.iter().collect();
        sources.sort_by_key(|s| s.id);
        let mut mappings: Vec<&Mapping> = self.mappings.iter().collect();
        mappings.sort_by_key(|m| m.c_start);

        let mut out = String::new();
        let w = &mut out;
        let _ = writeln!(w, "{{");
        let _ = writeln!(w, "  \"version\": {},", self.version);
        let _ = writeln!(w, "  \"generator\": \"{}\",", json_escape(&self.generator));
        let _ = writeln!(
            w,
            "  \"primary_source\": \"{}\",",
            json_escape(&self.primary_source)
        );
        let _ = writeln!(w, "  \"generated\": {{");
        let _ = writeln!(
            w,
            "    \"path\": \"{}\",",
            json_escape(&self.generated.path)
        );
        let _ = writeln!(w, "    \"hash\": \"{}\"", json_escape(&self.generated.hash));
        let _ = writeln!(w, "  }},");
        if sources.is_empty() {
            let _ = writeln!(w, "  \"sources\": [],");
        } else {
            let _ = writeln!(w, "  \"sources\": [");
            for (i, s) in sources.iter().enumerate() {
                let comma = if i + 1 == sources.len() { "" } else { "," };
                let _ = writeln!(w, "    {{");
                let _ = writeln!(w, "      \"id\": {},", s.id);
                let _ = writeln!(w, "      \"path\": \"{}\",", json_escape(&s.path));
                let _ = writeln!(w, "      \"hash\": \"{}\"", json_escape(&s.hash));
                let _ = writeln!(w, "    }}{}", comma);
            }
            let _ = writeln!(w, "  ],");
        }
        if mappings.is_empty() {
            let _ = writeln!(w, "  \"mappings\": []");
        } else {
            let _ = writeln!(w, "  \"mappings\": [");
            for (i, m) in mappings.iter().enumerate() {
                let comma = if i + 1 == mappings.len() { "" } else { "," };
                let _ = writeln!(w, "    {{");
                let _ = writeln!(w, "      \"c_start\": {},", m.c_start);
                let _ = writeln!(w, "      \"c_end\": {},", m.c_end);
                let _ = writeln!(w, "      \"c_start_line\": {},", m.c_start_line);
                let _ = writeln!(w, "      \"c_start_col\": {},", m.c_start_col);
                let _ = writeln!(w, "      \"c_end_line\": {},", m.c_end_line);
                let _ = writeln!(w, "      \"c_end_col\": {},", m.c_end_col);
                if let Some(id) = m.src_id {
                    let _ = writeln!(w, "      \"src_id\": {},", id);
                    let _ = writeln!(w, "      \"src_line_start\": {},", m.src_line_start);
                    let _ = writeln!(w, "      \"src_line_end\": {},", m.src_line_end);
                }
                let _ = writeln!(w, "      \"kind\": \"{}\",", json_escape(&m.kind));
                let _ = writeln!(w, "      \"synthetic\": {}", m.synthetic);
                let _ = writeln!(w, "    }}{}", comma);
            }
            let _ = writeln!(w, "  ]");
        }
        let _ = writeln!(w, "}}");
        out
    }
}

/// FNV-1a 64-bit content hash. Not cryptographic — it only has to detect
/// *accidental* staleness (a map left next to a rebuilt artifact), which is
/// the plan's stated threat model for the sidecar's build identity.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hash formatted for the `hash` fields: `fnv1a64:<16 lowercase hex>`.
pub fn hash_tag(data: &[u8]) -> String {
    format!("fnv1a64:{:016x}", fnv1a64(data))
}

/// Escape a string for embedding inside a JSON string literal. Control
/// characters are emitted as `\u00XX`; everything else (including UTF-8
/// multibyte sequences) passes through raw, which is valid JSON.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash_tag(b"a"), "fnv1a64:af63dc4c8601ec8c");
    }

    #[test]
    fn json_escapes_quotes_and_control_chars() {
        assert_eq!(json_escape("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("l1\nl2\tend"), "l1\\nl2\\tend");
        assert_eq!(json_escape("\u{01}"), "\\u0001");
        assert_eq!(json_escape("héllo→"), "héllo→");
    }

    #[test]
    fn to_json_orders_and_omits_synthetic_source_fields() {
        let mut f = SourceMapFile::new(
            "jetic",
            "main.jeti",
            GeneratedArtifact {
                path: "main.c".into(),
                hash: hash_tag(b"C"),
            },
        );
        f.sources = vec![
            SourceEntry {
                id: 0,
                path: "main.jeti".into(),
                hash: String::new(),
            },
            SourceEntry {
                id: 1,
                path: "lib.hje".into(),
                hash: hash_tag(b"L"),
            },
        ];
        f.mappings = vec![
            Mapping {
                c_start: 500,
                c_end: 600,
                c_start_line: 40,
                c_start_col: 1,
                c_end_line: 45,
                c_end_col: 3,
                src_id: None,
                src_line_start: 0,
                src_line_end: 0,
                kind: "synthetic".into(),
                synthetic: true,
            },
            Mapping {
                c_start: 10,
                c_end: 499,
                c_start_line: 2,
                c_start_col: 1,
                c_end_line: 39,
                c_end_col: 42,
                src_id: Some(0),
                src_line_start: 3,
                src_line_end: 40,
                kind: "source".into(),
                synthetic: false,
            },
        ];
        let json = f.to_json();
        // Mappings serialized in c_start order even though insertion differed.
        assert!(json.find("\"c_start\": 10,").unwrap() < json.find("\"c_start\": 500,").unwrap());
        // Synthetic mapping carries no src_* fields.
        let syn = &json[json.find("500,").unwrap()..];
        assert!(!syn.contains("src_id"));
        assert!(syn.contains("\"kind\": \"synthetic\""));
        // Source mapping keeps its span.
        assert!(json.contains("\"src_line_start\": 3,"));
        assert!(json.contains("\"src_line_end\": 40,"));
        // Document closes and parses as balanced braces.
        assert!(json.trim_end().ends_with('}'));
    }
}
