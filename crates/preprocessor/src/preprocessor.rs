use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use jeti_cst::{ColOrigin, ColumnRegion, SourceMap, SourceRegistry};

use jeti_cpp::{self as cpp, MacroDef};

pub struct Preprocessor {
    pub resolved_jeti: String,
    pub c_headers: Vec<String>,
    /// Maps each line of `resolved_jeti` back to (file, source line).
    pub source_map: SourceMap,
}

impl Preprocessor {
    pub fn new() -> Self {
        Preprocessor {
            resolved_jeti: String::new(),
            c_headers: Vec::new(),
            source_map: SourceMap::new(Vec::new()),
        }
    }
}

/// Check if a line is an #include or #import directive and extract the header name.
fn is_directive(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let after_hash = trimmed[1..].trim_start();
    let is_import = if after_hash.starts_with("import") { true }
    else if after_hash.starts_with("include") { false }
    else { return None };

    let body = if is_import {
        &after_hash["import".len()..]
    } else {
        &after_hash["include".len()..]
    };
    let body = body.trim();

    let end_char = if body.starts_with('<') {
        '>'
    } else if body.starts_with('"') {
        '"'
    } else {
        return None;
    };

    let body = &body[1..]; // skip opening < or "
    let end = body.find(end_char)?;
    let name = body[..end].to_string();

    // For #import: only treat as a jeti import if the file is a jeti header
    // (.hje) or jeti implementation (.jeti).
    let is_jeti_import = if is_import {
        let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("");
        ext == "hje" || ext == "jeti"
    } else {
        false
    };

    Some((is_jeti_import, name))
}

/// Try to open a file, searching through multiple directories.
fn try_open(name: &str, search_dirs: &[String]) -> Option<String> {
    for dir in search_dirs {
        let path = format!("{}/{}", dir, name);
        if let Ok(content) = fs::read_to_string(&path) {
            return Some(content);
        }
    }
    // Also try the raw name
    fs::read_to_string(name).ok()
}

/// A conditional-block frame on the `cond_stack`.
///
/// `active` is whether this specific branch emits, `any_met` whether an
/// earlier branch in the same chain was taken. For a *singleton self-guard*
/// (`#ifndef NAME` … `#define NAME` … `#endif` with nothing else inside) we
/// additionally remember the guard name and the define line so the guard can
/// be preserved verbatim in the C output. This lets the C compiler respect a
/// pre-existing definition of `NAME` (e.g. `jeti/runtime.h`'s `YES`/`NO`)
/// instead of redefining the macro (`-Wmacro-redefined`).
struct CondFrame {
    active: bool,
    any_met: bool,
    /// Some(NAME) while this frame is still a singleton-self-guard candidate.
    /// Cleared as soon as anything else (other content, `#else`, another
    /// define, …) appears inside the block.
    guard_name: Option<String>,
    /// The `#define NAME` line stashed while we wait for the matching `#endif`.
    pending_define: Option<String>,
    /// Set once the block contains anything other than the guarded define —
    /// disables preservation (behaves like a condition that is flattened away).
    /// True once `#else` was seen: a later `#elif`/`#else` is ill-formed
    /// (§6.10.1p6 allows `#else` only as the last branch).
    else_seen: bool,
    dirty: bool,
}

/// A guard candidate that turned out not to be a pure singleton self-guard
/// is flushed back to the plain flattened behaviour: the definition is emitted
/// unconditionally (as before) and the guard directives stay dropped.
fn poison_top_guard(cond_stack: &mut Vec<CondFrame>, c_out: &mut Vec<String>) {
    if let Some(frame) = cond_stack.last_mut() {
        if frame.guard_name.is_some() || frame.pending_define.is_some() {
            frame.dirty = true;
            let guard = frame.guard_name.take();
            if let Some(def) = frame.pending_define.take() {
                // A guard candidate turns out to have content, so the guard
                // directives are flattened away — but the companion
                // `#define NAME` of `#ifndef NAME` must NOT be re-emitted bare.
                // It is a marker, not a macro: on its own it makes a *re-emitted*
                // guarded definition (`#ifndef NAME` around the same struct)
                // skip itself and leave an incomplete type. A valued define is a
                // real macro and still comes through.
                let own_marker = guard
                    .as_deref()
                    .is_some_and(|g| def.trim() == format!("#define {}", g));
                if !own_marker {
                    c_out.push(def.trim().to_string());
                }
            }
        }
    }
}

/// Split an `#if` directive head from its controlling expression. `#if 1`,
/// `#if(1)` and `#if!defined(X)` are all valid C (§6.10.1), so only an
/// identifier character may *not* follow `if` — that case (`#iffy`) is not a
/// conditional directive at all. `#ifdef`/`#ifndef` are matched first.
fn strip_if_directive(after: &str) -> Option<&str> {
    let rest = after.strip_prefix("if")?;
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if !(c.is_ascii_alphanumeric() || c == '_') => Some(rest),
        _ => None,
    }
}

