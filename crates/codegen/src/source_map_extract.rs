//! Extraction of the sidecar source map (`.jeti.map`) from the FINAL generated
//! C text, by scanning its `#line` directive stream
//! (`docs/plans/source_locations_debug_lsp_plan.md` 阶段 4).
//!
//! Why scan the finished text instead of recording events during emission:
//! the emitted buffer is post-processed before it reaches disk
//! (`normalize_t_sentinels_text` rewrites sentinel comments), so any offset
//! recorded during emission would drift. The `#line` stream is the single
//! ground truth both the C preprocessor and this extractor consume, and it
//! already carries everything the map needs (file, source line, position in
//! the C). Within one directive scope every generated C line advances the
//! source line by one (CPP `#line` semantics), so scopes collapse into
//! line-aligned regions.
//!
//! The scanner is a strict C lexer state machine (code / line comment / block
//! comment / string / char literal). Directives are recognized only at
//! column 0 in code context — jetic always emits them there, and a `#line`
//! inside a passthrough comment or string literal can never fake a mapping.
//! Lines before the first directive are unmapped: that is exactly the
//! preprocessor's view (they keep the generated file's own numbering).

use std::fs;

use jeti_cst::source_map_file::{
    hash_tag, GeneratedArtifact, Mapping, SourceEntry, SourceMapFile,
};

use crate::codegen::SYNTHETIC_FILE;

/// Inputs for [`extract_source_map`] that the C text itself cannot supply.
pub struct ExtractOptions<'a> {
    /// Path recorded for the generated C artifact (`generated.path`).
    pub generated_path: String,
    /// The main `.jeti` translation unit this build compiled.
    pub primary_source: String,
    /// In-memory contents for source hashing, preferred over disk re-reads:
    /// `(path as spelled in the #line stream, content)` pairs.
    pub source_contents: &'a [(String, String)],
}

struct DirEvent {
    /// 1-based physical line the directive occupies.
    line: u32,
    file: String,
    n: u32,
    synthetic: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Lex {
    Code,
    LineComment,
    BlockComment,
    Str,
    Chr,
}

/// Lex the C text, returning physical-line start offsets and every `#line`
/// directive found at column 0 in code context.
fn scan(c_text: &str) -> (Vec<usize>, Vec<DirEvent>) {
    let b = c_text.as_bytes();
    let mut line_starts: Vec<usize> = vec![0];
    let mut events: Vec<DirEvent> = Vec::new();
    let mut st = Lex::Code;
    let mut i = 0usize;
    let mut line: u32 = 1;
    while i < b.len() {
        if b[i] == b'\n' {
            if st != Lex::BlockComment {
                // Comments survive a newline; an unterminated string/char
                // literal cannot legally span one, so treat it as closed
                // (lenient) and let the scan self-heal.
                st = Lex::Code;
            }
            i += 1;
            line_starts.push(i);
            line += 1;
            continue;
        }
        let at_line_start = i == *line_starts.last().unwrap();
        if at_line_start
            && st == Lex::Code
            && b[i] == b'#'
            && c_text[i..].starts_with("#line")
            && matches!(b.get(i + 5), Some(b' ') | Some(b'\t'))
        {
            if let Some((n, file)) = parse_line_directive(&c_text[i + 5..]) {
                let after = match c_text[i..].find('\n') {
                    Some(rel) => i + rel + 1,
                    None => b.len(), // last line without newline: empty scope
                };
                events.push(DirEvent {
                    line,
                    synthetic: file == SYNTHETIC_FILE,
                    n,
                    file,
                });
                // Jump past the directive body so its quotes/escapes are not
                // lexed as C: land on the newline so the branch above runs
                // next iteration (or end the loop at EOF).
                i = if after < b.len() { after - 1 } else { b.len() };
                continue;
            }
        }
        match st {
            Lex::Code => match b[i] {
                b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                    st = Lex::LineComment;
                    i += 2;
                }
                b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                    st = Lex::BlockComment;
                    i += 2;
                }
                b'"' => {
                    st = Lex::Str;
                    i += 1;
                }
                b'\'' => {
                    st = Lex::Chr;
                    i += 1;
                }
                _ => i += 1,
            },
            Lex::LineComment => i += 1,
            Lex::BlockComment => {
                if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    st = Lex::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            Lex::Str | Lex::Chr => {
                let closes =
                    (b[i] == b'"' && st == Lex::Str) || (b[i] == b'\'' && st == Lex::Chr);
                if closes {
                    st = Lex::Code;
                    i += 1;
                } else if b[i] == b'\\' {
                    if i + 1 < b.len() && b[i + 1] == b'\n' {
                        // Line continuation: full newline bookkeeping, state kept.
                        i += 2;
                        line_starts.push(i);
                        line += 1;
                    } else {
                        i += 2; // skip the escaped character wholesale
                    }
                } else {
                    i += 1;
                }
            }
        }
    }
    (line_starts, events)
}

