//! Source location model — the single source of truth for file identity and
//! position mapping (docs/plans/source_locations_debug_lsp_plan.md, 阶段 1).
//!
//! Every file read or imported gets a stable [`SourceId`]. The flattened
//! post-preprocessing buffer (what the lexer/parser see) maps *line ranges*
//! back to (file, line range) instead of the old per-line string table, and
//! regions with no real source (macro expansion output, generated text) are
//! marked [`SpanOrigin::Synthetic`].
//!
//! The legacy API ([`SourceMap::locate`] etc.) is kept for the diagnostic path;
//! consumers migrate to it incrementally. New nodes must not store bare
//! (line, col) without a file identity.

/// Identity of one source file within a compilation.
/// Indices into [`SourceRegistry::files`].
pub type SourceId = u32;

/// A registered source file: normalized path, display path, and line-start
/// byte offsets (for line <-> byte-offset conversion).
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Path as used for dedup / import resolution (what the line table stored).
    pub path: String,
    /// Byte offset of the start of each line (entry 0 is always 0).
    pub line_starts: Vec<usize>,
    /// Total byte length of the source text.
    pub len: usize,
}

impl SourceFile {
    /// Convert a 1-based line number to a byte offset (clamped to the file).
    pub fn line_to_offset(&self, line: u32) -> Option<usize> {
        if line == 0 {
            return None;
        }
        self.line_starts.get(line as usize - 1).copied()
    }

    /// Convert a byte offset to a 1-based line number.
    pub fn offset_to_line(&self, offset: usize) -> u32 {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i as u32 + 1,
            Err(i) => i as u32, // offset is inside line i (1-based)
        }
    }

    /// Convert a byte offset to a 1-based column (UTF-8 bytes).
    /// Requires `text` (the file content); kept separate so the registry stays
    /// lightweight when only line mapping is needed.
    pub fn offset_to_col(text: &str, offset: usize) -> u32 {
        if offset >= text.len() {
            return 1;
        }
        let line_start = text[..offset]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        // Column in bytes +1, matching the existing parser convention.
        (offset - line_start + 1) as u32
    }

    /// Convert a 1-based `(line, column)` — column counted in **UTF-8 bytes**,
    /// the internal convention — to a byte offset, clamped to the file length.
    /// `None` when the line does not exist.
    ///
    /// This needs only the registry's line-start table, so byte offsets are
    /// available on the legacy path too (plan §31: offsets are the precise
    /// identity; rows/columns are for display).
    pub fn line_col_to_offset(&self, line: u32, col: u32) -> Option<usize> {
        let base = self.line_to_offset(line)?;
        Some((base + col.saturating_sub(1) as usize).min(self.len))
    }

    /// Convert a byte offset to a 1-based `(line, column-in-UTF-8-bytes)`.
    pub fn offset_to_line_col(&self, offset: usize) -> (u32, u32) {
        let line = self.offset_to_line(offset);
        let base = self.line_to_offset(line).unwrap_or(0);
        (line, offset.saturating_sub(base) as u32 + 1)
    }

    /// Byte offset → LSP position (**0-based line, 0-based UTF-16 column**).
    ///
    /// LSP counts columns in UTF-16 code units, so non-ASCII text differs from
    /// the byte column: an emoji is one Unicode scalar but two UTF-16 units,
    /// and each CJK character is one unit in three UTF-8 bytes. `text` must be
    /// the file's content (plan §32: the unit conversion lives at the LSP
    /// boundary — never mix the two systems).
    pub fn offset_to_lsp(text: &str, offset: usize) -> LspPosition {
        let offset = offset.min(text.len());
        let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = text[..line_start].bytes().filter(|&b| b == b'\n').count() as u32;
        let character = text[line_start..offset].encode_utf16().count() as u32;
        LspPosition { line, character }
    }

    /// LSP position (**0-based line, 0-based UTF-16 column**) → byte offset.
    ///
    /// `None` when the line does not exist or the column runs past the line's
    /// end (LSP positions must stay within the line). A column that lands inside
    /// a surrogate pair advances to the next scalar boundary, so the result is
    /// always a valid UTF-8 offset (best effort — editors do not produce such
    /// positions).
    pub fn lsp_to_offset(text: &str, pos: LspPosition) -> Option<usize> {
        let line_start = line_start_of(text, pos.line)?;
        let line_end = text[line_start..]
            .find('\n')
            .map(|i| line_start + i)
            .unwrap_or(text.len());
        let mut units = 0u32;
        for (i, ch) in text[line_start..line_end].char_indices() {
            if units >= pos.character {
                return Some(line_start + i);
            }
            units += ch.len_utf16() as u32;
        }
        if units == pos.character { Some(line_end) } else { None }
    }

    /// The LSP range for `span` in this file's text, projected from the
    /// precomputed line table: a binary-searched line plus a single line's
    /// UTF-16 count, never a whole-file scan. Behavior matches
    /// [`SourceSpan::to_lsp_range`] exactly; the table and `text` must agree
    /// (the caller checks [`Self::len`]).
    pub fn to_lsp_range(&self, span: &SourceSpan, text: &str) -> LspRange {
        let to_pos = |line: u32, col: u32, byte: Option<usize>| {
            // A byte offset is the precise identity. A line/byte-column pair
            // (legacy spans) is clamped to the line's own '\n' — the last
            // position the raw-text path would reach on that line.
            let byte = match byte {
                Some(b) => b,
                None => {
                    let base = self.line_to_offset(line).unwrap_or(0);
                    let end = self
                        .line_starts
                        .get(line as usize)
                        .map(|&next| next.saturating_sub(1))
                        .unwrap_or(text.len());
                    (base + col.saturating_sub(1) as usize).min(end)
                }
            };
            let offset = byte.min(text.len());
            let line = self.offset_to_line(offset);
            let line_start = self.line_to_offset(line).unwrap_or(0);
            let character = text[line_start..offset].encode_utf16().count() as u32;
            LspPosition { line: line.saturating_sub(1), character }
        };
        LspRange {
            start: to_pos(span.start_line, span.start_col, span.start_byte),
            end: to_pos(span.end_line, span.end_col, span.end_byte),
        }
    }
}

