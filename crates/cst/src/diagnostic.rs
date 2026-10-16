//! Structured diagnostics shared by every front-end stage.
//!
//! Stages record `Diagnostic` values instead of pre-formatted strings; the
//! renderer (jetic side) turns them into either clang-style annotated source
//! output or plain `file:line:col: message` lines. Rendering lives elsewhere
//! so the recorder never needs source text or colors.

/// Severity of a [`Diagnostic`]. Order matters: errors dominate warnings when
/// callers ask "did anything fail".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
    Note,
}

impl Severity {
    /// Lowercase word used in rendered headers (`error:`, `warning:`).
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

/// One diagnostic: where it points and what it says.
///
/// `col` is the 1-based byte column of the primary caret. `end_col`, when
/// present, is the exclusive end of the annotated range and is rendered as
/// `~~~~^~~~~` (tildes up to the caret, matching clang). Diagnostics recorded
/// before spans were threaded through pass `end_col: None`, which renders a
/// single `^`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// File as reported by the SourceMap (`locate`). Empty for synthesized
    /// positions with no backing file.
    pub file: String,
    /// 1-based source line (already remapped through the SourceMap).
    pub line: usize,
    /// 1-based source column (already remapped through the SourceMap).
    pub col: usize,
    /// Exclusive end column of the annotated span on the same line, if known.
    pub end_col: Option<usize>,
    /// clang-style fix-it: a character sequence to insert at `hint.0` (1-based
    /// column on the primary line), rendered in green under the caret — e.g.
    /// `;` for "expected ';' after expression".
    pub hint: Option<(usize, String)>,
    pub message: String,
}

impl Diagnostic {
    pub fn error(file: impl Into<String>, line: usize, col: usize, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            file: file.into(),
            line,
            col,
            end_col: None,
            hint: None,
            message: message.into(),
        }
    }

    pub fn warning(file: impl Into<String>, line: usize, col: usize, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            file: file.into(),
            line,
            col,
            end_col: None,
            hint: None,
            message: message.into(),
        }
    }

    pub fn note(file: impl Into<String>, line: usize, col: usize, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Note,
            file: file.into(),
            line,
            col,
            end_col: None,
            hint: None,
            message: message.into(),
        }
    }

    /// Builder-style span setter for call sites that know the width.
    pub fn with_end_col(mut self, end_col: usize) -> Self {
        self.end_col = Some(end_col);
        self
    }

    /// Builder-style fix-it: insert `text` at column `col` on the annotated
    /// line, rendered clang-style under the caret.
    pub fn with_hint(mut self, col: usize, text: impl Into<String>) -> Self {
        self.hint = Some((col, text.into()));
        self
    }

    /// Plain `file:line:col: message` form — the historical format scripts
    /// and golden files may grep for. Keeps the `[checker]`-style prefixes
    /// out of the structured layer. A zero line marks a position-less
    /// (synthesized) diagnostic: only the message is rendered.
    pub fn to_plain(&self) -> String {
        if self.line == 0 {
            return self.message.clone();
        }
        if self.file.is_empty() {
            format!("{}:{}: {}", self.line, self.col, self.message)
        } else {
            format!("{}:{}:{}: {}", self.file, self.line, self.col, self.message)
        }
    }

    /// The diagnostic's position as a [`SourceSpan`] (1-based source line, byte
    /// columns, half-open end), so consumers that speak the span model — LSP,
    /// debugger adapters — project it through [`SourceSpan::to_lsp_range`]
    /// instead of re-deriving positions from `line`/`col` in their own units.
    ///
    /// `file` stays `0`: the id only means something to a [`SourceMap`], while
    /// the diagnostic carries the display path the consumer looks the text up
    /// with. A file-wide diagnostic (no file, or line 0) is marked synthetic so
    /// nothing invents a range for it.
    pub fn to_source_span(&self) -> crate::source_map::SourceSpan {
        use crate::source_map::{SourceSpan, SpanOrigin};
        let end_col = match self.end_col {
            Some(end) if end > self.col => end as u32,
            _ => self.col as u32,
        };
        SourceSpan {
            file: 0,
            start_line: self.line as u32,
            start_col: self.col as u32,
            end_line: self.line as u32,
            end_col,
            start_byte: None,
            end_byte: None,
            origin: if self.file.is_empty() || self.line == 0 {
                SpanOrigin::Synthetic
            } else {
                SpanOrigin::Source
            },
        }
    }
}

