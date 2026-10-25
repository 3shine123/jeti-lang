//! The C type-name table, recovered from the C compiler itself.
//!
//! C decides whether `(X *)p` is a cast and whether `x * y;` is a declaration
//! by the *symbol table* alone — the classic "lexer hack": a name is a type
//! name or it is an ordinary identifier, and the grammar reading follows. An
//! ObjC compiler can do that because it has preprocessed the `#include`s and
//! knows every typedef they declare.
//!
//! jetic deliberately does **not** read `#include`d headers (they are passed
//! through to the emitted C verbatim), so historically the parser had no type
//! table for them and guessed from token shape instead: a hardcoded list of
//! ~45 libc typedefs, plus "`IDENT *` looks like a declaration". Both guesses
//! are wrong in opposite directions — a cast to a real typedef outside the
//! list is rejected (`(sigset_t)v`), and `x * y;` on two variables is misread
//! as a declaration of `y` with type `x *`.
//!
//! This module closes that gap by asking the same authority the C compiler
//! consults: run the C preprocessor (`<cc> -E`) over a shim containing exactly
//! the TU's passthrough includes, then scan the preprocessed text for `typedef`
//! names. That is complete by construction (macros already expanded, `#if`
//! already resolved, system headers found the same way the real compile finds
//! them) and costs ~40 ms for a handful of headers.
//!
//! Struct/union/enum *tags* are deliberately not collected: in C a tag is not
//! a type name (`(Foo *)p` is wrong even with `struct Foo` in scope — you must
//! write `(struct Foo *)p`, which jeti's parser already handles from the
//! keyword), so adding tags would make jeti accept code C rejects.
//!
//! When the probe cannot run (no C compiler, a header that cannot be located,
//! freestanding flags that reject the shim) the caller keeps the historical
//! builtin list *and* the parser's shape fallbacks — the table is only allowed
//! to make decisions when it is known to be complete, so this feature can
//! never turn a working build into a rejected one.

use std::collections::BTreeSet;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Names the scanner never reports even if they appear in a declarator slot.
/// They are all C/attribute keywords, i.e. things jeti's lexer handles as
/// keywords and that would be nonsense in the parser's type-name list.
const NOT_TYPE_NAMES: &[&str] = &[
    // C89/C99/C11 keywords that can legally end a declarator chunk.
    "auto", "break", "case", "char", "const", "continue", "default", "do",
    "double", "else", "enum", "extern", "float", "for", "goto", "if", "inline",
    "int", "long", "register", "restrict", "return", "short", "signed",
    "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned",
    "void", "volatile", "while", "_Bool", "_Complex", "_Imaginary", "_Atomic",
    "_Generic", "_Noreturn", "_Static_assert", "_Thread_local", "_Alignas",
    "_Alignof", "_Pragma",
    // Declarator-position noise from attribute/qualifier spellings.
    "__attribute__", "__attribute", "__asm__", "__asm", "__inline",
    "__inline__", "__restrict", "__restrict__", "__const", "__const__",
    "__volatile__", "__extension__", "__typeof__", "__typeof", "__signed",
    "__signed__", "__unused", "asm", "typeof", "aligned", "packed",
    "const", "volatile", "restrict", "nonnull", "nullable", "unused",
    "_Nullable", "_Nonnull", "_Null_unspecified", "deprecated", "available",
];

/// Qualifiers/attribute spellings that may sit between `(` and `*`/`^` (or
/// after the pointer star) in a function-pointer or block declarator, and so
/// must be skipped when looking for the declared name.
const DECLARATOR_QUALIFIERS: &[&str] = &[
    "const", "volatile", "restrict", "__restrict", "__restrict__", "__const",
    "__const__", "_Nullable", "_Nonnull", "_Null_unspecified", "__nullable",
    "__nonnull", "__strong", "__weak", "__unsafe_unretained", "__autoreleasing",
    "__kindof", "noescape", "__noescape",
];

#[derive(Debug, PartialEq, Eq, Clone)]
enum Tok {
    Ident(String),
    Punct(char),
}