/// Byte offset of the start of a **0-based** line, or `None` past the end.
fn line_start_of(text: &str, line: u32) -> Option<usize> {
    let mut start = 0usize;
    let mut n = line;
    while n > 0 {
        let nl = text[start..].find('\n')?;
        start += nl + 1;
        if start > text.len() { return None; }
        n -= 1;
    }
    Some(start)
}

/// Byte offset of a **1-based** (line, byte column), clamped to that line's
/// end so an over-long column can never leak into the next line.
fn line_col_byte(text: &str, line: u32, col: u32) -> Option<usize> {
    let start = line_start_of(text, line.saturating_sub(1))?;
    let end = text[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(text.len());
    Some((start + col.saturating_sub(1) as usize).min(end))
}

/// Registry of all source files in one compilation. `SourceId 0` is reserved
/// for the main file so a default `SourceSpan` never aliases a real import.
#[derive(Debug, Clone, Default)]
pub struct SourceRegistry {
    pub files: Vec<SourceFile>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        SourceRegistry { files: Vec::new() }
    }

    /// Register a file (dedup by path); returns its SourceId.
    /// `text` is used to build the line-start table.
    pub fn add(&mut self, path: &str, text: &str) -> SourceId {
        if let Some(id) = self.id_of(path) {
            return id;
        }
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(
                text.bytes()
                    .enumerate()
                    .filter(|&(_, b)| b == b'\n')
                    .map(|(i, _)| i + 1),
            )
            .collect();
        let id = self.files.len() as SourceId;
        self.files.push(SourceFile {
            path: path.to_string(),
            line_starts,
            len: text.len(),
        });
        id
    }

    pub fn id_of(&self, path: &str) -> Option<SourceId> {
        self.files.iter().position(|f| f.path == path).map(|i| i as SourceId)
    }

    pub fn get(&self, id: SourceId) -> Option<&SourceFile> {
        self.files.get(id as usize)
    }

    pub fn path_of(&self, id: SourceId) -> &str {
        self.files.get(id as usize).map(|f| f.path.as_str()).unwrap_or("")
    }
}

/// Where an emitted line's content came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanOrigin {
    /// Text exists verbatim in a source file.
    #[default]
    Source,
    /// Generated text with no single source line (macro expansion output past
    /// the invocation site, future codegen helpers). Carries no file mapping.
    Synthetic,
}

/// LSP-facing position: **0-based line, 0-based UTF-16 code-unit column**.
///
/// The LSP protocol encodes positions this way, while Jeti's internal model is
/// 1-based lines with UTF-8 byte columns. Every crossing of the LSP boundary
/// must go through [`SourceFile::offset_to_lsp`] / [`SourceFile::lsp_to_offset`]
/// (or [`SourceSpan::to_lsp_range`]) — the two unit systems are never mixed
/// (plan §32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// LSP-facing half-open range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