/// Render a list of diagnostics back to the historical plain lines. Used for
/// the fallback output format and by unit tests.
pub fn render_plain(diags: &[Diagnostic]) -> String {
    diags
        .iter()
        .map(Diagnostic::to_plain)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Look up the full text of a source file by the name a diagnostic carries.
/// Returns `None` when the file cannot be read (rendering then falls back to
/// the plain one-line form for that diagnostic).
pub type SourceLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Render diagnostics clang-style, with the location leading the header line
/// (`file:line:col: severity: message`) and the offending source line with a
/// `~~~~^~~~~` annotation under it:
///
/// ```text
/// foo.jeti:27:14: error: null passed to a callee that requires a non-null argument
///    |
/// 27 |     [g greet:nil];
///    |            ~~~ ^
/// ```
///
/// Diagnostics whose file is empty, or whose file the lookup cannot provide,
/// fall back to the plain `file:line:col:` line so nothing is ever dropped.
pub fn render_annotated(diags: &[Diagnostic], lookup: &SourceLookup) -> String {
    render_annotated_colored(diags, lookup, false)
}

/// Same layout as [`render_annotated`], with ANSI colors when `color` is set:
/// severity word bold red/yellow, caret annotation in the severity color.
/// Callers gate this on stderr being a TTY.
pub fn render_annotated_colored(diags: &[Diagnostic], lookup: &SourceLookup, color: bool) -> String {
    diags
        .iter()
        .map(|d| render_one(d, lookup, color))
        .collect::<Vec<_>>()
        .join("\n")
}

fn severity_color(d: &Diagnostic, color: bool) -> (&'static str, &'static str) {
    if !color {
        ("", "")
    } else {
        match d.severity {
            Severity::Error => ("\x1b[1;31m", "\x1b[0m"),
            Severity::Warning => ("\x1b[1;33m", "\x1b[0m"),
            Severity::Note => ("\x1b[1;36m", "\x1b[0m"),
        }
    }
}

fn render_one(d: &Diagnostic, lookup: &SourceLookup, color: bool) -> String {
    let source = if d.file.is_empty() {
        None
    } else {
        lookup(&d.file)
    };
    let Some(text) = source else {
        return d.to_plain();
    };
    let Some(line_text) = text.lines().nth(d.line.checked_sub(1).unwrap_or(usize::MAX)) else {
        return d.to_plain();
    };

    let (s_on, s_off) = severity_color(d, color);
    // clang renders the caret/tilde annotation in green regardless of severity.
    let (c_on, c_off) = if color { ("\x1b[32m", "\x1b[0m") } else { ("", "") };
    // clang's header splits coloring: the location is bold, the severity word
    // bold+severity color, the message bold again (`\x1b[1m` ... `\x1b[0m`).
    let (b_on, b_off) = if color { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
    let ln = d.line.to_string();
    // clang's gutter: the line number is right-aligned in a column of width
    // max(len+1, 5) ("    3 |", " 1000 |"); the caret lines under it carry one
    // extra leading space so their `|` lines up with the source line's.
    let gutter_pad = (ln.len() + 1).max(5);
    let gutter = " ".repeat(gutter_pad - ln.len());
    let blank = " ".repeat(gutter_pad + 1);
    let mut out = String::new();
    out.push_str(&format!(
        "{b_on}{f}:{l}:{c}: {b_off}{s_on}{sev}: {s_off}{b_on}{msg}{b_off}\n{gutter}{ln} | {src}\n{blank}| ",
        f = d.file,
        l = d.line,
        c = d.col,
        sev = d.severity.as_str(),
        msg = d.message,
        src = &highlight_source_line(line_text, color),
    ));
    // Bytes before the caret render as spaces; multi-byte source content can
    // desync byte columns from display columns, but Jeti source is effectively
    // ASCII on code lines — revisit with display width if that changes.
    let col = d.col.max(1);
    out.push_str(&" ".repeat((col - 1).min(line_text.len())));
    match d.end_col {
        Some(end) if end > col + 1 => {
            // clang shape: caret at the primary column, tildes through the
            // rest of the span.
            let width = (end - col).min(line_text.len().saturating_sub(col - 1)).max(1);
            out.push_str(&format!("{}^{}{}", c_on, "~".repeat(width - 1), c_off));
        }
        _ => {
            // No recorded width: underline the token under the caret when the
            // caret sits at its start (previous char is not identifier-ish) —
            // `nil`, identifiers, keywords. Mid-token carets keep a plain ^.
            let b = line_text.as_bytes();
            let idx = (col - 1).min(b.len());
            let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
            let mut width = 1usize;
            if idx < b.len() && (is_word(b[idx]) || b[idx] == b'@')
                && (idx == 0 || !is_word(b[idx - 1]))
            {
                let mut j = idx + 1;
                if b[idx] == b'@' && j < b.len() && b[j] == b'(' {
                    // `@(...)` boxed expression: extend through the balanced
                    // closing paren so the annotation covers the whole expression.
                    let mut depth = 0usize;
                    while j < b.len() {
                        match b[j] {
                            b'(' => depth += 1,
                            b')' => {
                                depth -= 1;
                                if depth == 0 {
                                    j += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    width = j - idx;
                } else {
                    while j < b.len() && is_word(b[j]) { j += 1; }
                    width = j - idx;
                }
            }
            if width > 1 {
                out.push_str(&format!("{}^{}{}", c_on, "~".repeat(width - 1), c_off));
            } else {
                out.push_str(&format!("{}^{}", c_on, c_off));
            }
        }
    }
    // clang-style fix-it: the insertion rendered in green, on its own line,
    // aligned under the insertion column (`;` at the caret, like clang's
    // "expected ';' after expression" fix-it).
    if let Some((hcol, htext)) = &d.hint {
        let hcol = (*hcol).max(1);
        out.push('\n');
        out.push_str(&format!("{}| {}{}{}{}", blank, " ".repeat(hcol - 1), c_on, htext, c_off));
    }
    out
}

/// Minimal clang-style syntax coloring for a single source line shown in a
/// diagnostic: strings/chars green, comments dim, keywords and the `@`
/// jeti prefixes blue, numbers magenta. Not a lexer — a byte scan good enough
/// for display only; when `color` is off the line passes through untouched.
fn highlight_source_line(line: &str, color: bool) -> String {
    if !color || line.is_empty() {
        return line.to_string();
    }
    const KEYWORDS: &[&str] = &[
        "int", "char", "float", "double", "void", "long", "short", "signed", "unsigned",
        "const", "static", "struct", "union", "enum", "typedef", "sizeof", "return",
        "if", "else", "while", "for", "do", "switch", "case", "default", "break",
        "continue", "goto", "nil", "NULL", "BOOL", "id", "instancetype", "SEL", "self",
        "super", "YES", "NO",
    ];
    const RESET: &str = "\x1b[0m";
    const GREEN: &str = "\x1b[32m";
    const BLUE: &str = "\x1b[34m";
    const MAGENTA: &str = "\x1b[35m";
    const DIM: &str = "\x1b[2m";

    let b = line.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = String::with_capacity(line.len() + 32);
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            // String/char literal: green, through the closing quote (no escape
            // tracking needed for display — a trailing quote still colors fine).
            let quote = c;
            let start = i;
            i += 1;
            while i < b.len() && b[i] != quote {
                if b[i] == b'\\' && i + 1 < b.len() { i += 1; }
                i += 1;
            }
            if i < b.len() { i += 1; }
            out.push_str(GREEN);
            out.push_str(&line[start..i]);
            out.push_str(RESET);
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            // Line comment: dim to end of line.
            out.push_str(DIM);
            out.push_str(&line[i..]);
            out.push_str(RESET);
            i = b.len();
        } else if c == b'@' && i + 1 < b.len() && (b[i + 1] == b'"' || b[i + 1] == b'(') {
            // `@"..."` / `@(...)`: the prefix in blue, then re-scan the payload
            // naturally (the string branch handles `@"..."` on the next pass).
            out.push_str(BLUE);
            out.push('@');
            out.push_str(RESET);
            i += 1;
        } else if is_word(c) {
            let start = i;
            while i < b.len() && is_word(b[i]) { i += 1; }
            let word = &line[start..i];
            if KEYWORDS.contains(&word) {
                out.push_str(BLUE);
                out.push_str(word);
                out.push_str(RESET);
            } else if word.bytes().all(|w| w.is_ascii_digit()) {
                out.push_str(MAGENTA);
                out.push_str(word);
                out.push_str(RESET);
            } else {
                out.push_str(word);
            }
        } else {
            // Single byte, no coloring — pass through as a &str slice so
            // multi-byte UTF-8 bytes are never reinterpreted as lone chars.
            let start = i;
            i += 1;
            out.push_str(&line[start..i]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_format_matches_historical_layout() {
        let d = Diagnostic::error("foo.jeti", 27, 14, "boom");
        assert_eq!(d.to_plain(), "foo.jeti:27:14: boom");
        let d = Diagnostic::warning(String::new(), 3, 5, "synthetic");
        assert_eq!(d.to_plain(), "3:5: synthetic");
    }

    #[test]
    fn plain_lines_join_in_order() {
        let ds = vec![
            Diagnostic::error("a.jeti", 1, 1, "one"),
            Diagnostic::warning("a.jeti", 2, 1, "two"),
        ];
        assert_eq!(render_plain(&ds), "a.jeti:1:1: one\na.jeti:2:1: two");
    }

    fn lookup_ok(file: &str) -> Option<String> {
        if file == "foo.jeti" {
            Some("int main(void) {\n    [g greet:nil];\n    return 0;\n}\n".into())
        } else {
            None
        }
    }

    #[test]
    fn annotated_renders_caret_with_gutter() {
        let d = Diagnostic::error("foo.jeti", 2, 9, "boom");
        let out = render_annotated(&[d], &lookup_ok);
        assert_eq!(
            out,
            "foo.jeti:2:9: error: boom\n    2 |     [g greet:nil];\n      |         ^"
        );
    }

    #[test]
    fn annotated_renders_clang_tilde_span() {
        // span columns 13..18 → caret at 13, tildes through 17
        let d = Diagnostic::error("foo.jeti", 2, 13, "bad arg").with_end_col(18);
        let out = render_annotated(&[d], &lookup_ok);
        let line = out.lines().last().unwrap();
        assert_eq!(line, "      |             ^~~~~");
    }

    #[test]
    fn annotated_falls_back_to_plain_when_file_unreadable() {
        let d = Diagnostic::error("missing.jeti", 4, 7, "nope");
        let out = render_annotated(&[d], &lookup_ok);
        assert_eq!(out, "missing.jeti:4:7: nope");
    }

    #[test]
    fn annotated_falls_back_when_line_out_of_range() {
        let d = Diagnostic::error("foo.jeti", 99, 3, "beyond");
        let out = render_annotated(&[d], &lookup_ok);
        assert_eq!(out, "foo.jeti:99:3: beyond");
    }
}