/// Recursively resolve #import and collect #include from a single file's content.
fn resolve_source(
    content: &str,
    file_path: &str,
    search_dirs: &[String],
    resolved: &mut HashSet<String>,
    jeti_out: &mut String,
    c_out: &mut Vec<String>,
    defined: &mut HashSet<String>,
    jeti_macros: &mut HashMap<String, MacroDef>,
    cond_stack: &mut Vec<CondFrame>,
    line_map: &mut Vec<(String, u32)>,
    registry: &mut SourceRegistry,
) -> Result<(), String> {
    // Register this file with its real text (line-start table for byte-offset
    // mapping, plan 阶段 1). Dedup by path.
    if registry.id_of(file_path).is_none() {
        registry.add(file_path, content);
    }
    // Conditional depth this file starts at: an `#import` splices a file into
    // the middle of the including file's stream, so only the conditionals the
    // file itself opens may be closed by it.
    let entry_depth = cond_stack.len();
    let dir = Path::new(file_path).parent()
        .and_then(|p| p.to_str())
        .unwrap_or(".")
        .to_string();

    // Join backslash continuations into logical lines first: each logical
    // line keeps the (file, line) of its FIRST physical line. A trailing `\`
    // splices the next line's text (stripped) onto this one.
    let mut logical: Vec<(String, u32)> = Vec::new();
    {
        let mut pending: Option<(String, u32)> = None;
        for (line_idx, raw) in content.lines().enumerate() {
            let src_line = (line_idx + 1) as u32;
            match pending.take() {
                Some((mut acc, first)) => {
                    let raw = raw.trim();
                    if raw.ends_with('\\') {
                        acc.push_str(raw.trim_end_matches('\\').trim_end());
                        pending = Some((acc, first));
                    } else {
                        acc.push_str(raw);
                        logical.push((acc, first));
                    }
                }
                None => {
                    let t = raw.trim_end();
                    if t.ends_with('\\') {
                        pending = Some((t.trim_end_matches('\\').trim_end().to_string(), src_line));
                    } else {
                        logical.push((raw.to_string(), src_line));
                    }
                }
            }
        }
        // Unterminated continuation: flush what we have.
        if let Some((acc, first)) = pending {
            logical.push((acc, first));
        }
    }

    for (line, src_line) in &logical {
        let line = line.as_str();
        let src_line = *src_line;
        let trimmed = line.trim();
        // `active` = all blocks (including current) are emitting code.
        let active = cond_stack.iter().all(|f| f.active);
        // `parent_active` = all blocks except the innermost are emitting.
        // Used to decide #elif/#else (which replace the current branch), so the
        // current branch's own inactive state must not suppress re-evaluation.
        let parent_active = cond_stack[..cond_stack.len().saturating_sub(1)]
            .iter().all(|f| f.active);

        // Conditional directives: #ifdef / #ifndef / #if / #elif / #else / #endif
        if trimmed.starts_with('#') {
            let after = trimmed[1..].trim_start();
            if let Some(rest) = after.strip_prefix("ifdef") {
                let name = rest.trim().split(|c: char| c.is_whitespace() || c == '(' || c == ')')
                    .next().unwrap_or("").to_string();
                let truthy = parent_active && (defined.contains(&name) || predefined_macro(&name));
                cond_stack.push(CondFrame { active: truthy, any_met: truthy, guard_name: None, pending_define: None, else_seen: false, dirty: false });
                continue;
            } else if let Some(rest) = after.strip_prefix("ifndef") {
                let name = rest.trim().split(|c: char| c.is_whitespace() || c == '(' || c == ')')
                    .next().unwrap_or("").to_string();
                let truthy = parent_active && !(defined.contains(&name) || predefined_macro(&name));
                cond_stack.push(CondFrame {
                    active: truthy, any_met: truthy,
                    // Only an active singleton `#ifndef` can be preserved.
                    guard_name: if truthy { Some(name) } else { None },
                    pending_define: None, else_seen: false, dirty: false,
                });
                continue;
            } else if let Some(rest) = strip_if_directive(after) {
                let expr = rest.trim();
                let truthy = parent_active && eval_if_expr(expr, defined, jeti_macros);
                cond_stack.push(CondFrame { active: truthy, any_met: truthy, guard_name: None, pending_define: None, else_seen: false, dirty: false });
                continue;
            } else if after.starts_with("elif") {
                // An `#elif` inside a self-guard candidate forces flattening.
                poison_top_guard(cond_stack, c_out);
                let frame = match cond_stack.last_mut() {
                    None => return Err(format!("{}:{}: #elif without matching #if", file_path, src_line)),
                    Some(frame) => frame,
                };
                if frame.else_seen {
                    return Err(format!("{}:{}: #elif after #else", file_path, src_line));
                }
                frame.dirty = true;
                frame.guard_name = None;
                if frame.any_met {
                    frame.active = false;
                } else {
                    let rest = after["elif".len()..].trim();
                    let expr = rest.strip_prefix("if ").or_else(|| rest.strip_prefix("if\t")).unwrap_or(rest);
                    let result = parent_active && eval_if_expr(expr.trim(), defined, jeti_macros);
                    frame.active = result;
                    frame.any_met = result;
                }
                continue;
            } else if after.starts_with("else") {
                // An `#else` inside a self-guard candidate forces flattening.
                poison_top_guard(cond_stack, c_out);
                let frame = match cond_stack.last_mut() {
                    None => return Err(format!("{}:{}: #else without matching #if", file_path, src_line)),
                    Some(frame) => frame,
                };
                if frame.else_seen {
                    return Err(format!("{}:{}: #else after #else", file_path, src_line));
                }
                frame.else_seen = true;
                frame.dirty = true;
                frame.guard_name = None;
                frame.active = !frame.any_met;
                continue;
            } else if after.starts_with("endif") {
                if let Some(frame) = cond_stack.pop() {
                    if let Some(name) = frame.guard_name {
                        if !frame.dirty {
                            // Pure singleton self-guard: preserve it so the C
                            // compiler honours a definition seen earlier
                            // (e.g. jeti/runtime.h's `YES`/`NO`).
                            if let Some(def) = frame.pending_define {
                                c_out.push(format!("#ifndef {}", name));
                                c_out.push(def.trim().to_string());
                                c_out.push("#endif".to_string());
                            }
                        } else if let Some(def) = frame.pending_define {
                            c_out.push(def.trim().to_string());
                        }
                    } else if let Some(def) = frame.pending_define {
                        c_out.push(def.trim().to_string());
                    }
                } else {
                    return Err(format!("{}:{}: #endif without matching #if", file_path, src_line));
                }
                continue;
            }
        }

        // Only process non-directive and active sections past this point. A
        // skipped group (§6.10.1p6) is not processed at all: conditionals it
        // opens were handled above, and every other line — including a
        // `#define`, which must therefore not take effect — is dropped.
        if !active {
            continue;
        }

        if let Some((is_jeti_import, name)) = is_directive(line) {
            // Anything inside a guard candidate besides the define it guards
            // forces the flat (non-preserved) emission.
            if is_jeti_import {
                poison_top_guard(cond_stack, c_out);
                // #import of .hje/.jeti (or .jeti) → recursively resolve
                let mut search = search_dirs.to_vec();
                // Add source directory first
                if !search.contains(&dir) {
                    search.insert(0, dir.clone());
                }
                resolve_imports(&name, &search, resolved, jeti_out, c_out, defined, jeti_macros, cond_stack, line_map, registry)?;
            } else {
                poison_top_guard(cond_stack, c_out);
                // #include → collect for C output (verbatim)
                let orig = line.trim().to_string();
                if !c_out.contains(&orig) {
                    c_out.push(orig);
                }
            }
        } else if line.trim_start().starts_with('#') {
            // Preprocessor directives: #define, #pragma, etc.
            if let Some(rest) = trimmed.strip_prefix("#define") {
                // Continuation lines (\) have already been joined below, so
                // `line` here may span several source lines.
                let name = rest.trim().split_whitespace().next().unwrap_or("").to_string();
                if !name.is_empty() { defined.insert(name.clone()); }
                // Record EVERY well-formed define in the jeti macro table:
                // jetic expands all invocations itself (ISO 9899 §6.10.3
                // replacement). A single expander for plain-C and jeti-syntax
                // bodies avoids track-classification hazards (a C-track call
                // whose argument contains a jeti macro, forward references
                // between macros, ...). The definition line still passes to C
                // below, so externally linked C code sees it too — harmless,
                // because expanded text contains no macro names, making
                // clang's own expansion a no-op.
                let parsed = cpp::parse_define(rest);
                if let Some(def) = parsed.clone() {
                    jeti_macros.insert(def.name.clone(), def);
                }
                // Except: a body with jeti syntax (`@"..."` boxed string,
                // message send, block literal) must NOT reach the C prelude —
                // `#define X @"..."` is not plain C, so an expansion in
                // hand-written C would be a hard clang error. jetic expands
                // every invocation on the jeti track, so the C track dropping
                // the line loses nothing. Plain-C bodies still pass through.
                let c_track_ok = parsed.as_ref().map_or(true, |d| !cpp::body_has_jeti_syntax(&d.body));
                // Singleton self-guard candidate: `#ifndef NAME` guarding
                // exactly `#define NAME …` — stash the define so the guard can
                // be preserved in the C output (avoids macro redefinition when
                // the macro was already defined by an included header).
                let is_guarded_define = cond_stack.last().map_or(false, |f| {
                    f.guard_name.as_deref() == Some(name.as_str())
                        && f.pending_define.is_none()
                        && !f.dirty
                });
                if is_guarded_define {
                    if c_track_ok {
                        if let Some(frame) = cond_stack.last_mut() {
                            frame.pending_define = Some(line.to_string());
                        }
                    }
                    // jeti-syntax body: not stashed — the whole singleton
                    // guard vanishes from the C output (the guard wraps
                    // nothing else, so there is nothing left to preserve).
                } else {
                    poison_top_guard(cond_stack, c_out);
                    if c_track_ok {
                        let orig = line.to_string();
                        c_out.push(orig.trim().to_string());
                    }
                }
            } else if let Some(rest) = trimmed.strip_prefix("#undef") {
                // #undef removes the name from both tracks: the jeti macro
                // table (so later invocations no longer expand) and the
                // defined set (so #ifdef flips), then passes through to C
                // in case a same-named C macro exists (§6.10.3.5).
                let name = rest.trim().split_whitespace().next().unwrap_or("").to_string();
                if !name.is_empty() {
                    defined.remove(&name);
                    jeti_macros.remove(&name);
                }
                poison_top_guard(cond_stack, c_out);
                c_out.push(trimmed.to_string());
            } else if trimmed.starts_with("#pragma mark") {
                // `#pragma mark ...` is a purely cosmetic IDE marker (Xcode
                // navigator). Keep it inline in the jeti stream so the parser
                // carries it at its original position and codegen re-emits it
                // there. Hoisting it into the C prelude would collect every
                // marker at the top of the generated file. Other `#pragma`
                // directives still go to the prelude (they typically must
                // precede the code they affect).
                poison_top_guard(cond_stack, c_out);
                jeti_out.push_str(line);
                jeti_out.push('\n');
                line_map.push((file_path.to_string(), src_line));
            } else {
                // #pragma (non-mark), #warning, etc.
                poison_top_guard(cond_stack, c_out);
                let orig = line.to_string();
                c_out.push(orig.trim().to_string());
            }
        } else {
            // Regular jeti source line. Blank lines and `//` comments are
            // inert — they do not break a pure singleton self-guard.
            let non_inert = !(trimmed.is_empty() || trimmed.starts_with("//"));
            if non_inert {
                poison_top_guard(cond_stack, c_out);
            }
            jeti_out.push_str(line);
            jeti_out.push('\n');
            line_map.push((file_path.to_string(), src_line));
        }
    }

    // §6.10.1p6: every `#if` needs its `#endif`. An unclosed one would silently
    // swallow the rest of the file — the guard directives never reach clang, so
    // nothing downstream could notice — hence the explicit error.
    if cond_stack.len() > entry_depth {
        return Err(format!("{}: unterminated conditional directive — {} #if without #endif",
            file_path, cond_stack.len() - entry_depth));
    }
    Ok(())
}

