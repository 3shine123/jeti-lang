//! System C-header symbols, extracted from the real toolchain — no name
//! tables in the source. `cc -E` preprocesses the headers a document actually
//! imports, and the preprocessed token stream is scanned for prototypes and
//! extern variables; `cc -E -dM` contributes the macro names. The result
//! backs semantic coloring and the hover card for the passthrough surface
//! (`printf`, `errno`, `EOF`, …) that the analysis never parses.
//!
//! Everything here is display-layer: a misparse can only cost a color or a
//! signature line, never a compile. Failures (no `cc`, odd output) degrade to
//! an empty table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One extracted declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CDecl {
    /// The header that provided it, as written in `#include <...>`.
    pub header: String,
    /// Full spelling, clangd-style: `int printf(const char *restrict, ...)`.
    pub label: String,
    /// Return type for functions (`int`), type spelling for variables.
    pub type_spelling: String,
    /// Parameter spellings; empty for `void` and for variables.
    pub params: Vec<String>,
}

/// A macro's real definition from `-dM`: parameter names (empty for an
/// object-like macro) and the body after the parameter list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CMacro {
    pub header: String,
    pub params: Vec<String>,
    pub body: String,
}

/// A function, a variable, or a typedef, by name — macros live in
/// [`CSignatures::macros`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Function,
    Variable,
    Typedef,
}

/// The extracted passthrough surface: declarations by name, macro names, and
/// the header each symbol came from.
#[derive(Debug, Default)]
pub struct CSignatures {
    decls: HashMap<String, (DeclKind, CDecl)>,
    pub macros: HashMap<String, CMacro>,
    /// Terminal spellings for typedef targets the headers never define — the
    /// compiler builtins (`__builtin_va_list` → `char *` on clang). Probed
    /// from the compiler once per process; see [`resolve_builtins`].
    builtin_resolves: HashMap<String, String>,
}

impl CSignatures {
    pub fn decl(&self, name: &str) -> Option<(DeclKind, &CDecl)> {
        self.decls.get(name).map(|(k, d)| (*k, d))
    }

    pub fn macro_name(&self, name: &str) -> Option<&str> {
        self.macros.get(name).map(|m| m.header.as_str())
    }

    pub fn macro_def(&self, name: &str) -> Option<&CMacro> {
        self.macros.get(name)
    }

    /// Follow a typedef chain to its terminal spelling: `va_list` →
    /// `__darwin_va_list` → `__builtin_va_list` → `char *`. clangd shows the
    /// terminal, not the alias one hop away.
    pub fn resolve_type(&self, spelling: &str) -> String {
        let mut current = spelling.to_string();
        for _ in 0..5 {
            let is_plain = !current.is_empty()
                && current
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !is_plain {
                return current;
            }
            match self.decls.get(&current) {
                Some((DeclKind::Typedef, decl)) => current = decl.type_spelling.clone(),
                _ => {
                    return self
                        .builtin_resolves
                        .get(&current)
                        .cloned()
                        .unwrap_or(current);
                }
            }
        }
        current
    }
}

/// Terminal spellings for typedef targets no header defines — the compiler
/// builtins. Each distinct target is probed once (a deliberate member error
/// makes clang print the real type as `aka '…'`); unknown names probe to
/// nothing and stay themselves.
fn resolve_builtins(table: &mut CSignatures) {
    let targets: Vec<String> = table
        .decls
        .values()
        .filter_map(|(_, decl)| {
            let plain = !decl.type_spelling.is_empty()
                && decl
                    .type_spelling
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_');
            (plain && !table.decls.contains_key(&decl.type_spelling))
                .then(|| decl.type_spelling.clone())
        })
        .collect();
    for target in targets {
        if table.builtin_resolves.contains_key(&target) {
            continue;
        }
        if let Some(spelling) = probe_builtin_spelling(&target) {
            table.builtin_resolves.insert(target, spelling);
        }
    }
}

