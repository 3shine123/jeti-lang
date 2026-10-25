// Dead code is a build failure, not a warning: a function nobody calls is
// either a bug or a leftover, and both should surface at compile time rather
// than rot unnoticed. Mark intentional exceptions with #[allow(dead_code)]
// and a comment saying who will use it.
#![deny(dead_code)]
use jeti_ast::ast::*;
use jeti_cst::{Diagnostic, Nullability, Severity, TagKind, TypePrim, CstParam};
use jeti_symbol::*;
use std::collections::HashMap;

/// What kind of value a printf-style conversion consumes. Module-level so
/// the `Checker` format helpers can return it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatArgKind {
    /// `%@` — an NPString / object pointer.
    Object,
    /// `%d/%i/%u/%x/%X/%o/%c` — an integer (any width/signedness; width
    /// mismatches like `long` into `%d` are not judged).
    Int,
    /// `%f/%F/%e/%E/%g/%G/%a/%A` — a floating-point value.
    Float,
    /// `%s` — a `char *` C string specifically (an object here is the
    /// classic `%s`/`%@` mixup).
    Str,
    /// `%p` — any pointer, objects included (printing an address is valid).
    Ptr,
}

/// Type checker for Jeti programs.
/// Validates types and reports type errors.
pub struct Checker {
    pub symtab: Option<SymbolTable>,
    pub current_class: Option<String>,
    pub current_method: Option<String>,
    /// True while checking the body of a class method (`+`). In a class method
    /// `self` is the class object (NPClass *), so instance ivars can't be used.
    pub current_method_is_class: bool,
    pub has_error: bool,
    pub error_count: i32,
    /// Structured diagnostics (errors and warnings alike), the single source
    /// of truth the pipeline renders. Kept alongside the legacy string fields
    /// until every consumer reads from here.
    pub diagnostics: Vec<Diagnostic>,
    /// Legacy flat text of all errors, newline-joined — still consumed by
    /// `last_error()`; rendered from `diagnostics` on write.
    pub error_msg: String,
    /// Legacy warning strings, kept for existing consumers; rendered from
    /// `diagnostics` on write.
    pub warnings: Vec<String>,
    pub scope_vars: Vec<Vec<(String, AstType)>>,
    /// Flow-sensitive narrowing: names proven non-null inside the current
    /// branch, innermost scope last. `if (s) { [s length]; }` — `s` is
    /// narrowed in the then-branch, so passing it to a `nonnull` parameter
    /// there is fine and must not warn. A scope stack (not a flat set) so
    /// narrowing cannot leak past the branch that established it.
    pub narrowed: Vec<std::collections::HashSet<String>>,
    pub no_arc: bool,
    pub in_noarc: bool,
    /// Selector → param types (from the AST's method declarations), used to
    /// reject bare C-string literals passed to object-typed parameters.
    pub method_params: HashMap<String, Vec<Option<AstType>>>,
    /// Class name → (selector → param types). Receiver-class-aware signature
    /// lookup: two classes may declare the same selector with different param
    /// types (tt.jeti's `objectForKey:(const char *)` vs NPDictionary's
    /// `objectForKey:(K)key`); the global selector-keyed `method_params` is
    /// last-writer-wins and clobbers one of them, turning a valid send into a
    /// false positive (probe: NPError.jeti `[_userInfo objectForKey:@"…"]`).
    /// The MsgSend arm consults this table first and only falls back to the
    /// global one when the receiver's class is unknown.
    pub method_params_by_class: HashMap<String, HashMap<String, Vec<Option<AstType>>>>,
    /// Selector → param lists from ALL declarations (every class that declares
    /// the selector). Unlike `method_params` (last-writer-wins), this keeps
    /// every declaration so a check can require consensus before firing —
    /// same discipline as `method_kinds` (enforce only when all declarations
    /// agree; mixed declarations leave the effective signature ambiguous).
    pub method_param_decls: HashMap<String, Vec<Vec<Option<AstType>>>>,
    /// Selector → declared return type (from AST method declarations). Used
    /// for generic signature substitution: a specialized receiver's type_args
    /// replace Param (T) positions so the send's result type is the element
    /// type, not erased `id`.
    pub method_returns: HashMap<String, Option<AstType>>,
    /// Translates flattened inline-buffer lines back to (file, source line).
    pub source_map: Option<jeti_cst::source_map::SourceMap>,
    /// Function name → param types (from AST function declarations).
    pub function_params: HashMap<String, Vec<Option<AstType>>>,
    /// Function name → declared return type (same AST pass as
    /// `function_params`). The call family (C17 6.5.2.2) needs the type of a
    /// call expression, not just arity; `None` = not tracked yet.
    pub function_returns: HashMap<String, Option<AstType>>,
    /// Function name → `...` (variadic) flag from the same AST pass. Arity
    /// checks skip variadic functions (C17 6.5.2.2p2).
    pub function_variadic: HashMap<String, bool>,
    /// Typedef aliases whose underlying type is a block (`typedef void
    /// (^SimpleBlock)(void)`). The alias spelling carries no `is_block` flag
    /// on variable types, so the call family's called-object guard needs this
    /// registry to keep `b()` (block invocation through an alias) legal.
    pub block_alias_names: std::collections::HashSet<String>,
    /// Function name → declared parameter names, same order as
    /// `function_params`. Single-parameter functions report clang's
    /// "expected single argument 'a'" form, which quotes the parameter name.
    pub function_param_names: HashMap<String, Vec<Option<String>>>,
    /// Angle-bracket C headers this TU pulled in (`#include <stdio.h>`,
    /// fed by the pipeline from the preprocessor's include lines). Gate for
    /// the stdlib callee whitelist: headers whose `#include` reaches the C
    /// compiler verbatim (passthrough is an escape hatch) still declare
    /// functions the AST never saw, so the "call to undeclared function"
    /// check must consult what was included before firing (§G-expr 调用族).
    pub included_c_headers: Vec<String>,
    /// Any quoted `#include "…h"` in this TU. Quoted headers are the escape
    /// hatch's escape hatch — their declarations are not auditable, so a TU
    /// containing one disables the undeclared-function check entirely
    /// rather than flooding false errors over extern C surface.
    pub has_quoted_c_include: bool,
    /// Depth of clone-evaluation currently running (cast-family and
    /// assignment-family type snapshots recurse through `check_expr` on
    /// cloned nodes; the post-inner diagnostics hooks must not fire there,
    /// or every diagnostic doubles). Nonzero = inside a clone.
    pub in_clone_eval: usize,
    /// Selector → is_class_method (from AST declarations), used to reject
    /// calling a class method on an instance or an instance method on a class.
    /// Selector → is_class_method (from AST declarations), used to reject
    /// calling a class method on an instance or an instance method on a class.
    /// Keyed by `selector{NUL}class_fqn` so two classes may share a selector with
    /// different +/- kinds without clobbering each other (bug #5).
    /// Selector → is_class_method flags from ALL declarations of that
    /// selector. A selector declared consistently `+` or `-` everywhere
    /// allows enforcing the class/instance receiver rule; mixed declarations
    /// (two classes share a selector with different kinds) disable the check
    /// for that selector, since the receiver's class isn't statically known
    /// here — this mirrors ObjC's dynamic dispatch (bug #5).
    pub method_kinds: HashMap<String, Vec<bool>>,
    /// Selector → is_class_method, scoped to the declaring class.
    /// `method_kinds` above is keyed by selector name alone, so two unrelated
    /// classes sharing a selector (`NPRunLoop -run` vs `Circle +run`) feed
    /// each other's flags and the receiver-kind check fires a false positive
    /// on a send that is perfectly legal for its receiver. The receiver's
    /// static class is what decides whether a `+/–` mismatch is real, so the
    /// kind must be looked up along that class's chain — never globally —
    /// whenever the receiver's class is statically known.
    pub method_kinds_by_class: HashMap<String, HashMap<String, bool>>,
    /// Names declared as locals/params inside the current method body. Used to
    /// detect when a local `self` shadows the implicit class-method self.
    pub shadowed_locals: Vec<String>,
    /// Struct tags whose `==`/`!=` were rewritten to `jeti_struct_eq_<tag>`
    /// calls during checking. The pipeline passes this to codegen, which
    /// emits the per-struct comparison functions on demand (values-struct
    /// equality is a generated function because C forbids `a == b` on
    /// structs). Ordered so nested structs emit dependencies first.
    pub struct_eq_tags: Vec<String>,
    /// typedef alias -> underlying struct tag, for
    /// `typedef struct Point { ... } Point;`. The source may spell the type
    /// either way (`struct Point p;` or `Point p;`), but only the tag form
    /// carries `is_struct` on the type node, so without this map a
    /// typedef-named struct is invisible to the `==` rewrite and C rejects
    /// the comparison. Populated in `check()`'s first pass.
    pub struct_alias_tags: HashMap<String, String>,
    /// Typedef alias → the block type it names. A variable declared
    /// `MySink sink = ...;` carries only the alias name in its type node, so
    /// the block signature (params + their annotations) is unreachable without
    /// this map. Foundation and essentially all real block code use typedefs.
    pub typedef_blocks: HashMap<String, AstType>,
    /// Classes seen only as `@class X;` forward declarations in this TU, and
    /// classes whose `@interface` is actually visible here.
    ///
    /// A forward declaration registers a symbol with an empty method set —
    /// exactly the shape an `@interface` that declares nothing has — so the
    /// symbol table alone cannot tell "declares nothing" from "declaration
    /// lives in another TU". Only the latter is unknowable, and reporting "no
    /// visible method" there is a guess (baremetal's `Metal::Ghost` is forward
    /// declared and only ever messaged as nil). `class_declares_selector`
    /// consults both sets to answer None instead.
    pub forward_declared_classes: std::collections::HashSet<String>,
    pub visible_interface_classes: std::collections::HashSet<String>,
    /// Enum tag name → member list, collected from THIS TU's `enum` decls
    /// (including ones nested in namespaces and class bodies). Powers switch
    /// exhaustiveness: a switch over a *known* enum with no `default:` must
    /// cover every member. C's `-Wswitch` only reaches the same guarantee
    /// when the subject spells the enum type directly; collecting here lets
    /// the checker prove it from the declaration table instead of guessing.
    pub enum_members: HashMap<String, Vec<String>>,
    /// Enum member name → folded integer value (`RED` → 1). The parser
    /// carries explicit `= N` values and the implicit `previous + 1`
    /// increments for every member, so the checker can constant-fold case
    /// labels spelled as enum members — clang flags `case RED` duplicating
    /// `case 1` when both equal 1 (probe_switch ⑦ measured).
    pub enum_member_values: HashMap<String, i64>,
    /// typedef alias → enum tag name (`typedef enum Tag {...} Alias;`), the
    /// same shape as `struct_alias_tags`. The alias spelling is what a
    /// variable's type node carries, so the exhaustiveness check resolves
    /// `Alias` through this map to the member table keyed by the tag.
    pub enum_alias_tags: HashMap<String, String>,
    /// `throws` annotation of the declaration whose body is being checked.
    /// `None` = not annotated; `Some(None)` = bare `throws` ("declares it
    /// throws, type unstated"); `Some(Some(T))` = `throws(T)`. Saved and
    /// restored around each body, like `shadowed_locals`.
    throws_ann: Option<Option<AstType>>,
    /// Every `@throw` executed by the body being checked: position plus a
    /// best-effort static type (`None` = cannot say).
    thrown: Vec<(usize, usize, Option<AstType>)>,
    /// Positions of `@throw`s that leave the body without a local `@try`
    /// catching them — only those need a `throws` annotation.
    uncaught_throws: Vec<(usize, usize)>,
    /// Nesting depth of local `@try` blocks that have a `@catch`.
    try_depth: usize,
    /// `-eh checked` rewrites `@throw` into a flag-and-return *before* the
    /// checker runs, so no `@throw` statement survives to reconcile; the bare
    /// "must really throw" rule is suspended there.
    pub eh_checked: bool,
}

/// The C17 library function surface, curated from the library clause (clang
/// sources are off limits) — one static table, two consumers: the
/// undeclared-function whitelist gate (header-gated by what the TU included)
/// and the LSP hover cards ([`c_stdlib_function_header`]).
static C_STDLIB_FUNCTIONS: &[(&str, &[&str])] = &[
    ("stdio.h", &[
        "printf", "fprintf", "sprintf", "snprintf", "vprintf", "vfprintf",
        "vsprintf", "vsnprintf", "puts", "putchar", "fputs", "fputc",
        "fgets", "fgetc", "getc", "putc", "getchar", "gets", "ungetc",
        "fopen", "fclose", "fflush", "fread", "fwrite", "fseek", "ftell",
        "rewind", "feof", "ferror", "remove", "rename", "tmpfile",
        "perror", "getline", "sscanf", "fscanf", "scanf",
        "setvbuf", "setbuf", "fsetpos", "fgetpos", "fileno", "fdopen",
    ]),
    ("stdlib.h", &[
        "malloc", "calloc", "realloc", "free", "abort", "exit", "_Exit",
        "atexit", "at_quick_exit", "quick_exit", "rand", "srand",
        "qsort", "bsearch", "abs", "labs", "llabs", "div", "ldiv",
        "getenv", "setenv", "putenv", "unsetenv", "system", "strtol",
        "strtoul", "strtoll", "strtoull", "strtod", "strtof", "strtold",
        "atoi", "atol", "atoll", "strdup", "mkstemp", "mkdtemp", "rand_r",
    ]),
    ("string.h", &[
        "strcpy", "strncpy", "strcat", "strncat", "strcmp", "strncmp",
        "strchr", "strrchr", "strstr", "strlen", "memset", "memcmp",
        "memcpy", "memmove", "memchr", "strtok", "strerror", "strspn",
        "strcspn", "strpbrk", "strcoll", "strxfrm",
    ]),
    ("time.h", &[
        "time", "clock", "localtime", "gmtime", "mktime", "strftime",
        "difftime", "nanosleep", "timegm",
    ]),
    ("ctype.h", &[
        "isdigit", "isalpha", "isalnum", "isspace", "isupper", "islower",
        "ispunct", "isprint", "isgraph", "iscntrl", "isxdigit", "toupper",
        "tolower", "isblank",
    ]),
    ("stdarg.h", &["va_start", "va_arg", "va_end", "va_copy"]),
    ("math.h", &[
        "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "exp",
        "log", "log2", "log10", "pow", "sqrt", "cbrt", "fmod", "ceil",
        "floor", "fabs", "round", "rint", "trunc", "fmin", "fmax",
        "hypot", "copysign", "nan", "nearbyint",
    ]),
    ("setjmp.h", &["setjmp", "longjmp"]),
    ("unistd.h", &[
        "unlink", "getpid", "getppid", "getuid", "getgid", "getopt",
        "access", "dup", "dup2", "isatty", "ttyname", "fsync", "fork",
        "execve", "usleep", "getcwd", "chdir", "read", "write", "close",
    ]),
    ("signal.h", &[
        "signal", "raise", "kill", "sigemptyset", "sigaddset",
        "sigfillset", "sigprocmask", "sigismember", "sigdelset",
        "sigaction", "sigpending", "sigsuspend",
    ]),
    ("dirent.h", &["opendir", "readdir", "closedir"]),
    ("pthread.h", &[
        "pthread_create", "pthread_join", "pthread_detach", "pthread_exit",
        "pthread_mutex_lock", "pthread_mutex_unlock", "pthread_self",
        "pthread_cancel", "pthread_cond_wait", "pthread_cond_signal",
        "pthread_attr_init", "pthread_attr_destroy", "pthread_mutex_init",
        "pthread_mutex_destroy", "pthread_cond_init", "pthread_cond_destroy",
    ]),
    ("complex.h", &[
        "creal", "cimag", "crealf", "cimagf", "cabs", "cabsf", "carg",
        "cargf", "conj", "conjf",
    ]),
    ("dlfcn.h", &[
        "dlopen", "dlsym", "dlvsym", "dlerror", "dlclose", "dladdr",
        "dlinfo", "dlmopen",
    ]),
];

/// The C17 library *binding* surface (globals and macro constants) — same
/// two consumers as [`C_STDLIB_FUNCTIONS`].
static C_STDLIB_BINDINGS: &[(&str, &[&str])] = &[
    ("errno.h", &[
        "errno", "EPERM", "ENOENT", "ESRCH", "EINTR", "EIO", "ENXIO", "E2BIG",
        "ENOEXEC", "EBADF", "ECHILD", "EAGAIN", "ENOMEM", "EACCES", "EFAULT",
        "EBUSY", "EEXIST", "EXDEV", "ENODEV", "ENOTDIR", "EISDIR", "EINVAL",
        "ENFILE", "EMFILE", "ENOTTY", "EFBIG", "EPIPE", "EDOM", "ERANGE",
        "EDEADLK", "ENAMETOOLONG", "ENOLCK", "ENOSYS", "ENOTEMPTY", "ELOOP",
        "EWOULDBLOCK", "ETIMEDOUT", "ECONNRESET", "ECONNREFUSED",
        "EADDRINUSE", "EINPROGRESS", "EHOSTUNREACH", "ENETUNREACH",
    ]),
    ("stdio.h", &[
        "stdin", "stdout", "stderr", "EOF", "FILE", "BUFSIZ", "SEEK_SET",
        "SEEK_CUR", "SEEK_END",
    ]),
    ("unistd.h", &[
        "STDIN_FILENO", "STDOUT_FILENO", "STDERR_FILENO", "environ",
        "optarg", "optind", "opterr", "optopt",
    ]),
    ("termios.h", &[
        "ECHO", "ECHONL", "ECHOE", "ECHOK", "ICANON", "ISIG", "IEXTEN",
        "OPOST", "ONLCR", "O_NONBLOCK", "VMIN", "VTIME", "TCSANOW",
        "TCSADRAIN", "TCSAFLUSH", "IGNBRK", "BRKINT", "PARMRK", "ISTRIP",
        "INLCR", "IGNCR", "ICRNL", "IXON", "IXOFF", "PARENB", "PARODD",
        "CS7", "CS8", "CSTOPB", "CREAD", "CLOCAL", "TIOCGWINSZ",
    ]),
    ("fcntl.h", &[
        "O_RDONLY", "O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND",
        "O_EXCL", "O_NONBLOCK", "O_SYNC", "F_GETFL", "F_SETFL", "F_GETFD",
        "F_SETFD",
    ]),
    ("signal.h", &[
        "SIGINT", "SIGTERM", "SIGKILL", "SIGSEGV", "SIGHUP", "SIGPIPE",
        "SIGABRT", "SIGQUIT", "SIG_DFL", "SIG_IGN", "SIG_ERR",
    ]),
    ("limits.h", &[
        "CHAR_BIT", "SCHAR_MIN", "SCHAR_MAX", "UCHAR_MAX", "CHAR_MIN",
        "CHAR_MAX", "SHRT_MIN", "SHRT_MAX", "USHRT_MAX", "INT_MIN",
        "INT_MAX", "UINT_MAX", "LONG_MIN", "LONG_MAX", "ULONG_MAX",
        "LLONG_MIN", "LLONG_MAX", "ULLONG_MAX", "SIZE_MAX", "SSIZE_MAX",
        "INT8_MAX", "INT16_MAX", "INT32_MAX", "INT64_MAX", "UINT8_MAX",
        "UINT16_MAX", "UINT32_MAX", "UINT64_MAX", "INT32_MIN", "INT64_MIN",
        "PATH_MAX", "NAME_MAX",
    ]),
    ("math.h", &[
        "M_PI", "M_E", "M_LOG2E", "M_SQRT2", "INFINITY", "NAN", "HUGE_VAL",
        "HUGE_VALF",
    ]),
    ("time.h", &["CLOCKS_PER_SEC"]),
    ("stdlib.h", &["EXIT_SUCCESS", "EXIT_FAILURE", "RAND_MAX"]),
    ("dlfcn.h", &[
        "RTLD_LAZY", "RTLD_NOW", "RTLD_GLOBAL", "RTLD_LOCAL",
        "RTLD_DEFAULT", "RTLD_NEXT", "RTLD_SELF",
    ]),
];

/// Which C17 header declares this function — the LSP hover card's
/// provenance line. Header-agnostic on purpose: the caller decides whether
/// the TU actually pulled the header in.
pub fn c_stdlib_function_header(name: &str) -> Option<&'static str> {
    C_STDLIB_FUNCTIONS.iter()
        .find(|(_, table)| table.contains(&name))
        .map(|(header, _)| *header)
}

/// Which C17 header declares this binding (global/macro constant) — same
/// provenance contract as [`c_stdlib_function_header`].
pub fn c_stdlib_binding_header(name: &str) -> Option<&'static str> {
    C_STDLIB_BINDINGS.iter()
        .find(|(_, table)| table.contains(&name))
        .map(|(header, _)| *header)
}

impl Checker {
    pub fn new(symtab: Option<SymbolTable>) -> Self {
        Checker {
            symtab,
            current_class: None,
            current_method: None,
            current_method_is_class: false,
            has_error: false,
            error_count: 0,
            diagnostics: Vec::new(),
            error_msg: String::new(),
            warnings: Vec::new(),
            scope_vars: Vec::new(),
            narrowed: Vec::new(),
            no_arc: false,
            in_noarc: false,
            method_params: HashMap::new(),
            method_params_by_class: HashMap::new(),
            method_param_decls: HashMap::new(),
            method_returns: HashMap::new(),
            function_params: HashMap::new(),
            function_returns: HashMap::new(),
            function_variadic: HashMap::new(),
            function_param_names: HashMap::new(),
            block_alias_names: std::collections::HashSet::new(),
            included_c_headers: Vec::new(),
            has_quoted_c_include: false,
            in_clone_eval: 0,
            method_kinds: HashMap::new(),
            method_kinds_by_class: HashMap::new(),
            shadowed_locals: Vec::new(),
            struct_eq_tags: Vec::new(),
            struct_alias_tags: HashMap::new(),
            typedef_blocks: HashMap::new(),
            forward_declared_classes: std::collections::HashSet::new(),
            visible_interface_classes: std::collections::HashSet::new(),
            enum_members: HashMap::new(),
            enum_member_values: HashMap::new(),
            enum_alias_tags: HashMap::new(),
            throws_ann: None,
            thrown: Vec::new(),
            uncaught_throws: Vec::new(),
            try_depth: 0,
            eh_checked: false,
            source_map: None,
        }
    }

    pub fn has_error(&self) -> bool {
        self.has_error
    }

    pub fn last_error(&self) -> &str {
        &self.error_msg
    }