/// Predefined macros (target platform/compiler). jetic always emits C that is
/// compiled by clang (or gcc via zig), so `__clang__`/`__GNUC__` are defined.
/// `_WIN32`: the preprocessor resolves `#ifdef` at flatten time, before any C
/// compiler runs — define it when jetic itself runs on Windows, or when the
/// target is forced there with `JETIC_WINDOWS=1` (zig-cc cross builds, and
/// local verification of Win32-only source paths).
fn predefined_macro(name: &str) -> bool {
    matches!(name,
        "__APPLE__" | "__MACH__" | "__LP64__" | "__x86_64__" | "__aarch64__" | "__amd64__")
        || (name == "_WIN32"
            && (cfg!(windows) || std::env::var_os("JETIC_WINDOWS").is_some()))
}

/// Evaluate a `#if`/`#elif` expression. Per the standard (ISO 9899 §6.10.1p4)
/// the expression is fully macro-expanded first, except that operands of
/// `defined` are exempt. Implementation: resolve every `defined(X)` /
/// `defined X` to a literal 1/0 *before* expansion (so expansion cannot touch
/// them), then expand remaining macros, then evaluate the arithmetic.
fn eval_if_expr(expr: &str, defined: &HashSet<String>, macros: &HashMap<String, MacroDef>) -> bool {
    let shielded = resolve_defined(expr, defined);
    let expanded = cpp::expand(&shielded, macros).unwrap_or_else(|_| shielded.clone());
    eval_if_expr_inner(&expanded, defined)
}

