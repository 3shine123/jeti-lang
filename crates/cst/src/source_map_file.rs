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
//! JSON is written and read through `serde` (the reader is what the 阶段 6
//! LSP/debugger-adapter consumers use; the writer stays the compiler's). The
//! on-disk schema above is unchanged: `src_id` / `src_line_start` /
//! `src_line_end` are omitted for synthetic regions exactly as documented.
//!
//! Paths are stored exactly as emitted into the `#line` directives; on
//! Windows a backslash is JSON-escaped (`\\`), which is valid JSON and
//! round-trips.

use serde::{Deserialize, Serialize};

/// Bumped on any schema change; consumers must refuse unknown versions.
pub const SOURCE_MAP_VERSION: u64 = 1;

/// One `#include`d / `#import`ed source file named by the `#line` stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEntry {
    pub id: u64,
    pub path: String,
    /// Content hash, spelled `fnv1a64:<16 hex>`; empty string = unknown
    /// (file could not be re-read at map-assembly time).
    pub hash: String,
}

/// Identity of the generated C artifact the mappings refer to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratedArtifact {
    pub path: String,
    pub hash: String,
}

/// One contiguous generated-C line range attributed to one source position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Index into `sources[]`; `None` for synthetic regions (field omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_id: Option<u64>,
    /// 1-based source line the region starts at (the `#line` value); `None`
    /// for synthetic regions (field omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_line_start: Option<u32>,
    /// 1-based source line the region ends at (start + C line count - 1);
    /// `None` for synthetic regions (field omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_line_end: Option<u32>,
    /// Generation category; see the module docs for version-1 values.
    pub kind: String,
    pub synthetic: bool,
}

/// The complete sidecar document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        let mut doc = self.clone();
        doc.sources.sort_by_key(|s| s.id);
        doc.mappings.sort_by_key(|m| m.c_start);
        serde_json::to_string_pretty(&doc).expect("SourceMapFile always serializes")
    }

    /// Parse a sidecar document. An unknown `version` is refused so a consumer
    /// never trusts a schema it does not understand (阶段 6 LSP/debugger).
    pub fn from_json(text: &str) -> Result<SourceMapFile, String> {
        let doc: SourceMapFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if doc.version != SOURCE_MAP_VERSION {
            return Err(format!(
                "unsupported source-map version {} (expected {})",
                doc.version, SOURCE_MAP_VERSION
            ));
        }
        Ok(doc)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash_tag(b"a"), "fnv1a64:af63dc4c8601ec8c");
    }

    /// Paths and hashes with quotes / backslashes / newlines survive a
    /// serialize → parse round trip (serde owns the escaping now).
    #[test]
    fn json_round_trips_awkward_paths() {
        let mut f = SourceMapFile::new(
            "jetic",
            "dir with \"quotes\"/main.jeti",
            GeneratedArtifact {
                path: "a\\b\\main.c".into(),
                hash: hash_tag(b"C"),
            },
        );
        f.sources = vec![SourceEntry {
            id: 0,
            path: "weird\tname\n.jeti".into(),
            hash: String::new(),
        }];
        let json = f.to_json();
        assert!(json.contains("\\t") && json.contains("\\n"));
        let back = SourceMapFile::from_json(&json).expect("round trip");
        assert_eq!(back, f);
        assert_eq!(back.primary_source, "dir with \"quotes\"/main.jeti");
        assert_eq!(back.generated.path, "a\\b\\main.c");
    }

    #[test]
    fn from_json_refuses_unknown_version() {
        let f = SourceMapFile::new(
            "jetic",
            "main.jeti",
            GeneratedArtifact { path: "main.c".into(), hash: String::new() },
        );
        let json = f.to_json().replace("\"version\": 1", "\"version\": 99");
        let err = SourceMapFile::from_json(&json).unwrap_err();
        assert!(err.contains("unsupported source-map version 99"), "got: {err}");
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
                src_line_start: None,
                src_line_end: None,
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
                src_line_start: Some(3),
                src_line_end: Some(40),
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
        // Document closes and parses back; serialization normalized the
        // mapping order, so the parsed document is the sorted form.
        assert!(json.trim_end().ends_with('}'));
        let back = SourceMapFile::from_json(&json).unwrap();
        assert_eq!(back.sources, f.sources);
        assert_eq!(back.mappings.len(), 2);
        assert_eq!((back.mappings[0].c_start, back.mappings[1].c_start), (10, 500));
        assert_eq!(back.mappings[0].src_line_start, Some(3));
        assert_eq!(back.mappings[0].src_line_end, Some(40));
        assert!(back.mappings[1].synthetic);
        assert_eq!(back.mappings[1].src_id, None);
    }
}