/// Tokenize preprocessed C, keeping punctuation adjacency intact (adjacency is
/// what makes `(*` a function-pointer declarator) and dropping comments,
/// string/char literals and `#`-line markers.
fn tokenize(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // Whether we are at the start of a line, for `# 1 "file.h"` markers and
    // for comments/escaped newlines.
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'#' && line_start {
            // Line marker / remaining directive: skip the physical line.
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        line_start = false;
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < b.len() && b[i] != quote {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            // A literal is not an identifier and not a `(*` — dropping it is
            // enough; it cannot join two punctuation marks that were apart.
            out.push(Tok::Punct('L'));
            continue;
        }
        if c == b'_' || c.is_ascii_alphabetic() {
            let start = i;
            while i < b.len() && (b[i] == b'_' || b[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            out.push(Tok::Ident(String::from_utf8_lossy(&b[start..i]).into_owned()));
            continue;
        }
        out.push(Tok::Punct(c as char));
        i += 1;
    }
    out
}

/// Collect every `typedef` name declared in preprocessed C text.
///
/// The scanner is intentionally *over*-inclusive (a declarator it cannot parse
/// precisely contributes whatever identifier it can see): a spurious entry
/// only means jeti parses a cast/declaration that the C compiler will reject
/// anyway, whereas a missed entry would reject legal code.
pub fn scan_c_type_names(preprocessed: &str) -> Vec<String> {
    let toks = tokenize(preprocessed);
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut i = 0;
    while i < toks.len() {
        if let Tok::Ident(kw) = &toks[i] {
            if kw == "typedef" {
                i = collect_typedef_decl(&toks, i + 1, &mut names);
                continue;
            }
        }
        i += 1;
    }
    names.retain(|n| !NOT_TYPE_NAMES.contains(&n.as_str()));
    names.into_iter().collect()
}

/// Consume one `typedef ... ;` starting at `start` (the token after `typedef`),
/// add every declared name, and return the index past the terminating `;`.
fn collect_typedef_decl(toks: &[Tok], start: usize, names: &mut BTreeSet<String>) -> usize {
    let mut depth = 0i32;
    let mut chunk: Vec<Tok> = Vec::new();
    let mut i = start;
    while i < toks.len() {
        match &toks[i] {
            Tok::Punct(c @ ('(' | '[' | '{')) => {
                let _ = c;
                depth += 1;
                chunk.push(toks[i].clone());
            }
            Tok::Punct(c @ (')' | ']' | '}')) => {
                let _ = c;
                depth -= 1;
                chunk.push(toks[i].clone());
            }
            Tok::Punct(';') if depth == 0 => {
                declarator_name(&chunk, names);
                return i + 1;
            }
            // `typedef unsigned a, b;` — one specifier list, several declarators.
            Tok::Punct(',') if depth == 0 => {
                declarator_name(&chunk, names);
                chunk.clear();
            }
            other => chunk.push(other.clone()),
        }
        i += 1;
    }
    // Unterminated (truncated input): still harvest what we saw.
    declarator_name(&chunk, names);
    i
}

/// Extract the declared name(s) from one declarator chunk.
fn declarator_name(chunk: &[Tok], names: &mut BTreeSet<String>) {
    if chunk.is_empty() {
        return;
    }
    // Function-pointer / block declarator: the name sits inside `(*name)` /
    // `(^name)`, which the "last identifier" rule cannot see (the trailing
    // parameter list is a deeper nesting level). Skipped inside `{...}`: a
    // typedef'd struct body's fn-ptr FIELDS are not type names — runtime.h's
    // `typedef struct jeti_kvc_entry { ...; id (*get)(id self); } ...` put the
    // field name `get` into the table, and every later `(get != NULL)`
    // expression misparsed as a cast ("expected ')'" — nullability.jeti:126).
    let mut brace_depth = 0i32;
    for m in 0..chunk.len().saturating_sub(1) {
        match &chunk[m] {
            Tok::Punct('{') => brace_depth += 1,
            Tok::Punct('}') => brace_depth -= 1,
            _ => {}
        }
        if brace_depth > 0 {
            continue;
        }
        let is_declarer_open = matches!(chunk[m], Tok::Punct('('))
            && matches!(chunk[m + 1], Tok::Punct('*') | Tok::Punct('^'));
        if !is_declarer_open {
            continue;
        }
        let mut k = m + 2;
        // Skip extra stars and qualifiers between `(*` and the name.
        while k < chunk.len() {
            match &chunk[k] {
                Tok::Punct('*') => k += 1,
                Tok::Ident(id) if DECLARATOR_QUALIFIERS.contains(&id.as_str()) => k += 1,
                Tok::Ident(id) => {
                    names.insert(id.clone());
                    break;
                }
                _ => break,
            }
        }
    }


    // Otherwise: the declared name is the last identifier at declarator
    // nesting depth 0 — that is, not inside a trailing `[...]` array suffix or
    // a parameter list — and not itself the opening of a suffix group.
    let mut depth = 0i32;
    let mut k = chunk.len();
    while k > 0 {
        k -= 1;
        match &chunk[k] {
            Tok::Punct(')') | Tok::Punct(']') | Tok::Punct('}') => depth += 1,
            Tok::Punct('(') | Tok::Punct('[') | Tok::Punct('{') => depth -= 1,
            Tok::Ident(id) if depth == 0 => {
                // `typedef int FOO __attribute__((x));` — `__attribute__` is
                // followed by `(`, so it is not the declared name.
                if matches!(chunk.get(k + 1), Some(Tok::Punct('('))) {
                    continue;
                }
                if NOT_TYPE_NAMES.contains(&id.as_str()) {
                    continue;
                }
                names.insert(id.clone());
                break;
            }
            _ => {}
        }
    }
}

/// Ask the C compiler's preprocessor for the type names visible to a
/// translation unit whose passthrough includes are `includes`.
///
/// Returns `None` when the table cannot be established (no compiler, a header
/// the preprocessor cannot find, a shim the flags reject) — the caller must
/// then fall back to the builtin list and the parser's lenient heuristics.
pub fn probe_c_type_names(
    cc: &[String],
    includes: &[String],
    search_dirs: &[String],
    macros: &[&str],
    arch: Option<&str>,
    freestanding: bool,
    nostdinc: bool,
) -> Option<Vec<String>> {
    if cc.is_empty() || includes.is_empty() {
        return None;
    }
    // Mirror the emitted C's own include so the table also covers the runtime
    // typedefs the generated code can see (`jeti_autoreleasepool_t`, ...).
    let mut shim = String::from("#include <jeti/runtime.h>\n");
    for line in includes {
        shim.push_str(line);
        shim.push('\n');
    }

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "jeti_ctype_probe_{}_{}.c",
        std::process::id(),
        n
    ));
    if fs::write(&path, shim).is_err() {
        return None;
    }

    let mut cmd = Command::new(&cc[0]);
    cmd.args(&cc[1..]);
    cmd.arg("-E").arg("-x").arg("c");
    for d in search_dirs {
        cmd.arg(format!("-I{}", d));
    }
    for m in macros {
        cmd.arg(format!("-D{}", m));
    }
    if freestanding {
        cmd.arg("-ffreestanding");
    }
    // Orthogonal to -ffreestanding: strips the system include path (and the
    // compiler's builtin freestanding headers with it), so the user's -I dirs
    // must supply stdint.h/stddef.h/stdbool.h. Failure here just degrades the
    // probe (None → parser fallbacks), never the build.
    if nostdinc {
        cmd.arg("-nostdinc");
    }
    if let Some(a) = arch {
        cmd.arg("-arch").arg(a);
    }
    cmd.arg(&path);

    let output = cmd.output();
    let _ = fs::remove_file(&path);
    let output = output.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let names = scan_c_type_names(&text);
    // An empty result means the shim preprocessed to nothing useful; treat it
    // as "unknown" rather than "there are no types", so the caller keeps the
    // builtin list.
    if names.is_empty() {
        return None;
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<String> {
        scan_c_type_names(src)
    }

    #[test]
    fn simple_typedef() {
        assert_eq!(names("typedef unsigned long size_t;"), vec!["size_t"]);
    }

    #[test]
    fn tagged_and_anonymous_structs() {
        assert_eq!(names("typedef struct __sFILE FILE;"), vec!["FILE"]);
        assert_eq!(names("typedef struct { int a; } Pair;"), vec!["Pair"]);
        // Tags are not type names in C.
        let n = names("typedef struct __sFILE { int a; } FILE;");
        assert_eq!(n, vec!["FILE"]);
        assert!(names("struct OnlyATag { int a; };").is_empty());
    }

    #[test]
    fn function_pointer_typedef() {
        assert_eq!(names("typedef void (*MyCallback)(int, void *);"), vec!["MyCallback"]);
        assert_eq!(names("typedef int (*handler_t)();"), vec!["handler_t"]);
    }

    #[test]
    fn block_typedef() {
        assert_eq!(names("typedef void (^ActionBlock)(int);"), vec!["ActionBlock"]);
    }

    #[test]
    fn array_and_pointer_typedefs() {
        assert_eq!(names("typedef int Row4[4];"), vec!["Row4"]);
        assert_eq!(names("typedef char *string;"), vec!["string"]);
        assert_eq!(names("typedef int **IntPtrPtr;"), vec!["IntPtrPtr"]);
    }

    #[test]
    fn multiple_declarators() {
        let n = names("typedef unsigned int uint, uint32;");
        assert!(n.contains(&"uint".to_string()));
        assert!(n.contains(&"uint32".to_string()));
    }

    #[test]
    fn attribute_noise_is_skipped() {
        assert_eq!(
            names("typedef int FOO __attribute__((aligned(8)));"),
            vec!["FOO"]
        );
    }

    #[test]
    fn comments_and_line_markers_are_ignored() {
        let src = "# 1 \"stdio.h\"\n/* typedef int NotAType; */\ntypedef int Real;\n// typedef int AlsoNot;\n";
        assert_eq!(names(src), vec!["Real"]);
    }

    #[test]
    fn typedef_inside_struct_body_does_not_leak() {
        // The inner `;` is at depth 1, so the outer declaration ends at the
        // final `;` and only `Outer` is harvested.
        let n = names("typedef struct { int a; } Outer;");
        assert_eq!(n, vec!["Outer"]);
    }

    #[test]
    fn nested_function_pointer_parameter() {
        let n = names("typedef void (*Cb)(int (*inner)(char));");
        assert!(n.contains(&"Cb".to_string()));
        assert!(n.contains(&"inner".to_string()));
    }

    #[test]
    fn keywords_are_not_type_names() {
        assert!(names("typedef int;").is_empty());
        assert!(names("typedef struct;").is_empty());
    }
}