/// Replace `defined(X)` / `defined X` with `1` or `0`. Doing this before
/// macro expansion both exempts the operands from expansion (§6.10.1) and
/// keeps the evaluator free of placeholder plumbing.
fn resolve_defined(expr: &str, defined: &HashSet<String>) -> String {
    let mut out = String::with_capacity(expr.len());
    let b = expr.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"defined") {
            let before_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
            let mut j = i + 7;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') { j += 1; }
            if before_ok && j < b.len() && (b[j] == b'(' || b[j].is_ascii_alphabetic() || b[j] == b'_') {
                let (name, next) = if b[j] == b'(' {
                    let close = match expr[j..].find(')') { Some(k) => j + k, None => break };
                    (expr[j + 1..close].trim().to_string(), close + 1)
                } else {
                    let s = j;
                    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') { j += 1; }
                    (expr[s..j].to_string(), j)
                };
                let truthy = defined.contains(&name) || predefined_macro(&name);
                out.push_str(if truthy { "1" } else { "0" });
                i = next;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// Evaluate the already-expanded `#if` expression (`defined` operands were
/// resolved to 1/0 and macros expanded by `eval_if_expr`). Tokenizes and
/// evaluates with full C operator precedence; a malformed condition evaluates
/// to false instead of panicking (the old char-slicing evaluator panicked on
/// `3 > 2` and mis-evaluated every comparison as `l == r` on bools).
fn eval_if_expr_inner(expr: &str, defined: &HashSet<String>) -> bool {
    eval_const_expr(expr, defined).map(|v| v != 0).unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq)]
enum IfTok {
    Num(i64),
    Ident(String),
    Punct(&'static str),
    End,
}

/// Tokenize a `#if` constant expression: integer literals (decimal / hex /
/// octal / binary, uUlL suffixes, ` digit separators), character constants,
/// identifiers, and operators.
fn if_tokenize(s: &str) -> Result<Vec<IfTok>, String> {
    let b = s.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let (radix, dig_start) = if c == b'0' && i + 1 < b.len() && (b[i + 1] | 32) == b'x' {
                (16u32, i + 2)
            } else if c == b'0' && i + 1 < b.len() && (b[i + 1] | 32) == b'b' {
                (2, i + 2)
            } else if c == b'0' {
                (8, i + 1)
            } else {
                (10, i)
            };
            let mut j = dig_start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let digits: String = s[dig_start..j].chars().filter(|ch| *ch != '_').collect();
            let digits = digits.trim_end_matches(|ch| matches!(ch, 'u' | 'U' | 'l' | 'L'));
            let val = if digits.is_empty() {
                0
            } else {
                i64::from_str_radix(digits, radix)
                    .map_err(|_| format!("invalid integer literal `{}` in #if", &s[i..j]))?
            };
            toks.push(IfTok::Num(val));
            i = j;
            continue;
        }
        if c == b'\'' {
            // Character constant: 'a', '\n', ...
            let mut j = i + 1;
            let mut val: i64 = 0;
            while j < b.len() && b[j] != b'\'' {
                val = if b[j] == b'\\' && j + 1 < b.len() {
                    j += 1;
                    match b[j] {
                        b'n' => 10,
                        b't' => 9,
                        b'r' => 13,
                        b'0' => 0,
                        b'\\' => 92,
                        b'\'' => 39,
                        b'"' => 34,
                        other => other as i64,
                    }
                } else {
                    b[j] as i64
                };
                j += 1;
            }
            if j >= b.len() {
                return Err("unterminated character constant in #if".into());
            }
            toks.push(IfTok::Num(val));
            i = j + 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(IfTok::Ident(s[start..i].to_string()));
            continue;
        }
        // Two-char operators first, then single-char.
        let two: Option<&'static str> = if i + 1 < b.len() {
            match &s[i..i + 2] {
                "||" => Some("||"),
                "&&" => Some("&&"),
                "==" => Some("=="),
                "!=" => Some("!="),
                "<=" => Some("<="),
                ">=" => Some(">="),
                "<<" => Some("<<"),
                ">>" => Some(">>"),
                _ => None,
            }
        } else {
            None
        };
        if let Some(p) = two {
            toks.push(IfTok::Punct(p));
            i += 2;
            continue;
        }
        let one: Option<&'static str> = match c {
            b'(' => Some("("),
            b')' => Some(")"),
            b'?' => Some("?"),
            b':' => Some(":"),
            b'|' => Some("|"),
            b'^' => Some("^"),
            b'&' => Some("&"),
            b'<' => Some("<"),
            b'>' => Some(">"),
            b'+' => Some("+"),
            b'-' => Some("-"),
            b'*' => Some("*"),
            b'/' => Some("/"),
            b'%' => Some("%"),
            b'!' => Some("!"),
            b'~' => Some("~"),
            _ => None,
        };
        match one {
            Some(p) => {
                toks.push(IfTok::Punct(p));
                i += 1;
            }
            None => return Err(format!("unexpected character `{}` in #if", c as char)),
        }
    }
    Ok(toks)
}