/// Substitute a macro use's actual arguments into the body: whole-word
/// parameter names only (`ap` must not touch `apa`).
pub fn substitute_params(body: &str, params: &[String], args: &[String]) -> String {
    let word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut out = body.to_string();
    for (param, arg) in params.iter().zip(args) {
        let pb = param.as_bytes();
        let bytes = out.as_bytes();
        let mut replaced = String::with_capacity(out.len());
        let mut i = 0;
        while i < bytes.len() {
            let hit = bytes[i..].starts_with(pb)
                && (i == 0 || !word_byte(bytes[i - 1]))
                && (i + pb.len() == bytes.len() || !word_byte(bytes[i + pb.len()]));
            if hit {
                replaced.push_str(arg);
                i += pb.len();
            } else {
                let len = out[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                replaced.push_str(&out[i..i + len]);
                i += len;
            }
        }
        out = replaced;
    }
    out
}

/// Extract (with a per-process cache keyed on the header set) the symbols for
/// `headers` — names as written in `#include <...>`. Project and user headers
/// do not belong here: this table is only the passthrough surface. The cache
/// is a table, not a single slot: two documents with different header sets
/// are edited alternately, and a single slot would re-run `cc` per keystroke.
pub fn signatures_for(headers: &[String]) -> Arc<CSignatures> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<CSignatures>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut fingerprint = headers.to_vec();
    fingerprint.sort();
    let fingerprint = fingerprint.join("\u{1}");
    let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(hit) = guard.get(&fingerprint) {
        return hit.clone();
    }
    let table = Arc::new(extract(headers));
    guard.insert(fingerprint, table.clone());
    table
}

/// The compiler's own spelling for a builtin type name: a deliberate
/// member-reference error makes clang print the real type as `aka '…'`
/// (`typedef __builtin_va_list probe_t; probe_t v; v.f = 1;` →
/// `… base type 'probe_t' (aka 'char *') …`). This is how the typedef chain
/// reaches its terminal without a single hardcoded type name.
fn probe_builtin_spelling(builtin: &str) -> Option<String> {
    let src = format!(
        "typedef {builtin} jeti_probe_t;\nvoid jeti_probe_fn(void) {{ jeti_probe_t v; v.jeti_probe_field = 1; }}\n"
    );
    let tmp = std::env::temp_dir().join(format!("jeti-lsp-aka-{}.c", std::process::id()));
    std::fs::write(&tmp, src).ok()?;
    let output = std::process::Command::new("cc")
        .args(["-fsyntax-only", "-xc", &tmp.to_string_lossy()])
        .output()
        .ok()?;
    parse_aka(&String::from_utf8_lossy(&output.stderr))
}

/// The first `aka '…'` spelling in a compiler diagnostic.
fn parse_aka(stderr: &str) -> Option<String> {
    let i = stderr.find("aka '")? + "aka '".len();
    let rest = &stderr[i..];
    let end = rest.find('\'')?;
    let spelling = rest[..end].trim();
    if spelling.is_empty() {
        return None;
    }
    Some(spelling.to_string())
}

/// Run `cc` over a synthetic include file per header and scan the outputs.
/// One run *per header* (not a merged one): line-marker attribution inside a
/// merged run can misbind a transitively-included `sys/time.h` to a wanted
/// `time.h`, while a solo run's first header marker is the header itself.
fn extract(headers: &[String]) -> CSignatures {
    let mut table = CSignatures::default();
    // Unique per process: the server and test binaries share /tmp, and a
    // fixed name would let one overwrite the file the other is compiling.
    let tmp = std::env::temp_dir().join(format!("jeti-lsp-sigscan-{}.c", std::process::id()));
    for header in headers {
        let source = format!("#include <{header}>\n");
        if std::fs::write(&tmp, &source).is_err() {
            return table;
        }
        if let Ok(output) = std::process::Command::new("cc")
            .args(["-E", "-xc", &tmp.to_string_lossy()])
            .output()
        {
            let preprocessed = String::from_utf8_lossy(&output.stdout);
            scan_declarations(&preprocessed, header, &mut table);
        }
        if let Ok(output) = std::process::Command::new("cc")
            .args(["-E", "-dM", "-xc", &tmp.to_string_lossy()])
            .output()
        {
            let defines = String::from_utf8_lossy(&output.stdout);
            scan_defines(&defines, header, &mut table);
        }
    }
    resolve_builtins(&mut table);
    table
}