/// A half-open `[start, end)` range in a registered source file.
///
/// Position fields are 1-based; `end_*` is the position **one past** the last
/// character (exclusive), mirroring the `Mapping` schema in
/// [`crate::source_map_file`]. `start_byte` / `end_byte` are offsets into the
/// file text and are `None` only when the file was registered without a line
/// table (legacy paths); when present they are the precise identity for
/// slicing and incremental parsing (plan §31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: SourceId,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column (UTF-8 bytes).
    pub start_col: u32,
    /// 1-based end line (exclusive end position).
    pub end_line: u32,
    /// 1-based end column (UTF-8 bytes, exclusive).
    pub end_col: u32,
    /// Byte offset of the first character, when known.
    pub start_byte: Option<usize>,
    /// Byte offset one past the last character, when known.
    pub end_byte: Option<usize>,
    pub origin: SpanOrigin,
}

impl SourceSpan {
    pub fn is_synthetic(&self) -> bool {
        self.origin == SpanOrigin::Synthetic
    }

    /// A zero-width span at one position (what a line-level resolution can
    /// honestly report before the parser supplies an end position).
    fn point(
        file: SourceId, line: u32, col: u32,
        byte: Option<usize>, origin: SpanOrigin,
    ) -> SourceSpan {
        SourceSpan {
            file,
            start_line: line,
            start_col: col,
            end_line: line,
            end_col: col,
            start_byte: byte,
            end_byte: byte,
            origin,
        }
    }

    /// The LSP range for this span, in the file's text.
    ///
    /// Prefers the recorded byte offsets (exact); otherwise converts the
    /// 1-based line/byte-column fields. This is the single place the internal
    /// model is projected onto LSP's UTF-16 coordinates. Hot paths with a
    /// [`SourceMap`] in hand prefer [`SourceMap::to_lsp_range`] (table-driven,
    /// O(log n)); this raw-text form scans per call.
    pub fn to_lsp_range(&self, text: &str) -> LspRange {
        let to_pos = |line: u32, col: u32, byte: Option<usize>| match byte {
            Some(b) => SourceFile::offset_to_lsp(text, b),
            None => match line_col_byte(text, line, col) {
                Some(b) => SourceFile::offset_to_lsp(text, b),
                None => LspPosition::default(),
            },
        };
        LspRange {
            start: to_pos(self.start_line, self.start_col, self.start_byte),
            end: to_pos(self.end_line, self.end_col, self.end_byte),
        }
    }
}

/// One region of the flattened (post-preprocessing) buffer, mapped back to
/// its source file and 1-based line range.
#[derive(Debug, Clone, Copy)]
pub struct LineRegion {
    /// 1-based start line in the flattened buffer.
    pub out_start: u32,
    /// 1-based end line in the flattened buffer (inclusive).
    pub out_end: u32,
    pub file: SourceId,
    /// 1-based start line in the source file.
    pub src_start: u32,
    pub origin: SpanOrigin,
}

/// How a run of columns on one flattened line maps back to the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColOrigin {
    /// Verbatim source text that an expansion earlier on the same line shifted
    /// by `delta` bytes: the real column is `out_col - delta`.
    Verbatim { delta: i32 },
    /// Text a macro expansion produced. It has no column of its own in the
    /// source, so it resolves to the invocation site — the 1-based column of
    /// the macro name at the call, which is where the author can act on it.
    Invocation { col: u32 },
}

/// One run of columns on a flattened-buffer line whose column needs remapping.
///
/// Keyed by output line because an expansion shifts only the line it happens
/// on; the line itself is translated by [`SourceMap::locate`]. Runs are
/// disjoint and ordered left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnRegion {
    /// 1-based line in the flattened buffer.
    pub out_line: u32,
    /// First 1-based column covered (inclusive).
    pub out_start: u32,
    /// First column past the run (exclusive).
    pub out_end: u32,
    pub origin: ColOrigin,
}

/// Maps flattened-buffer lines to source files via [`LineRegion`]s.
///
/// Built by the preprocessor; consumed by the diagnostic path
/// (`translate_lines`) and, later, codegen `#line` emission (阶段 2).
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    regions: Vec<LineRegion>,
    /// Files touched by this mapping (parallel data, indexed by SourceId).
    registry: SourceRegistry,
    /// Column-provenance runs from macro expansion, keyed by flattened line.
    /// Empty when no macro ran.
    columns: Vec<ColumnRegion>,
}

impl SourceMap {
    /// Build from a region list + registry.
    pub fn from_regions(regions: Vec<LineRegion>, registry: SourceRegistry) -> Self {
        SourceMap { regions, registry, columns: Vec::new() }
    }

    pub fn regions(&self) -> &[LineRegion] {
        &self.regions
    }

    pub fn registry(&self) -> &SourceRegistry {
        &self.registry
    }