struct IfParser<'a> {
    toks: &'a [IfTok],
    pos: usize,
    defined: &'a HashSet<String>,
}

impl<'a> IfParser<'a> {
    fn peek(&self) -> &'a IfTok {
        self.toks.get(self.pos).unwrap_or(&IfTok::End)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if let IfTok::Punct(q) = self.peek() {
            if *q == p {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn expect_punct(&mut self, p: &str) -> Result<(), String> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err(format!("expected `{}` in #if expression", p))
        }
    }

    /// conditional-expression (ternary, right-assoc) — lowest precedence
    fn parse_cond(&mut self) -> Result<i64, String> {
        let cond = self.parse_binary(1)?;
        if self.eat_punct("?") {
            let then_v = self.parse_cond()?;
            self.expect_punct(":")?;
            let else_v = self.parse_cond()?;
            return Ok(if cond != 0 { then_v } else { else_v });
        }
        Ok(cond)
    }

    /// precedence-climbing binary parser following the C precedence table
    fn parse_binary(&mut self, min_prec: u8) -> Result<i64, String> {
        let mut lhs = self.parse_unary()?;
        loop {
            let (op, prec) = match self.peek() {
                IfTok::Punct(p) => match *p {
                    "||" => ("||", 1u8),
                    "&&" => ("&&", 2),
                    "|" => ("|", 3),
                    "^" => ("^", 4),
                    "&" => ("&", 5),
                    "==" => ("==", 6),
                    "!=" => ("!=", 6),
                    "<" => ("<", 7),
                    ">" => (">", 7),
                    "<=" => ("<=", 7),
                    ">=" => (">=", 7),
                    "<<" => ("<<", 8),
                    ">>" => (">>", 8),
                    "+" => ("+", 9),
                    "-" => ("-", 9),
                    "*" => ("*", 10),
                    "/" => ("/", 10),
                    "%" => ("%", 10),
                    _ => break,
                },
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.pos += 1;
            let rhs = self.parse_binary(prec + 1)?;
            lhs = apply_if_binop(op, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<i64, String> {
        match self.peek().clone() {
            IfTok::Punct("!") => {
                self.pos += 1;
                let v = self.parse_unary()?;
                Ok((v == 0) as i64)
            }
            IfTok::Punct("~") => {
                self.pos += 1;
                Ok(!self.parse_unary()?)
            }
            IfTok::Punct("-") => {
                self.pos += 1;
                Ok(self.parse_unary()?.wrapping_neg())
            }
            IfTok::Punct("+") => {
                self.pos += 1;
                self.parse_unary()
            }
            IfTok::Punct("(") => {
                self.pos += 1;
                let v = self.parse_cond()?;
                self.expect_punct(")")?;
                Ok(v)
            }
            IfTok::Num(n) => {
                self.pos += 1;
                Ok(n)
            }
            IfTok::Ident(name) => {
                self.pos += 1;
                // Remaining identifiers: declared/predefined macro names count
                // as 1 (platform probes like `#if __APPLE__` — jeti records
                // their names but not values); anything else is 0 per the
                // standard's "replaced by 0" rule.
                Ok((self.defined.contains(&name) || predefined_macro(&name)) as i64)
            }
            other => Err(format!("unexpected {:?} in #if expression", other)),
        }
    }
}

fn apply_if_binop(op: &str, l: i64, r: i64) -> Result<i64, String> {
    Ok(match op {
        "||" => ((l != 0) || (r != 0)) as i64,
        "&&" => ((l != 0) && (r != 0)) as i64,
        "|" => l | r,
        "^" => l ^ r,
        "&" => l & r,
        "==" => (l == r) as i64,
        "!=" => (l != r) as i64,
        "<" => (l < r) as i64,
        ">" => (l > r) as i64,
        "<=" => (l <= r) as i64,
        ">=" => (l >= r) as i64,
        "<<" => l.wrapping_shl(r as u32),
        ">>" => l.wrapping_shr(r as u32),
        "+" => l.wrapping_add(r),
        "-" => l.wrapping_sub(r),
        "*" => l.wrapping_mul(r),
        "/" => {
            if r == 0 {
                return Err("division by zero in #if".into());
            }
            l.wrapping_div(r)
        }
        "%" => {
            if r == 0 {
                return Err("modulo by zero in #if".into());
            }
            l.wrapping_rem(r)
        }
        _ => return Err(format!("unknown #if operator `{}`", op)),
    })
}

fn eval_const_expr(expr: &str, defined: &HashSet<String>) -> Result<i64, String> {
    let toks = if_tokenize(expr)?;
    let mut p = IfParser { toks: &toks, pos: 0, defined };
    let v = p.parse_cond()?;
    if p.pos != toks.len() {
        return Err("trailing tokens in #if expression".into());
    }
    Ok(v)
}



/// Open a file and resolve its imports.
fn resolve_imports(
    name: &str,
    search_dirs: &[String],
    resolved: &mut HashSet<String>,
    jeti_out: &mut String,
    c_out: &mut Vec<String>,
    defined: &mut HashSet<String>,
    jeti_macros: &mut HashMap<String, MacroDef>,
    cond_stack: &mut Vec<CondFrame>,
    line_map: &mut Vec<(String, u32)>,
    registry: &mut SourceRegistry,
) -> Result<(), String> {
    // Try to find the file
    let content = try_open(name, search_dirs)
        .ok_or_else(|| format!("cannot open import: {}", name))?;

    // Resolve the full path for dedup
    let full_path = search_dirs.iter()
        .map(|d| format!("{}/{}", d, name))
        .find(|p| Path::new(p).exists())
        .unwrap_or_else(|| name.to_string());

    // Dedup: skip if already imported
    if resolved.contains(&full_path) {
        return Ok(());
    }
    resolved.insert(full_path.clone());

    resolve_source(&content, &full_path, search_dirs, resolved, jeti_out, c_out, defined, jeti_macros, cond_stack, line_map, registry)
}

impl Preprocessor {
    /// Process a .jeti source file: resolve imports, collect headers.
    pub fn process_file(input_path: &str, search_dirs: &[String], extra_macros: &[&str]) -> Result<Preprocessor, String> {
        let content = fs::read_to_string(input_path)
            .map_err(|e| format!("cannot read {}: {}", input_path, e))?;

        Self::process(&content, input_path, search_dirs, extra_macros)
    }

    /// Process source text with import resolution.
    /// `extra_macros` are compiler-specific predefined macros (e.g. `__clang__`).
    pub fn process(content: &str, file_path: &str, search_dirs: &[String], extra_macros: &[&str]) -> Result<Preprocessor, String> {
        let mut resolved = HashSet::new();
        resolved.insert(file_path.to_string());

        let mut jeti_out = String::new();
        let mut c_out = Vec::new();
        let mut defined = HashSet::new();
        for m in extra_macros { defined.insert(m.to_string()); }
        let mut cond_stack: Vec<CondFrame> = Vec::new();
        // Jeti-syntax macro table (dual-track): bodies a C compiler could not
        // expand are parsed here and expanded at the source level before
        // lexing; plain C defines keep flowing to the C prelude.
        let mut jeti_macros: HashMap<String, MacroDef> = HashMap::new();
        // Line map: for each emitted inline line, the (file, source line) it
        // came from. Lets parser/binder/checker errors point at real source
        // positions instead of the flattened inlined buffer.
        let mut line_map: Vec<(String, u32)> = Vec::new();
        // Per-file registry (plan 阶段 1): real line-start tables, indexed by
        // SourceId; the region-based SourceMap is built from it below.
        let mut registry = SourceRegistry::new();

        resolve_source(content, file_path, search_dirs, &mut resolved, &mut jeti_out, &mut c_out, &mut defined, &mut jeti_macros, &mut cond_stack, &mut line_map, &mut registry)?;

        // Expand jeti-syntax macros across the whole resolved stream
        // (ISO 9899 §6.10.3 replacement, implemented in jeti-cpp).
        //
        // A call may span source lines: the expansion then collapses them, so
        // the line map is rebuilt from what the expander reports — one entry per
        // emitted line, each pointing at the line its call *starts* on (the
        // invocation site). Collapsing is therefore invisible to every later
        // diagnostic: positions still name the line the author wrote.
        let (jeti_out, line_map, columns) = if jeti_macros.is_empty() {
            (jeti_out, line_map, Vec::new())
        } else {
            let (text, expanded) = cpp::expand_mapped(&jeti_out, &jeti_macros)
                .map_err(|e| format!("Macro expansion failed:\n[cpp] {}", e))?;
            let mapped: Vec<(String, u32)> = expanded
                .iter()
                .filter_map(|l| l.src_line.checked_sub(1).and_then(|k| line_map.get(k as usize)).cloned())
                .collect();
            let columns = column_regions(&expanded);
            (text, mapped, columns)
        };

        Ok(Preprocessor {
            resolved_jeti: jeti_out,
            c_headers: c_out,
            source_map: SourceMap::from_line_table(line_map, &mut registry).with_columns(columns),
        })
    }
}

/// Turn the expander's per-line runs into the column-provenance table the
/// [`SourceMap`] carries: a verbatim run records how far an expansion shifted
/// it, and expansion output names the invocation site it came from.
fn column_regions(lines: &[cpp::ExpandedLine]) -> Vec<ColumnRegion> {
    let mut columns = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let out_line = idx as u32 + 1;
        let mut col = 1u32;
        for seg in &line.segs {
            let out_start = col;
            col += seg.len;
            let out_end = col;
            match seg.kind {
                cpp::SegKind::Verbatim { src_col } => {
                    let delta = out_start as i64 - src_col as i64;
                    if delta != 0 {
                        columns.push(ColumnRegion {
                            out_line, out_start, out_end,
                            origin: ColOrigin::Verbatim { delta: delta as i32 },
                        });
                    }
                }
                cpp::SegKind::Invocation { col: invoke } => columns.push(ColumnRegion {
                    out_line, out_start, out_end,
                    origin: ColOrigin::Invocation { col: invoke },
                }),
            }
        }
    }
    columns
}

#[cfg(test)]
mod if_eval_tests {
    use super::*;

    fn obj(name: &str, body: &str) -> (String, MacroDef) {
        (
            name.to_string(),
            MacroDef { name: name.to_string(), params: None, variadic: false, body: body.to_string() },
        )
    }

    fn eval(expr: &str, table: &[(&str, &str)], defined: &[&str]) -> bool {
        let macros: HashMap<String, MacroDef> = table.iter().map(|(n, b)| obj(n, b)).collect();
        let def: HashSet<String> = defined.iter().map(|s| s.to_string()).collect();
        eval_if_expr(expr, &def, &macros)
    }

    #[test]
    fn if_expands_macros_before_eval() {
        // `LEVEL > THRESHOLD` must expand to `3 > 2` (was: panic in rfind_token
        // slicing a 5-byte string with a 2-byte token).
        assert!(eval("LEVEL > THRESHOLD", &[("LEVEL", "3"), ("THRESHOLD", "2")], &[]));
        assert!(!eval("LEVEL < THRESHOLD", &[("LEVEL", "3"), ("THRESHOLD", "2")], &[]));
    }

    #[test]
    fn if_defined_operands_are_exempt_from_expansion() {
        // defined(X) must check definedness, not expand X's body.
        assert!(eval("defined(FEATURE) && LEVEL == 3",
                     &[("FEATURE", "1"), ("LEVEL", "3")],
                     &["FEATURE"]));
        // defined of an unregistered name is false even though expansion would 0 it.
        assert!(eval("!defined(NOPE)", &[("NOPE", "1")], &[]));
        // bare `defined NAME` form
        assert!(eval("defined FEATURE", &[], &["FEATURE"]));
    }

    #[test]
    fn if_precedence_and_arithmetic() {
        assert!(eval("2 + 3 * 4 == 14", &[], &[]));
        assert!(!eval("(2 + 3) * 4 == 14", &[], &[]));
        assert!(eval("1 << 4 == 16", &[], &[]));
        assert!(eval("0x10 == 16", &[], &[]));
        assert!(eval("17 / 5 + 17 % 5 == 5", &[], &[]));
        assert!(eval("-3 < 0", &[], &[]));
    }

    #[test]
    fn if_comparisons_and_logic() {
        // The old evaluator returned `l == r` on bools for every comparison:
        // `0 > 5` was true. These pin the fix.
        assert!(!eval("0 > 5", &[], &[]));
        assert!(eval("5 >= 5", &[], &[]));
        assert!(eval("4 <= 4", &[], &[]));
        assert!(eval("1 && !0", &[], &[]));
        assert!(eval("0 || 2", &[], &[]));
        assert!(eval("1 ? 10 : 20 == 10", &[], &[]));
    }

    #[test]
    fn if_undefined_identifier_is_zero() {
        assert!(!eval("UNREGISTERED_NAME", &[], &[]));
        assert!(eval("SOME_FLAG", &[], &["SOME_FLAG"]));
    }

    #[test]
    fn if_malformed_evaluates_false_not_panic() {
        assert!(!eval("3 >", &[], &[]));
        assert!(!eval("(1", &[], &[]));
        assert!(!eval("1 / 0", &[], &[]));
    }
}

#[cfg(test)]
mod directive_tests {
    use super::*;

    /// Resolve a snippet the way the pipeline does (no search dirs, no extra
    /// platform macros), returning the jeti stream or the error message.
    fn run(src: &str) -> Result<String, String> {
        Preprocessor::process(src, "t.jeti", &[], &[]).map(|p| p.resolved_jeti)
    }

    fn body(src: &str) -> String {
        run(src).expect("snippet should resolve").trim().to_string()
    }

    #[test]
    fn unterminated_if_is_reported() {
        // Without this, the rest of the file is silently swallowed: the guard
        // directives never reach clang, so nothing downstream would notice.
        let e = run("#if 0\nA\n").unwrap_err();
        assert!(e.contains("unterminated conditional"), "got: {e}");
        assert_eq!(body("#if 1\nA\n#endif\n"), "A");
    }

    #[test]
    fn a_guard_marker_is_not_re_emitted_bare() {
        // `#ifndef NAME / #define NAME / <content> / #endif` cannot keep its
        // guard — the content lives on the jeti track while `c_out` is a
        // separate stream — but the companion `#define NAME` must NOT survive
        // alone: a bare marker makes a re-emitted `#ifndef NAME` around the
        // same struct skip itself and leave an incomplete type.
        let p = Preprocessor::process(
            "#ifndef FOO_DEFINED\n#define FOO_DEFINED\nstruct Foo { int a; };\n#endif\n",
            "t.jeti", &[], &[],
        ).expect("resolve");
        assert!(
            !p.c_headers.iter().any(|l| l.trim() == "#define FOO_DEFINED"),
            "bare guard marker leaked into the C output: {:?}",
            p.c_headers,
        );
        // A valued define is a real macro, not a marker — it still comes through.
        let p = Preprocessor::process(
            "#ifndef BAR\n#define BAR 42\nstruct Bar { int b; };\n#endif\n",
            "t.jeti", &[], &[],
        ).expect("resolve");
        assert!(
            p.c_headers.iter().any(|l| l.trim() == "#define BAR 42"),
            "a valued define must survive: {:?}",
            p.c_headers,
        );
    }

    #[test]
    fn stray_endif_elif_else_are_reported() {
        assert!(run("A\n#endif\n").unwrap_err().contains("#endif without matching #if"));
        assert!(run("#elif 1\nA\n").unwrap_err().contains("#elif without matching #if"));
        assert!(run("#else\nA\n").unwrap_err().contains("#else without matching #if"));
    }

    #[test]
    fn branch_after_else_is_reported() {
        let src = "#if 0\nA\n#else\nB\n#elif 1\nC\n#endif\n";
        assert!(run(src).unwrap_err().contains("#elif after #else"));
        let src = "#if 0\nA\n#else\nB\n#else\nC\n#endif\n";
        assert!(run(src).unwrap_err().contains("#else after #else"));
    }

    #[test]
    fn elif_chain_takes_the_first_true_branch() {
        let src = "#if 0\nA\n#elif 1\nB\n#elif 1\nC\n#else\nD\n#endif\nE\n";
        assert_eq!(body(src), "B\nE");
    }

    #[test]
    fn inactive_branch_skips_nested_conditionals() {
        let src = "#if 0\n#if 1\nA\n#endif\nB\n#endif\nC\n";
        assert_eq!(body(src), "C");
    }

    #[test]
    fn if_accepts_forms_without_a_space() {
        assert_eq!(body("#if(1)\nA\n#endif\n"), "A");
        assert_eq!(body("#if !defined(NOPE)\nA\n#endif\n"), "A");
        // `#iffy` is not a conditional directive — it must neither open a block
        // (which would then be unterminated) nor swallow the lines after it.
        assert_eq!(body("A\n#iffy\nB\n"), "A\nB");
    }

    #[test]
    fn undef_flips_ifdef() {
        let src = "#define X 1\n#ifdef X\nA\n#endif\n#undef X\n#ifdef X\nB\n#endif\n";
        assert_eq!(body(src), "A");
    }

    #[test]
    fn if_expands_function_like_macro_calls() {
        let src = "#define ADD(a, b) a + b\n#if ADD(1, 2) == 3\nA\n#endif\n";
        assert_eq!(body(src), "A");
    }

    #[test]
    fn skipped_group_does_not_define_macros() {
        // §6.10.1p6: a skipped group is not processed, so its `#define` takes no
        // effect — `X` stays undefined for the later `#ifdef`.
        let src = "#if 0\n#define X 1\n#endif\n#ifdef X\nA\n#else\nB\n#endif\n";
        assert_eq!(body(src), "B");
    }

    #[test]
    fn leading_whitespace_before_directives_is_allowed() {
        assert_eq!(body("  #if 1\nA\n\t#endif\n"), "A");
    }
}