/// Parse `"<spaces> <digits> <spaces> \"path\""` (the text after `#line`).
/// Only `\\` and `\"` are unescaped — those are the only escapes
/// [`crate::codegen::escape_line_path`] produces; other backslash sequences
/// are kept verbatim.
fn parse_line_directive(rest: &str) -> Option<(u32, String)> {
    let b = rest.as_bytes();
    let mut j = 0;
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    let digits_start = j;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    if j == digits_start {
        return None;
    }
    let n: u32 = rest[digits_start..j].parse().ok()?;
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    if j >= b.len() || b[j] != b'"' {
        return None;
    }
    j += 1;
    let mut path = String::new();
    while j < b.len() {
        match b[j] {
            b'"' => return Some((n, path)),
            b'\\' if j + 1 < b.len() && (b[j + 1] == b'"' || b[j + 1] == b'\\') => {
                path.push(b[j + 1] as char);
                j += 2;
            }
            lead => {
                // Whole UTF-8 char (paths are never re-escaped beyond `\\`/`\"`).
                let len = if lead < 0x80 {
                    1
                } else if lead < 0xE0 {
                    2
                } else if lead < 0xF0 {
                    3
                } else {
                    4
                };
                path.push_str(rest.get(j..j + len)?);
                j += len;
            }
        }
    }
    None // unterminated path
}

/// Hash a mapped source file: caller-supplied content wins, then a best-effort
/// disk read at the path as spelled; empty string = unknown (the schema allows
/// absent hashes so a missing file never blocks map generation).
fn source_hash(path: &str, opts: &ExtractOptions) -> String {
    for (p, content) in opts.source_contents {
        if p == path {
            return hash_tag(content.as_bytes());
        }
    }
    match fs::read_to_string(path) {
        Ok(s) => hash_tag(s.as_bytes()),
        Err(_) => String::new(),
    }
}