/// A preprocessed token: identifiers, numbers, strings, chars, and single
/// punctuation. Whitespace separates but is not itself a token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Punct(char),
    Literal(String),
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if b == b'"' || b == b'\'' {
            let quote = b;
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(Tok::Literal(text[start..i].to_string()));
        } else if b.is_ascii_alphanumeric() || b == b'_' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            tokens.push(Tok::Word(text[start..i].to_string()));
        } else {
            tokens.push(Tok::Punct(b as char));
            i += 1;
        }
    }
    tokens
}

/// Toks to drop wherever they appear between the declaration start and the
/// name: toolchain annotations the preprocessor leaves behind.
fn is_noise(word: &str) -> bool {
    matches!(
        word,
        "__attribute__" | "__asm" | "__asm__" | "__volatile__" | "__restrict" | "__restrict__"
            | "__extension__"
    )
}

/// Does a `name (`'s close end a declaration? Skip what may follow a
/// prototype's parens in real headers — toolchain noise words and annotation
/// *call groups* (`__attribute__((…))`, `__printflike(1, 2)`,
/// `__DARWIN_ALIAS(printf)`) — then require `;` or `,`.
fn ends_a_declaration(tokens: &[Tok], close_index: usize) -> bool {
    let i = skip_annotation(tokens, close_index + 1);
    matches!(tokens.get(i), Some(Tok::Punct(';')) | Some(Tok::Punct(',')))
}

/// Step past annotation trivia (see [`ends_a_declaration`]); returns the index
/// of the next meaningful token.
fn skip_annotation(tokens: &[Tok], mut i: usize) -> usize {
    loop {
        match tokens.get(i) {
            Some(Tok::Word(w)) if is_noise(w) => i += 1,
            // A bare paren group directly after noise (`__attribute__ ((…))`).
            Some(Tok::Punct('(')) => i = skip_group(tokens, i),
            // An annotation call: `name (…)` — skip its whole group.
            Some(Tok::Word(_)) if matches!(tokens.get(i + 1), Some(Tok::Punct('('))) => {
                i = skip_group(tokens, i + 1)
            }
            _ => return i,
        }
    }
}