    /// Attach the column-provenance runs the macro expander reported.
    pub fn with_columns(mut self, columns: Vec<ColumnRegion>) -> Self {
        self.columns = columns;
        self
    }

    /// Correct a flattened-buffer column to its real source column, reporting
    /// where that column came from (macro-expansion output is `Synthetic`).
    ///
    /// Lines come back through [`Self::locate`]; this covers the shift an
    /// expansion causes *within* a line: text the expansion produced has no
    /// column of its own and resolves to the invocation site, while text merely
    /// pushed right by an expansion moves back by that delta.
    pub fn adjust_col(&self, inlined_line: usize, out_col: u32) -> (u32, SpanOrigin) {
        if self.columns.is_empty() {
            return (out_col, SpanOrigin::Source);
        }
        let line = inlined_line as u32;
        // Runs are sorted by (line, column) and disjoint, so the only candidate
        // is the last one starting at or before this column.
        let idx = self.columns.partition_point(|c| (c.out_line, c.out_start) <= (line, out_col));
        if idx == 0 {
            return (out_col, SpanOrigin::Source);
        }
        let c = &self.columns[idx - 1];
        if c.out_line != line || out_col >= c.out_end {
            return (out_col, SpanOrigin::Source);
        }
        match c.origin {
            ColOrigin::Verbatim { delta } => (
                (out_col as i64 - delta as i64).max(1) as u32,
                SpanOrigin::Source,
            ),
            ColOrigin::Invocation { col } => (col, SpanOrigin::Synthetic),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Number of flattened-buffer lines covered (used by legacy `len`).
    pub fn out_lines(&self) -> u32 {
        self.regions.last().map(|r| r.out_end).unwrap_or(0)
    }

    /// Locate a flattened-buffer line: returns (display file, 1-based line).
    /// Unmapped lines resolve to ("", line) — the legacy behavior for
    /// lines the preprocessor did not tag.
    pub fn locate(&self, inlined_line: usize) -> (String, u32) {
        let l = inlined_line as u32;
        for r in &self.regions {
            if l >= r.out_start && l <= r.out_end {
                let real = r.src_start + (l - r.out_start);
                return (self.registry.path_of(r.file).to_string(), real);
            }
        }
        (String::new(), l)
    }

    /// Structured line-level resolution: the span covers `inlined_line`, as a
    /// zero-width point at column 1. Consumers that know the node's extent use
    /// [`Self::span_from_inline`] instead.
    pub fn locate_span(&self, inlined_line: usize) -> SourceSpan {
        let l = inlined_line as u32;
        for r in &self.regions {
            if l >= r.out_start && l <= r.out_end {
                let src_line = r.src_start + (l - r.out_start);
                let byte = self
                    .registry
                    .get(r.file)
                    .and_then(|f| f.line_col_to_offset(src_line, 1));
                return SourceSpan::point(r.file, src_line, 1, byte, r.origin);
            }
        }
        SourceSpan::point(0, l, 1, None, SpanOrigin::Synthetic)
    }

    /// Resolve an inline-buffer position pair to a source [`SourceSpan`].
    ///
    /// `start` and `end` are 1-based `(line, column)` in the flattened buffer
    /// the lexer/parser saw — exactly what CST/AST nodes carry today. The result
    /// names the real file, the source lines, the macro-adjusted columns
    /// (expansion output resolves to its invocation site, plan stage 1) and the
    /// byte offsets into that file.
    ///
    /// The span is synthetic when either endpoint (or the line) is synthetic, so
    /// a caller can tell "this came from real source" from "the compiler made
    /// this up".
    pub fn span_from_inline(&self, start: (u32, u32), end: (u32, u32)) -> SourceSpan {
        let s = self.map_inline_pos(start.0, start.1);
        let e = self.map_inline_pos(end.0, end.1);
        let (start_byte, end_byte, file, sl, sc, el, ec, origin) = match (s, e) {
            (Some((sf, sl, sc, so)), Some((ef, el, ec, eo))) => {
                // Prefer the start endpoint's file identity; a span never
                // crosses files in the inline buffer (an `#import` boundary is
                // a separate region).
                let sb = self.registry.get(sf).and_then(|f| f.line_col_to_offset(sl, sc));
                let eb = if sf == ef {
                    self.registry.get(sf).and_then(|f| f.line_col_to_offset(el, ec))
                } else {
                    None
                };
                let origin = if so == SpanOrigin::Synthetic || eo == SpanOrigin::Synthetic {
                    SpanOrigin::Synthetic
                } else {
                    SpanOrigin::Source
                };
                (sb, eb, sf, sl, sc, el, ec, origin)
            }
            // Either endpoint unmapped: a synthetic point, no file identity.
            (s, e) => {
                let (line, col) = match (s, e) {
                    (Some((_, l, c, _)), _) => (l, c),
                    (_, Some((_, l, c, _))) => (l, c),
                    _ => (start.0, start.1),
                };
                (None, None, 0, line, col, line, col, SpanOrigin::Synthetic)
            }
        };
        SourceSpan {
            file,
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
            start_byte,
            end_byte,
            origin,
        }
    }

    /// Map one inline-buffer `(line, col)` to `(file, source line, source col,
    /// origin)`, applying the macro column correction. `None` when the inline
    /// line is not covered by any region.

    /// The LSP range for `span`, projected through this map's line tables
    /// where `span`'s file is registered (O(log n) per position — the hot
    /// diagnostic/index paths); an unregistered file falls back to
    /// [`SourceSpan::to_lsp_range`]'s raw-text scan. `text` is that file's
    /// content; the length check guards against a table built from another
    /// revision of it.
    pub fn to_lsp_range(&self, span: &SourceSpan, text: &str) -> LspRange {
        match self.registry.get(span.file) {
            Some(file) if file.len == text.len() => file.to_lsp_range(span, text),
            _ => span.to_lsp_range(text),
        }
    }

    /// Resolve a CST/AST node position pair to a [`SourceSpan`].
    ///
    /// Node positions follow the parser's convention: `line` is a line in the
    /// flattened post-preprocessing buffer, while `col` was already remapped to
    /// a **source** column by the parser as it read each token. So this resolves
    /// the lines (to a file + source line) and the byte offsets, and must NOT
    /// re-apply the macro column correction that [`Self::span_from_inline`]
    /// performs for raw lexer positions.
    ///
    /// `end` positions that would precede the start (a synthesized node whose
    /// end was never stamped) collapse to a zero-width span at the start.
    pub fn span_from_node(&self, start: (u32, u32), end: (u32, u32)) -> SourceSpan {
        let (file, sl, so) = match self.map_line(start.0) {
            Some(m) => m,
            None => {
                return SourceSpan::point(0, start.0, start.1, None, SpanOrigin::Synthetic);
            }
        };
        let (efile, el, eo) = self.map_line(end.0).unwrap_or((file, end.0, SpanOrigin::Synthetic));
        // Guard against an unstamped end (0/stale) pointing before the start.
        let (el, ec) = if (el, end.1) < (sl, start.1) {
            (sl, start.1)
        } else {
            (el, end.1)
        };
        let reg = self.registry.get(file);
        let start_byte = reg.and_then(|f| f.line_col_to_offset(sl, start.1));
        let end_byte = if efile == file {
            reg.and_then(|f| f.line_col_to_offset(el, ec))
        } else {
            None
        };
        let origin = if so == SpanOrigin::Synthetic || eo == SpanOrigin::Synthetic {
            SpanOrigin::Synthetic
        } else {
            SpanOrigin::Source
        };
        SourceSpan {
            file,
            start_line: sl,
            start_col: start.1,
            end_line: el,
            end_col: ec,
            start_byte,
            end_byte,
            origin,
        }
    }

    /// Map one flattened-buffer line to `(file, source line, origin)`.
    fn map_line(&self, line: u32) -> Option<(SourceId, u32, SpanOrigin)> {
        for r in &self.regions {
            if line >= r.out_start && line <= r.out_end {
                return Some((r.file, r.src_start + (line - r.out_start), r.origin));
            }
        }
        None
    }

    fn map_inline_pos(&self, line: u32, col: u32) -> Option<(SourceId, u32, u32, SpanOrigin)> {
        for r in &self.regions {
            if line >= r.out_start && line <= r.out_end {
                let src_line = r.src_start + (line - r.out_start);
                let (src_col, col_origin) = self.adjust_col(line as usize, col);
                let origin = if r.origin == SpanOrigin::Synthetic || col_origin == SpanOrigin::Synthetic {
                    SpanOrigin::Synthetic
                } else {
                    SpanOrigin::Source
                };
                return Some((r.file, src_line, src_col, origin));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Legacy line-table API (kept until all consumers migrate; plan 阶段 1 keeps
// 兼容访问器 to avoid a one-shot big-bang change).
// ---------------------------------------------------------------------------

impl SourceMap {
    /// Legacy constructor: build the region list from the old per-line
    /// `(file, line)` table, merging consecutive same-file runs.
    pub fn new(lines: Vec<(String, u32)>) -> Self {
        let mut registry = SourceRegistry::new();
        // Register every distinct file; dedup keeps legacy behavior where the
        // same path shown repeatedly maps to one identity.
        for (path, _) in &lines {
            if registry.id_of(path).is_none() {
                registry.add(path, "");
            }
        }
        let mut regions: Vec<LineRegion> = Vec::new();
        for (i, (path, line)) in lines.iter().enumerate() {
            let file = registry.id_of(path).expect("registered above") as SourceId;
            let out_start = i as u32 + 1;
            match regions.last_mut() {
                Some(r)
                    if r.file == file
                        && r.origin == SpanOrigin::Source
                        && r.out_end + 1 == out_start
                        && r.src_start + (r.out_end - r.out_start) + 1 == *line =>
                {
                    r.out_end = out_start;
                }
                _ => regions.push(LineRegion {
                    out_start,
                    out_end: out_start,
                    file,
                    src_start: *line,
                    origin: SpanOrigin::Source,
                }),
            }
        }
        SourceMap { regions, registry, columns: Vec::new() }
    }

    /// Build from the old per-line `(file, line)` table but reuse an existing
    /// registry (the preprocessor registers files with real text; the legacy
    /// path above re-registers with empty text). Files present in `lines` but
    /// missing from the registry are added with empty text.
    pub fn from_line_table(lines: Vec<(String, u32)>, registry: &mut SourceRegistry) -> Self {
        for (path, _) in &lines {
            if registry.id_of(path).is_none() {
                registry.add(path, "");
            }
        }
        let mut regions: Vec<LineRegion> = Vec::new();
        for (i, (path, line)) in lines.iter().enumerate() {
            let file = registry
                .id_of(path)
                .expect("registered above") as SourceId;
            let out_start = i as u32 + 1;
            match regions.last_mut() {
                Some(r)
                    if r.file == file
                        && r.origin == SpanOrigin::Source
                        && r.out_end + 1 == out_start
                        && r.src_start + (r.out_end - r.out_start) + 1 == *line =>
                {
                    r.out_end = out_start;
                }
                _ => regions.push(LineRegion {
                    out_start,
                    out_end: out_start,
                    file,
                    src_start: *line,
                    origin: SpanOrigin::Source,
                }),
            }
        }
        let owned = std::mem::take(registry);
        SourceMap { regions, registry: owned, columns: Vec::new() }
    }

    /// Total number of mapped flattened-buffer lines.
    pub fn len(&self) -> usize {
        self.out_lines() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_line_table_locate() {
        let sm = SourceMap::new(vec![
            ("main.jeti".into(), 1),
            ("main.jeti".into(), 2),
            ("foo.hje".into(), 10),
        ]);
        assert_eq!(sm.locate(1), ("main.jeti".to_string(), 1));
        assert_eq!(sm.locate(2), ("main.jeti".to_string(), 2));
        assert_eq!(sm.locate(3), ("foo.hje".to_string(), 10));
        assert_eq!(sm.locate(4), (String::new(), 4));
    }

    #[test]
    fn adjust_col_moves_expansion_output_to_invocation() {
        // `SHORT` (5 bytes) expands to 10 bytes at column 1, so the tail of the
        // line is shifted right by 5: `nil` at flattened column 26 is really at
        // 21 — and anything the expansion produced points at the call instead.
        let sm = SourceMap::from_regions(vec![], SourceRegistry::new()).with_columns(vec![
            ColumnRegion {
                out_line: 1, out_start: 1, out_end: 11,
                origin: ColOrigin::Invocation { col: 1 },
            },
            ColumnRegion {
                out_line: 1, out_start: 11, out_end: 27,
                origin: ColOrigin::Verbatim { delta: 5 },
            },
        ]);
        assert_eq!(sm.adjust_col(1, 3), (1, SpanOrigin::Synthetic));
        assert_eq!(sm.adjust_col(1, 26), (21, SpanOrigin::Source));
        // A line with no runs passes through untouched.
        assert_eq!(sm.adjust_col(2, 26), (26, SpanOrigin::Source));
    }

    #[test]
    fn regions_merge_same_file_runs() {
        let sm = SourceMap::new(vec![
            ("a.jeti".into(), 1),
            ("a.jeti".into(), 2),
            ("a.jeti".into(), 3),
        ]);
        assert_eq!(sm.regions().len(), 1);
        assert_eq!(sm.regions()[0].out_start, 1);
        assert_eq!(sm.regions()[0].out_end, 3);
    }

    #[test]
    fn registry_line_offsets() {
        let mut reg = SourceRegistry::new();
        let id = reg.add("x.jeti", "one\ntwo\nthree");
        let f = reg.get(id).unwrap();
        assert_eq!(f.line_to_offset(2), Some(4));
        assert_eq!(f.offset_to_line(6), 2); // inside "two"
        assert_eq!(SourceFile::offset_to_col("one\ntwo\nthree", 6), 3);
    }

    #[test]
    fn locate_span_reports_file_identity() {
        let mut reg = SourceRegistry::new();
        let a = reg.add("a.jeti", "line1\nline2\n");
        let regions = vec![LineRegion {
            out_start: 1,
            out_end: 2,
            file: a,
            src_start: 5,
            origin: SpanOrigin::Source,
        }];
        let sm = SourceMap::from_regions(regions, reg);
        let s = sm.locate_span(2);
        assert_eq!(s.file, a);
        assert_eq!(s.start_line, 6);
        assert!(!s.is_synthetic());
        // Line-level resolution is a zero-width point at column 1.
        assert_eq!((s.start_col, s.end_line, s.end_col), (1, 6, 1));
    }

    #[test]
    fn span_from_inline_maps_start_end_and_bytes() {
        // Source file with six lines; the flattened buffer starts at source
        // line 5, so inline line 1 == source line 5.
        let mut reg = SourceRegistry::new();
        // line_starts = [0,3,6,9,12,17]; line 5 starts at 12, line 6 at 17.
        let a = reg.add("a.jeti", "l1\nl2\nl3\nl4\naaaa\nbbbb\n");
        let regions = vec![LineRegion {
            out_start: 1,
            out_end: 2,
            file: a,
            src_start: 5,
            origin: SpanOrigin::Source,
        }];
        let sm = SourceMap::from_regions(regions, reg);
        // Inline (1,2)..(2,3) -> source (5,2)..(6,3).
        let s = sm.span_from_inline((1, 2), (2, 3));
        assert_eq!(s.file, a);
        assert_eq!((s.start_line, s.start_col), (5, 2));
        assert_eq!((s.end_line, s.end_col), (6, 3));
        assert_eq!(s.start_byte, Some(13));
        assert_eq!(s.end_byte, Some(19));
        assert_eq!(s.origin, SpanOrigin::Source);
        assert!(!s.is_synthetic());

        // An unmapped inline line degrades to a synthetic point (file 0).
        let s2 = sm.span_from_inline((9, 1), (9, 4));
        assert_eq!(s2.file, 0);
        assert!(s2.is_synthetic());
        assert_eq!(s2.start_byte, None);
    }

    #[test]
    fn span_to_lsp_range_uses_utf16_columns() {
        // "héllo" — é is 2 bytes, 1 UTF-16 unit. "中文" — 3 bytes, 1 unit each.
        let text = "héllo 中文";
        let mut reg = SourceRegistry::new();
        let f = reg.add("u.jeti", text);
        let regions = vec![LineRegion {
            out_start: 1, out_end: 1, file: f, src_start: 1, origin: SpanOrigin::Source,
        }];
        let sm = SourceMap::from_regions(regions, reg);
        // Bytes 0..6 = "héllo" (1 + 2 + 1 + 1 + 1); UTF-16 units 0..5 (é is one
        // unit despite being two bytes).
        let sp = sm.span_from_inline((1, 1), (1, 7));
        assert_eq!(sp.start_byte, Some(0));
        assert_eq!(sp.end_byte, Some(6));
        let r = sp.to_lsp_range(text);
        assert_eq!(r.start, LspPosition { line: 0, character: 0 });
        assert_eq!(r.end, LspPosition { line: 0, character: 5 });
        // 中文: bytes 7..13, UTF-16 units 6..8 -> proves the unit conversion.
        let sp2 = sm.span_from_inline((1, 8), (1, 14));
        let r2 = sp2.to_lsp_range(text);
        assert_eq!(r2.start, LspPosition { line: 0, character: 6 });
        assert_eq!(r2.end, LspPosition { line: 0, character: 8 });
    }

    #[test]
    fn table_projection_matches_raw_text_projection() {
        // Multi-line, mixed UTF-8: the table-driven fast path must project
        // every span exactly as the raw-text path does. Node positions always
        // land on char boundaries (token starts/ends), so parity is asserted
        // for boundary columns — both paths reject the rest identically.
        let text = "l1\nhéllo 中文\nlast\n";
        let mut reg = SourceRegistry::new();
        let f = reg.add("u.jeti", text);
        let regions = (1..=3)
            .map(|l| LineRegion {
                out_start: l, out_end: l, file: f, src_start: l, origin: SpanOrigin::Source,
            })
            .collect();
        let sm = SourceMap::from_regions(regions, reg);
        for (line_idx, line_text) in text.split_inclusive('\n').enumerate() {
            let l = (line_idx + 1) as u32;
            // 1-based byte columns at every char boundary, plus line end.
            let cols: Vec<u32> = line_text
                .char_indices()
                .map(|(i, _)| i as u32 + 1)
                .chain(std::iter::once(line_text.len() as u32))
                .collect();
            for &s in &cols {
                for &e in cols.iter().filter(|&&c| c >= s) {
                    let sp = sm.span_from_inline((l, s), (l, e));
                    assert_eq!(
                        sm.to_lsp_range(&sp, text),
                        sp.to_lsp_range(text),
                        "byte span {sp:?}"
                    );
                }
            }
        }
        // A legacy byte-less span (line/col only) projects identically.
        let legacy = SourceSpan {
            file: f, start_line: 2, start_col: 2, end_line: 2, end_col: 8,
            start_byte: None, end_byte: None, origin: SpanOrigin::Source,
        };
        assert_eq!(sm.to_lsp_range(&legacy, text), legacy.to_lsp_range(text));
        // An unregistered file takes the raw fallback.
        let other = SourceSpan {
            file: 9, start_line: 2, start_col: 2, end_line: 2, end_col: 8,
            start_byte: None, end_byte: None, origin: SpanOrigin::Source,
        };
        assert_eq!(sm.to_lsp_range(&other, text), other.to_lsp_range(text));
    }

    #[test]
    fn span_from_node_maps_lines_and_columns_without_re_adjusting() {
        let mut reg = SourceRegistry::new();
        // line_starts = [0,3,6,9,12,17]; line 5 is "aaaa" (bytes 12..17).
        let a = reg.add("a.jeti", "l1\nl2\nl3\nl4\naaaa\nbbbb\n");
        let regions = vec![LineRegion {
            out_start: 1, out_end: 2, file: a, src_start: 5, origin: SpanOrigin::Source,
        }];
        let sm = SourceMap::from_regions(regions, reg);
        // Inline line 1 -> source line 5; columns are taken as already-source.
        let s = sm.span_from_node((1, 2), (1, 5));
        assert_eq!((s.file, s.start_line, s.start_col), (a, 5, 2));
        assert_eq!((s.end_line, s.end_col), (5, 5));
        assert_eq!(s.start_byte, Some(13));
        assert_eq!(s.end_byte, Some(16));
        assert_eq!(s.origin, SpanOrigin::Source);

        // An unstamped/backwards end collapses to a zero-width span.
        let bad = sm.span_from_node((1, 5), (1, 2));
        assert_eq!((bad.start_col, bad.end_col), (5, 5));
        assert_eq!(bad.start_byte, bad.end_byte);
        let zero = sm.span_from_node((1, 1), (0, 0));
        assert_eq!((zero.start_line, zero.end_line), (5, 5));

        // An unmapped line degrades to a synthetic point with no file.
        let off = sm.span_from_node((9, 1), (9, 2));
        assert_eq!(off.file, 0);
        assert!(off.is_synthetic());
    }

    #[test]
    fn utf16_position_round_trips_with_astral_chars() {
        // "a😀b" — 😀 is 4 UTF-8 bytes and 2 UTF-16 code units.
        let text = "a😀b\nsecond";
        // offset of 'b' is 5 bytes.
        assert_eq!(
            SourceFile::offset_to_lsp(text, 5),
            LspPosition { line: 0, character: 3 }
        );
        assert_eq!(SourceFile::lsp_to_offset(text, LspPosition { line: 0, character: 3 }), Some(5));
        // Line 1 ("second") starts at byte 7 (0-based): line 1, char 0.
        assert_eq!(
            SourceFile::offset_to_lsp(text, 7),
            LspPosition { line: 1, character: 0 }
        );
        assert_eq!(SourceFile::lsp_to_offset(text, LspPosition { line: 1, character: 6 }), Some(13));
        // Past the end of the line / file -> None.
        assert_eq!(SourceFile::lsp_to_offset(text, LspPosition { line: 1, character: 99 }), None);
        assert_eq!(SourceFile::lsp_to_offset(text, LspPosition { line: 9, character: 0 }), None);
    }

    #[test]
    fn line_col_offset_round_trips() {
        let mut reg = SourceRegistry::new();
        let id = reg.add("x.jeti", "one\ntwo\nthree");
        let f = reg.get(id).unwrap();
        assert_eq!(f.line_col_to_offset(2, 3), Some(6));
        assert_eq!(f.offset_to_line_col(6), (2, 3));
        // Column past the file end clamps to the length.
        assert_eq!(f.line_col_to_offset(3, 999), Some(f.len));
        assert_eq!(f.line_col_to_offset(99, 1), None);
    }
}