/// Extract the sidecar source map from the final generated C text — the exact
/// bytes that reach disk, so every offset below is byte-identical to the
/// written `.c` file.
pub fn extract_source_map(c_text: &str, opts: &ExtractOptions) -> SourceMapFile {
    let (line_starts, events) = scan(c_text);
    let total_lines: u32 = {
        let nl = c_text.bytes().filter(|&c| c == b'\n').count() as u32;
        nl + u32::from(!c_text.ends_with('\n'))
    };

    // Sources: every distinct real file the directive stream names, sorted
    // for stable ids across emission-order changes.
    let mut paths: Vec<&str> = events
        .iter()
        .filter(|e| !e.synthetic)
        .map(|e| e.file.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let sources: Vec<SourceEntry> = paths
        .iter()
        .enumerate()
        .map(|(id, p)| SourceEntry {
            id: id as u64,
            path: (*p).to_string(),
            hash: source_hash(p, opts),
        })
        .collect();

    let mut mappings = Vec::new();
    for (k, ev) in events.iter().enumerate() {
        // `#line N` numbers the FOLLOWING line N (CPP semantics).
        let first = ev.line + 1;
        let last = events
            .get(k + 1)
            .map_or(total_lines, |next| next.line.saturating_sub(1));
        if first > last {
            continue; // empty scope (back-to-back directives / trailing directive)
        }
        let c_start = line_starts.get((first - 1) as usize).copied().unwrap_or(0);
        let c_end = line_starts
            .get(last as usize)
            .copied()
            .unwrap_or(c_text.len());
        // Exclusive-end column on the last line: its byte length + 1,
        // excluding the trailing newline.
        let last_line_start = line_starts.get((last - 1) as usize).copied().unwrap_or(0);
        let mut content = c_end.saturating_sub(last_line_start);
        if c_text.as_bytes().get(c_end.saturating_sub(1)) == Some(&b'\n') {
            content = content.saturating_sub(1);
        }
        let (src_id, src_line_start, src_line_end, kind, synthetic) = if ev.synthetic {
            (None, None, None, "synthetic", true)
        } else {
            let id = paths.iter().position(|p| *p == ev.file.as_str()).map(|p| p as u64);
            // Unreachable: ev is non-synthetic, so its file is in `paths`.
            let id = match id {
                Some(id) => id,
                None => continue,
            };
            (
                Some(id),
                Some(ev.n),
                Some(ev.n.saturating_add(last - first)),
                "source",
                false,
            )
        };
        mappings.push(Mapping {
            c_start: c_start as u64,
            c_end: c_end as u64,
            c_start_line: first,
            c_start_col: 1,
            c_end_line: last,
            c_end_col: content.saturating_add(1) as u32,
            src_id,
            src_line_start,
            src_line_end,
            kind: kind.to_string(),
            synthetic,
        });
    }

    let mut f = SourceMapFile::new(
        "jetic",
        &opts.primary_source,
        GeneratedArtifact {
            path: opts.generated_path.clone(),
            hash: hash_tag(c_text.as_bytes()),
        },
    );
    f.sources = sources;
    f.mappings = mappings;
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts<'a>(
        contents: &'a [(String, String)],
    ) -> ExtractOptions<'a> {
        ExtractOptions {
            generated_path: "main.c".into(),
            primary_source: "main.jeti".into(),
            source_contents: contents,
        }
    }

    #[test]
    fn basic_regions_spans_and_offsets() {
        let c_text = "#include <stdio.h>\n\n#line 3 \"main.jeti\"\nint a = 1;\nint b = 2;\n#line 1 \"<jeti-generated>\"\nvoid glue(void) {}\nstatic int x;\n#line 7 \"lib.hje\"\nreturn7;\n";
        let m = extract_source_map(c_text, &opts(&[]));
        assert_eq!(m.sources.len(), 2);
        // Sorted by path: lib.hje < main.jeti.
        assert_eq!(m.sources[0].path, "lib.hje");
        assert_eq!(m.sources[0].id, 0);
        assert_eq!(m.sources[1].path, "main.jeti");
        assert_eq!(m.sources[1].id, 1);

        assert_eq!(m.mappings.len(), 3);
        // Region A: C lines 4-5 ← main.jeti 3-4.
        let a = &m.mappings[0];
        assert_eq!(a.c_start_line, 4);
        assert_eq!(a.c_end_line, 5);
        assert_eq!(a.c_start, c_text.find("int a = 1;").unwrap() as u64);
        assert_eq!(
            a.c_end,
            c_text.find("#line 1 \"<jeti-generated>\"").unwrap() as u64
        );
        assert_eq!(a.src_id, Some(1));
        assert_eq!(a.src_line_start, Some(3));
        assert_eq!(a.src_line_end, Some(4));
        assert_eq!(a.c_start_col, 1);
        assert_eq!(a.c_end_col, 11); // "int b = 2;" is 10 bytes → exclusive col 11
        // Region B: synthetic, no src fields.
        let b = &m.mappings[1];
        assert!(b.synthetic);
        assert_eq!(b.kind, "synthetic");
        assert_eq!(b.src_id, None);
        assert_eq!(b.c_start_line, 7);
        assert_eq!(b.c_end_line, 8);
        // Region C: last mapped line reaches EOF including its newline.
        let c = &m.mappings[2];
        assert_eq!(c.src_id, Some(0));
        assert_eq!(c.src_line_start, Some(7));
        assert_eq!(c.src_line_end, Some(7));
        assert_eq!(c.c_start, c_text.find("return7;").unwrap() as u64);
        assert_eq!(c.c_end, c_text.len() as u64);
    }

    #[test]
    fn escaped_path_is_unescaped() {
        let c_text = "x;\n#line 2 \"dir\\\\my \\\"x\\\".jeti\"\ny;\n";
        let m = extract_source_map(c_text, &opts(&[]));
        assert_eq!(m.sources.len(), 1);
        assert_eq!(m.sources[0].path, "dir\\my \"x\".jeti");
        assert_eq!(m.mappings.len(), 1);
        assert_eq!(m.mappings[0].src_id, Some(0));
        // Directive occupies physical line 2, so it numbers line 3 as src 2.
        assert_eq!(m.mappings[0].src_line_start, Some(2));
        assert_eq!(m.mappings[0].src_line_end, Some(2));
    }

    #[test]
    fn directive_inside_comment_or_string_is_ignored() {
        // Block comment spanning lines, string with #line inside, char escape.
        let c_text = "/*\n#line 99 \"fake.jeti\"\n*/\nconst char* s = \"#line 5 \\\"x\\\"\";\nchar c = '\\'';\n// #line 6 \"also.jeti\"\n";
        let m = extract_source_map(c_text, &opts(&[]));
        assert!(m.sources.is_empty());
        assert!(m.mappings.is_empty());
    }

    #[test]
    fn string_continuation_keeps_line_accounting() {
        let c_text = "char* s = \"abc\\\ndef\";\n#line 4 \"m.jeti\"\nx;\n";
        let m = extract_source_map(c_text, &opts(&[]));
        assert_eq!(m.mappings.len(), 1);
        let r = &m.mappings[0];
        assert_eq!(r.c_start_line, 4);
        assert_eq!(r.c_end_line, 4);
        assert_eq!(r.src_line_start, Some(4));
        assert_eq!(r.c_start, c_text.find("x;").unwrap() as u64);
    }

    #[test]
    fn no_directives_yields_empty_map_with_generated_hash() {
        let c_text = "int main(void) { return 0; }\n";
        let m = extract_source_map(c_text, &opts(&[]));
        assert!(m.mappings.is_empty());
        assert!(m.sources.is_empty());
        assert_eq!(m.generated.path, "main.c");
        assert!(m.generated.hash.starts_with("fnv1a64:"));
    }

    #[test]
    fn trailing_directive_without_newline_is_empty_scope() {
        let c_text = "a;\n#line 5 \"m.jeti\"";
        let m = extract_source_map(c_text, &opts(&[]));
        assert!(m.mappings.is_empty());
        assert_eq!(m.sources.len(), 1); // still declared as a source
    }

    #[test]
    fn leading_whitespace_directive_is_not_recognized() {
        // jetic emits column 0 only; indented `#line` stays unparsed.
        let c_text = "  #line 5 \"m.jeti\"\nx;\n";
        let m = extract_source_map(c_text, &opts(&[]));
        assert!(m.mappings.is_empty());
    }

    #[test]
    fn source_hash_prefers_caller_contents() {
        let c_text = "#line 1 \"main.jeti\"\nx;\n";
        let contents = vec![("main.jeti".to_string(), "int x;\n".to_string())];
        let m = extract_source_map(c_text, &opts(&contents));
        assert_eq!(m.sources[0].hash, hash_tag(b"int x;\n"));
    }
}