    /// Warning subset of [`Self::diagnostics`], for `-Werror` rendering.
    pub fn warnings_as_diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .cloned()
            .collect()
    }

    /// Error subset of [`Self::diagnostics`], for the fatal-failure render.
    pub fn errors_as_diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .cloned()
            .collect()
    }

    fn check_error(&mut self, line: usize, col: usize, msg: &str) {
        self.record(Severity::Error, line, col, None, msg);
    }

    /// Span-aware entry point: `end_col` is the exclusive end of the
    /// annotated range on the same line (`None` = single caret). Call sites
    /// graduate to this one as spans are threaded through (span-widening work).
    fn check_error_span(&mut self, line: usize, col: usize, end_col: Option<usize>, msg: &str) {
        self.record(Severity::Error, line, col, end_col, msg);
    }

    /// Annotated end column for a node whose start is `(line, col)`: the node's
    /// own end when it is on the same line and actually wider, `None` otherwise
    /// (synthesized nodes carry `end == 0`, multi-line nodes cannot express
    /// their end as one column on the start line) — a single caret is the
    /// honest annotation then.
    fn span_end(line: usize, col: usize, end_line: usize, end_col: usize) -> Option<usize> {
        if end_line == line && end_col > col {
            Some(end_col)
        } else {
            None
        }
    }

    /// Record an error annotated over a node's whole extent, so editors and the
    /// rendered CLI diagnostics underline the construct the message is about
    /// instead of a single caret.
    fn check_error_over(
        &mut self,
        line: usize,
        col: usize,
        end_line: usize,
        end_col: usize,
        msg: &str,
    ) {
        let end = Self::span_end(line, col, end_line, end_col);
        self.check_error_span(line, col, end, msg);
    }

    fn check_warning(&mut self, line: usize, col: usize, msg: &str) {
        self.record(Severity::Warning, line, col, None, msg);
    }

    /// Warning counterpart of [`Checker::check_error_over`]: annotate over the
    /// node's whole extent when it has one on a single line.
    fn check_warning_over(
        &mut self,
        line: usize,
        col: usize,
        end_line: usize,
        end_col: usize,
        msg: &str,
    ) {
        let end = Self::span_end(line, col, end_line, end_col);
        self.record(Severity::Warning, line, col, end, msg);
    }

    /// The single recording path. Resolves (file, line) through the SourceMap
    /// and stores a structured `Diagnostic`; the legacy flat strings are kept
    /// in sync for `last_error()`/`warnings` consumers.
    fn record(&mut self, severity: Severity, line: usize, col: usize, end_col: Option<usize>, msg: &str) {
        let (file, real_line) = self.resolve_line(line);
        let mut d = match severity {
            Severity::Error => Diagnostic::error(file, real_line, col, msg),
            Severity::Warning => Diagnostic::warning(file, real_line, col, msg),
            Severity::Note => Diagnostic::note(file, real_line, col, msg),
        };
        d.end_col = end_col;
        self.diagnostics.push(d.clone());
        if severity == Severity::Error {
            self.has_error = true;
            self.error_count += 1;
            if self.error_msg.is_empty() {
                self.error_msg = d.to_plain();
            } else {
                self.error_msg = format!("{}\n{}", self.error_msg, d.to_plain());
            }
        } else if severity == Severity::Warning {
            self.warnings.push(d.to_plain());
        }
    }

    /// Resolves a flattened inline-buffer line to (file, source line) via the
    /// SourceMap; falls back to the raw line when no map is set.
    fn resolve_line(&self, line: usize) -> (String, usize) {
        if let Some(ref sm) = self.source_map {
            if !sm.is_empty() {
                let (file, real_line) = sm.locate(line);
                if !file.is_empty() {
                    return (file, real_line as usize);
                }
            }
        }
        (String::new(), line)
    }

    /// Does this diagnostic's line come from a Foundation source file
    /// (include/Foundation/*.jeti)? The selector-visibility check stays
    /// silent there — umbrella inlining plus our own sources made every
    /// report from that path noise.
    fn is_foundation_source(&self, line: usize) -> bool {
        if let Some(ref sm) = self.source_map {
            if !sm.is_empty() {
                let (file, _) = sm.locate(line);
                return file.contains("/Foundation/") || file.starts_with("Foundation/");
            }
        }
        false
    }

    /// Is this a Foundation/object pointer type (id, instancetype, or a named
    /// class pointer like `NPString *`)? Used to reject bare C-string literals
    /// (`"..."`) where an NPString/object is expected — mirroring ObjC, where
    /// `NSLog(NSString *, ...)` rejects `const char *` at the C type level.
    fn is_object_type(t: &AstType) -> bool {
        t.prim == TypePrim::Id
            || t.prim == TypePrim::Instancetype
            || (t.prim == TypePrim::Named && t.is_pointer)
    }

    /// The class name behind an object-pointer type (`NPDictionary *` →
    /// "NPDictionary", generic args stripped). `id`/instancetype → None.
    fn named_class_of(t: &AstType) -> Option<String> {
        if !t.is_pointer || t.prim != TypePrim::Named {
            return None;
        }
        t.name.as_ref().map(|n| n.split('<').next().unwrap_or(n).to_string())
    }

    /// Parse a specialized class-name receiver spelling into its type:
    /// `Box<NPString *>` → Named "Box" with type_args [NPString *], pointer
    /// set (class-name receivers are class objects, but only the type_args
    /// matter here — substitution reads `type_args` and strips `<...>` for
    /// the base name). Bare names return None: no args, nothing to substitute.
    fn parse_generic_receiver_name(name: &str) -> Option<AstType> {
        let lt = name.find('<')?;
        let base = name[..lt].trim().to_string();
        if base.is_empty() { return None; }
        let end = name.rfind('>')?;
        if end < lt { return None; }
        let args_str = &name[lt + 1..end];
        let mut args = Vec::new();
        // Split top-level commas (depth-aware for nested generics).
        let mut depth = 0usize;
        let mut cur = String::new();
        for ch in args_str.chars() {
            match ch {
                '<' => { depth += 1; cur.push(ch); }
                '>' => { depth = depth.saturating_sub(1); cur.push(ch); }
                ',' if depth == 0 => { args.push(cur.trim().to_string()); cur.clear(); }
                _ => cur.push(ch),
            }
        }
        if !cur.trim().is_empty() { args.push(cur.trim().to_string()); }
        if args.is_empty() { return None; }
        // Render each arg string into an AstType (pointer suffix / nested
        // generics), the same shapes codegen's parse_generic_type_string
        // accepts.
        let render = |s: &str| -> AstType {
            let s = s.trim();
            let (base, is_ptr) = match s.strip_suffix('*') {
                Some(b) => (b.trim().to_string(), true),
                None => (s.to_string(), false),
            };
            let mut t = if let Some(lt2) = base.find('<') {
                if base.ends_with('>') {
                    let outer = AstType::new(TypePrim::Named);
                    let mut t2 = outer;
                    t2.name = Some(base[..lt2].trim().to_string());
                    // Nested args keep their rendered spelling in the name;
                    // deep nesting is rare and the checker only needs
                    // element identity, not full structure.
                    t2.is_pointer = is_ptr;
                    t2
                } else {
                    let mut t2 = AstType::new(TypePrim::Named);
                    t2.name = Some(base.clone());
                    t2.is_pointer = is_ptr;
                    t2
                }
            } else {
                let mut t2 = AstType::new(TypePrim::Named);
                t2.name = Some(base);
                t2.is_pointer = is_ptr;
                t2
            };
            t.class_ref = t.name.clone();
            t
        };
        let mut t = AstType::new(TypePrim::Named);
        t.name = Some(base);
        t.class_ref = t.name.clone();
        t.is_pointer = true;
        t.type_args = args.iter().map(|a| render(a)).collect();
        Some(t)
    }

    /// `+`/`-` kinds of `selector` as declared by `class_name` or its
    /// superclass chain. None when no class in the chain declares it — the
    /// caller must stay silent then, never fall back to the global
    /// selector-keyed table (that is exactly the cross-class confusion this
    /// table exists to end).
    fn method_kinds_along_chain(&self, class_name: &str, selector: &str) -> Option<Vec<bool>> {
        let mut kinds = Vec::new();
        let mut cur = class_name.to_string();
        for _ in 0..32 {
            if let Some(k) = self.method_kinds_by_class.get(&cur).and_then(|m| m.get(selector)) {
                kinds.push(*k);
            }
            let sup = self.symtab.as_ref()
                .and_then(|st| st.find_class(&cur))
                .and_then(|c| match &c.data {
                    SymbolData::Class { superclass: Some(s), .. } if !s.is_empty() => Some(s.clone()),
                    _ => None,
                });
            match sup {
                Some(s) => cur = s,
                None => break,
            }
        }
        if kinds.is_empty() { None } else { Some(kinds) }
    }

    /// The receiver's static class when it is statically known: a typed
    /// expression, a typed local/param, or (for a `+` send) a class-name
    /// receiver. None for `id` / protocol-qualified / unresolved receivers,
    /// where every class-scoped judgement must stay silent.
    fn receiver_static_class(&self, receiver: &AstExpr, is_class_method: bool) -> Option<String> {
        if let Some(t) = receiver.expr_type.as_ref() {
            if let Some(c) = Self::named_class_of(t) {
                return Some(c);
            }
        }
        if let AstExprData::VarRef { name, .. } = &receiver.data {
            if is_class_method
                && self.symtab.as_ref().and_then(|st| st.find_class(name)).is_some()
            {
                return Some(name.clone());
            }
            if let Some(t) = self.lookup_scope_var_type(name) {
                if let Some(c) = Self::named_class_of(&t) {
                    return Some(c);
                }
            }
        }
        None
    }

    /// Param types for `selector` as declared by `class_name` or its
    /// superclass chain. Returns None when no class in the chain declares the
    /// selector (the caller falls back to the global selector-keyed table).
    fn method_params_for_receiver(&self, class_name: &str, selector: &str) -> Option<Vec<Option<AstType>>> {
        let mut cur = class_name.to_string();
        for _ in 0..32 {
            if let Some(m) = self.method_params_by_class.get(&cur) {
                if let Some(v) = m.get(selector) {
                    return Some(v.clone());
                }
            }
            let sup = self.symtab.as_ref()
                .and_then(|st| st.find_class(&cur))
                .and_then(|c| match &c.data {
                    SymbolData::Class { superclass: Some(s), .. } if !s.is_empty() => Some(s.clone()),
                    _ => None,
                });
            match sup {
                Some(s) => cur = s,
                None => return None,
            }
        }
        None
    }

    /// Does `class_name` (or any superclass) declare an instance method
    /// `selector`? Consults the per-class param table (same source the
    /// receiver-aware signature lookup uses) along the superclass chain.
    /// Returns None when the class itself is unknown (forward shell / other
    /// TU) — the caller must stay silent in that case.
    fn class_declares_selector(&self, class_name: &str, selector: &str) -> Option<bool> {
        let st = self.symtab.as_ref()?;
        if st.find_class(class_name).is_none() {
            return None; // unresolved class: cannot judge
        }
        // A class that exists here only as `@class X;` has no method set to
        // consult — its `@interface` lives in another TU. The symbol is present
        // (so the check above passes) but empty, which would otherwise read as
        // "declares nothing" and turn a legal send into a false positive. Not
        // visible is not the same as not declared.
        if self.forward_declared_classes.contains(class_name)
            && !self.visible_interface_classes.contains(class_name)
        {
            return None;
        }
        let mut cur = class_name.to_string();
        for _ in 0..32 {
            if self.method_params_by_class.get(&cur)
                .map(|m| m.contains_key(selector))
                .unwrap_or(false)
            {
                return Some(true);
            }
            let sup = st.find_class(&cur).and_then(|c| match &c.data {
                SymbolData::Class { superclass: Some(s), .. } if !s.is_empty() => Some(s.clone()),
                _ => None,
            });
            match sup {
                Some(s) => cur = s,
                None => return Some(false),
            }
        }
        Some(false)
    }

    /// Is `selector` declared anywhere in this unit — on any class (via the
    /// per-class table, which the binder also fills with protocol methods
    /// propagated into `@interface`) or in any `@protocol`? This is the last
    /// resort for receivers that have no static class chain to walk (`id`,
    /// `instancetype`, `id<P>`, forward-declared types). Only a selector that
    /// is declared *nowhere* is reported: with no class to consult, anything
    /// weaker would turn legal code into a false positive.
    fn selector_declared_anywhere(&self, selector: &str) -> bool {
        if self.method_param_decls.contains_key(selector) {
            return true;
        }
        // Protocol symbols store the binder's canonical selector spelling —
        // external parameter names concatenated, i.e. every colon dropped
        // (`taskAction:` → `taskAction`). Normalize before comparing; a
        // colon-stripped comparison also covers zero-parameter selectors,
        // which carry no colon in the first place.
        let flat: String = selector.chars().filter(|c| *c != ':').collect();
        if let Some(ref st) = self.symtab {
            for sym in &st.global.symbols {
                if let SymbolData::Protocol { required_methods, optional_methods, .. } = &sym.data {
                    if required_methods.iter().any(|m| *m == flat)
                        || optional_methods.iter().any(|m| *m == flat)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Is `selector` really an ivar name on some class in this unit? Jeti keeps
    /// the C bracket form usable for field access — `[obj _bodyX]` yields the
    /// ivar (an array there, whose `[0]` the next bracket subscripts). Such a
    /// send never dispatches through a vtable slot, so the receiver-visibility
    /// checks must not judge it by method visibility.
    fn selector_is_known_ivar(&self, selector: &str) -> bool {
        if let Some(ref st) = self.symtab {
            for sym in &st.global.symbols {
                if let SymbolData::Class { ivars, .. } = &sym.data {
                    if ivars.iter().any(|i| i == selector) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Is `selector` declared on some class that derives from `class_name`?
    /// This is the downcast-safe-dispatch allowance: the receiver is typed as
    /// an ancestor, but a subclass in this unit derives from it *and* declares
    /// the selector — `NPObject *o; [o speak]` with `Dog : NPObject` declaring
    /// `speak` (the guarded `respondsToSelector:` idiom, and the
    /// protocol-qualified root receivers written `NPObject<P> *`). Sibling
    /// classes never qualify, so a `Mute *` sending `Talker`'s method stays an
    /// error — that shape is a bug, not dynamic dispatch.
    fn a_subclass_declares_selector(&self, class_name: &str, selector: &str) -> bool {
        let st = match self.symtab.as_ref() {
            Some(st) => st,
            None => return false,
        };
        for (decl_class, methods) in &self.method_params_by_class {
            if !methods.contains_key(selector) {
                continue;
            }
            let mut cur = decl_class.clone();
            for _ in 0..32 {
                if cur == class_name {
                    return true;
                }
                let sup = st.find_class(&cur).and_then(|c| match &c.data {
                    SymbolData::Class { superclass: Some(s), .. } if !s.is_empty() => Some(s.clone()),
                    _ => None,
                });
                match sup {
                    Some(s) => cur = s,
                    None => break,
                }
            }
        }
        false
    }

    /// Selectors exempt from the receiver-class visibility check because they
    /// never dispatch through the receiver's own vtable slot:
    /// `respondsToSelector:` is a compiler pseudo-method lowered to a
    /// NULL-slot probe on the uniform vtable, and the rest are the
    /// runtime-implicit root methods codegen synthesizes for every class
    /// (the same set codegen's KVC collector excludes as "runtime/infra").
    fn is_vtable_exempt_selector(selector: &str) -> bool {
        matches!(selector,
            "respondsToSelector:"
            | "conformsToProtocol:"
            | "init" | "copy" | "mutableCopy" | "retain" | "release"
            | "autorelease" | "dealloc" | "class" | "description")
    }

    /// Does EVERY declaration of `selector` agree on a plain C string
    /// (`char *` pointer) parameter at position `idx`? False when any
    /// declaration disagrees or has no such param — the conservative
    /// direction: mixed declarations (tt.jeti's `objectForKey:(const char *)`
    /// vs NPDictionary's `objectForKey:(K)key`) must not turn a valid send
    /// into a false positive, same discipline as `method_kinds`.
    fn selector_param_all_cstr(&self, selector: &str, idx: usize) -> bool {
        match self.method_param_decls.get(selector) {
            Some(decls) if !decls.is_empty() => decls.iter().all(|params| {
                params.get(idx)
                    .and_then(|p| p.as_ref())
                    .map_or(false, |t| t.prim == TypePrim::Char && t.is_pointer)
            }),
            _ => false,
        }
    }

    /// Reverse of the bare-C-string check: an `@"..."` object literal passed
    /// where a plain C string (`char *` / `const char *`) is expected compiles
    /// (the pointer just flows through) but reads the NPString object header
    /// as character data — silent garbage at runtime (probe:
    /// `stringWithUTF8String:@"bad"` printed junk). Mirrors clang's
    /// "incompatible pointer types sending 'NSString *' to parameter of type
    /// 'char *'". Only character pointers are rejected (conservative, zero
    /// false positives); `void *` and object params keep C-level tolerance.
    /// Fix hint: `[str UTF8String]` hands over the char data.
    fn check_atstring_to_cstr(&mut self, pt: &AstType, a: &AstExpr) {
        if matches!(a.data, AstExprData::AtString(_))
            && pt.prim == TypePrim::Char
            && pt.is_pointer
        {
            self.check_error_over(a.line, a.col, a.end_line, a.end_col,
                "cannot pass an @\"...\" object where a C string ('char *') is expected — use [str UTF8String]");
        }
    }

    // ─── C17 cast diagnostics (6.5.4 / 6.3.2.3) ─────────────────────────
    //
    // Pointer width is assumed 64 bits (LP64 — hosted ARM64 and bare-metal
    // ARM64 share this assumption); 32-bit targets would narrow which casts
    // warn (corpus-phase TODO, same metric as the plan doc).

    /// Pointer-like operands for the cast family: pointer declarators and
    /// the pointer-flavored ObjC primitives (`id`/`Class`/`SEL` carry their
    /// star inside the primitive spelling, not `is_pointer`).
    fn is_pointer_like(t: &AstType) -> bool {
        t.is_pointer
            || matches!(t.prim, TypePrim::Id | TypePrim::Class | TypePrim::Sel)
    }

    /// Integer primaries for the cast family (excludes pointer/block/array/
    /// struct spellings). `BOOL` is a 4-byte integer here; typedef'd sugar
    /// spellings keep their names — corpus phase measures clang's rendering.
    fn is_integral_type(t: &AstType) -> bool {
        if Self::is_pointer_like(t) || t.is_block || t.is_array || t.is_struct {
            return false;
        }
        matches!(
            t.prim,
            TypePrim::Char | TypePrim::Short | TypePrim::Int | TypePrim::Long
                | TypePrim::LongLong | TypePrim::Bool | TypePrim::Signed
                | TypePrim::Unsigned
        )
    }

    /// Bit width of the integer primaries. `-1` width = not integral here.
    fn integral_bit_width(t: &AstType) -> i32 {
        match t.prim {
            TypePrim::Char => 8,
            TypePrim::Short => 16,
            TypePrim::Int | TypePrim::Bool | TypePrim::Signed | TypePrim::Unsigned => 32,
            TypePrim::Long | TypePrim::LongLong => 64,
            _ => -1,
        }
    }

    /// Spell a type in pointer position the way clang does in this family's
    /// messages: `void *` always carries its star; `id`/`Class`/`SEL` are
    /// pointer-spelled primitives; everything else goes through the shared
    /// type display (which appends ` *` for `is_pointer`).
    fn cast_pointer_spelling(t: &AstType) -> String {
        match t.prim {
            TypePrim::Void => "void *".to_string(),
            TypePrim::Id => "id".to_string(),
            TypePrim::Class => "Class".to_string(),
            TypePrim::Sel => "SEL".to_string(),
            _ => Self::type_display(t),
        }
    }

    /// Integer constants are exempt: clang stays silent on `(char *)123`
    /// (probe-measured — literals are the deliberate escape hatch for
    /// sentinel addresses).
    fn is_int_literal_expr(e: &AstExpr) -> bool {
        match &e.data {
            AstExprData::Int(_) | AstExprData::Char(_) | AstExprData::Bool(_)
            | AstExprData::Float(_) | AstExprData::FloatRaw(_) => true,
            AstExprData::Paren(inner) => Self::is_int_literal_expr(inner),
            _ => false,
        }
    }

    // ─── C17 6.5.2.2 call family / 6.5.16 assignment family / 6.8.4.2
    //     switch family ──────────────────────────────────────────────────
    //
    // All three land clang-measured wording/severity. Shared gates first —
    // synthesized spans and Foundation's own sources stay silent, exactly
    // like the cast family above.
    fn family_span_gates(&self, line: usize, end_line: usize, end_col: usize) -> bool {
        if end_line == 0 || end_col == 0 { return true; }
        if self.is_foundation_source(line) { return true; }
        false
    }

    /// Does the type name resolve to a known `enum` in this TU? Enum-typed
    /// switch subjects are legal integer-ish subjects (switch exhaustiveness
    /// is a separate, already-shipped check).
    fn is_enum_type(&self, t: &AstType) -> bool {
        t.tag == TagKind::Enum
            || t.name.as_ref().is_some_and(|n| {
                self.enum_members.contains_key(n)
                    || self.enum_alias_tags.contains_key(n)
            })
    }

    // ─── call family (C17 6.5.2.2) ──────────────────────────────────────
    //
    // clang-measured (probe_call / probe2 / probe3, clang 18, LP64):
    //   `too few arguments to function call, expected 2, have 1`      error, caret at `)`
    //   `too many arguments to function call, expected 3, have 4`     error, caret at `)`
    //   `too many arguments to function call, expected single argument 'a',
    //      have 2 arguments`                                          error (1-param fns quote the name)
    //   `called object type 'int' is not a function or function pointer` error, caret at `(`
    //   `call to undeclared function 'f'; ISO C99 and later do not support
    //      implicit function declarations`                            error, caret at name
    fn check_call_family(&mut self, e: &AstExpr) {
        if Self::family_span_gates(self, e.line, e.end_line, e.end_col) { return; }
        let Some((name, arg_count)) = (match &e.data {
            AstExprData::FuncCall { name, args, .. } => Some((name.clone(), args.len())),
            _ => None,
        }) else { return };
        // Callee-expression calls (`d.fn(9)` — a member function pointer,
        // chained calls): no spelled name, the callee is checked recursively
        // and the name-based undeclared test cannot apply.
        if name.is_empty() { return; }
        // Innermost-name-wins: a local/param/global of the same name shadows
        // any function. Blocks (direct or typedef-aliased) and fn pointers
        // are proper callees (skip); everything else fails the "called
        // object" test in C.
        if let Some(ty) = self.lookup_scope_var_type(&name) {
            if ty.is_block || ty.is_fn_ptr { return; } // proper block invocation
            if ty.prim == TypePrim::Named
                && ty.name.as_ref().is_some_and(|n| self.block_alias_names.contains(n))
            {
                return; // block invocation through a typedef alias
            }
            self.check_error(e.line, e.col + name.len(), &format!(
                "called object type '{}' is not a function or function pointer",
                Self::type_display(&ty)));
            return;
        }
        let Some(expected_types) = self.function_params.get(&name).cloned() else {
            // Not a declared function → callee type/declaration checks.
            if name.starts_with("__") || name.starts_with("jeti_") || name.starts_with('&') {
                return; // compiler-internal namespace, never diagnose
            }
            if let Some(ref st) = self.symtab {
                if st.lookup(&name).is_some() { return; }
            }
            // Passthrough headers and quoted includes declare functions the
            // AST never saw (escape hatch, not the normal channel — §1 决策).
            // The quoted-include escape hatch is un-auditable in total, so it
            // disables the check for this TU; angle headers are gated through
            // the curated C17-library table.
            if self.has_quoted_c_include || Self::c_stdlib_declares(self, &name) { return; }
            self.check_error(e.line, e.col, &format!(
                "call to undeclared function '{}'; ISO C99 and later do not support implicit function declarations",
                name));
            return;
        };
        if *self.function_variadic.get(&name).unwrap_or(&false) { return; }
        if Self::refs_jeti_synth(e) { return; }
        if expected_types.is_empty() && arg_count == 1 {
            // Ambiguous empty signature (parser dropped the parameter chain):
            // stay silent rather than guess.
            return;
        }
        // Caret at the closing parenthesis — measured on three shapes.
        let caret = if e.end_line == e.line { e.end_col.saturating_sub(1) } else { e.end_col };
        if arg_count < expected_types.len() {
            self.check_error(e.end_line, caret, &format!(
                "too few arguments to function call, expected {}, have {}",
                expected_types.len(), arg_count));
        } else if arg_count > expected_types.len() {
            if expected_types.len() == 1 {
                let pname = self.function_param_names.get(&name)
                    .and_then(|v| v.first())
                    .and_then(|p| p.clone())
                    .unwrap_or_else(|| "the parameter".to_string());
                self.check_error(e.end_line, caret, &format!(
                    "too many arguments to function call, expected single argument '{}', have {} arguments",
                    pname, arg_count));
            } else {
                self.check_error(e.end_line, caret, &format!(
                    "too many arguments to function call, expected {}, have {}",
                    expected_types.len(), arg_count));
            }
        }
    }

    /// Deep call-family walk for clone-evaluated subtrees. The assignment
    /// hook evaluates clones of target/value with the post-inner hooks
    /// suppressed (double-reporting guard), which silently exempted every
    /// call nested inside an assignment value — `part = va_arg(ap, T)` was
    /// fine but `r = wrap(one_arg(1))` never saw its arity check. This walk
    /// fires check_call_family once per FuncCall node in the subtree; it must
    /// only be called on nodes whose REAL counterparts are never visited
    /// outside the clone (assignment target/value chains).
    fn check_call_family_deep(&mut self, e: &AstExpr) {
        if let AstExprData::FuncCall { .. } = &e.data {
            self.check_call_family(e);
        }
        match &e.data {
            AstExprData::Unary { operand, .. }
            | AstExprData::Paren(operand)
            | AstExprData::Cast { expr: operand, .. } => {
                self.check_call_family_deep(operand);
            }
            AstExprData::Binary { left, right, .. } => {
                self.check_call_family_deep(left);
                self.check_call_family_deep(right);
            }
            AstExprData::Ternary { cond, then, else_ } => {
                self.check_call_family_deep(cond);
                self.check_call_family_deep(then);
                self.check_call_family_deep(else_);
            }
            AstExprData::Comma(exprs) => {
                for ex in exprs { self.check_call_family_deep(ex); }
            }
            AstExprData::Subscript { object, key, .. } => {
                self.check_call_family_deep(object);
                self.check_call_family_deep(key);
            }
            AstExprData::FuncCall { callee, args, .. } => {
                if let Some(c) = callee { self.check_call_family_deep(c); }
                for a in args { self.check_call_family_deep(a); }
            }
            AstExprData::MsgSend { receiver, args, .. } => {
                self.check_call_family_deep(receiver);
                for a in args { self.check_call_family_deep(a); }
            }
            AstExprData::Assign { target, value } => {
                self.check_call_family_deep(target);
                self.check_call_family_deep(value);
            }
            _ => {}
        }
    }

    // ─── assignment family (C17 6.5.16) ─────────────────────────────────
    //
    // clang-measured (probe_assign / probe2 / probe_objc.m):
    //   const target            error `cannot assign to variable 'cp' with
    //                             const-qualified type 'int *const'`
    //   cast target             error `assignment to cast is illegal, lvalue
    //                             casts are not supported`
    //   ptr ↔ ptr mismatch      WARN  `incompatible pointer types assigning
    //                             to 'char *' from 'int *'`
    //   int → ptr (variable)    error `incompatible integer to pointer
    //                             conversion assigning to 'int *' from 'int';
    //                             take the address with &`
    //   ptr → int (plain value) error `incompatible pointer to integer
    //                             conversion assigning to 'int' from 'int *';
    //                             dereference with *`
    //   ptr → int (&value)      same, but hint `remove &`
    //   struct ← builtin/other  error `assigning to 'struct A' from
    //                             incompatible type 'int'`
    // Silent by measurement: `void *` both ways, 0/nil literal → pointer,
    // id/Class/SEL/instancetype/NPObject* wildcards, child-class value into
    // parent-class target (obj_wildcard_cast parity with codegen Phase 0).
    fn check_assign_family(
        &mut self,
        e: &AstExpr,
        target: &AstExpr,
        value: &AstExpr,
        vty_snapshot: Option<&AstType>,
    ) {
        // Only shapes whose static types both sides can prove get judged —
        // everything else is silent, never guessed.
        if Self::family_span_gates(self, e.line, e.end_line, e.end_col) { return; }
        if Self::refs_jeti_synth(target) || Self::refs_jeti_synth(value) { return; }
        // Lvalue-ness first (needs no types): casts as assignment targets.
        if matches!(target.data, AstExprData::Cast { .. }) {
            self.check_error(target.line, target.col,
                "assignment to cast is illegal, lvalue casts are not supported");
            return;
        }
        // Target type: scope-frames only (see assign_target_type). The clone
        // snapshot would answer `int` for `self`/ivar reads — the loose
        // fallback — which reported every `self = [super init]` and plain
        // ivar store as a pointer/int conversion error (regression: TOMLEditor).
        let Some(tty) = self.assign_target_type(target) else { return };
        // const target (probe ⑩): clang quotes the variable back.
        if tty.is_const {
            if let AstExprData::VarRef { name, .. } = &target.data {
                self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                    "cannot assign to variable '{}' with const-qualified type '{}'",
                    name, Self::type_display(&tty)));
                return;
            }
        }
        // Value type: the snapshot from clone-evaluation, with calls typed
        // through the declared return-type table. A call with NO tracked
        // return (`va_arg`, extern passthrough decls) is outside this
        // family's proof — the checker's call arm answers `int` loosely,
        // which would misreport `p = v()`; silent instead.
        let Some(mut vty) = vty_snapshot.cloned() else { return };
        if let AstExprData::FuncCall { name, .. } = &value.data {
            match self.function_returns.get(name).cloned().flatten() {
                Some(rt) => vty = rt,
                None => return,
            }
        }
        // `&x` in value place: the loose Unary arm answers the operand's
        // type; the address-of shape is a pointer TO that type. Only plain
        // variable operands are folded (probe2 ⑰ measured this shape).
        if let AstExprData::Unary { op, operand, .. } = &value.data {
            if *op == 4 {
                if let AstExprData::VarRef { name, .. } = &operand.data {
                    if let Some(t) = self.lookup_scope_var_type(name) {
                        let mut pt = t.clone();
                        pt.is_pointer = true;
                        vty = pt;
                    }
                }
            }
        }
        // Value shapes the loose type model collapses to `int` (Binary,
        // Ternary, Comma, subscript/member reads) are outside this family's
        // proof — pointer-arithmetic and member loads would misreport here.
        // `&x` IS judgeable — the wrap above supplies its pointer type.
        if !Self::assign_value_shape_judgeable(value) { return; }
        let v_is_zero_literal = Self::is_nil_or_zero(value);
        // Both pointer-like: the incompatible-pointer sub-family. `void *`
        // joins implicitly (C17 6.3.2.3p1-2 — probe2 ⑫⑬ silent), and the
        // ObjC wildcards mirror codegen's obj_wildcard_cast.
        if Self::is_pointer_like(&tty) && Self::is_pointer_like(&vty) {
            if tty.prim == TypePrim::Void || vty.prim == TypePrim::Void { return; }
            if Self::obj_wildcard(&tty) || Self::obj_wildcard(&vty) { return; }
            // Top-level const is a read-side qualifier: `int *q = cp` with
            // `int *const cp` reads the pointer value, discarding nothing the
            // standard protects — clang is silent there. Compare spellings
            // with the top-level const stripped.
            let mut tty_eff = tty.clone();
            let mut vty_eff = vty.clone();
            tty_eff.is_const = false;
            vty_eff.is_const = false;
            let tn = Self::named_class_of(&tty_eff);
            let vn = Self::named_class_of(&vty_eff);
            match (&tn, &vn) {
                // Same named class: the same spelling joins itself.
                (Some(t), Some(v)) if t == v => return,
                // Both primitive pointers with the same spelling (`int *` ←
                // `int *`): silent. Different spellings fall through to the
                // warning (char * ← int * — probe_assign ③ measured).
                (None, None) if Self::type_display(&tty_eff) == Self::type_display(&vty_eff) => return,
                _ => {}
            }
            // Child-class value into parent-class target is the ordinary
            // upcast (probe_objc ① silent); everything else warns.
            if let (Some(tn), Some(vn)) = (&tn, &vn) {
                if let Some(ref st) = self.symtab {
                    if self.is_subclass_of(st, vn, tn) { return; }
                }
            }
            self.check_warning_over(e.line, e.col, e.end_line, e.end_col, &format!(
                "incompatible pointer types assigning to '{}' from '{}'",
                Self::type_display(&tty_eff), Self::type_display(&vty_eff)));
            return;
        }
        // Pointer-target ← scalar (probe ⑭): zero/nil literals are the
        // documented null constant, silent.
        if Self::is_pointer_like(&tty) && Self::is_integral_type(&vty) && !v_is_zero_literal {
            let hint = Self::ptr_source_hint(value, "take the address with &");
            self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                "incompatible integer to pointer conversion assigning to '{}' from '{}'; {}",
                Self::type_display(&tty), Self::type_display(&vty), hint));
            return;
        }
        // Scalar-target ← pointer (probe ⑯⑰): hint differs by shape —
        // `&x` wants its & removed, a dereferenceable variable wants `*`.
        if Self::is_integral_type(&tty) && Self::is_pointer_like(&vty) {
            let hint = Self::ptr_source_hint(value, "dereference with *");
            self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                "incompatible pointer to integer conversion assigning to '{}' from '{}'; {}",
                Self::type_display(&tty), Self::type_display(&vty), hint));
            return;
        }
        // Struct target ← builtin / foreign struct (probe ⑮⑱ — both measured
        // under one wording; the value side spells `struct T` with its tag).
        if tty.is_struct && !vty.is_block && (!vty.is_struct || tty.name != vty.name) {
            self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                "assigning to '{}' from incompatible type '{}'",
                Self::struct_spell(&tty),
                if vty.is_struct { Self::struct_spell(&vty) } else { Self::type_display(&vty) }));
        }
    }

    /// Type of an assignment target — **simple declared variables only**, and
    /// only types that were *declared*: `scope_vars` frames are written by
    /// real declarations (globals in frame 0, params/locals in body frames)
    /// plus the compiler-synthetic `__jeti_*` state. Everything the loose
    /// resolver answers by fallback (`self` in methods, ivar reads) never
    /// reaches here, because those shapes are either not VarRefs the family
    /// accepts or not frame entries at all — so the conversion family stays
    /// silent instead of misfiring on `self = [super init]`.
    fn assign_target_type(&self, target: &AstExpr) -> Option<AstType> {
        let AstExprData::VarRef { name, .. } = &target.data else { return None };
        self.lookup_scope_var_type(name)
    }

    /// Does this identifier spelling name something the symbol table really
    /// knows — directly, or through a `@using` fold? Generic type-arg
    /// suffixes are stripped first (`LogBuffer<LogTool::LogEntry *>` names
    /// the class `LogBuffer` / `LogTool::LogBuffer`).
    fn symtab_knows_spelling(&self, name: &str) -> bool {
        let base = name.split('<').next().unwrap_or(name);
        let Some(ref st) = self.symtab else { return false };
        if st.lookup(base).is_some() || st.find_class(base).is_some() {
            return true;
        }
        if let Some(u) = st.find_using(base) {
            return st.lookup(&u.fqn).is_some() || st.find_class(&u.fqn).is_some();
        }
        false
    }

    /// Static type of a dereference-chain subject (`**p`, `*p`) for the
    /// switch family: the loose Unary arm does NOT deref (it returns the
    /// operand's own type), so `switch (*p)` over `char *p` looked like a
    /// pointer subject and misfired the integrality check (regression:
    /// TOMLEditor). Fold one deref level per `*`, through scope-frame types
    /// only; `None` = cannot prove → silent.
    fn deref_subject_type(&self, e: &AstExpr) -> Option<AstType> {
        if let AstExprData::Unary { op, operand, .. } = &e.data {
            if *op == 2 {
                let base = match &operand.data {
                    AstExprData::VarRef { name, .. } => self.lookup_scope_var_type(name),
                    _ => self.deref_subject_type(operand),
                };
                if let Some(mut t) = base {
                    if t.is_pointer {
                        t.is_pointer = false;
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    /// Value shapes whose inferred type is trustworthy for the mixed
    /// conversions: literals, declared variables, explicit casts, message
    /// sends and calls (through the tables below). Everything else — whose
    /// model collapses to `int` — stays silent.
    fn assign_value_shape_judgeable(value: &AstExpr) -> bool {
        matches!(&value.data,
            AstExprData::Int(_) | AstExprData::Char(_) | AstExprData::Bool(_)
            | AstExprData::Float(_) | AstExprData::FloatRaw(_)
            | AstExprData::String(_) | AstExprData::AtString(_)
            | AstExprData::VarRef { .. } | AstExprData::Cast { .. }
            | AstExprData::MsgSend { .. } | AstExprData::FuncCall { .. }
            // address-of: judgeable only through the wrap in
            // check_assign_family (`&plain-variable`); the wrap guarantees
            // the pointer type, so the shape is proven.
            | AstExprData::Unary { op: 4, .. })
    }

    /// clang appends a fix-it hint to the int↔pointer conversion errors; the
    /// hint depends on the value's shape (`v = &v` → "remove &", plain
    /// variable → "dereference with *"). `default_hint` for the int→pointer
    /// direction ("take the address with &") unless the shape says otherwise.
    fn ptr_source_hint(value: &AstExpr, default_hint: &'static str) -> &'static str {
        match &value.data {
            AstExprData::Unary { op, .. } if *op == 4 => "remove &",
            AstExprData::Unary { op, .. } if *op == 2 => "dereference with *",
            AstExprData::VarRef { .. } => default_hint,
            _ => "", // unmeasured shape — no hint rather than a wrong one
        }
    }

    /// Object-pointer wildcard parity with codegen's `obj_wildcard_cast`
    /// (Phase 0): id / instancetype / Class / SEL / NPObject * are the
    /// neutral pointer spellings; substantive class pointers join only
    /// through them.
    fn obj_wildcard(t: &AstType) -> bool {
        matches!(t.prim, TypePrim::Id | TypePrim::Instancetype | TypePrim::Class | TypePrim::Sel)
            || t.name.as_deref() == Some("NPObject")
    }

    /// Struct spelling in diagnostics the way clang quotes it back: it keeps
    /// the tag keyword (`struct A`), which `type_display` alone does not add.
    fn struct_spell(t: &AstType) -> String {
        format!("{} {}", t.tag.keyword(), Self::type_display(t))
    }

    // ─── switch family (C17 6.8.4.2) ────────────────────────────────────
    //
    // clang-measured (probe_switch):
    //   pointer/struct/float subject   error `statement requires expression
    //     of integer type ('int *' invalid)` — caret at `switch`
    //   `case 1.5:`                    error `integer constant expression must
    //     have integer type, not 'double'` — caret at the value
    //   duplicate literal case         error `duplicate case value '1'` — caret at value
    //   second `default:`              error `multiple default labels in one
    //     switch` — caret at the second `default`
    //   `case after default:`          legal (no diagnostic) — measured silent
    fn check_switch_family(
        &mut self,
        switch_line: usize,
        switch_col: usize,
        switch_end_line: usize,
        switch_end_col: usize,
        expr: &AstExpr,
        body: &AstStmt,
    ) {
        if Self::family_span_gates(self, switch_line, switch_end_line, switch_end_col) { return; }
        // Subject integrality — only shapes whose static type is known get
        // judged; unknown, enum, and lowering-synth subjects stay silent.
        // Deref-chain subjects (`**p`) fold through deref_subject_type: the
        // loose Unary typing does not deref and would misjudge `switch (*p)`
        // over `char *p` as a pointer subject.
        let subject_ty = if matches!(expr.data, AstExprData::Unary { .. }) {
            self.deref_subject_type(expr)
        } else {
            expr.expr_type.as_deref().cloned()
        };
        if let Some(ty) = subject_ty {
            if !Self::is_integral_type(&ty)
                && !self.is_enum_type(&ty)
                && !Self::refs_jeti_synth(expr)
            {
                self.check_error_over(switch_line, switch_col, switch_end_line, switch_end_col, &format!(
                    "statement requires expression of integer type ('{}' invalid)",
                    Self::type_display(&ty)));
            }
        }
        // Case/default labels of THIS switch: the same walk shape as
        // check_switch_exhaustiveness (labels at the switch body's level),
        // evaluating only literal constants — expression-valued cases are
        // not constant-folded here, and enum-member spellings stay out.
        let mut seen: Vec<(i64, String)> = Vec::new();
        let mut default_seen = false;
        self.walk_switch_labels(body, &mut seen, &mut default_seen);
    }

    /// Duplicate/multi-default/label-well-formedness walk for one switch.
    /// Nested switches are their own family — descent stops at them; labels
    /// inside an arm's braces DO belong to this switch (C17 6.8.4.2p2),
    /// mirroring clang's duplicate detection.
    fn walk_switch_labels(&mut self, s: &AstStmt, seen: &mut Vec<(i64, String)>, default_seen: &mut bool) {
        if matches!(&s.data, AstStmtData::Switch { .. }) { return; }
        if Self::family_span_gates(self, s.line, s.end_line, s.end_col) { return; }
        match &s.data {
            AstStmtData::Case { value, body } => {
                match &value.data {
                    AstExprData::Int(v) => {
                        let spelling = v.to_string();
                        if seen.iter().any(|(val, _)| val == v) {
                            self.check_error_over(value.line, value.col, value.end_line, value.end_col,
                                &format!("duplicate case value '{}'", spelling));
                        } else {
                            seen.push((*v, spelling));
                        }
                    }
                    AstExprData::VarRef { name, .. } => {
                        // Enum-member case label, folded through the member
                        // value table: `case RED` duplicates `case 1` when
                        // both equal 1 (probe_switch ⑦ — clang quotes both
                        // spellings). Unfoldable spellings stay silent.
                        if let Some(&v) = self.enum_member_values.get(name) {
                            if let Some(&(_, ref prev)) = seen.iter().find(|(val, _)| *val == v) {
                                self.check_error_over(value.line, value.col, value.end_line, value.end_col,
                                    &format!("duplicate case value: '{}' and '{}' both equal '{}'", prev, name, v));
                            } else {
                                seen.push((v, name.clone()));
                            }
                        }
                    }
                    AstExprData::Float(_) | AstExprData::FloatRaw(_) => {
                        self.check_error_over(value.line, value.col, value.end_line, value.end_col,
                            "integer constant expression must have integer type, not 'double'");
                    }
                    // Integer expressions, object literals: not judged here
                    // (no constant folding in this checker).
                    _ => {}
                }
                self.walk_switch_labels(body, seen, default_seen);
            }
            AstStmtData::Default(body) => {
                if *default_seen {
                    self.check_error_over(s.line, s.col, s.end_line, s.end_col,
                        "multiple default labels in one switch");
                }
                *default_seen = true;
                self.walk_switch_labels(body, seen, default_seen);
            }
            AstStmtData::Compound(v) => {
                for st in v {
                    self.walk_switch_labels(st, seen, default_seen);
                }
            }
            AstStmtData::If { cond: _, then, else_ } => {
                self.walk_switch_labels(then, seen, default_seen);
                if let Some(el) = else_ {
                    self.walk_switch_labels(el, seen, default_seen);
                }
            }
            _ => {}
        }
    }

    // ─── stdlib callee whitelist (angle-header gated) ───────────────────
    //
    /// `#include <stdio.h>` etc. reach the C compiler verbatim (escape
    /// hatch), so every function they declare is real at codegen time with
    /// no AST trace. This table names the functions each C17 library header
    /// declares (curated from the library clause — clang sources are off
    /// limits). It is a whitelist gate for the undeclared-function check
    /// ONLY; it never invents arity or type information.
    fn c_stdlib_declares(&self, name: &str) -> bool {
        for h in &self.included_c_headers {
            if c_stdlib_function_header(name) == Some(h.as_str()) {
                return true;
            }
        }
        false
    }

    /// Same gate, for *bindings*: the globals and macro constants C headers
    /// declare (`errno`, `stdin`, `STDIN_FILENO`, `ECHO`, `INT_MAX`...). The
    /// use-of-undeclared-identifier check consults this before firing — a
    /// mention of `stderr` is real C that reaches the compiler through the
    /// passthrough header, never through the AST.
    fn c_stdlib_binds(&self, name: &str) -> bool {
        // Bindings are a transitive surface: `#import <termios.h>` pulls
        // unistd's STDIN_FILENO, stdio.h pulls EOF, errno.h pulls the E*
        // codes. The gate is therefore "the TU included at least one audited
        // C header", and the whole curated union applies. A missed diagnostic
        // here is the escape hatch working as designed; a false error would
        // not be.
        if !self.included_c_headers.iter().any(|h| {
            C_STDLIB_BINDINGS.iter().any(|(hdr, _)| *hdr == h.as_str())
        }) {
            return false;
        }
        C_STDLIB_BINDINGS.iter().any(|(_, table)| table.contains(&name))
    }

    /// C headers the pipeline already collected (`pre.c_headers`, whole
    /// `#include <…>` / `#import <…>` lines — the preprocessor routes both
    /// spellings of C-header includes here verbatim). Angle spells become
    /// the audited gate for the undeclared-function check; any quoted C
    /// include sets `has_quoted_c_include` (un-auditable extern surface →
    /// check off).
    pub fn feed_included_headers(&mut self, lines: &[String]) {
        for line in lines {
            let t = line.trim();
            let spec = t.strip_prefix("#include")
                .or_else(|| t.strip_prefix("#import"))
                .or_else(|| t.strip_prefix("#include_next"))
                .map(str::trim);
            let Some(spec) = spec else { continue };
            if let Some(h) = spec.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
                let h = h.trim();
                if !self.included_c_headers.iter().any(|x| x == h) {
                    self.included_c_headers.push(h.to_string());
                }
            } else if spec.starts_with('"') && spec.contains(".h") {
                self.has_quoted_c_include = true;
            }
        }
    }

    /// Do any operand references point to a lowering-desugar internal
    /// (`__jeti_*` variables — async task state, eh exception state)? The
    /// lowering passes deliberately *keep* origin spans on their synthesized
    /// casts (caret anchoring), so an `end == 0` guard cannot cover them;
    /// the authoritative tell is the referenced internals themselves.
    fn refs_jeti_synth(e: &AstExpr) -> bool {
        let hit = |n: &str| n.starts_with("__jeti");
        match &e.data {
            AstExprData::VarRef { name, .. } => hit(name),
            AstExprData::IvarRef { obj, .. } | AstExprData::PropRef { obj, .. } => {
                Self::refs_jeti_synth(obj)
            }
            AstExprData::Unary { operand, .. } => Self::refs_jeti_synth(operand),
            AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => {
                Self::refs_jeti_synth(inner)
            }
            AstExprData::FuncCall { name, callee, args, .. } => {
                hit(name)
                    || callee.as_deref().map_or(false, Self::refs_jeti_synth)
                    || args.iter().any(Self::refs_jeti_synth)
            }
            _ => false,
        }
    }

    /// clang-measured wording/severity (c_standard_diagnostics_plan §G-expr):
    ///   warn "cast to 'char *' from smaller integer type 'int'"      (int → ptr)
    ///   warn "cast to smaller integer type 'int' from 'void *'"     (ptr → int)
    /// Same-width casts stay silent (`long` ↔ pointers are LP64-wide).
    fn check_cast_types(&mut self, line: &usize, col: &usize, end_line: usize, end_col: usize,
                        target: &AstType, operand: &AstExpr, src: Option<&AstType>) {
        let Some(src) = src else { return };
        // Source-span iron law: synthesized nodes carry `end == 0`
        // (`docs/plans/source_locations_debug_lsp_plan.md`).
        if end_line == 0 || end_col == 0 {
            return;
        }
        if self.is_foundation_source(*line) {
            return;
        }
        // Lowering-injected casts (async task state, eh exception state) keep
        // origin spans — detected through the `__jeti_*` internals they wrap.
        if Self::refs_jeti_synth(operand) {
            return;
        }
        // A function designator already has its fn-pointer type: casts
        // *from* it are same-width by construction (clang stays silent on
        // `(int (*)(int))fs_double_it`); the checker's loose VarRef typing
        // still calls the designator `int`, so resolve it from the symbol
        // table instead.
        if let AstExprData::VarRef { name, .. } = &operand.data {
            if let Some(sym) = self.symtab.as_ref().and_then(|st| st.lookup(name)) {
                if sym.kind == SymbolKind::Function {
                    return;
                }
            }
        }
        // A call to a whitelisted passthrough-C function has no signature in
        // the curated table (names only, by design), so its type falls back
        // to `int` — a width warning built on that guess would be false (the
        // real `dlsym` returns `void *`; clang stays silent on the textbook
        // `(fnptr)dlsym(...)`). A missed diagnostic here is the escape hatch
        // working as designed; a false error would not be.
        if let AstExprData::FuncCall { name, .. } = &operand.data {
            if !name.is_empty() && Self::c_stdlib_declares(self, name) {
                return;
            }
        }
        let is_ptr = |t: &AstType| Self::is_pointer_like(t);
        let width = |t: &AstType| -> i32 {
            if is_ptr(t) { 64 } else { Self::integral_bit_width(t) }
        };
        if Self::is_integral_type(src) && is_ptr(target)
            && !Self::is_int_literal_expr(operand)
        {
            let src_w = width(src);
            if src_w >= 0 && src_w < 64 {
                self.check_warning(*line, *col,
                    &format!("cast to '{}' from smaller integer type '{}'",
                             Self::cast_pointer_spelling(target),
                             Self::type_display(src)));
            }
        } else if is_ptr(src) && Self::is_integral_type(target) {
            let dst_w = width(target);
            if dst_w >= 0 && dst_w < 64 {
                self.check_warning(*line, *col,
                    &format!("cast to smaller integer type '{}' from '{}'",
                             Self::type_display(target),
                             Self::cast_pointer_spelling(src)));
            }
        }
    }

    /// Spell a type the way the source would, for diagnostics. `{:?}` is the
    /// fallback for prims with no C spelling of their own, but plain `int`
    /// must read `int` (not `Int`) — these messages quote the user's code back
    /// at them (`illegal type 'int' in a dictionary literal`).
    fn type_display(t: &AstType) -> String {
        let mut s = match t.prim {
            TypePrim::Void => "void".to_string(),
            TypePrim::Char => "char".to_string(),
            TypePrim::Short => "short".to_string(),
            TypePrim::Int => "int".to_string(),
            TypePrim::Long => "long".to_string(),
            TypePrim::LongLong => "long long".to_string(),
            TypePrim::Float => "float".to_string(),
            TypePrim::Double => "double".to_string(),
            TypePrim::Bool => "BOOL".to_string(),
            TypePrim::Signed => "signed".to_string(),
            TypePrim::Unsigned => "unsigned".to_string(),
            TypePrim::Id => "id".to_string(),
            TypePrim::Class => "Class".to_string(),
            TypePrim::Sel => "SEL".to_string(),
            TypePrim::Instancetype => "instancetype".to_string(),
            _ => t.name.clone().unwrap_or_else(|| format!("{:?}", t.prim)),
        };
        // Show type_args in diagnostics: `NPArray<NPString *> *` instead of a
        // bare `NPArray *` — otherwise generic-mismatch errors read as
        // "NPArray * from NPArray *" and the user can't see the difference.
        if !t.type_args.is_empty() {
            let args = t.type_args.iter()
                .map(Self::type_display)
                .collect::<Vec<_>>()
                .join(", ");
            s.push_str(&format!("<{}>", args));
        }
        if t.is_pointer {
            // clang spells a const-qualified pointer level `int *const`
            // (probe_assign ①) — mirror it so the const-assignment message
            // quotes the type the user wrote.
            if t.is_const {
                s.push_str(" *const");
            } else {
                s.push_str(" *");
            }
        }
        s
    }

    // ─── `throws` reconciliation ────────────────────────────────────────

    /// Map a declaration's `throws` field onto the three-state reconciliation
    /// input. The parser encodes a bare `throws` ("declared to throw, type
    /// unstated") as a void-typed annotation, so `void` means "bare" here.
    fn throws_state(t: Option<&AstType>) -> Option<Option<AstType>> {
        match t {
            None => None,
            Some(ty) if ty.prim == TypePrim::Void && !ty.is_pointer => Some(None),
            Some(ty) => Some(Some(ty.clone())),
        }
    }

    /// Best-effort static type of a `@throw` operand. Deliberately
    /// conservative: `None` means "cannot say", and the reconciler then
    /// accepts any declared type. Message sends stay unjudged — the registry
    /// here is selector-only, so their class is not knowable.
    fn throw_expr_type(&self, e: &AstExpr) -> Option<AstType> {
        match &e.data {
            AstExprData::AtString(_) => {
                let mut t = AstType::new(TypePrim::Id);
                t.is_pointer = true;
                Some(t)
            }
            AstExprData::String(_) => {
                // A bare C string is not an object — `@throw "boom"` is a bug
                // the type check should be able to name.
                let mut t = AstType::new(TypePrim::Char);
                t.is_pointer = true;
                Some(t)
            }
            AstExprData::Cast { target_type, .. } => Some(target_type.clone()),
            AstExprData::VarRef { name, .. } => self.lookup_scope_var_type(name),
            _ => None,
        }
    }

    /// Type of a name visible in the body being checked (locals + params,
    /// innermost scope first). `None` when unknown — never guess.
    fn lookup_scope_var_type(&self, name: &str) -> Option<AstType> {
        for scope in self.scope_vars.iter().rev() {
            for (n, t) in scope.iter().rev() {
                if n == name {
                    return Some(t.clone());
                }
            }
        }
        None
    }

    /// True when `name` is currently proven non-null by control flow
    /// (`if (x) { ... }`). Innermost scope wins, matching `scope_vars` order.
    fn is_narrowed_nonnull(&self, name: &str) -> bool {
        self.narrowed.iter().rev().any(|s| s.contains(name))
    }

    /// Peel the `(...)` / explicit-cast wrappers down to the operand that
    /// carries the real type. The `_Block_copy` idiom *requires* the cast
    /// (`_Block_copy((const void *)blk)`, to silence the ABI's own
    /// pointer-conversion warning), so the cast target is always `void *` and
    /// tells us nothing — the operand underneath is what has to be judged.
    fn peel_casts(e: &AstExpr) -> &AstExpr {
        match &e.data {
            AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. } => Self::peel_casts(inner),
            _ => e,
        }
    }

    /// `true` only when a `_Block_copy`/`_Block_release` operand is *provably*
    /// not a block.
    ///
    /// Deliberately narrow, per the project's "missing a bug is acceptable, a
    /// false positive is not" rule: a block literal and any block-typed or
    /// block-typedef'd value are blocks; `id`/`instancetype`/`void *` and
    /// anything whose type we cannot read stay silent (a block legitimately
    /// lives in an `id`/`void *` between a copy and its release, which is
    /// exactly how NPTimer's ivar is spelled). What remains — a named class
    /// pointer, `char *`, a struct pointer — can only be a bug: the ABI will
    /// read it as a block header.
    fn block_operand_is_provably_not_a_block(&self, a: &AstExpr) -> bool {
        let inner = Self::peel_casts(a);
        if matches!(inner.data, AstExprData::Block { .. }) {
            return false;
        }
        let ty = match &inner.data {
            AstExprData::VarRef { name, .. } => self
                .lookup_scope_var_type(name)
                .or_else(|| inner.expr_type.as_ref().map(|t| (**t).clone())),
            _ => inner.expr_type.as_ref().map(|t| (**t).clone()),
        };
        let Some(ty) = ty else { return false };
        if ty.is_block || ty.is_fn_ptr {
            return false;
        }
        // A typedef'd block alias carries no `is_block` on the spelling
        // (`SimpleBlock blk` is just a named type) — same lookup the block
        // invocation path needs, same reason.
        if let Some(n) = ty.name.as_ref() {
            if self.block_alias_names.contains(n) || self.typedef_blocks.contains_key(n) {
                return false;
            }
        }
        if !ty.is_pointer {
            // A bare scalar is also not a block, but only a *known* pointer
            // shape is reported — see the doc comment.
            return false;
        }
        // `void *` (and `id`, `instancetype`, `Class`, `SEL`) are where a block
        // can legitimately be parked: not provably wrong.
        if matches!(ty.prim, TypePrim::Void | TypePrim::Id | TypePrim::Instancetype
                              | TypePrim::Class | TypePrim::Sel) {
            return false;
        }
        // A named class pointer, `char *`, or a struct pointer.
        ty.prim == TypePrim::Named || ty.prim == TypePrim::Char || ty.is_struct
    }

    /// The nullability an argument expression *carries*, or `None` when unknown.
    ///
    /// Deliberately narrow — only shapes whose declared annotation we can read
    /// off:
    /// - a variable (its declared type in `scope_vars` / an ivar),
    /// - an explicit cast (the target type's annotation),
    /// - a message send (the declared method return annotation).
    ///
    /// Anything else (literals, arithmetic, a call's result) is `None` = "say
    /// nothing", which is what keeps this diagnostic from flooding: only a
    /// *provably* nullable value passed to a `nonnull` parameter is reported,
    /// exactly like ObjC's `-Wnullable-to-nonnull-conversion`.
    fn arg_nullability(&self, a: &AstExpr) -> Option<Nullability> {
        match &a.data {
            AstExprData::VarRef { name, .. } => {
                let t = self.lookup_scope_var_type(name)
                    .or_else(|| a.expr_type.as_ref().map(|t| (**t).clone()))?;
                if t.nulls == Nullability::Unspecified { None } else { Some(t.nulls) }
            }
            AstExprData::Cast { target_type, .. } => {
                if target_type.nulls == Nullability::Unspecified { None }
                else { Some(target_type.nulls) }
            }
            AstExprData::Paren(inner) => self.arg_nullability(inner),
            AstExprData::MsgSend { selector, .. } => self
                .method_returns
                .get(selector)
                .and_then(|r| r.as_ref())
                .map(|t| t.nulls)
                .filter(|n| *n != Nullability::Unspecified),
            _ => None,
        }
    }

    /// `nullable` value flowing into a `nonnull` parameter — an **error**, not a
    /// warning: the callee is entitled to dereference without a check, so this
    /// is a latent nil dereference at runtime, and the whole point of the
    /// annotation is to make it a compile error.
    ///
    /// Skipped when the value is flow-narrowed non-null in this branch, and
    /// when the argument type is unknown (see `arg_nullability`).
    fn check_nullability_transfer(&mut self, param: &AstType, arg: &AstExpr) {
        if param.nulls != Nullability::Nonnull {
            return;
        }
        if let AstExprData::VarRef { name, .. } = &arg.data {
            if self.is_narrowed_nonnull(name) {
                return;
            }
        }
        // A literal nil is statically known-null — strictly worse than a
        // maybe-nil value. clang's -Wnonnull flags `f(nil)` for the same
        // reason (verified against the host SDK). Only for pointer-ish
        // parameters: `nonnull int` is meaningless and handled by the
        // non-pointer diagnostic family, not here.
        let pointerish = param.is_pointer
            || matches!(param.prim, TypePrim::Id | TypePrim::Instancetype | TypePrim::Class);
        if pointerish && Self::is_nil_or_zero(arg) {
            self.check_error_over(arg.line, arg.col, arg.end_line, arg.end_col,
                "null passed to a callee that requires a non-null argument — pass a real object, or declare the parameter 'nullable'");
            return;
        }
        if self.arg_nullability(arg) != Some(Nullability::Nullable) {
            return;
        }
        let pname = param.name.clone().unwrap_or_else(|| "object".into());
        self.check_error_over(arg.line, arg.col, arg.end_line, arg.end_col, &format!(
            "nullable value passed to nonnull parameter '{}' — the callee may assume it is non-nil; guard with 'if (x)' or pass a non-null value",
            pname));
    }

    /// Names a condition proves non-null, as `(then, else)`.
    ///
    /// Handles only the shapes where the implication is sound and obvious:
    /// `if (x)`, `if (!x)`, `if (x != nil)`, `if (x == nil)`. Anything else
    /// narrows nothing — a missed narrowing costs a spurious diagnostic, but a
    /// *wrong* one would hide a real nil dereference, so the bias is
    /// deliberately conservative.
    fn narrow_from_cond(cond: &AstExpr) -> (std::collections::HashSet<String>,
                                           std::collections::HashSet<String>) {
        let mut then_set = std::collections::HashSet::new();
        let mut else_set = std::collections::HashSet::new();
        match &cond.data {
            // `if (x)` — truthy in the then arm, nil/falsy in the else arm.
            AstExprData::VarRef { name, .. } => {
                then_set.insert(name.clone());
                else_set.insert(name.clone());
            }
            AstExprData::Paren(inner) => return Self::narrow_from_cond(inner),
            // `if (!x)` — inverted: the then arm proves x is NOT non-null.
            // Unary `!` is op 8 in the parser (`~` is 7).
            AstExprData::Unary { op: 8, operand, .. } => {
                if let AstExprData::VarRef { name, .. } = &operand.data {
                    else_set.insert(name.clone());
                }
            }
            // `if (x != nil)` → then arm has x; `if (x == nil)` → else arm does.
            AstExprData::Binary { op, left, right } if *op == 12 || *op == 13 => {
                let is_eq = *op == 12;
                if let (AstExprData::VarRef { name, .. }, true) =
                    (&left.data, Self::is_nil_or_zero(right))
                {
                    if is_eq { else_set.insert(name.clone()); } else { then_set.insert(name.clone()); }
                }
                // Mirrored form: `nil != x`.
                if let (AstExprData::VarRef { name, .. }, true) =
                    (&right.data, Self::is_nil_or_zero(left))
                {
                    if is_eq { else_set.insert(name.clone()); } else { then_set.insert(name.clone()); }
                }
            }
            _ => {}
        }
        (then_set, else_set)
    }

    /// True when control cannot fall out of the bottom of `s` — `return`,
    /// `throw`, a `goto`, or a compound whose final statement does. Used to
    /// justify narrowing the statements that follow an `if`.
    ///
    /// `break` / `continue` are deliberately excluded: they only exit an
    /// enclosing loop or switch, so narrowing after them would need to know
    /// which construct that is, and a wrong answer here would *hide* a real
    /// nil dereference.
    fn exits_unconditionally(s: &AstStmt) -> bool {
        match &s.data {
            AstStmtData::Return(_) | AstStmtData::Throw(_) | AstStmtData::Goto(_) => true,
            AstStmtData::Compound(inner) => {
                inner.last().is_some_and(Self::exits_unconditionally)
            }
            AstStmtData::If { then, else_, .. } => match else_ {
                Some(e) => Self::exits_unconditionally(then) && Self::exits_unconditionally(e),
                None => false,
            },
            _ => false,
        }
    }

    /// A literal that can only compare equal to a nil/zero pointer.
    ///
    /// `nil` / `NULL` reach the AST as plain variables (the elaborator maps
    /// `CstExprKind::Nil` → `VarRef { name: "nil" }`), so they are matched by
    /// name — there are no dedicated expression variants.
    fn is_nil_or_zero(e: &AstExpr) -> bool {
        match &e.data {
            AstExprData::Int(0) | AstExprData::Bool(false) => true,
            AstExprData::VarRef { name, .. } => name == "nil" || name == "NULL",
            _ => false,
        }
    }

    /// The class a pointer type names (`NPError *` -> `NPError`).
    fn class_name_of(t: &AstType) -> Option<String> {
        // Symbol-table lookups key the BASE class: class_ref may carry the
        // instantiation spelling (`NPMutableArray<NPString *>`) for codegen's
        // monomorphized-clone dispatch, and the registry has no entry under
        // that mangled name. Codegen reads class_ref raw; this helper is the
        // symtab-facing spelling.
        let raw = t.class_ref.clone().or_else(|| t.name.clone())?;
        Some(raw.split('<').next().unwrap_or(&raw).to_string())
    }

    /// Is a `@throw` of `actual` covered by a declared `throws(declared)`?
    /// Subclasses are accepted; `id` on either side is accepted (an object of
    /// unstated class). Anything not *provably* a mismatch is accepted: this
    /// is a reconciliation aid, and a false error here costs more than a
    /// missed one.
    fn throw_type_matches(&self, actual: &AstType, declared: &AstType) -> bool {
        let is_id = |t: &AstType| t.prim == TypePrim::Id || t.prim == TypePrim::Instancetype;
        if is_id(actual) || is_id(declared) {
            return true;
        }
        if Self::is_object_type(actual) != Self::is_object_type(declared) {
            return false;
        }
        if !Self::is_object_type(actual) {
            return true; // scalars: C's usual conversions apply
        }
        let (Some(a), Some(d)) = (Self::class_name_of(actual), Self::class_name_of(declared)) else {
            return true; // unnamed pointer — cannot disprove
        };
        if a == d {
            return true;
        }
        let Some(ref st) = self.symtab else { return true };
        // Only judge when both names resolve to classes of this unit: an
        // unresolved spelling (namespace-qualified, or defined in another TU)
        // is not evidence of a mismatch.
        if st.find_class(&a).is_none() || st.find_class(&d).is_none() {
            return true;
        }
        self.is_subclass_of(st, &a, &d)
    }

    fn is_subclass_of(&self, st: &SymbolTable, sub: &str, sup: &str) -> bool {
        let mut cur = sub.to_string();
        for _ in 0..64 {
            // Cycle guard: a malformed superclass chain must not hang.
            if cur == sup {
                return true;
            }
            match st.find_class(&cur).map(|c| &c.data) {
                Some(SymbolData::Class { superclass: Some(s), .. }) if !s.is_empty() => {
                    cur = s.clone()
                }
                _ => return false,
            }
        }
        false
    }

    /// Human-readable type spelling for `throws` diagnostics.
    fn type_desc(t: &AstType) -> String {
        let named = t.name.clone();
        let base = match t.prim {
            TypePrim::Id => "id".to_string(),
            TypePrim::Instancetype => "instancetype".to_string(),
            TypePrim::Class => "Class".to_string(),
            TypePrim::Sel => "SEL".to_string(),
            TypePrim::Param => "T".to_string(),
            TypePrim::Void => "void".to_string(),
            _ => named.unwrap_or_else(|| match t.prim {
                TypePrim::Char => "char".to_string(),
                TypePrim::Short => "short".to_string(),
                TypePrim::Int => "int".to_string(),
                TypePrim::Long => "long".to_string(),
                TypePrim::LongLong => "long long".to_string(),
                TypePrim::Float => "float".to_string(),
                TypePrim::Double => "double".to_string(),
                TypePrim::Bool => "BOOL".to_string(),
                TypePrim::Signed => "signed".to_string(),
                TypePrim::Unsigned => "unsigned".to_string(),
                _ => "?".to_string(),
            }),
        };
        if t.is_pointer && !base.ends_with('*') {
            format!("{} *", base)
        } else {
            base
        }
    }

    /// Reconcile the body's `@throw` statements with the declaration's
    /// `throws` annotation, mirroring Java's checked exceptions:
    /// `throws(T)` must cover every `@throw` in the body (subclasses allowed);
    /// a bare `throws` must be telling the truth; an unannotated declaration
    /// may only throw where a local `@catch` handles it.
    fn reconcile_throws(&mut self, name: Option<&str>, line: usize, col: usize) {
        let who = match name {
            Some(n) => format!("'{}'", n),
            None => "this declaration".to_string(),
        };
        match self.throws_ann.clone() {
            None => {
                for (l, c) in std::mem::take(&mut self.uncaught_throws) {
                    self.check_error(
                        l,
                        c,
                        &format!(
                            "'@throw' escapes {} without a 'throws' annotation; declare it with \
                             'throws(<type>)' or handle it with a local '@try'",
                            who
                        ),
                    );
                }
            }
            Some(None) => {
                // Under `-eh checked` every `@throw` was rewritten before this
                // ran, so an empty list here proves nothing.
                if !self.eh_checked && self.thrown.is_empty() {
                    self.check_error(
                        line,
                        col,
                        &format!(
                            "{} is marked 'throws' but its body never executes '@throw'",
                            who
                        ),
                    );
                }
            }
            Some(Some(declared)) => {
                for (l, c, ty) in std::mem::take(&mut self.thrown) {
                    if let Some(actual) = ty {
                        if !self.throw_type_matches(&actual, &declared) {
                            self.check_error(
                                l,
                                c,
                                &format!(
                                    "'@throw' of type '{}' does not match the declared \
                                     'throws({})'",
                                    Self::type_desc(&actual),
                                    Self::type_desc(&declared)
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    // ─── printf-style format checking (-Wformat, step 1: %@) ───────────

    /// Selectors whose first argument is an NPString format literal followed by
    /// printf-style variadic args (the `%@` family). Only this whitelist is
    /// scanned — a random method that happens to take an NPString is not a
    /// format consumer, and scanning it would false-positive.
    const FORMAT_MSGSENDS: &[&str] = &[
        "stringWithFormat:",
        "initWithFormat:",
        "appendFormat:",
        "stringByAppendingFormat:",
    ];

    /// Runtime primitives whose C contract is nil-safe (`if (!obj) return;` /
    /// `return NULL;` — verified in runtime.c: `jeti_release` :222,
    /// `jeti_retain` :214, `jeti_autorelease` :246). A nullable argument to
    /// these is the documented calling convention, not a bug: ARC's scope-end
    /// injection releases locals the analyzer cannot prove non-nil, and
    /// release-before-nil is a normal MRC idiom. A jeti-visible `nonnull`
    /// declaration of one is a lie, and the nullability transfer check refuses
    /// to enforce it (enforcing it would make every ARC program with a
    /// nullable local fail to compile).
    const NIL_SAFE_RUNTIME_FNS: &[&str] = &[
        "jeti_release",
        "jeti_retain",
        "jeti_autorelease",
    ];

    /// The Clang Blocks ABI entry points. Their prototype is
    /// `void *_Block_copy(const void *)` / `void _Block_release(const void *)`,
    /// so the C type system accepts *any* pointer — the operand's block-ness is
    /// not expressed in the signature and cannot be. A non-block operand is
    /// read as a block header and dereferenced (`_Block_copy` inspects
    /// `isa`/`flags`), which segfaults on the spot: the NPSortDescriptor probe
    /// passed an `NPString *` key and died before printing its first line. The
    /// checker supplies the check the ABI cannot.
    const BLOCK_RUNTIME_FNS: &[&str] = &["_Block_copy", "_Block_release"];

    /// Extract printf-style conversion characters from a format-string literal,
    /// in order (`'@'` for `%@`, `'d'` for `%d`, …). `%%` is skipped; flags,
    /// width, precision and length modifiers (`l`/`ll`/`h`/`z`/`t`/`j`/`q`/`L`)
    /// are consumed so `%02ld` yields `'d'`.
    fn format_specifiers(fmt: &str) -> Vec<char> {
        let chars: Vec<char> = fmt.chars().collect();
        let mut specs = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '%' {
                i += 1;
                continue;
            }
            i += 1;
            if i >= chars.len() {
                break;
            }
            if chars[i] == '%' {
                i += 1;
                continue;
            }
            while i < chars.len()
                && matches!(chars[i], '-' | '+' | ' ' | '#' | '\'' | '.' | '*' | '0'..='9')
            {
                i += 1;
            }
            while i < chars.len() && matches!(chars[i], 'l' | 'h' | 'z' | 't' | 'j' | 'q' | 'L') {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            specs.push(chars[i]);
            i += 1;
        }
        specs
    }

    /// Map a printf conversion character to the argument kind it consumes.
    /// Unknown conversions return `None` and are simply not type-checked
    /// (exotic specifiers are UB territory — clang's domain, not ours).
    fn format_arg_kind(c: char) -> Option<FormatArgKind> {
        match c {
            '@' => Some(FormatArgKind::Object),
            'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'c' => Some(FormatArgKind::Int),
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => Some(FormatArgKind::Float),
            's' => Some(FormatArgKind::Str),
            'p' => Some(FormatArgKind::Ptr),
            _ => None,
        }
    }

    /// Human-readable description of a format argument kind, used in warnings.
    fn format_kind_desc(k: FormatArgKind) -> &'static str {
        match k {
            FormatArgKind::Object => "an object (id / class pointer)",
            FormatArgKind::Int => "an integer",
            FormatArgKind::Float => "a floating-point value",
            FormatArgKind::Str => "a C string (char *)",
            FormatArgKind::Ptr => "a pointer",
        }
    }

    /// Does `arg` satisfy the kind its specifier wants? Loose where C is
    /// loose: any pointer kind satisfies `%p`; integers of any width satisfy
    /// any integer conversion; `%s` strictly wants a `char *`.
    fn format_kinds_compatible(spec: FormatArgKind, arg: FormatArgKind) -> bool {
        use FormatArgKind::*;
        match (spec, arg) {
            (Object, Object) => true,
            (Int, Int) => true,
            (Float, Float) => true,
            (Str, Str) => true,
            (Ptr, Str) | (Ptr, Ptr) | (Ptr, Object) => true,
            _ => false,
        }
    }

    /// Classify a declared type for format matching. `None` = "can't tell"
    /// (e.g. a non-pointer named type is a typedef of unknown arity —
    /// `size_t` is an int but a struct alias is not, and the checker has no
    /// scalar-alias table).
    fn type_format_kind(t: &AstType) -> Option<FormatArgKind> {
        // Arrays decay to pointers at the call: `char buf[8]` into `%s` is valid.
        if t.is_array {
            return Some(if t.prim == TypePrim::Char { FormatArgKind::Str } else { FormatArgKind::Ptr });
        }
        match t.prim {
            TypePrim::Char => {
                if t.is_pointer { Some(FormatArgKind::Str) } else { Some(FormatArgKind::Int) }
            }
            TypePrim::Short | TypePrim::Int | TypePrim::Long | TypePrim::LongLong
            | TypePrim::Signed | TypePrim::Unsigned | TypePrim::Bool => {
                if t.is_pointer { Some(FormatArgKind::Ptr) } else { Some(FormatArgKind::Int) }
            }
            TypePrim::Float | TypePrim::Double => {
                if t.is_pointer { Some(FormatArgKind::Ptr) } else { Some(FormatArgKind::Float) }
            }
            TypePrim::Id | TypePrim::Instancetype | TypePrim::Class => Some(FormatArgKind::Object),
            TypePrim::Named => {
                if t.is_pointer { Some(FormatArgKind::Object) } else { None }
            }
            // SEL / Param / Void: don't judge.
            _ => None,
        }
    }

    /// Conservative classification of an argument expression. `None` means
    /// "can't tell" — the slot is left unchecked: this checker types every
    /// call as `id`, so judging call results would false-positive on
    /// integer-returning helpers like `[n intValue]`. Only provable facts
    /// (literals, casts, locals/params with known types) classify.
    fn arg_format_kind(&self, e: &AstExpr) -> Option<FormatArgKind> {
        match &e.data {
            AstExprData::Int(_) | AstExprData::Char(_) | AstExprData::Bool(_) => Some(FormatArgKind::Int),
            AstExprData::Float(_) => Some(FormatArgKind::Float),
            // Bare C string: `char *` (same rule as the generic bare-string
            // argument check — it is not an object).
            AstExprData::String(_) => Some(FormatArgKind::Str),
            AstExprData::AtString(_) => Some(FormatArgKind::Object),
            // A cast states its target type explicitly — user intent, reliable.
            AstExprData::Cast { target_type, .. } => Self::type_format_kind(target_type),
            AstExprData::VarRef { name, .. } => {
                for scope in self.scope_vars.iter().rev() {
                    for (vname, vtype) in scope.iter() {
                        if vname == name {
                            return Self::type_format_kind(vtype);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Check the format string against the variadic arguments that follow
    /// it: every specifier must have an argument, extra arguments warn
    /// (they are silently ignored at runtime), and each argument's kind
    /// must be compatible with its specifier. Unclassifiable arguments
    /// leave their slots unchecked (see `arg_format_kind`).
    fn check_format_args(&mut self, fmt: &str, args: &[AstExpr], line: usize, col: usize, what: &str) {
        let specs = Self::format_specifiers(fmt);
        // Count check first: a mismatch makes positional pairing meaningless,
        // so kind checks below are skipped when the counts disagree.
        if specs.len() != args.len() {
            let (more, side) = if specs.len() > args.len() {
                ("more format specifiers than", "arguments")
            } else {
                ("more arguments than", "format specifiers")
            };
            self.check_warning(line, col, &format!(
                "format string of {} has {} {}", what, more, side));
            return;
        }
        for (i, c) in specs.iter().enumerate() {
            let Some(spec) = Self::format_arg_kind(*c) else { continue };
            let Some(a) = args.get(i) else { continue };
            let Some(arg) = self.arg_format_kind(a) else { continue };
            if !Self::format_kinds_compatible(spec, arg) {
                let mut msg = format!(
                    "format specifier %{c} (argument {} of {what}) expects {exp}, but the argument is {got}",
                    i + 1,
                    exp = Self::format_kind_desc(spec),
                    got = Self::format_kind_desc(arg),
                );
                // Hint for the two classic mixups.
                if spec == FormatArgKind::Object {
                    msg.push_str(" — use %d/%s/%f for scalars");
                } else if arg == FormatArgKind::Object {
                    msg.push_str(" — use %@ for NPString objects");
                }
                self.check_warning_over(a.line, a.col, a.end_line, a.end_col, &msg);
            }
        }
    }

    /// True if `name` refers to a definitely-instance value: a local variable
    /// in scope, a method parameter, or an ivar of the current class.
    /// (Specifically NOT a class name and NOT a dynamic `[... class]` expression.)
    fn is_local_var(&self, name: &str) -> bool {
        for scope in self.scope_vars.iter().rev() {
            for (n, _) in scope.iter() {
                if n == name { return true; }
            }
        }
        if name == "self" || name == "_self" { return true; }
        if let (Some(ref cls_name), Some(ref st)) = (&self.current_class, &self.symtab) {
            if let Some(csym) = st.find_class(cls_name) {
                if let SymbolData::Class { ref ivars, .. } = csym.data {
                    if ivars.contains(&name.to_string()) { return true; }
                }
            }
        }
        false
    }

    /// Collect method and function signatures from the AST into the
    /// `method_params` / `function_params` maps.  Runs once before checking.
    fn collect_signatures(&mut self, decl: &AstDecl) {
        match &decl.data {
            // `@class X;` — the class exists (the binder registers a symbol) but
            // nothing about its interface is visible in this TU. Recorded so
            // `class_declares_selector` can answer "unknown" rather than
            // "declares nothing".
            AstDeclData::ForwardClass { names } => {
                for n in names {
                    self.forward_declared_classes.insert(n.clone());
                }
            }
            AstDeclData::Typedef { aliased_type: Some(at), .. } => {
                // Block-typed and function-pointer typedefs (`typedef void
                // (^SimpleBlock)(void)`, `typedef int (*IntFn)(int)`): the
                // alias spelling carries no callee flag on variable types,
                // so the call family's called-object guard consults this
                // registry to keep calls through aliases legal.
                if at.is_block || at.is_fn_ptr {
                    if let Some(alias) = &decl.name {
                        self.block_alias_names.insert(alias.clone());
                    }
                }
            }
            AstDeclData::Class { methods, properties, .. } => {
                let cls_name = decl.name.clone();
                if let Some(ref cn) = cls_name {
                    self.visible_interface_classes.insert(cn.clone());
                }
                for m in methods {
                    if let AstDeclData::Method { params, is_class_method, return_type, .. } = &m.data {
                        if let Some(ref sel) = m.name {
                            let mut v = Vec::new();
                            let mut cur = params.as_ref().map(|b| &**b);
                            while let Some(p) = cur {
                                v.push(p.par_type.as_ref().map(|pt| Self::cst_type_to_ast_type(pt)));
                                cur = p.next.as_ref().map(|n| &**n);
                            }
                            self.method_params.insert(sel.clone(), v.clone());
                            self.method_returns.insert(sel.clone(), return_type.as_ref().map(|rt| (**rt).clone()));
                            self.method_kinds.entry(sel.clone()).or_default().push(*is_class_method);
                            // Per-class signature table: same selector, two
                            // classes, different param types — the global
                            // selector-keyed table clobbers one of them.
                            if let Some(ref cn) = cls_name {
                                self.method_params_by_class
                                    .entry(cn.clone())
                                    .or_default()
                                    .insert(sel.clone(), v.clone());
                                self.method_kinds_by_class
                                    .entry(cn.clone())
                                    .or_default()
                                    .insert(sel.clone(), *is_class_method);
                            }
                            self.method_param_decls.entry(sel.clone()).or_default().push(v);
                        }
                    }
                }
                // @property synthesizes a getter `prop` and a setter
                // `setProp:` — register them so the selector-visibility check
                // doesn't reject legal property sends (tt.jeti's `node.nextNode`
                // / `setNextNode:`). No ivar-existence guard here: `@synthesize
                // prop = _ivar` puts `_ivar` in the AST's ivar list, but the
                // accessors exist regardless (codegen emits them for
                // synthesized properties either way). A user-declared method
                // with the same selector wins — the methods loop above ran
                // first, and the `contains_key` checks below keep this
                // idempotent.
                for p in properties {
                    if let AstDeclData::Property { prop_type, is_readonly, is_dynamic, .. } = &p.data {
                        if *is_dynamic { continue; }
                        let Some(pname) = p.name.as_deref() else { continue };
                        let Some(ref cn) = cls_name else { continue };
                        let pt = prop_type.as_ref().map(|t| (**t).clone());
                        let class_table = self.method_params_by_class
                            .entry(cn.clone()).or_default();
                        if !class_table.contains_key(pname) {
                            self.method_params.insert(pname.to_string(), vec![]);
                            self.method_returns.insert(pname.to_string(), pt.clone());
                            self.method_kinds.entry(pname.to_string()).or_default().push(false);
                            self.method_kinds_by_class
                                .entry(cn.clone()).or_default()
                                .insert(pname.to_string(), false);
                            class_table.insert(pname.to_string(), vec![]);
                            self.method_param_decls.entry(pname.to_string()).or_default().push(vec![]);
                        }
                        if !*is_readonly {
                            let setter = match pname.chars().next() {
                                Some(c0) => format!(
                                    "set{}{}:", c0.to_uppercase(), &pname[c0.len_utf8()..]),
                                None => continue,
                            };
                            if !class_table.contains_key(&setter) {
                                self.method_params.insert(setter.clone(), vec![pt.clone()]);
                                self.method_returns.insert(setter.clone(), None);
                                self.method_kinds.entry(setter.clone()).or_default().push(false);
                                self.method_kinds_by_class
                                    .entry(cn.clone()).or_default()
                                    .insert(setter.clone(), false);
                                class_table.insert(setter.clone(), vec![pt.clone()]);
                                self.method_param_decls.entry(setter.clone()).or_default().push(vec![pt]);
                            }
                        }
                    }
                }
            }
            AstDeclData::Method { params, is_class_method, return_type, .. } => {
                if let Some(ref sel) = decl.name {
                    let mut v = Vec::new();
                    let mut cur = params.as_ref().map(|b| &**b);
                    while let Some(p) = cur {
                        v.push(p.par_type.as_ref().map(|pt| Self::cst_type_to_ast_type(pt)));
                        cur = p.next.as_ref().map(|n| &**n);
                    }
                    self.method_params.insert(sel.clone(), v.clone());
                    self.method_returns.insert(sel.clone(), return_type.as_ref().map(|rt| (**rt).clone()));
                    self.method_kinds.entry(sel.clone()).or_default().push(*is_class_method);
                    self.method_param_decls.entry(sel.clone()).or_default().push(v);
                }
            }
            AstDeclData::Function { params, return_type, has_variadic, .. } => {
                if let Some(ref name) = decl.name {
                    let mut v = Vec::new();
                    let mut names = Vec::new();
                    let mut cur = params.as_ref().map(|b| &**b);
                    while let Some(p) = cur {
                        // `(void)` parameter lists are the C17 6.7.6.3 spelling
                        // of "no parameters" — they are not a parameter (clang
                        // would count zero). An unnamed parameter of other
                        // type still counts as a position.
                        let is_void_marker = p.par_type.as_ref().map_or(false, |pt| {
                            pt.prim == TypePrim::Void && !pt.is_pointer && p.name.is_none()
                        });
                        if !is_void_marker {
                            v.push(p.par_type.as_ref().map(|pt| Self::cst_type_to_ast_type(pt)));
                            names.push(p.name.clone());
                        }
                        cur = p.next.as_ref().map(|n| &**n);
                    }
                    self.function_params.insert(name.clone(), v);
                    self.function_param_names.insert(name.clone(), names);
                    self.function_returns.insert(name.clone(), return_type.as_ref().map(|rt| (**rt).clone()));
                    self.function_variadic.insert(name.clone(), *has_variadic);
                }
            }
            _ => {}
        }
    }

    fn cst_type_to_ast_type(ct: &jeti_cst::CstType) -> AstType {
        AstType::from_cst_type(ct)
    }

    /// Substitute a specialized receiver's type_args into a method signature
    /// type at Param (T) positions. Bare spellings (no type_args) leave the
    /// signature untouched — full erasure, zero migration for existing code.
    /// `param_names` is the class's declared type_params in order (e.g.
    /// `["A", "B"]` for `Pair<A, B>`); Param nodes carry their parameter name
    /// (the parser keeps it), so multi-param generics match by name. A Param
    /// whose name is unknown or absent falls back to args[0] — the historical
    /// single-parameter behavior.
    fn substitute_type_args(ty: &AstType, args: &[AstType], param_names: &[String]) -> AstType {
        let mut t = ty.clone();
        if t.prim == TypePrim::Param {
            let idx = t.name.as_deref()
                .and_then(|n| param_names.iter().position(|p| p == n));
            let chosen = idx.and_then(|i| args.get(i)).or_else(|| args.first());
            if let Some(arg) = chosen {
                let mut c = arg.clone();
                c.is_pointer = c.is_pointer || ty.is_pointer;
                return c;
            }
        }
        if let Some(ref mut sub) = t.subtype {
            **sub = Self::substitute_type_args(sub, args, param_names);
        }
        if !t.type_args.is_empty() {
            for a in t.type_args.iter_mut() { *a = Self::substitute_type_args(a, args, param_names); }
        }
        t
    }

    /// True if this signature type contains a Param (T) slot anywhere
    /// (including nested block params and type_args).
    fn type_has_param(ty: &AstType) -> bool {
        if ty.prim == TypePrim::Param { return true; }
        if let Some(ref sub) = ty.subtype { if Self::type_has_param(sub) { return true; } }
        if let Some(ref bp) = ty.block_params { if Self::type_has_param(bp) { return true; } }
        ty.type_args.iter().any(Self::type_has_param)
    }

    /// Generic-erasure warning: a bare generic template assigned to a
    /// specialization of the same base class. Layout-identical (always safe
    /// at the machine level) but the bare side's element type is unknown —
    /// this is the "type laundering" channel. Warning, not error, matching
    /// clang's treatment of ObjC lightweight generics; `-Werror` upgrades it.
    /// Only fires bare → specialized; specialized → bare silently loses the
    /// promise, which is safe and allowed.
    fn init_generic_erasure_warning(&self, vt: &AstType, it: &AstType) -> Option<String> {
        if vt.prim != TypePrim::Named || it.prim != TypePrim::Named { return None; }
        // Generic-ness lives in type_args OR in the name itself: direct
        // spellings (`NPArray<NPString *>`) carry type_args, while `@using`
        // alias expansion bakes `<...>` into the resolved name with
        // type_args left empty. Cover both.
        let name_is_gen = |n: &str| n.contains('<');
        let v_gen = !vt.type_args.is_empty() || vt.name.as_deref().map(name_is_gen).unwrap_or(false);
        let i_gen = !it.type_args.is_empty() || it.name.as_deref().map(name_is_gen).unwrap_or(false);
        if !v_gen || i_gen { return None; } // only bare (declared) ← specialized (expr)
        let (Some(vn), Some(iname)) = (&vt.name, &it.name) else { return None };
        // Same base class (bare NPArray vs NPArray<X>), ignoring subclass
        // relations — those take the normal compatibility path.
        let v_base = vn.split('<').next().unwrap_or(vn);
        let i_base = iname.split('<').next().unwrap_or(iname);
        if v_base != i_base { return None; }
        Some(format!(
            "assigning a bare '{}' to a specialization of it — the bare container's element type is unchecked; add an explicit cast if the contents are known to match",
            Self::type_display(it)))
    }

    /// Pointer/object vs scalar mismatch on a declaration initializer.
    /// Returns Some(message) for provably wrong mixes (`int v = <ptr expr>`),
    /// None when either side is unknown or the mix is legal C. Generic
    /// substitution makes the init type concrete, so `int v = [m
    /// objectAtIndex:0]` is now catchable where it used to slip through.
    /// The scalar-into-pointer direction only fires for provable literals:
    /// unknown-typed expressions (ivar subscripts etc.) fall back to `int`,
    /// and flagging those would flood Foundation's own code with false
    /// positives.
    fn init_ptr_scalar_mismatch(&self, vt: &AstType, it: &AstType, init: &AstExpr) -> Option<String> {
        if std::env::var("JETI_DEBUG_INIT").is_ok() {
            eprintln!("[init-debug] vt={:?}/{:?} it={:?}/{:?}", vt.name, vt.prim, it.name, it.prim);
        }
        let v_ptr = Self::is_object_type(vt) || vt.is_pointer;
        let i_ptr = Self::is_object_type(it) || it.is_pointer;
        if v_ptr == i_ptr {
            // Both object-typed: two *named* class pointers must be name-equal
            // or superclass-related (`NPNumber *bad = [strArray
            // objectAtIndex:0]` is the generic-mismatch bug this catches).
            if v_ptr && vt.prim == TypePrim::Named && it.prim == TypePrim::Named {
                if !self.named_types_compatible(vt, it) {
                    if std::env::var("JETI_DEBUG_INIT").is_ok() {
                        eprintln!("[init-debug2] MISMATCH vt={:?} ptr={} args={} | it={:?} ptr={} args={}",
                            vt.name, vt.is_pointer, vt.type_args.len(), it.name, it.is_pointer, it.type_args.len());
                    }
                    return Some(format!(
                        "initializing '{}' from expression of incompatible object type '{}'",
                        Self::type_display(vt), Self::type_display(it)));
                }
            }
            return None;
        }
        if v_ptr {
            // Only provable scalars: literal expressions with known kinds.
            // `0` is a legal C null-pointer constant (`Foo *p = 0;`) — exempt.
            let literal_scalar = matches!(init.data,
                AstExprData::Int(_) | AstExprData::Float(_) | AstExprData::FloatRaw(_)
                | AstExprData::Bool(_) | AstExprData::Char(_));
            let is_zero = matches!(&init.data, AstExprData::Int(0));
            if !literal_scalar || is_zero { return None; }
            Some(format!(
                "initializing '{}' from scalar expression of type '{}' — pointer/object expected",
                Self::type_display(vt), Self::type_display(it)))
        } else {
            // Pointer/object init into a scalar decl: only fire when the object
            // type is a *concrete named class* (substituted generic results).
            // A raw `id` init type means the send's return was unknown/fallback
            // (`[someId count]` etc.) — flagging those floods real code.
            if it.prim != TypePrim::Named || it.name.is_none() { return None; }
            Some(format!(
                "initializing '{}' from pointer/object expression of type '{}' — scalar expected",
                Self::type_display(vt), Self::type_display(it)))
        }
    }

    /// True if `actual` can be passed where `param` (already substituted) is
    /// expected. Object-kind looseness follows the established rules: `id`
    /// accepts anything object-shaped; two *named* class pointers must be
    /// name-equal or related by the superclass chain (substitution makes T
    /// concrete — `NPNumber *` into an `NPString *` container is exactly the
    /// bug this check exists to catch). Scalars must match non-pointer scalars.
    fn arg_type_ok(&self, param: &AstType, actual: &AstType) -> bool {
        let param_obj = Self::is_object_type(param) || param.prim == TypePrim::Param;
        let actual_obj = Self::is_object_type(actual);
        if param_obj && actual_obj {
            // `id` is the universal object type.
            if param.prim == TypePrim::Id || actual.prim == TypePrim::Id { return true; }
            // Two named class pointers: name-equal or subclass-related.
            if param.prim == TypePrim::Named && actual.prim == TypePrim::Named {
                return self.named_types_compatible(param, actual);
            }
            return true;
        }
        if param_obj != actual_obj { return false; }
        // Both scalars: compatible when the same primitive family.
        param.prim == actual.prim
    }

    /// Named class pointer compatibility: name equality or superclass-chain
    /// relation in either direction (sub → super assigns; super → sub is the
    /// ObjC downcast convention clang only warns about — allow with the same
    /// looseness as the rest of this checker). Unknown names (forward decls,
    /// foreign typedefs) are accepted — never guess.
    fn named_types_compatible(&self, a: &AstType, b: &AstType) -> bool {
        let (Some(an), Some(bn)) = (&a.name, &b.name) else { return true };
        // A bare generic template and its specialization are layout-identical
        // (monomorphization only rewrites T positions; verified byte-for-byte
        // for NPArray), so `VectorBuffer *` ↔ `VectorBuffer<X> *` assign fine
        // in both directions. Names differ only by the `<...>` suffix.
        let a_base = an.split('<').next().unwrap_or(an);
        let b_base = bn.split('<').next().unwrap_or(bn);
        if a_base != b_base {
            let Some(ref st) = self.symtab else { return true };
            return st.find_class(an).map(|c| &c.data).is_some()
                && st.find_class(bn).map(|c| &c.data).is_some()
                && (self.is_subclass_of(st, an, bn) || self.is_subclass_of(st, bn, an));
        }
        // Same base class: element type_args must match too (invariant,
        // user-decided). `@[ @1 ]` infers NPArray<NPNumber *> — assigning it
        // to NPArray<NPString *> must fail even though the names are equal.
        // Recurse through arg_type_ok so subclasses stay acceptable.
        if a.type_args.len() != b.type_args.len() {
            // One side bare, one specialized: layout-identical, allowed.
            return true;
        }
        for (ta, tb) in a.type_args.iter().zip(b.type_args.iter()) {
            if !self.arg_type_ok(tb, ta) { return false; }
        }
        true
    }

    /// Check an expression, set its expr_type, and return the type.
    pub fn check_expr(&mut self, e: &mut AstExpr) -> Option<AstType> {
        let result = self.check_expr_inner(e);
        e.expr_type = result.clone().map(Box::new);
        // C17 6.5.4 cast family diagnostics (checked against clang's default
        // severities — see c_standard_diagnostics_plan §G-expr). Runs after
        // `check_expr_inner` because the match there holds `&mut e.data` and
        // this pass needs the node's span fields.
        if let AstExprData::Cast { target_type, expr } = &e.data {
            let mut src_expr = expr.clone();
            self.in_clone_eval += 1;
            let src_ty = self.check_expr(&mut src_expr);
            self.in_clone_eval -= 1;
            self.check_cast_types(&e.line, &e.col, e.end_line, e.end_col,
                                  target_type, &src_expr, src_ty.as_ref());
        }
        // C17 6.5.2.2 call family — post-inner so the node's span fields
        // exist, and only outside clone evaluation (double-reporting).
        if self.in_clone_eval == 0 {
            if matches!(&e.data, AstExprData::FuncCall { .. }) {
                self.check_call_family(e);
            }
            // C17 6.5.16 assignment family — same post-inner shape. The
            // inner Assign arm only visits the target, so the value's expr
            // type is recorded here through cloned evaluation.
            if let AstExprData::Assign { target, value } = &e.data {
                let mut tclone = (**target).clone();
                let mut vclone = (**value).clone();
                self.in_clone_eval += 1;
                // Target eval still runs for its own diagnostics (undeclared
                // identifiers, ivar-in-class-method); the family reads the
                // target type from the scope frames instead — snapshots of
                // the loose chain are not trustworthy for conversions.
                self.check_expr(&mut tclone);
                let vt = self.check_expr(&mut vclone);
                self.in_clone_eval -= 1;
                // Call family inside assignment values: the clone eval runs
                // with the post-inner hooks suppressed, so every call in the
                // value/target subtree gets its family check through the
                // dedicated deep walk (each clone node fires exactly once —
                // the real subtree is never visited outside this clone).
                self.check_call_family_deep(&tclone);
                self.check_call_family_deep(&vclone);
                self.check_assign_family(e, &tclone, &vclone, vt.as_ref());
            }
        }
        // `a[i]` on an object receiver is not C indexing — C rejects it outright
        // ("operand of type 'NPArray' where arithmetic or pointer type is
        // required"), and the error surfaces in the generated C with no trace
        // back to the source line. Rewrite it to `[a objectAtIndex:i]` here,
        // where the receiver's declared type is known.
        if let Some(new_ty) = self.maybe_rewrite_object_subscript(e) {
            return Some(new_ty);
        }
        // `@(x + 1)` — the NPNumber factory depends on the operand's static
        // type, which only exists here; C99 has no `_Generic` to fall back on.
        if let Some(new_ty) = self.maybe_rewrite_boxed_expr(e) {
            return Some(new_ty);
        }
        if let Some(new_ty) = self.maybe_rewrite_object_assign(e) {
            return Some(new_ty);
        }
        self.maybe_rewrite_struct_eq(e);
        result
    }

    /// Warn when a type carries type arguments but names a class that declares
    /// no type parameters. Jeti's monomorphization is driven by *declaration*,
    /// so `NPArray<NPString *> *` parses and type-checks but never
    /// monomorphizes: the generated C contains no `NPArray_NPString` at all
    /// and `objectAtIndex:` still returns `NPObject *`. That silent erasure is
    /// a usability trap — the reader reasonably expects element type checking.
    /// The user-declared generic containers (e.g. `Box<T>`) are exempt because
    /// those DO monomorphize.
    fn warn_if_erased_generics(&mut self, t: &AstType, line: usize, col: usize) {
        if t.type_args.is_empty() { return; }
        // Defensive: a `<...>` block routes to EITHER a protocol list OR type
        // args, never both. If a type somehow carries both, the parser
        // misrouted it (e.g. a reused type-param name once did), and warning
        // "erasure" here would be a false positive — the class really was
        // declared generic.
        if !t.protocol_refs.is_empty() { return; }
        let Some(name) = Self::class_name_of(t) else { return };
        let Some(ref st) = self.symtab else { return };
        // Unresolved spelling (defined in another TU, or namespace-qualified in a
        // way we do not resolve): stay silent rather than guess.
        let Some(sym) = st.find_class(&name) else { return };
        let SymbolData::Class { type_params, .. } = &sym.data else { return };
        if !type_params.is_empty() { return; }
        self.check_warning(line, col, &format!(
            "type arguments on non-generic class '{}' are ignored — it declares no type parameters, so no specialized type is generated and element types are not checked; drop the '<...>' or use a generic container",
            name));
    }

    // ── Generic protocol bounds (`T : Proto`) — docs/plans/generic_bounds_plan.md ──

    /// Short name of a possibly-namespaced symbol (`ns::P` → `P`).
    fn short_type_name(n: &str) -> &str {
        n.rsplit("::").next().unwrap_or(n)
    }

    /// Resolve a protocol symbol by exact or short name and return its parent
    /// protocol names. Mirrors the tolerant resolution in
    /// `check_protocol_conformance` (short or namespace-qualified spellings).
    fn protocol_parents(&self, name: &str) -> Option<Vec<String>> {
        let st = self.symtab.as_ref()?;
        let mut short_match: Option<Vec<String>> = None;
        for sym in &st.global.symbols {
            if let SymbolData::Protocol { parents, .. } = &sym.data {
                if sym.name == name {
                    return Some(parents.clone());
                }
                if short_match.is_none()
                    && Self::short_type_name(&sym.name) == Self::short_type_name(name)
                {
                    short_match = Some(parents.clone());
                }
            }
        }
        short_match
    }

    /// Is `ancestor` equal to `proto` or on its protocol-inheritance chain?
    fn protocol_extends(&self, proto: &str, ancestor: &str) -> bool {
        if Self::short_type_name(proto) == Self::short_type_name(ancestor) { return true; }
        let mut seen: Vec<String> = Vec::new();
        let mut queue: Vec<String> = vec![proto.to_string()];
        while let Some(p) = queue.pop() {
            if seen.iter().any(|s| *s == p) { continue; }
            seen.push(p.clone());
            let Some(parents) = self.protocol_parents(&p) else { continue };
            for par in parents {
                if Self::short_type_name(&par) == Self::short_type_name(ancestor) { return true; }
                queue.push(par);
            }
        }
        false
    }

    /// Does `cls` (or any superclass) list `proto` (or a descendant of it)?
    fn class_conforms_to_protocol(&self, cls: &str, proto: &str) -> bool {
        let Some(ref st) = self.symtab else { return false };
        let mut cur = cls.to_string();
        for _ in 0..64 {
            let Some(sym) = st.find_class(&cur) else { return false };
            let SymbolData::Class { protocols, superclass, .. } = &sym.data else { return false };
            for p in protocols {
                if self.protocol_extends(p, proto) { return true; }
            }
            match superclass {
                Some(s) if !s.is_empty() => cur = s.clone(),
                _ => return false,
            }
        }
        false
    }

    /// Is `cls` equal to `ancestor` or derived from it (superclass walk)?
    fn class_derives_from(&self, cls: &str, ancestor: &str) -> bool {
        if Self::short_type_name(cls) == Self::short_type_name(ancestor) { return true; }
        let Some(ref st) = self.symtab else { return false };
        let mut cur = cls.to_string();
        for _ in 0..64 {
            let Some(sym) = st.find_class(&cur) else { return false };
            let SymbolData::Class { superclass, .. } = &sym.data else { return false };
            match superclass {
                Some(s) if !s.is_empty() => {
                    if Self::short_type_name(s) == Self::short_type_name(ancestor) { return true; }
                    cur = s.clone();
                }
                _ => return false,
            }
        }
        false
    }

    /// Is the argument class a fully-declared symbol? A forward declaration
    /// (`@class X`) leaves a shell with no members, and the real declaration
    /// may live in another TU — judging a shell would be a false positive, so
    /// shells escape (§3 escape channels; a missed report beats a false one).
    fn arg_class_is_resolved(&self, name: &str) -> bool {
        let Some(ref st) = self.symtab else { return false };
        let Some(sym) = st.find_class(name) else { return false };
        match &sym.data {
            SymbolData::Class { protocols, methods, ivars, properties, .. } => {
                !(protocols.is_empty() && methods.is_empty()
                    && ivars.is_empty() && properties.is_empty())
            }
            _ => false,
        }
    }

    /// §4 diagnostic when `arg` violates the bound on `param` of `cls`, or
    /// None when it passes: conforms, is an escape spelling (`id`,
    /// `instancetype`, a nested type-param slot), names an unresolved class,
    /// or the bound itself resolves to nothing (unknown name).
    fn bound_violation(&self, arg: &AstType, bound: &str, cls: &str, param: &str) -> Option<String> {
        if arg.prim == TypePrim::Id || arg.prim == TypePrim::Instancetype { return None; }
        if arg.prim == TypePrim::Param { return None; } // nested generic slot (Box<S<T>>)
        let Some(arg_name) = Self::class_name_of(arg) else { return None };
        let arg_base = arg_name.split('<').next().unwrap_or(&arg_name).to_string();
        // The bound names a protocol or a class; if it resolves to neither,
        // the name is unknown — escape (unverifiable → silent).
        let is_proto = self.protocol_parents(bound).is_some();
        let arg_ok = if is_proto {
            self.class_conforms_to_protocol(&arg_base, bound)
        } else if self.symtab.as_ref().and_then(|st| st.find_class(bound)).is_some() {
            // Class-pointer bound (`T : NSObject *`): the argument must be
            // that class or derive from it.
            self.class_derives_from(&arg_base, bound)
        } else {
            return None;
        };
        if arg_ok { return None; }
        if !self.arg_class_is_resolved(&arg_base) { return None; }
        let spelling = format!("{}<{} : {}>", cls, param, bound);
        if is_proto {
            Some(format!(
                "'{}' does not conform to protocol '{}' required by '{}' — add <{}> to {}'s declaration or use a conforming type argument",
                arg_base, bound, spelling, bound, arg_base))
        } else {
            Some(format!(
                "'{}' is not a subclass of '{}' required by '{}' — use a conforming type argument",
                arg_base, bound, spelling))
        }
    }

    /// §3: check one explicitly-specialized instantiation (`Box<Arg…> *v;`)
    /// against the bounds declared on Box and, where the positional mapping
    /// is provable, on its generic ancestors (§5.3). Fired from the Variable
    /// arm of `check_decl` — the point where every explicit specialization
    /// enters the checker. Bare spellings carry no type_args and never
    /// trigger (erasure compatibility; today's code keeps compiling).
    fn check_generic_bounds_instantiation(&mut self, t: &AstType, line: usize, col: usize) {
        if t.type_args.is_empty() { return; }
        if !t.protocol_refs.is_empty() { return; } // parser routed to a protocol list
        let Some(name) = Self::class_name_of(t) else { return };
        // `@using` aliases can bake `<...>` into the resolved name
        // (init_generic_erasure_warning precedent) — strip to the base class.
        let base = name.split('<').next().unwrap_or(&name).to_string();
        // Snapshot the bound table so the symtab borrow ends before error
        // reporting (&mut self) — same shape as check_protocol_conformance.
        let (params, bounds, super_name) = {
            let Some(ref st) = self.symtab else { return };
            let Some(sym) = st.find_class(&base) else { return };
            let SymbolData::Class { type_params, type_bounds, superclass, .. } = &sym.data else { return };
            (type_params.clone(), type_bounds.clone(), superclass.clone())
        };
        let mut violations: Vec<String> = Vec::new();
        // args[i] pairs with params[i] (substitute_type_args pairing); bounds
        // are keyed by parameter name.
        for (pname, bound) in &bounds {
            let Some(idx) = params.iter().position(|p| p == pname) else { continue };
            let Some(arg) = t.type_args.get(idx) else { continue };
            if let Some(msg) = self.bound_violation(arg, bound, &base, pname) {
                violations.push(msg);
            }
        }
        // §5.3: ancestor bounds apply to the specialization too — but only
        // where the slot mapping is provable (the `MutableBox<T> : Box<T>`
        // idiom: the same parameter name at the same index). Anything else is
        // unverifiable and escapes.
        let mut cur = super_name;
        for _ in 0..64 {
            let Some(p) = cur else { break };
            if p.is_empty() { break; }
            let Some(ref st) = self.symtab else { break };
            let Some(sym) = st.find_class(&p) else { break };
            let SymbolData::Class { type_params, type_bounds, superclass, .. } = &sym.data else { break };
            for (pname, bound) in type_bounds {
                let Some(idx) = type_params.iter().position(|q| q == pname) else { continue };
                if params.get(idx).map(String::as_str) != Some(pname.as_str()) { continue; }
                let Some(arg) = t.type_args.get(idx) else { continue };
                if let Some(msg) = self.bound_violation(arg, bound, &p, pname) {
                    violations.push(msg);
                }
            }
            cur = superclass.clone();
        }
        for msg in violations {
            self.check_error(line, col, &msg);
        }
    }

    /// Does `cls` (or any superclass) declare an instance method `sel`?
    /// Selector spelling is normalized by dropping EVERY colon: the binder
    /// stores `setObject:atIndex:` as `setObjectatIndex` and `objectAtIndex:`
    /// as `objectAtIndex`, so a single-part and a multi-part selector need the
    /// same treatment.
    fn class_has_instance_method(&self, cls: &str, sel: &str) -> bool {
        let Some(ref st) = self.symtab else { return false };
        let norm = |s: &str| s.chars().filter(|c| *c != ':').collect::<String>();
        let want = norm(sel);
        let mut cur = cls.to_string();
        for _ in 0..64 {
            let Some(sym) = st.find_class(&cur) else { return false };
            let SymbolData::Class { methods, superclass, .. } = &sym.data else { return false };
            if methods.iter().any(|m| norm(m) == want) { return true; }
            match superclass {
                Some(s) if !s.is_empty() => cur = s.clone(),
                _ => return false,
            }
        }
        false
    }

    /// Rewrite `@(expr)` into the `NPNumber` factory matching the operand's
    /// static type. The parser cannot make this choice (it has no types) and the
    /// C99 backend has no `_Generic`, so it is made here, where `expr_type` is
    /// known. Non-arithmetic operands are rejected instead of silently boxed:
    /// ObjC would box an `NSString *`, but Jeti has no string boxing, and
    /// handing back a number where an object was meant hides the mistake.
    fn maybe_rewrite_boxed_expr(&mut self, e: &mut AstExpr) -> Option<AstType> {
        let inner_ty = {
            let AstExprData::Boxed(inner) = &e.data else { return None };
            inner.expr_type.as_deref().cloned()
        };
        let Some(ty) = inner_ty else { return None };
        let sel = match ty.prim {
            TypePrim::Double | TypePrim::Float => "numberWithDouble:",
            TypePrim::Bool => "numberWithBool:",
            TypePrim::Char => "numberWithChar:",
            TypePrim::LongLong | TypePrim::Long => "numberWithLongLong:",
            TypePrim::Int | TypePrim::Short | TypePrim::Signed | TypePrim::Unsigned => "numberWithInt:",
            // A typedef'd integer (`typedef int MyInt;`) arrives as a named
            // type. A *struct* typedef must not be boxed, hence the guard.
            TypePrim::Named if !ty.is_pointer && !ty.is_struct => "numberWithInt:",
            _ => {
                let tstr = Self::type_display(&ty);
                self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                    "illegal type '{}' in a boxed expression — '@(...)' accepts arithmetic and BOOL values only",
                    tstr));
                return None;
            }
        };
        let (line, col) = (e.line, e.col);
        let inner = match std::mem::replace(&mut e.data, AstExprData::Int(0)) {
            AstExprData::Boxed(inner) => *inner,
            _ => unreachable!("matched Boxed above"),
        };
        let mut nt = AstType::new(TypePrim::Named);
        nt.name = Some("NPNumber".to_string());
        nt.class_ref = Some("NPNumber".to_string());
        nt.is_pointer = true;
        e.kind = AstExprKind::MsgSend;
        e.expr_type = Some(Box::new(nt.clone()));
        e.data = AstExprData::MsgSend {
            receiver: Box::new(AstExpr {
                kind: AstExprKind::VarRef, expr_type: None, line, col,
                data: AstExprData::VarRef { sym: None, name: "NPNumber".to_string() },
                end_line: 0,
                end_col: 0,
            }),
            method: None,
            vtable_index: -1,
            is_class_method: true,
            is_super: false,
            super_name: None,
            selector: sel.to_string(),
            args: vec![inner],
        };
        Some(nt)
    }

    /// The receiver class of a subscript when it is an *object* (a class
    /// pointer declaring an `objectAtIndex:` method), else `None` for a genuine
    /// C array/pointer subscript which must pass through untouched.
    fn subscript_receiver_class(&self, object: &AstExpr) -> Option<String> {
        let t = self.receiver_static_type(object)?;
        // Arrays and scalars are C indexing. `id`/`instancetype` are untyped
        // receivers with no known class, so they stay C indexing too.
        if t.prim != TypePrim::Named || !t.is_pointer || t.is_array {
            return None;
        }
        let cls = Self::class_name_of(&t)?;
        // Two container families opt in: indexed (objectAtIndex:) and keyed
        // (objectForKey: — dictionaries). The rewrite below picks the actual
        // selector from what the class declares.
        if self.class_has_instance_method(&cls, "objectAtIndex:")
            || self.class_has_instance_method(&cls, "objectForKey:")
        {
            return Some(cls);
        }
        None
    }

    /// Static type of a subscript receiver. Falls back to the scope table when
    /// the node carries no `expr_type`: the `Assign` arm type-checks its target
    /// through a *clone* (`check_expr(&mut *target.clone())`), so the real
    /// target node never gets annotated. Without this fallback the write-side
    /// rewrite cannot see that `m` is an `NPMutableArray`.
    ///
    /// Returns an owned type because the fallback is a fresh lookup.
    fn receiver_static_type(&self, object: &AstExpr) -> Option<AstType> {
        if let Some(t) = object.expr_type.as_deref() {
            return Some(t.clone());
        }
        match &object.data {
            AstExprData::VarRef { name, .. } => self.lookup_scope_var_type(name),
            _ => None,
        }
    }

    /// Rewrite `recv[i]` into `[recv objectAtIndex:i]` when the receiver is an
    /// object. Reuses the `MsgSend` node so the whole downstream chain (static
    /// vtable dispatch, nil-guard, SEL constants) needs no special case —
    /// the same shape the for-in desugar relies on. Returns the rewritten
    /// node's type (`id`) when a rewrite happened.
    fn maybe_rewrite_object_subscript(&mut self, e: &mut AstExpr) -> Option<AstType> {
        if !matches!(&e.data, AstExprData::Subscript { .. }) { return None; }
        // Judge the receiver's class *before* taking ownership of it.
        let cls = {
            let AstExprData::Subscript { object, .. } = &e.data else { return None };
            self.subscript_receiver_class(object)?
        };
        let (object, key) = match std::mem::replace(&mut e.data, AstExprData::Int(0)) {
            AstExprData::Subscript { object, key } => (object, key),
            _ => unreachable!("matched Subscript above"),
        };
        // Keyed containers (dictionaries) spell the read `objectForKey:`;
        // everything else keeps `objectAtIndex:`.
        let selector = if self.class_has_instance_method(&cls, "objectForKey:") {
            "objectForKey:"
        } else {
            "objectAtIndex:"
        };
        let mut id_ty = AstType::new(TypePrim::Id);
        id_ty.is_pointer = true;
        e.kind = AstExprKind::MsgSend;
        e.expr_type = Some(Box::new(id_ty.clone()));
        e.data = AstExprData::MsgSend {
            receiver: object,
            method: None,
            vtable_index: -1,
            is_class_method: false,
            is_super: false,
            super_name: None,
            selector: selector.to_string(),
            args: vec![*key],
        };
        Some(id_ty)
    }

    /// Rewrite `recv[i] = v` into `[recv setObject:v atIndex:i]` when the
    /// receiver is a mutable object. The read rewrite alone would emit
    /// `recv[i] = v` verbatim, which C rejects ("assigning to 'NPMutableArray'
    /// from incompatible type"). Jeti containers spell this
    /// `setObject:atIndex:`, so reuse that; the receiver must declare it,
    /// otherwise an immutable `NPArray` keeps the loud C error.
    fn maybe_rewrite_object_assign(&mut self, e: &mut AstExpr) -> Option<AstType> {
        if !matches!(&e.data, AstExprData::Assign { .. }) { return None; }
        let cls = {
            let AstExprData::Assign { target, .. } = &e.data else { return None };
            let is_sub = matches!(&target.data, AstExprData::Subscript { .. });
            if !is_sub { return None; }
            let AstExprData::Subscript { object, .. } = &target.data else { return None };
            // Must be an object that *also* offers a setter — this is what
            // separates NPMutableArray from an immutable NPArray.
            let t = self.receiver_static_type(object)?;
            if t.prim != TypePrim::Named || !t.is_pointer || t.is_array { return None; }
            let cls = Self::class_name_of(&t)?;
            // Indexed (setObject:atIndex:) or keyed (setObject:forKey:)
            // setter — the family decides the selector spelled below.
            if !self.class_has_instance_method(&cls, "setObject:atIndex:")
                && !self.class_has_instance_method(&cls, "setObject:forKey:")
            {
                return None;
            }
            cls
        };
        let (target, value) = match std::mem::replace(&mut e.data, AstExprData::Int(0)) {
            AstExprData::Assign { target, value } => (target, value),
            _ => unreachable!("matched Assign above"),
        };
        let inner = *target;
        let (object, key) = match inner.data {
            AstExprData::Subscript { object, key } => (object, key),
            _ => unreachable!("matched Subscript above"),
        };
        // Keyed containers spell the write `setObject:forKey:` (same
        // value-first arg order as `setObject:atIndex:`).
        let selector = if self.class_has_instance_method(&cls, "setObject:forKey:")
            && !self.class_has_instance_method(&cls, "setObject:atIndex:")
        {
            "setObject:forKey:"
        } else {
            "setObject:atIndex:"
        };
        e.kind = AstExprKind::MsgSend;
        e.data = AstExprData::MsgSend {
            receiver: object,
            method: None,
            vtable_index: -1,
            is_class_method: false,
            is_super: false,
            super_name: None,
            selector: selector.to_string(),
            args: vec![*value, *key],
        };
        // `setObject:atIndex:` returns void.
        let void_ty = AstType::new(TypePrim::Void);
        e.expr_type = Some(Box::new(void_ty.clone()));
        Some(void_ty)
    }

    /// A value struct type: `struct Tag` (or a typedef alias of one) used
    /// directly, NOT through a pointer. Pointers keep C's address-comparison
    /// semantics. Returns the TAG name (not the alias), because that is what
    /// the emitted `jeti_struct_eq_<Tag>` function is keyed on.
    /// True if the type is a C99 complex (`float _Complex` etc.).
    fn is_complex_type(t: &AstType) -> bool {
        t.is_complex
            || (t.prim == TypePrim::Named && t.name.as_deref().is_some_and(|n| {
                n == "_Complex" || n.ends_with(" _Complex")
            }))
    }

    /// True for real (non-complex, non-pointer) arithmetic types — the
    /// narrowing target side of a complex→real implicit conversion.
    fn is_real_scalar(t: &AstType) -> bool {
        !t.is_pointer && !Self::is_complex_type(t) && matches!(t.prim,
            TypePrim::Int | TypePrim::Long | TypePrim::LongLong | TypePrim::Short |
            TypePrim::Char | TypePrim::Float | TypePrim::Double | TypePrim::Bool |
            TypePrim::Signed | TypePrim::Unsigned)
    }

    fn is_value_struct(&self, t: &AstType) -> Option<String> {
        if t.is_pointer {
             return None;
        }
        let name = t.name.as_ref()?;
        if t.is_struct {
            return Some(name.clone());
        }
        // Typedef spelling: `typedef struct Point { ... } Point;` declared as
        // `Point a, b;` — the type node is a plain named type with no
        // `is_struct` flag, so consult the alias map collected in check().
        self.struct_alias_tags.get(name).cloned()
    }

    /// Rewrite `a == b` / `a != b` when both sides are the SAME value struct
    /// type into a call to the generated field-wise comparison function
    /// `jeti_struct_eq_<Tag>(a, b)` (`!=` becomes `(eq(a, b) == 0)`). C
    /// rejects `a == b` on structs outright ("invalid operands"), so without
    /// this rewrite every struct comparison is a hard clang error. The tags
    /// used are recorded in `struct_eq_tags`; codegen emits one comparison
    /// function per tag, on demand (static, no unused warnings).
    fn maybe_rewrite_struct_eq(&mut self, e: &mut AstExpr) {
        let (op, is_eq) = match &e.data {
            AstExprData::Binary { op: 12, .. } => (12, true),
            AstExprData::Binary { op: 13, .. } => (13, false),
            _ => return,
        };
        let (lt, rt) = match &e.data {
            AstExprData::Binary { left, right, .. } => {
                let lt = left.expr_type.as_deref().and_then(|t| self.is_value_struct(t));
                let rt = right.expr_type.as_deref().and_then(|t| self.is_value_struct(t));
                (lt, rt)
            }
            _ => return,
        };
        let (Some(ltag), Some(rtag)) = (lt, rt) else { return };
        if ltag != rtag {
            // Different struct types: let C report the mismatch — but note
            // clang would too, so staying silent here is safe.
            return;
        }
        let tag = ltag;
        let line = e.line;
        let col = e.col;
        let fn_name = format!("jeti_struct_eq_{}", tag);
        if !self.struct_eq_tags.contains(&tag) {
            self.struct_eq_tags.push(tag.clone());
        }
        // Extract the two operands (they are already type-checked).
        let (left, right) = match std::mem::replace(
            &mut e.data,
            AstExprData::Int(0),
        ) {
            AstExprData::Binary { left, right, .. } => (left, right),
            _ => unreachable!("matched Binary above"),
        };
        let call = AstExpr {
            kind: AstExprKind::FuncCall,
            expr_type: None,
            line,
            col,
            data: AstExprData::FuncCall {
                func: None,
                name: fn_name,
                callee: None,
                args: vec![*left, *right],
            },
            end_line: 0,
            end_col: 0,
        };
        let boxed = Box::new(call);
        e.data = if is_eq {
            // `a == b` → `jeti_struct_eq_X(a, b)`
            boxed.data
        } else {
            // `a != b` → `(jeti_struct_eq_X(a, b) == 0)`
            AstExprData::Binary {
                op: 12,
                left: boxed,
                right: Box::new(AstExpr {
                    kind: AstExprKind::Int,
                    expr_type: None,
                    line,
                    col,
                    data: AstExprData::Int(0),
                    end_line: 0,
                    end_col: 0,
                }),
            }
        };
        e.kind = if is_eq { AstExprKind::FuncCall } else { AstExprKind::Binary };
        e.expr_type = Some(Box::new(AstType::new(TypePrim::Bool)));
        let _ = op; // op is 13 when !is_eq; embedded in the rewritten == above
    }

    fn check_expr_inner(&mut self, e: &mut AstExpr) -> Option<AstType> {
        // An integer literal may arrive with a suffix-derived type (`7L` ->
        // long, annotated by the parser) — do not downgrade it to int.
        let int_pre_typed = match &e.data {
            AstExprData::Int(_) => e.expr_type.clone().filter(|t| {
                matches!(t.prim, TypePrim::Long | TypePrim::LongLong | TypePrim::Unsigned)
                    || t.is_unsigned
            }),
            _ => None,
        };
        match &mut e.data {
            AstExprData::Int(_) => Some(*int_pre_typed
                .unwrap_or_else(|| Box::new(AstType::new(TypePrim::Int)))),
            AstExprData::Float(_) => Some(AstType::new(TypePrim::Double)),
            // Imaginary literal (`2.0i`) — the static type carries the complex
            // flag so `check_decl` can flag implicit narrowing into a real
            // variable (clang: -Wcomplex-component-init). The *value* is only
            // the real component here; codegen re-emits the raw literal.
            AstExprData::FloatRaw(_) => {
                let mut t = AstType::new(TypePrim::Double);
                t.is_complex = true;
                Some(t)
            }
            AstExprData::String(_) => {
                let mut t = AstType::new(TypePrim::Char);
                t.is_pointer = true;
                Some(t)
            }
            AstExprData::AtString(_) => {
                let mut t = AstType::new(TypePrim::Id);
                t.is_pointer = true;
                Some(t)
            }
            AstExprData::Char(_) => Some(AstType::new(TypePrim::Char)),
            AstExprData::Bool(_) => Some(AstType::new(TypePrim::Bool)),
            AstExprData::VarRef { name, .. } => {
                // Real declarations win first: the -eh desugar declares its
                // `__jeti_eh_tmp_N` hoist temporaries in scope_vars (their
                // `__auto_type` init derives the true type). The `__` builtin
                // fallback below must not shadow them — it made the generic
                // argument check see `int` for an `NPMutableString *` temp
                // (0:0, "does not match the container's declared element
                // type"). Builtin macros (__FILE__/__LINE__) are never in
                // scope, so this reordering cannot change their result.
                for scope in self.scope_vars.iter().rev() {
                    for (vname, vtype) in scope.iter() {
                        if vname == name {
                            return Some(vtype.clone());
                        }
                    }
                }
                if name.starts_with("__") {
                    return Some(AstType::new(TypePrim::Int));
                }
                // Desugar-internal linker-level reference, e.g. the typed-catch
                // isa test's `&JETI_CLASS_$_Foo` emitted by the -eh checked
                // pass. Not expressible as a C identifier, so there is no
                // binding to resolve — same allowance as the `__` prefix above.
                if name.starts_with('&') {
                    return Some(AstType::new(TypePrim::Int));
                }
                for scope in self.scope_vars.iter().rev() {
                    for (vname, vtype) in scope.iter() {
                        if vname == name {
                            return Some(vtype.clone());
                        }
                    }
                }
                if name == "self" || name == "_cmd" || name == "super" || name == "nil" || name == "NULL" || name == "YES" || name == "NO" || name == "true" || name == "false" {
                    return Some(AstType::new(TypePrim::Int));
                }
                if let Some(ref st) = self.symtab {
                    if st.lookup(name).is_some() {
                        return Some(AstType::new(TypePrim::Int));
                    }
                }
                // A function name used as a value (function pointer, e.g. the
                // async state-machine entry passed to jeti_task_create).
                if self.function_params.contains_key(name) {
                    return Some(AstType::new(TypePrim::Int));
                }
                if let Some(ref cls_name) = self.current_class {
                    if let Some(ref st) = self.symtab {
                        if let Some(cls) = st.find_class(cls_name) {
                            if let SymbolData::Class { ref ivars, .. } = cls.data {
                                if ivars.contains(name) {
                                    return Some(AstType::new(TypePrim::Int));
                                }
                            }
                        }
                    }
                }
                // C passthrough headers declare globals and macro constants
                // (`errno`, `stdin`, `STDIN_FILENO`, `ECHO`…) the AST never
                // sees — same escape hatch as the callee whitelist, gated on
                // the headers this TU actually included.
                if self.c_stdlib_binds(name) {
                    return Some(AstType::new(TypePrim::Int));
                }
                // Generic instantiations (`Box<NPString *>`) and
                // namespace-qualified spellings (`A::B::List<T *>`) arrive
                // as ONE identifier whose base names a real class — possibly
                // through a `@using` fold. Strip the type-arg suffix, then
                // consult the table directly and via the using entry.
                if self.symtab_knows_spelling(name) {
                    return Some(AstType::new(TypePrim::Int));
                }
                self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!("use of undeclared identifier '{}'", name));
                None
            }
            AstExprData::MsgSend { receiver, args, selector, is_class_method, .. } => {
                self.check_expr(&mut *receiver);
                // nil-messaging: sending a message to a receiver that is
                // statically known to be `nil` is a safe no-op at runtime
                // (returns 0/nil, never crashes — codegen guards against nil),
                // but it is almost always a logic bug: the result will silently
                // be zero. Warn so the programmer knows the send is dead.
                if matches!(receiver.kind, AstExprKind::Nil) {
                    self.check_warning_over(e.line, e.col, e.end_line, e.end_col, &format!(
                        "message '{}' sent to nil receiver; result is always zero/nil", selector));
                }
                // Receiver kind vs method kind: a class singleton receives only
                // `+` class methods, an instance only `-` instance methods.  In
                // ObjC these are runtime "unrecognized selector" crashes; Jeti
                // (static) rejects them at compile time.
                //
                // Resolved along the receiver's OWN class chain. The old global
                // selector-keyed lookup mixed declarations from unrelated
                // classes (`NPRunLoop -run` + `Circle +run`), so a legal send
                // could be rejected because some distant class gave the shared
                // name the other kind. The `+`/`-` kind belongs to the class
                // that declares it, not to the selector name.
                let recv_kind_class = self.receiver_static_class(receiver, *is_class_method);
                let receiver_kinds: Option<Vec<bool>> = match &recv_kind_class {
                    // Class known but its chain never declares the selector:
                    // stay silent here — the "no visible method" check below
                    // owns that case.
                    Some(cn) => self.method_kinds_along_chain(cn, selector),
                    // `id` / anchor-class receiver: only the global table can
                    // say anything, and only when every declaration agrees.
                    None => self.method_kinds.get(selector).cloned(),
                };
                if let Some(kinds) = receiver_kinds {
                    let all_class = kinds.iter().all(|&k| k);
                    let any_class = kinds.iter().any(|&k| k);
                    if all_class != any_class && !all_class && *is_class_method {
                        self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                            "instance method '{}' cannot be called on a class name", selector));
                    } else if all_class != any_class && all_class && !*is_class_method {
                        let recv_is_instance = match &receiver.data {
                            AstExprData::VarRef { name, .. } => {
                                name != "self" && name != "_self" && self.is_local_var(name)
                            }
                            AstExprData::IvarRef { obj, .. } => {
                                !matches!(obj.data, AstExprData::MsgSend { .. })
                            }
                            _ => false,
                        };
                        if recv_is_instance {
                            self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                                "class method '{}' cannot be called on an instance", selector));
                        }
                    }
                }
                // `respondsToSelector:` is a compiler pseudo-method implemented
                // as a NULL-slot check on the uniform vtable, so the selector must
                // be a compile-time-known literal. It is lowered to a reference to
                // a `struct jeti_vtable` member, which only exists for a name that
                // appears in this TU — a runtime `SEL` variable has no member to
                // name. Reject it here so it never reaches codegen (which would
                // fall through to the arrow-access path and emit bad C).
                if selector == "respondsToSelector:" && args.len() == 1 {
                    if !matches!(args[0].data, AstExprData::Selector(_)) {
                        self.check_error_over(e.line, e.col, e.end_line, e.end_col,
                            "respondsToSelector: requires an @selector(...) literal argument");
                    }
                }
                // Check each arg's type against the method's declared parameter
                // types (collected from the AST in `collect_signatures`).
                // Reject bare C-string literals ("...") passed where an object
                // type is expected — only @"..." (AtString) is valid.
                // Receiver-class-aware signature lookup: the global
                // selector-keyed table is last-writer-wins, and two classes may
                // declare the same selector with different param types (tt.jeti's
                // `objectForKey:(const char *)` clobbered NPDictionary's
                // `objectForKey:(K)key`), turning valid sends into false
                // positives. Prefer the receiver's own class chain; fall back
                // to the global table only when the receiver's class is
                // statically unknown.
                let recv_class = receiver.expr_type.as_ref().and_then(|t| Self::named_class_of(t))
                    .or_else(|| match &receiver.data {
                        AstExprData::VarRef { name, .. } => {
                            self.lookup_scope_var_type(name).and_then(|t| Self::named_class_of(&t))
                        }
                        _ => None,
                    });
                // Static dispatch safety: a statically-typed receiver whose
                // class chain does NOT declare the selector would dispatch
                // through a NULL vtable slot — a segfault at runtime (ObjC
                // rejects this at compile time: "no visible @interface").
                // ERROR on user source: receiver resolution is reliable now
                // that method parameters live in a per-method scope frame
                // (the old cross-method leak made the first user of a
                // parameter name win everywhere, which is why this used to
                // be a warning). Foundation sources stay silent: reports
                // from that path were pure noise from our own inlining
                // quirks. `id` receivers never reach here (recv_class None).
                if let Some(ref cn) = recv_class {
                    if !*is_class_method {
                        if let Some(false) = self.class_declares_selector(cn, selector) {
                            // Two exemptions only: Foundation sources (their own
                            // inlining quirks are not user errors) and selectors
                            // that never dispatch through the receiver's vtable
                            // slot (`respondsToSelector:` plus the synthesized root
                            // methods). A selector declared on SOME OTHER class in
                            // this TU used to be exempt too, on the theory that the
                            // uniform vtable has a slot for the name — it does, but
                            // the receiver's own class initializes it to NULL, so
                            // the send segfaulted at runtime (probe: `Mute *m;
                            // [m speak]` with `speak` declared on Talker only).
                            // ObjC rejects the same shape at compile time.
                            // Two shape-based escapes remain besides Foundation:
                            // a subclass that declares the selector makes the
                            // send downcast-safe (see
                            // `a_subclass_declares_selector`), and `[obj ivar]`
                            // is C field access, not a send (see
                            // `selector_is_known_ivar`).
                            if !self.is_foundation_source(e.line)
                                && !Self::is_vtable_exempt_selector(selector)
                                && !self.selector_is_known_ivar(selector)
                                && !self.a_subclass_declares_selector(cn, selector)
                            {
                                self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                                    "no visible method '{}' on receiver type '{} *' — static dispatch would hit a NULL vtable slot; declare it on '{}' (or a superclass) or type the receiver as 'id'",
                                    selector, cn, cn));
                            }
                        }
                    }
                } else if !*is_class_method {
                    // No static class to walk: `id` / `instancetype` / a
                    // protocol-qualified receiver / an unresolved chain. The only
                    // thing provable here is the absolute absence of a
                    // declaration in this unit, which leaves codegen with no
                    // vtable member to name at all — it emitted `o->shrink` and
                    // clang rejected it with "no member named 'shrink' in 'struct
                    // NPObject'". Report that as a Jeti diagnostic instead of
                    // letting it surface as broken generated C.
                    if !self.is_foundation_source(e.line)
                        && !Self::is_vtable_exempt_selector(selector)
                        && !self.selector_is_known_ivar(selector)
                        && !self.selector_declared_anywhere(selector)
                    {
                        self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                            "no visible method '{}' — no class or protocol in this translation unit declares it, so there is no vtable slot to dispatch through; declare it in a shared header",
                            selector));
                    }
                }
                let param_types = recv_class
                    .as_deref()
                    .and_then(|cn| self.method_params_for_receiver(cn, selector))
                    .or_else(|| self.method_params.get(selector).cloned())
                    .unwrap_or_default();
                for a in args.iter_mut() { self.check_expr(a); }
                for (i, a) in args.iter().enumerate() {
                    if let Some(Some(ref pt)) = param_types.get(i) {
                        if Self::is_object_type(pt) && matches!(a.data, AstExprData::String(_)) {
                            self.check_warning_over(a.line, a.col, a.end_line, a.end_col,
                                "argument as a bare C string is not an object; use @\"...\" for an NPString");
                        }
                        // Reverse direction: an @"..." object literal passed where a
                        // plain C string (`const char *`/`char *`) is expected reads
                        // the NPString object header as character data — silent
                        // garbage at runtime (probe: stringWithUTF8String:@"bad").
                        // Enforced only when every declaration of this selector
                        // agrees on the char* param (method_kinds discipline) —
                        // mixed declarations stay silent.
                        if self.selector_param_all_cstr(selector, i) {
                            self.check_atstring_to_cstr(pt, a);
                        }
                        self.check_nullability_transfer(pt, a);
                    }
                }
                // Specialized generic container: the receiver's type carries
                // type_args (e.g. `NPMutableArray<NPString *>`). The write-side
                // selectors take the ELEMENT type T — a provably scalar arg
                // (int/float/char literal) is a type error the erased spelling
                // would silently pass through.
                let elem_check: Option<AstType> = receiver.expr_type.as_ref()
                    .map(|t| (**t).clone())
                    .or_else(|| match &receiver.data {
                        AstExprData::VarRef { name, .. } => self.lookup_scope_var_type(name),
                        _ => None,
                    }).and_then(|t| {
                    if t.type_args.is_empty() { None } else { t.type_args.first().cloned() }
                });
                if let Some(elem) = elem_check {
                    let elem_is_obj = elem.is_pointer || matches!(elem.prim, TypePrim::Id);
                    if elem_is_obj {
                        let write_sels = ["addObject:", "insertObject:atIndex:", "setObject:atIndex:"];
                        let first_arg_idx = if selector == "insertObject:atIndex:" || selector == "setObject:atIndex:" { 0 } else { usize::MAX };
                        if write_sels.contains(&selector.as_str()) {
                            let idx = if first_arg_idx == usize::MAX { 0 } else { first_arg_idx };
                            // Provably-not-T args: scalar literals AND boxed
                            // number literals (`@42` desugars to
                            // `[NPNumber numberWithInt:]` before the checker
                            // sees it, so it is a MsgSend — classify it here).
                            let scalar = args.get(idx).map(|a| match &a.data {
                                AstExprData::Int(_) | AstExprData::Float(_) | AstExprData::FloatRaw(_)
                                | AstExprData::Bool(_) | AstExprData::Char(_) | AstExprData::String(_) => true,
                                AstExprData::MsgSend { receiver, selector: sel, .. } => {
                                    matches!(&receiver.data, AstExprData::VarRef { name, .. } if name == "NPNumber")
                                        && (sel.starts_with("numberWithInt")
                                            || sel.starts_with("numberWithDouble")
                                            || sel.starts_with("numberWithLongLong")
                                            || sel.starts_with("numberWithBool"))
                                }
                                _ => false,
                            }).unwrap_or(false);
                            if scalar {
                                let elem_str = elem.name.clone().unwrap_or_else(|| "object".into());
                                self.check_error_over(args[idx].line, args[idx].col,
                                    args[idx].end_line, args[idx].end_col, &format!(
                                    "inserting a scalar into a typed container — its element type is '{}'; use an object of that type",
                                    elem_str));
                            }
                        }
                    }
                }
                // Generic signature substitution: a specialized receiver
                // (`NPArray<NPString *> *`) carries type_args on its static
                // type. Substitute them into the method signature at Param (T)
                // positions, then check each argument and the result type.
                // Bare spellings (`NPArray *`) have no type_args — full
                // erasure, zero migration.
                let recv_ty: Option<AstType> = match &receiver.data {
                    AstExprData::VarRef { name, .. } => {
                        // A local/param variable's declared type wins (same
                        // type the VarRef arm resolved). A class-name receiver
                        // is NOT in scope: parse `Base<Arg, ...>` from the name
                        // so a CLASS-method send on a specialized class name
                        // substitutes type_args into the signature exactly like
                        // an instance receiver does. Without this, a declared
                        // `+ (T)fetch` return stays raw `T` — object-kind
                        // returns erase to `id` (the ROADMAP generic
                        // class-method return gap; probe:
                        // tests/probe_generic_classmethod_ret.jeti).
                        self.lookup_scope_var_type(name)
                            .or_else(|| Self::parse_generic_receiver_name(name))
                            .or_else(|| receiver.expr_type.as_ref().map(|t| (**t).clone()))
                    }
                    _ => receiver.expr_type.as_ref().map(|t| (**t).clone()),
                };
                let recv_args: Vec<AstType> = recv_ty.as_ref()
                    .map(|t| t.type_args.clone())
                    .unwrap_or_default();
                // The class's declared type_params in order (`Pair<A, B>` →
                // ["A", "B"]). Param nodes carry their parameter name (the
                // parser keeps it), so multi-param generics match by name;
                // single-param classes fall back to args[0] when the table
                // is unavailable.
                let recv_class: Option<String> = recv_ty.as_ref()
                    .and_then(|t| t.name.clone())
                    .map(|n| n.split('<').next().unwrap_or(&n).to_string());
                let param_names: Vec<String> = recv_class
                    .and_then(|cn| {
                        let st = self.symtab.as_ref()?;
                        st.find_class(&cn).map(|c| match &c.data {
                            SymbolData::Class { type_params, .. } => type_params.clone(),
                            _ => Vec::new(),
                        })
                    })
                    .unwrap_or_default();
                let param_types_sub: Vec<Option<AstType>> = if recv_args.is_empty() {
                    param_types.clone()
                } else {
                    param_types.iter()
                        .map(|pt| pt.as_ref().map(|t| Self::substitute_type_args(t, &recv_args, &param_names)))
                        .collect()
                };
                // Only check when the signature actually has T (Param) slots:
                // `method_params` is keyed by selector globally, so a shared
                // selector may resolve to a non-generic class's signature
                // (e.g. `stringWithFormat:`) — type_args must not apply there.
                let sig_has_param = param_types.iter().any(|pt| {
                    pt.as_ref().map(|t| Self::type_has_param(t)).unwrap_or(false)
                });
                // Gate on recv_args: inside a generic class's own method
                // bodies `self` carries no type_args, so the raw `T` in the
                // signature must not be compared against arguments there.
                if !recv_args.is_empty() && sig_has_param {
                    for (i, a) in args.iter().enumerate() {
                        if let (Some(Some(ref pt)), Some(at)) = (param_types_sub.get(i), a.expr_type.as_ref()) {
                            if !self.arg_type_ok(pt, at) {
                                self.check_error_over(a.line, a.col, a.end_line, a.end_col, &format!(
                                    "argument of type '{}' does not match the container's declared element type '{}'",
                                    Self::type_display(at), Self::type_display(pt)));
                            }
                        }
                    }
                }
                // `%@` family: the format literal's %@ slots must receive objects.
                if Self::FORMAT_MSGSENDS.contains(&selector.as_str()) {
                    if let Some(AstExprData::AtString(fmt)) = args.first().map(|a| &a.data) {
                        self.check_format_args(fmt, &args[1..], e.line, e.col, selector);
                    }
                }
                // ARC mode: forbid manual retain/release/dealloc/autorelease
                // unless inside @noarc { } or implementing the runtime method itself.
                if !self.no_arc && !self.in_noarc {
                    let sel = selector.trim_end_matches(':');
                    if sel == "retain" || sel == "release" || sel == "autorelease" || sel == "dealloc" {
                        let impl_ok = self.current_method.as_deref().map(|m| m.trim_end_matches(':') == sel).unwrap_or(false);
                        if !impl_ok {
                            self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                                "explicit '{}' not allowed in ARC mode; wrap in @noarc {{ }} to manage manually", sel));
                        }
                    }
                }
                // Result type: substitute type_args into the declared return
                // type so `NPString *s = [m objectAtIndex:0]` sees `NPString *`
                // instead of erased `id`. Bare receivers fall back to the
                // declared return type (`count` → `size_t`) when known —
                // strictly more precise than the old blanket `id`.
                let result_ty = if recv_args.is_empty() {
                    // NPNumber factory family: unambiguous known returns (no
                    // subclass, no cross-class selector collision). Whitelisted
                    // so `@42` / `@(expr)` infer as `NPNumber *` — required for
                    // array-literal element inference (`@[ @1, @2 ]`).
                    const NPNUMBER_FACTORIES: [&str; 5] = [
                        "numberWithInt:", "numberWithLongLong:", "numberWithDouble:",
                        "numberWithBool:", "numberWithChar:",
                    ];
                    if NPNUMBER_FACTORIES.contains(&selector.as_str()) {
                        let mut t = AstType::new(TypePrim::Named);
                        t.name = Some("NPNumber".to_string());
                        t.class_ref = Some("NPNumber".to_string());
                        t.is_pointer = true;
                        return Some(t);
                    }
                    self.method_returns.get(selector)
                        .and_then(|rt| rt.as_ref())
                        .map(|rt| {
                            // `method_returns` is keyed by selector globally, so a
                            // shared selector resolves to whichever class declared
                            // it last (`-init` in NPMutableDictionary poisons every
                            // `[x init]`). Object-kind returns therefore stay `id`
                            // for bare receivers — only scalar returns (count →
                            // size_t) keep their declared precision.
                            let objectish = rt.is_pointer || matches!(rt.prim,
                                TypePrim::Id | TypePrim::Instancetype | TypePrim::Class
                                | TypePrim::Named | TypePrim::Param);
                            if objectish { AstType::new(TypePrim::Id) } else { rt.clone() }
                        })
                        .unwrap_or_else(|| AstType::new(TypePrim::Id))
                } else {
                    // copy/mutableCopy: the result is the receiver's own
                    // (substituted) element type, not erased `id`. `[m copy]`
                    // on NPMutableArray<NPString *> is NPArray-shaped with
                    // element NPString * — without this rule the result
                    // carries no type info and `int bad = [m copy]` slips
                    // through the scalar checks. Bare receivers (no
                    // type_args) keep the old `id` — zero migration.
                    if selector == "copy" || selector == "mutableCopy" {
                        if let Some(rt) = recv_ty.clone() {
                            return Some(rt);
                        }
                    }
                    self.method_returns.get(selector)
                        .and_then(|rt| rt.as_ref())
                        .map(|rt| Self::substitute_type_args(rt, &recv_args, &param_names))
                        .unwrap_or_else(|| AstType::new(TypePrim::Id))
                };
                Some(result_ty)
            }
            AstExprData::FuncCall { name, args, .. } => {
                if name == "NPLog" {
                    match args.first().map(|a| &a.data) {
                        Some(AstExprData::AtString(fmt)) => {
                            self.check_format_args(fmt, &args[1..], e.line, e.col, "NPLog");
                        }
                        Some(_) => {
                            self.check_warning_over(args[0].line, args[0].col, args[0].end_line, args[0].end_col, "NPLog first argument should be an NPString literal (@\"...\")");
                        }
                        None => {}
                    }
                }
                // General: warn about bare C-string literals passed to object-typed params.
                let param_types = self.function_params.get(name).cloned().unwrap_or_default();
                for a in args.iter_mut() { self.check_expr(a); }
                // `_Block_copy`/`_Block_release` take a block and nothing else,
                // but their ABI prototype (`const void *`) cannot say so — see
                // BLOCK_RUNTIME_FNS. Runs after the args are checked so their
                // types are recorded.
                if Self::BLOCK_RUNTIME_FNS.contains(&name.as_str()) {
                    if let Some(a) = args.first() {
                        if self.block_operand_is_provably_not_a_block(a) {
                            let msg = format!(
                                "argument to '{}' is not a block; only a block literal or \
                                 block-typed value can be passed — a non-block pointer is read \
                                 as a block header and dereferenced",
                                name);
                            self.check_error_over(a.line, a.col, a.end_line, a.end_col, &msg);
                        }
                    }
                }
                for (i, a) in args.iter().enumerate() {
                    if let Some(Some(ref pt)) = param_types.get(i) {
                        if Self::is_object_type(pt) && matches!(a.data, AstExprData::String(_)) {
                            self.check_warning_over(a.line, a.col, a.end_line, a.end_col,
                                "argument as a bare C string is not an object; use @\"...\" for an NPString");
                        }
                        // Same check as the MsgSend arm: @"..." object into a
                        // `char *` param is a silent-garbage bug.
                        self.check_atstring_to_cstr(pt, a);
                        // Nil-safe runtime primitives: their C contract is
                        // `if (!obj) return;`/`return NULL;` (runtime.c), so a
                        // possibly-nil argument is not a bug — it is the
                        // documented way to call them. A `nonnull` declaration
                        // of one is a lie the checker refuses to enforce;
                        // otherwise every ARC-injected scope-end
                        // `jeti_release` of a nullable local (or a manual
                        // release-before-nil idiom) would read as a
                        // nullable→nonnull violation. Covers both the ARC
                        // injection and hand-written calls.
                        if !Self::NIL_SAFE_RUNTIME_FNS.contains(&name.as_str()) {
                            self.check_nullability_transfer(pt, a);
                        }
                    }
                }
                // Block invocation: `sink(s)`. A block invocation is a plain
                // FuncCall in the AST (the elaborator lowers `Call` to
                // `FuncCall`), and `function_params` holds nothing for it — the
                // signature lives in the *variable's* block type. So read the
                // callee's declared type from scope and check its `block_params`.
                //
                // A typedef'd block (`typedef void (^Sink)(nonnull NPString *);`
                // — the form Foundation and essentially all real code uses) has
                // to go through `typedef_blocks` first: the variable's declared
                // type is just the alias name, with no `is_block` or params on
                // it, so without this lookup every typedef'd block silently
                // skipped the check.
                if let Some(mut bt) = self.lookup_scope_var_type(name) {
                    if !bt.is_block {
                        if let Some(alias) = bt.name.clone() {
                            if let Some(real) = self.typedef_blocks.get(&alias) {
                                bt = real.clone();
                            }
                        }
                    }
                    if bt.is_block {
                        if let Some(ref bp) = bt.block_params {
                            let mut cur = Some(&**bp);
                            let mut i = 0usize;
                            while let Some(p) = cur {
                                if let Some(a) = args.get(i) {
                                    self.check_nullability_transfer(p, a);
                                }
                                i += 1;
                                cur = p.next.as_deref();
                            }
                        }
                    }
                }
                Some(AstType::new(TypePrim::Int))
            }
            AstExprData::Binary { left, right, .. } => {
                self.check_expr(&mut *left);
                self.check_expr(&mut *right);
                // Arithmetic promotion: if either operand is floating point,
                // the result is double (C usual arithmetic conversions for
                // the int/float cases the checker can prove). Lets codegen's
                // block return-type inference see `x / 2.0f` as double.
                let left_float = left.expr_type.as_deref().map_or(false, |t| matches!(t.prim, TypePrim::Float | TypePrim::Double));
                let right_float = right.expr_type.as_deref().map_or(false, |t| matches!(t.prim, TypePrim::Float | TypePrim::Double));
                if left_float || right_float {
                    Some(AstType::new(TypePrim::Double))
                } else {
                    Some(AstType::new(TypePrim::Int))
                }
            }
            AstExprData::Unary { op, operand, .. } => {
                let t = self.check_expr(&mut *operand.clone());
                // `&` yields a pointer to the operand's type (C 6.5.3.2).
                // The bare-operand type previously flowed through, so
                // `(void *)&fsi` was inferred as casting `int` — a checker
                // false-warning source (C cast diagnostics §G-expr).
                if let Some(mut pt) = t.clone() {
                    if *op == 4 && !pt.is_pointer && !pt.is_block && !pt.is_array
                        && !matches!(pt.prim, TypePrim::Id | TypePrim::Class | TypePrim::Sel)
                        && Self::is_integral_type(&pt)
                    {
                        pt.is_pointer = true;
                        return Some(pt);
                    }
                }
                t
            }
            AstExprData::Assign { target: _, value: _ } => {
                // Target/value checking happens after `check_expr_inner`
                // (the assignment-family hook evaluates clones of both for
                // their static types); doing it here as well made every
                // target diagnostic record twice.
                None
            }
            AstExprData::Cast { target_type, .. } => {
                Some(target_type.clone())
            }
AstExprData::Subscript { object, key, .. } => {
                self.check_expr(&mut *object);
                self.check_expr(&mut *key);
                Some(AstType::new(TypePrim::Int))
            }
            AstExprData::Ternary { then, .. } => {
                // Deliberately types from `then` only: the checker's type
                // inference is too loose to gate ternary branches (MsgSends
                // fall back to `id`, ivar reads to `Int`), so a hard
                // branch-compat check here rejected LEGAL C like
                // `s ? [s utf8String] : ""` and `_cstr ? _cstr : ""`.
                // Truly illegal mixes (NPTask* : int) are hard errors in C
                // itself, so the generated-C compile is the backstop.
                self.check_expr(&mut *then.clone())
            }
            AstExprData::Comma(exprs) => {
                exprs.last().and_then(|e| {
                    let mut e = e.clone();
                    self.check_expr(&mut e)
                })
            }
            AstExprData::Paren(inner) => {
                self.check_expr(&mut *inner.clone())
            }
            AstExprData::IvarRef { obj, ivar, .. } => {
                self.check_expr(&mut *obj);
                // Instance variables cannot be used in class methods (`+`): `self`
                // there is the class object (NPClass *), not an instance. Mirrors
                // ObjC's "instance variable '_x' accessed in class method".
                // EXCEPT when `self` is shadowed by a local variable/parameter —
                // e.g. the factory pattern `+ (W *)withTitle:... { W *self =
                // [[W alloc] init]; self->_handle = ...; }` where the ivar
                // access goes through the local instance pointer, not the class.
                let self_shadowed = matches!(&obj.data, AstExprData::VarRef { name, .. } if name == "self")
                    && self.shadowed_locals.iter().any(|n| n == "self");
                if self.current_method_is_class
                    && !self_shadowed
                    && matches!(&obj.data, AstExprData::VarRef { name, .. } if name == "self")
                {
                    let ivn = ivar.clone().unwrap_or_else(|| "?".to_string());
                    self.check_error_over(e.line, e.col, e.end_line, e.end_col, &format!(
                        "instance variable '{}' accessed in class method (self is the class, not an instance)", ivn));
                }
                Some(AstType::new(TypePrim::Int))
            }
            AstExprData::PropRef { obj, .. } => {
                self.check_expr(&mut *obj);
                Some(AstType::new(TypePrim::Int))
            }
            AstExprData::Selector(_) => {
                Some(AstType::new(TypePrim::Sel))
            }
            AstExprData::ArrayLit(items) => {
                // Elements must be visited: they need `expr_type` for codegen,
                // and the type-directed rewrites (`@(expr)` boxing, object
                // subscripts) only fire from `check_expr`. Skipping them left
                // `@[ @(i + 1) ]` emitting a bare `(i + 1)` int in an object
                // array literal — type-correct jeti, garbage at runtime.
                for item in items.iter_mut() {
                    self.check_expr(item);
                }
                // Element-type inference (ObjC-style, user-decided): when
                // every element has the same concrete named class type, the
                // literal infers `NPArray<X>` so assignment to a typed
                // container checks. Mixed elements or non-class types fall
                // back to the bare erased `NPArray` — zero change for
                // existing code.
                let mut inferred: Option<AstType> = None;
                let mut all_same = !items.is_empty();
                for item in items.iter() {
                    let Some(et) = item.expr_type.as_deref() else {
                        all_same = false; break;
                    };
                    let is_class = et.prim == TypePrim::Named && et.is_pointer
                        && et.name.is_some() && !et.name.as_deref().unwrap_or("").contains('<');
                    if !is_class { all_same = false; break; }
                    match &inferred {
                        None => inferred = Some(et.clone()),
                        Some(prev) => {
                            if prev.name != et.name { all_same = false; break; }
                        }
                    }
                }
                if all_same {
                    if let Some(elem) = inferred {
                        let mut t = AstType::new(TypePrim::Named);
                        t.name = Some("NPArray".to_string());
                        t.class_ref = Some("NPArray".to_string());
                        t.is_pointer = true;
                        t.type_args = vec![elem];
                        return Some(t);
                    }
                }
                Some(AstType::new(TypePrim::Id))
            }
            AstExprData::InitList(items) => {
                if let Some(first) = items.first() {
                    let mut e = first.clone();
                    self.check_expr(&mut e)
                } else {
                    Some(AstType::new(TypePrim::Int))
                }
            }
            AstExprData::DesignatedInit { expr, .. } => {
                // A designated entry `.field = v` / `[i] = v` has the type of
                // its value expression (the enclosing struct/array type is the
                // declaration's, not the entry's).
                let mut e = (**expr).clone();
                self.check_expr(&mut e)
            }
            AstExprData::DictLit { keys, values } => {
                // Same reasoning as ArrayLit above: the entries must be visited
                // so nested `@(expr)` boxing and object subscripts fire, and so
                // a bare scalar entry is caught here rather than leaked into
                // the generated C as a non-object argument to
                // `jeti_dictionary_create` (which would compile and then
                // misbehave at runtime). ObjC rejects these too: "collection
                // element of type 'int' is not an Objective-C object".
                for entry in keys.iter_mut().chain(values.iter_mut()) {
                    if let Some(t) = self.check_expr(entry) {
                        if !Self::is_object_type(&t) {
                            let tstr = Self::type_display(&t);
                            self.check_error_over(entry.line, entry.col, entry.end_line, entry.end_col, &format!(
                                "illegal type '{}' in a dictionary literal — keys and values must be Objective-C objects",
                                tstr));
                        }
                    }
                }
                Some(AstType::new(TypePrim::Id))
            }
            AstExprData::Block { params, body, .. } => {
                // Annotate `return e;` expressions inside the block body so
                // codegen can infer the block literal's return type. Minimal
                // walk: only Return expressions are checked (no full statement
                // checking — a full walk would re-check nested decls twice).
                // Block params ARE pushed into scope_vars so `return b;` where
                // `b` is a block param resolves to the param's declared type
                // instead of the unknown-VarRef fallback.
                fn annotate_returns(c: &mut Checker, s: &mut AstStmt) {
                    match &mut s.data {
                        AstStmtData::Return(Some(e)) => { c.check_expr(e); }
                        AstStmtData::Expr(e) => {
                            if matches!(e.data, AstExprData::Block { .. }) { c.check_expr(e); }
                        }
                        AstStmtData::Compound(stmts) => {
                            for st in stmts { annotate_returns(c, st); }
                        }
                        AstStmtData::If { then, else_, .. } => {
                            annotate_returns(c, then);
                            if let Some(el) = else_ { annotate_returns(c, el); }
                        }
                        AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
                        | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. }
                        | AstStmtData::Synchronized { body, .. } | AstStmtData::NoArc(body)
                        | AstStmtData::Autoreleasepool(body) | AstStmtData::Default(body) => {
                            annotate_returns(c, body)
                        }
                        AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. } => {
                            annotate_returns(c, body)
                        }
                        AstStmtData::Try { try_block, catches, finally_block } => {
                            annotate_returns(c, try_block);
                            for ct in catches {
                                if let AstStmtData::Catch { body, .. } = &mut ct.data {
                                    annotate_returns(c, body);
                                }
                            }
                            if let Some(f) = finally_block { annotate_returns(c, f); }
                        }
                        _ => {}
                    }
                }
                if let Some(b) = body {
                    // Bind block params into a fresh scope so `return b;`
                    // resolves the param's declared type (not a fallback).
                    // Block expr params are `Vec<(AstType, String)>`.
                    self.scope_vars.push(Vec::new());
                    let bound: Vec<(String, AstType)> = params.iter()
                        .filter(|(_, n)| !n.is_empty())
                        .map(|(t, n)| (n.clone(), t.clone()))
                        .collect();
                    self.scope_vars.last_mut().unwrap().extend(bound);
                    annotate_returns(self, b);
                    self.scope_vars.pop();
                }
                Some(AstType::new(TypePrim::Id))
            }
            // `({ ... })` statement expression: the body statements are
            // checked in a fresh scope (its declarations are local to the
            // expression); the value is the last expression statement's type.
            AstExprData::StmtExpr(body) => {
                self.scope_vars.push(Vec::new());
                let mut last: Option<AstType> = None;
                for s in body.iter_mut() {
                    if let AstStmtData::Expr(e) = &mut s.data {
                        last = self.check_expr(e);
                    } else {
                        self.check_stmt(s);
                    }
                }
                self.scope_vars.pop();
                last
            }
            AstExprData::Sizeof { .. } => {
                let mut t = AstType::new(TypePrim::Long);
                t.is_pointer = false;
                Some(t)
            }
            AstExprData::Alignof(_) => {
                let mut t = AstType::new(TypePrim::Long);
                t.is_pointer = false;
                Some(t)
            }
            AstExprData::TypeLiteral(_) => Some(AstType::new(TypePrim::Int)),
            // `await e` has the type of `e` (suspension does not change the
            // awaited value's type). Whether the enclosing method may suspend
            // is decided by the async analysis pass, not here.
            AstExprData::Await { inner, .. } => {
                let inner_ty = self.check_expr(&mut *inner.clone());
                // await unwraps the task box: `await` on `NPTask<T> *`
                // yields `T` (docs/plans/async_nptask_plan.md). Await on a non-task
                // expression keeps the expression's own type (async-call
                // sugar: the call's return type IS the NPTask handle... so
                // through-sugar awaits also land here and unwrap).
                if let Some(t) = &inner_ty {
                    if t.is_pointer && t.name.as_deref() == Some("NPTask") {
                        if let Some(arg) = t.type_args.first() {
                            // `int`-style args unwrap to the value type; the
                            // arg's own pointer-ness is preserved by the clone.
                            return Some(arg.clone());
                        }
                        // NPTask<void> / bare NPTask: await yields void.
                        return Some(AstType::new(TypePrim::Void));
                    }
                }
                inner_ty
            }
            // `@(expr)` — the factory call is chosen from the operand's static
            // type by `maybe_rewrite_boxed_expr`, which `check_expr` runs right
            // after this returns. Type the node as the operand's type here so
            // that rewrite can read it.
            AstExprData::Boxed(inner) => self.check_expr(inner),
        }
    }

    /// Check a statement
    pub fn check_stmt(&mut self, s: &mut AstStmt) {
        // Span scalars captured up front so family checks can be invoked
        // from inside `match &mut s.data` without double-borrowing the
        // statement (see check_switch_family).
        let (s_line, s_col, s_end_line, s_end_col) = (s.line, s.col, s.end_line, s.end_col);
        match &mut s.data {
            AstStmtData::Expr(e) => { self.check_expr(e); }
            AstStmtData::Compound(stmts) => {
                self.scope_vars.push(Vec::new());
                let mut i = 0;
                while i < stmts.len() {
                    self.check_stmt(&mut stmts[i]);
                    i += 1;
                    // Early-exit narrowing: `if (!s) { return; }` proves `s`
                    // non-null for the REST of this block — the single most
                    // common guard shape in ObjC code, so leaving it out makes
                    // the diagnostic fire on correct code. Only the no-`else`
                    // form is handled, and only when the `then` arm cannot fall
                    // through: then the sole way past the `if` is the else path.
                    // (`if (s) { return; }` proves the opposite — s is nil — so
                    // it contributes nothing.)
                    let carry = match &stmts[i - 1].data {
                        AstStmtData::If { cond, then, else_ } if else_.is_none() => {
                            let (_, else_narrow) = Self::narrow_from_cond(cond);
                            if !else_narrow.is_empty() && Self::exits_unconditionally(then) {
                                Some(else_narrow)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(names) = carry {
                        self.narrowed.push(names);
                        for rest in &mut stmts[i..] { self.check_stmt(rest); }
                        self.narrowed.pop();
                        break;
                    }
                }
                self.scope_vars.pop();
            }
            AstStmtData::Return(expr) => {
                if let Some(e) = expr { self.check_expr(&mut *e); }
            }
            AstStmtData::If { cond, then, else_ } => {
                self.check_expr(&mut *cond);
                // Flow-sensitive narrowing: the condition proves the name
                // non-null in one arm and (for a bare `if (x)`) in the other.
                // Scope-stack push/pop so it cannot escape the branch.
                let (then_narrow, else_narrow) = Self::narrow_from_cond(cond);
                if !then_narrow.is_empty() {
                    self.narrowed.push(then_narrow);
                    self.check_stmt(&mut *then);
                    self.narrowed.pop();
                } else {
                    self.check_stmt(&mut *then);
                }
                if let Some(ref mut els) = else_ {
                    if !else_narrow.is_empty() {
                        self.narrowed.push(else_narrow);
                        self.check_stmt(&mut *els);
                        self.narrowed.pop();
                    } else {
                        self.check_stmt(&mut *els);
                    }
                }
            }
            AstStmtData::While { cond, body } => {
                self.check_expr(&mut *cond);
                self.check_stmt(&mut *body);
            }
            AstStmtData::Do { body, cond } => {
                self.check_stmt(&mut *body);
                self.check_expr(&mut *cond);
            }
            AstStmtData::For { init, cond, incr, body } => {
                if let Some(ref mut i) = init { self.check_stmt(&mut *i); }
                if let Some(ref mut c) = cond { self.check_expr(&mut *c); }
                if let Some(ref mut i) = incr { self.check_expr(&mut *i); }
                self.check_stmt(&mut *body);
            }
            AstStmtData::ForIn { var, collection, body } => {
                // The loop variable (`for (id item in arr)`) is a valid
                // binding inside the body but is never a *declaration* the
                // checker would meet — binder no longer registers mention
                // identifiers (undeclared check honesty), so this is its
                // one scope entry. `None`-typed spellings bind as `id`
                // (objects are the for-each domain; scalars keep their
                // declared type through the var expr).
                let forin_var_ty = match &var.data {
                    AstExprData::VarRef { name, .. } => {
                        let mut t = AstType::new(TypePrim::Id);
                        t.is_pointer = true;
                        if let Some(scope) = self.scope_vars.last_mut() {
                            if scope.iter().all(|(n, _)| n != name) {
                                scope.push((name.clone(), t.clone()));
                            }
                        }
                        Some(t)
                    }
                    AstExprData::Cast { target_type, expr } => {
                        if let AstExprData::VarRef { name, .. } = &expr.data {
                            let mut t = target_type.clone();
                            t.is_pointer = t.is_pointer || Self::is_pointer_like(&t);
                            if let Some(scope) = self.scope_vars.last_mut() {
                                if scope.iter().all(|(n, _)| n != name) {
                                    scope.push((name.clone(), t.clone()));
                                }
                            }
                            Some(t)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                let _ = forin_var_ty;
                self.check_expr(&mut *var);
                self.check_expr(&mut *collection);
                self.check_stmt(&mut *body);
            }
            AstStmtData::Switch { expr, body } => {
                self.check_expr(&mut *expr);
                self.check_stmt(&mut *body);
                // C17 6.8.4.2 switch family (subject integrality, duplicate
                // literal cases, multiple defaults) — BEFORE the enum
                // exhaustiveness check, which assumes the same walk shape.
                self.check_switch_family(s_line, s_col, s_end_line, s_end_col, expr, body);
                self.check_switch_exhaustiveness(expr, body);
            }
            AstStmtData::Case { value, body } => {
                self.check_expr(&mut *value);
                self.check_stmt(&mut *body);
            }
            AstStmtData::Default(body) => { self.check_stmt(&mut *body); }
            AstStmtData::Throw(expr) => {
                let ty = if let Some(e) = expr.as_mut() {
                    self.check_expr(e);
                    Self::throw_expr_type(self, &**e)
                } else {
                    None
                };
                if self.try_depth == 0 {
                    self.uncaught_throws.push((s.line, s.col));
                }
                self.thrown.push((s.line, s.col, ty));
            }
            AstStmtData::Try { try_block, catches, finally_block } => {
                // A local `@catch` handles whatever its try block throws, so
                // those throws never reach the caller.
                let has_catch = !catches.is_empty();
                if has_catch { self.try_depth += 1; }
                self.check_stmt(&mut *try_block);
                if has_catch { self.try_depth -= 1; }
                for c in catches { self.check_stmt(c); }
                if let Some(ref mut f) = finally_block { self.check_stmt(&mut *f); }
            }
            AstStmtData::Catch { body, .. } => { self.check_stmt(&mut *body); }
            AstStmtData::Finally(body) => { self.check_stmt(&mut *body); }
            AstStmtData::Synchronized { lock, body } => {
                self.check_expr(&mut *lock);
                // M1 known limitation: a @throw inside the block longjmps past
                // the scope, so the generated cleanup(jeti_syncAutoCleanup)
                // never runs and the lock stays held. sjlj semantics cannot
                // fix this (same class as the documented cross-function-throw
                // release-skipping limitation) — warn instead of staying
                // silent. -eh checked rewrites throws into flag+return, which
                // DOES run the cleanup, so checked mode has no such hazard.
                // (Checked is the DEFAULT backend — only warn under -eh sjlj.)
                if !self.eh_checked && Self::stmt_has_throw(body) {
                    self.check_warning_over(lock.line, lock.col, lock.end_line, lock.end_col,
                        "@throw inside a '@synchronized' block skips its unlock (setjmp/longjmp bypasses the cleanup attribute) — handle it with a local '@try' or restructure");
                }
                self.check_stmt(&mut *body);
            }
            AstStmtData::Autoreleasepool(body) => { self.check_stmt(&mut *body); }
            AstStmtData::NoArc(body) => {
                let old = self.in_noarc;
                self.in_noarc = true;
                self.check_stmt(&mut *body);
                self.in_noarc = old;
            }
            AstStmtData::Decl(d) => { self.check_decl(d); }
            _ => {}
        }
    }

    /// True when the statement tree contains a `@throw` at any nesting depth
    /// (including inside block literals, whose bodies compile to separate
    /// functions but still longjmp through the enclosing frame's jmp_buf).
    fn stmt_has_throw(s: &AstStmt) -> bool {
        match &s.data {
            AstStmtData::Throw(_) => true,
            AstStmtData::Compound(v) => v.iter().any(Self::stmt_has_throw),
            AstStmtData::If { then, else_, .. } => {
                Self::stmt_has_throw(then) || else_.as_deref().is_some_and(Self::stmt_has_throw)
            }
            AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
            | AstStmtData::For { body, .. } | AstStmtData::ForIn { body, .. }
            | AstStmtData::Switch { body, .. } | AstStmtData::Case { body, .. }
            | AstStmtData::Default(body) | AstStmtData::Synchronized { body, .. }
            | AstStmtData::NoArc(body) | AstStmtData::Autoreleasepool(body)
            | AstStmtData::Defer(body) => Self::stmt_has_throw(body),
            AstStmtData::Try { try_block, catches, finally_block } => {
                Self::stmt_has_throw(try_block)
                    || catches.iter().any(Self::stmt_has_throw)
                    || finally_block.as_ref().is_some_and(|f| Self::stmt_has_throw(f))
            }
            AstStmtData::Catch { body, .. } | AstStmtData::Finally(body) => Self::stmt_has_throw(body),
            AstStmtData::Expr(e) => Self::expr_has_throw(e),
            AstStmtData::Decl(d) => Self::decl_has_throw(d),
            AstStmtData::Return(Some(e)) => Self::expr_has_throw(e),
            _ => false,
        }
    }

    fn expr_has_throw(e: &AstExpr) -> bool {
        match &e.data {
            AstExprData::Block { body, .. } => {
                body.as_deref().is_some_and(Self::stmt_has_throw)
            }
            AstExprData::Paren(inner) | AstExprData::Cast { expr: inner, .. }
            | AstExprData::Await { inner, .. } | AstExprData::Boxed(inner) => Self::expr_has_throw(inner),
            AstExprData::Unary { operand: inner, .. } => Self::expr_has_throw(inner),
            AstExprData::Binary { left, right, .. } => {
                Self::expr_has_throw(left) || Self::expr_has_throw(right)
            }
            AstExprData::Assign { target, value } => {
                Self::expr_has_throw(target) || Self::expr_has_throw(value)
            }
            AstExprData::Ternary { cond, then, else_ } => {
                Self::expr_has_throw(cond) || Self::expr_has_throw(then) || Self::expr_has_throw(else_)
            }
            AstExprData::Comma(v) => v.iter().any(Self::expr_has_throw),
            AstExprData::FuncCall { callee, args, .. } => {
                callee.as_ref().is_some_and(|c| Self::expr_has_throw(c))
                    || args.iter().any(Self::expr_has_throw)
            }
            AstExprData::MsgSend { receiver, args, .. } => {
                Self::expr_has_throw(receiver) || args.iter().any(Self::expr_has_throw)
            }
            AstExprData::ArrayLit(v) | AstExprData::InitList(v) => v.iter().any(Self::expr_has_throw),
            AstExprData::DictLit { keys, values } => {
                keys.iter().any(Self::expr_has_throw) || values.iter().any(Self::expr_has_throw)
            }
            _ => false,
        }
    }

    fn decl_has_throw(d: &AstDecl) -> bool {
        match &d.data {
            AstDeclData::Variable { init, next, .. } => {
                init.as_deref().is_some_and(Self::expr_has_throw)
                    || next.as_deref().is_some_and(Self::decl_has_throw)
            }
            _ => false,
        }
    }

    /// Check a declaration
    /// Recursively collect all local variable / parameter names declared in a
    /// statement tree (used to detect a local `self` shadowing the class-method
    /// self). Params are added by the caller.
    fn collect_decl_names_stmt(s: &AstStmt, out: &mut Vec<String>) {
        match &s.data {
            AstStmtData::Decl(d) => {
                let mut cur: Option<&AstDecl> = Some(d);
                while let Some(decl) = cur {
                    if let Some(n) = &decl.name {
                        if !out.contains(n) { out.push(n.clone()); }
                    }
                    if let AstDeclData::Variable { next, .. } = &decl.data {
                        cur = next.as_ref().map(|b| &**b);
                    } else { cur = None; }
                }
            }
            // The elaborator lowers some declarations (notably `W *self = ...`
            // in class methods) to bare assignment statements. Collect the
            // assignment target too so a shadowing local `self` is still seen.
            AstStmtData::Expr(ex) => {
                if let AstExprData::Assign { target, .. } = &ex.data {
                    if let AstExprData::VarRef { name, .. } = &target.data {
                        if !out.contains(name) { out.push(name.clone()); }
                    }
                }
            }
            AstStmtData::Compound(stmts) =>
                for st in stmts { Self::collect_decl_names_stmt(st, out); },
            AstStmtData::If { then, else_, .. } => {
                Self::collect_decl_names_stmt(then, out);
                if let Some(el) = else_ { Self::collect_decl_names_stmt(el, out); }
            }
            AstStmtData::While { body, .. } | AstStmtData::Do { body, .. }
            | AstStmtData::Autoreleasepool(body) | AstStmtData::NoArc(body)
            | AstStmtData::Synchronized { body, .. } =>
                Self::collect_decl_names_stmt(body, out),
            AstStmtData::For { init, body, .. } => {
                if let Some(i) = init { Self::collect_decl_names_stmt(i, out); }
                Self::collect_decl_names_stmt(body, out);
            }
            AstStmtData::ForIn { body, .. } => {
                // `var` is a plain expression, not a declaration — nothing to collect.
                Self::collect_decl_names_stmt(body, out);
            }
            AstStmtData::Try { try_block, catches, finally_block } => {
                Self::collect_decl_names_stmt(try_block, out);
                for c in catches { Self::collect_decl_names_stmt(c, out); }
                if let Some(f) = finally_block { Self::collect_decl_names_stmt(f, out); }
            }
            AstStmtData::Catch { body, .. } | AstStmtData::Finally(body) =>
                Self::collect_decl_names_stmt(body, out),
            AstStmtData::Switch { body, .. } => Self::collect_decl_names_stmt(body, out),
            _ => {}
        }
    }

    fn add_params_to_scope(&mut self, params: &Option<Box<CstParam>>) {
        if self.scope_vars.is_empty() {
            self.scope_vars.push(Vec::new());
        }
        let mut p = params.as_ref().map(|b| &**b);
        while let Some(param) = p {
            // Function-pointer parameters spell their declarator name inside
            // the type (`int (*cb)(int)` → par_type.block_name), not on the
            // parameter node — both spellings declare `cb`.
            let pname = param.name.clone().or_else(|| {
                param.par_type.as_ref().and_then(|t| {
                    if t.is_fn_ptr { t.block_name.clone() } else { None }
                })
            });
            if let Some(ref name) = pname {
                if let Some(scope) = self.scope_vars.last_mut() {
                    if !scope.iter().any(|(n, _)| n == name) {
                        let t = param.par_type.as_ref()
                            .map(|ct| Self::cst_type_to_ast_type(ct))
                            .unwrap_or_else(|| AstType::new(TypePrim::Int));
                        scope.push((name.clone(), t));
                    }
                }
            }
            p = param.next.as_ref().map(|n| &**n);
        }
    }

    pub fn check_decl(&mut self, d: &mut AstDecl) {
        match &mut d.data {
            AstDeclData::Function { body, params, throws, .. } => {
                if let Some(ref mut b) = body {
                    let old = self.current_method.clone();
                    let old_ann = std::mem::replace(&mut self.throws_ann, Self::throws_state(throws.as_deref()));
                    let old_thrown = std::mem::take(&mut self.thrown);
                    let old_uncaught = std::mem::take(&mut self.uncaught_throws);
                    let old_depth = std::mem::replace(&mut self.try_depth, 0);
                    self.current_method = d.name.clone();
                    // Fresh frame for this function's params, popped right
                    // after the body: nothing leaks into the next declaration.
                    self.scope_vars.push(Vec::new());
                    self.add_params_to_scope(params);
                    self.check_stmt(b);
                    self.scope_vars.pop();
                    self.reconcile_throws(d.name.as_deref(), d.line, d.col);
                    self.try_depth = old_depth;
                    self.uncaught_throws = old_uncaught;
                    self.thrown = old_thrown;
                    self.throws_ann = old_ann;
                    self.current_method = old;
                }
            }
            AstDeclData::Variable { var_type, init, next, is_const, .. } => {
                let head_type = var_type.as_ref().map(|b| (**b).clone());
                // Leading-qualifier const (`const int z`) lives on the DECL,
                // not the type node — fold it into the scoped type below so
                // the assignment family's const-target check sees what C sees.
                let decl_is_const = *is_const;
                // Type arguments on a class that declares none are silently
                // erased (no monomorphization, no element type checking).
                if let Some(ref t) = head_type {
                    self.warn_if_erased_generics(t, d.line, d.col);
                    self.check_generic_bounds_instantiation(t, d.line, d.col);
                }
                if let Some(ref name) = d.name {
                    // Reserved compiler/runtime names the -eh desugar assigns
                    // but never declares (the _N-suffixed temporaries ARE
                    // desugar-declared and stay legal). A user re-declaration
                    // would collide with the runtime global at link time.
                    if matches!(name.as_str(),
                        "__jeti_eh_flag" | "__jeti_eh_val" | "__jeti_eh_isa"
                        | "__jeti_exception_value" | "__jeti_exception_buf")
                    {
                        self.check_error_over(d.line, d.col, d.end_line, d.end_col, &format!(
                            "'{}' is reserved for the exception runtime (-eh); use a different name",
                            name));
                    }
                    if let Some(scope) = self.scope_vars.last_mut() {
                        if !scope.iter().any(|(n, _)| n == name) {
                            let mut t = head_type.clone().unwrap_or_else(|| AstType::new(TypePrim::Int));
                            // Leading-qualifier const (`const int z`) lives on
                            // the DECL, not the type node. C17 6.7.2 reading:
                            // with no pointer it consts the variable itself;
                            // WITH a pointer (`const char *p`) it consts the
                            // POINTEE and the variable stays assignable —
                            // clang errors on `p = x` only for `char *const`
                            // (probe_assign ①), never for `const char *p`.
                            if decl_is_const && !t.is_pointer {
                                t.is_const = true;
                            }
                            scope.push((name.clone(), t));
                        }
                    }
                }
                if let Some(ref mut i) = init {
                    let init_ty = self.check_expr(i);
                    // `__auto_type` (eh desugar's hoisted expression temp,
                    // `__jeti_eh_tmp_N`): GNU semantics — infer the declared
                    // type from the initializer. An init the checker cannot
                    // type falls back to `id` (universal object), never the
                    // scalar-ish marker; mismatch checks are superseded.
                    let is_auto = head_type.as_ref().map_or(false, |t| {
                        t.prim == TypePrim::Named && t.name.as_deref() == Some("__auto_type")
                    });
                    if is_auto {
                        let derived = init_ty
                            .clone()
                            .unwrap_or_else(|| AstType::new(TypePrim::Id));
                        if let (Some(scope), Some(ref name)) =
                            (self.scope_vars.last_mut(), d.name.as_ref())
                        {
                            for (n, t) in scope.iter_mut() {
                                if n == name.as_str() {
                                    *t = derived;
                                    break;
                                }
                            }
                        }
                    } else if let (Some(vt), Some(it)) = (var_type.as_deref(), init_ty.as_ref()) {
                        let declared_complex = Self::is_complex_type(vt);
                        if !declared_complex && it.is_complex && Self::is_real_scalar(vt) {
                            self.check_warning_over(d.line, d.col, d.end_line, d.end_col, &format!(
                                "initialization of real type from complex value discards the imaginary part ('{}' = '{}')",
                                Self::type_desc(vt), Self::type_desc(it)));
                        }
                        // Pointer/object vs scalar mismatch: `int v = [m
                        // objectAtIndex:0]` used to slip through because both
                        // sides were "unknown" before generic substitution.
                        // Now the substituted return type is concrete.
                        if let Some(m) = self.init_ptr_scalar_mismatch(vt, it, i) {
                            self.check_error_over(d.line, d.col, d.end_line, d.end_col, &m);
                        }
                        // Bare → specialized generic assignment: safe at the
                        // machine level (layout-identical) but the element
                        // type is unchecked — warn (clang-style), -Werror
                        // upgrades.
                        if let Some(m) = self.init_generic_erasure_warning(vt, it) {
                            self.check_warning_over(d.line, d.col, d.end_line, d.end_col, &m);
                        }
                    }
                }
                // Comma-declarator chain: `T a = 1, *b = &a;` — the parser chains
                // subsequent declarators in `next`, each with its own `var_type`
                // (since the pointer-declarator-list fix). Only the head used to
                // be registered, so in `Point p1 = {1,2}, p2 = {1,2};` the checker
                // never learned `p2`'s type and the `==` value-struct rewrite
                // silently skipped it, leaving raw `p1 == p2` for the C compiler.
                let mut cur = next.as_deref_mut();
                while let Some(nd) = cur {
                    let mut tail: Option<&mut AstDecl> = None;
                    if let AstDeclData::Variable { var_type: vt, init: ni, next: nn, .. } = &mut nd.data {
                        if let Some(ref name) = nd.name {
                            if let Some(scope) = self.scope_vars.last_mut() {
                                if !scope.iter().any(|(n, _)| n == name) {
                                    let t = vt.as_deref().cloned()
                                        .or_else(|| head_type.clone())
                                        .unwrap_or_else(|| AstType::new(TypePrim::Int));
                                    scope.push((name.clone(), t));
                                }
                            }
                        }
                        if let Some(ref mut i) = ni { self.check_expr(i); }
                        tail = nn.as_deref_mut();
                    }
                    cur = tail;
                }
            }
            AstDeclData::Class { methods, .. } => {
                let old = self.current_class.clone();
                self.current_class = d.name.clone();
                for m in methods { self.check_decl(m); }
                self.current_class = old;
            }
            AstDeclData::Method { body, params, is_class_method, throws, .. } => {
                if let Some(ref mut b) = body {
                    let old = self.current_method.clone();
                    let old_is_class = self.current_method_is_class;
                    let old_locals = std::mem::take(&mut self.shadowed_locals);
                    let old_ann = std::mem::replace(&mut self.throws_ann, Self::throws_state(throws.as_deref()));
                    let old_thrown = std::mem::take(&mut self.thrown);
                    let old_uncaught = std::mem::take(&mut self.uncaught_throws);
                    let old_depth = std::mem::replace(&mut self.try_depth, 0);
                    self.current_method = d.name.clone();
                    self.current_method_is_class = *is_class_method;
                    // Fresh frame for this method's params, popped after the
                    // body — a later method reusing the parameter name
                    // (`other` in NPString vs NPMutableArray) must resolve
                    // its OWN declared type, not the first declarer's.
                    self.scope_vars.push(Vec::new());
                    self.add_params_to_scope(params);
                    // `self` enters the frame with its REAL type so the
                    // assignment family sees `self = [super init]` as an
                    // object join, not an int ← pointer conversion (the
                    // implied-list fallback answers `int` — the TOMLEditor
                    // regression). Instance methods: instancetype; class
                    // methods: the class object itself.
                    if let Some(scope) = self.scope_vars.last_mut() {
                        if !scope.iter().any(|(n, _)| n == "self") {
                            let self_ty = if *is_class_method {
                                let mut t = AstType::new(TypePrim::Named);
                                t.name = self.current_class.clone();
                                t
                            } else {
                                AstType::new(TypePrim::Instancetype)
                            };
                            scope.push(("self".to_string(), self_ty));
                        }
                    }
                    if params.is_some() {
                        let mut p = params.as_ref().map(|b| &**b);
                        while let Some(param) = p {
                            if let Some(ref name) = param.name {
                                if !self.shadowed_locals.contains(name) { self.shadowed_locals.push(name.clone()); }
                            }
                            p = param.next.as_ref().map(|n| &**n);
                        }
                    }
                    Self::collect_decl_names_stmt(b, &mut self.shadowed_locals);
                    self.check_stmt(b);
                    self.scope_vars.pop();
                    self.reconcile_throws(d.name.as_deref(), d.line, d.col);
                    self.try_depth = old_depth;
                    self.uncaught_throws = old_uncaught;
                    self.thrown = old_thrown;
                    self.throws_ann = old_ann;
                    self.shadowed_locals = old_locals;
                    self.current_method = old;
                    self.current_method_is_class = old_is_class;
                }
            }
            _ => {}
        }
    }

    /// Enum exhaustiveness for a plain-C `switch` (pattern arms are lowered
    /// to goto/if by the pattern pass BEFORE the checker runs, so any
    /// `Switch` node that reaches here is an ordinary C switch whose cases
    /// are constant expressions).
    ///
    /// Fires only when the subject's static type is an enum this TU can see
    /// the member list of (tag decl, or typedef alias → tag). Any other
    /// subject — scalars, `id`, enums declared only in a `#include`d C
    /// header — stays silent: same conservative direction as the rest of
    /// this checker (a missed exhaustiveness costs nothing; a false one
    /// rejects legal C). A `default:` arm opts out, exactly like C's
    /// `-Wswitch`.
    fn check_switch_exhaustiveness(&mut self, expr: &AstExpr, body: &AstStmt) {
        let type_name = expr.expr_type.as_ref().and_then(|t| t.name.clone());
        let Some(name) = type_name else { return };
        // Member list: three spellings reach here. (1) `enum Color c` — the
        // type node carries the tag. (2) `typedef enum {Up, Down} Direction;`
        // — the PARSER rewrites this to a plain Enum decl named `Direction`
        // (parse_typedef's Enum branch sets the alias as the decl name), so
        // the elaborated AstDeclData::Enum is keyed by the alias itself and
        // the variable's type node just spells `Direction`. (3) a typedef
        // alias of a separately-named tag — resolved through enum_alias_tags.
        // Lookup is by name, tag-agnostic: a struct/typedef can never share
        // the key with an enum member table entry because collect_enums only
        // inserts from Enum decls.
        let members = self.enum_members.get(&name).cloned()
            .or_else(|| {
                self.enum_alias_tags.get(&name)
                    .and_then(|tag| self.enum_members.get(tag).cloned())
            });
        let Some(members) = members else { return };
        if members.is_empty() { return; }

        // Collect the case labels of THIS switch: the body's top-level
        // Case/Default statements. Case bodies are NOT descended into — a
        // nested switch lives inside an arm's body and its labels belong to
        // that inner switch.
        let mut has_default = false;
        let mut covered: Vec<String> = Vec::new();
        let mut all_recognized = true;
        fn walk(body: &AstStmt, members: &[String], covered: &mut Vec<String>,
                has_default: &mut bool, all_recognized: &mut bool) {
            match &body.data {
                AstStmtData::Case { value, .. } => {
                    match &value.data {
                        AstExprData::VarRef { name, .. } if members.iter().any(|m| m == name) => {
                            covered.push(name.clone());
                        }
                        _ => {
                            // A constant expression (int literal, `A | B`, a
                            // macro) — member identity is not provable here.
                            *all_recognized = false;
                        }
                    }
                }
                AstStmtData::Default(_) => { *has_default = true; }
                AstStmtData::Compound(v) => {
                    for s in v { walk(s, members, covered, has_default, all_recognized); }
                }
                _ => {}
            }
        }
        walk(body, &members, &mut covered, &mut has_default, &mut all_recognized);

        if has_default || !all_recognized { return; }
        let missing: Vec<&String> = members.iter()
            .filter(|m| !covered.contains(m))
            .collect();
        if missing.is_empty() { return; }
        let list = missing.iter().map(|m| format!("'{}'", m)).collect::<Vec<_>>().join(", ");
        self.check_error_over(expr.line, expr.col, expr.end_line, expr.end_col, &format!(
            "switch over enum '{}' does not cover member(s): {} — add the missing case(s) or a default arm",
            name, list));
    }

    /// Collect enum member tables and typedef→enum-tag aliases from the
    /// unit (first pass, before any body is checked). Recurses into
    /// namespaces and class bodies so an enum declared inside either still
    /// participates in exhaustiveness.
    fn collect_enums(&mut self, decl: &AstDecl) {
        match &decl.data {
            AstDeclData::Enum { members, values } => {
                if let Some(tag) = &decl.name {
                    self.enum_members.insert(tag.clone(), members.clone());
                }
                // Fold member values (parser guarantees a value per member —
                // explicit `= N` or the implicit increment). Only integer
                // literals fold; expression-valued members stay unmapped and
                // the duplicate check stays silent for them.
                for (m, v) in members.iter().zip(values.iter()) {
                    if let AstExprData::Int(val) = &v.data {
                        self.enum_member_values.insert(m.clone(), *val);
                    }
                }
            }
            AstDeclData::Typedef { aliased_type: Some(at), .. } => {
                if at.tag == TagKind::Enum {
                    if let (Some(alias), Some(tag)) = (&decl.name, &at.name) {
                        self.enum_alias_tags.insert(alias.clone(), tag.clone());
                    }
                }
            }
            AstDeclData::Namespace(inner) => {
                for d in inner { self.collect_enums(d); }
            }
            AstDeclData::Class { methods, impl_vars, .. } => {
                for m in methods { self.collect_enums(m); }
                for v in impl_vars { self.collect_enums(v); }
            }
            _ => {}
        }
    }

    /// Check the entire AST unit
    pub fn check(&mut self, unit: &mut AstUnit) -> i32 {
        // First pass: collect method/function signatures from the AST.
        for decl in &unit.decls {
            self.collect_signatures(decl);
        }
        // Enum member tables for switch exhaustiveness (same pass shape as
        // the typedef maps below).
        for decl in &unit.decls {
            self.collect_enums(decl);
        }
        // Angle-bracket C headers feed through `Pipeline::prepare` from
        // `pre.c_headers` (`feed_included_headers`) — the passthrough include
        // lines live in the preprocessor, not in the AST.
        // Map `typedef struct Tag { ... } Alias;` so a variable declared with
        // the alias name still compares as a value struct (see struct_alias_tags).
        for decl in &unit.decls {
            if let AstDeclData::Typedef { aliased_type: Some(at), .. } = &decl.data {
                if let Some(alias) = &decl.name {
                    // `typedef struct Tag { … } Alias;` → the alias name must
                    // still compare as a value struct.
                    if at.is_struct {
                        if let Some(tag) = at.name.as_ref() {
                            self.struct_alias_tags.insert(alias.clone(), tag.clone());
                        }
                    }
                    // `typedef void (^Sink)(nonnull NPString *);` → keep the
                    // block type so a variable declared `Sink sink = …;` can
                    // have its parameters (and their annotations) checked at
                    // the call site. Deliberately a separate arm: a block type
                    // carries no `name`, so folding this into the
                    // `struct_alias_tags` arm above would never match.
                    if at.is_block {
                        // `at: &Box<AstType>` — one deref is still the Box.
                        self.typedef_blocks.insert(alias.clone(), (**at).clone());
                    }
                }
            }
        }
        // File-level scope frame: top-level variable declarations register
        // here and stay visible to every function/method body below. The
        // per-function frames push on top of this one and are popped when
        // their body finishes — parameters can no longer leak into the next
        // declaration's scope (the old leak made the FIRST method using a
        // parameter name win for every later method with the same name,
        // mis-typing receivers Foundation-wide: `other` resolved to
        // NPString* inside NPMutableArray).
        self.scope_vars.push(Vec::new());
        for decl in &mut unit.decls {
            self.check_decl(decl);
        }
        if !self.has_error {
            self.check_protocol_conformance(unit);
            self.check_generic_bounds_decls(unit);
            self.check_category_rules(unit);
        }
        if self.has_error { -1 } else { 0 }
    }

    /// Protocol conformance: for every `@interface X <P> ...` in this unit,
    /// each required method of `P` (and its ancestors) must be implemented
    /// somewhere in the unit — the class's method list (interface or
    /// implementation). Mirrors ObjC's "method declared in protocol not
    /// implemented" diagnostic. Skipped when the class has no visible
    /// implementation in this unit (header-only classes link their impl
    /// elsewhere); `@optional` methods are exempt.
    fn check_protocol_conformance(&mut self, unit: &AstUnit) {
        // Snapshot the protocol table (fqn -> required methods + parents) up
        // front so the borrow of `self.symtab` ends before error reporting
        // (which needs `&mut self`).
        let mut proto_table: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
        if let Some(ref st) = self.symtab {
            for sym in &st.global.symbols {
                if let SymbolData::Protocol { required_methods, parents, .. } = &sym.data {
                    proto_table.insert(sym.name.clone(), (required_methods.clone(), parents.clone()));
                }
            }
        }
        // class_fqn -> implemented selector set (from all Class decls in unit)
        let mut class_methods: HashMap<String, Vec<String>> = HashMap::new();
        let mut has_impl: Vec<String> = Vec::new();
        for decl in &unit.decls {
            if let AstDeclData::Class { methods, is_implementation, .. } = &decl.data {
                if let Some(ref cname) = decl.name {
                    if *is_implementation && !has_impl.iter().any(|c| c == cname) {
                        has_impl.push(cname.clone());
                    }
                    let entry = class_methods.entry(cname.clone()).or_default();
                    for m in methods {
                        if let Some(ref sel) = m.name {
                            // Only methods WITH a body count as implemented.
                            // binder's propagate_protocol_methods injects bare
                            // protocol declarations into the @interface to keep
                            // cross-TU vtable layouts stable — those have no
                            // body and must not satisfy conformance here.
                            let has_body = matches!(&m.data,
                                AstDeclData::Method { body: Some(_), .. });
                            if has_body && !entry.contains(sel) { entry.push(sel.clone()); }
                        }
                    }
                }
            }
        }
        // Collect (line, col, class, selector, protocol) violations, then report.
        let mut violations: Vec<(usize, usize, String, String, String)> = Vec::new();
        for decl in &unit.decls {
            if let AstDeclData::Class { protocols, .. } = &decl.data {
                let Some(ref cname) = decl.name else { continue };
                // A class with no @implementation in this unit can't be
                // checked here — its methods live in another TU. Protocols
                // usually sit on the @interface (not the impl), so match by
                // class name, not by is_implementation on this decl.
                if !has_impl.iter().any(|c| c == cname) { continue; }
                let Some(implemented) = class_methods.get(cname) else { continue };
                for proto in protocols {
                    // Resolve the protocol symbol (short or namespace FQN).
                    let ns = cname.split("::").next().unwrap_or("").to_string();
                    let resolved = if proto_table.contains_key(proto) {
                        Some(proto.clone())
                    } else if !ns.is_empty() {
                        let fqn = format!("{}::{}", ns, proto);
                        if proto_table.contains_key(&fqn) { Some(fqn) } else { None }
                    } else {
                        None
                    };
                    let Some(pfqn) = resolved else { continue };
                    // Walk this protocol and its ancestors (cycle-guarded).
                    let mut seen: Vec<String> = Vec::new();
                    let mut queue: Vec<String> = vec![pfqn];
                    while let Some(p) = queue.pop() {
                        if seen.iter().any(|s| *s == p) { continue; }
                        seen.push(p.clone());
                        let Some((required, parents)) = proto_table.get(&p) else { continue };
                        for sel in required {
                            // Selector spelling varies: binder stores multi-part
                            // selectors with a trailing colon (`deployShield:`),
                            // AST method names may drop it. Compare colon-stripped.
                            let norm = |s: &str| s.trim_end_matches(':').to_string();
                            let sel_n = norm(sel);
                            if !implemented.iter().any(|s| norm(s) == sel_n)
                                && !violations.iter().any(|(_, _, c, s, _)| c == cname && s == sel)
                            {
                                violations.push((decl.line, decl.col, cname.clone(), sel.clone(), proto.clone()));
                            }
                        }
                        queue.extend(parents.iter().cloned());
                    }
                }
            }
        }
        for (line, col, cls, sel, proto) in violations {
            self.check_error(line, col, &format!(
                "class '{}' does not implement required method '{}' from protocol '{}'",
                cls, sel, proto));
        }
    }

    /// Category implementation rules (D3/D4 of docs/plans/categories_protocol_plan.md):
    /// D4 — a selector implemented twice for the same class (main
    /// @implementation + category, or two categories) is a compile-time error.
    /// Static dispatch has no method list to prepend, so the ObjC "later
    /// category wins silently" semantics cannot be reproduced — the P2 probe
    /// showed the current behavior is a silent body override, which hides
    /// real conflicts; refuse them loudly instead.
    /// D3 — a category implementation must not declare ivars: the struct
    /// layout belongs to the main @implementation; an ivar visible only in
    /// the category TU would shift nothing in other TUs' sizeof and misread
    /// fields cross-TU (ObjC forbids this for the same reason).
    fn check_category_rules(&mut self, unit: &AstUnit) {
        let norm = |s: &str| s.trim_end_matches(':').to_string();
        // Pass 1: selectors implemented (with a body) in main @implementations.
        let mut main_sels: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for decl in &unit.decls {
            let AstDeclData::Class { methods, is_implementation, is_category, .. } = &decl.data
                else { continue };
            if !*is_implementation || *is_category { continue; }
            let Some(ref cname) = decl.name else { continue };
            let entry = main_sels.entry(cname.clone()).or_default();
            for m in methods {
                if let (AstDeclData::Method { body: Some(_), .. }, Some(ref sel)) = (&m.data, &m.name) {
                    if !entry.iter().any(|s| s == sel) { entry.push(sel.clone()); }
                }
            }
        }
        // Pass 2: category implementations — D3 ivar ban + D4 duplicate selectors
        // (against the main impl and against other categories of the same class).
        let mut violations: Vec<(usize, usize, String)> = Vec::new();
        let mut seen_cat_sels: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for decl in &unit.decls {
            let AstDeclData::Class { methods, impl_vars, is_implementation, is_category, .. } = &decl.data
                else { continue };
            if !(*is_implementation && *is_category) { continue; }
            let Some(ref cname) = decl.name else { continue };
            // D3: ivars arrive here as impl_vars (the parser collects an
            // @implementation body's field block through its declaration
            // path). File/function `static` globals stay legal — a
            // category's private statics are an established pattern — so
            // only non-static instance fields are refused.
            for iv in impl_vars {
                let AstDeclData::Variable { is_static, .. } = &iv.data else { continue };
                if *is_static { continue; }
                if let Some(ref n) = iv.name {
                    violations.push((decl.line, decl.col, format!(
                        "category on '{}' declares ivar '{}' — categories cannot add instance variables (the struct layout belongs to the main @implementation); move it to the main @interface",
                        cname, n)));
                }
            }
            let seen = seen_cat_sels.entry(cname.clone()).or_default();
            for m in methods {
                let (AstDeclData::Method { body: Some(_), .. }, Some(ref sel)) = (&m.data, &m.name)
                    else { continue };
                let sel_n = norm(sel);
                if main_sels.get(cname).map_or(false, |v| v.iter().any(|s| norm(s) == sel_n)) {
                    violations.push((m.line, m.col, format!(
                        "category on '{}' re-implements '{}' which the main @implementation already implements — static dispatch cannot order the two; remove one",
                        cname, sel)));
                } else if seen.iter().any(|s| norm(s) == sel_n) {
                    violations.push((m.line, m.col, format!(
                        "selector '{}' is implemented by two categories on '{}' — static dispatch cannot order them; remove one",
                        sel, cname)));
                } else {
                    seen.push(sel.clone());
                }
            }
        }
        for (line, col, msg) in violations {
            self.check_error(line, col, &msg);
        }
    }

    /// §5.1/§5.2: a subclass of a bounded generic must re-declare every
    /// inherited bound (explicit spelling — `.hje` readers see the whole
    /// constraint, same philosophy as full ivar layouts in shared headers)
    /// and must not weaken it (the re-declared bound must be equal or a
    /// sub-protocol/subclass of the inherited one). Unit-level pass, same
    /// shape as `check_protocol_conformance`: violations are collected first
    /// and reported after the symtab borrow ends.
    fn check_generic_bounds_decls(&mut self, unit: &AstUnit) {
        let mut violations: Vec<(usize, usize, String)> = Vec::new();
        for decl in &unit.decls {
            let AstDeclData::Class { super_name, type_params, type_bounds, .. } = &decl.data
                else { continue };
            let Some(ref parent) = super_name else { continue };
            let Some(cls_name) = decl.name.as_ref() else { continue };
            // Parent's bound table lives in the symbol table, so parents
            // declared in imported headers are covered too.
            let (p_params, p_bounds) = {
                let Some(ref st) = self.symtab else { continue };
                let Some(sym) = st.find_class(parent) else { continue };
                let SymbolData::Class { type_params, type_bounds, .. } = &sym.data else { continue };
                (type_params.clone(), type_bounds.clone())
            };
            if p_bounds.is_empty() { continue; }
            for (pname, pbound) in &p_bounds {
                // Positional parent→subclass mapping (the `MutableBox<T> :
                // Box<T>` idiom); unmappable positions escape.
                let Some(idx) = p_params.iter().position(|q| q == pname) else { continue };
                let Some(sub_param) = type_params.get(idx) else { continue };
                let sub_bound = type_bounds.iter()
                    .find(|(n, _)| n == sub_param)
                    .map(|(_, b)| b.clone());
                let Some(sbound) = sub_bound else {
                    violations.push((decl.line, decl.col, format!(
                        "class '{}' must re-declare the bound on type parameter '{}' (inherited bound '{}' from '{}') — bounds are not implicit",
                        cls_name, sub_param, pbound, parent)));
                    continue;
                };
                // §5.2 strength: equal or more specific only. Unresolvable
                // names escape — weakening cannot be proven here.
                let is_proto_bound = self.protocol_parents(pbound).is_some();
                let ok = if is_proto_bound {
                    self.protocol_extends(&sbound, pbound)
                } else if self.symtab.as_ref().and_then(|st| st.find_class(pbound)).is_some() {
                    self.class_derives_from(&sbound, pbound)
                } else {
                    true
                };
                if !ok {
                    violations.push((decl.line, decl.col, format!(
                        "bound '{}' on '{}' weakens the inherited bound '{}' from '{}' — the re-declared bound must be equal or more specific",
                        sbound, sub_param, pbound, parent)));
                }
            }
        }
        for (line, col, msg) in violations {
            self.check_error(line, col, &msg);
        }
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `self->_x` (an instance-ivar access via self).
    fn self_ivar_expr() -> AstExpr {
        AstExpr {
            kind: AstExprKind::IvarRef, expr_type: None, line: 1, col: 1,
            data: AstExprData::IvarRef {
                ivar: Some("_x".into()),
                cls: Some("Foo".into()),
                obj: Box::new(AstExpr {
                    kind: AstExprKind::Self_, expr_type: None, line: 1, col: 1,
                    data: AstExprData::VarRef { sym: None, name: "self".into() },
                    end_line: 0,
                    end_col: 0,
                }),
            },
            end_line: 0,
            end_col: 0,
        }
    }

    /// Check a one-statement method body `_x;` and return the checker result.
    fn check_method(is_class_method: bool) -> i32 {
        let body = AstStmt {
            kind: AstStmtKind::Expr, line: 1, col: 1,
            data: AstStmtData::Expr(self_ivar_expr()),
            end_line: 0,
            end_col: 0,
        };
        let method = AstDecl {
            kind: AstDeclKind::Method, name: Some("foo".into()), line: 1, col: 1,
            data: AstDeclData::Method {
                method_sym: None, is_class_method,
                return_type: None, params: None,
                has_variadic: false,
                throws: None,
                async_marker: false,
                body: Some(Box::new(body)),
            },
            attributes: Vec::new(),
            end_line: 0,
            end_col: 0,
        };
        let mut unit = AstUnit { decls: vec![method], filename: String::new(), namespaces: Vec::new(), usings: Vec::new() };
        let mut c = Checker::new(None);
        c.check(&mut unit)
    }

    #[test]
    fn class_method_ivar_access_is_error() {
        assert_eq!(check_method(true), -1,
            "class method ('+') must not access instance ivars (self is the class)");
    }

    #[test]
    fn instance_method_ivar_access_is_ok() {
        assert_eq!(check_method(false), 0,
            "instance method ('-') may access instance ivars");
    }

    /// Regression (referendum #3, gap 2): the -eh desugar hoists call-bearing
    /// subexpressions into `__auto_type __jeti_eh_tmp_N` declarations. Looking
    /// those names up must return the type the initializer produced — the
    /// `__`-prefix builtin fallback (`__FILE__`/`__LINE__` → int) used to run
    /// FIRST and shadow the real binding, so the generic-argument check read
    /// `int` for an `NPMutableString *` temp and rejected valid code.
    #[test]
    fn eh_hoist_temp_resolves_to_declared_type_not_the_int_fallback() {
        let mut c = Checker::new(None);
        let mut npstring_ptr = AstType::new(TypePrim::Named);
        npstring_ptr.name = Some("NPString".into());
        npstring_ptr.is_pointer = true;
        c.scope_vars.push(vec![("__jeti_eh_tmp_0".to_string(), npstring_ptr)]);
        let mut e = AstExpr {
            kind: AstExprKind::VarRef, expr_type: None, line: 1, col: 1,
            data: AstExprData::VarRef { sym: None, name: "__jeti_eh_tmp_0".into() },
            end_line: 0,
            end_col: 0,
        };
        let got = c.check_expr(&mut e).expect("hoisted temp must have a type");
        assert_eq!(got.name.as_deref(), Some("NPString"),
            "the `__` builtin fallback must not shadow the hoisted temp's real type");
        assert!(got.is_pointer, "hoisted object temp stays a pointer");
    }

    /// The reordering must not break the fallback it guards: genuine builtin
    /// macros are never registered in `scope_vars`, so they still resolve to
    /// int (and C string-literal builtins like `__FILE__` keep working).
    #[test]
    fn builtin_prefix_fallback_still_applies_when_not_in_scope() {
        let mut c = Checker::new(None);
        let mut e = AstExpr {
            kind: AstExprKind::VarRef, expr_type: None, line: 1, col: 1,
            data: AstExprData::VarRef { sym: None, name: "__LINE__".into() },
            end_line: 0,
            end_col: 0,
        };
        let got = c.check_expr(&mut e).expect("builtin must resolve");
        assert_eq!(got.prim, TypePrim::Int,
            "__LINE__ outside scope still takes the int fallback");
    }

    /// Check `NPLog(fmt, …args)` as a one-statement method body.
    /// Returns (checker result, warnings).
    fn check_nplog(fmt: &str, args: Vec<AstExpr>) -> (i32, Vec<String>) {
        let mut call_args = vec![AstExpr {
            kind: AstExprKind::AtString, expr_type: None, line: 1, col: 1,
            data: AstExprData::AtString(fmt.into()),
            end_line: 0,
            end_col: 0,
        }];
        call_args.extend(args);
        let call = AstExpr {
            kind: AstExprKind::FuncCall, expr_type: None, line: 1, col: 1,
            data: AstExprData::FuncCall {
                func: None, name: "NPLog".into(), callee: None, args: call_args,
            },
            end_line: 0,
            end_col: 0,
        };
        let body = AstStmt {
            kind: AstStmtKind::Expr, line: 1, col: 1,
            data: AstStmtData::Expr(call),
            end_line: 0,
            end_col: 0,
        };
        let method = AstDecl {
            kind: AstDeclKind::Method, name: Some("foo".into()), line: 1, col: 1,
            data: AstDeclData::Method {
                method_sym: None, is_class_method: false,
                return_type: None, params: None,
                has_variadic: false,
                throws: None,
                async_marker: false,
                body: Some(Box::new(body)),
            },
            attributes: Vec::new(),
            end_line: 0,
            end_col: 0,
        };
        let mut unit = AstUnit { decls: vec![method], filename: String::new(), namespaces: Vec::new(), usings: Vec::new() };
        let mut c = Checker::new(None);
        let r = c.check(&mut unit);
        (r, c.warnings().to_vec())
    }

    fn lit_int(v: i64) -> AstExpr {
        AstExpr { kind: AstExprKind::Int, expr_type: None, line: 1, col: 1, data: AstExprData::Int(v), end_line: 0, end_col: 0 }
    }

    fn lit_atstr(s: &str) -> AstExpr {
        AstExpr { kind: AstExprKind::AtString, expr_type: None, line: 1, col: 1, data: AstExprData::AtString(s.into()), end_line: 0, end_col: 0 }
    }

    #[test]
    fn nplog_percent_at_with_int_warns() {
        let (r, w) = check_nplog("count %@", vec![lit_int(42)]);
        assert_eq!(r, 0, "format mismatch is a warning, not an error");
        assert_eq!(w.len(), 1, "expected exactly one warning: {:?}", w);
        assert!(w[0].contains("%@"), "warning should name the specifier: {:?}", w);
    }

    #[test]
    fn nplog_percent_at_with_object_ok() {
        let (_, w) = check_nplog("obj %@", vec![lit_atstr("fine")]);
        assert!(w.is_empty(), "%@ with an object must not warn: {:?}", w);
    }

    #[test]
    fn nplog_scalar_format_ok() {
        let (_, w) = check_nplog("n %d", vec![lit_int(7)]);
        assert!(w.is_empty(), "%d with int must not warn: {:?}", w);
    }

    #[test]
    fn nplog_percent_at_count_mismatch_warns() {
        let (_, w) = check_nplog("%@ %@", vec![lit_atstr("only")]);
        assert_eq!(w.len(), 1, "more %@ slots than args should warn once: {:?}", w);
    }
}