/// Index just past the `)` matching the `(` at `from`; the end if unpaired.
fn skip_group(tokens: &[Tok], from: usize) -> usize {
    let mut depth = 1usize;
    let mut j = from + 1;
    while j < tokens.len() {
        match tokens[j] {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    j
}

/// Join declaration tokens into a display spelling: `int printf(const char
/// *restrict, ...)`. Punctuation hugs (`printf(`, `*,`), `*` hugs forward,
/// and toolchain noise words are skipped so callers can slice the original
/// token stream without reindexing.
fn spell(tokens: &[Tok]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Tok> = None;
    for tok in tokens {
        if let Tok::Word(w) = tok {
            if is_noise(w) {
                continue;
            }
        }
        let piece = match tok {
            Tok::Word(w) => w.clone(),
            Tok::Literal(l) => l.clone(),
            Tok::Punct(p) => p.to_string(),
        };
        match prev {
            None => out.push_str(&piece),
            Some(previous) => {
                // `...` arrives as three dots: dots glue right (the first one
                // takes the comma's space), commas hug left but keep a space
                // before what follows (`restrict, ...`).
                let hugs_left = matches!(tok, Tok::Punct('(' | ')' | '[' | ']' | ','));
                let prev_hugs_right = matches!(previous, Tok::Punct('*' | '&' | '.'))
                    && !matches!(tok, Tok::Punct('*'));
                if hugs_left || prev_hugs_right {
                    // `*` still takes a space *after* it: `char *restrict`.
                } else if matches!(previous, Tok::Punct('(' | '[')) {
                } else {
                    out.push(' ');
                }
                out.push_str(&piece);
            }
        }
        prev = Some(tok);
    }
    out
}

/// Split `(`..`)` tokens into parameter spellings on depth-0 commas; `void`
/// means no parameters.
fn split_params(tokens: &[Tok]) -> Vec<String> {
    let mut params = Vec::new();
    let mut current: Vec<Tok> = Vec::new();
    let mut depth = 0usize;
    for tok in tokens {
        match tok {
            Tok::Punct('(') => {
                depth += 1;
                current.push(tok.clone());
            }
            Tok::Punct(')') => {
                depth = depth.saturating_sub(1);
                current.push(tok.clone());
            }
            Tok::Punct(',') if depth == 0 => {
                if !current.is_empty() {
                    params.push(spell(&current));
                    current.clear();
                }
            }
            _ => current.push(tok.clone()),
        }
    }
    if !current.is_empty() {
        params.push(spell(&current));
    }
    if params.len() == 1 && params[0] == "void" {
        params.clear();
    }
    params
}

/// Type keywords a declaration may start with — a `name (` whose name is one
/// of these is a function-pointer declarator (`int (*fp)(void)`), not a call.
fn is_type_word(word: &str) -> bool {
    matches!(
        word,
        "void" | "bool" | "char" | "short" | "int" | "long" | "float" | "double"
            | "signed" | "unsigned" | "const" | "volatile" | "struct" | "union"
            | "enum" | "extern" | "static" | "inline" | "register" | "_Bool"
    )
}

/// Does a line-marker path name this header? Component match, so `/x/my` +
/// `stdio.h` cannot pass for `/x/mystdio.h`.
fn is_header_file(path: &str, header: &str) -> bool {
    path.rsplit('/').next() == Some(header)
}

/// Scan one header's `cc -E` output for prototypes, extern variables, and
/// typedefs. Line markers (`# 12 "/usr/include/stdio.h" 1 3`) drive a file
/// stack; the wanted header stays on stage while any frame in the stack is
/// that file — `printf` lives in `_printf.h`, reached through `stdio.h`, and
/// must still be attributed to the `stdio.h` the document asked for. A
/// first-wins insert keeps the document's first provider.
fn scan_declarations(text: &str, header: &str, table: &mut CSignatures) {
    let mut stack: Vec<String> = Vec::new();
    let mut current_header = String::new();
    let mut decl_start = 0usize; // token index where the current declaration begins
    let mut pending_typedef = None; // decl_start of a `typedef struct { … }` in flight
    let mut typedef_depth = 0usize; // open braces inside the pending typedef body
    let tokens = tokenize(text);
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Tok::Punct('#') => {
                // Line marker: `# 34 "/…/_printf.h" 1 3 4`. Flag 1 enters a
                // new file, flag 2 returns to the includer, neither is a
                // line jump in the current file.
                if let Some(Tok::Literal(path)) = tokens.get(i + 2) {
                    let path = path.trim_matches('"').to_string();
                    let mut j = i + 3;
                    let (mut enters, mut returns) = (false, false);
                    while matches!(tokens.get(j), Some(Tok::Word(w)) if w.chars().all(|c| c.is_ascii_digit()))
                    {
                        let Tok::Word(w) = &tokens[j] else { unreachable!() };
                        enters |= w == "1";
                        returns |= w == "2";
                        j += 1;
                    }
                    if enters {
                        stack.push(path);
                    } else if returns {
                        stack.pop();
                    } else {
                        match stack.last_mut() {
                            Some(top) => *top = path,
                            None => stack.push(path),
                        }
                    }
                    current_header = if stack.iter().any(|p| is_header_file(p, header)) {
                        header.to_string()
                    } else {
                        String::new()
                    };
                    decl_start = j;
                    i = j;
                } else {
                    // `#pragma` and friends: break the declaration, move on.
                    decl_start = i + 1;
                    i += 1;
                }
            }
            Tok::Punct(';') => {
                decl_start = i + 1;
                i += 1;
            }
            Tok::Punct('{') => {
                // A typedef's body braces do not reset the declaration: the
                // name comes after `}` (`typedef struct { ... } Name;`).
                // Nested braces just deepen; the body's close below unwinds.
                if pending_typedef.is_some() {
                    typedef_depth += 1;
                } else if matches!(&tokens[decl_start], Tok::Word(w) if w == "typedef") {
                    pending_typedef = Some(decl_start);
                    typedef_depth = 1;
                } else {
                    decl_start = i + 1;
                }
                i += 1;
            }
            Tok::Punct('}') => {
                // Inside a typedef body the declaration start stays put; the
                // body's own close hands the span back so the name after `}`
                // sees `typedef ...`.
                match pending_typedef {
                    Some(start) => {
                        typedef_depth -= 1;
                        if typedef_depth == 0 {
                            decl_start = start;
                            pending_typedef = None;
                        }
                    }
                    None => decl_start = i + 1,
                }
                i += 1;
            }
            Tok::Punct('(') => {
                // Candidate: `name (` with a type before it. Pair the parens.
                let is_name = i > decl_start + 1
                    && matches!(&tokens[i - 1], Tok::Word(w) if !is_type_word(w));
                if !is_name {
                    i += 1;
                    continue;
                }
                let mut depth = 1usize;
                let mut close = None;
                for (j, tok) in tokens.iter().enumerate().skip(i + 1) {
                    match tok {
                        Tok::Punct('(') => depth += 1,
                        Tok::Punct(')') => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(j);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let Some(close) = close else {
                    // An unpaired `(` (macro expansion debris): skip it, the
                    // scan goes on.
                    i += 1;
                    continue;
                };
                if ends_a_declaration(&tokens, close) && !current_header.is_empty() {
                    let name_index = i - 1;
                    let Tok::Word(name) = &tokens[name_index] else { unreachable!() };
                    // A declaration starts with a type; a statement keyword
                    // marks inline-body control flow (`if (x) abort();`), a
                    // `(` in the would-be return type marks a fn-ptr
                    // declarator, and an `=` marks an initializer statement.
                    let starts_statement = matches!(&tokens[decl_start], Tok::Word(w)
                        if matches!(w.as_str(), "if"|"else"|"for"|"while"|"switch"|"return"|"do"|"case"|"default"));
                    let mut ret = spell(&tokens[decl_start..name_index]);
                    if let Some(rest) = ret.strip_prefix("extern ") {
                        ret = rest.to_string();
                    }
                    if ret.is_empty() || starts_statement || ret.contains('(') || ret.contains('=')
                    {
                        decl_start = close + 1;
                        i = close + 1;
                        continue;
                    }
                    let mut label = spell(&tokens[decl_start..=close]);
                    if let Some(rest) = label.strip_prefix("extern ") {
                        label = rest.to_string();
                    }
                    let params = split_params(&tokens[i + 1..close]);
                    table
                        .decls
                        .entry(name.clone())
                        .or_insert((
                            DeclKind::Function,
                            CDecl {
                                header: current_header.clone(),
                                label,
                                type_spelling: ret,
                                params,
                            },
                        ));
                    decl_start = close + 1;
                    i = close + 1;
                    continue;
                }
                i += 1;
            }
            Tok::Word(word) => {
                // Extern variable: `extern int errno ;` — a name directly
                // before `;` or a declarator-list `,`, the declaration led by
                // `extern`. The extern requirement keeps struct fields
                // (`typedef struct { int x; } T;`) out of the table.
                if matches!(tokens.get(i + 1), Some(Tok::Punct(';')) | Some(Tok::Punct(',')))
                    && i > decl_start
                    && !is_noise(word)
                    && !current_header.is_empty()
                    && (matches!(&tokens[decl_start], Tok::Word(w) if w == "extern")
                        || matches!(&tokens[decl_start], Tok::Punct('*')))
                {
                    let mut type_spelling = spell(&tokens[decl_start..i]);
                    if let Some(rest) = type_spelling.strip_prefix("extern ") {
                        type_spelling = rest.to_string();
                    }
                    if !type_spelling.is_empty() {
                        table
                            .decls
                            .entry(word.clone())
                            .or_insert((
                                DeclKind::Variable,
                                CDecl {
                                    header: current_header.clone(),
                                    label: spell(&tokens[decl_start..=i]),
                                    type_spelling,
                                    params: Vec::new(),
                                },
                            ));
                        // A declarator list (`extern char *a, *b;`): the next
                        // name's type starts past the comma.
                        if matches!(tokens.get(i + 1), Some(Tok::Punct(','))) {
                            decl_start = i + 2;
                        }
                    }
                }
                // Typedef: `typedef … Name ;`. Plain spellings (`typedef int
                // MyInt`) and brace forms (`typedef struct { … } Name`) both
                // qualify; a `(` anywhere means a function-pointer declarator
                // and is skipped. Covers FILE, size_t, time_t, va_list, …
                if matches!(tokens.get(i + 1), Some(Tok::Punct(';')))
                    && i > decl_start + 1
                    && !is_noise(word)
                    && !current_header.is_empty()
                    && pending_typedef.is_none()
                    && matches!(&tokens[decl_start], Tok::Word(w) if w == "typedef")
                {
                    let span = &tokens[decl_start..i];
                    let open = span.iter().position(|t| *t == Tok::Punct('{'));
                    // A brace form only counts once its `}` is behind the
                    // name, and only the head *before* `{` must be free of
                    // `(` — parens inside the body are function-pointer
                    // members (`FILE`'s `(*__close)(void *)`) and are fine.
                    let ok = match open {
                        Some(open_idx) => {
                            let closed = span
                                .iter()
                                .rposition(|t| *t == Tok::Punct('}'))
                                .is_some_and(|c| c > open_idx);
                            closed
                                && !span[..open_idx].iter().any(|t| *t == Tok::Punct('('))
                        }
                        None => !span.iter().any(|t| *t == Tok::Punct('(')),
                    };
                    if ok {
                        let (type_spelling, label) =
                            match span.iter().position(|t| *t == Tok::Punct('{')) {
                                Some(open) => {
                                    let head = spell(&span[..open]);
                                    let base =
                                        head.strip_prefix("typedef ").unwrap_or(&head).to_string();
                                    (format!("{base} {{ … }}"), format!("{head} {{ … }} {word}"))
                                }
                                None => {
                                    let head = spell(span);
                                    let base =
                                        head.strip_prefix("typedef ").unwrap_or(&head).to_string();
                                    (base, format!("{head} {word}"))
                                }
                            };
                        if !type_spelling.is_empty() {
                            // clangd attributes an alias to the file its
                            // definition physically sits in, not the entry
                            // header that pulled it in.
                            let defining =
                                defining_display(stack.last(), header);
                            table
                                .decls
                                .entry(word.clone())
                                .or_insert((
                                    DeclKind::Typedef,
                                    CDecl {
                                        header: defining,
                                        label,
                                        type_spelling,
                                        params: Vec::new(),
                                    },
                                ));
                        }
                    }
                }
                i += 1;
            }
            Tok::Literal(_) => i += 1,
            // `,` `*` `>` `...` and friends: never structural here, but the
            // scan must advance (a no-match arm would spin forever).
            Tok::Punct(_) => i += 1,
        }
    }
}

/// clangd-style display for a defining header path: nested SDK headers show
/// their path past `usr/include/` (`sys/_types/_va_list.h`), everything else
/// its file name (the clang-builtin include dir has no `usr/include`).
fn defining_display(path: Option<&String>, fallback: &str) -> String {
    let Some(path) = path else {
        return fallback.to_string();
    };
    if let Some(i) = path.find("usr/include/") {
        return path[i + "usr/include/".len()..].to_string();
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Scan one header's `cc -E -dM` output (`#define NAME body`) for macro
/// definitions — parameter list and body both, so the hover card can show the
/// real `#define` and the expansion, the way clangd does.
fn scan_defines(text: &str, header: &str, table: &mut CSignatures) {
    // Backslash continuations first: a multi-line body is one definition.
    let mut joined = String::new();
    let mut open = String::new();
    for line in text.lines() {
        if let Some(carried) = line.strip_suffix('\\') {
            open.push_str(carried);
            continue;
        }
        if open.is_empty() {
            joined.push_str(line);
        } else {
            open.push_str(line.trim_start());
            joined.push_str(&open);
            open.clear();
        }
        joined.push('\n');
    }
    for line in joined.lines() {
        let Some(rest) = line.strip_prefix("#define ") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let after = &rest[name.len()..];
        // Function-like iff `(` follows the name with no space (`#define
        // f(x) …` vs `#define EOF (-1)`).
        let (params, body) = if after.starts_with('(') {
            match after.find(')') {
                Some(close) => (
                    after[1..close]
                        .split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(String::from)
                        .collect(),
                    after[close + 1..].trim().to_string(),
                ),
                None => (Vec::new(), String::new()),
            }
        } else {
            (Vec::new(), after.trim().to_string())
        };
        table.macros.entry(name).or_insert(CMacro {
            header: header.to_string(),
            params,
            body,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototypes_variables_and_macros_extract_from_preprocessed_text() {
        let text = r#"# 1 "/usr/include/stdio.h"
extern int fsc_printf(const char *restrict, ...);
extern int fsc_errno;
typedef struct { int x; } fsc_pair_t;
static inline int fsc_inline(void) { return 1; }
extern char *fsc_copy(char *, const char *);
"#;
        let mut table = CSignatures::default();
        scan_declarations(text, "stdio.h", &mut table);

        let (kind, printf) = table.decl("fsc_printf").expect("printf prototype");
        assert_eq!(kind, DeclKind::Function);
        assert_eq!(printf.type_spelling, "int");
        assert_eq!(printf.params, vec!["const char *restrict", "..."]);
        assert_eq!(printf.label, "int fsc_printf(const char *restrict, ...)");
        assert_eq!(printf.header, "stdio.h");

        let (kind, errno) = table.decl("fsc_errno").expect("errno variable");
        assert_eq!(kind, DeclKind::Variable);
        assert_eq!(errno.type_spelling, "int", "extern is stripped for display");
        assert_eq!(errno.label, "extern int fsc_errno");

        let (kind, pair) = table.decl("fsc_pair_t").expect("typedef extraction");
        assert_eq!(kind, DeclKind::Typedef);
        assert_eq!(pair.type_spelling, "struct { … }");
        assert_eq!(pair.label, "typedef struct { … } fsc_pair_t");

        // The inline definition's body is not a declaration; the function
        // itself is lost to the `{` reset — acceptable, it is not a prototype.
        assert!(table.decl("fsc_inline").is_none());
        // The typedef's field must not leak as a variable (`int x;` inside {}).
        assert!(table.decl("x").is_none());

        let mut table = CSignatures::default();
        scan_declarations("# 1 \"/usr/include/stdio.h\"\n", "stdio.h", &mut table);
        scan_defines(
            "#define EOF (-1)\n#define BUFSIZ 8192\n#define FMT(a, b) ((a) > (b) ? (a) : (b))\n#define WIDE(x) \\\n    do { (void)(x); } while (0)\n",
            "stdio.h",
            &mut table,
        );
        assert_eq!(table.macro_name("EOF"), Some("stdio.h"));
        // The definition is real: object-like keeps its body, function-like
        // keeps its parameter names, and a backslash continuation joins.
        let eof = table.macro_def("EOF").expect("EOF");
        assert_eq!((eof.params.as_slice(), eof.body.as_str()), (&[][..], "(-1)"));
        let fmt = table.macro_def("FMT").expect("FMT");
        assert_eq!(fmt.params, vec!["a", "b"]);
        assert_eq!(fmt.body, "((a) > (b) ? (a) : (b))");
        let wide = table.macro_def("WIDE").expect("WIDE");
        assert_eq!(wide.params, vec!["x"]);
        assert_eq!(wide.body, "do { (void)(x); } while (0)");
    }

    #[test]
    /// `Expands to` substitutes whole-word parameters only: `ap` must not
    /// touch `apa`, and the actual call-site arguments land in order.
    fn macro_expansion_substitutes_whole_words() {
        let out = substitute_params(
            "__builtin_va_start(ap, param)",
            &["ap".to_string(), "param".to_string()],
            &["my_args".to_string(), "fmt".to_string()],
        );
        assert_eq!(out, "__builtin_va_start(my_args, fmt)");
        let out = substitute_params("((a) > (b) ? (a) : (b))", &["a".into(), "b".into()], &["x".into(), "y".into()]);
        assert_eq!(out, "((x) > (y) ? (x) : (y))");
        // A parameter name that is a prefix of another identifier stays put.
        let out = substitute_params("apa + ap", &["ap".to_string()], &["Z".to_string()]);
        assert_eq!(out, "apa + Z");
    }

    #[test]
    /// The `aka '…'` spelling is what a typedef chain terminates at, and a
    /// diagnostic without one yields nothing.
    fn aka_parsing() {
        let stderr = "probe.c:2:30: error: member reference base type 'jeti_probe_t' (aka 'char *') is not a structure or union";
        assert_eq!(parse_aka(stderr), Some("char *".to_string()));
        assert_eq!(parse_aka("error: something else"), None);
        assert_eq!(parse_aka("error: aka '' is empty"), None);
    }

    #[test]
    /// `resolve_type` walks the alias chain to the terminal spelling.
    fn typedef_chain_resolves() {
        let mut table = CSignatures::default();
        let mk = |header: &str, ts: &str| {
            (
                DeclKind::Typedef,
                CDecl {
                    header: header.to_string(),
                    label: String::new(),
                    type_spelling: ts.to_string(),
                    params: Vec::new(),
                },
            )
        };
        table.decls.insert("va_list".into(), mk("stdarg.h", "__darwin_va_list"));
        table.decls.insert("__darwin_va_list".into(), mk("sys/_types/_va_list.h", "__builtin_va_list"));
        table.builtin_resolves.insert("__builtin_va_list".into(), "char *".into());
        assert_eq!(table.resolve_type("va_list"), "char *");
        // A non-alias spelling resolves to itself.
        assert_eq!(table.resolve_type("int"), "int");
    }

    #[test]
    fn annotations_are_stripped_and_decl_lists_split() {
        // Real preprocessor output carries a line marker first; the scanner
        // attributes declarations through it.
        let text = "\
# 1 \"/usr/include/stdio.h\"
extern int fsc_fmt(const char *fmt, ...) __attribute__((__format__(__printf__, 1, 2)));
extern char *fsc_dst, *fsc_src;
";
        let mut table = CSignatures::default();
        scan_declarations(text, "stdio.h", &mut table);
        let (_, fmt) = table.decl("fsc_fmt").expect("fmt prototype");
        assert_eq!(fmt.label, "int fsc_fmt(const char *fmt, ...)");
        assert!(!fmt.label.contains("attribute"), "{fmt:?}");
        assert!(table.decl("fsc_dst").is_some(), "declarator list member");
        assert!(table.decl("fsc_src").is_some(), "declarator list member");
    }

    #[test]
    fn spelling_hugs_punctuation_like_clangd() {
        let toks = tokenize("const char *restrict");
        assert_eq!(spell(&toks), "const char *restrict");
        let toks = tokenize("int fsc_max ( int a , int b )");
        assert_eq!(spell(&toks), "int fsc_max(int a, int b)");
    }
}
