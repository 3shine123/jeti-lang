use jeti_lexer::{KeywordKind, Lexer, Token, TokenKind, INT_SUFFIX_L, INT_SUFFIX_LL, INT_SUFFIX_U};
use jeti_cst::*;

pub struct Parser<'a> {
    lexer: Lexer<'a>,
    source: &'a str,
    current: Token,
    previous: Token,
    has_error: bool,
    error_count: usize,
    err_msg: String,
    /// Structured diagnostics with token-width spans. `error()` records here
    /// alongside the legacy flat string; the pipeline renders these instead of
    /// re-parsing `err_msg`.
    diagnostics: Vec<Diagnostic>,
    panic_mode: bool,
    type_names: Vec<String>,
    /// True when `type_names` is known to be *complete* — i.e. it came from the
    /// C preprocessor's own view of this TU's `#include`d headers (jetic's
    /// `ctype_probe`). Only then may the parser let the symbol table decide the
    /// cases C decides by the symbol table: whether `(X *)p` is a cast, and
    /// whether `x * y;` is a declaration. When the table is not authoritative
    /// the historical shape fallbacks stay in force, so a missing table can
    /// never turn working code into a parse error.
    type_table_complete: bool,
    type_params: Vec<String>,
    generic_class_names: Vec<String>,
    /// True while parsing a type in a position that may carry a nullability
    /// annotation (method return type, method/function parameter, ivar).
    /// Those positions unambiguously expect a type, so `nullable` / `nonnull`
    /// can be read as annotations with **no lookahead** — which is what keeps
    /// `int nullable = 5;` compiling, per the C-superset rule. Gated by a flag
    /// rather than guessed, so a declaration whose *name* is `nullable` is
    /// never misread.
    annotating: bool,
    /// True while `annotating` covers an `@interface` ivar type. Ivars accept
    /// nullability only in the postfix spelling (`NPString * _Nullable x`):
    /// clang rejects the prefix form there ("unknown type name" for the bare
    /// spellings, "cannot be applied to non-pointer type" for the underscore
    /// ones — probe-verified 2026-10-24), so the qualifier loop reports a
    /// targeted error instead of silently recording it. C struct fields and
    /// @implementation ivars parse through `parse_type_full` /
    /// `parse_declaration` (never with `annotating` set), so they are
    /// untouched by this flag.
    ivar_position: bool,
    /// Inside an `NP_ASSUME_NONNULL_BEGIN` … `_END` region: an unannotated
    /// pointer type defaults to `Nonnull` (ObjC's audit-cost-O(1) trick).
    /// Toggled by the two marker identifiers in `parse_declaration`.
    nonnull_region: bool,
    /// File the open region was opened in. The parser reads one inlined buffer,
    /// so it has no `#include` boundary of its own; without this an open region
    /// would silently mark every pointer in every `#import`ed header nonnull.
    /// `None` = no region open.
    region_file: Option<String>,
    /// Maps inlined-buffer lines back to (file, source line), so the region can
    /// be scoped to one file. Injected by the pipeline (`pre.source_map`).
    source_map: Option<jeti_cst::SourceMap>,
    /// Bracket depth of the token stream, maintained by advance(). Used by the
    /// in-message bare-`await` ban: a message's `[` records the depth just
    /// inside it, and an `await` at that same depth sits unparenthesized in a
    /// receiver/argument slot.
    expr_paren_depth: usize,
    /// Open message sends: (bracket depth just inside the message's `[`,
    /// already-reported flag). Popped when the depth falls below the recorded
    /// value. Entries survive block `{`: a block body is one level deeper, so
    /// awaits inside it never match, and the enclosing message's ban must
    /// still cover arguments that follow a block argument.
    msg_await_stack: Vec<(usize, bool)>,
    /// Open-bracket stack for the `to match this '('` companion notes
    /// (clang prints one under every mismatched `)`/`]`/`}`): kind
    /// (0=`(`, 1=`[`, 2=`{`) plus the opener's raw line/column, recorded in
    /// `advance()` so every consume path stays balanced — the same invariant
    /// that keeps `expr_paren_depth` honest (speculative lookaheads never
    /// cross a bracket token without rewinding).
    open_brackets: Vec<(u8, usize, usize)>,
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut lexer = Lexer::new(source);
        let current = lexer.next_token();
        Parser {
            lexer,
            source,
            current,
            previous: Token {
                kind: TokenKind::Eof, keyword: KeywordKind::None,
                start: 0, length: 0, line: 0, column: 0, end_line: 0, end_col: 0,
                char_val: 0, int_suffix: 0,
            },
            has_error: false,
            error_count: 0,
            err_msg: String::new(),
            diagnostics: Vec::new(),
            panic_mode: false,
            type_names: vec![
                // Built-in C types (from old C parser's register_builtin_types)
                "fd_set".into(), "timeval".into(), "timespec".into(),
                "termios".into(), "sigaction".into(), "stat".into(),
                "sockaddr".into(), "in_addr".into(), "sockaddr_in".into(),
                "addrinfo".into(), "dirent".into(), "passwd".into(),
                "FILE".into(), "size_t".into(), "ssize_t".into(),
                "int8_t".into(), "int16_t".into(), "int32_t".into(), "int64_t".into(),
                "uint8_t".into(), "uint16_t".into(), "uint32_t".into(), "uint64_t".into(),
                "uintptr_t".into(), "intptr_t".into(),
                "pthread_t".into(), "pthread_mutex_t".into(), "pthread_cond_t".into(),
                "va_list".into(),
                // Time / POSIX types (used with clock()/time()/etc.)
                "clock_t".into(), "time_t".into(), "clockid_t".into(),
                "off_t".into(), "pid_t".into(), "mode_t".into(),
                "socklen_t".into(), "useconds_t".into(), "suseconds_t".into(),
                "ssize_t".into(), "wchar_t".into(), "char16_t".into(), "char32_t".into(),
                "key_t".into(), "fsblkcnt_t".into(), "fsfilcnt_t".into(), "blkcnt_t".into(),
                "blksize_t".into(), "dev_t".into(), "id_t".into(), "ino_t".into(),
                "nlink_t".into(), "uid_t".into(), "gid_t".into(),
                // `NPTask<T>` — real task-handle class (docs/plans/async_nptask_plan.md).
                // Registered so `NPTask<int>` parses type args; the `async`
                // modifier in front of it is a contextual keyword handled at
                // the return-type position (at_async_modifier).
                "NPTask".into(),
            ],
            type_table_complete: false,
            type_params: Vec::new(),
            generic_class_names: vec!["NPTask".into()],
            annotating: false,
            ivar_position: false,
            nonnull_region: false,
            region_file: None,
            source_map: None,
            expr_paren_depth: 0,
            msg_await_stack: Vec::new(),
            open_brackets: Vec::new(),
        }
    }

    /// Give the parser the pipeline's line→file map so an `NP_ASSUME_NONNULL`
    /// region can be scoped to the file that opened it.
    pub fn set_source_map(&mut self, sm: jeti_cst::SourceMap) {
        self.source_map = Some(sm);
        // The first token was read before the map arrived; catch it up so the
        // first diagnostic of a file is not the one that drifts.
        Self::remap_token_col(&self.source_map, &mut self.current);
    }

    /// The file the given inlined-buffer line came from, or `""` when unknown.
    fn file_of_line(&self, line: usize) -> String {
        match &self.source_map {
            Some(sm) => sm.locate(line).0,
            None => String::new(),
        }
    }

    /// True when an open nonnull region applies at `line`. A region belongs to
    /// the file that opened it: crossing into an `#import`ed file ends it, so
    /// an unclosed region can never silently mark a whole library's parameters
    /// nonnull. (Conservative — a region that resumed after the include would be
    /// a nicety, but the failure mode here is "wrong types", not "missed
    /// warning".)
    fn region_applies_at(&mut self, line: usize) -> bool {
        if !self.nonnull_region {
            return false;
        }
        if self.source_map.is_none() {
            // No map: single-file parse, the region plainly applies.
            return true;
        }
        let here = self.file_of_line(line);
        match &self.region_file {
            Some(f) => *f == here,
            None => false,
        }
    }

    /// Parser whose type-name table is seeded with the names declared by the C
    /// headers this TU includes (see `type_table_complete`). `complete` must
    /// only be `true` when those names came from the real C preprocessor.
    pub fn with_c_type_names(source: &'a str, names: &[String], complete: bool) -> Self {
        let mut p = Self::new(source);
        for n in names {
            p.add_type_name(n);
        }
        p.type_table_complete = complete;
        p
    }

    /// Is the type-name table the C preprocessor's own (so an unknown name here
    /// is unknown to C as well)?
    fn type_table_is_authoritative(&self) -> bool {
        self.type_table_complete
    }

    fn current_text(&self) -> &'a str {
        &self.source[self.current.start..self.current.start + self.current.length]
    }

    fn previous_text(&self) -> &'a str {
        &self.source[self.previous.start..self.previous.start + self.previous.length]
    }


    fn check(&self, kind: TokenKind) -> bool {
        self.current.kind == kind
    }

    fn check_keyword(&self, kw: KeywordKind) -> bool {
        self.current.kind == TokenKind::Keyword && self.current.keyword == kw
    }

    fn advance(&mut self) {
        let mut next = self.lexer.next_token();
        Self::remap_token_col(&self.source_map, &mut next);
        // A `"…"` the lexer had to stop at a raw newline is missing its closing
        // quote. Say so at the literal itself, in C's wording: the lexer stops
        // there instead of scanning on for a quote *anywhere* in the buffer, so
        // this diagnostic is all the file loses — the literal still parses as
        // one, and every declaration after it keeps its own diagnostics.
        let unclosed_literal = matches!(next.kind, TokenKind::String | TokenKind::AtString)
            && !Self::literal_is_closed(self.source, &next);
        self.previous = std::mem::replace(&mut self.current, next);
        if unclosed_literal {
            let token = self.current.clone();
            self.error_at(&token, "missing terminating '\"' character");
        }
        // Bracket-depth bookkeeping for the in-message bare-`await` ban (see
        // msg_await_stack). Counted inside advance() so every consume path
        // stays balanced; the parser's speculative lookaheads never cross a
        // bracket token without rewinding, so the count cannot skew. Blocks
        // (`{`) are deliberately NOT special: a block body sits one level
        // deeper than the enclosing message's interior, so statement-position
        // `await` inside it never matches the recorded depth (no false
        // positive), while the enclosing message's ban survives to cover
        // arguments that follow a block argument —
        // `[self run:^{ ... } then:await [x y]]` must still be rejected.
        match self.current.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                self.expr_paren_depth += 1;
                let kind = match self.current.kind {
                    TokenKind::LBracket => 1u8,
                    TokenKind::LBrace => 2u8,
                    _ => 0u8,
                };
                self.open_brackets.push((kind, self.current.line, self.current.column));
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                self.expr_paren_depth = self.expr_paren_depth.saturating_sub(1);
                // The matching opener is the nearest same-kind entry below the
                // top; strays above it belong to error paths that never closed
                // and stay for their own EOF reporting.
                let want = match self.current.kind {
                    TokenKind::RBracket => 1u8,
                    TokenKind::RBrace => 2u8,
                    _ => 0u8,
                };
                if let Some(pos) = self.open_brackets.iter().rposition(|(k, _, _)| *k == want) {
                    self.open_brackets.remove(pos);
                }
                // Falling below a message's interior depth closes it (its own
                // `]`, or an enclosing closer on an error-recovery path).
                while self
                    .msg_await_stack
                    .last()
                    .map_or(false, |&(d, _)| self.expr_paren_depth < d)
                {
                    self.msg_await_stack.pop();
                }
            }
            _ => {}
        }
    }

    /// Translate a token's column from the expanded buffer back to the source.
    ///
    /// Macro expansion moves the text that follows it along the line, and the
    /// bytes it produced have no column in the source at all — so the lexer's
    /// column is a column in the *expanded* buffer, and reporting it lands next
    /// to, or past the end of, the line the author actually wrote. Rows need no
    /// work here: the map's line half attributes the line, and the checker
    /// translates it when it renders a diagnostic.
    fn remap_token_col(sm: &Option<jeti_cst::SourceMap>, token: &mut Token) {
        if let Some(sm) = sm {
            token.column = sm.adjust_col(token.line, token.column as u32).0 as usize;
            // The end column needs the same correction; a macro that shifted
            // this line shifts it for the token's end too.
            token.end_col = sm.adjust_col(token.end_line, token.end_col as u32).0 as usize;
        }
    }

    /// Whether the lexer found a closing quote for a `String`/`AtString` token.
    ///
    /// The token holds only the literal's *body* — the quotes are not part of it
    /// — so the character right after it is the quote that closed it.
    /// `Lexer::read_string` ends the body at a raw newline instead of scanning on
    /// for the next quote anywhere in the buffer, so a body not followed by `"`
    /// is an unclosed literal (it stopped at a newline or at EOF).
    fn literal_is_closed(source: &str, token: &Token) -> bool {
        source.as_bytes().get(token.start + token.length) == Some(&b'"')
    }

    fn match_token(&mut self, kind: TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn match_keyword(&mut self, kw: KeywordKind) -> bool {
        if self.check_keyword(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn match_name(&mut self) -> bool {
        if self.match_token(TokenKind::Identifier) {
            return true;
        }
        if self.current.kind == TokenKind::Keyword {
            let kw = self.current.keyword;
            if kw == KeywordKind::Id || kw == KeywordKind::Class ||
               kw == KeywordKind::Sel || kw == KeywordKind::Instancetype {
                self.advance();
                return true;
            }
        }
        false
    }

    /// Contextual property-attribute words (copy/retain/weak/strong/assign/
    /// nonatomic/getter/setter/readonly/readwrite) are only special inside
    /// `@property (...)`. Everywhere else they are ordinary identifiers
    /// (e.g. `int copy = 0;`, `[obj retain]`, a function named `weak`).
    fn is_contextual_kw_ident(&self) -> bool {
        self.current.kind == TokenKind::Keyword && matches!(
            self.current.keyword,
            KeywordKind::AtCopy | KeywordKind::AtRetain | KeywordKind::AtWeak
                | KeywordKind::AtStrong | KeywordKind::AtAssign
                | KeywordKind::AtNonatomic | KeywordKind::AtGetter
                | KeywordKind::AtSetter | KeywordKind::AtReadonly
                | KeywordKind::AtReadwrite
        )
    }

    fn is_name_token(&self) -> bool {
        self.current.kind == TokenKind::Identifier ||
        (self.current.kind == TokenKind::Keyword &&
            (self.current.keyword == KeywordKind::Self_ ||
             // `id` degrades to a plain identifier in every NAME position
             // (struct field, ivar, local/loop variable, C/ObjC param name):
             // the builtin object type only wins where a TYPE is expected —
             // parse_type_* consumes it there first. C-superset iron law:
             // `struct User { int id; };` and `int id = 42;` must parse.
             self.current.keyword == KeywordKind::Id ||
             // `_cmd` degrades the same way: the generated-C method shape
             // `(NPObject * self, SEL _cmd, ...)` must re-parse cleanly, and
             // in plain C `_cmd` is an ordinary identifier.
             self.current.keyword == KeywordKind::Cmd)) ||
        self.is_contextual_kw_ident()
    }

    fn consume(&mut self, kind: TokenKind, msg: &str) {
        if self.check(kind) {
            self.advance();
        } else {
            // Single-character closers/separators get a clang-style insertion
            // fix-it; multi-char or keyword-ish tokens are spelled in the
            // message and have no single insert.
            let insert = match kind {
                TokenKind::Semicolon => ";",
                TokenKind::Comma => ",",
                TokenKind::RParen => ")",
                TokenKind::RBracket => "]",
                TokenKind::RBrace => "}",
                TokenKind::Colon => ":",
                _ => "",
            };
            self.error_at_missing(msg, insert);
            // clang prints a companion note under every mismatched closer,
            // pointing at the opener: `to match this '('`. The stack holds
            // the openers seen so far — the nearest same-kind entry is the
            // one this closer would have matched.
            self.emit_match_note(kind);
            // Do NOT advance past the offending token. The old behaviour ate it,
            // which for a missing ';' swallowed the *next* declaration's leading
            // token (`int b = 2` lost its `int`, the remnant parsed as a harmless
            // assignment) — so every second error stayed hidden. Leaving the
            // token in place lets the recovery loop in parse_translation_unit /
            // parse_compound_statement resync from the real position.
        }
    }

    /// clang's companion note for a failed closer consume: `to match this '('`
    /// at the opener's own position (probe-measured wording — the note is a
    /// NOTE, never an error: it must not latch panic mode or count toward the
    /// error total). Deduped with the same last-entry guard as the error
    /// paths, so a file ending in two unclosed braces prints one note, not
    /// one per compound statement.
    fn emit_match_note(&mut self, closer_kind: TokenKind) {
        let (want, spelling) = match closer_kind {
            TokenKind::RParen => (0u8, "'('"),
            TokenKind::RBracket => (1u8, "'['"),
            TokenKind::RBrace => (2u8, "'{'"),
            _ => return,
        };
        let Some(&(_, line, col)) = self.open_brackets.iter().rev().find(|(k, _, _)| *k == want)
        else {
            return;
        };
        let (file, real_line) = self.file_of_line_mapped(line);
        let msg = format!("to match this {}", spelling);
        // The note's dedupe cannot use repeats_last alone: a consume failure
        // pushes error-then-note back to back, and the NEXT failure's note
        // compares against that error, not the previous note — one unclosed
        // brace printed its note twice. Scan the whole list: notes are rare
        // and the list is small.
        if self.diagnostics.iter().any(|d| {
            d.severity == Severity::Note && d.file == file && d.line == real_line
                && d.col == col && d.message == msg
        }) {
            return;
        }
        self.diagnostics.push(Diagnostic::note(file, real_line, col, &msg));
    }

    fn consume_keyword(&mut self, kw: KeywordKind, msg: &str) {
        if self.check_keyword(kw) {
            self.advance();
        } else {
            self.error_at_missing(msg, "");
        }
    }

    /// Report at the place the missing token belongs — clang points the caret
    /// of an `expected` diagnostic at the *hole*, not at the token before it:
    /// at the unexpected token when it sits on the same line (`f(1;` → the
    /// `;`), and just past the previous token when the hole is at end of line
    /// (`int a = 1` newline → the position after `1`, where clang's caret and
    /// fix-it both sit). A single caret: clang's `expected` family does not
    /// underline the offending token. `insert` keeps the clang-style fix-it
    /// that shows the missing character under the caret.
    fn error_at_missing(&mut self, msg: &str, insert: &str) {
        // clang's `expected` family points the caret at the INSERTION POINT —
        // previous's end — for every missing token, whether or not an
        // unexpected token fills the slot: `f(1;` carets the `;` (insertion
        // point == offending token), `return 0 }` carets the space before
        // `}` (insertion point < offending token; measured against clang:
        // `1:26` with the fix-it anchored there). The caret is the fix-it's
        // home, which is why the two never diverge in clang's output.
        let line = self.previous.line;
        let col = self.previous.column + self.previous.length.max(1);
        if self.panic_mode { return; }
        let (file, real_line) = self.file_of_line_mapped(line);
        // Same dedupe key as [`Self::error_at`]: the *recorded* (remapped)
        // position — the raw token line never matches a diagnostic's remapped
        // line once a map is set, and the guard would suppress nothing.
        if self.repeats_last(&file, real_line, col, msg) { return; }
        self.panic_mode = true;
        self.has_error = true;
        self.error_count += 1;
        let entry = format!("{}:{}: {}", line, col, msg);
        if self.err_msg.is_empty() {
            self.err_msg = entry;
        } else {
            self.err_msg = format!("{}\n{}", self.err_msg, entry);
        }
        let mut diagnostic = Diagnostic::error(file, real_line, col, msg).with_end_col(col + 1);
        if !insert.is_empty() {
            // The insertion point is where the missing token belongs: right
            // after the previous token (clang anchors its `;` fix-it there too).
            let insert_col = self.previous.column + self.previous.length.max(1);
            diagnostic.hint = Some((insert_col, insert.to_string()));
        }
        self.diagnostics.push(diagnostic);
    }

    /// The dedupe guard behind both error paths: the same diagnostic (file,
    /// line, col, message) as the last *recorded* one is noise — a file ending
    /// with two unclosed braces gets ONE `expected '}'` (plus its note), not
    /// one per compound statement. The parser's nested compounds each
    /// consume-fail at that same EOF back-to-back, so comparing against the
    /// last recorded diagnostic drops the repeat. The file is part of the key:
    /// the same line/col/message in two different files (an inlined header's
    /// error and the including file's) are two diagnostics, not a repeat.
    ///
    /// Pure read on the just-pushed list, so both error paths can call it
    /// before they latch panic mode or append to the plain-text lines.
    fn repeats_last(&self, file: &str, line: usize, col: usize, msg: &str) -> bool {
        match self.diagnostics.last() {
            Some(last) => {
                last.file == file && last.line == line && last.col == col && last.message == msg
            }
            None => false,
        }
    }

    fn error(&mut self, msg: &str) {
        let token = self.previous.clone();
        self.error_at(&token, msg);
    }

    /// Like [`Self::error`], but reports at a token other than `self.previous`.
    ///
    /// [`Self::advance`] uses it for a literal the lexer had to stop early: by
    /// the time the parser would otherwise complain, `previous` is whatever
    /// token follows the literal, and the caret has to stay on the literal
    /// itself (as clang's `missing terminating '"' character` does).
    fn error_at(&mut self, token: &Token, msg: &str) {
        if self.panic_mode { return; }
        let line = token.line;
        let col = token.column;
        // Span width from the offending token itself: end_col is exclusive,
        // so `nil` at column 14 (length 3) annotates as ^~~.
        let end_col = col + token.length.max(1);
        let (file, real_line) = self.file_of_line_mapped(line);
        // clang never emits the same diagnostic twice at one spot: a file
        // ending with two unclosed braces gets ONE `expected '}'` (plus its
        // note), not one per compound statement. The parser's nested compounds
        // would each consume-fail at that same EOF, so a repeat with the same
        // position and message is noise — drop it. The key is the *recorded*
        // (remapped) position: comparing the raw token line against the
        // diagnostic's remapped line never matches once a map is set, and the
        // guard would suppress nothing.
        if self.repeats_last(&file, real_line, col, msg) { return; }
        self.panic_mode = true;
        self.has_error = true;
        self.error_count += 1;
        let entry = format!("{}:{}: {}", line, col, msg);
        if self.err_msg.is_empty() {
            self.err_msg = entry;
        } else {
            self.err_msg = format!("{}\n{}", self.err_msg, entry);
        }
        self.diagnostics.push(
            Diagnostic::error(file, real_line, col, msg).with_end_col(end_col)
        );
    }

    /// Resolves a line to (file, source line) via the SourceMap; falls back
    /// to the raw line when no map is set (single-file parse).
    fn file_of_line_mapped(&self, line: usize) -> (String, usize) {
        match &self.source_map {
            Some(sm) if !sm.is_empty() => {
                let (file, real_line) = sm.locate(line);
                if !file.is_empty() {
                    (file, real_line as usize)
                } else {
                    (String::new(), line)
                }
            }
            _ => (String::new(), line),
        }
    }

    /// Skip to a recovery point after a parse error, so one bad declaration
    /// does not hide every error after it. Returns the token offset it stopped
    /// at, which the caller uses to guarantee forward progress.
    ///
    /// A recovery point is a token that can only start a new declaration or
    /// statement. Type keywords are included deliberately: without them a file
    /// like `int a = 1 \n int b = 2` would skip from the first error all the way
    /// to EOF (no `;` and no statement keyword ever appears) and every later
    /// error would stay hidden.
    fn synchronize(&mut self) -> usize {
        self.panic_mode = false;
        while self.current.kind != TokenKind::Eof {
            if self.previous.kind == TokenKind::Semicolon { return self.current.start; }
            if self.current.kind == TokenKind::Keyword {
                match self.current.keyword {
                    // Declaration / statement openers.
                    KeywordKind::AtInterface | KeywordKind::AtImplementation |
                    KeywordKind::AtProtocol | KeywordKind::AtEnd |
                    KeywordKind::AtEndNamespace |
                    KeywordKind::AtClass | KeywordKind::AtNamespace | KeywordKind::AtUsing |
                    KeywordKind::Return |
                    KeywordKind::If | KeywordKind::While | KeywordKind::For |
                    KeywordKind::Do | KeywordKind::Switch |
                    KeywordKind::Break | KeywordKind::Continue | KeywordKind::Else => return self.current.start,
                    // Type keywords: a fresh `int` / `struct` / typedef name means
                    // the previous declaration ended (with a missing `;`).
                    KeywordKind::Void | KeywordKind::Char | KeywordKind::Short |
                    KeywordKind::Int | KeywordKind::Long | KeywordKind::Float |
                    KeywordKind::Double | KeywordKind::Signed | KeywordKind::Unsigned |
                    KeywordKind::Bool | KeywordKind::Id | KeywordKind::Class |
                    KeywordKind::Sel | KeywordKind::Instancetype |
                    KeywordKind::Struct | KeywordKind::Union | KeywordKind::Enum |
                    KeywordKind::Typedef | KeywordKind::Static | KeywordKind::Extern |
                    KeywordKind::Const | KeywordKind::Volatile | KeywordKind::Register |
                    KeywordKind::Inline | KeywordKind::Import | KeywordKind::Include |
                    KeywordKind::Define => return self.current.start,
                    _ => {}
                }
            }
            if self.current.kind == TokenKind::Eof { return self.current.start; }
            self.advance();
        }
        self.current.start
    }

    fn add_type_name(&mut self, name: &str) {
        if !name.is_empty() && !self.type_names.iter().any(|n| n == name) {
            self.type_names.push(name.to_string());
        }
    }

    fn peek_colon_colon(&self) -> bool {
        // Check if the source after the current identifier has ::
        let start = self.current.start + self.current.length;
        let after = &self.source[start..];
        after.starts_with("::")
    }

    /// Lookahead for `for (Type var in collection)`: scan the source after the
    /// current token for a depth-0 `in` keyword before the matching `)` (and
    /// before any depth-0 `;`, which means a plain for header). Non-advancing:
    /// the caller parses the header normally after this returns true. `in` is
    /// a hard keyword, so a source-slice word match is unambiguous.
    fn scan_for_in_header(&self) -> bool {
        let bytes = self.source.as_bytes();
        let mut i = self.current.start;
        let mut depth: i32 = 0;
        while i < bytes.len() {
            let c = bytes[i] as char;
            match c {
                '(' => { depth += 1; i += 1; }
                ')' => {
                    if depth == 0 { return false; } // end of for header
                    depth -= 1;
                    i += 1;
                }
                ';' if depth == 0 => return false, // plain for header
                '"' => { // skip string literal
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' { i += 1; }
                        i += 1;
                    }
                    i += 1;
                }
                '\'' => { // skip char literal
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'\'' {
                        if bytes[i] == b'\\' { i += 1; }
                        i += 1;
                    }
                    i += 1;
                }
                c if c.is_alphabetic() || c == '_' => {
                    let word_start = i;
                    while i < bytes.len() {
                        let w = bytes[i] as char;
                        if w.is_alphanumeric() || w == '_' { i += 1; } else { break; }
                    }
                    // Contextual `in`: only a standalone word at depth 0
                    // counts. A `.` or `->` immediately before the word means
                    // member access (`for (i = p.in; ...)`), not a for-in
                    // connector — scan backwards over the just-read word.
                    let is_member = word_start > 0
                        && matches!(bytes[word_start - 1], b'.' | b'>');
                    if depth == 0 && !is_member && &self.source[word_start..i] == "in" {
                        return true;
                    }
                }
                _ => { i += 1; }
            }
        }
        false
    }

    fn is_type_name(&self, name: &str) -> bool {
        self.type_names.iter().any(|n| n == name)
    }

    /// Textual lookahead for the case-binding shape `Type [*] name :` —
    /// or `Type [*] name when <guard> :` — starting at the current token (an
    /// already-checked type-name identifier). Consumes nothing. Returns true
    /// only when the shape is confirmed, so the caller may commit to the Bind
    /// branch.
    fn scan_case_bind_shape(&self) -> bool {
        // Re-tokenize from the current token's offset: the parser has no
        // cheap multi-token peek (current/previous only), so scan a fresh
        // lexer over the remaining source. Bounded by the enclosing line's
        // worth of tokens — a `:` or `;` terminates.
        let rest = &self.source[self.current.start..];
        let mut lex = Lexer::new(rest);
        // tok[0] == type name (already known)
        lex.next_token();
        let mut tok = lex.next_token();
        if tok.kind == TokenKind::Star {
            tok = lex.next_token();
        }
        if tok.kind != TokenKind::Identifier {
            return false;
        }
        // Binding name parsed. A `when` guard (contextual keyword — a plain
        // identifier in the lexer) sits between the name and the arm's `:`;
        // skip the guard textually so the shape still confirms as a Bind.
        // Without this, `case T *x when <expr>:` fell through to the plain
        // constant path — and a switch mixing that with real Bind arms was
        // then rejected by the collector.
        let mut next = lex.next_token();
        if next.kind == TokenKind::Identifier && next.text(rest) == "when" {
            next = self.scan_past_guard(&mut lex);
        }
        next.kind == TokenKind::Colon
    }

    /// Drive `lex` past a `when` guard's tokens up to (and returning) the
    /// token that ends it: the arm's `:` at bracket depth 0, or a
    /// `;`/closing brace at depth 0. Depth-aware over (), [], {} so a
    /// bracketed subexpression may contain `:`. A ternary's `:` at depth 0
    /// ends the scan early — harmless, since `parse_case_pattern` re-parses
    /// the whole guard properly after the Bind branch is committed.
    fn scan_past_guard(&self, lex: &mut Lexer) -> Token {
        let mut depth: i32 = 0;
        loop {
            let t = lex.next_token();
            match t.kind {
                TokenKind::Eof => return t,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
                | TokenKind::AtArray | TokenKind::AtDict | TokenKind::AtLParen => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        return t;
                    }
                    depth -= 1;
                }
                TokenKind::Colon | TokenKind::Semicolon if depth == 0 => return t,
                _ => {}
            }
        }
    }

    /// Parse one case pattern after the `case` keyword:
    ///   - dangling comparison (`> 10`, `<= 5`, `> 0 && < 100`) → Cond
    ///   - declaration shape (`NSString *s`, `int x`) → Bind
    ///   - ObjC object literal (`@"x"`, `@1`, `@YES`, `@'c'`, `@(expr)`) → Const
    ///   - anything else → Const (plain constant; comma multi-value is
    ///     handled by the caller)
    /// Optionally followed by a `when <expr>` guard.
    fn parse_case_pattern(&mut self) -> (CstPattern, Option<Box<CstExpr>>) {
        let pattern = self.parse_case_pattern_head();
        // `when` guard — contextual keyword: only special right after a
        // pattern head; `when` stays a legal C identifier everywhere else.
        let guard = if self.current.kind == TokenKind::Identifier
            && self.current_text() == "when"
        {
            self.advance();
            self.parse_expression().map(Box::new)
        } else {
            None
        };
        (pattern, guard)
    }

    fn parse_case_pattern_head(&mut self) -> CstPattern {
        // ObjC object literals keep Const (value-equality decided at
        // lowering): @"..." / @1 / @YES / @'c' / @(expr).
        match self.current.kind {
            TokenKind::AtString | TokenKind::AtNumber | TokenKind::AtBool | TokenKind::AtChar | TokenKind::AtLParen => {
                if let Some(e) = self.parse_unary() {
                    return CstPattern::Const(Box::new(e));
                }
                return CstPattern::Const(Box::new(CstExpr {
                    kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0,
                    data: CstExprData::Integer(0),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
}));
            }
            // Dangling comparison: operator token starts the pattern; the
            // missing left operand is the switch subject (spliced at lowering).
            TokenKind::Greater | TokenKind::Less | TokenKind::Geq | TokenKind::Leq | TokenKind::Eq | TokenKind::Neq => {
                if let Some(e) = self.parse_case_cond() {
                    return CstPattern::Cond(Box::new(e));
                }
            }
            // Declaration shape `Type *name` (pointer) or `Type name`
            // (scalar) — C/ObjC-style binding. Committed only after a
            // textual lookahead confirms the `Type [*] name :` shape (same
            // textual-lookahead discipline as scan_cast_star_paren; the
            // lexer has no snapshot/restore, so we must not consume on a
            // failed guess — otherwise `case enumVal:` (plain constant)
            // would be half-consumed).
            TokenKind::Identifier => {
                let name = self.current_text().to_string();
                if self.is_type_name(&name) && self.scan_case_bind_shape() {
                    if let Some(ty) = self.parse_type_full() {
                        let is_ptr = self.match_token(TokenKind::Star);
                        if self.current.kind == TokenKind::Identifier {
                            let id = self.current_text().to_string();
                            self.advance();
                            return CstPattern::Bind { ty: Box::new(ty), name: id };
                        }
                        let _ = is_ptr;
                    }
                }
            }
            _ => {}
        }
        // Plain constant (default path). `case` was consumed, so a constant is
        // required: `case :` is a hole, and the synthesized 0 must never stand
        // in silently (the old fill made `case :` mean `case 0:`). The
        // synthesized node stays as the recovery fill.
        match self.required_expression(|p| p.parse_assignment()) {
            Some(e) => CstPattern::Const(Box::new(e)),
            None => CstPattern::Const(Box::new(CstExpr {
                kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0,
                data: CstExprData::Integer(0),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
})),
        }
    }

    /// Parse a dangling comparison chain: `> 10`, `> 0 && < 100`,
    /// `>= 5`, `== 3`. The subject appears as the missing left operand of
    /// each comparison; represented as a Binary with an empty Ident left
    /// side (spliced by the pattern crate).
    fn parse_case_cond(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_case_cond_atom()?;
        while self.current.kind == TokenKind::LogicalAnd {
            self.advance();
            let right = self.parse_case_cond_atom()?;
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: 17, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    /// One comparison arm: `<op> <rhs>`. Emits Binary { op, left: subject
    /// placeholder (empty Ident), right }.
    fn parse_case_cond_atom(&mut self) -> Option<CstExpr> {
        let op = match self.current.kind {
            TokenKind::Greater => 9,
            TokenKind::Less => 8,
            TokenKind::Geq => 11,
            TokenKind::Leq => 10,
            TokenKind::Eq => 12,
            TokenKind::Neq => 13,
            _ => return None,
        };
        let line = self.current.line;
        let col = self.current.column;
        self.advance();
        // rhs must NOT go through parse_assignment: its precedence chain
        // includes `&&`, and a failed sub-parse after match_token consumes
        // the operator irrecoverably (`0 && < 100` swallows the `&&`, then
        // dies on `<`). A dangling comparison's rhs is a shift-level
        // expression; the `&&` chaining between comparisons belongs to
        // parse_case_cond's loop.
        let rhs = self.parse_shift()?;
        Some(CstExpr {
            kind: CstExprKind::Binary, expr_type: None, line, col,
            data: CstExprData::Binary {
                op,
                left: Box::new(CstExpr {
                    kind: CstExprKind::Ident, expr_type: None, line, col,
                    data: CstExprData::Ident(String::new()),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
}),
                right: Box::new(rhs),
            },
        
            end_line: self.previous.end_line,
            end_col: self.previous.end_col,
})
    }

    /// Lookahead for the unambiguously-cast shape `( X * ... )`: from the
    /// source offset just past the identifier `X`, skip whitespace and
    /// qualifiers, require at least one `*`, and then require `)`.
    ///
    /// The `*` is *inside* the parentheses, which is what removes the ambiguity:
    /// the multiplication reading `(X) * y` needs the `*` after the closing
    /// paren, and a `*` cannot take `)` as its operand. Purely textual, so it
    /// also works for type names the parser has never heard of.
    fn scan_cast_star_paren(&self, from: usize) -> bool {
        let bytes = self.source.as_bytes();
        let mut i = from;
        let mut stars = 0usize;
        loop {
            if i >= bytes.len() {
                return false;
            }
            let c = bytes[i];
            if c.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            if c == b'*' {
                stars += 1;
                i += 1;
                continue;
            }
            if (c as char).is_ascii_alphabetic() {
                // `(char * const)` — qualifiers may sit between the stars and `)`.
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_alphanumeric() {
                    i += 1;
                }
                let word = &self.source[start..i];
                if stars > 0
                    && matches!(word, "const" | "volatile" | "restrict" | "__restrict" | "__restrict__")
                {
                    continue;
                }
                return false;
            }
            return stars > 0 && c == b')';
        }
    }

    fn add_type_param(&mut self, name: &str) {
        if !name.is_empty() && !self.type_params.iter().any(|n| n == name) {
            self.type_params.push(name.to_string());
        }
    }

    fn is_type_param(&self, name: &str) -> bool {
        self.type_params.iter().any(|n| n == name)
    }

    fn is_generic_class(&self, name: &str) -> bool {
        self.generic_class_names.iter().any(|n| n == name)
    }

    // ─── Type parsing ─────────────────────────────────────────────────────

    /// Parse a type in a position that may carry a nullability annotation
    /// (method return type, method/function parameter, ivar).
    ///
    /// Sets `annotating` for the duration so `parse_type_name`'s qualifier loop
    /// recognizes `nullable` / `nonnull` as annotations. The flag is restored
    /// even on the early-return path, so a variable declaration nested inside
    /// (impossible today, but cheap to keep true) cannot leak the mode.
    ///
    /// Also applies the `NP_ASSUME_NONNULL` region default: inside the region an
    /// *unannotated pointer* becomes `Nonnull`. Applied here rather than in
    /// `parse_type_name` so the region can never annotate a non-pointer, and so
    /// an explicit `_Null_unspecified` opt-out survives the default.
    fn parse_type_annotated(&mut self) -> Option<CstType> {
        let saved = self.annotating;
        self.annotating = true;
        let at_line = self.current.line;
        let mut t = self.parse_type_full();
        self.annotating = saved;
        if self.region_applies_at(at_line) {
            if let Some(tt) = t.as_mut() {
                let pointerish = tt.is_pointer
                    || matches!(tt.prim, TypePrim::Id | TypePrim::Instancetype);
                if pointerish && tt.nulls == Nullability::Unspecified {
                    tt.nulls = Nullability::Nonnull;
                }
            }
        }
        t
    }

    /// The nullability annotation the current token spells, if any. Does NOT
    /// consume the token.
    ///
    /// Recognizes both spellings ObjC accepts, on purpose: the bare
    /// `nullable` / `nonnull` (which ObjC gets from macros expanding to
    /// `_Nullable` / `_Nonnull`) and the underscore forms. They mean the same
    /// thing, so accepting both costs nothing and lets C headers written for
    /// clang be used verbatim.
    ///
    /// Deliberately NOT in `KW_TABLE` — a hard keyword would make
    /// `int nullable = 5;` a parse error, breaking the C-superset rule
    /// (AGENTS.md). Identifiers keep working everywhere else; only the
    /// `annotating` positions consult this.
    fn nullability_prefix(&mut self) -> Option<Nullability> {
        if self.current.kind != TokenKind::Identifier {
            return None;
        }
        let n = match self.current_text() {
            "nullable" => Nullability::Nullable,
            "nonnull" => Nullability::Nonnull,
            "_Nullable" => Nullability::Nullable,
            "_Nonnull" => Nullability::Nonnull,
            "_Null_unspecified" => Nullability::NullUnspecified,
            _ => return None,
        };
        // The underscore spellings are clang RESERVED WORDS — they can never be a
        // declarator name, so they skip the ambiguity guard entirely. Without
        // this exemption the guard sees `_Nullable s)` (postfix annotation in a
        // block/function parameter list) and misreads it as "type named
        // `_Nullable`, declarator `s`", silently dropping the annotation.
        if matches!(
            self.current_text(),
            "_Nullable" | "_Nonnull" | "_Null_unspecified"
        ) {
            return Some(n);
        }
        // Guard: `nullable` / `nonnull` are ordinary identifiers, so the word is a
        // NAME — not an annotation — in either of the two positions a C
        // declarator can put one:
        //   1. declarator position:  `int nullable = 5;`   (word then `=`)
        //   2. type position:         `nullable foo = 5;`   (word, name, then `=`)
        // Both are legal C and must keep working (the C-superset rule).
        //
        // Shape only — deliberately NOT consulting the type table, because a
        // type may be declared LATER in the file (forward reference) and
        // requiring `is_type_name` would silently drop annotations for those.
        //
        // `)` is intentionally NOT a "name follows" signal by itself: the
        // property attribute form `@property (nullable) T *x` needs the word
        // followed by `)` to still read as an annotation, and that position has
        // no ambiguity (nothing can be declared inside the parens). It IS used
        // in the two-token form 2 check below, where a name really did precede.
        //
        // `previous` is saved and restored as well — `advance()` overwrites it,
        // and leaving it clobbered corrupts every later `previous_text()`
        // caller (declarator names, `@selector`, …).
        let saved_lex = self.lexer.save_pos();
        let saved_cur = self.current.clone();
        let saved_prev = self.previous.clone();
        self.advance();
        let decl_pos = matches!(
            self.current.kind,
            TokenKind::Assign | TokenKind::Semicolon | TokenKind::Comma
        );
        // Form 2 needs one more token of lookahead: `nullable foo = 5;`.
        let type_then_name = !decl_pos
            && matches!(self.current.kind, TokenKind::Identifier | TokenKind::Keyword)
            && {
                self.advance();
                matches!(
                    self.current.kind,
                    TokenKind::Assign | TokenKind::Semicolon | TokenKind::Comma | TokenKind::RParen
                )
            };
        self.lexer.restore_pos(saved_lex);
        self.current = saved_cur;
        self.previous = saved_prev;
        if decl_pos || type_then_name { None } else { Some(n) }
    }

    fn parse_type_name(&mut self) -> Option<CstType> {
        let mut t = CstType::new(TypePrim::Void);

        while {
            if self.match_keyword(KeywordKind::Const) { t.is_const = true; true }
            else if self.match_keyword(KeywordKind::Volatile) { t.is_volatile = true; true }
            else if self.match_keyword(KeywordKind::Static) { true }
            else if self.match_keyword(KeywordKind::Extern) { true }
            else if self.match_keyword(KeywordKind::Weak) { t.is_weak_qual = true; true }
            else if self.match_keyword(KeywordKind::Block) { t.is_block_qual = true; true }
            else if self.annotating {
                // Only in an annotation position (method return type, method or
                // function parameter, ivar) — see `parse_type_annotated`. Those
                // positions unambiguously expect a type, so no lookahead is
                // needed, which is exactly what keeps `int nullable = 5;`
                // (a variable NAMED nullable) compiling. See the C-superset
                // rule in AGENTS.md; the same reasoning retires ObjC's
                // macro-based spelling without giving up its syntax.
                if let Some(n) = self.nullability_prefix() {
                    if self.ivar_position {
                        // Consume + record anyway: one targeted error, the
                        // declaration still parses (the annotation keeps its
                        // meaning for the checker), and no cascade from a
                        // dangling token. `error_at` on the current token —
                        // plain `error()` would point at `previous` (the
                        // section's `{`), not the annotation word.
                        let tok = self.current.clone();
                        self.error_at(
                            &tok,
                            "nullability annotations in ivar declarations must follow the pointer (NPString * _Nullable x) — the prefix form is only legal in method and property positions");
                    }
                    self.advance();
                    t.nulls = n;
                    true
                } else { false }
            }
            else { false }
        } {}

        if self.match_keyword(KeywordKind::Unsigned) { t.prim = TypePrim::Unsigned; t.is_unsigned = true; }
        else if self.match_keyword(KeywordKind::Signed) { t.prim = TypePrim::Signed; }

        if self.match_keyword(KeywordKind::Long) {
            if self.match_keyword(KeywordKind::Long) { t.prim = TypePrim::LongLong; }
            else if t.prim == TypePrim::Void || t.prim == TypePrim::Named { t.prim = TypePrim::Long; }
            else { t.prim = TypePrim::Long; }
        }
        if self.match_keyword(KeywordKind::Short) { t.prim = TypePrim::Short; }

        if t.prim == TypePrim::Signed || t.prim == TypePrim::Unsigned {
            if self.match_keyword(KeywordKind::Char) { t.prim = TypePrim::Char; }
            else if self.match_keyword(KeywordKind::Short) { t.prim = TypePrim::Short; }
            else if self.match_keyword(KeywordKind::Int) { t.prim = TypePrim::Int; }
            else if self.match_keyword(KeywordKind::Long) {
                t.prim = TypePrim::Long;
                if self.match_keyword(KeywordKind::Long) { t.prim = TypePrim::LongLong; }
            }
        } else if t.prim == TypePrim::Long || t.prim == TypePrim::LongLong {
            if self.match_keyword(KeywordKind::Int) {}
        } else if t.prim == TypePrim::Short {
            if self.match_keyword(KeywordKind::Int) {}
        }

        if t.prim != TypePrim::Signed && t.prim != TypePrim::Unsigned
            && t.prim != TypePrim::Long && t.prim != TypePrim::LongLong && t.prim != TypePrim::Short
            && t.prim != TypePrim::Char && t.prim != TypePrim::Int
            && t.prim != TypePrim::Float && t.prim != TypePrim::Double && t.prim != TypePrim::Bool
            && t.prim != TypePrim::Id && t.prim != TypePrim::Class && t.prim != TypePrim::Sel
            && t.prim != TypePrim::Instancetype
        {
            if self.match_keyword(KeywordKind::Void) { t.prim = TypePrim::Void; }
            else if self.match_keyword(KeywordKind::Char) { t.prim = TypePrim::Char; }
            else if self.match_keyword(KeywordKind::Int) { t.prim = TypePrim::Int; }
            else if self.match_keyword(KeywordKind::Float) { t.prim = TypePrim::Float; }
            else if self.match_keyword(KeywordKind::Double) { t.prim = TypePrim::Double; }
            else if self.match_keyword(KeywordKind::Bool) { t.prim = TypePrim::Bool; }
            else if self.match_keyword(KeywordKind::Id) { t.prim = TypePrim::Id; }
            else if self.match_keyword(KeywordKind::Class) { t.prim = TypePrim::Class; }
            else if self.match_keyword(KeywordKind::Sel) { t.prim = TypePrim::Sel; }
            else if self.match_keyword(KeywordKind::Instancetype) { t.prim = TypePrim::Instancetype; }
            else if self.match_keyword(KeywordKind::Struct) || self.match_keyword(KeywordKind::Union) || self.match_keyword(KeywordKind::Enum) {
                // match_keyword already consumed the tag keyword, so it is
                // `self.previous` now (not `self.current`, which is the tag
                // identifier). Read it before advancing over the name.
                let tag = match self.previous.keyword {
                    KeywordKind::Union => TagKind::Union,
                    KeywordKind::Enum => TagKind::Enum,
                    _ => TagKind::Struct,
                };
                if self.current.kind == TokenKind::Identifier {
                    self.advance();
                    t.prim = TypePrim::Named;
                    t.is_struct = true;
                    // `enum Mode` and `struct Mode` are distinct, incompatible C
                    // types, so codegen must echo back the keyword the source used.
                    t.tag = tag;
                    t.name = Some(self.previous_text().to_string());
                } else if tag != TagKind::Enum && self.check(TokenKind::LBrace) {
                    // Anonymous inline definition in type position:
                    // `struct { … } name` / `union { … } name` (C99 6.7.2.1) —
                    // the generated runtime emits this for its intern table.
                    // Consume the body (so parsing stays in sync) and keep the
                    // raw text so codegen can re-emit the layout verbatim.
                    let body_start = self.current.start;
                    if self.parse_struct_body(tag == TagKind::Union).is_some() {
                        let body_end = self.previous.start + self.previous.length;
                        if body_end >= body_start && body_end <= self.source.len() {
                            t.prim = TypePrim::Named;
                            t.is_struct = true;
                            t.tag = tag;
                            t.name = None;
                            t.anon_struct_body = Some(self.source[body_start..body_end].to_string());
                        }
                    }
                }
            }
else if self.match_keyword(KeywordKind::Typeof) {
                // `typeof ( expr )` / `typeof ( type )` — capture raw text so it
                // passes through to C verbatim (e.g. `__typeof__(x) z = 2;`).
                let start = self.previous.start;
                self.consume(TokenKind::LParen, "expected '(' after 'typeof'");
                let mut depth = 1;
                while depth > 0 {
                    if self.current.kind == TokenKind::Eof { break; }
                    if self.current.kind == TokenKind::LParen { depth += 1; }
                    else if self.current.kind == TokenKind::RParen {
                        depth -= 1;
                        if depth == 0 { break; }
                    }
                    self.advance();
                }
                let end = self.current.start + self.current.length;
                self.consume(TokenKind::RParen, "expected ')'");
                let raw = &self.source[start..end];
                t.prim = TypePrim::Named;
                t.name = Some(raw.to_string());
            }
            else if self.current.kind == TokenKind::Identifier {
                let tname = self.current_text().to_string();
                if self.is_type_param(&tname) {
                    self.advance();
                    t.prim = TypePrim::Param;
                    t.name = Some(tname);
                } else {
                    t.prim = TypePrim::Named;
                    t.name = self.parse_qualified_name();
                    if t.name.is_none() { return None; }
                }
            } else {
                return None;
            }
        }

        // C99 `_Complex` (§6.2.5p13): `float _Complex x`, `double _Complex y`,
        // `float _Complex`. Sits after the base type specifier, so it is
        // consumed here — outside the base-type branch above (prim is already
        // Float/Double by then). Pure passthrough flag; codegen echoes it.
        if self.match_keyword(KeywordKind::Complex) {
            t.is_complex = true;
        }

        Some(t)
    }

    fn parse_type_full(&mut self) -> Option<CstType> {
        let mut t = self.parse_type_name()?;

        // Protocol qualifiers or generic type args: <...>
        if matches!(t.prim, TypePrim::Id | TypePrim::Named | TypePrim::Class | TypePrim::Instancetype) {
            if self.match_token(TokenKind::Less) {
                let mut is_protocol = false;
                if self.current.kind == TokenKind::Identifier {
                    let mut is_generic = false;
                    if t.prim == TypePrim::Id {
                        is_generic = false; // id<P> always protocols
                    } else if let Some(ref name) = t.name {
                        // Check both the fully qualified name and the simple name
                        // (e.g. "System::IO::Buffer" should match registered "Buffer")
                        is_generic = self.is_generic_class(name)
                            || name.rsplit("::").next().map_or(false, |s| self.is_generic_class(s));
                    }
                    if !is_generic {
                        // Protocols path: <Proto1, Proto2> and intersection
                        // <P & Q> — `&` separates protocols that must ALL be
                        // conformed to, landing in the same flat list
                        // (conjunction semantics; checker requires every one).
                        //
                        // Speculative, so it MUST rewind on failure: the
                        // generic-args path below re-reads the same tokens.
                        // `NPArray<NPString *> *a` is not a protocol list (the
                        // `*` fails the `>` lookahead); without a rewind the
                        // args path would start at `*`, produce no args, and
                        // silently erase `<NPString *>`.
                        let saved_lex = self.lexer.save_pos();
                        let saved_tok = self.current.clone();
                        let mut protocols = Vec::new();
                        let mut protocol_ok = true;
                        loop {
                            if self.current.kind != TokenKind::Identifier {
                                protocol_ok = false;
                                break;
                            }
                            protocols.push(self.current_text().to_string());
                            self.advance();
                            if !(self.match_token(TokenKind::Comma) || self.match_token(TokenKind::Ampersand)) { break; }
                        }
                        if protocol_ok && self.current.kind == TokenKind::Greater {
                            self.advance();
                            if t.prim == TypePrim::Id || t.prim == TypePrim::Named ||
                               t.prim == TypePrim::Class || t.prim == TypePrim::Instancetype {
                                t.protocols = protocols;
                                is_protocol = true;
                            }
                        } else {
                            self.lexer.restore_pos(saved_lex);
                            self.current = saved_tok;
                        }
                    }
                }
                if !is_protocol {
                    let mut type_args = Vec::new();
                    loop {
                        if let Some(arg) = self.parse_type_full() {
                            type_args.push(arg);
                        } else {
                            while self.current.kind != TokenKind::Eof &&
                                  self.current.kind != TokenKind::Comma &&
                                  self.current.kind != TokenKind::Greater {
                                self.advance();
                            }
                            if self.current.kind == TokenKind::Eof { break; }
                        }
                        if !self.match_token(TokenKind::Comma) { break; }
                    }
                    if self.current.kind == TokenKind::Greater {
                        self.advance();
                    } else {
                        self.error("expected '>'");
                        while self.current.kind != TokenKind::Eof &&
                              self.current.kind != TokenKind::Greater {
                            self.advance();
                        }
                        if self.current.kind == TokenKind::Greater { self.advance(); }
                    }
                    t.type_args = type_args;
                }
            }
        }

        // C17 6.7.2: a qualifier BETWEEN the base type and the declarator
        // stars (`int const *p`, `int const w`) qualifies the BASE type —
        // it never names a variable. `const`/`volatile` are contextual
        // keywords (legal variable names by the C-superset rule), so they
        // must be consumed HERE, before the star loop misreads them as the
        // declarator name and the whole declaration fell out.
        while {
            let mut ate = false;
            if self.match_keyword(KeywordKind::Const) { t.is_const = true; ate = true; }
            if self.match_keyword(KeywordKind::Volatile) { t.is_volatile = true; ate = true; }
            ate
        } {}

        // A prefix annotation (`nullable NPString *s`) parsed onto the base by
        // the qualifier loop is lifted off here and re-applied after the star
        // loop. Leaving it on the base would mark the INNER pointer level.
        let prefix_nulls = t.nulls;
        t.nulls = Nullability::Unspecified;
        let mut star_count = 0usize;

        // Pointer *
        while self.match_token(TokenKind::Star) {
            star_count += 1;
            let mut ptr = CstType::new(t.prim);
            ptr.is_pointer = true;
            ptr.name = t.name.clone();
            ptr.is_struct = t.is_struct;
            ptr.protocols = std::mem::take(&mut t.protocols);
            ptr.type_args = std::mem::take(&mut t.type_args);
            ptr.subtype = Some(Box::new(t));
            t = ptr;
            // Postfix annotation directly after this star annotates THIS level:
            // `NPError * _Nullable *` marks the middle pointer, while `NPError *
            // * _Nullable` and the plain one-star `NPString * _Nonnull` mark the
            // outermost (the last star has just been wrapped). That is C's
            // declarator reading order. The check must live INSIDE the loop —
            // after it, the interleaved `* _Nullable *` form would leave the
            // second star unconsumed. The `_`-spelled words are clang reserved
            // words so the ambiguity guard always accepts them here; a bare
            // `nullable` before a declarator name is rejected by the same guard.
            if self.annotating {
                if let Some(n) = self.nullability_prefix() {
                    self.advance();
                    t.nulls = n;
                }
            }
            // C17 6.7 declarator-position qualifiers: `int *const cp` — a
            // `const` (or `volatile`) right after a star qualifies THAT
            // pointer level, not a variable named `const`. Without this
            // consume the contextual-keyword name rule read the qualifier as
            // the declarator name and the whole declaration fell out (the
            // variable ended up undeclared and the C emission lost it).
            while {
                let mut ate = false;
                if self.match_keyword(KeywordKind::Const) { t.is_const = true; ate = true; }
                if self.match_keyword(KeywordKind::Volatile) { t.is_volatile = true; ate = true; }
                ate
            } {}
        }

        // A prefix annotation with a SINGLE star lands on that (outermost)
        // pointer. With multiple stars clang REJECTS the form outright —
        // verified: `void g(_Nonnull Widget * * out)` → "nullability specifier
        // '_Nonnull' cannot be applied to non-pointer type 'Widget'" — because
        // it refuses to guess which level was meant. Match that, and point at
        // the postfix spelling that annotates a chosen level.
        if prefix_nulls != Nullability::Unspecified {
            if star_count > 1 {
                let tn = t.name.clone().unwrap_or_else(|| "id".into());
                self.error(&format!(
                    "nullability specifier cannot be applied to non-pointer type '{}' — for a multi-level pointer, annotate the level you mean with the postfix spelling (NPError * _Nullable *)",
                    tn));
            } else if t.nulls == Nullability::Unspecified {
                t.nulls = prefix_nulls;
            }
        }

        // Block type: T (^)(params) or T (^name)(params)
        if self.match_token(TokenKind::LParen) {
            if self.match_token(TokenKind::Caret) {
                let mut bt = CstType::new(t.prim);
                bt.is_block = true;
                bt.nulls = t.nulls;   // same carry-up as the pointer case above
                bt.subtype = Some(Box::new(t));
                // `T (^ *name)(params)` — pointer to block: clang accepts the
                // `*` between the caret paren and the optional name. Without
                // this the `*` was left unconsumed, which both lost the
                // pointer level and deadlocked the ivar loop (failed consume
                // → zero-progress, probe-verified).
                while self.match_token(TokenKind::Star) {
                    bt.is_pointer = true;
                }
                // is_name_token, not a bare Identifier: generated-C vtable
                // members are named `copy`/`retain` — contextual keywords that
                // degrade to identifiers in every name position.
                if self.is_name_token() {
                    bt.block_name = Some(self.current_text().to_string());
                    self.advance();
                }
                self.consume(TokenKind::RParen, "expected ')'");
                if self.match_token(TokenKind::LParen) {
                    let mut params: Vec<CstType> = Vec::new();
                    while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                if let Some(ptype) = self.parse_type_annotated() {
                    if self.is_name_token() {
                        self.advance();
                    }
                    params.push(ptype);
                } else {
                    self.advance();
                }
                if !self.match_token(TokenKind::Comma) { break; }
            }
            // Link params via next
                    let mut head = None;
                    let mut tail: &mut Option<Box<CstType>> = &mut head;
                    for p in params {
                        let boxed = Box::new(p);
                        tail = &mut tail.insert(boxed).next;
                    }
                    bt.block_params = head;
                    self.consume(TokenKind::RParen, "expected ')'");
                }
                t = bt;
            } else if self.match_token(TokenKind::Star) {
                // Function pointer: T (*)(params) or T (*name)(params)
                let mut ft = CstType::new(t.prim);
                ft.is_fn_ptr = true;
                ft.is_pointer = true;
                ft.subtype = Some(Box::new(t));
                // is_name_token: `NPObject * (*copy)(NPObject *, SEL);` — the
                // vtable's fn-ptr members named after contextual keywords.
                if self.is_name_token() {
                    ft.block_name = Some(self.current_text().to_string());
                    self.advance();
                    // Function pointer ARRAY: T (*name[N])(params) — a
                    // declarator with an array suffix between the `)` and the
                    // parameter list (e.g. `int (*row[4])(int)`).
                    if self.match_token(TokenKind::LBracket) {
                        ft.is_array = true;
                        if self.check(TokenKind::RBracket) {
                            ft.array_size = 0;
                        } else if let Some(num) = self.current_text().parse::<i32>().ok() {
                            ft.array_size = num;
                            self.advance();
                        } else {
                            ft.array_size_name = Some(self.current_text().to_string());
                            self.advance();
                        }
                        self.consume(TokenKind::RBracket, "expected ']'");
                    }
                }
                self.consume(TokenKind::RParen, "expected ')'");
                if self.match_token(TokenKind::LParen) {
                    let mut params: Vec<CstType> = Vec::new();
                    while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                        // `int (*fp)(int, ...)` — the trailing ellipsis belongs
                        // to the TYPE: record it so the emitted cast stays
                        // signature-exact (AGENTS.md iron law 6).
                        if self.check(TokenKind::Ellipsis) {
                            ft.is_variadic = true;
                            self.advance();
                            break;
                        }
                        if let Some(ptype) = self.parse_type_full() {
                            // Optional parameter name. `self` is keyword-ized by
                            // the lexer, so a parameter named `self` used to be
                            // left unconsumed here, breaking the rest of the
                            // parameter list (and producing a misleading error).
                            // is_name_token covers it plus the other keywords
                            // that degrade to identifiers in name positions.
                            if self.is_name_token() {
                                self.advance();
                            }
                            params.push(ptype);
                        } else {
                            self.advance();
                        }
                        if !self.match_token(TokenKind::Comma) { break; }
                    }
                    let mut head = None;
                    let mut tail: &mut Option<Box<CstType>> = &mut head;
                    for p in params {
                        let boxed = Box::new(p);
                        tail = &mut tail.insert(boxed).next;
                    }
                    ft.block_params = head;
                    self.consume(TokenKind::RParen, "expected ')'");
                }
                t = ft;
            } else {
                self.consume(TokenKind::RParen, "expected ')'");
            }
        }

        // C99 6.7.2.1 array suffix in type position: `T[]`, `T[3]`, `T *[]`.
        // The generated-C protocol metadata emits compound literals of array
        // type — `(NPProtocolMethod[]){…}` and `(struct NPProtocol *[]){…}` —
        // where the suffix sits AFTER the base type / pointer stars, so it is
        // consumed here. Declarator-level arrays (`name[size]`) are handled by
        // the declaration paths, which see a NAME (not `[`) after the type, so
        // this never over-consumes a declaration.
        while self.match_token(TokenKind::LBracket) {
            let mut arr = CstType::new(TypePrim::Named);
            arr.subtype = Some(Box::new(t));
            arr.is_array = true;
            if self.current.kind == TokenKind::Integer {
                arr.array_size = self.current_text().parse().unwrap_or(0);
                self.advance();
            } else if self.current.kind == TokenKind::Identifier {
                arr.array_size_name = Some(self.current_text().to_string());
                self.advance();
            }
            self.consume(TokenKind::RBracket, "expected ']'");
            t = arr;
        }

        Some(t)
    }

    // ─── Qualified name parsing ──────────────────────────────────────────

    fn parse_qualified_name(&mut self) -> Option<String> {
        if self.current.kind != TokenKind::Identifier && self.current.kind != TokenKind::Keyword {
            return None;
        }
        let mut name = String::new();
        if self.match_name() {
            name.push_str(self.previous_text());
        } else {
            return None;
        }
        while self.match_token(TokenKind::ColonColon) {
            name.push_str("::");
            if self.match_name() {
                name.push_str(self.previous_text());
            } else {
                break;
            }
        }
        Some(name)
    }

    // Render a CstType back into a fully-qualified source-level type string,
    // including protocol qualifiers (`id<P>`) and generic type arguments
    // (`Name<T*>`). Used by `@using Alias = FQN` so aliases can target
    // protocol-qualified ids and generic instantiations.
    fn type_to_fqn(t: &CstType) -> String {
        let mut s = String::new();
        match t.prim {
            TypePrim::Void => s.push_str("void"),
            TypePrim::Char => s.push_str("char"),
            TypePrim::Short => s.push_str("short"),
            TypePrim::Int => s.push_str("int"),
            TypePrim::Long => s.push_str("long"),
            TypePrim::LongLong => s.push_str("long long"),
            TypePrim::Float => s.push_str("float"),
            TypePrim::Double => s.push_str("double"),
            TypePrim::Bool => s.push_str("_Bool"),
            TypePrim::Signed => s.push_str("signed"),
            TypePrim::Unsigned => s.push_str("unsigned"),
            TypePrim::Id => s.push_str("id"),
            TypePrim::Class => s.push_str("Class"),
            TypePrim::Sel => s.push_str("SEL"),
            TypePrim::Instancetype => s.push_str("instancetype"),
            TypePrim::Named | TypePrim::Param => {
                if let Some(ref n) = t.name {
                    s.push_str(n);
                }
            }
        }
        if !t.protocols.is_empty() {
            s.push('<');
            for (i, p) in t.protocols.iter().enumerate() {
                if i > 0 { s.push_str(", "); }
                s.push_str(p);
            }
            s.push('>');
        }
        if !t.type_args.is_empty() {
            s.push('<');
            for (i, a) in t.type_args.iter().enumerate() {
                if i > 0 { s.push_str(", "); }
                s.push_str(&Self::type_to_fqn(a));
            }
            s.push('>');
        }
        if t.is_pointer { s.push('*'); }
        s
    }

    fn parse_qualified_name_from(&mut self, receiver: &CstExpr) -> Option<String> {
        let mut name = String::new();
        if let CstExprData::Ident(ref ident) = receiver.data {
            name.push_str(ident);
        } else {
            return None;
        }
        while self.match_token(TokenKind::ColonColon) {
            name.push_str("::");
            if self.match_name() {
                name.push_str(self.previous_text());
            } else {
                break;
            }
        }
        Some(name)
    }

    // ─── Expression parsing ──────────────────────────────────────────────

    /// Desugar a boxing literal (`@123`, `@YES`, `@'c'`) into a normal
    /// `[NPNumber <selector> <arg>]` class-method send, so the whole
    /// downstream chain (binder resolution, checker, vtable dispatch) is the
    /// same one a hand-written message send goes through.
    fn mk_npnumber_send(&self, selector: &str, arg: CstExpr, line: usize, col: usize) -> CstExpr {
        CstExpr {
            kind: CstExprKind::MessageSend, expr_type: None, line, col,
            end_line: line, end_col: col,
            data: CstExprData::Message {
                receiver: Box::new(CstExpr {
                    kind: CstExprKind::Ident, expr_type: None, line, col,
                    end_line: line, end_col: col,
                    data: CstExprData::Ident("NPNumber".into()),
                }),
                selector: selector.to_string(),
                args: vec![arg],
            },
        }
    }
    /// Parse an integer literal token's text with C's radix rules: `0x`/`0X`
    /// hex, leading-`0` octal, decimal otherwise. Shared by primary-expression
    /// parsing and enum values — the enum path used to call `str::parse`,
    /// which rejects every `0x` literal, and its `unwrap_or` fallback then
    /// silently substituted the auto-increment value.
    fn parse_int_literal_text(text: &str) -> i64 {
        if text.len() > 2 && (text.starts_with("0x") || text.starts_with("0X")) {
            i64::from_str_radix(&text[2..], 16).unwrap_or_else(|_| u64::from_str_radix(&text[2..], 16).unwrap_or(0) as i64)
        } else if text.len() > 1 && text.starts_with('0') && text[1..].bytes().all(|c| (b'0'..=b'7').contains(&c)) {
            i64::from_str_radix(&text[1..], 8).unwrap_or(0)
        } else {
            text.parse::<i64>().unwrap_or_else(|_| text.parse::<u64>().unwrap_or(0) as i64)
        }
    }

    /// Fold an enumerator's constant expression to an integer.
    ///
    /// C requires enumerator values to be integer constant expressions, and the
    /// parser stores every member as its folded value — so anything that cannot
    /// be folded must be *reported*, never quietly replaced by the
    /// auto-increment value (that silent substitution is how
    /// `enum { A = 1, B = 0x10, C }` used to compile to `A=1, B=2, C=3`).
    ///
    /// Covered: integer/char/bool literals, earlier enumerators of the *same*
    /// enum (via `seen`), unary `+ - ~ !`, the binary arithmetic / shift /
    /// relational / equality / bitwise / logical operators, the conditional
    /// operator, and casts/parentheses over any of those. Anything else — a
    /// `sizeof`, an identifier from outside this enum, a string — is `None`.
    fn fold_enum_const(e: &CstExpr, seen: &std::collections::HashMap<String, i64>) -> Option<i64> {
        match &e.data {
            CstExprData::Integer(v) => Some(*v),
            CstExprData::Char(c) => Some(*c as i64),
            CstExprData::Bool(b) => Some(if *b { 1 } else { 0 }),
            CstExprData::Ident(name) => seen.get(name).copied(),
            CstExprData::Paren(inner) => Self::fold_enum_const(inner, seen),
            CstExprData::Cast { expr, .. } => Self::fold_enum_const(expr, seen),
            CstExprData::Unary { op, operand, is_postfix } => {
                if *is_postfix {
                    return None; // `x++` is not a constant expression
                }
                let v = Self::fold_enum_const(operand, seen)?;
                match *op {
                    5 => Some(v.wrapping_neg()), // -
                    6 => Some(v),                // +
                    7 => Some(!v),               // ~
                    8 => Some(if v == 0 { 1 } else { 0 }), // !
                    _ => None,                   // ++/--/*/& are not constant
                }
            }
            CstExprData::Binary { op, left, right } => {
                let l = Self::fold_enum_const(left, seen)?;
                let r = Self::fold_enum_const(right, seen)?;
                match *op {
                    1 => Some(l.wrapping_mul(r)),
                    2 => if r == 0 { None } else { Some(l.wrapping_div(r)) },
                    3 => if r == 0 { None } else { Some(l.wrapping_rem(r)) },
                    4 => Some(l.wrapping_add(r)),
                    5 => Some(l.wrapping_sub(r)),
                    6 => Some(l.wrapping_shl(r as u32)),
                    7 => Some(l.wrapping_shr(r as u32)),
                    8 => Some(if l < r { 1 } else { 0 }),
                    9 => Some(if l > r { 1 } else { 0 }),
                    10 => Some(if l <= r { 1 } else { 0 }),
                    11 => Some(if l >= r { 1 } else { 0 }),
                    12 => Some(if l == r { 1 } else { 0 }),
                    13 => Some(if l != r { 1 } else { 0 }),
                    14 => Some(l & r),
                    15 => Some(l ^ r),
                    16 => Some(l | r),
                    17 => Some(if l != 0 && r != 0 { 1 } else { 0 }),
                    18 => Some(if l != 0 || r != 0 { 1 } else { 0 }),
                    _ => None,
                }
            }
            CstExprData::Ternary { cond, true_expr, false_expr } => {
                let c = Self::fold_enum_const(cond, seen)?;
                if c != 0 {
                    Self::fold_enum_const(true_expr, seen)
                } else {
                    Self::fold_enum_const(false_expr, seen)
                }
            }
            _ => None,
        }
    }

    fn parse_primary(&mut self) -> Option<CstExpr> {
        let (pri_line, pri_col) = (self.current.line, self.current.column);
        // Soft keyword `await` — suspension-point operator
        // (docs/plans/async_soft_keywords_plan.md §3). Operator only in
        // expression-prefix position followed by a unary-start token
        // (at_await_operator judgment table §3.4); otherwise plain C
        // identifier. Replaces the removed `@await` machinery token.
        if self.at_await_operator() {
            // In-message bare-`await` ban (2026-10-13 decision): inside a
            // message send, `await` must be parenthesized — a receiver is
            // `[(await [x y]) foo]`, an argument is `[self fetch:(await [x y])]`.
            // Unparenthesized, `await [x y] foo` reads like Swift's
            // await-over-the-whole-chain, so we ban it outright instead of
            // inventing a precedence. `(await …)` sits one paren deeper and
            // never trips this; `await` in SELECTOR position (`[task await]`)
            // is eaten by the selector loop and never reaches this arm; C
            // superset holds because this fires only when the operator
            // judgment table (§3.4) already classified the token as the
            // await operator — a plain `await` identifier elsewhere is
            // untouched. Reported once per message to avoid repeat noise.
            let mut bare_in_msg = false;
            if let Some(top) = self.msg_await_stack.last_mut() {
                if top.0 == self.expr_paren_depth && !top.1 {
                    top.1 = true;
                    bare_in_msg = true;
                }
            }
            if bare_in_msg {
                self.error("bare 'await' inside a message send must be parenthesized: write [(await [x y]) foo] for a receiver or [self fetch:(await [x y])] for an argument");
            }
            self.advance(); // consume `await`
            let inner = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Await, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Await { inner: Box::new(inner) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        // Async block literal (§5.1): `async ^…` — the flag sits BEFORE `^`,
        // and the block layout is exactly the plain block's (`^` Return-type?
        // Params? Body). The modifier lives OUTSIDE the block structure, so a
        // user typedef named `async` still works as a `^async(...)` return
        // type — the two never share a slot. Jeti extension syntax: `async`
        // followed by `^` is always the async-block start (no XOR fallback —
        // a variable named `async` combined with `^` is not supported). The
        // removed `async T ^…` layout (return type between flag and `^`)
        // reports a migration error and is recovered as-is so diagnostics
        // don't cascade.
        if self.current.kind == TokenKind::Identifier
            && self.current_text() == "async"
            && self.at_async_modifier()
        {
            if !self.at_async_block_start() {
                self.error("async block syntax changed: the return type now goes after '^' — write 'async ^int (int x) { … }'; the 'async int ^…' spelling was removed");
            }
            self.advance(); // consume `async`
            return self.parse_async_block_literal();
        }
        if self.match_keyword(KeywordKind::Extension) {
            // __extension__ is a prefix that suppresses pedantic warnings.
            // Skip it and parse the underlying expression.
            return self.parse_primary();
        }
        if self.match_token(TokenKind::Identifier) {
            let text = self.previous_text().to_string();
            let line = self.previous.line;
            let col = self.previous.column;
            if self.current.kind == TokenKind::ColonColon {
                let ident_expr = CstExpr {
                    kind: CstExprKind::Ident, expr_type: None,
                    line, col,
                    data: CstExprData::Ident(text.clone()),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
                if let Some(qn) = self.parse_qualified_name_from(&ident_expr) {
                    return Some(CstExpr {
                        kind: CstExprKind::Ident, expr_type: None,
                        line, col,
                        data: CstExprData::Ident(qn),
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
});
                }
            }
            return Some(CstExpr {
                kind: CstExprKind::Ident,
                expr_type: None,
                line, col,
                data: CstExprData::Ident(text),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        if self.current.kind == TokenKind::Keyword {
            let kw = self.current.keyword;
            if kw == KeywordKind::Id || kw == KeywordKind::Class ||
               kw == KeywordKind::Sel || kw == KeywordKind::Instancetype ||
               self.is_contextual_kw_ident() {
                self.advance();
                let text = self.previous_text().to_string();
                return Some(CstExpr {
                    kind: CstExprKind::Ident,
                    expr_type: None,
line: pri_line,
                    col: pri_col,
                    data: CstExprData::Ident(text),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
});
            }
        }

        if self.match_keyword(KeywordKind::Self_) {
            return Some(CstExpr {
                kind: CstExprKind::Self_, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Ident("self".into()),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Super) {
            return Some(CstExpr {
                kind: CstExprKind::Super, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Ident("super".into()),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Cmd) {
            return Some(CstExpr {
                kind: CstExprKind::Cmd, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Ident("_cmd".into()),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Nil) {
            return Some(CstExpr {
                kind: CstExprKind::Nil, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Ident("nil".into()),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Null) {
            return Some(CstExpr {
                kind: CstExprKind::Null, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Ident("NULL".into()),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Yes) || self.match_keyword(KeywordKind::No) {
            let val = self.previous.keyword == KeywordKind::Yes;
            return Some(CstExpr {
                kind: CstExprKind::Bool, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Bool(val),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Integer) {
            let text = self.previous_text();
            // C literal bases: 0x/0X hex, leading-0 octal, decimal otherwise.
            // Octal was parsed as decimal (0755 → 755): the generated C lost
            // the radix, so `mkdir(p, 0755)` opened the directory with a
            // wrong mode — a silent C-superset break caught by the
            // NPFileManager probes.
            let val = Self::parse_int_literal_text(text);
            // The lexer strips the integer suffix from the token text but
            // records it in `int_suffix`. Annotate the literal's type here —
            // downstream return-type inference (block literals) reads this;
            // without it `^{ return 7L; }` inferred `int`.
            let suf = self.previous.int_suffix;
            let expr_type = if suf != 0 {
                let prim = if suf & INT_SUFFIX_LL != 0 { TypePrim::LongLong }
                    else if suf & INT_SUFFIX_L != 0 { TypePrim::Long }
                    else { TypePrim::Unsigned };
                let mut t = CstType::new(prim);
                if suf & INT_SUFFIX_U != 0 { t.is_unsigned = true; }
                Some(t)
            } else { None };
            return Some(CstExpr {
                kind: CstExprKind::Integer, expr_type: expr_type.map(Box::new),
                line: pri_line, col: pri_col,
                data: CstExprData::Integer(val),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Float) {
            let raw = self.previous_text().to_string();
            // Imaginary suffix (§6.4.4.2): preserve the literal verbatim — an
            // f64 parse would silently drop the imaginary part (`2.0i` -> 2.0).
            if raw.ends_with(|c: char| c == 'i' || c == 'I' || c == 'j' || c == 'J') {
                return Some(CstExpr {
                    kind: CstExprKind::Float, expr_type: None,
                    line: pri_line, col: pri_col,
                    data: CstExprData::FloatRaw(raw),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
});
            }
            let text = raw.trim_end_matches(|c: char| c == 'f' || c == 'F');
            let val = text.parse::<f64>().unwrap_or(0.0);
            return Some(CstExpr {
                kind: CstExprKind::Float, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Float(val),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::String) {
            let mut text = self.previous_text().to_string();
            // Adjacent string literals ("a" "b") are concatenated by the C
            // preprocessor; support them so multi-line printf formats parse.
            while self.check(TokenKind::String) {
                text.push_str(self.current_text());
                self.advance();
            }
            return Some(CstExpr {
                kind: CstExprKind::String, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::String(text),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::AtString) {
            let text = self.previous_text().to_string();
            return Some(CstExpr {
                kind: CstExprKind::AtString, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::AtString(text),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        // `@123` / `@1.5` — NPNumber boxing literal. Desugared to a normal
        // class-method send (`[NPNumber numberWithInt:123]` /
        // `numberWithLongLong:` / `numberWithDouble:`) so all downstream
        // resolution (binder/checker/codegen) reuses the message-send path.
        // The suffix decides the width: `@7L` must box as long long —
        // `numberWithInt:` would truncate past 32 bits.
        if self.match_token(TokenKind::AtNumber) {
            let text = self.previous_text().to_string();
            let suf = self.previous.int_suffix;
            let line = self.previous.line;
            let col = self.previous.column;
            let is_float = text.contains('.') || text.contains('e') || text.contains('E');
            let number_expr = if is_float {
                CstExpr {
                    kind: CstExprKind::Float, expr_type: None, line, col,
                    data: CstExprData::Float(text.parse::<f64>().unwrap_or(0.0)),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
}
            } else {
                CstExpr {
                    kind: CstExprKind::Integer, expr_type: None, line, col,
                    data: CstExprData::Integer(text.parse::<i64>().unwrap_or_else(|_| text.parse::<u64>().unwrap_or(0) as i64)),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
}
            };
            let selector = if is_float {
                "numberWithDouble:"
            } else if suf & (INT_SUFFIX_L | INT_SUFFIX_LL) != 0 {
                "numberWithLongLong:"
            } else {
                "numberWithInt:"
            };
            return Some(self.mk_npnumber_send(selector, number_expr, line, col));
        }
        // `@YES` / `@NO` (`@true` / `@false`) — boxed BOOL literal.
        if self.match_token(TokenKind::AtBool) {
            let val = self.previous.char_val as i64;
            let line = self.previous.line;
            let col = self.previous.column;
            let arg = CstExpr {
                kind: CstExprKind::Integer, expr_type: None, line, col,
                data: CstExprData::Integer(val),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
            return Some(self.mk_npnumber_send("numberWithBool:", arg, line, col));
        }
        // `@'c'` — boxed character literal.
        if self.match_token(TokenKind::AtChar) {
            let val = self.previous.char_val;
            let line = self.previous.line;
            let col = self.previous.column;
            let arg = CstExpr {
                kind: CstExprKind::Char, expr_type: None, line, col,
                data: CstExprData::Char(val),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
            return Some(self.mk_npnumber_send("numberWithChar:", arg, line, col));
        }
        // `@(expr)` — boxed expression. The parser has no types, so the
        // NPNumber factory is picked by the checker from the expression's
        // static type; carry the node through as `Boxed`.
        if self.match_token(TokenKind::AtLParen) {
            let line = self.previous.line;
            let col = self.previous.column;
            let inner = self.parse_expression();
            self.consume(TokenKind::RParen, "expected ')'");
            return inner.map(|e| CstExpr {
                kind: CstExprKind::Boxed, expr_type: None, line, col,
                data: CstExprData::Boxed(Box::new(e)),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Char) {
            return Some(CstExpr {
                kind: CstExprKind::Char, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Char(self.previous.char_val),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        // @selector(...)
        if self.match_keyword(KeywordKind::AtSelector) {
            self.consume(TokenKind::LParen, "expected '(' after '@selector'");
            let mut sel = String::new();
            while self.current.kind == TokenKind::Identifier ||
                  (self.current.kind == TokenKind::Keyword && !matches!(self.current.keyword,
                      KeywordKind::AtInterface | KeywordKind::AtImplementation | KeywordKind::AtEnd |
                      KeywordKind::AtProperty | KeywordKind::AtSynthesize | KeywordKind::AtDynamic |
                      KeywordKind::AtSelector | KeywordKind::AtEncode | KeywordKind::AtProtocol |
                      KeywordKind::AtOptional | KeywordKind::AtRequired | KeywordKind::AtClass |
                      KeywordKind::AtTry | KeywordKind::AtCatch | KeywordKind::AtFinally |
                      KeywordKind::AtThrow | KeywordKind::AtThrows | KeywordKind::AtSynchronized | KeywordKind::AtAutoreleasepool | KeywordKind::AtDefer |
                       KeywordKind::AtNoArc |
                      KeywordKind::AtPublic | KeywordKind::AtPackage | KeywordKind::AtProtected |
                      KeywordKind::AtPrivate | KeywordKind::AtDefs | KeywordKind::AtNamespace |
                      KeywordKind::AtEndNamespace |
                      KeywordKind::AtUsing | KeywordKind::Self_ | KeywordKind::Super |
                      KeywordKind::Return | KeywordKind::If | KeywordKind::Else |
                      KeywordKind::Switch | KeywordKind::Case | KeywordKind::Default |
                      KeywordKind::While | KeywordKind::Do | KeywordKind::For |
                      KeywordKind::Break | KeywordKind::Continue | KeywordKind::Goto |
                      KeywordKind::Sizeof | KeywordKind::Typeof | KeywordKind::Typedef |
                      KeywordKind::Struct | KeywordKind::Union | KeywordKind::Enum |
                      KeywordKind::Const | KeywordKind::Volatile | KeywordKind::Extern |
                      KeywordKind::Static | KeywordKind::Auto | KeywordKind::Register |
                      KeywordKind::Inline | KeywordKind::Restrict |
                      KeywordKind::Imp | KeywordKind::NpZone |
                      KeywordKind::Import | KeywordKind::Include | KeywordKind::Define |
                      KeywordKind::Ifdef | KeywordKind::Ifndef | KeywordKind::Endif |
                      KeywordKind::Pragma | KeywordKind::Elif | KeywordKind::Undef
                  )) {
                self.advance();
                sel.push_str(self.previous_text());
                if self.current.kind == TokenKind::Colon {
                    self.advance();
                    sel.push(':');
                } else {
                    break;
                }
            }
            self.consume(TokenKind::RParen, "expected ')'");
            return Some(CstExpr {
                kind: CstExprKind::Selector, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Selector(sel),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        // @protocol(...)
        if self.match_keyword(KeywordKind::AtProtocol) {
            self.consume(TokenKind::LParen, "expected '(' after '@protocol'");
            let mut proto = String::new();
            if self.current.kind == TokenKind::Identifier {
                self.advance();
                proto = self.previous_text().to_string();
            }
            self.consume(TokenKind::RParen, "expected ')'");
            return Some(CstExpr {
                kind: CstExprKind::Protocol, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Protocol(proto),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        // @encode(...)
        if self.match_keyword(KeywordKind::AtEncode) {
            self.consume(TokenKind::LParen, "expected '(' after '@encode'");
            let ty = self.parse_type_full().unwrap_or_else(|| CstType::new(TypePrim::Void));
            self.consume(TokenKind::RParen, "expected ')'");
            return Some(CstExpr {
                kind: CstExprKind::Encode, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Encode(ty),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        // ( expression ) or (type)cast
        if self.match_token(TokenKind::LParen) {
            // GNU C statement expression: `({ stmts...; value; })` — the EH
            // backend emits one per caught evaluation. `({` is neither a cast
            // type nor an expression start, so it must be claimed here before
            // either interpretation below. The body keeps every statement as
            // written; the value is the last expression statement.
            if self.check(TokenKind::LBrace) {
                self.advance(); // consume `{`
                let mut body = Vec::new();
                while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                    let start = self.current.start;
                    match self.parse_statement() {
                        Some(s) => {
                            body.push(s);
                            if self.panic_mode {
                                self.panic_mode = false;
                                if self.current.start == start && !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                                    self.advance();
                                }
                            }
                        }
                        None => {
                            self.synchronize();
                            if self.current.start == start && !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                                self.advance();
                            }
                        }
                    }
                }
                self.consume(TokenKind::RBrace, "expected '}' in statement expression");
                self.consume(TokenKind::RParen, "expected ')' after statement expression");
                return Some(CstExpr {
                    kind: CstExprKind::StmtExpr, expr_type: None,
                    line: pri_line, col: pri_col,
                    data: CstExprData::StmtExpr(body),
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
                });
            }
            let is_cast = {
                if self.current.kind == TokenKind::Keyword {
                    let kw = self.current.keyword;
                    matches!(kw, KeywordKind::Int | KeywordKind::Char | KeywordKind::Short |
                        KeywordKind::Long | KeywordKind::Float | KeywordKind::Double |
                        KeywordKind::Void | KeywordKind::Bool | KeywordKind::Signed |
                        KeywordKind::Unsigned | KeywordKind::Const |
                        KeywordKind::Id | KeywordKind::Class | KeywordKind::Sel |
                        KeywordKind::Instancetype | KeywordKind::Struct |
                        KeywordKind::Union | KeywordKind::Enum)
                } else if self.current.kind == TokenKind::Identifier {
                    let tname = self.current_text().to_string();
                    // Check if this is a qualified name (Namespace::Type) or a known type name
                    self.is_type_name(&tname) || self.peek_colon_colon()
                } else {
                    false
                }
            };
            // `(X *)`: the `*` sits INSIDE the parentheses, so the expression
            // reading is impossible — a `*` cannot take `)` as its operand.
            // clang agrees: for `(Foo *)p` with `Foo` an ordinary variable it
            // reports "expected expression" at that `)`, never a multiplication.
            // So an unknown name here is unknown to C too, and the cascade
            // ("expected ')'") can be replaced by the real
            // reason — but only when the type table is the C preprocessor's own
            // view, because otherwise a name may simply be a typedef we were
            // never told about (a `#include`d header jetic passes through).
            if !is_cast
                && self.current.kind == TokenKind::Identifier
                && self.type_table_is_authoritative()
            {
                let tname = self.current_text().to_string();
                if self.scan_cast_star_paren(self.current.start + self.current.length)
                    && !self.is_type_name(&tname)
                    && !self.is_type_param(&tname)
                {
                    self.advance(); // put the identifier in `previous` for error()
                    self.error(&format!(
                        "unknown type name '{}' — declare it (@class / @interface / typedef) or include the header that declares it",
                        tname));
                    return None;
                }
            }
            if is_cast {
                if let Some(ct) = self.parse_type_full() {
                    if self.match_token(TokenKind::RParen) {
                        // A cast consumed its `)`, so an operand is required:
                        // `(int);` is a hole, not a legal omission. Report it —
                        // the old fill made `(int);` mean `(int)0` in silence —
                        // and keep the synthesized operand as the recovery fill.
                        let expr = self.required_expression(|p| p.parse_unary());
                        return Some(CstExpr {
                            kind: CstExprKind::Cast, expr_type: None,
                            line: pri_line, col: pri_col,
                            data: CstExprData::Cast { target_type: ct, expr: Box::new(expr.unwrap_or_else(||
                                CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}
                            ))},
                        
                            end_line: self.previous.end_line,
                            end_col: self.previous.end_col,
});
                    }
                }
            }
            let expr = self.parse_expression();
            self.consume(TokenKind::RParen, "expected ')'");
            return expr.map(|e| CstExpr {
                kind: CstExprKind::Paren, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Paren(Box::new(e)),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        // Block literal: ^(params) { ... }
        if self.match_token(TokenKind::Caret) {
            return self.parse_block_literal(false);
        }

        // Message send [receiver ...]
        if self.match_token(TokenKind::AtArray) {
            // @[...] array literal
            let mut elements = Vec::new();
            while !self.check(TokenKind::RBracket) && !self.check(TokenKind::Eof) {
                if let Some(e) = self.parse_assignment() {
                    elements.push(e);
                }
                if !self.match_token(TokenKind::Comma) { break; }
            }
            self.consume(TokenKind::RBracket, "expected ']'");
            return Some(CstExpr {
                kind: CstExprKind::ArrayLit, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::ArrayLit(elements),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.check(TokenKind::LBracket) {
            // In-message bare-`await` ban: record the depth at this message's
            // `[` — the bracket itself is already counted while current, and
            // nothing after it is. MUST be captured BEFORE advance(): the token
            // after `[` may itself be an opener (`[(await …) recv]`), and
            // capturing after advance() would record that opener's depth and
            // mis-flag the parenthesized await as bare. A later `await` at
            // exactly this depth sits unparenthesized in a receiver or
            // argument slot and is rejected in parse_primary.
            let inside_depth = self.expr_paren_depth;
            self.msg_await_stack.push((inside_depth, false));
            self.advance(); // consume `[` (counts the next token if an opener)
            // The receiver may be a generic-instantiated type expression like
            // `VectorBuffer<RenderPoint2D*>` in `[[VectorBuffer<RenderPoint2D*> alloc] init]`.
            // parse_expression() would treat `<` as a comparison operator, so try
            // parse_type_full first; if it parses a type with generic args and the
            // next token is an identifier (the start of a selector), treat the
            // rendered type string as the receiver identifier. Otherwise rewind
            // both lexer and current token so parse_expression handles it normally.
            let mut receiver = None;
            let saved_lex = self.lexer.save_pos();
            let saved_tok = self.current.clone();
            // The speculative parse below consumes tokens through advance(),
            // which maintains the bracket-depth counters; a rewind must undo
            // that too, or a bracket that only became current during the
            // speculation stays counted (observed: `[await [x y] foo]` — the
            // type try ate `await`, made `[` current (+1 depth), then rewound
            // the token stream but not the counter, desynchronizing the
            // in-message bare-await ban).
            let saved_depth = self.expr_paren_depth;
            let saved_stack = self.msg_await_stack.clone();
            if self.current.kind == TokenKind::Identifier {
                if let Some(t) = self.parse_type_full() {
                    if (!t.type_args.is_empty() || !t.protocols.is_empty())
                        && self.current.kind == TokenKind::Identifier {
                        // Type receiver confirmed — build ident expr from rendered type.
                        // For protocols-only (e.g. NPObject<P>), use base class name
                        // to avoid codegen interpreting <P> as generic args.
                        let rstr = if !t.type_args.is_empty() {
                            Self::type_to_fqn(&t)
                        } else {
                            t.name.clone().unwrap_or_else(|| Self::type_to_fqn(&t))
                        };
                        receiver = Some(CstExpr {
                            kind: CstExprKind::Ident, expr_type: None,
                            line: pri_line, col: pri_col,
                            data: CstExprData::Ident(rstr),
                        
                            end_line: self.previous.end_line,
                            end_col: self.previous.end_col,
});
                    }
                }
                if receiver.is_none() {
                    // Rewind lexer + current token to the saved position.
                    self.lexer.restore_pos(saved_lex);
                    self.current = saved_tok;
                    self.expr_paren_depth = saved_depth;
                    self.msg_await_stack = saved_stack;
                }
            }
            if receiver.is_none() {
                receiver = self.parse_expression();
            }
            if let Some(ref mut r) = receiver {
                if r.kind == CstExprKind::Ident && self.current.kind == TokenKind::ColonColon {
                    if let Some(qn) = self.parse_qualified_name_from(r) {
                        r.data = CstExprData::Ident(qn);
                    }
                }
            }
            let mut selector = String::new();
            let mut args = Vec::new();
            while self.current.kind == TokenKind::Identifier ||
                  (self.current.kind == TokenKind::Keyword && !matches!(self.current.keyword,
                      // @-structural directives
                      KeywordKind::AtInterface | KeywordKind::AtImplementation | KeywordKind::AtEnd |
                      KeywordKind::AtProperty | KeywordKind::AtSynthesize | KeywordKind::AtDynamic |
                      KeywordKind::AtSelector | KeywordKind::AtEncode | KeywordKind::AtProtocol |
                      KeywordKind::AtOptional | KeywordKind::AtRequired | KeywordKind::AtClass |
                      KeywordKind::AtTry | KeywordKind::AtCatch | KeywordKind::AtFinally |
                      KeywordKind::AtThrow | KeywordKind::AtThrows | KeywordKind::AtSynchronized | KeywordKind::AtAutoreleasepool | KeywordKind::AtDefer |
                       KeywordKind::AtNoArc |
                      KeywordKind::AtPublic | KeywordKind::AtPackage | KeywordKind::AtProtected |
                      KeywordKind::AtPrivate | KeywordKind::AtDefs | KeywordKind::AtNamespace |
                      KeywordKind::AtEndNamespace |
                      KeywordKind::AtUsing |
                      // Flow control / declaration keywords (never selectors)
                      KeywordKind::Return | KeywordKind::If | KeywordKind::Else |
                      KeywordKind::Switch | KeywordKind::Case | KeywordKind::Default |
                      KeywordKind::While | KeywordKind::Do | KeywordKind::For |
                      KeywordKind::Break | KeywordKind::Continue | KeywordKind::Goto |
                      KeywordKind::Sizeof | KeywordKind::Typeof | KeywordKind::Typedef |
                      KeywordKind::Struct | KeywordKind::Union | KeywordKind::Enum |
                      KeywordKind::Const | KeywordKind::Volatile | KeywordKind::Extern |
                      KeywordKind::Static | KeywordKind::Auto | KeywordKind::Register |
                      KeywordKind::Inline | KeywordKind::Restrict |
                      KeywordKind::Imp | KeywordKind::NpZone |
                      KeywordKind::Import | KeywordKind::Include | KeywordKind::Define |
                      KeywordKind::Ifdef | KeywordKind::Ifndef | KeywordKind::Endif |
                      KeywordKind::Pragma | KeywordKind::Elif | KeywordKind::Undef
                  ))
            {
                let kw = self.current.keyword;
                if kw == KeywordKind::Id || kw == KeywordKind::Class ||
                   kw == KeywordKind::Sel || kw == KeywordKind::Instancetype {
                    // These keywords can be part of a selector
                }
                self.advance();
                let part = self.previous_text().to_string();
                selector.push_str(&part);
                if self.current.kind == TokenKind::Colon {
                    self.advance();
                    selector.push(':');
                    let arg = self.parse_assignment();
                    if let Some(a) = arg {
                        args.push(a);
                    }
                    // ObjC variadic call: `sel:a, b, c` — collect the
                    // comma-separated arguments that belong to this same
                    // selector part (terminated by `]`).
                    while self.current.kind == TokenKind::Comma {
                        self.advance();
                        if self.current.kind == TokenKind::RBracket { break; }
                        if let Some(a) = self.parse_assignment() {
                            args.push(a);
                        }
                    }
                } else if !args.is_empty() {
                    args.push(CstExpr {
                        kind: CstExprKind::Ident, expr_type: None,
                        line: 0, col: 0,
                        data: CstExprData::Ident(part),
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
});
                }
            }
            // If no args were parsed, it's a zero-arg message
            self.consume(TokenKind::RBracket, "expected ']'");

            // Variadic Foundation collection constructor:
            //   [NPArray arrayWithObjects:a, b, c, nil]  →  @[a, b, c]
            // ObjC variadic methods don't exist in Jeti; the array literal
            // already lowers to the `jeti_array_create` runtime helper, so
            // desugar to it (dropping the trailing `nil` terminator).
            let colon_count = selector.matches(':').count();
            if colon_count > 0 && args.len() > colon_count && selector == "arrayWithObjects:" {
                let mut elems = args;
                if elems.last().map_or(false, |e| e.kind == CstExprKind::Nil) {
                    elems.pop();
                }
                return Some(CstExpr {
                    kind: CstExprKind::ArrayLit, expr_type: None,
                    line: pri_line, col: pri_col,
                    data: CstExprData::ArrayLit(elems),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
});
            }

            return Some(CstExpr {
                kind: CstExprKind::MessageSend, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::Message {
                    receiver: Box::new(receiver.unwrap_or_else(||
                        CstExpr { kind: CstExprKind::Ident, expr_type: None, line: 0, col: 0, data: CstExprData::Ident("self".into()) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}
                    )),
                    selector,
                    args,
                },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        // @[...] array literal
        if self.match_token(TokenKind::LBracket) {
            // This is the @[ case — the @ was consumed as LBracket, now [ is the next token
            // Actually, @[ produces LBracket (for @), then LBracket (for [).
            // So we've already consumed the @ via LBracket, and now we need to consume [.
            // But wait: the @ was consumed as a LBracket, and [ is the next token as LBracket.
            // Let me re-check: the lexer for @[ returns LBracket (for @) with length 1.
            // Then next call to next() returns LBracket (for [).
            // So match_token(LBracket) consumed the @, and now we need to consume the [.
            // But wait, match_token(LBracket) already consumed one token. If we're here,
            // it means the token was an LBracket. But @[ produces TWO LBrackets.
            // So we need to handle this differently.
            // Actually, let me re-think: the parser sees @[ as two tokens: LBracket (for @) and LBracket (for [).
            // The parser's @[] handling should consume BOTH.
            // But we already consumed one LBracket to get here. Let me consume the second one.
            // Hmm, actually I think this is wrong. Let me just handle array literals differently.
            // The @[ case: the lexer outputs LBracket (for @), then LBracket (for [).
            // The parser hits this code when it sees LBracket in parse_primary.
            // But we already consumed the first LBracket (the @).
            // Now we need to consume the second LBracket (the [).
            // Let me just check: is the current token LBracket?
            if self.current.kind == TokenKind::LBracket {
                self.advance(); // consume the [
            }

            let mut elements = Vec::new();
            while !self.check(TokenKind::RBracket) && !self.check(TokenKind::Eof) {
                if let Some(e) = self.parse_assignment() {
                    elements.push(e);
                }
                if !self.match_token(TokenKind::Comma) { break; }
            }
            self.consume(TokenKind::RBracket, "expected ']'");
            return Some(CstExpr {
                kind: CstExprKind::ArrayLit, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::ArrayLit(elements),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        if self.match_token(TokenKind::LBrace) {
            return self.parse_init_list_or_dict();
        }

        // @{...} dictionary literal
        if self.match_token(TokenKind::AtDict) {
            let mut keys = Vec::new();
            let mut values = Vec::new();
            let mut is_dict = false;
            while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                if let Some(k) = self.parse_assignment() {
                    if self.match_token(TokenKind::Colon) {
                        is_dict = true;
                        // The value MUST be parsed one level below the comma
                        // operator. `parse_expression()` swallows the `,` that
                        // separates entries (`@1, @"b"` became a single Comma
                        // expression), so only the LAST pair of a multi-entry
                        // literal survived and the next key's `:` collided with
                        // "expected '}'". Array literals already use
                        // `parse_assignment()` for exactly this reason.
                        if let Some(v) = self.parse_assignment() {
                            keys.push(k);
                            values.push(v);
                        }
                    } else {
                        if !is_dict {
                            keys.push(k);
                        }
                    }
                }
                if !self.match_token(TokenKind::Comma) { break; }
            }
            self.consume(TokenKind::RBrace, "expected '}'");
            // `@{}` is an empty *dictionary* (ObjC: `@{}` and `@[]` are
            // different literals); only a colon-less non-empty body keeps the
            // tolerant array fallback.
            if is_dict || keys.is_empty() {
                return Some(CstExpr {
                    kind: CstExprKind::DictLit, expr_type: None,
                    line: pri_line, col: pri_col,
                    data: CstExprData::DictLit { keys, values },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
});
            }
            return Some(CstExpr {
                kind: CstExprKind::ArrayLit, expr_type: None,
                line: pri_line, col: pri_col,
                data: CstExprData::ArrayLit(keys),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        // @(number) — @( produces LParen (for @), then LParen (for ()
        // Actually same pattern: @( is LParen, then LParen
        // We handle this in parse_expression normally

        None
    }

    fn parse_init_list_or_dict(&mut self) -> Option<CstExpr> {
        let (il_line, il_col) = (self.previous.line, self.previous.column);
        let mut elements = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            // C99 designated initializer: `.field = e` / `[i] = e` / `[2].y = e`.
            if self.check(TokenKind::Dot) || self.check(TokenKind::LBracket) {
                if let Some(e) = self.parse_designated_init() {
                    elements.push(e);
                }
                if !self.match_token(TokenKind::Comma) { break; }
                continue;
            }
            if let Some(e) = self.parse_assignment() {
                elements.push(e);
            }
            if !self.match_token(TokenKind::Comma) { break; }
        }
        self.consume(TokenKind::RBrace, "expected '}'");
        Some(CstExpr {
            kind: CstExprKind::InitList, expr_type: None,
            line: il_line, col: il_col,
            data: CstExprData::InitList(elements),
        
            end_line: self.previous.end_line,
            end_col: self.previous.end_col,
})
    }

    /// Parse one C99 designated initializer entry: a chain of `.field` /
    /// `[index]` designators followed by `= expr`. Returns a
    /// `DesignatedInit` expression node.
    fn parse_designated_init(&mut self) -> Option<CstExpr> {
        let line = self.current.line;
        let col = self.current.column;
        let mut designators: Vec<CstDesignator> = Vec::new();
        loop {
            if self.match_token(TokenKind::Dot) {
                let name = if self.current.kind == TokenKind::Identifier || self.is_contextual_kw_ident() {
                    let t = self.current_text().to_string();
                    self.advance();
                    t
                } else {
                    self.error("expected field name after '.' in designated initializer");
                    return None;
                };
                designators.push(CstDesignator::Member(name));
            } else if self.match_token(TokenKind::LBracket) {
                let idx = self.parse_expression()?;
                self.consume(TokenKind::RBracket, "expected ']'");
                designators.push(CstDesignator::Index(Box::new(idx)));
            } else {
                break;
            }
        }
        if designators.is_empty() {
            self.error("expected designator ('.field' or '[index]') in designated initializer");
            return None;
        }
        self.consume(TokenKind::Assign, "expected '='");
        let expr = self.parse_assignment()?;
        Some(CstExpr {
            kind: CstExprKind::DesignatedInit, expr_type: None,
            line, col,
            data: CstExprData::DesignatedInit {
                designators,
                expr: Box::new(expr),
            },
        
            end_line: self.previous.end_line,
            end_col: self.previous.end_col,
})
    }

    /// Async block literal (§5.1): `async` already consumed. Accepted form:
    /// `^` Return-type? Params? Body — the plain block layout, return type
    /// AFTER `^` (read by the shared tail below). The legacy `async T ^…`
    /// layout (return type BEFORE `^`) is only a recovery reader here: the
    /// caller has already reported the migration error; parsing the old
    /// shape keeps the diagnostic list clean instead of cascading.
    /// Params/body reuse the plain block machinery; IIFE `(args)` call is
    /// handled by postfix parsing.
    fn parse_async_block_literal(&mut self) -> Option<CstExpr> {
        // Optional return type before `^`: builtin type keyword or typename
        // identifier (`async void ^`, `async Result *^`, `async int ^`,
        // `async NPArray<NPString *> *^`). Stop at `^` / `{` / `(`.
        let return_type = if self.check(TokenKind::Caret) || self.check(TokenKind::LBrace) {
            None
        } else {
            let saved_annotating = self.annotating;
            self.annotating = true;
            let rt = if self.current.kind == TokenKind::Keyword &&
                matches!(self.current.keyword, KeywordKind::Void | KeywordKind::Int |
                    KeywordKind::Char | KeywordKind::Short | KeywordKind::Long |
                    KeywordKind::Float | KeywordKind::Double | KeywordKind::Bool |
                    KeywordKind::Signed | KeywordKind::Unsigned | KeywordKind::Id |
                    KeywordKind::Class | KeywordKind::Sel | KeywordKind::Instancetype) {
                self.parse_type_name().map(Box::new)
            } else if self.current.kind == TokenKind::Identifier {
                self.parse_type_full().map(Box::new)
            } else {
                None
            };
            self.annotating = saved_annotating;
            rt
        };

        if !self.match_token(TokenKind::Caret) {
            self.error("expected '^' after 'async' return type");
            return None;
        }
        // Shared tail: Parameters? BlockBody — identical to plain blocks.
        self.parse_block_literal_tail(return_type, true)
    }

    fn parse_block_literal(&mut self, is_async: bool) -> Option<CstExpr> {
        self.parse_block_literal_tail(None, is_async)
    }

    /// Shared block tail after `^` (plain blocks) or after the `async ^`
    /// flag: Parameters? BlockBody. `is_async` marks the CST node
    /// (§5.2: evaluation/call yields `NPTask<T> *`).
    fn parse_block_literal_tail(
        &mut self,
        return_type: Option<Box<CstType>>,
        is_async: bool,
    ) -> Option<CstExpr> {
        let (bl_line, bl_col) = (self.previous.line, self.previous.column);
        let mut params: Option<Box<CstParam>> = None;
        let mut param_count = 0;

        // Plain block form `^ReturnType?`: optional return type right after
        // `^` (async blocks resolve theirs BEFORE `^`, in
        // parse_async_block_literal).
        //
        // `annotating` is set by hand rather than going through
        // `parse_type_annotated`, because that helper calls `parse_type_full`,
        // which would eat the `(` that starts the parameter list — the exact
        // hazard the comment above warns about. The flag is what lets the
        // qualifier loop recognize a leading `nullable` / `nonnull`.
        let return_type = if return_type.is_some() {
            return_type
        } else {
            let saved_annotating = self.annotating;
            self.annotating = true;
            let rt = if self.current.kind == TokenKind::Keyword &&
                matches!(self.current.keyword, KeywordKind::Void | KeywordKind::Int |
                    KeywordKind::Char | KeywordKind::Short | KeywordKind::Long |
                    KeywordKind::Float | KeywordKind::Double | KeywordKind::Bool |
                    KeywordKind::Signed | KeywordKind::Unsigned | KeywordKind::Id |
                    KeywordKind::Class | KeywordKind::Sel | KeywordKind::Instancetype) {
                self.parse_type_name().map(Box::new)
            } else if self.current.kind == TokenKind::Identifier {
                // Clang block grammar: `^T(` — the `(` after a typename
                // identifier starts the PARAMETER list, never a function-type
                // declarator (`^async(int v)` with a user typedef named
                // `async` is a block taking int, not a block of type
                // "async(int)"). Peek past the identifier: a `(` — directly
                // or after pointer stars (`NPString *(…)`) — ⇒ take the bare
                // name (+stars) and let the tail's param reader consume the
                // paren; anything else (`<`, another ident) still goes
                // through the full type parser.
                let ident_end = self.current.start + self.current.length;
                let bytes = self.source.as_bytes();
                let mut j = ident_end;
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                let mut stars = 0usize;
                while j < bytes.len() && bytes[j] == b'*' {
                    stars += 1;
                    j += 1;
                    while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                        j += 1;
                    }
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    let mut base = CstType::new(TypePrim::Named);
                    base.name = Some(self.current_text().to_string());
                    self.advance();
                    let mut ty = base;
                    for _ in 0..stars {
                        if !self.match_token(TokenKind::Star) { break; }
                        let mut outer = CstType::new(TypePrim::Named);
                        outer.is_pointer = true;
                        outer.subtype = Some(Box::new(ty));
                        ty = outer;
                    }
                    Some(Box::new(ty))
                } else {
                    self.parse_type_full().map(Box::new)
                }
            } else {
                None
            };
            self.annotating = saved_annotating;
            rt
        };

        if self.match_token(TokenKind::LParen) {
            // `(void)` (C's spelling of "no parameters") is handled INSIDE the
            // loop below, because this parser's `peek_next()` is a stub that
            // returns the current token — lookahead is not available here.
            // parse params: ^int(int x, float y) or ^(int x, float y)
            let mut head: Option<Box<CstParam>> = None;
            let mut tail: &mut Option<Box<CstParam>> = &mut head;
            while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                if let Some(ptype) = self.parse_type_annotated() {
                    // Decided before `ptype` is moved into the parameter below.
                    let void_ty = ptype.prim == TypePrim::Void && !ptype.is_pointer;
                    let mut p = CstParam {
                        par_type: Some(Box::new(ptype)),
                        name: None,
                        external_name: None,
                    attributes: Vec::new(),
                        next: None,
                    };
                    if self.is_name_token() {
                        p.name = Some(self.current_text().to_string());
                        self.advance();
                    }
                    // `(void)` is C's spelling of "no parameters": a `void`
                    // parameter type that carries no name is not a parameter at
                    // all. Keeping it produced `^int(void _arg) { ... }` in the
                    // generated C — wrong, and a hard error under -Werror.
                    // A genuine `void *p` has `is_pointer` set and is kept.
                    let bare_void = p.name.is_none() && void_ty;
                    if !bare_void {
                        param_count += 1;
                        tail = &mut tail.insert(Box::new(p)).next;
                    }
                } else {
                    self.advance();
                }
                if !self.match_token(TokenKind::Comma) { break; }
            }
            self.consume(TokenKind::RParen, "expected ')'");
            params = head;
        }

        // Return type: ^(int x, float y) -> int { ... } or ^(void) { ... }
        // In ObjC, block return type is inferred or specified as ^int(^)(void)
        // Skip for now

        if self.check(TokenKind::LBrace) {
            let body = self.parse_compound_statement();
            return Some(CstExpr {
                kind: CstExprKind::Block, expr_type: None,
                line: bl_line, col: bl_col,
                data: CstExprData::Block {
                    params,
                    param_count,
                    return_type,
                    body: body.map(Box::new),
                    is_async,
                },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }

        Some(CstExpr {
            kind: CstExprKind::Block, expr_type: None,
            line: bl_line, col: bl_col,
            data: CstExprData::Block {
                params,
                param_count,
                return_type,
                body: None,
                is_async,
            },
        
            end_line: self.previous.end_line,
            end_col: self.previous.end_col,
})
    }

    fn parse_postfix(&mut self) -> Option<CstExpr> {
        let (pf_line, pf_col) = (self.current.line, self.current.column);
        let mut expr = self.parse_primary()?;
        loop {
            // Namespace qualifier: expr::name (e.g. Engine::Core::WeaponSystem)
            if self.match_token(TokenKind::ColonColon) {
                if self.current.kind == TokenKind::Identifier ||
                   (self.current.kind == TokenKind::Keyword && matches!(self.current.keyword, KeywordKind::Id | KeywordKind::Class | KeywordKind::Sel | KeywordKind::Instancetype)) {
                    let name = self.current_text().to_string();
                    self.advance();
                    // Build qualified name from the previous expression + :: + name
                    let prev_name = match &expr.data {
                        CstExprData::Ident(s) => s.clone(),
                        _ => String::new(),
                    };
                    let qualified = if prev_name.is_empty() { name } else { format!("{}::{}", prev_name, name) };
                    expr = CstExpr {
                        kind: CstExprKind::Ident, expr_type: None,
                        line: expr.line, col: expr.col,
                        data: CstExprData::Ident(qualified),
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
};
                    continue; // Check for more :: qualifiers
                } else {
                    break;
                }
            }
            // Dot access: expr.property
            if self.match_token(TokenKind::Dot) {
                // The member name is any name token: a struct field may be
                // spelled with a keyword that degrades to an identifier
                // (`retain`, `copy`, `_cmd`, …) — the generated-C vtable
                // dispatch does exactly `->retain`. Class/Sel/Instancetype are
                // kept explicitly (is_name_token does not cover them).
                if self.is_name_token() ||
                   (self.current.kind == TokenKind::Keyword && matches!(self.current.keyword, KeywordKind::Class | KeywordKind::Sel | KeywordKind::Instancetype)) {
                    let prop = self.current_text().to_string();
                    self.advance();
                    expr = CstExpr {
                        kind: CstExprKind::DotAccess, expr_type: None,
                        line: expr.line, col: expr.col,
                        data: CstExprData::Dot {
                            object: Box::new(expr),
                            property: prop,
                        },
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
};
                } else {
                    break;
                }
            }
            // Subscript: expr[key] — unless the brackets hold a message send,
            // which is the common shape after a statement that forgot its `;`
            // (see `bracket_opens_a_message_send`).
            else if self.check(TokenKind::LBracket) && !self.bracket_opens_a_message_send() {
                self.advance();
                // `[` consumed, so an index is required: `a[]` is a hole, and
                // the synthesized 0 must never stand in silently (the old fill
                // made `a[] = 1;` mean `a[0] = 1;`). The synthesized node stays
                // as the recovery fill.
                let key = self.required_expression(|p| p.parse_expression());
                self.consume(TokenKind::RBracket, "expected ']'");
                expr = CstExpr {
                    kind: CstExprKind::Subscript, expr_type: None,
                    line: expr.line, col: expr.col,
                    data: CstExprData::Subscript {
                        object: Box::new(expr),
                        key: Box::new(key.unwrap_or_else(||
                            CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}
                        )),
                    },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
            }
            // Arrow access: expr->property — member name is any name token,
            // see the Dot branch above for why keyword-spelled fields matter.
            else if self.match_token(TokenKind::Arrow) {
                if self.is_name_token() ||
                   (self.current.kind == TokenKind::Keyword && matches!(self.current.keyword, KeywordKind::Class | KeywordKind::Sel | KeywordKind::Instancetype)) {
                    let prop = self.current_text().to_string();
                    self.advance();
                    expr = CstExpr {
                        kind: CstExprKind::Arrow, expr_type: None,
                        line: expr.line, col: expr.col,
                        data: CstExprData::Arrow {
                            object: Box::new(expr),
                            property: prop,
                        },
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
};
                } else {
                    break;
                }
            }
            // Function call: expr(args)
            else if self.match_token(TokenKind::LParen) {
                let mut args = Vec::new();
                // A consumed comma means one more argument is required — the C
                // argument list has no trailing form, so `f(1,)` leaves the
                // list one argument short and must report, not exit quietly.
                let mut after_comma = false;
                while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                    // Type arguments for builtins like `__builtin_offsetof(struct Pt, y)`,
                    // `__builtin_types_compatible_p(int, long)`, `va_arg(ap, int)`.
                    if self.is_builtin_type_arg_start() {
                        if let Some(ty) = self.parse_type_full() {
                            args.push(CstExpr {
                                kind: CstExprKind::TypeLiteral, expr_type: None,
                                line: pf_line, col: pf_col,
                                data: CstExprData::TypeLiteral(ty),
                            
                                end_line: self.previous.end_line,
                                end_col: self.previous.end_col,
});
                            after_comma = self.match_token(TokenKind::Comma);
                            if !after_comma { break; }
                            continue;
                        }
                    }
                    if let Some(a) = self.parse_assignment() {
                        args.push(a);
                    } else {
                        // `(` was consumed, so an empty slot is a missing
                        // argument, not a legal omission.
                        self.report_missing_expression();
                    }
                    after_comma = self.match_token(TokenKind::Comma);
                    if !after_comma { break; }
                }
                if after_comma {
                    self.report_missing_expression();
                }
                self.consume(TokenKind::RParen, "expected ')'");
                expr = CstExpr {
                    kind: CstExprKind::Call, expr_type: None,
                    line: expr.line, col: expr.col,
                    data: CstExprData::Call {
                        callee: Box::new(expr),
                        args,
                    },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
            }
            // Postfix ++/--
            else if self.match_token(TokenKind::Incr) {
                expr = CstExpr {
                    kind: CstExprKind::Unary, expr_type: None,
                    line: expr.line, col: expr.col,
                    data: CstExprData::Unary {
                        op: 1, // ++
                        operand: Box::new(expr),
                        is_postfix: true,
                    },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
            }
            else if self.match_token(TokenKind::Decr) {
                expr = CstExpr {
                    kind: CstExprKind::Unary, expr_type: None,
                    line: expr.line, col: expr.col,
                    data: CstExprData::Unary {
                        op: 2, // --
                        operand: Box::new(expr),
                        is_postfix: true,
                    },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_unary(&mut self) -> Option<CstExpr> {
        let (un_line, un_col) = (self.current.line, self.current.column);
        if self.match_token(TokenKind::Incr) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 1, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Decr) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 2, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Star) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 3, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Ampersand) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 4, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Minus) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 5, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Plus) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 6, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Tilde) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 7, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        if self.match_token(TokenKind::Exclam) {
            let operand = self.parse_unary()?;
            return Some(CstExpr {
                kind: CstExprKind::Unary, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Unary { op: 8, operand: Box::new(operand), is_postfix: false },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        // sizeof / sizeof(type)
        if self.match_keyword(KeywordKind::Sizeof) {
            if self.match_token(TokenKind::LParen) {
                let saved = self.current.clone();
                // Only attempt a type when the next token can actually start one
                // (builtin type keyword, or a registered type name). Otherwise
                // `sizeof(a[0])` would let parse_type_full chew `a` and a `]`
                // would be left over for the expression path.
                let looks_like_type = match &self.current.kind {
                    TokenKind::Identifier => self.is_type_name(&self.current_text()),
                    TokenKind::Keyword => matches!(self.current_text(),
                        "struct" | "union" | "enum" | "void" | "char" | "short" |
                        "int" | "long" | "float" | "double" | "signed" | "unsigned" |
                        "bool" | "_Bool" | "const" | "volatile" | "id" | "Class" |
                        "SEL" | "instancetype"),
                    _ => false,
                };
                if looks_like_type {
                // Could be sizeof(type) or sizeof(expr). If this turns out not
                // to be a real type, fall through to the expression path below
                // (which restores the outer `saved` — same position, nothing
                // was consumed before the attempt).
                if let Some(ty) = self.parse_type_full() {
                    if self.match_token(TokenKind::RParen) {
                        // Only accept as sizeof(type) if it's a real type (struct, or known type name)
                        let is_real_type = ty.is_struct ||
                            matches!(ty.prim, TypePrim::Void | TypePrim::Char | TypePrim::Short |
                                TypePrim::Int | TypePrim::Long | TypePrim::LongLong |
                                TypePrim::Float | TypePrim::Double | TypePrim::Bool |
                                TypePrim::Signed | TypePrim::Unsigned | TypePrim::Id |
                                TypePrim::Class | TypePrim::Sel | TypePrim::Instancetype) ||
                            (ty.prim == TypePrim::Named && ty.name.as_ref().map_or(false, |n| self.is_type_name(n)));
                        if is_real_type {
                            return Some(CstExpr {
                                kind: CstExprKind::Sizeof, expr_type: None,
                                line: un_line, col: un_col,
                                data: CstExprData::Sizeof { type_expr: ty, expr: None },
                            
                                end_line: self.previous.end_line,
                                end_col: self.previous.end_col,
});
                        }
                        // Simple identifier that is not a known type, or a
                        // compound operand like `a[0]` / `s.field` — parse_type_full
                        // already consumed part of it, so reset to just after the
                        // `(` and parse the whole thing as an expression.
                    }
                }
                }
                // Not a type (or the operand can't start one), parse as
                // expression. `saved` was taken *after* the `(` was consumed, so
                // the cursor already sits on the first token of the operand —
                // advancing here would skip it.
                self.current = saved;
                let expr = self.parse_expression();
                self.consume(TokenKind::RParen, "expected ')'");
                return expr.map(|e| CstExpr {
                    kind: CstExprKind::Sizeof, expr_type: None,
                    line: un_line, col: un_col,
                    data: CstExprData::Sizeof { type_expr: CstType::new(TypePrim::Void), expr: Some(Box::new(e)) },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
});
            }
            // Paren-less form: `sizeof expr` (C99 §6.5.3). The paren path above
            // already returned; here the operand is a unary expression.
            let operand = self.parse_unary();
            return operand.map(|e| CstExpr {
                kind: CstExprKind::Sizeof, expr_type: None,
                line: un_line, col: un_col,
                data: CstExprData::Sizeof { type_expr: CstType::new(TypePrim::Void), expr: Some(Box::new(e)) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        // __alignof__ / __alignof(type)
        if self.match_keyword(KeywordKind::Alignof) {
            if self.match_token(TokenKind::LParen) {
                if let Some(ty) = self.parse_type_full() {
                    self.consume(TokenKind::RParen, "expected ')'");
                    return Some(CstExpr {
                        kind: CstExprKind::Alignof, expr_type: None,
                        line: un_line, col: un_col,
                        data: CstExprData::Alignof(ty),
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
});
                }
            }
        }
        self.parse_postfix()
    }

    fn parse_multiplicative(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_unary()?;
        let ops = [TokenKind::Star, TokenKind::Slash, TokenKind::Percent];
        while let Some(op) = ops.iter().find(|o| self.match_token(**o)) {
            let right = self.required_expression(|p| p.parse_unary())?;
            let op_val = match op {
                TokenKind::Star => 1,
                TokenKind::Slash => 2,
                TokenKind::Percent => 3,
                _ => 0,
            };
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: op_val, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_additive(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_multiplicative()?;
        let ops = [TokenKind::Plus, TokenKind::Minus];
        while let Some(op) = ops.iter().find(|o| self.match_token(**o)) {
            let right = self.required_expression(|p| p.parse_multiplicative())?;
            let op_val = match op {
                TokenKind::Plus => 4,
                TokenKind::Minus => 5,
                _ => 0,
            };
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: op_val, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_shift(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_additive()?;
        let ops = [TokenKind::LShift, TokenKind::RShift];
        while let Some(op) = ops.iter().find(|o| self.match_token(**o)) {
            let right = self.required_expression(|p| p.parse_additive())?;
            let op_val = match op {
                TokenKind::LShift => 6,
                TokenKind::RShift => 7,
                _ => 0,
            };
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: op_val, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_relational(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_shift()?;
        let ops = [TokenKind::Less, TokenKind::Greater, TokenKind::Leq, TokenKind::Geq];
        while let Some(op) = ops.iter().find(|o| self.match_token(**o)) {
            let right = self.required_expression(|p| p.parse_shift())?;
            let op_val = match op {
                TokenKind::Less => 8,
                TokenKind::Greater => 9,
                TokenKind::Leq => 10,
                TokenKind::Geq => 11,
                _ => 0,
            };
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: op_val, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_equality(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_relational()?;
        let ops = [TokenKind::Eq, TokenKind::Neq];
        while let Some(op) = ops.iter().find(|o| self.match_token(**o)) {
            let right = self.required_expression(|p| p.parse_relational())?;
            let op_val = match op {
                TokenKind::Eq => 12,
                TokenKind::Neq => 13,
                _ => 0,
            };
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: op_val, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_bitwise_and(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_equality()?;
        while self.match_token(TokenKind::Ampersand) {
            let right = self.required_expression(|p| p.parse_equality())?;
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: 14, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_bitwise_xor(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_bitwise_and()?;
        while self.match_token(TokenKind::Caret) {
            let right = self.required_expression(|p| p.parse_bitwise_and())?;
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: 15, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_bitwise_or(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_bitwise_xor()?;
        while self.match_token(TokenKind::Pipe) {
            let right = self.required_expression(|p| p.parse_bitwise_xor())?;
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: 16, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_logical_and(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_bitwise_or()?;
        while self.match_token(TokenKind::LogicalAnd) {
            let right = self.required_expression(|p| p.parse_bitwise_or())?;
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: 17, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_logical_or(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_logical_and()?;
        while self.match_token(TokenKind::LogicalOr) {
            let right = self.required_expression(|p| p.parse_logical_and())?;
            expr = CstExpr {
                kind: CstExprKind::Binary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Binary { op: 18, left: Box::new(expr), right: Box::new(right) },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    fn parse_conditional(&mut self) -> Option<CstExpr> {
        let mut expr = self.parse_logical_or()?;
        if self.match_token(TokenKind::Question) {
            let true_expr = self.required_expression(|p| p.parse_expression())?;
            self.consume(TokenKind::Colon, "expected ':'");
            let false_expr = self.required_expression(|p| p.parse_conditional())?;
            expr = CstExpr {
                kind: CstExprKind::Ternary, expr_type: None,
                line: expr.line, col: expr.col,
                data: CstExprData::Ternary {
                    cond: Box::new(expr),
                    true_expr: Box::new(true_expr),
                    false_expr: Box::new(false_expr),
                },
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
};
        }
        Some(expr)
    }

    /// An operator or keyword before this parse has already been consumed, so
    /// an empty result is a *missing expression*, not a legal omission (empty
    /// is only legal before an operator, where callers guard with lookahead).
    /// Reports clang's `expected_expression` on the unexpected token. The parse
    /// runs through the closure — not `self.parse_x()` in the argument — so the
    /// receiver borrow and the parse borrow never overlap.
    fn required_expression<F>(&mut self, parse: F) -> Option<CstExpr>
    where
        F: FnOnce(&mut Self) -> Option<CstExpr>,
    {
        match parse(self) {
            Some(expr) => Some(expr),
            None => {
                self.report_missing_expression();
                None
            }
        }
    }

    /// The report half of [`Self::required_expression`], for the sites that
    /// obtain the expression some other way and need only the diagnostic.
    /// No-op while panic mode holds, so a nested failure is never reported
    /// twice.
    fn report_missing_expression(&mut self) {
        let token = self.current.clone();
        self.error_at(&token, "expected expression");
    }

    fn parse_assignment(&mut self) -> Option<CstExpr> {
        let expr = self.parse_conditional()?;
        let assign_ops = [
            TokenKind::Assign, TokenKind::PlusAssign, TokenKind::MinusAssign,
            TokenKind::StarAssign, TokenKind::SlashAssign, TokenKind::PercentAssign,
            TokenKind::AndAssign, TokenKind::OrAssign, TokenKind::XorAssign,
            TokenKind::LShiftAssign, TokenKind::RShiftAssign,
        ];
        for &op in &assign_ops {
            if self.match_token(op) {
                let value = self.required_expression(|p| p.parse_assignment())?;
                if op == TokenKind::Assign {
                    return Some(CstExpr {
                        kind: CstExprKind::Assign, expr_type: None,
                        line: expr.line, col: expr.col,
                        data: CstExprData::Assign { target: Box::new(expr), value: Box::new(value) },
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
});
                }
                let op_val = match op {
                    TokenKind::PlusAssign => 100, TokenKind::MinusAssign => 101,
                    TokenKind::StarAssign => 102, TokenKind::SlashAssign => 103,
                    TokenKind::PercentAssign => 104, TokenKind::AndAssign => 105,
                    TokenKind::OrAssign => 106, TokenKind::XorAssign => 107,
                    TokenKind::LShiftAssign => 108, TokenKind::RShiftAssign => 109,
                    _ => 0,
                };
                return Some(CstExpr {
                    kind: CstExprKind::Binary, expr_type: None,
                    line: expr.line, col: expr.col,
                    data: CstExprData::Binary { op: op_val, left: Box::new(expr), right: Box::new(value) },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
});
            }
        }
        Some(expr)
    }

    fn parse_expression(&mut self) -> Option<CstExpr> {
        let expr = self.parse_assignment()?;
        if self.match_token(TokenKind::Comma) {
            let line = expr.line;
            let col = expr.col;
            let mut exprs = vec![expr];
            loop {
                // A comma was consumed, so an operand is required: the comma
                // *operator* has no trailing form (`(1, );` is a hole), while
                // the lists that do allow a trailing comma — initializer
                // lists, enum members, case values — are parsed elsewhere.
                exprs.push(self.required_expression(|p| p.parse_assignment())?);
                if !self.match_token(TokenKind::Comma) { break; }
            }
            return Some(CstExpr {
                kind: CstExprKind::Comma, expr_type: None,
                line, col,
                data: CstExprData::Comma(exprs),
            
                end_line: self.previous.end_line,
                end_col: self.previous.end_col,
});
        }
        Some(expr)
    }

    // ─── Statement parsing ──────────────────────────────────────────────

    /// Does the `[` the parser is looking at open a **message send** rather than
    /// a subscript?
    ///
    /// `[receiver selector]` is the dominant bracketed form in Jeti. After a
    /// statement that forgot its `;`, the next line's send would otherwise be
    /// read as `expr[…]`, and the cascade it produces — "expected ']' after
    /// subscript" on the *next* line plus a missing-`;` error reported at a stale
    /// token — buries the mistake the user actually made. clang reports just the
    /// missing `;`, on the line missing it, at the end of the statement.
    ///
    /// Two adjacent words inside the brackets are never a valid subscript key, so
    /// only that shape is left to the statement level. The scan reads the source
    /// text (the parser has a single-token lookahead): everything else — `[0]`,
    /// `[i + 1]`, `[f(x)]`, `[b[0]]`, a string, a comment — keeps the subscript
    /// path exactly as before.
    fn bracket_opens_a_message_send(&self) -> bool {
        let is_word_start = |c: char| c.is_alphabetic() || c == '_' || c == '$';
        let is_word_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
        let mut chars = self.source[self.current.start + self.current.length..].chars().peekable();
        let skip_space = |chars: &mut std::iter::Peekable<std::str::Chars>| {
            while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                chars.next();
            }
        };
        // The receiver.
        skip_space(&mut chars);
        if !matches!(chars.peek(), Some(&c) if is_word_start(c)) {
            return false;
        }
        while matches!(chars.peek(), Some(&c) if is_word_char(c)) {
            chars.next();
        }
        // A second word right after it: a selector.
        skip_space(&mut chars);
        matches!(chars.peek(), Some(&c) if is_word_start(c))
    }

    fn parse_expression_statement(&mut self) -> Option<CstStmt> {
        // No expression at all means recovery landed mid-expression (on a `]`, a
        // stray word, …). Reporting a missing `;` here would point at the
        // *previous* token — an earlier statement — which is how one real error
        // used to surface twice at one position. The caller resynchronizes.
        let expr = self.parse_expression()?;
        self.consume(TokenKind::Semicolon, "expected ';' after expression");
        Some(CstStmt {
            kind: CstStmtKind::Expr,
            line: expr.line, column: expr.col,
            data: CstStmtData::Expr(expr),
        
            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    // ─── Inline assembly ────────────────────────────────────────────────────

    fn parse_asm_statement(&mut self) -> Option<CstStmt> {
        // `asm` keyword already consumed
        let line = self.previous.line;
        let column = self.previous.column;
        let (is_volatile, is_goto, template, outputs, inputs, clobbers, labels) = self.parse_asm_body()?;
        Some(CstStmt {
            kind: CstStmtKind::Asm,
            line, column,
            data: CstStmtData::Asm { is_volatile, is_goto, template, outputs, inputs, clobbers, labels },
        
            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_asm_body(&mut self) -> Option<(bool, bool, String, Vec<CstAsmOperand>, Vec<CstAsmOperand>, Vec<String>, Vec<String>)> {
        let is_volatile = self.match_keyword(KeywordKind::Volatile);
        let has_goto = self.match_keyword(KeywordKind::Goto);
        self.consume(TokenKind::LParen, "expected '('");
        let template = self.parse_asm_template()?;
        let mut outputs: Vec<CstAsmOperand> = Vec::new();
        let mut inputs: Vec<CstAsmOperand> = Vec::new();
        let mut clobbers: Vec<String> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        let mut sections = 0;
        while self.match_token(TokenKind::Colon) {
            sections += 1;
            if self.check(TokenKind::Colon) { continue; }
            if sections == 1 {
                outputs = self.parse_asm_operands()?;
            } else if sections == 2 {
                inputs = self.parse_asm_operands()?;
            } else if sections == 3 {
                clobbers = self.parse_asm_clobbers()?;
            } else if sections == 4 && has_goto {
                labels = self.parse_asm_labels()?;
            } else {
                break;
            }
        }
        self.consume(TokenKind::RParen, "expected ')'");
        self.consume(TokenKind::Semicolon, "expected ';' after asm");
        Some((is_volatile, has_goto, template, outputs, inputs, clobbers, labels))
    }

    fn parse_asm_template(&mut self) -> Option<String> {
        if !self.check(TokenKind::String) {
            self.error("expected string literal in asm template");
            return None;
        }
        let mut template = String::new();
        while self.check(TokenKind::String) {
            self.advance();
            template.push_str(self.previous_text());
        }
        Some(template)
    }

    fn parse_asm_operands(&mut self) -> Option<Vec<CstAsmOperand>> {
        let mut ops = Vec::new();
        loop {
            ops.push(self.parse_asm_operand()?);
            if !self.match_token(TokenKind::Comma) { break; }
        }
        Some(ops)
    }

    fn parse_asm_operand(&mut self) -> Option<CstAsmOperand> {
        let name = if self.match_token(TokenKind::LBracket) {
            let n = if self.current.kind == TokenKind::Identifier {
                let n = self.current_text().to_string();
                self.advance();
                n
            } else {
                String::new()
            };
            self.consume(TokenKind::RBracket, "expected ']'");
            if n.is_empty() { None } else { Some(n) }
        } else {
            None
        };
        if !self.check(TokenKind::String) {
            self.error("expected constraint string in asm operand");
            return None;
        }
        self.advance();
        let constraint = self.previous_text().to_string();
        self.consume(TokenKind::LParen, "expected '('");
        let expr = match self.parse_expression() {
            Some(e) => e,
            None => {
                self.error("expected expression in asm operand");
                return None;
            }
        };
        self.consume(TokenKind::RParen, "expected ')'");
        Some(CstAsmOperand { name, constraint, expr: Box::new(expr) })
    }

    fn parse_asm_clobbers(&mut self) -> Option<Vec<String>> {
        let mut cl = Vec::new();
        // Empty clobber section: the caller consumed the section's `:`, and a
        // well-formed asm may leave the list empty (`asm("…" : "+r"(x) : : );`).
        // The next token is then `)` — not a string — so read nothing. (Only
        // the LAST section is ever parsed by the caller, so a non-string here
        // means "empty list", never a stray token from a later section.)
        if !self.check(TokenKind::String) {
            return Some(cl);
        }
        loop {
            self.advance();
            cl.push(self.previous_text().to_string());
            if !self.match_token(TokenKind::Comma) { break; }
        }
        Some(cl)
    }

    fn parse_asm_labels(&mut self) -> Option<Vec<String>> {
        let mut labels = Vec::new();
        loop {
            if self.current.kind == TokenKind::Identifier {
                labels.push(self.current_text().to_string());
                self.advance();
            } else {
                self.error("expected identifier label in asm goto");
                return None;
            }
            if !self.match_token(TokenKind::Comma) { break; }
        }
        Some(labels)
    }

    fn parse_compound_statement(&mut self) -> Option<CstStmt> {
        // Capture the `{` position: the node is built after the closing brace,
        // where `self.previous` is `}`, so reading the start from `previous`
        // (as this used to) pointed the whole block at its own last line.
        let (start_line, start_col) = (self.current.line, self.current.column);
        self.consume(TokenKind::LBrace, "expected '{'");
        let mut stmts = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            // Error recovery: a malformed statement must not swallow the rest of
            // the block. Stop at the next `;` or statement keyword, and keep the
            // `start` guard so a recovery point that is already current cannot
            // spin forever.
            let start = self.current.start;
            if let Some(s) = self.parse_statement() {
                stmts.push(s);
                // Same reasoning as the top-level loop: a statement that failed
                // on a missing ';' still returns Some, leaving panic_mode set,
                // which would suppress every later error in the block. Reset it
                // here without skipping — the parser is still positioned at the
                // start of the *next* statement in the common case.
                if self.panic_mode {
                    self.panic_mode = false;
                    if self.current.start == start && !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                        self.advance();
                    }
                }
            } else {
                self.synchronize();
                if self.current.start == start && !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                    self.advance();
                }
            }
        }
        self.consume(TokenKind::RBrace, "expected '}'");
        Some(CstStmt {
            kind: CstStmtKind::Compound,
            line: start_line, column: start_col,
            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
            data: CstStmtData::Compound(stmts),
        })
    }

    fn parse_statement(&mut self) -> Option<CstStmt> {
        let (stmt_line, stmt_col) = (self.current.line, self.current.column);
        // `#pragma mark ...` pass-through — kept at its source position.
        if self.current.kind == TokenKind::Keyword && self.current.keyword == KeywordKind::Pragma {
            if let Some(d) = self.parse_raw_pragma_decl() {
                return Some(CstStmt { kind: CstStmtKind::Decl, line: d.line, column: d.column, data: CstStmtData::Decl(d) , end_line: self.previous.end_line, end_column: self.previous.end_col 
});
            }
        }
        // Label: `ident:` at statement start (used by asm goto and plain C goto)
        if self.current.kind == TokenKind::Identifier {
            let after = self.current.start + self.current.length;
            let rest = &self.source[after..];
            let trimmed = rest.trim_start();
            if trimmed.starts_with(':') && !trimmed.starts_with("::") {
                let label = self.current_text().to_string();
                let line = self.current.line;
                let column = self.current.column;
                self.advance();
                self.consume(TokenKind::Colon, "expected ':'");
                return Some(CstStmt {
                    kind: CstStmtKind::Label,
                    line, column,
                    data: CstStmtData::Label(label),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
        }
        // Jump statements
        if self.match_keyword(KeywordKind::Return) {
            // Capture the keyword's own position NOW: parsing the expression
            // and consuming ';' advance `previous`, and diagnostics for this
            // statement must point at `return`, not the trailing ';'.
            let (kw_line, kw_col) = (self.previous.line, self.previous.column);
            let expr = if !self.check(TokenKind::Semicolon) && !self.check(TokenKind::RBrace) {
                self.parse_expression()
            } else { None };
            self.consume(TokenKind::Semicolon, "expected ';' after return statement");
            return Some(CstStmt {
                kind: CstStmtKind::Return,
                line: kw_line, column: kw_col,
                data: CstStmtData::Return(expr.map(Box::new)),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Break) {
            let (kw_line, kw_col) = (self.previous.line, self.previous.column);
            self.consume(TokenKind::Semicolon, "expected ';' after break");
            return Some(CstStmt {
                kind: CstStmtKind::Break,
                line: kw_line, column: kw_col,
                data: CstStmtData::Return(None),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Continue) {
            let (kw_line, kw_col) = (self.previous.line, self.previous.column);
            self.consume(TokenKind::Semicolon, "expected ';' after continue");
            return Some(CstStmt {
                kind: CstStmtKind::Continue,
                line: kw_line, column: kw_col,
                data: CstStmtData::Return(None),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Goto) {
            let label = if self.current.kind == TokenKind::Identifier {
                let l = self.current_text().to_string();
                self.advance();
                l
            } else { String::new() };
            self.consume(TokenKind::Semicolon, "expected ';' after goto");
            return Some(CstStmt {
                kind: CstStmtKind::Goto,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Goto(label),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::Asm) {
            return self.parse_asm_statement();
        }
        if self.match_keyword(KeywordKind::AtThrow) {
            let expr = if !self.check(TokenKind::Semicolon) {
                self.parse_expression()
            } else { None };
            self.consume(TokenKind::Semicolon, "expected ';' after @throw");
            return Some(CstStmt {
                kind: CstStmtKind::Throw,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Throw(expr.map(Box::new)),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        // `@throws` in statement position is a misuse — the exception
        // annotation is a declaration tail (`- (void)f throws(...);`), not a
        // statement (and the @ spelling itself was removed). Point the user
        // at `@throw` for raising and at the bare annotation spelling.
        if self.check_keyword(KeywordKind::AtThrows) {
            self.error("'@throws' was removed and is not a statement anyway — annotate a declaration with bare 'throws' or 'throws(<type>)' before ';' or '{'; use '@throw <expr>' to raise an exception");
            // Consume the annotation for error recovery.
            self.advance();
            if self.match_token(TokenKind::LParen) {
                let _ = self.parse_type_full();
                self.consume(TokenKind::RParen, "expected ')' after throws(...)");
            }
            return None;
        }

        // Compound statement
        if self.check(TokenKind::LBrace) {
            return self.parse_compound_statement();
        }

        // If
        if self.match_keyword(KeywordKind::If) {
            self.consume(TokenKind::LParen, "expected '(' after 'if'");
            let cond = self.parse_expression().unwrap_or_else(||
                CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}
            );
            self.consume(TokenKind::RParen, "expected ')'");
            let then_branch = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            let else_branch = if self.match_keyword(KeywordKind::Else) {
                self.parse_statement().map(Box::new)
            } else { None };
            return Some(CstStmt {
                kind: CstStmtKind::If,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::If { cond: Box::new(cond), then_branch, else_branch },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // While
        if self.match_keyword(KeywordKind::While) {
            self.consume(TokenKind::LParen, "expected '(' after 'while'");
            let cond = self.parse_expression().unwrap_or_else(||
                CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}
            );
            self.consume(TokenKind::RParen, "expected ')'");
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            return Some(CstStmt {
                kind: CstStmtKind::While,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::While { cond: Box::new(cond), body },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // Do-while
        if self.match_keyword(KeywordKind::Do) {
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            self.consume_keyword(KeywordKind::While, "expected 'while'");
            self.consume(TokenKind::LParen, "expected '(' after 'while'");
            let cond = self.parse_expression().unwrap_or_else(||
                CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}
            );
            self.consume(TokenKind::RParen, "expected ')'");
            self.consume(TokenKind::Semicolon, "expected ';' after do/while statement");
            return Some(CstStmt {
                kind: CstStmtKind::Do,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Do { body, cond: Box::new(cond) },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // For / For-in
        if self.match_keyword(KeywordKind::For) {
            self.consume(TokenKind::LParen, "expected '(' after 'for'");
            // For-in: `for (Type var in collection)` — desugared here (like
            // `@42` → `[NPNumber numberWithInt:]`) into a plain C for loop so
            // downstream (checker/ARC/codegen) only ever sees For+Decl+MsgSend.
            // Detection is a token scan (no parser rewind): if a depth-0 `in`
            // keyword appears before the matching `)` (and no depth-0 `;`),
            // this is a for-in header. `parse_declaration` consumes the `;`
            // and would record an error at `in`, so a speculative full parse
            // with rewind is not viable here.
            if self.scan_for_in_header() {
                let var_type = self.parse_type_full().unwrap_or_else(|| {
                    let mut t = CstType::new(TypePrim::Named);
                    t.name = Some("id".into());
                    t
                });
                let var_name = if self.is_name_token() {
                    let n = self.current_text().to_string();
                    self.advance();
                    n
                } else {
                    self.error("expected loop variable name in for-in");
                    String::new()
                };
                // `in` is a contextual keyword (see scan_for_in_header): the
                // scan already matched the token text `in` at depth 0, so
                // consume it as a plain identifier token.
                if self.current_text() == "in" { self.advance(); }
                let coll_line = self.previous.line;
                let coll_col = self.previous.column;
                let collection = self.parse_expression().unwrap_or_else(|| {
                    CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}
                });
                self.consume(TokenKind::RParen, "expected ')'");
                let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                    Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
                );
                // Desugar:
                // { T *__jeti_fi = <coll> (borrowed, not owned);
                //   for (size_t __jeti_fi_i = 0; __jeti_fi_i < [__jeti_fi count]; __jeti_fi_i++) {
                //       T var = [__jeti_fi objectAtIndex:__jeti_fi_i]; body } }
                // The collection is an alias (never retained/released), the
                // element variable is a borrowed element alias — ARC needs
                // no injection and sees only plain For+Decl+MsgSend.
                let fi = "__jeti_fi";
                let mk_ident = |name: &str, line: usize, col: usize| CstExpr {
                    kind: CstExprKind::Ident, expr_type: None, line, col,
                    data: CstExprData::Ident(name.to_string()),
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
                let mk_msg = |recv: CstExpr, sel: &str, arg: Option<CstExpr>, line: usize, col: usize| CstExpr {
                    kind: CstExprKind::MessageSend, expr_type: None, line, col,
                    data: CstExprData::Message {
                        receiver: Box::new(recv),
                        selector: sel.to_string(),
                        args: arg.into_iter().collect(),
                    },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
                let coll_ident = || mk_ident(fi, coll_line, coll_col);
                // { id __jeti_fi = <collection>;    (borrowed alias — no ARC;
                //   `id` already renders as `NPObject *` — do NOT set
                //   is_pointer here, that emitted `NPObject * * __jeti_fi`)
                // The alias is typed `id`, NOT the element type: the collection
                // is whatever object the user passed (NPArray, or any type
                // providing `count`/`objectAtIndex:`), which is unrelated to the
                // loop variable's declared type. Using the element type here
                // emitted `NPString * __jeti_fi = arr;` — the checker then
                // rejected every for-in whose element type was more specific
                // than the collection's (e.g. `for (NPString *item in npArray)`).
                let coll_decl = CstStmt {
                    kind: CstStmtKind::Decl, line: coll_line, column: coll_col,
                    data: CstStmtData::Decl(CstDecl {
                        kind: CstDeclKind::Variable,
                        line: coll_line, column: coll_col,
                        name: Some(fi.to_string()),
                        next: None,
                        data: CstDeclData::Variable {
                            var_type: Some(Box::new(CstType::new(TypePrim::Id))),
                            initializer: Some(Box::new(collection.clone())),
                            is_static: false, is_extern: false,
                            is_const: false, is_block_qual: false, is_weak: false,
                        },
                        attributes: Vec::new(),
                    
                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
}),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
};
                // __jeti_fi_i < [__jeti_fi count]
                let cond = CstExpr {
                    kind: CstExprKind::Binary, expr_type: None, line: coll_line, col: coll_col,
                    data: CstExprData::Binary {
                        op: 8, // <
                        left: Box::new(mk_ident("__jeti_fi_i", coll_line, coll_col)),
                        right: Box::new(mk_msg(coll_ident(), "count", None, coll_line, coll_col)),
                    },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
                // __jeti_fi_i++
                let incr = CstExpr {
                    kind: CstExprKind::Unary, expr_type: None, line: coll_line, col: coll_col,
                    data: CstExprData::Unary {
                        op: 1, // ++
                        operand: Box::new(mk_ident("__jeti_fi_i", coll_line, coll_col)),
                        is_postfix: true,
                    },
                
                    end_line: self.previous.end_line,
                    end_col: self.previous.end_col,
};
                // T var = [__jeti_fi objectAtIndex:__jeti_fi_i]
                let elem_decl = CstStmt {
                    kind: CstStmtKind::Decl, line: coll_line, column: coll_col,
                    data: CstStmtData::Decl(CstDecl {
                        kind: CstDeclKind::Variable,
                        line: coll_line, column: coll_col,
                        name: Some(var_name),
                        next: None,
                        data: CstDeclData::Variable {
                            var_type: Some(Box::new(var_type.clone())),
                            initializer: Some(Box::new(mk_msg(
                                coll_ident(), "objectAtIndex:",
                                Some(mk_ident("__jeti_fi_i", coll_line, coll_col)),
                                coll_line, coll_col,
                            ))),
                            is_static: false, is_extern: false,
                            is_const: false, is_block_qual: false, is_weak: false,
                        },
                        attributes: Vec::new(),
                    
                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
}),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
};
                // elem_decl + body → loop body (elem decl first, then user body)
                let loop_body_stmts = match body.data {
                    CstStmtData::Compound(stmts) => {
                        let mut v = Vec::with_capacity(stmts.len() + 1);
                        v.push(elem_decl);
                        v.extend(stmts);
                        v
                    }
                    _ => vec![elem_decl, *body],
                };
                let loop_body = Box::new(CstStmt {
                    kind: CstStmtKind::Compound, line: coll_line, column: coll_col,
                    data: CstStmtData::Compound(loop_body_stmts),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
                // size_t __jeti_fi_i = 0  (for-init counter declaration)
                let counter_init = CstStmt {
                    kind: CstStmtKind::Decl, line: coll_line, column: coll_col,
                    data: CstStmtData::Decl(CstDecl {
                        kind: CstDeclKind::Variable,
                        line: coll_line, column: coll_col,
                        name: Some("__jeti_fi_i".to_string()),
                        next: None,
                        data: CstDeclData::Variable {
                            var_type: Some(Box::new({
                                let mut t = CstType::new(TypePrim::Long);
                                t.is_unsigned = true; // size_t
                                t
                            })),
                            initializer: Some(Box::new(CstExpr {
                                kind: CstExprKind::Integer, expr_type: None,
                                line: coll_line, col: coll_col,
                                data: CstExprData::Integer(0),
                            
                                end_line: self.previous.end_line,
                                end_col: self.previous.end_col,
})),
                            is_static: false, is_extern: false,
                            is_const: false, is_block_qual: false, is_weak: false,
                        },
                        attributes: Vec::new(),
                    
                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
}),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
};
                let for_stmt = CstStmt {
                    kind: CstStmtKind::For, line: coll_line, column: coll_col,
                    data: CstStmtData::For {
                        init: Some(Box::new(counter_init)),
                        cond: Some(Box::new(cond)),
                        incr: Some(Box::new(incr)),
                        body: loop_body,
                    },
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
};
                return Some(CstStmt {
                    kind: CstStmtKind::Compound, line: coll_line, column: coll_col,
                    data: CstStmtData::Compound(vec![coll_decl, for_stmt]),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
            let init = if !self.check(TokenKind::Semicolon) {
                // Check for decl: type name = expr;  (uses the same declaration-start
                // detection as regular statements, so class-name types like
                // `Mini *c = [[Mini alloc] init]` are parsed as declarations)
                if self.is_declaration_start() {
                    let decl = self.parse_declaration();
                    decl.map(|d| CstStmt {
                        kind: CstStmtKind::Decl,
                        line: d.line, column: d.column,
                        data: CstStmtData::Decl(d),
                    
                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
})
                } else {
                    self.parse_expression_statement()
                }
            } else {
                self.advance();
                None
            };
            let cond = if !self.check(TokenKind::Semicolon) {
                let e = self.parse_expression();
                self.consume(TokenKind::Semicolon, "expected ';' after for condition");
                e
            } else {
                self.advance();
                None
            };
            let incr = if !self.check(TokenKind::RParen) {
                let e = self.parse_expression();
                self.consume(TokenKind::RParen, "expected ')'");
                e
            } else {
                self.advance();
                None
            };
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            return Some(CstStmt {
                kind: CstStmtKind::For,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::For {
                    init: init.map(Box::new),
                    cond: cond.map(Box::new),
                    incr: incr.map(Box::new),
                    body,
                },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // For-in dead stub removed: `for (T x in coll)` is desugared directly
        // inside the For branch above (parser-level desugar, like `@42`).

        // Switch
        if self.match_keyword(KeywordKind::Switch) {
            self.consume(TokenKind::LParen, "expected '(' after 'switch'");
            let expr = self.parse_expression().map(Box::new).unwrap_or_else(||
                Box::new(CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
})
            );
            self.consume(TokenKind::RParen, "expected ')'");
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            // Pattern routing: if the body contains any pattern arm
            // (condition / type binding / boxed-literal / when guard), the
            // whole switch becomes a SwitchPat with a flat arm list and is
            // lowered to goto/if dispatch by the pattern crate (Step 3.95).
            // An all-constant switch stays on the plain C path, unchanged.
            let mut collected = PatternArms::default();
            collect_pattern_arms(&body, &mut collected);
            if !collected.arms.is_empty() {
                if collected.has_const_arm {
                    self.error("switch mixes plain constant 'case' arms with pattern arms — M1 cannot lower both in one switch; split them into separate switches");
                }
                // A dangling-comparison arm (`case > 10:`) is lowered to
                // `subject > 10`. When another arm needs the subject as an
                // object (type binding or object literal) the subject is
                // materialized as `NPObject *` instead of `__auto_type`, so the
                // comparison degenerates to a *pointer* vs integer compare —
                // always true, silently, with no diagnostic anywhere. Reject the
                // combination instead of emitting the always-true test.
                if collected.has_cond_arm && collected.has_object_arm {
                    self.error("switch mixes a dangling comparison 'case' arm (e.g. 'case > 10:') with a type-binding or object-literal arm — the subject would be compared as a pointer; use separate switches, or compare with a method like 'case [s intValue] > 10:'");
                }
                // Same degeneration with a cond arm ALONE, when the subject is
                // syntactically an object: no object arm forces `__auto_type`,
                // but `__auto_type` of a pointer is still a pointer, so
                // `__auto_type __jeti_sw = @7; __jeti_sw > 100` is an always-true
                // pointer compare. A variable subject (`id o; switch (o)`) needs
                // real type information, which M1 does not have — that residual
                // case is documented as a known limit.
                if collected.has_cond_arm && expr_is_definitely_object(&expr) {
                    self.error("switch subject is an object but a dangling comparison 'case' arm (e.g. 'case > 10:') is present — the comparison would be a pointer compare, always true; switch on a scalar value instead (e.g. 'switch ([o intValue])')");
                }
                return Some(CstStmt {
                    kind: CstStmtKind::Switch,
                    line: stmt_line, column: stmt_col,
                    data: CstStmtData::SwitchPat {
                        expr,
                        arms: collected.arms,
                        has_default: collected.has_default,
                        default_body: collected.default_body,
                    },
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
            return Some(CstStmt {
                kind: CstStmtKind::Switch,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Switch { expr, body },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // Case
        if self.match_keyword(KeywordKind::Case) {
            let (pattern, guard) = self.parse_case_pattern();
            // A case label must be a C constant expression or a pattern. A
            // message send / call / assignment can never be either, and letting
            // it through emitted invalid C (`case [obj msg]:`) whose error
            // pointed into generated code — report it at the source instead.
            // Object literals are exempt: they are patterns (compared with
            // `isEqual:`), not C case labels.
            if let CstPattern::Const(v) = &pattern {
                if !is_object_literal(v) && expr_is_non_constant(v) {
                    self.error(CASE_NON_CONSTANT_MSG);
                }
            }
            // Multi-value constants: `case 1, 2, 3:` desugars to stacked C
            // labels — collected BEFORE the ':' (the comma list is part of
            // the label, not the body). Values parse at assignment level
            // (NOT parse_expression) so the comma separator isn't swallowed
            // into one comma-expression — the old path silently emitted
            // invalid C `case (1, 2, 3):`.
            let mut extra: Vec<Box<CstExpr>> = Vec::new();
            if matches!(pattern, CstPattern::Const(_)) && guard.is_none() {
                while self.match_token(TokenKind::Comma) {
                    match self.required_expression(|p| p.parse_assignment()) {
                        Some(v) => {
                            if !is_object_literal(&v) && expr_is_non_constant(&v) {
                                self.error(CASE_NON_CONSTANT_MSG);
                            }
                            extra.push(Box::new(v));
                        }
                        // Reported by `required_expression`: a comma consumed,
                        // so a value was required — a trailing comma before
                        // `:` is a hole here too. Breaking keeps the label
                        // going, so recovery still binds the body.
                        None => break,
                    }
                }
            }
            self.consume(TokenKind::Colon, "expected ':'");
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            if let CstPattern::Const(first) = pattern {
                let mut node = CstStmt {
                    kind: CstStmtKind::Case,
                    line: stmt_line, column: stmt_col,
                    data: CstStmtData::Case { value: first, body },
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
};
                while let Some(v) = extra.pop() {
                    node = CstStmt {
                        kind: CstStmtKind::Case,
                        line: node.line, column: node.column,
                        data: CstStmtData::Case { value: v, body: Box::new(node) },
                    
                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
};
                }
                return Some(node);
            }
            // Pattern arm (condition / type binding / object literal / when
            // guard): wrap as a single-arm SwitchPat node. The enclosing
            // switch's collector (collect_pattern_arms) merges it into the
            // parent SwitchPat; the case branch cannot know at parse time
            // whether it sits inside a switch body, so no error here.
            let arm = CstArm {
                pattern,
                guard,
                body,
                line: stmt_line, column: stmt_col,
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
            };
            return Some(CstStmt {
                kind: CstStmtKind::Switch,
                line: arm.line, column: arm.column,
                data: CstStmtData::SwitchPat {
                    expr: Box::new(CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
}),
                    arms: vec![arm],
                    has_default: false,
                    default_body: None,
                },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // Default
        if self.match_keyword(KeywordKind::Default) {
            self.consume(TokenKind::Colon, "expected ':'");
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            return Some(CstStmt {
                kind: CstStmtKind::Default,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Default(body),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // @try/@catch/@finally
        if self.match_keyword(KeywordKind::AtTry) {
            let try_block = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            let mut catches = Vec::new();
            while self.match_keyword(KeywordKind::AtCatch) {
                let mut param = CstParam {
                    par_type: None, name: None, external_name: None, next: None, attributes: Vec::new(),
                };
                if self.match_token(TokenKind::LParen) {
                    param.par_type = self.parse_type_full().map(Box::new);
                    if self.current.kind == TokenKind::Identifier {
                        param.name = Some(self.current_text().to_string());
                        self.advance();
                    }
                    self.consume(TokenKind::RParen, "expected ')'");
                }
                let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                    Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
                );
                catches.push(CstStmt {
                    kind: CstStmtKind::Catch,
                    line: stmt_line, column: stmt_col,
                    data: CstStmtData::Catch { param, body },
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
            let finally_block = if self.match_keyword(KeywordKind::AtFinally) {
                self.parse_statement().map(Box::new)
            } else { None };
            return Some(CstStmt {
                kind: CstStmtKind::Try,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Try { try_block, catches, finally_block },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // @synchronized
        if self.match_keyword(KeywordKind::AtSynchronized) {
            self.consume(TokenKind::LParen, "expected '(' after '@synchronized'");
            let lock = self.parse_expression().map(Box::new).unwrap_or_else(||
                Box::new(CstExpr { kind: CstExprKind::Integer, expr_type: None, line: 0, col: 0, data: CstExprData::Integer(0) , end_line: self.previous.end_line, end_col: self.previous.end_col 
})
            );
            self.consume(TokenKind::RParen, "expected ')'");
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            return Some(CstStmt {
                kind: CstStmtKind::Synchronized,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Synchronized { lock, body },
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // @autoreleasepool
        if self.match_keyword(KeywordKind::AtAutoreleasepool) {
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            return Some(CstStmt {
                kind: CstStmtKind::Autoreleasepool,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Autoreleasepool(body),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // @defer — scope-exit execution. Opaque wrapper here; crates/defer
        // (pipeline Step 3.9) splices the body into every exit of the
        // enclosing block before ARC runs. See AGENTS.md `@defer` section.
        if self.match_keyword(KeywordKind::AtDefer) {
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            return Some(CstStmt {
                kind: CstStmtKind::Defer,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::Defer(body),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        if self.match_keyword(KeywordKind::AtNoArc) {
            let body = self.parse_statement().map(Box::new).unwrap_or_else(||
                Box::new(CstStmt { kind: CstStmtKind::Compound, line: 0, column: 0, data: CstStmtData::Compound(Vec::new()) , end_line: self.previous.end_line, end_column: self.previous.end_col 
})
            );
            return Some(CstStmt {
                kind: CstStmtKind::NoArc,
                line: stmt_line, column: stmt_col,
                data: CstStmtData::NoArc(body),
            
                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // Label: identifier : (not ::)
        if self.current.kind == TokenKind::Identifier && self.peek_next() == TokenKind::Colon {
            let name = self.current_text().to_string();
            // Check if next is :: (namespace) vs : (label)
            let saved = self.current.start;
            self.advance();
            if self.current.kind == TokenKind::Colon && self.peek_next() != TokenKind::Colon {
                self.advance(); // consume :
                return Some(CstStmt {
                    kind: CstStmtKind::Label,
                    line: stmt_line, column: stmt_col,
                    data: CstStmtData::Label(name),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
            // Not a label, rewind
            self.current = Token {
                kind: TokenKind::Identifier,
                keyword: KeywordKind::None,
                start: saved, length: name.len() as usize,
                line: stmt_line, column: stmt_col,
                end_line: stmt_line,
                end_col: stmt_col + name.len(),
                char_val: 0, int_suffix: 0,
            };
        }

        // Declaration
        let stmt_attrs = if self.current.kind == TokenKind::Identifier &&
            (self.current_text() == "__attribute__" || self.current_text() == "__attribute") {
            self.parse_attributes()
        } else { Vec::new() };
        if self.is_declaration_start() {
            let decl = self.parse_declaration();
            #[cfg(feature = "never")]
            eprintln!("[stmt-decl-debug] decl={:?} kind={:?}", decl.as_ref().map(|d| (&d.kind, &d.name)), ());
            if let Some(mut d) = decl {
                // Local-variable leading attribute: `__attribute__((unused)) int x;`
                let mut merged = stmt_attrs;
                merged.append(&mut d.attributes);
                d.attributes = merged;
                return Some(CstStmt {
                    kind: CstStmtKind::Decl,
                    line: d.line, column: d.column,
                    data: CstStmtData::Decl(d),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
        }

        // Expression statement
        self.parse_expression_statement()
    }

    fn peek_next(&self) -> TokenKind {
        // This is a simplified peekahead — in the real parser we'd need to save/restore
        // For now, just return current
        self.current.kind
    }

    fn is_declaration_start(&self) -> bool {
        if self.current.kind == TokenKind::Keyword {
            match self.current.keyword {
                KeywordKind::Int | KeywordKind::Char | KeywordKind::Float |
                KeywordKind::Double | KeywordKind::Long | KeywordKind::Short |
                KeywordKind::Void | KeywordKind::Bool |
                KeywordKind::Const | KeywordKind::Static | KeywordKind::Extern |
                KeywordKind::Struct | KeywordKind::Union | KeywordKind::Enum |
                KeywordKind::Typedef | KeywordKind::Signed | KeywordKind::Unsigned |
                KeywordKind::Volatile | KeywordKind::Auto | KeywordKind::Register |
                KeywordKind::Inline | KeywordKind::Restrict |
                KeywordKind::Block | KeywordKind::Weak | KeywordKind::Strong |
                KeywordKind::Autoreleasing | KeywordKind::UnsafeUnretained |
                KeywordKind::Typeof | KeywordKind::Extension => true,
                // id/Class/Sel/Instancetype are type keywords that can also be variable names
                // Peek at next non-space char to disambiguate:
                //   id obj = ... → next char is identifier → declaration
                //   id = ...     → next char is '=' → expression (variable assignment)
                KeywordKind::Id | KeywordKind::Class | KeywordKind::Sel | KeywordKind::Instancetype => {
                    let after = self.current.start + self.current.length;
                    let rest = &self.source[after..];
                    let next = rest.trim_start().chars().next().unwrap_or(';');
                    // If followed by =, ;, ), , → expression (variable usage)
                    // If followed by *, [, (, <, identifier → declaration
                    matches!(next, '*' | '[' | '(' | '<' | '_' | 'a'..='z' | 'A'..='Z')
                }
                _ => false,
            }
        } else if self.current.kind == TokenKind::Identifier {
            let tname = self.current_text().to_string();
            // Soft keywords (§3/§5): `await`/`async` in operator position
            // START an expression, never a declaration — otherwise the
            // IDENT-IDENT shape rule below reads `await await e` as a decl of
            // variable `await`, and `async ^{...}` / `async void ^...` as a
            // decl of variable `async` (probe-verified misparses). Top-level
            // `async T f(...)` fn decls don't route through here, so this
            // doesn't break the §4.2 function position.
            if (tname == "await" && self.at_await_operator())
                || (tname == "async" && self.at_async_modifier())
            {
                return false;
            }
            // Check if it's a known type name, a type parameter, or a namespace-qualified name
            // Also: `IDENT IDENT` (e.g. `clock_t t0 = ...`) is a declaration —
            // the first identifier is a typedef from an included C header.
            let after = self.current.start + self.current.length;
            let rest = &self.source[after..];
            let trimmed = rest.trim_start();
            let next = trimmed.chars().next().unwrap_or(';');
            // Compound assignments like `x *= e`, `x -= e`, `x >>= e` are
            // expressions, not declarations (`x` is a variable, not a type).
            let is_compound_assign = trimmed.starts_with("*=") || trimmed.starts_with("/=")
                || trimmed.starts_with("%=") || trimmed.starts_with("+=")
                || trimmed.starts_with("-=") || trimmed.starts_with("<<=")
                || trimmed.starts_with(">>=") || trimmed.starts_with("&=")
                || trimmed.starts_with("|=") || trimmed.starts_with("^=");
            // `IDENT IDENT` (`clock_t t0 = ...`) cannot be an expression in C —
            // juxtaposing two primary expressions is not a production — so the
            // shape alone settles it. `IDENT *` cannot: `x * y;` is a
            // multiplication when `x` is a variable and a declaration when `x`
            // is a typedef (`FILE *fp;`), and C picks by the symbol table. Do
            // the same whenever the type table is authoritative; otherwise fall
            // back to the shape guess, which misreads `x * y;` but never turns
            // working code into a parse error.
            let shape_decl = !is_compound_assign
                && (matches!(next, '_' | 'a'..='z' | 'A'..='Z')
                    || (next == '*' && !self.type_table_is_authoritative()));
            self.is_type_name(&tname) || self.is_type_param(&tname)
                || trimmed.starts_with("::")
                || shape_decl
        } else {
            false
        }
    }

    /// True when the current token begins a C type *keyword* (not a bare
    /// identifier type name, which is ambiguous with a variable). Used to parse
    /// type arguments inside builtin calls like `__builtin_offsetof(struct Pt, y)`,
    /// `__builtin_types_compatible_p(int, long)`, and `va_arg(ap, int)`.
    fn is_builtin_type_arg_start(&self) -> bool {
        if self.current.kind == TokenKind::Keyword {
            return matches!(self.current.keyword,
                KeywordKind::Int | KeywordKind::Char | KeywordKind::Float |
                KeywordKind::Double | KeywordKind::Long | KeywordKind::Short |
                KeywordKind::Void | KeywordKind::Bool |
                KeywordKind::Struct | KeywordKind::Union | KeywordKind::Enum |
                KeywordKind::Signed | KeywordKind::Unsigned |
                KeywordKind::Const | KeywordKind::Volatile |
                KeywordKind::Typeof);
        }
        // A registered type name (identifier) also opens a type argument — the
        // common `va_arg(ap, NPObject *)` case, where the type is a class/struct
        // pointer rather than a builtin keyword. In valid C a type name cannot
        // double as a value in the same scope, so this is unambiguous in
        // argument position (the same decision the cast path makes in is_cast).
        if self.current.kind == TokenKind::Identifier {
            return self.is_type_name(&self.current_text());
        }
        false
    }

    // ─── Declaration parsing ────────────────────────────────────────────

    fn parse_qualified_name_with_keywords(&mut self) -> Option<String> {
        let mut name = String::new();
        if self.current.kind == TokenKind::Identifier {
            self.advance();
            name.push_str(self.previous_text());
        } else if self.current.kind == TokenKind::Keyword {
            let kw = self.current.keyword;
            if kw == KeywordKind::Id || kw == KeywordKind::Class ||
               kw == KeywordKind::Sel || kw == KeywordKind::Instancetype ||
               self.is_contextual_kw_ident() {
                self.advance();
                name.push_str(self.previous_text());
            } else {
                return None;
            }
        } else {
            return None;
        }
        while self.match_token(TokenKind::ColonColon) {
            name.push_str("::");
            if self.current.kind == TokenKind::Identifier {
                self.advance();
                name.push_str(self.previous_text());
            } else if self.current.kind == TokenKind::Keyword {
                let kw = self.current.keyword;
                if kw == KeywordKind::Id || kw == KeywordKind::Class ||
                   kw == KeywordKind::Sel || kw == KeywordKind::Instancetype {
                    self.advance();
                    name.push_str(self.previous_text());
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        Some(name)
    }

    /// Contextual `async` modifier (docs/plans/async_soft_keywords_plan.md §4):
    /// the identifier `async` followed by a TYPE START (`void`, builtin type
    /// keyword, `struct/union/enum`, or a typename identifier) = async
    /// function/method modifier; followed by `^` = async block literal (§5,
    /// handled in expression position); anything else (`= ; ) .` …) = a plain
    /// C identifier. Raw-source scan (same style as `peek_colon_colon`), so a
    /// user typedef/variable named `async` keeps working everywhere else —
    /// C superset iron law. `require_nptask_async` (old NPTask-only rule) is
    /// gone: `async` may precede ANY legal return type, including `void`.
    /// `async` immediately followed by `^` (whitespace skipped): the async
    /// block literal start, per §5.1's `async ^…` form. Distinct from
    /// `at_async_modifier`, which also accepts `async` + type-start for the
    /// fn/method return-type positions.
    fn at_async_block_start(&self) -> bool {
        if self.current.kind != TokenKind::Identifier || self.current_text() != "async" {
            return false;
        }
        let mut i = self.current.start + self.current.length;
        let bytes = self.source.as_bytes();
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        i < bytes.len() && bytes[i] == b'^'
    }

    fn at_async_modifier(&self) -> bool {
        if self.current.kind != TokenKind::Identifier || self.current_text() != "async" {
            return false;
        }
        let mut i = self.current.start + self.current.length;
        let bytes = self.source.as_bytes();
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return false;
        }
        let c = bytes[i] as char;
        if c == '^' {
            return true; // async block literal (expression position)
        }
        // Type start: builtin type keyword spellings.
        for kw in ["void", "int", "char", "float", "double", "long", "short",
                   "signed", "unsigned", "bool", "struct", "union", "enum",
                   "const", "volatile", "id", "instancetype"] {
            if self.source[i..].starts_with(kw) {
                let after = self.source[i + kw.len()..].chars().next();
                if after.map_or(true, |a| !(a.is_alphanumeric() || a == '_')) {
                    return true;
                }
            }
        }
        // Typename identifier: `async Result *f(void)` — an identifier start
        // (including the `id`/`Class` keyword spellings covered above).
        c.is_alphabetic() || c == '_'
    }

    /// Soft keyword `await` (docs/plans/async_soft_keywords_plan.md §3.2): in
    /// expression-prefix position, `await` is the suspension operator iff the
    /// next non-whitespace char starts a unary expression — EXCEPT the
    /// unary/binary same-shape operators `+ - * &`, which stay C-compatible
    /// (`await + 1`, `await * p` keep `await` an identifier; `++`/`--` are in
    /// the start set). Raw-source scan, same style as `at_async_modifier`, so
    /// a user variable/function named `await` keeps working in every other
    /// position — C superset iron law. Judgment table: doc §3.4.
    fn at_await_operator(&self) -> bool {
        if self.current.kind != TokenKind::Identifier || self.current_text() != "await" {
            return false;
        }
        let bytes = self.source.as_bytes();
        let mut i = self.current.start + self.current.length;
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return false;
        }
        let c = bytes[i] as char;
        match c {
            '(' | '[' | '^' | '!' | '~' | '@' | '"' | '\'' => true,
            '0'..='9' => true,                          // int/float literal start
            '+' => bytes.get(i + 1) == Some(&b'+'),     // `++` yes, `+` no
            '-' => bytes.get(i + 1) == Some(&b'-'),     // `--` yes, `-` no
            c => c.is_alphabetic() || c == '_',         // Identifier / sizeof
        }
    }

    fn parse_function_decl_or_definition(&mut self, return_type: CstType, name: String, prefix_attrs: Vec<String>, top_level_async: bool, start: (usize, usize)) -> Option<CstDecl> {
        let mut params = Vec::new();
        let mut has_variadic = false;

        self.consume(TokenKind::LParen, "expected '('");
        while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
            if self.match_token(TokenKind::Ellipsis) {
                has_variadic = true;
                break;
            }
            let ptype = self.parse_type_annotated();
            if let Some(mut pt) = ptype {
                let pname = if self.is_name_token() {
                    let n = self.current_text().to_string();
                    self.advance();
                    // Array suffix after param name: name[]
                    if self.match_token(TokenKind::LBracket) {
                        let mut arr_type = CstType::new(TypePrim::Named);
                        arr_type.subtype = Some(Box::new(pt));
                        arr_type.is_array = true;
                        if self.current.kind == TokenKind::Integer {
                            arr_type.array_size = self.current_text().parse().unwrap_or(0);
                            self.advance();
                        } else if self.current.kind == TokenKind::Identifier {
                            arr_type.array_size_name = Some(self.current_text().to_string());
                            self.advance();
                        }
                        self.consume(TokenKind::RBracket, "expected ']'");
                        pt = arr_type;
                    }
                    n
                } else if let Some(bn) = pt.block_name.take() {
                    // Block/fn-ptr declarator: `void (^blk)(…)` / `int (*cb)(…)` —
                    // the type parser consumed the declarator name into
                    // block_name, so no name token follows here. Adopt it as the
                    // parameter name and strip it from the type: codegen's
                    // format_param_decl re-inserts the param name into the
                    // (^…)/(*…) position, so the emitted C keeps exactly one
                    // name. Without this the param registered nameless and every
                    // body reference failed with "use of undeclared identifier"
                    // when generated C was re-parsed (vfs_manager
                    // enumerateChildrenUsingBlock_, round-trip closed loop).
                    bn
                } else { String::new() };
                // Parameter-level attributes: `int x __attribute__((unused))`
                let param_attrs = self.parse_attributes();
                params.push(CstParam {
                    par_type: Some(Box::new(pt)),
                    name: if pname.is_empty() { None } else { Some(pname) },
                    external_name: None,
                    attributes: param_attrs,
                    next: None,
                });
            } else {
                self.advance();
            }
            if !self.match_token(TokenKind::Comma) { break; }
        }
        self.consume(TokenKind::RParen, "expected ')'");

        // Trailing attributes: `int f(int) __attribute__((pure));`
        let mut trailing_attrs = self.parse_attributes();
        trailing_attrs.splice(0..0, prefix_attrs);

        // Trailing declaration annotation: bare `throws` / `throws(T *)` before
        // `;` or `{` — same soft keyword as the method tail (2026-10-13: the
        // @-prefix was dropped). `@throws` here gets the removal diagnostic.
        let mut throws: Option<Box<CstType>> = None;
        if self.check_keyword(KeywordKind::AtThrows) {
            self.error("'@throws' was removed — spell the exception annotation bare: 'throws' or 'throws(<type>)', e.g. 'int f(int) throws(MathError *);'");
            self.advance(); // consume the token for error recovery
            if self.match_token(TokenKind::LParen) {
                let _ = self.parse_type_full();
                self.consume(TokenKind::RParen, "expected ')' after throws(...)");
            }
        } else if self.current.kind == TokenKind::Identifier
            && self.current_text() == "throws"
        {
            self.advance(); // consume the soft keyword
            if self.match_token(TokenKind::LParen) {
                throws = self.parse_type_full().map(Box::new);
                self.consume(TokenKind::RParen, "expected ')' after throws(...)");
            } else {
                // Bare `throws` — "declared to throw, type unstated" (void
                // encoding, see the method tail for the rationale).
                throws = Some(Box::new(CstType::new(TypePrim::Void)));
            }
            if !self.check(TokenKind::LBrace) && !self.check(TokenKind::Semicolon) {
                self.error("expected '('type')' or ';' after throws");
            }
        } else if self.match_keyword(KeywordKind::AtThrow) {
            self.error("@throw is a statement (it raises an exception inside a body); use 'throws' or 'throws(<type>)' to annotate this declaration");
        }

        let body = if self.check(TokenKind::LBrace) {
            self.parse_compound_statement().map(Box::new)
        } else {
            self.consume(TokenKind::Semicolon, "expected ';' after function decl");
            None
        };

        // Link params via next
        let mut head = None;
        let mut tail = &mut head;
        for p in params {
            tail = &mut tail.insert(Box::new(p)).next;
        }

        Some(CstDecl {
            kind: CstDeclKind::Function,
            line: start.0, column: start.1,
            end_line: self.previous.end_line, end_column: self.previous.end_col,
            name: Some(name),
            next: None,
            data: CstDeclData::Function {
                return_type: Some(Box::new(return_type)),
                params: head,
                has_variadic,
                body,
                throws,
                // async_marker: true for a top-level `async T f(...)` function
                // (§4.2); method-position async is set in parse_method.
                async_marker: top_level_async,
            },
                    attributes: trailing_attrs,
})
    }

    /// Parse a raw pass-through line — `#pragma mark ...`, or the C99 operator
    /// spelling `_Pragma("...")` — as a raw declaration. Consumes every token on
    /// the same source line and returns the raw text so codegen can re-emit it
    /// at its original position (both spellings are consumed by the C compiler
    /// there, and position decides what a diagnostic push/pop or `pack` covers).
    fn parse_raw_pragma_decl(&mut self) -> Option<CstDecl> {
        let line = self.current.line;
        let column = self.current.column;
        let start = self.current.start;
        let mut end = start;
        while !self.check(TokenKind::Eof) && self.current.line == line {
            end = self.current.start + self.current.length.max(1);
            self.advance();
        }
        let text = self.source.get(start..end).unwrap_or("").trim_end().to_string();
        Some(CstDecl {
            kind: CstDeclKind::RawLine,
            line, column,
            name: None,
            next: None,
            data: CstDeclData::RawLine(text),
            attributes: Vec::new(),
        
            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_declaration(&mut self) -> Option<CstDecl> {
        // `NP_ASSUME_NONNULL_BEGIN` / `NP_ASSUME_NONNULL_END` — true syntax,
        // NOT macros. ObjC's spelling is a macro wrapping
        // `_Pragma("clang assume_nonnull begin")`, which jetic cannot use: it
        // refuses `_Pragma` inside a macro body (crates/cpp — a source-level
        // expander has no invocation-site position to place it).
        //
        // So the region is applied here, in jetic's own parser
        // (`parse_type_annotated` coerces unannotated pointers to Nonnull), and
        // the marker is rewritten to the clang pragma the macro would have
        // expanded to — emitted via the existing RawLine channel so BOTH sides
        // agree. Passing the bare identifier through instead would leave clang
        // with an undeclared identifier.
        if self.current.kind == TokenKind::Identifier {
            let marker = self.current_text();
            if marker == "NP_ASSUME_NONNULL_BEGIN" || marker == "NP_ASSUME_NONNULL_END" {
                let line = self.current.line;
                let column = self.current.column;
                let opening = marker == "NP_ASSUME_NONNULL_BEGIN";
                if self.nonnull_region == opening {
                    // BEGIN inside BEGIN, or END with no BEGIN. Both mean the
                    // author's intent is not what they think it is, so say so
                    // rather than silently flipping the flag.
                    self.error(if opening {
                        "NP_ASSUME_NONNULL_BEGIN inside an existing nonnull region"
                    } else {
                        "NP_ASSUME_NONNULL_END without a matching NP_ASSUME_NONNULL_BEGIN"
                    });
                }
                self.nonnull_region = opening;
                self.region_file = if opening { Some(self.file_of_line(line)) } else { None };
                while !self.check(TokenKind::Eof) && self.current.line == line {
                    self.advance();
                }
                return Some(CstDecl {
                    kind: CstDeclKind::RawLine,
                    line, column,
                    name: None,
                    next: None,
                    data: CstDeclData::RawLine(
                        if opening {
                            "_Pragma(\"clang assume_nonnull begin\")".to_string()
                        } else {
                            "_Pragma(\"clang assume_nonnull end\")".to_string()
                        }
                    ),
                    attributes: Vec::new(),
                
                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
        }
        // Capture leading `__attribute__((...))` spellings before parsing the
        // declaration itself, so they travel through CST/AST to the C output.
        let attrs = self.parse_attributes();
        let mut decl = self.parse_declaration_inner()?;
        // Merge: leading attrs go first, then any mid/trailing attrs collected
        // by the inner parser (e.g. `void *__attribute__((x)) f(void) __attribute__((y))`).
        let mut merged = attrs;
        merged.append(&mut decl.attributes);
        decl.attributes = merged;
        Some(decl)
    }

    /// Consume one or more `__attribute__((...))` groups, returning the raw
    /// inner attribute spellings (e.g. `packed`, `aligned(8)`).
    fn parse_attributes(&mut self) -> Vec<String> {
        let mut attrs = Vec::new();
        loop {
            let is_attr = self.current.kind == TokenKind::Identifier
                && (self.current_text() == "__attribute__" || self.current_text() == "__attribute");
            if !is_attr {
                break;
            }
            self.advance();
            if !self.match_token(TokenKind::LParen) {
                break;
            }
            if !self.match_token(TokenKind::LParen) {
                break;
            }
            let mut parts: Vec<String> = Vec::new();
            let mut cur = String::new();
            let mut depth = 0;
            loop {
                if self.current.kind == TokenKind::Eof {
                    break;
                }
                if self.match_token(TokenKind::LParen) {
                    depth += 1;
                    cur.push_str("(");
                } else if self.current.kind == TokenKind::RParen {
                    if depth == 0 {
                        self.advance();
                        break;
                    }
                    depth -= 1;
                    self.advance();
                    cur.push_str(")");
                } else if self.current.kind == TokenKind::Comma && depth == 0 {
                    self.advance();
                    parts.push(cur.trim().to_string());
                    cur.clear();
                } else {
                    if self.current.kind == TokenKind::String {
                        // String tokens exclude their quotes (lexer design); an
                        // attribute like `no_sanitize("address","thread")` needs
                        // them back or clang rejects it.
                        cur.push('"');
                        cur.push_str(self.current_text());
                        cur.push('"');
                    } else {
                        cur.push_str(self.current_text());
                    }
                    cur.push(' ');
                    self.advance();
                }
            }
            parts.push(cur.trim().to_string());
            for p in parts {
                if !p.is_empty() {
                    attrs.push(p);
                }
            }
            self.match_token(TokenKind::RParen);
        }
        attrs
    }

    fn parse_declaration_inner(&mut self) -> Option<CstDecl> {
        // The declaration's real start: this function is entered on the
        // declaration's first token (usually the type), but the node literals
        // below are built after the declarator (and, for functions, the whole
        // body), so `self.previous` there is the *last* token, not the first.
        let (decl_line, decl_col) = (self.current.line, self.current.column);
        // Top-level function-position `async` modifier (§4.2): identifier
        // `async` followed by a type start = `async T f(...)` / `async void
        // f(...)`; followed by `^` = async block literal, left to expression
        // parsing. Otherwise `async` is a plain C identifier (`async = 5;`).
        // Declared up front so the struct-fallthrough path can carry it too.
        let mut top_level_async = false;
        if self.current.kind == TokenKind::Identifier
            && self.current_text() == "async"
            && self.at_async_modifier()
        {
            self.advance(); // consume `async`
            top_level_async = true;
        }
        // __extension__ suppresses pedantic warnings — skip it and continue.
        if self.match_keyword(KeywordKind::Extension) {
            return self.parse_declaration_inner();
        }
        // Top-level inline assembly (file-scope asm)
        if self.match_keyword(KeywordKind::Asm) {
            let line = self.previous.line;
            let column = self.previous.column;
            let (is_volatile, is_goto, template, outputs, inputs, clobbers, labels) = self.parse_asm_body()?;
            return Some(CstDecl {
                kind: CstDeclKind::Asm,
                line, column,
                name: None,
                next: None,
                data: CstDeclData::Asm { is_volatile, is_goto, template, outputs, inputs, clobbers, labels },
                            attributes: Vec::new(),

                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }

        // `#pragma mark ...` pass-through — kept at its source position.
        if self.current.kind == TokenKind::Keyword && self.current.keyword == KeywordKind::Pragma {
            return self.parse_raw_pragma_decl();
        }

        // @interface / @implementation / @protocol / @class / @namespace / @using
        if self.current.kind == TokenKind::Keyword {
            match self.current.keyword {
                KeywordKind::AtInterface => return self.parse_class_interface(),
                KeywordKind::AtImplementation => return self.parse_class_implementation(),
                KeywordKind::AtProtocol => return self.parse_protocol(),
                KeywordKind::AtClass => return self.parse_forward_class(),
                KeywordKind::AtNamespace => return self.parse_namespace(),
                KeywordKind::AtUsing => return self.parse_using(),
                _ => {}
            }
        }

        // Typedef
        if self.match_keyword(KeywordKind::Typedef) {
            // `typedef __attribute__((aligned(8))) unsigned long w;`
            let td_attrs = self.parse_attributes();
            let mut td = self.parse_typedef((decl_line, decl_col))?;
            td.attributes.splice(0..0, td_attrs);
            return Some(td);
        }

        // Struct/union/enum
        if self.match_keyword(KeywordKind::Struct) || self.match_keyword(KeywordKind::Union) {
            let is_union = self.previous.keyword == KeywordKind::Union;
            if self.current.kind == TokenKind::Identifier {
                let name = self.current_text().to_string();
                self.advance();
                if self.check(TokenKind::LBrace) {
                    let fields = self.parse_struct_body(is_union)?;
                    // Post-body attributes: `struct S { ... } __attribute__((packed));`
                    let body_attrs = self.parse_attributes();
                    self.consume(TokenKind::Semicolon, "expected ';' after struct");
                    return Some(CstDecl {
                        kind: CstDeclKind::Struct,
                        line: decl_line, column: decl_col,
                        name: Some(name),
                        next: None,
                        data: CstDeclData::Aggregate { fields, is_union },
                                            attributes: body_attrs,

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
});
                }
                if self.check(TokenKind::Semicolon) {
                    self.advance();
                    return Some(CstDecl {
                        kind: CstDeclKind::Struct,
                        line: decl_line, column: decl_col,
                        name: Some(name),
                        next: None,
                        data: CstDeclData::Aggregate { fields: Vec::new(), is_union },
                                            attributes: Vec::new(),

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
});
                }
                // struct Name var = ... — fall through to regular declaration parsing
                // Restore struct/name tokens so parse_type_full can handle it
                // We need to make the parser think we haven't consumed struct Foo yet
                // By reconstructing the type from what we've consumed
                let struct_type = CstType {
                    prim: TypePrim::Named, is_pointer: false, is_struct: true,
                    tag: if is_union { TagKind::Union } else { TagKind::Struct },
                    name: Some(name.clone()), subtype: None, next: None,
                    block_params: None, is_const: false, is_block: false,
                    is_array: false, array_size: 0, is_volatile: false,
                    is_block_qual: false, is_weak_qual: false, is_unsigned: false,
                    block_name: None, protocols: Vec::new(), type_args: Vec::new(),
                    array_size_name: None, is_fn_ptr: false, is_complex: false,
                    nulls: Nullability::Unspecified, anon_struct_body: None, is_variadic: false,
                };
                // `struct Name *p`, `struct Name **p`, `struct Name *arr[]` —
                // consume the pointer suffix(es) so the declared variable gets
                // the right pointer type (otherwise `struct Name *p` falls out
                // of the declaration path entirely).
                let struct_type = {
                    let mut t = struct_type;
                    while self.match_token(TokenKind::Star) {
                        let mut ptr = CstType::new(TypePrim::Named);
                        ptr.is_pointer = true;
                        ptr.is_struct = true;
                        ptr.name = Some(name.clone());
                        ptr.subtype = Some(Box::new(t));
                        t = ptr;
                    }
                    t
                };
                // Continue to regular declaration parsing with struct_type as the return type
                // (falls through to lines 1892+)
                let qualifiers = self.parse_decl_qualifiers();
                let name = self.parse_qualified_name_with_keywords()?;
                // Function or variable?
                if self.check(TokenKind::LParen) {
                    return self.parse_function_decl_or_definition(struct_type, name, Vec::new(), top_level_async, (decl_line, decl_col));
                } else {
                    // Variable declaration
                    let mut var = CstDecl {
                        kind: CstDeclKind::Variable,
                        line: decl_line, column: decl_col,
                        name: Some(name.clone()),
                        next: None,
                        data: CstDeclData::Variable {
                            var_type: Some(Box::new(struct_type)),
                            initializer: None,
                            is_static: qualifiers.0,
                            is_extern: qualifiers.1,
                            is_const: qualifiers.2,
                            is_block_qual: qualifiers.3,
                            is_weak: qualifiers.4,
                        },
                                            attributes: Vec::new(),

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
};
                    // Array suffix
                    if self.match_token(TokenKind::LBracket) {
                        let mut array_type = CstType::new(TypePrim::Named);
                        if let CstDeclData::Variable { ref mut var_type, .. } = var.data {
                            let base = var_type.take().map(|t| *t).unwrap_or_else(|| CstType::new(TypePrim::Int));
                            array_type.subtype = Some(Box::new(base));
                            array_type.is_array = true;
                            if self.current.kind == TokenKind::Integer {
                                array_type.array_size = self.current_text().parse().unwrap_or(0);
                                self.advance();
                            }
                            *var_type = Some(Box::new(array_type));
                        }
                        self.consume(TokenKind::RBracket, "expected ']'");
                    }
                    // Initializer
                    if self.match_token(TokenKind::Assign) {
                        let init = self.required_expression(|p| p.parse_assignment());
                        if let CstDeclData::Variable { ref mut initializer, .. } = var.data {
                            *initializer = init.map(Box::new);
                        }
                    }
                    // Built before the initializer, so its construction-time end
                    // snapshot under-reports the extent; stamp the real end now.
                    var.end_line = self.previous.end_line;
                    var.end_column = self.previous.end_col;
                    // Comma-separated declarations
                    if self.match_token(TokenKind::Comma) {
                        let mut head = Box::new(var);
                        let mut tail = &mut head;
                        loop {
                            if self.current.kind != TokenKind::Identifier { break; }
                            let n = self.current_text().to_string();
                            self.advance();
                            let mut next_var = CstDecl {
                                kind: CstDeclKind::Variable,
                                line: self.previous.line, column: self.previous.column,
                                name: Some(n),
                                next: None,
                                data: CstDeclData::Variable {
                                    var_type: None,
                                    initializer: None,
                                    is_static: qualifiers.0,
                                    is_extern: qualifiers.1,
                                    is_const: qualifiers.2,
                                    is_block_qual: qualifiers.3,
                                    is_weak: qualifiers.4,
                                },
                                                            attributes: Vec::new(),

                                end_line: self.previous.end_line,
                                end_column: self.previous.end_col,
};
                            if self.match_token(TokenKind::Assign) {
                                if let CstDeclData::Variable { ref mut initializer, .. } = next_var.data {
                                    *initializer = self.required_expression(|p| p.parse_assignment()).map(Box::new);
                                }
                            }
                            next_var.end_line = self.previous.end_line;
                            next_var.end_column = self.previous.end_col;
                            tail.next = Some(Box::new(next_var));
                            tail = tail.next.as_mut().unwrap();
                            if !self.match_token(TokenKind::Comma) { break; }
                        }
                        self.consume(TokenKind::Semicolon, "expected ';' at end of declaration");
                        return Some(*head);
                    }
                    self.consume(TokenKind::Semicolon, "expected ';' at end of declaration");
                    return Some(var);
                }
            }
            if let Some(fields) = self.parse_struct_body(is_union) {
                // Post-body attributes: `struct { ... } __attribute__((packed));`
                let body_attrs = self.parse_attributes();
                self.consume(TokenKind::Semicolon, "expected ';' after struct");
                return Some(CstDecl {
                    kind: CstDeclKind::Struct,
                    line: decl_line, column: decl_col,
                    name: None,
                    next: None,
                    data: CstDeclData::Aggregate { fields, is_union },
                                    attributes: body_attrs,

                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
            return None;
        }
        if self.match_keyword(KeywordKind::Enum) {
            return self.parse_enum();
        }

        // `async` detection moved to the top of parse_declaration_inner
        // (struct fallthrough path needs it too).

        // Regular declaration: type name = ...; or type name(params) { ... }
        let qualifiers = self.parse_decl_qualifiers();
        // Annotated: a top-level function's return type is a nullability
        // position (`nullable NPString *f(void)` / `NPString * _Nullable f(void)`).
        // Without this the annotation landed nowhere and the return direction of
        // the diagnostic could never fire.
        let return_type = match self.parse_type_annotated() {
            Some(t) => t,
            None => {
                // Fallback: treat unknown identifier as type name (like C parser)
                if self.current.kind == TokenKind::Identifier {
                    let tname = self.current_text().to_string();
                    self.advance();
                    let mut t = CstType::new(TypePrim::Named);
                    t.name = Some(tname);
                    t
                } else {
                    return None;
                }
            }
        };
        // (Declarator-position `const`/`volatile` are consumed inside
        // parse_type_full — qualifier between base and stars qualifies the
        // base, and must not be read as the declarator's name.)
        // Mid-declaration attributes between return type and name:
        // `void *__attribute__((warn_unused_result)) f(void);`
        let mid_attrs = self.parse_attributes();
        let name = if return_type.block_name.is_some() {
            // Block type consumed the name as block_name (e.g., int (^name)(params))
            return_type.block_name.clone().unwrap()
        } else {
            self.parse_qualified_name_with_keywords()?
        };

        // Function or variable?
        if self.check(TokenKind::LParen) {
            self.parse_function_decl_or_definition(return_type, name, mid_attrs, top_level_async, (decl_line, decl_col))
        } else {
            // Variable declaration
            let mut var = CstDecl {
                kind: CstDeclKind::Variable,
                line: decl_line, column: decl_col,
                name: Some(name.clone()),
                next: None,
                data: CstDeclData::Variable {
                    var_type: Some(Box::new(return_type)),
                    initializer: None,
                    is_static: qualifiers.0,
                    is_extern: qualifiers.1,
                    is_const: qualifiers.2,
                    is_block_qual: qualifiers.3,
                    is_weak: qualifiers.4,
                },
                            attributes: mid_attrs.clone(),

                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
};

            // Array suffix: name[size]
            if self.match_token(TokenKind::LBracket) {
                let mut array_type = CstType::new(TypePrim::Named);
                if let CstDeclData::Variable { ref mut var_type, .. } = var.data {
                    let base = var_type.take().map(|t| *t).unwrap_or_else(|| CstType::new(TypePrim::Int));
                    array_type.subtype = Some(Box::new(base));
                    array_type.is_array = true;
                    if self.current.kind == TokenKind::Integer {
                        array_type.array_size = self.current_text().parse().unwrap_or(0);
                        self.advance();
                    } else if self.current.kind == TokenKind::Identifier {
                        array_type.array_size_name = Some(self.current_text().to_string());
                        self.advance();
                    }
                    *var_type = Some(Box::new(array_type));
                }
                self.consume(TokenKind::RBracket, "expected ']'");
            }

            // Initializer
            if self.match_token(TokenKind::Assign) {
                let init = self.required_expression(|p| p.parse_assignment());
                if let CstDeclData::Variable { ref mut initializer, .. } = var.data {
                    *initializer = init.map(Box::new);
                }
            }
            // Built before the initializer — stamp the real end (see above).
            var.end_line = self.previous.end_line;
            var.end_column = self.previous.end_col;

            // Comma-separated declarations
            if self.match_token(TokenKind::Comma) {
                let mut head = Box::new(var);
                // Base type (without per-declarator pointer/array decorations)
                // cloned from the head so each subsequent declarator gets its
                // own `*`/`[]` applied on top — `T *a = x, *b = y;` and
                // `T a[N], b[M];` must work like C.
                let base_type: Option<CstType> = match &head.data {
                    CstDeclData::Variable { var_type, .. } => var_type.as_deref().cloned(),
                    _ => None,
                };
                let mut tail = &mut head;
                loop {
                    // Optional per-declarator pointer stars: `T *b`, `T **b`
                    let mut decl_type = base_type.clone();
                    let mut stars = 0usize;
                    while self.match_token(TokenKind::Star) { stars += 1; }
                    // Per-declarator const/volatile (`int *const a, *const b;`)
                    // — qualifier on THIS declarator's pointer level, and must
                    // not be read as the declarator's name.
                    let mut decl_const = false;
                    let mut decl_volatile = false;
                    while {
                        let mut ate = false;
                        if self.match_keyword(KeywordKind::Const) { decl_const = true; ate = true; }
                        if self.match_keyword(KeywordKind::Volatile) { decl_volatile = true; ate = true; }
                        ate
                    } {}
                    for _ in 0..stars {
                        let mut ptr = CstType::new(TypePrim::Named);
                        ptr.is_pointer = true;
                        ptr.is_const = decl_const;
                        ptr.is_volatile = decl_volatile;
                        ptr.subtype = Some(Box::new(decl_type.take().unwrap_or_else(|| CstType::new(TypePrim::Int))));
                        decl_type = Some(ptr);
                    }
                    // Bare const on a non-pointer declarator (`const int a, b;`
                    // is normally handled by the leading-qualifier path, but a
                    // mid-list `T const b` must qualify b's type too).
                    if (decl_const || decl_volatile) && stars == 0 {
                        if let Some(dt) = decl_type.as_mut() {
                            if decl_const { dt.is_const = true; }
                            if decl_volatile { dt.is_volatile = true; }
                        }
                    }
                    if self.current.kind != TokenKind::Identifier {
                        // Not another declarator — restore position is not
                        // possible, so treat as a syntax error like C would.
                        self.consume(TokenKind::Identifier, "expected identifier in declaration list");
                        break;
                    }
                    let n = self.current_text().to_string();
                    self.advance();
                    let mut next_var = CstDecl {
                        kind: CstDeclKind::Variable,
                        line: self.previous.line, column: self.previous.column,
                        name: Some(n),
                        next: None,
                        data: CstDeclData::Variable {
                            var_type: decl_type.map(Box::new), // None inherits type from first
                            initializer: None,
                            is_static: qualifiers.0,
                            is_extern: qualifiers.1,
                            is_const: qualifiers.2,
                            is_block_qual: qualifiers.3,
                            is_weak: qualifiers.4,
                        },
                                            attributes: Vec::new(),

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
};
                    // Array suffix on this declarator: `T a[2], b[3];`
                    if self.match_token(TokenKind::LBracket) {
                        let mut array_type = CstType::new(TypePrim::Named);
                        array_type.is_array = true;
                        if self.current.kind == TokenKind::Integer {
                            array_type.array_size = self.current_text().parse().unwrap_or(0);
                            self.advance();
                        } else if self.current.kind == TokenKind::Identifier {
                            array_type.array_size_name = Some(self.current_text().to_string());
                            self.advance();
                        }
                        let inner = match &mut next_var.data {
                            CstDeclData::Variable { var_type, .. } => {
                                var_type.take().map(|t| *t).unwrap_or_else(|| CstType::new(TypePrim::Int))
                            }
                            _ => CstType::new(TypePrim::Int),
                        };
                        array_type.subtype = Some(Box::new(inner));
                        if let CstDeclData::Variable { ref mut var_type, .. } = next_var.data {
                            *var_type = Some(Box::new(array_type));
                        }
                        self.consume(TokenKind::RBracket, "expected ']'");
                    }
                    if self.match_token(TokenKind::Assign) {
                        if let CstDeclData::Variable { ref mut initializer, .. } = next_var.data {
                            *initializer = self.required_expression(|p| p.parse_assignment()).map(Box::new);
                        }
                    }
                    next_var.end_line = self.previous.end_line;
                    next_var.end_column = self.previous.end_col;
                    tail.next = Some(Box::new(next_var));
                    tail = tail.next.as_mut().unwrap();
                    if !self.match_token(TokenKind::Comma) { break; }
                }
                self.consume(TokenKind::Semicolon, "expected ';' at end of declaration");
                return Some(*head);
            }

            self.consume(TokenKind::Semicolon, "expected ';' at end of declaration");
            Some(var)
        }
    }

    fn parse_decl_qualifiers(&mut self) -> (bool, bool, bool, bool, bool) {
        let mut is_static = false;
        let mut is_extern = false;
        let mut is_const = false;
        let mut is_block = false;
        let mut is_weak = false;
        loop {
            if self.match_keyword(KeywordKind::Static) { is_static = true; }
            else if self.match_keyword(KeywordKind::Extern) { is_extern = true; }
            else if self.match_keyword(KeywordKind::Const) { is_const = true; }
            else if self.match_keyword(KeywordKind::Block) { is_block = true; }
            else if self.match_keyword(KeywordKind::Weak) { is_weak = true; }
            else { break; }
        }
        (is_static, is_extern, is_const, is_block, is_weak)
    }

    fn parse_typedef(&mut self, start: (usize, usize)) -> Option<CstDecl> {
        // `typedef enum {...} Alias;` — delegate to parse_enum (which handles
        // optional tag name + brace body + members), then take trailing alias.
        // The struct/union branch below only handles Struct/Union, so without
        // this Enum branch `typedef enum {...} GameState;` fell through to
        // parse_type_full and dropped all enum members.
        if self.current.kind == TokenKind::Keyword && self.current.keyword == KeywordKind::Enum {
            // Consume the `enum` keyword so parse_enum sees the tag name or `{`
            // (parse_enum expects current to be IDENT or LBrace, NOT the enum kw).
            self.advance();
            // `typedef enum Tag Alias;` — a tag reference with a trailing alias and
            // NO body. parse_enum's tag-reference path expects `;` immediately after
            // the tag, so it trips on the alias. Detect this two-identifier form and
            // build the typedef directly; the tag itself is normally defined elsewhere
            // (e.g. `enum Tag {…};` a few lines up), so only the alias needs a name.
            if self.current.kind == TokenKind::Identifier {
                let saved_lex = self.lexer.save_pos();
                let saved_tok = self.current.clone();
                let tag = self.current_text().to_string();
                self.advance();
                if self.current.kind == TokenKind::Identifier {
                    let alias = self.current_text().to_string();
                    self.advance();
                    self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
                    self.add_type_name(&tag);
                    self.add_type_name(&alias);
                    return Some(CstDecl {
                        kind: CstDeclKind::Typedef,
                        line: start.0, column: start.1,
                        end_line: self.previous.end_line, end_column: self.previous.end_col,
                        name: Some(alias.clone()),
                        next: None,
                        data: CstDeclData::Typedef {
                            alias_type: Some(Box::new(CstType {
                                prim: TypePrim::Named, is_struct: true,
                                tag: TagKind::Enum,
                                name: Some(tag),
                                ..CstType::new(TypePrim::Named)
                            })),
                            struct_fields: Vec::new(),
                        },
                        attributes: Vec::new(),
                    });
                }
                // Not the alias form (`enum Tag {` or `enum Tag;`) — rewind.
                self.lexer.restore_pos(saved_lex);
                self.current = saved_tok;
            }
            let mut enum_decl = self.parse_enum()?;
            if self.current.kind == TokenKind::Identifier {
                let alias = self.current_text().to_string();
                self.advance();
                self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
                self.add_type_name(&alias);
                if let CstDeclData::Enum { .. } = enum_decl.data {
                    enum_decl.name = Some(alias);
                }
                return Some(enum_decl);
            }
            self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
            return Some(enum_decl);
        }
        if self.current.kind == TokenKind::Keyword &&
           (self.current.keyword == KeywordKind::Struct ||
            self.current.keyword == KeywordKind::Union) {
            let is_union = self.current.keyword == KeywordKind::Union;
            self.advance();
            let _anon = false;
            if self.current.kind == TokenKind::Identifier {
                // Named struct
                let name = self.current_text().to_string();
                self.advance();
                if self.check(TokenKind::LBrace) {
                    let struct_decl = self.parse_struct_body(is_union)?;
                    self.add_type_name(&name);
                    // Parse typedef name
if self.current.kind == TokenKind::Identifier {
                    let alias = self.current_text().to_string();
                    self.advance();
                    self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
                    self.add_type_name(&name);
                    self.add_type_name(&alias);
                        return Some(CstDecl {
                            kind: CstDeclKind::Typedef,
                            line: start.0, column: start.1,
                            end_line: self.previous.end_line, end_column: self.previous.end_col,
                            name: Some(alias),
                            next: None,
                            data: CstDeclData::Typedef {
                                alias_type: Some(Box::new(CstType {
                                    prim: TypePrim::Named, is_struct: true,
                                    name: Some(name),
                                    ..CstType::new(TypePrim::Named)
                                })),
                                struct_fields: struct_decl,
                            },
                                                    attributes: Vec::new(),
});
                    }
                    self.consume(TokenKind::Semicolon, "expected ';' after struct");
                    return Some(CstDecl {
                        kind: CstDeclKind::Struct,
                        line: start.0, column: start.1,
                        name: Some(name),
                        next: None,
                        data: CstDeclData::Aggregate { fields: struct_decl, is_union },
                                            attributes: Vec::new(),

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
});
                }
                // `typedef struct Tag Alias;` — tag reference with an alias
                // (the tag may be only forward-declared). Emit a Typedef whose
                // alias_type is `struct Tag`; the forward-declared Struct for
                // the tag itself is emitted separately by consumers.
                if self.current.kind == TokenKind::Identifier {
                    let alias = self.current_text().to_string();
                    self.advance();
                    self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
                    self.add_type_name(&alias);
                    return Some(CstDecl {
                        kind: CstDeclKind::Typedef,
                        line: start.0, column: start.1,
                        end_line: self.previous.end_line, end_column: self.previous.end_col,
                        name: Some(alias),
                        next: None,
                        data: CstDeclData::Typedef {
                            alias_type: Some(Box::new(CstType {
                                prim: TypePrim::Named, is_struct: true,
                                name: Some(name.clone()),
                                ..CstType::new(TypePrim::Named)
                            })),
                            struct_fields: Vec::new(),
                        },
                        attributes: Vec::new(),
                    });
                }
                // Forward declaration
                self.consume(TokenKind::Semicolon, "expected ';' after struct");
                return Some(CstDecl {
                    kind: CstDeclKind::Struct,
                    line: start.0, column: start.1,
                    name: Some(name),
                    next: None,
                    data: CstDeclData::Aggregate { fields: Vec::new(), is_union },
                                    attributes: Vec::new(),

                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
            // Anonymous struct/union
            if self.check(TokenKind::LBrace) {
                let fields = self.parse_struct_body(is_union)?;
                if self.current.kind == TokenKind::Identifier {
                    let alias = self.current_text().to_string();
                    self.advance();
                    self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
                    self.add_type_name(&alias);
                    return Some(CstDecl {
                        kind: CstDeclKind::Typedef,
                        line: start.0, column: start.1,
                        name: Some(alias),
                        next: None,
                        data: CstDeclData::Typedef {
                            alias_type: None,
                            struct_fields: fields,
                        },
                                            attributes: Vec::new(),

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
});
                }
                self.consume(TokenKind::Semicolon, "expected ';' after struct");
                return Some(CstDecl {
                    kind: CstDeclKind::Struct,
                    line: start.0, column: start.1,
                    name: None,
                    next: None,
                    data: CstDeclData::Aggregate { fields, is_union },
                                    attributes: Vec::new(),

                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
            self.consume(TokenKind::Semicolon, "expected ';' after struct");
            return None;
        }

        // typedef type name;
        // Support `typedef enum { ... } Name;` — parse the enum body inline,
        // then take the trailing identifier as the alias name. Return an Enum
        // decl (not Typedef) so elaborator/codegen emit `enum Name {...};` +
        // `typedef enum Name Name;` and the members are preserved. Returning
        // Typedef with `alias_type=None` previously dropped all enum members.
        if self.check_keyword(KeywordKind::Enum) {
            let mut enum_decl = self.parse_enum()?;
            // Optional trailing alias identifier: `} AliasName;`
            if self.current.kind == TokenKind::Identifier {
                let alias = self.current_text().to_string();
                self.advance();
                self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
                self.add_type_name(&alias);
                // Override the enum decl's name to the trailing alias so codegen
                // emits `typedef enum Alias Alias;` matching the source form.
                if let CstDeclData::Enum { .. } = enum_decl.data {
                    enum_decl.name = Some(alias);
                }
                return Some(enum_decl);
            }
            self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
            return Some(enum_decl);
        }

        // typedef type name;
        // Annotated: a block typedef's result type is a nullability position —
        // `typedef nullable NPString * (^Getter)(void);` is the block equivalent
        // of a nullable return type. Without this the leading annotation landed
        // nowhere and the typedef failed to parse.
        let alias_type = self.parse_type_annotated();
        // Check if the type has a block_name (e.g. typedef int (^name)(params) → name is the alias)
        if let Some(ref at) = alias_type {
            if let Some(ref block_name) = at.block_name {
                let name = block_name.clone();
                self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
                self.add_type_name(&name);
                return Some(CstDecl {
                    kind: CstDeclKind::Typedef,
                    line: start.0, column: start.1,
                    name: Some(name),
                    next: None,
                    data: CstDeclData::Typedef {
                        alias_type: Some(Box::new(at.clone())),
                        struct_fields: Vec::new(),
                    },
                                    attributes: Vec::new(),

                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
        }
        if self.current.kind == TokenKind::Identifier {
            let name = self.current_text().to_string();
            self.advance();
            // typedef array: `typedef int Row4[4];` — the array size(s) follow
            // the typedef name and attach to the aliased type.
            let mut at = alias_type;
            while self.match_token(TokenKind::LBracket) {
                let mut arr = CstType::new(TypePrim::Named);
                arr.subtype = Some(Box::new(at.unwrap_or_else(|| CstType::new(TypePrim::Int))));
                arr.is_array = true;
                if self.current.kind == TokenKind::Integer {
                    arr.array_size = self.current_text().parse().unwrap_or(0);
                    self.advance();
                } else if self.current.kind == TokenKind::Identifier {
                    arr.array_size_name = Some(self.current_text().to_string());
                    self.advance();
                }
                at = Some(arr);
                self.consume(TokenKind::RBracket, "expected ']'");
            }
            self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
            self.add_type_name(&name);
            return Some(CstDecl {
                kind: CstDeclKind::Typedef,
                line: start.0, column: start.1,
                name: Some(name),
                next: None,
                data: CstDeclData::Typedef {
                    alias_type: at.map(Box::new),
                    struct_fields: Vec::new(),
                },
                            attributes: Vec::new(),

                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        self.consume(TokenKind::Semicolon, "expected ';' after top level declarator");
        None
    }

    fn parse_struct_body(&mut self, _is_union: bool) -> Option<Vec<CstDecl>> {
        self.advance(); // consume {
        let mut fields = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            self.match_keyword(KeywordKind::Extension); // __extension__ prefix
            if let Some(ftype) = self.parse_type_full() {
                // Field name: either a trailing identifier, or the name
                // embedded in a function-pointer/block declarator type
                // (`int (*cb)(int)` / `void (^blk)(int)`).
                let fname = if let Some(ref bn) = ftype.block_name {
                    bn.clone()
                } else if self.current.kind == TokenKind::Identifier ||
                          (self.current.kind == TokenKind::Keyword &&
                           self.current.keyword == KeywordKind::Id) {
                    let n = self.current_text().to_string();
                    self.advance();
                    n
                } else {
                    String::new()
                };
                if !fname.is_empty() {
                    let mut field_type = ftype;
                    // Array field(s): name[size1][size2]... — loop so
                    // multi-dimensional arrays nest correctly.
                    while self.match_token(TokenKind::LBracket) {
                        let mut array_type = CstType::new(TypePrim::Named);
                        array_type.subtype = Some(Box::new(field_type));
                        array_type.is_array = true;
                        if self.current.kind == TokenKind::Integer {
                            array_type.array_size = self.current_text().parse().unwrap_or(0);
                            self.advance();
                        } else if self.current.kind == TokenKind::Identifier {
                            array_type.array_size_name = Some(self.current_text().to_string());
                            self.advance();
                        }
                        field_type = array_type;
                        self.consume(TokenKind::RBracket, "expected ']'");
                    }
                    // Bitfield: `unsigned a : 3;` — the width is REAL C17
                    // 6.7.2.1 semantics, not a downgrade: record it as a
                    // `: N` attribute on the field so codegen emits the
                    // `: N` spelling and the layout engine can compute
                    // bit-granular offsets (clangd-parity hover shows
                    // "3 bits (+29 bits padding)"). Constant widths only:
                    // an identifier width (macro) is recorded verbatim and
                    // left for the layout engine to skip (not folded).
                    let mut field_attrs: Vec<String> = Vec::new();
                    while self.match_token(TokenKind::Colon) {
                        // Constant width: a literal (possibly with the
                        // arithmetic shapes real headers use).
                        let width_text = if self.check(TokenKind::Integer) {
                            let w = self.current_text().to_string();
                            self.advance();
                            // Constant-expression tail: + - * << >> | &
                            let mut expr = w;
                            while matches!(self.current.kind,
                                TokenKind::Plus | TokenKind::Minus | TokenKind::Star |
                                TokenKind::LShift | TokenKind::RShift |
                                TokenKind::Pipe | TokenKind::Ampersand)
                            {
                                let op = self.current_text().to_string();
                                self.advance();
                                if self.check(TokenKind::Integer) {
                                    expr.push_str(&format!("{op}{}", self.current_text()));
                                    self.advance();
                                } else { break; }
                            }
                            // Fold simple constant expressions (no eval engine
                            // needed for the shapes C headers use: `3`,
                            // `1+3`, `8*sizeof(int)` is NOT folded — recorded).
                            if expr.chars().all(|c| c.is_ascii_digit()) {
                                expr
                            } else {
                                expr.clone()
                            }
                        } else if self.check(TokenKind::Identifier) {
                            self.current_text().to_string()
                        } else {
                            String::new()
                        };
                        if !width_text.is_empty() {
                            field_attrs.push(format!(": {width_text}"));
                        }
                    }
                    field_attrs.extend(self.parse_attributes());
                    fields.push(CstDecl {
                        kind: CstDeclKind::Ivar,
                        line: self.previous.line, column: self.previous.column,
                        name: Some(fname),
                        next: None,
                        data: CstDeclData::Ivar {
                            ivar_type: Some(Box::new(field_type)),
                            iboutlet: false,
                            is_weak: false,
                        },
                        attributes: field_attrs,

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
});
                }
                // Handle comma-separated fields
                while self.match_token(TokenKind::Comma) {
                    if self.current.kind == TokenKind::Identifier {
                        let fname = self.current_text().to_string();
                        self.advance();
                        let field_attrs = self.parse_attributes();
                        fields.push(CstDecl {
                            kind: CstDeclKind::Ivar,
                            line: self.previous.line, column: self.previous.column,
                            name: Some(fname),
                            next: None,
                            data: CstDeclData::Ivar {
                                ivar_type: None,
                                iboutlet: false,
                                is_weak: false,
                            },
                            attributes: field_attrs,

                            end_line: self.previous.end_line,
                            end_column: self.previous.end_col,
});
                    }
                }
            } else if !self.check(TokenKind::Semicolon) && !self.check(TokenKind::RBrace) {
                break;
            }
            if self.check(TokenKind::Semicolon) {
                self.advance();
            } else {
                self.consume(TokenKind::Semicolon, "expected ';' after field");
            }
            // NOTE: do not skip qualifiers here. A `const` / `volatile` right
            // after the `;` belongs to the NEXT field (`const char *a; const
            // char *b;`) — eating it silently dropped the second field's const,
            // and the vtable's `const char *(*)(…)` members then mismatched
            // their implementations.
        }
                self.consume(TokenKind::RBrace, "expected '}'");
        Some(fields)
    }
    fn parse_enum(&mut self) -> Option<CstDecl> {
        let (en_line, en_col) = (self.previous.line, self.previous.column);
        let mut members = Vec::new();
        let mut values = Vec::new();
        // Optional tag name: `enum Foo { ... }` vs anonymous `enum { ... }`
        // (the latter arises from `typedef enum { ... } Alias;` where `enum`
        // is immediately followed by `{`). The previous code only handled the
        // tagged form and returned None for anonymous enums, dropping the
        // entire decl from the output (cf. old C parser line ~2336 which
        // proceeds into the brace body when tag is NULL).
        let mut name: Option<String> = None;
        if self.current.kind == TokenKind::Identifier {
            name = Some(self.current_text().to_string());
            self.advance();
            // For a tagged enum that is NOT followed by `{`, treat as forward
            // declaration: `enum Foo;` — register the tag and return an empty
            // enum decl so the name resolves.
            if !self.check(TokenKind::LBrace) {
                if let Some(ref n) = name { self.add_type_name(n); }
                self.consume(TokenKind::Semicolon, "expected ';' after enum");
                return Some(CstDecl {
                    kind: CstDeclKind::Enum,
                    line: en_line, column: en_col,
                    name,
                    next: None,
                    data: CstDeclData::Enum { members, values },
                                    attributes: Vec::new(),

                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
});
            }
        }
        // Body: `{ member = value, ... }` — tagged or anonymous
        if self.check(TokenKind::LBrace) {
            self.advance();
            let mut next_val: i64 = 0;
            // Enumerators already assigned in THIS enum, so a later member may
            // refer to an earlier one (`enum { A = 1, B = A + 1 }`).
            let mut seen: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
            while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                if self.current.kind == TokenKind::Identifier ||
                   (self.current.kind == TokenKind::Keyword &&
                    self.current.keyword == KeywordKind::Id) {
                    // `id` as an enumerator name: constant-name position, the
                    // builtin type never wins here (C superset: `enum E { id };`).
                    let mname = self.current_text().to_string();
                    self.advance();
                    members.push(mname.clone());
                    if self.match_token(TokenKind::Assign) {
                        // C: an enumerator value is a `constant-expression`
                        // (= `conditional-expression`, which excludes the comma
                        // operator). Parse at that level — a lone integer token
                        // is only its simplest case — then fold it.
                        //
                        // An unfoldable value is *reported*. The previous
                        // single-token path silently fell back to the
                        // auto-increment value for anything it did not
                        // recognise, so `enum { A = 1, B = 0x10, C }` compiled
                        // to A=1, B=2, C=3 with no diagnostic at all.
                        let val_token = self.current.clone();
                        let folded = match self.parse_conditional() {
                            Some(e) => Self::fold_enum_const(&e, &seen),
                            None => None,
                        };
                        match folded {
                            Some(v) => next_val = v,
                            None => self.error_at(
                                &val_token,
                                "enumerator value is not an integer constant expression",
                            ),
                        }
                    }
                    seen.insert(mname, next_val);
                    values.push(CstExpr {
                        kind: CstExprKind::Integer, expr_type: None,
                        line: self.previous.line, col: self.previous.column,
                        data: CstExprData::Integer(next_val),
                    
                        end_line: self.previous.end_line,
                        end_col: self.previous.end_col,
});
                    next_val += 1;
                    if !self.match_token(TokenKind::Comma) { break; }
                } else {
                    break;
                }
            }
            self.consume(TokenKind::RBrace, "expected '}'");
            // Do NOT consume `;` here — `typedef enum {...} Alias;` flow needs
            // to read the trailing alias identifier first (handled by caller
            // `parse_typedef`). For a bare `enum Foo {...};` decl, the caller
            // (`parse_declaration`) consumes the `;`.
            return Some(CstDecl {
                kind: CstDeclKind::Enum,
                line: en_line, column: en_col,
                name,
                next: None,
                data: CstDeclData::Enum { members, values },
                            attributes: Vec::new(),

                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        None
    }

    // ─── @interface / @implementation / @protocol parsing ──────────────

    fn parse_class_interface(&mut self) -> Option<CstDecl> {
        let (ci_line, ci_col) = (self.current.line, self.current.column);
        self.advance(); // consume @interface

        if !self.match_name() { self.error("expected class name after @interface"); return None; }
        let mut name = self.previous_text().to_string();
        // Check for qualified name (Namespace::ClassName)
        while self.match_token(TokenKind::ColonColon) {
            name.push_str("::");
            if self.match_name() {
                name.push_str(self.previous_text());
            } else {
                self.error("expected identifier after '::'");
                return None;
            }
        }
        self.add_type_name(&name);

        // Check for category: @interface ClassName (CategoryName)
        let mut category_name = None;
        if self.match_token(TokenKind::LParen) {
            // Category name — may contain non-ASCII characters, consume until )
            let mut cat = String::new();
            while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                if self.current.kind == TokenKind::Identifier ||
                   self.current.kind == TokenKind::Keyword {
                    cat.push_str(self.current_text());
                } else {
                    // Non-ASCII chars: just consume the raw text
                    let start = self.current.start;
                    let end = if self.current.length > 0 { start + self.current.length } else { start };
                    if end > start {
                        if let Some(s) = self.source.get(start..end) {
                            cat.push_str(s);
                        }
                    }
                }
                self.advance();
            }
            self.consume(TokenKind::RParen, "expected ')'");
            category_name = if cat.is_empty() { None } else { Some(cat) };
        }

        // Generic type params: <T> — OR protocol list: <Proto1, Proto2>.
        // ObjC has no generics; a `<...>` block whose names are already
        // declared types (e.g. an earlier `@protocol Drawable`) is a protocol
        // conformance list, not type params. `@protocol` registers its name
        // via add_type_name, so is_type_name disambiguates. Unknown names
        // (e.g. `Box<T>`) stay generic type params.
        let mut type_params = Vec::new();
        let mut type_bounds: Vec<(String, String)> = Vec::new();
        let mut protocols = Vec::new();
        if self.match_token(TokenKind::Less) {
            let mut names: Vec<String> = Vec::new();
            let mut bounds: Vec<(String, String)> = Vec::new();
            while self.current.kind == TokenKind::Identifier ||
                  (self.current.kind == TokenKind::Keyword &&
                   matches!(self.current.keyword, KeywordKind::Id | KeywordKind::Class |
                    KeywordKind::Sel | KeywordKind::Instancetype)) {
                let tp = self.current_text().to_string();
                self.advance();
                names.push(tp.clone());
                // Optional bound: `T : Greetable` (bare protocol), `T : id<Greetable>`
                // (ObjC spelling — the shell is stripped, jeti protocol types are
                // compile-time labels so both spellings are equivalent), or
                // `T : NSObject *` (class-pointer bound, stored bare). A protocol
                // conformance list never carries a colon, so this only fires for
                // real type params. Unknown/unresolvable bound names are kept as
                //-is; the checker's escape channel lets them pass.
                if self.match_token(TokenKind::Colon) {
                    if self.current.kind == TokenKind::Keyword
                        && self.current.keyword == KeywordKind::Id {
                        // ObjC spelling `T : id<P, Q>` — the shell is stripped
                        // (jeti protocol types are compile-time labels, so
                        // `id<P>` and bare `P` are equivalent); each protocol
                        // in the conjunction becomes its own bound entry.
                        self.advance();
                        self.consume(TokenKind::Less, "expected '<'");
                        while self.current.kind == TokenKind::Identifier {
                            let bound = self.current_text().to_string();
                            self.advance();
                            bounds.push((tp.clone(), bound));
                            if !(self.match_token(TokenKind::Comma) || self.match_token(TokenKind::Ampersand)) { break; }
                        }
                        self.consume(TokenKind::Greater, "expected '>'");
                    } else if self.current.kind == TokenKind::Identifier {
                        // Bare protocol name, or class-pointer spelling
                        // `NSObject *` — the `*` is stripped; storage is
                        // always the bare name.
                        let bound = self.current_text().to_string();
                        self.advance();
                        while self.match_token(TokenKind::Star) {}
                        bounds.push((tp.clone(), bound));
                    }
                }
                // `<P & Q>` protocol intersection: `&` joins protocol names
                // into one conjunction list (same semantics as `,`).
                if !(self.match_token(TokenKind::Comma) || self.match_token(TokenKind::Ampersand)) { break; }
            }
            self.consume(TokenKind::Greater, "expected '>'");
            // Protocol names come from `@protocol` (add_type_name); type-param
            // names ALSO end up in type_names once their class finishes parsing
            // (below), so `@interface B<T>` after `@interface A<T>` would see
            // `T` as a "declared type" and misroute it to protocols — leaving
            // B's type_params empty even though its `B<int>` uses still
            // monomorphize. A current type-param name therefore wins over the
            // protocol reading.
            let all_protos = !names.is_empty()
                && names.iter().all(|n| self.is_type_name(n) && !self.is_type_param(n));
            if all_protos {
                protocols = names;
            } else {
                for tp in &names {
                    self.add_type_param(tp);
                }
                type_params = names;
                type_bounds = bounds;
            }
        }

        // Register class as generic if it has type params
        if !type_params.is_empty() && !self.generic_class_names.contains(&name) {
            self.generic_class_names.push(name.clone());
        }

        // Superclass: : SuperClassName (may be a qualified name like Engine::Graphics::RenderNode)
        let mut superclass = None;
        if self.match_token(TokenKind::Colon) {
            if self.current.kind == TokenKind::Identifier {
                superclass = self.parse_qualified_name();
            }
        }

        // Protocols after superclass: `@interface X : Super <Proto1, Proto2>`
        // (merged with any protocol list parsed before the colon).
        if self.match_token(TokenKind::Less) {
            while self.current.kind == TokenKind::Identifier {
                let p = self.current_text().to_string();
                self.advance();
                protocols.push(p);
                // `<P & Q>` intersection joins into the same conjunction list.
                if !(self.match_token(TokenKind::Comma) || self.match_token(TokenKind::Ampersand)) { break; }
            }
            // Check for > or >> (>> is parsed as two > tokens)
            if self.current.kind == TokenKind::Greater {
                self.advance();
            } else if self.current.kind == TokenKind::RShift {
                // >> is two > tokens
                self.advance();
            }
        }

        // Ivars: { ... }
        let mut ivars = Vec::new();
        if self.match_token(TokenKind::LBrace) {
            while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
                // ivar qualifiers: @public, @protected, @private, @package
                if self.match_keyword(KeywordKind::AtPublic) ||
                   self.match_keyword(KeywordKind::AtProtected) ||
                   self.match_keyword(KeywordKind::AtPrivate) ||
                   self.match_keyword(KeywordKind::AtPackage) {
                    continue;
                }
                self.match_keyword(KeywordKind::Extension); // __extension__ prefix
                // ivar attribute parens: `@public (weak) DirectoryNode *_parent;`
                // Parse the qualifier list to detect `weak`.
                let mut is_weak = false;
                if self.match_token(TokenKind::LParen) {
                    while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                        if (self.current.kind == TokenKind::Identifier || self.current.kind == TokenKind::Keyword)
                            && self.current_text() == "weak" {
                            is_weak = true;
                        }
                        self.advance();
                    }
                    self.consume(TokenKind::RParen, "expected ')'");
                }
                // IBOutlet qualifier
                let iboutlet = false;
                let saved_ivar = self.ivar_position;
                self.ivar_position = true;
                let ivar_type = self.parse_type_annotated();
                self.ivar_position = saved_ivar;
                if let Some(ivar_type) = ivar_type {
                    while self.match_name() {
                        let ivar_name = self.previous_text().to_string();
                        let mut final_type = ivar_type.clone();
                        // Check for array suffix: name[size]
                        if self.match_token(TokenKind::LBracket) {
                            let mut arr_type = CstType::new(TypePrim::Named);
                            arr_type.subtype = Some(Box::new(final_type));
                            arr_type.is_array = true;
                            // Array size may be an integer literal OR a macro/enum
                            // constant identifier (e.g. `FSNode *_children[MAX_CHILDREN];`).
                            // The previous code only accepted Integer tokens, leaving the
                            // identifier in place so `consume(RBracket)` failed with
                            // `expected ']' after array size (got identifier)`.
                            if self.current.kind == TokenKind::Integer {
                                arr_type.array_size = self.current_text().parse().unwrap_or(0);
                                self.advance();
                            } else if self.current.kind == TokenKind::Identifier {
                                // Symbolic size — record the identifier name so codegen
                                // can emit `T[MAX_CHILDREN]` instead of `T[]` (flexible
                                // array member, which C forbids for non-trailing fields).
                                arr_type.array_size = 0;
                                arr_type.array_size_name = Some(self.current_text().to_string());
                                self.advance();
                            }
                            self.consume(TokenKind::RBracket, "expected ']'");
                            final_type = arr_type;
                        }
                        ivars.push(CstDecl {
                            kind: CstDeclKind::Ivar,
                            line: ci_line, column: ci_col,
                            name: Some(ivar_name),
                            next: None,
                            data: CstDeclData::Ivar {
                                ivar_type: Some(Box::new(final_type)),
                                iboutlet,
                                is_weak,
                            },
                                                    attributes: Vec::new(),

                            end_line: self.previous.end_line,
                            end_column: self.previous.end_col,
});
                        if !self.match_token(TokenKind::Comma) { break; }
                    }
                }
                // Zero-progress guard: a failed consume leaves the current
                // token in place; without this the ivar loop spins forever on
                // any unparseable declaration (observed with
                // `void (^ *b)(void);` before the block-pointer fix).
                let before = self.current.start;
                self.consume(TokenKind::Semicolon, "expected ';' after ivar");
                if self.current.start == before
                    && !self.check(TokenKind::RBrace)
                    && !self.check(TokenKind::Eof)
                {
                    self.advance();
                }
            }
            self.consume(TokenKind::RBrace, "expected '}'");
        }

        // Properties and methods
        let mut properties = Vec::new();
        let mut methods = Vec::new();
        while !self.match_keyword(KeywordKind::AtEnd) && !self.check(TokenKind::Eof) {
            if self.current.kind == TokenKind::Keyword && self.current.keyword == KeywordKind::Pragma {
                if let Some(p) = self.parse_raw_pragma_decl() { methods.push(p); }
                continue;
            }
            if self.match_keyword(KeywordKind::AtProperty) {
                if let Some(prop) = self.parse_property() {
                    properties.push(prop);
                }
            } else if self.current.kind == TokenKind::Keyword &&
                      (self.current.keyword == KeywordKind::AtSynthesize ||
                       self.current.keyword == KeywordKind::AtDynamic) {
                self.advance();
            while self.current.kind == TokenKind::Identifier ||
                  (self.current.kind == TokenKind::Keyword && !matches!(self.current.keyword,
                      KeywordKind::AtInterface | KeywordKind::AtImplementation | KeywordKind::AtEnd |
                      KeywordKind::AtProperty | KeywordKind::AtSynthesize | KeywordKind::AtDynamic |
                      KeywordKind::AtSelector | KeywordKind::AtEncode | KeywordKind::AtProtocol |
                      KeywordKind::AtOptional | KeywordKind::AtRequired | KeywordKind::AtClass |
                      KeywordKind::AtTry | KeywordKind::AtCatch | KeywordKind::AtFinally |
                      KeywordKind::AtThrow | KeywordKind::AtThrows | KeywordKind::AtSynchronized | KeywordKind::AtAutoreleasepool | KeywordKind::AtDefer |
                       KeywordKind::AtNoArc |
                      KeywordKind::AtPublic | KeywordKind::AtPackage | KeywordKind::AtProtected |
                      KeywordKind::AtPrivate | KeywordKind::AtDefs | KeywordKind::AtNamespace |
                      KeywordKind::AtEndNamespace |
                      KeywordKind::AtUsing | KeywordKind::Self_ | KeywordKind::Super |
                      KeywordKind::Return | KeywordKind::If | KeywordKind::Else |
                      KeywordKind::Switch | KeywordKind::Case | KeywordKind::Default |
                      KeywordKind::While | KeywordKind::Do | KeywordKind::For |
                      KeywordKind::Break | KeywordKind::Continue | KeywordKind::Goto |
                      KeywordKind::Sizeof | KeywordKind::Typeof | KeywordKind::Typedef |
                      KeywordKind::Struct | KeywordKind::Union | KeywordKind::Enum |
                      KeywordKind::Const | KeywordKind::Volatile | KeywordKind::Extern |
                      KeywordKind::Static | KeywordKind::Auto | KeywordKind::Register |
                      KeywordKind::Inline | KeywordKind::Restrict |
                      KeywordKind::Imp | KeywordKind::NpZone |
                      KeywordKind::Import | KeywordKind::Include | KeywordKind::Define |
                      KeywordKind::Ifdef | KeywordKind::Ifndef | KeywordKind::Endif |
                      KeywordKind::Pragma | KeywordKind::Elif | KeywordKind::Undef
                  )) {
                    self.advance();
                    if self.match_token(TokenKind::Assign) {
                        if self.current.kind == TokenKind::Identifier {
                            self.advance();
                        }
                    }
                    if !self.match_token(TokenKind::Comma) { break; }
                }
                self.consume(TokenKind::Semicolon, "expected ';' after @synthesize/@dynamic");
            } else if self.current.kind == TokenKind::Keyword &&
                      (self.current.keyword == KeywordKind::AtPublic ||
                       self.current.keyword == KeywordKind::AtProtected ||
                       self.current.keyword == KeywordKind::AtPrivate ||
                       self.current.keyword == KeywordKind::AtPackage) {
                self.advance();
            } else {
                // Method (+/-)
                if self.current.kind == TokenKind::Keyword &&
                   (self.current.keyword == KeywordKind::Block ||
                    self.current.keyword == KeywordKind::Weak ||
                    self.current.keyword == KeywordKind::Strong) {
                    // This might be a __block/__weak declaration inside @interface — skip
                    // Actually, these appear inside @implementation for local variables
                    self.advance();
                    continue;
                }
                if let Some(method) = self.parse_method() {
                    methods.push(method);
                } else {
                    self.advance();
                }
            }
        }

        // Register type param names
        for tp in &type_params {
            self.add_type_name(tp);
        }
        self.add_type_name(&name);

        let kind = if category_name.is_some() {
            CstDeclKind::CategoryInterface
        } else {
            CstDeclKind::ClassInterface
        };

        Some(CstDecl {
            kind,
            line: ci_line, column: ci_col,
            name: Some(name),
            next: None,
            data: CstDeclData::Class {
                superclass,
                category_name,
                protocols,
                type_params,
                type_bounds,
                ivars,
                properties,
                methods,
                impl_vars: Vec::new(),
            },
                    attributes: Vec::new(),

            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_class_implementation(&mut self) -> Option<CstDecl> {
        let (cp_line, cp_col) = (self.current.line, self.current.column);
        self.advance(); // consume @implementation
        // Class name may be namespace-qualified: `@implementation Game::Player`.
        let name = match self.parse_qualified_name_with_keywords() {
            Some(n) if !n.is_empty() => n,
            _ => { self.error("expected class name after @implementation"); return None; }
        };

        // Check for category: @implementation ClassName (CategoryName)
        let mut category_name = None;
        if self.match_token(TokenKind::LParen) {
            let mut cat = String::new();
            while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                if self.current.kind == TokenKind::Identifier ||
                   self.current.kind == TokenKind::Keyword {
                    cat.push_str(self.current_text());
                } else {
                    let start = self.current.start;
                    let end = if self.current.length > 0 { start + self.current.length } else { start };
                    if end > start {
                        if let Some(s) = self.source.get(start..end) {
                            cat.push_str(s);
                        }
                    }
                }
                self.advance();
            }
            self.consume(TokenKind::RParen, "expected ')'");
            category_name = if cat.is_empty() { None } else { Some(cat) };
        }

        let mut methods = Vec::new();
        let mut impl_vars = Vec::new();
        // Superclass suffix: `@implementation Base : NPObject` — ObjC allows
        // repeating the superclass on @implementation. Without consuming it,
        // an empty implementation body would leave `: NPObject` dangling and
        // the fallback token-skip would swallow the following `@interface`.
        let mut superclass = None;
        if self.match_token(TokenKind::Colon) {
            if self.current.kind == TokenKind::Identifier {
                superclass = self.parse_qualified_name();
            }
        }
        while !self.match_keyword(KeywordKind::AtEnd) && !self.check(TokenKind::Eof) {
            if self.current.kind == TokenKind::Keyword && self.current.keyword == KeywordKind::Pragma {
                if let Some(p) = self.parse_raw_pragma_decl() { methods.push(p); }
                continue;
            }
            if self.current.kind == TokenKind::Keyword &&
               (self.current.keyword == KeywordKind::AtProperty ||
                self.current.keyword == KeywordKind::AtSynthesize ||
                self.current.keyword == KeywordKind::AtDynamic) {
                self.advance();
                // Skip property/synthesize/dynamic in @implementation
                if self.current.kind == TokenKind::LParen {
                    // Skip property attributes
                    let mut depth = 1;
                    while depth > 0 && !self.check(TokenKind::Eof) {
                        if self.current.kind == TokenKind::LParen { depth += 1; }
                        if self.current.kind == TokenKind::RParen { depth -= 1; }
                        self.advance();
                    }
                }
                // Skip to ;
                while !self.check(TokenKind::Semicolon) && !self.check(TokenKind::Eof) {
                    self.advance();
                }
                if self.current.kind == TokenKind::Semicolon { self.advance(); }
                continue;
            }
            // Check for variable declarations inside @implementation
            // (e.g., static globals used by categories).
            // But if the current token is + or - (method type indicators),
            // try parse_method first to avoid confusing method return types
            // (like int, void, BOOL) with declaration starts.
            if self.current.kind == TokenKind::Plus || self.current.kind == TokenKind::Minus {
                if let Some(method) = self.parse_method() {
                    methods.push(method);
                    continue;
                }
            } else if self.is_declaration_start() {
                if let Some(decl) = self.parse_declaration() {
                    impl_vars.push(decl);
                    continue;
                }
            }
            if let Some(method) = self.parse_method() {
                methods.push(method);
            } else {
                // Skip to next @end or method
                if self.current.kind == TokenKind::Eof { break; }
                self.advance();
            }
        }

        let kind = if category_name.is_some() {
            CstDeclKind::CategoryImplementation
        } else {
            CstDeclKind::ClassImplementation
        };

        Some(CstDecl {
            kind,
            line: cp_line, column: cp_col,
            name: Some(name),
            next: None,
            data: CstDeclData::Class {
                superclass,
                category_name,
                protocols: Vec::new(),
                type_params: Vec::new(),
                type_bounds: Vec::new(),
                ivars: Vec::new(),
                properties: Vec::new(),
                methods,
                impl_vars,
            },
                    attributes: Vec::new(),

            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_property(&mut self) -> Option<CstDecl> {
        let (pp_line, pp_col) = (self.previous.line, self.previous.column);
        // Attributes: (readonly, weak, nonatomic, getter=xxx, setter=xxx:)
        let mut is_readonly = false;
        let mut is_weak = false;
        let mut is_assign = false;
        let mut is_retain = false;
        let mut is_copy = false;
        let mut is_nonatomic = false;
        let mut getter = None;
        let mut setter = None;
        // Nullability written as a property attribute: `@property (nullable) T *x`.
        // Kept separate from the type's own annotation until the type is parsed,
        // then merged onto it — so the annotation lives in exactly one place
        // (`prop_type.nulls`) for the elaborator and checker to read.
        let mut attr_nulls = None;

        if self.match_token(TokenKind::LParen) {
            while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                if self.match_keyword(KeywordKind::AtReadonly) { is_readonly = true; }
                else if self.match_keyword(KeywordKind::AtWeak) { is_weak = true; }
                else if self.match_keyword(KeywordKind::AtAssign) { is_assign = true; }
                else if self.match_keyword(KeywordKind::AtRetain) { is_retain = true; }
                else if self.match_keyword(KeywordKind::AtCopy) { is_copy = true; }
                else if self.match_keyword(KeywordKind::AtNonatomic) { is_nonatomic = true; }
                // `nullable` / `nonnull` as property attributes. They arrive as
                // plain identifiers (never KW_TABLE entries — see
                // `nullability_prefix`), so read by text and consume by hand.
                // `nullable_t x;` is impossible here: this loop only ever sees
                // comma-separated attributes inside `(...)`.
                else if self.nullability_prefix().is_some() {
                    attr_nulls = self.nullability_prefix();
                    self.advance();
                }
                else if self.match_keyword(KeywordKind::AtGetter) {
                    self.consume(TokenKind::Assign, "expected '='");
                    if self.current.kind == TokenKind::Identifier {
                        getter = Some(self.current_text().to_string());
                        self.advance();
                    }
                }
                else if self.match_keyword(KeywordKind::AtSetter) {
                    self.consume(TokenKind::Assign, "expected '='");
                    if self.current.kind == TokenKind::Identifier {
                        let s = self.current_text().to_string();
                        self.advance();
                        if self.match_token(TokenKind::Colon) {
                            // setter name includes colon
                        }
                        setter = Some(s);
                    }
                }
                else {
                    self.advance();
                }
                if !self.match_token(TokenKind::Comma) { break; }
            }
            self.consume(TokenKind::RParen, "expected ')'");
        }

        let mut prop_type = self.parse_type_full();
        // Merge a `(nullable)` / `(nonnull)` attribute onto the parsed type.
        // Done here — after the type is parsed, before any array wrapping or
        // Property construction — so every one of the three construction sites
        // below inherits it with no per-site plumbing. An explicit annotation in
        // the type itself (`@property (nullable) nullable NPString *x`) would be
        // contradictory; the attribute wins because it is written later and is
        // the more specific of the two.
        if let (Some(n), Some(t)) = (attr_nulls, prop_type.as_mut()) {
            t.nulls = n;
        }
        // Check for array suffix: name[size]
        if self.match_name() {
            let name = self.previous_text().to_string();
            // Array suffix
            if self.match_token(TokenKind::LBracket) {
                let mut arr_type = CstType::new(TypePrim::Named);
                if let Some(ref pt) = prop_type {
                    arr_type.subtype = Some(Box::new(pt.clone()));
                }
                arr_type.is_array = true;
                if self.current.kind == TokenKind::Integer {
                    arr_type.array_size = self.current_text().parse().unwrap_or(0);
                    self.advance();
                }
                self.consume(TokenKind::RBracket, "expected ']'");
                prop_type = Some(arr_type);
            }
            // Check for comma-separated names
            if self.match_token(TokenKind::Comma) {
                // Parse additional names with the same type
                let mut head = CstDecl {
                    kind: CstDeclKind::Property,
                    line: pp_line, column: pp_col,
                    name: Some(name),
                    next: None,
                    data: CstDeclData::Property {
                        prop_type: prop_type.clone().map(Box::new),
                        getter: getter.clone(),
                        setter: setter.clone(),
                        is_readonly,
                        is_weak,
                        is_assign,
                        is_retain,
                        is_copy,
                        is_nonatomic,
                        is_dynamic: false,
                    },
                                    attributes: Vec::new(),

                    end_line: self.previous.end_line,
                    end_column: self.previous.end_col,
};
                let mut tail = &mut head;
                while self.match_name() {
                    let nname = self.previous_text().to_string();
                    tail.next = Some(Box::new(CstDecl {
                        kind: CstDeclKind::Property,
                        line: pp_line, column: pp_col,
                        name: Some(nname),
                        next: None,
                        data: CstDeclData::Property {
                            prop_type: prop_type.clone().map(Box::new),
                            getter: getter.clone(),
                            setter: setter.clone(),
                            is_readonly,
                            is_weak,
                            is_assign,
                            is_retain,
                            is_copy,
                            is_nonatomic,
                            is_dynamic: false,
                        },
                                            attributes: Vec::new(),

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
}));
                    tail = tail.next.as_mut().unwrap();
                    if !self.match_token(TokenKind::Comma) { break; }
                }
                self.consume(TokenKind::Semicolon, "expected ';' at end of declaration list");
                return Some(head);
            }
            self.consume(TokenKind::Semicolon, "expected ';' at end of declaration list");
            return Some(CstDecl {
                kind: CstDeclKind::Property,
                line: pp_line, column: pp_col,
                name: Some(name),
                next: None,
                data: CstDeclData::Property {
                    prop_type: prop_type.map(Box::new),
                    getter,
                    setter,
                    is_readonly,
                    is_weak,
                    is_assign,
                    is_retain,
                    is_copy,
                    is_nonatomic,
                    is_dynamic: false,
                },
                            attributes: Vec::new(),

                end_line: self.previous.end_line,
                end_column: self.previous.end_col,
});
        }
        self.consume(TokenKind::Semicolon, "expected ';' at end of declaration list");
        None
    }

    fn parse_method(&mut self) -> Option<CstDecl> {
        let (m_line, m_col) = (self.current.line, self.current.column);
        let is_class_method = if self.match_token(TokenKind::Plus) { true }
        else if self.match_token(TokenKind::Minus) { false }
        else { return None; };

        // Parse return type: (type) or plain type. Inside the parens, a bare
        // `async` identifier followed by `NPTask` is the async return-type
        // modifier (docs/plans/async_nptask_plan.md), not part of the type.
        let mut async_modifier = false;
        let return_type = if self.match_token(TokenKind::LParen) {
            async_modifier = self.at_async_modifier();
            if async_modifier {
                self.advance();
            }
            let rt = self.parse_type_annotated();
            self.consume(TokenKind::RParen, "expected ')'");
            if async_modifier {
                match rt {
                    Some(t) => Some(t),
                    None => {
                        self.error("'async' requires a return type");
                        None
                    }
                }
            } else {
                rt
            }
        } else {
            self.parse_type_annotated()
        };
        let async_marker = async_modifier;

        // Parse method selector and params
        let mut params: Option<Box<CstParam>> = None;
        let mut tail: &mut Option<Box<CstParam>> = &mut params;
        let mut has_keyword = false;
        let mut has_variadic = false;
        let mut method_name = String::new();

        // First keyword/param
        if self.current.kind == TokenKind::Identifier ||
           (self.current.kind == TokenKind::Keyword &&
            !matches!(self.current.keyword, KeywordKind::Return | KeywordKind::If |
                KeywordKind::While | KeywordKind::For | KeywordKind::Do |
                KeywordKind::Switch | KeywordKind::Case | KeywordKind::Default |
                KeywordKind::Break | KeywordKind::Continue | KeywordKind::Goto |
                KeywordKind::Sizeof | KeywordKind::Const | KeywordKind::Static |
                KeywordKind::Extern | KeywordKind::Volatile | KeywordKind::Struct |
                KeywordKind::Union | KeywordKind::Enum | KeywordKind::Typedef |
                KeywordKind::Void | KeywordKind::Int | KeywordKind::Char |
                KeywordKind::Short | KeywordKind::Long | KeywordKind::Float |
                KeywordKind::Double | KeywordKind::Bool | KeywordKind::Signed |
                KeywordKind::Unsigned | KeywordKind::Class |
                KeywordKind::Sel | KeywordKind::Instancetype |
                KeywordKind::Block | KeywordKind::Weak | KeywordKind::Strong |
                KeywordKind::Autoreleasing | KeywordKind::UnsafeUnretained |
                KeywordKind::AtThrow | KeywordKind::AtThrows)) {
            let sel_part = self.current_text().to_string();
            self.advance();

            if self.match_token(TokenKind::Colon) {
                has_keyword = true;
                method_name.push_str(&sel_part);
                method_name.push(':');
                let mut p = CstParam {
                    par_type: None,
                    name: None,
                    external_name: Some(sel_part),
                            attributes: Vec::new(),
                    next: None,
                };
                // Type name (optional)
                if self.match_token(TokenKind::LParen) {
                    p.par_type = self.parse_type_annotated().map(Box::new);
                    self.consume(TokenKind::RParen, "expected ')'");
                } else {
                    p.par_type = self.parse_type_annotated().map(Box::new);
                }
                if self.is_name_token() &&
                   !self.check(TokenKind::Colon) && !self.check(TokenKind::Semicolon) &&
                   !self.check(TokenKind::RBrace) {
                    p.name = Some(self.current_text().to_string());
                    self.advance();
                }
                tail = &mut tail.insert(Box::new(p)).next;

                // More keyword:param pairs
                loop {
                    // ObjC 2.0 variadic methods: `- (void)log:(const char *)fmt, ...;`
                    if self.match_token(TokenKind::Comma) {
                        if self.match_token(TokenKind::Ellipsis) {
                            has_variadic = true;
                        }
                        break;
                    }
                    if self.current.kind == TokenKind::Identifier ||
                       (self.current.kind == TokenKind::Keyword &&
                        !matches!(self.current.keyword, KeywordKind::Return | KeywordKind::If |
                            KeywordKind::While | KeywordKind::For | KeywordKind::Do |
                            KeywordKind::Switch | KeywordKind::Break | KeywordKind::Continue |
                            KeywordKind::Goto | KeywordKind::Sizeof | KeywordKind::Const |
                            KeywordKind::Static | KeywordKind::Extern | KeywordKind::Struct |
                            KeywordKind::Union | KeywordKind::Enum | KeywordKind::Typedef |
                            KeywordKind::Void | KeywordKind::Int | KeywordKind::Char |
                            KeywordKind::Short | KeywordKind::Long | KeywordKind::Float |
                            KeywordKind::Double | KeywordKind::Bool | KeywordKind::Signed |
                            KeywordKind::Unsigned | KeywordKind::Class |
                            KeywordKind::Sel | KeywordKind::Instancetype |
                            KeywordKind::Block | KeywordKind::Weak | KeywordKind::Strong |
                            KeywordKind::AtThrow | KeywordKind::AtThrows)) {
                        // Consume `next_part` only if it really starts another
                        // `sel:` segment (colon AFTER the identifier). Parser
                        // discipline: a failed consume must not advance —
                        // blindly eating the token here used to swallow the
                        // trailing soft keyword `throws` (§9.2) and leave the
                        // method's failable flag silently dropped.
                        let colon_follows = {
                            let saved_lex = self.lexer.save_pos();
                            let saved_cur = self.current.clone();
                            let saved_prev = self.previous.clone();
                            self.advance();
                            let c = self.match_token(TokenKind::Colon);
                            self.lexer.restore_pos(saved_lex);
                            self.current = saved_cur;
                            self.previous = saved_prev;
                            c
                        };
                        if !colon_follows {
                            break; // trailing identifier (e.g. `throws`) — not a selector segment
                        }
                        let next_part = self.current_text().to_string();
                        self.advance();
                        if self.match_token(TokenKind::Colon) {
                            has_keyword = true;
                            method_name.push_str(&next_part);
                            method_name.push(':');
                            let mut next_p = CstParam {
                                par_type: None,
                                name: None,
                                external_name: Some(next_part),
                                attributes: Vec::new(),
                                next: None,
                            };
                            if self.match_token(TokenKind::LParen) {
                                next_p.par_type = self.parse_type_annotated().map(Box::new);
                                self.consume(TokenKind::RParen, "expected ')'");
                            } else {
                                next_p.par_type = self.parse_type_annotated().map(Box::new);
                            }
                             if self.is_name_token() &&
                                !self.check(TokenKind::Colon) && !self.check(TokenKind::Semicolon) &&
                                !self.check(TokenKind::RBrace) {
                                next_p.name = Some(self.current_text().to_string());
                                self.advance();
                            }
                            tail = &mut tail.insert(Box::new(next_p)).next;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            } else {
                method_name = sel_part;
            }
        }

        // If no keyword params, check for C-style params: (type name, ...)
        if !has_keyword && self.match_token(TokenKind::LParen) {
            while !self.check(TokenKind::RParen) && !self.check(TokenKind::Eof) {
                if self.match_token(TokenKind::Ellipsis) {
                    has_variadic = true;
                    break;
                }
                if let Some(ptype) = self.parse_type_annotated() {
                    let pname = if self.is_name_token() {
                        let n = self.current_text().to_string();
                        self.advance();
                        n
                    } else { String::new() };
                    let p = CstParam {
                        par_type: Some(Box::new(ptype)),
                        name: if pname.is_empty() { None } else { Some(pname) },
                        external_name: None,
                    attributes: Vec::new(),
                        next: None,
                    };
                    tail = &mut tail.insert(Box::new(p)).next;
                }
                if !self.match_token(TokenKind::Comma) { break; }
            }
            self.consume(TokenKind::RParen, "expected ')'");
        }

        // Trailing declaration annotation: bare `throws` / `throws(T *)` before
        // `;` or `{` — the SAME soft-keyword spelling on methods and functions
        // (2026-10-13 decision: the @-prefix was dropped from the annotation).
        // Compile-time only — carried in CST, never emitted to C. Position
        // disambiguation for the soft keyword (C superset: `throws` stays a
        // valid identifier everywhere else):
        //   * `throws:` in the selector area was consumed by the selector loop
        //     above as a selector segment (`throws:` param, `[t throws:1]`
        //     call sites), because that loop only continues on a following
        //     colon;
        //   * `- (int)throws;` names the METHOD `throws` — the name is the
        //     first identifier after the return type, so this tail is only
        //     reached with `throws` AFTER the parameter list, where it can
        //     only be the annotation;
        //   * anything else after `)` that is not `throws`/`@throws` leaves
        //     `throws = None` (unannotated declaration).
        let mut throws: Option<Box<CstType>> = None;
        if self.check_keyword(KeywordKind::AtThrows) {
            self.error("'@throws' was removed — spell the exception annotation bare: 'throws' or 'throws(<type>)', e.g. '- (void)f throws(NPString *);'");
            self.advance(); // consume the token for error recovery
            if self.match_token(TokenKind::LParen) {
                let _ = self.parse_type_full();
                self.consume(TokenKind::RParen, "expected ')' after throws(...)");
            }
        } else if self.current.kind == TokenKind::Identifier
            && self.current_text() == "throws"
        {
            self.advance(); // consume the soft keyword
            if self.match_token(TokenKind::LParen) {
                throws = self.parse_type_full().map(Box::new);
                self.consume(TokenKind::RParen, "expected ')' after throws(...)");
            } else {
                // Bare `throws` — "declared to throw, type unstated". Encoded
                // as a void-typed annotation so it stays distinguishable from
                // "not annotated" (`None`); the checker then only requires the
                // body to really contain a `@throw` and does not check types.
                throws = Some(Box::new(CstType::new(TypePrim::Void)));
            }
            if !self.check(TokenKind::LBrace) && !self.check(TokenKind::Semicolon) {
                self.error("expected '('type')' or ';' after throws");
            }
        } else if self.match_keyword(KeywordKind::AtThrow) {
            self.error("@throw is a statement (it raises an exception inside a body); use 'throws' or 'throws(<type>)' to annotate this declaration");
        }

        // Body
        let body = if self.check(TokenKind::LBrace) {
            self.parse_compound_statement().map(Box::new)
        } else {
            self.consume(TokenKind::Semicolon, "expected ';' after method declaration");
            None
        };

        // Build method selector name from params
        let method_name = method_name; // make it non-mut

        Some(CstDecl {
            kind: CstDeclKind::Method,
            line: m_line, column: m_col,
            name: if method_name.is_empty() { None } else { Some(method_name) },
            next: None,
            data: CstDeclData::Method {
                is_class_method,
                return_type: return_type.map(Box::new),
                params,
                has_variadic,
                body,
                throws,
                async_marker,
            },
                    attributes: Vec::new(),

            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_protocol(&mut self) -> Option<CstDecl> {
        let (pr_line, pr_col) = (self.current.line, self.current.column);
        self.advance(); // consume @protocol
        if !self.match_name() { self.error("expected protocol name"); return None; }
        let name = self.previous_text().to_string();

        // Protocol inheritance: <Proto1, Proto2> or composition <P & Q> —
        // binder merges P's and Q's required methods into this protocol.
        let mut protocols = Vec::new();
        if self.match_token(TokenKind::Less) {
            while self.current.kind == TokenKind::Identifier {
                let p = self.current_text().to_string();
                self.advance();
                protocols.push(p);
                if !(self.match_token(TokenKind::Comma) || self.match_token(TokenKind::Ampersand)) { break; }
            }
            if self.current.kind == TokenKind::Greater { self.advance(); }
            else if self.current.kind == TokenKind::RShift { self.advance(); }
        }

        let mut methods = Vec::new();
        let mut is_optional = false;
        while !self.match_keyword(KeywordKind::AtEnd) && !self.check(TokenKind::Eof) {
            if self.match_keyword(KeywordKind::AtOptional) {
                is_optional = true;
                continue;
            }
            if self.match_keyword(KeywordKind::AtRequired) {
                is_optional = false;
                continue;
            }
            if let Some(method) = self.parse_method() {
                methods.push(method);
            } else {
                self.advance();
            }
        }

        self.add_type_name(&name);
        Some(CstDecl {
            kind: CstDeclKind::Protocol,
            line: pr_line, column: pr_col,
            name: Some(name),
            next: None,
            data: CstDeclData::ProtocolData { protocols, methods, is_optional },
                    attributes: Vec::new(),

            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_forward_class(&mut self) -> Option<CstDecl> {
        let (fc_line, fc_col) = (self.current.line, self.current.column);
        self.advance(); // consume @class
        let mut names = Vec::new();
        // Names may be namespace-qualified — `@class Net::Remote;` declares
        // `Remote` in namespace `Net`, exactly like a bare `@class Remote;`
        // written inside `@namespace Net … @endnamespace`.
        while self.current.kind == TokenKind::Identifier ||
              self.current.kind == TokenKind::Keyword {
            match self.parse_qualified_name() {
                Some(n) => names.push(n),
                None => break,
            }
            if !self.match_token(TokenKind::Comma) { break; }
        }
        self.consume(TokenKind::Semicolon, "expected ';' after @class");
        for n in &names {
            self.add_type_name(n);
        }
        Some(CstDecl {
            kind: CstDeclKind::ForwardClass,
            line: fc_line, column: fc_col,
            name: None,
            next: None,
            data: CstDeclData::Forward(names),
                    attributes: Vec::new(),

            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_namespace(&mut self) -> Option<CstDecl> {
        let (ns_line, ns_col) = (self.current.line, self.current.column);
        self.advance(); // consume @namespace
        // Namespace name may be qualified: `@namespace Core::Swarm … @endnamespace`
        let name = self.parse_qualified_name_with_keywords();
        let name = match name {
            Some(n) if !n.is_empty() => n,
            _ => { self.error("expected namespace name"); return None; }
        };
        // `@namespace Name` … `@endnamespace` (empty namespace allowed).
        let mut decls = Vec::new();
        while !self.match_keyword(KeywordKind::AtEndNamespace) && !self.check(TokenKind::Eof) {
            if let Some(d) = self.parse_declaration() {
                decls.push(d);
            } else {
                self.advance();
            }
        }
        Some(CstDecl {
            kind: CstDeclKind::Namespace,
            line: ns_line, column: ns_col,
            name: Some(name),
            next: None,
            data: CstDeclData::Namespace(decls),
                    attributes: Vec::new(),

            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    fn parse_using(&mut self) -> Option<CstDecl> {
        let (us_line, us_col) = (self.current.line, self.current.column);
        self.advance(); // consume @using

        // @using namespace Name;
        let is_ns = self.match_keyword(KeywordKind::AtNamespace) ||
            (self.current.kind == TokenKind::Identifier && self.current_text() == "namespace" && { self.advance(); true });
        if is_ns {
            // Namespace may be qualified: `@using namespace Network::Extensions;`
            if let Some(fqn) = self.parse_qualified_name_with_keywords() {
                if !fqn.is_empty() {
                    self.consume(TokenKind::Semicolon, "expected ';' after @using namespace");
                    return Some(CstDecl {
                        kind: CstDeclKind::Using,
                        line: us_line, column: us_col,
                        name: None,
                        next: None,
                        data: CstDeclData::Using { fqn, alias: None },
                                            attributes: Vec::new(),

                        end_line: self.previous.end_line,
                        end_column: self.previous.end_col,
});
                }
            }
            self.consume(TokenKind::Semicolon, "expected ';' after @using namespace");
            return None;
        }

        // @using Alias = FQN; or @using FQN;
        let mut alias = None;
        let mut fqn = String::new();

        if self.current.kind == TokenKind::Identifier {
            let first = self.current_text().to_string();
            self.advance();

            // Check for Alias = FQN pattern
            if self.match_token(TokenKind::Assign) {
                alias = Some(first.clone());
                // Use parse_type_full (not parse_qualified_name) so that the
                // FQN may contain protocol qualifiers (`id<P>`) and generic
                // type arguments (`VectorBuffer<T*>`). Render the parsed type
                // back to a source-level fqn string for the symbol table.
                if let Some(t) = self.parse_type_full() {
                    fqn = Self::type_to_fqn(&t);
                }
                // Register the alias as a type name so that subsequent
                // `Alias *var = ...` declarations are recognized as declarations
                // (is_declaration_start relies on type_names membership).
                self.add_type_name(&first);
            } else {
                fqn = first;
                while self.match_token(TokenKind::ColonColon) {
                    fqn.push_str("::");
                    if self.current.kind == TokenKind::Identifier {
                        fqn.push_str(self.current_text());
                        self.advance();
                    } else {
                        break;
                    }
                }
                // @using Namespace::Class; — register the short (last) name
                // as a type name so that `Class *var = ...` parses as a declaration.
                if let Some(short) = fqn.rsplit("::").next() {
                    if !short.is_empty() && short != fqn.as_str() {
                        self.add_type_name(short);
                    }
                }
            }
        }

        self.consume(TokenKind::Semicolon, "expected ';' after @using");
        Some(CstDecl {
            kind: CstDeclKind::Using,
            line: us_line, column: us_col,
            name: None,
            next: None,
            data: CstDeclData::Using { fqn, alias },
                    attributes: Vec::new(),

            end_line: self.previous.end_line,
            end_column: self.previous.end_col,
})
    }

    // ─── Top-level ──────────────────────────────────────────────────────

    pub fn parse_translation_unit(&mut self) -> Option<TranslationUnit> {
        let mut decls = Vec::new();
        while self.current.kind != TokenKind::Eof {
            // Error recovery: skip to the next declaration boundary so one bad
            // declaration does not hide every error after it. The `start` guard
            // guarantees forward progress — without it a recovery point that is
            // already current (e.g. a stray `;` we just consumed) would spin.
            let start = self.current.start;
            if let Some(d) = self.parse_declaration() {
                decls.push(d);
                // `consume` records the error but still lets the declaration
                // complete, so a bad declaration returns Some. Reset and skip to
                // the next boundary here — otherwise `panic_mode` stays set and
                // every later `error()` is suppressed, which is why one compile
                // used to report exactly one problem.
                if self.panic_mode {
                    // Reset only — do NOT skip. The parser is still inside a
                    // well-formed declaration (typically sitting on the next
                    // declarator's name after a missing ';'), and skipping to a
                    // recovery point here would swallow the following
                    // declaration whole: `int a = 1 / int b = 2` reported one
                    // error instead of two. The `start` guard is still needed so
                    // a declaration that completes without consuming anything
                    // cannot spin.
                    self.panic_mode = false;
                    if self.current.start == start && self.current.kind != TokenKind::Eof {
                        self.advance();
                    }
                }
            } else {
                self.synchronize();
                if self.current.start == start && self.current.kind != TokenKind::Eof {
                    self.advance();
                }
            }
        }
        // A region left open at EOF means every subsequent declaration silently
        // inherited `nonnull`, including ones the author never looked at. That
        // is exactly the silent-wrong direction, so report it rather than
        // letting it ride. (ObjC's macro form catches this too — clang warns on
        // an unterminated `assume_nonnull` region.)
        if self.nonnull_region {
            self.error("NP_ASSUME_NONNULL_BEGIN without a matching NP_ASSUME_NONNULL_END — every later pointer in this file would be treated as nonnull");
        }
        Some(TranslationUnit {
            decls,
            filename: String::new(),
        })
    }

    pub fn has_error(&self) -> bool {
        self.has_error
    }

    /// Number of errors recorded. Diagnostics tests use this to assert that
    /// recovery reports *every* problem instead of only the first.
    pub fn error_count(&self) -> usize {
        self.error_count
    }

    pub fn last_error(&self) -> &str {
        &self.err_msg
    }

    /// Structured diagnostics with token-width spans, for annotated rendering.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// Error text for a `case` label that is neither a C constant expression nor a
/// pattern. Shared by the single-value and comma-list paths so the wording
/// cannot drift.
const CASE_NON_CONSTANT_MSG: &str = "case label is not a constant expression — a message send, call or assignment cannot label a C `case`; use a pattern (`case T *x`, `case > 10`, `case @\"lit\"`) or an integer constant";

/// True when the expression is an ObjC object literal — a *pattern* compared
/// with `isEqual:`, never a valid C case label: `@"..."` (AtString kind),
/// `@(expr)` (the Boxed marker the checker rewrites by type), `@N`/`@YES`/
/// `@'c'` (desugared by `mk_npnumber_send` into a message send on NPNumber),
/// or an explicit call to one of those factories.
///
/// The boxed-scalar clauses matter: without them `case @42:` was classified as
/// a plain constant arm, so an all-literal switch stayed on the C path and
/// emitted `case [NPNumber numberWithInt:42]:` — invalid C, reported against
/// the generated code with no source correspondence.
fn is_object_literal(e: &CstExpr) -> bool {
    if matches!(
        e.kind,
        CstExprKind::AtString | CstExprKind::Boxed | CstExprKind::NumberLit
    ) {
        return true;
    }
    if matches!(&e.data, CstExprData::Message { receiver, .. }
        if matches!(receiver.kind, CstExprKind::AtString))
    {
        return true;
    }
    matches!(&e.data, CstExprData::Message { selector, .. }
        if matches!(selector.as_str(),
            "numberWithInt:" | "numberWithDouble:" | "numberWithBool:"
            | "numberWithChar:" | "numberWithLongLong:"))
}

/// True when the expression provably cannot be a C `case` label: it contains a
/// message send, a call or an assignment. C requires an integer constant
/// expression, which none of those can be — but Jeti let them through and
/// emitted `case [obj msg]:`, so the error surfaced from the generated C.
/// Conservative by construction: an unrecognized node shape returns false, so
/// this can only ever flag what is definitely invalid.
fn expr_is_non_constant(e: &CstExpr) -> bool {
    match &e.data {
        // A message send / call is non-constant in itself — it can never be an
        // integer constant expression, whatever its operands are.
        CstExprData::Message { .. } | CstExprData::Call { .. } => true,
        CstExprData::Assign { .. } => true,
        CstExprData::Binary { left, right, .. } => {
            expr_is_non_constant(left) || expr_is_non_constant(right)
        }
        CstExprData::Unary { operand, .. } => expr_is_non_constant(operand),
        CstExprData::Ternary {
            cond,
            true_expr,
            false_expr,
        } => {
            expr_is_non_constant(cond)
                || expr_is_non_constant(true_expr)
                || expr_is_non_constant(false_expr)
        }
        CstExprData::Cast { expr, .. } => expr_is_non_constant(expr),
        CstExprData::Comma(items) => items.iter().any(expr_is_non_constant),
        CstExprData::NumberLit(inner) => expr_is_non_constant(inner),
        _ => false,
    }
}

/// True when a `switch` subject is *syntactically* an object, so a dangling
/// comparison arm against it would compile to a pointer-vs-integer compare
/// (always true) instead of the intended numeric compare.
///
/// Deliberately narrow — only shapes that are objects by construction:
/// object literals (`@"..."`, `@N`, `@YES`, `@'c'`, `@(expr)`), a cast to a
/// pointer type, `nil`, and an explicit `NPObject *`/`id` cast. A bare
/// identifier is **not** flagged: `switch (o)` where `o` holds an `int` is
/// perfectly valid, and M1 has no type information to tell the two apart. That
/// residual case is a documented M1 limit, not silent-wrong by construction.
fn expr_is_definitely_object(e: &CstExpr) -> bool {
    if is_object_literal(e) {
        return true;
    }
    // `nil` / `NULL` live on the *kind*, not the data payload.
    if matches!(e.kind, CstExprKind::Nil | CstExprKind::Null) {
        return true;
    }
    match &e.data {
        CstExprData::Cast { target_type, .. } => target_type.is_pointer,
        // A message send is an object by definition in Jeti, but its *value*
        // may be a scalar-returning method, so it is not flagged.
        _ => false,
    }
}

/// Flat pattern-arm collection state for one `switch` body.
#[derive(Default)]
struct PatternArms {
    arms: Vec<CstArm>,
    has_default: bool,
    /// `default:` body, grouped with its fallthrough siblings; the pattern
    /// crate emits it as the `__jeti_case_d` labeled block.
    default_body: Option<Box<CstStmt>>,
    /// A plain constant `case N:` arm appeared alongside pattern arms. The two
    /// families cannot share one lowered switch (M1): the C path needs the
    /// original body, the pattern path discards it. The caller reports an error
    /// instead of silently dropping the constant arm.
    has_const_arm: bool,
    /// At least one dangling-comparison arm (`case > 10:`). Lowered to
    /// `subject > 10` — only sound when the subject stays a scalar.
    has_cond_arm: bool,
    /// At least one arm that needs the subject as an *object* (type binding or
    /// object literal). Its presence is what forces the subject to be
    /// materialized as `NPObject *`, which is what makes a coexisting
    /// `has_cond_arm` arm degenerate into a pointer comparison.
    has_object_arm: bool,
}

/// True for statements that START an arm group: a `case`/`default` label, or
/// the single-arm `SwitchPat` wrapper the Case branch builds for a pattern.
fn is_arm_node(s: &CstStmt) -> bool {
    matches!(s.data,
        CstStmtData::Case { .. } | CstStmtData::Default(_) | CstStmtData::SwitchPat { .. })
}

/// Walk a plain switch body (as parsed by parse_statement) and pull out every
/// pattern arm (Cond / Bind / boxed literal / when guard) plus `default`.
///
/// Grouping follows C fallthrough: after a `case`/`default` label, every
/// following sibling belongs to that arm until the next label, so an arm body
/// is re-wrapped as a compound of its own statement plus those siblings.
///
/// Plain constant arms (`case 1:`) stay untouched on the C path — the collected
/// list only decides whether the switch needs pattern lowering at all. When any
/// pattern arm is found the caller converts the WHOLE switch to SwitchPat and
/// discards the original body, so a constant arm seen along the way is flagged
/// via `has_const_arm` for the caller to reject.
fn collect_pattern_arms(body: &CstStmt, st: &mut PatternArms) {
    let CstStmtData::Compound(inner) = &body.data else { return; };
    let mut i = 0;
    while i < inner.len() {
        if !is_arm_node(&inner[i]) {
            i += 1;
            continue;
        }
        // This arm owns every sibling up to the next arm node.
        let mut j = i + 1;
        while j < inner.len() && !is_arm_node(&inner[j]) {
            j += 1;
        }
        collect_arm_node(&inner[i], &inner[i + 1..j], st);
        i = j;
    }
}

/// Fold one arm label node (plus its fallthrough siblings) into the flat list.
fn collect_arm_node(node: &CstStmt, rest: &[CstStmt], st: &mut PatternArms) {
    let (line, column) = (node.line, node.column);
    // The arm body: the label's own statement, then every fallthrough sibling.
    let grouped = |own: &CstStmt| -> Box<CstStmt> {
        if rest.is_empty() {
            return Box::new(own.clone());
        }
        let mut group = Vec::with_capacity(rest.len() + 1);
        group.push(own.clone());
        group.extend(rest.iter().cloned());
        Box::new(CstStmt { kind: CstStmtKind::Compound, line, column, data: CstStmtData::Compound(group), end_line: node.end_line, end_column: node.end_column })
    };

    // `case a, b:` stacks Case nodes — walk to the innermost body, then
    // register every value of the chain with that shared body.
    let mut values: Vec<&CstExpr> = Vec::new();
    let mut cur = node;
    let innermost: &CstStmt;
    loop {
        match &cur.data {
            CstStmtData::Case { value, body } => {
                values.push(value);
                cur = body;
            }
            _ => {
                innermost = cur;
                break;
            }
        }
    }
    if !values.is_empty() {
        for v in values {
            if is_object_literal(v) {
                st.has_object_arm = true;
                st.arms.push(CstArm {
                    pattern: CstPattern::Const(Box::new(v.clone())),
                    guard: None,
                    body: grouped(innermost),
                    line, column,
                    end_line: innermost.end_line,
                    end_column: innermost.end_column,
                });
            } else {
                st.has_const_arm = true;
            }
        }
        return;
    }
    match &node.data {
        // Single-arm wrapper built by the Case branch for a pattern arm: merge
        // it (with its fallthrough siblings) into the enclosing flat list.
        CstStmtData::SwitchPat { arms, has_default, .. } => {
            for a in arms {
                match &a.pattern {
                    // A type binding is an object arm for sure. A `Const` arm
                    // reaches here already filtered to object literals by the
                    // branch above, so it counts too.
                    CstPattern::Bind { .. } | CstPattern::Const(_) => st.has_object_arm = true,
                    CstPattern::Cond(_) => st.has_cond_arm = true,
                }
                let mut a = a.clone();
                a.body = grouped(&a.body);
                st.arms.push(a);
            }
            st.has_default |= *has_default;
        }
        CstStmtData::Default(own) => {
            st.has_default = true;
            if st.default_body.is_none() {
                st.default_body = Some(grouped(own));
            }
        }
        _ => {}
    }
}

mod tests {
    // `#[test]` bodies are stripped from a non-test build, so an ungated
    // `use super::*` here warns as unused in `cargo build` while being
    // required by `cargo test` (CstDeclKind/CstDeclData reach this module only
    // through the file-level `use jeti_cst::*` in the parent). Gate it.
    #[cfg(test)]
    use super::*;
    #[cfg(test)]
    use crate::Parser;

    /// True iff the parsed unit contains at least one Await operator node.
    // `mod tests` itself is not cfg(test)-gated in this file (historical
    // shape); every non-`#[test]` item here must carry the gate or its body
    // gets type-checked in a normal build, where `#[cfg(test)]` uses are gone.
    #[cfg(test)]
    fn unit_has_await(src: &str) -> bool {
        let mut p = Parser::new(src);
        let unit = p.parse_translation_unit().unwrap();
        // `Await { inner }` (CST struct variant): match the variant name
        // without assuming the old tuple form `Await(`.
        let dbg = format!("{:?}", unit);
        dbg.contains("Await {") || dbg.contains("Await(")
    }

    #[test]
    fn test_await_soft_keyword_table() {
        // Operator: `await` followed by a unary-start token (§3.2 start set).
        assert!(unit_has_await("void f(void) { await [w m]; }"));
        assert!(unit_has_await("void f(void) { await (x); }")); // callsite: C fn `await` renamed
        assert!(unit_has_await("void f(void) { await await [f m]; }")); // nested double unwrap
        assert!(unit_has_await("void f(void) { await ++x; }"));
        assert!(unit_has_await("void f(void) { await 42; }"));

        // Identifier: C-compat boundary table (§3.4).
        assert!(!unit_has_await("void f(void) { int await = 1; }")); // decl position
        assert!(!unit_has_await("void f(void) { await = 5; }")); // `=` not in set
        assert!(!unit_has_await("void f(void) { return await; }")); // `;`
        assert!(!unit_has_await("void f(void) { int r = await + 1; }")); // `+` excluded
        assert!(!unit_has_await("void f(void) { int r = await * p; }")); // `*` excluded
        assert!(!unit_has_await("void f(void) { await -t; }")); // `-` excluded
        assert!(!unit_has_await("void f(void) { await; }")); // bare, no operand
    }

    #[test]
    fn test_at_await_spelling_removed() {
        // §6: the old `@await` spelling is no longer accepted — it must not
        // produce an Await node (lexer has no AtAwait token anymore).
        assert!(!unit_has_await("void f(void) { @await [w m]; }"));
    }

    #[test]
    fn test_async_block_literal() {
        // §5.1 shapes (2026-10-15 `async ^` layout); async blocks carry
        // is_async: true. The return type sits AFTER `^` like a plain block's.
        let async_block = |src: &str| {
            let mut p = Parser::new(src);
            let unit = p.parse_translation_unit().unwrap();
            let text = format!("{:?}", unit);
            (text.contains("is_async: true"), text.contains("is_async: false"), p.last_error().is_empty())
        };

        // Complete forms: `async ^` Return-type? Params? Body.
        let (a, has_plain, ok) = async_block("void f(void) { async ^ void (NPString *u) { await [self fetchUser:u]; }; }");
        assert!(a && ok && !has_plain, "async void form: a={} ok={} err", a, ok);
        let (a, _, ok) = async_block("void f(void) { async ^ int (int x) { return x + 1; }; }");
        assert!(a && ok, "async int form");
        let (a, _, ok) = async_block("void f(void) { async ^ NPString *(NPString *k) { return await [self get:k]; }; }");
        assert!(a && ok, "async object-return form");

        // Ellided form (inference per §8-①: rules A-D).
        let (a, _, ok) = async_block("void f(void) { async ^ { [self performTask]; }; }");
        assert!(a && ok, "ellided form");
        // `(void)` no-param spelling.
        let (a, _, ok) = async_block("void f(void) { async ^ void (void) { [self t]; }; }");
        assert!(a && ok, "(void) param form");

        // IIFE: definition-then-call, empty and with args (user 2026-10-11).
        let (a, _, ok) = async_block("void f(void) { async ^ void (void) { [self t]; }(); }");
        assert!(a && ok, "IIFE empty-args form");
        let (a, _, ok) = async_block("void f(void) { async ^ void (NPString *u) { await [self fetchUser:u]; }(u); }");
        assert!(a && ok, "IIFE with-args form");

        // The removed `async T ^…` layout (return type between flag and `^`)
        // is a migration error, not a silent reparse.
        let mut p = Parser::new("void f(void) { async void ^(void) { [self t]; }; }");
        let _ = p.parse_translation_unit().unwrap();
        assert!(!p.last_error().is_empty(), "old async T ^ spelling must error");

        // Plain blocks stay is_async: false — including `^async(...)` with a
        // user typedef named `async` as the return type (the whole point of
        // the `async ^` layout: the two never share a syntax slot).
        let (_, has_plain, ok) = async_block("void f(void) { int (^b)(int) = ^(int x) { return x; }; }");
        assert!(has_plain && ok, "plain block not marked async");
        let (_, has_plain, ok) = async_block("void f(void) { int v = ^async (int x) { return x; }(3); }");
        assert!(has_plain && ok, "^async(...) is a plain block with typedef return type");
    }

    #[test]
    fn test_blockon_paren_async_block_arg() {
        // 2026-10-15 regression: `[NPTask blockOn:(async ^{ … })]` — a
        // paren-wrapped async block as a message argument used to fall through
        // the blockOn bridge and emit `NPTask->blockOn_`. Parsing itself was
        // never wrong (probe-verified: the Paren(Block{is_async:true}) AST was
        // correct), but pin the message-arg shapes so a parser regression
        // here is caught at the parser layer.
        let msg_arg = |src: &str| {
            let mut p = Parser::new(src);
            let unit = p.parse_translation_unit().unwrap();
            let text = format!("{:?}", unit);
            (
                text.contains("Message"),
                text.contains("is_async: true"),
                text.contains("is_async: false"),
                p.last_error().is_empty(),
            )
        };

        // A. plain block, paren-wrapped, as message argument.
        let (msg, a, pa, ok) = msg_arg(
            "void f(void) { [NPTask blockOn:(^{ doSomething(); })]; }");
        assert!(msg && !a && pa && ok, "A plain-block paren arg: msg={} a={} pa={} err", msg, a, pa);
        // B. async block, paren-wrapped, as message argument (the M3 shape).
        let (msg, a, pa, ok) = msg_arg(
            "void f(void) { [NPTask blockOn:(async ^{ await [api run]; })]; }");
        assert!(msg && a && !pa && ok, "B async-block paren arg: msg={} a={} pa={} err", msg, a, pa);
        // C. async block as a local initializer (paren and bare).
        let (msg, a, _, ok) = msg_arg("void f(void) { id t = (async ^{ await [api run]; }); }");
        assert!(msg && a && ok, "C paren initializer");
        let (msg, a, _, ok) = msg_arg("void f(void) { id t = async ^{ await [api run]; }; }");
        assert!(msg && a && ok, "C bare initializer");
        // D. async block as a plain function-call argument.
        let (msg, a, _, ok) = msg_arg("void f(void) { sink(async ^{ await [api run]; }); }");
        assert!(msg && a && ok, "D function-call arg");
        // E. async block as an argument of another message send.
        let (msg, a, _, ok) = msg_arg(
            "void f(void) { [box hold:(async ^{ await [api run]; })]; }");
        assert!(msg && a && ok, "E nested message arg");

        // Paren-wrapping must not regress either spelling of the bridge.
        let (msg, a, _, ok) = msg_arg("void f(void) { [NPTask blockOn:async ^ void { [api run]; }]; }");
        assert!(msg && a && ok, "bare blockOn arg");
        let (msg, a, _, ok) = msg_arg("void f(void) { [NPTask blockOn:(async ^ void { [api run]; })]; }");
        assert!(msg && a && ok, "paren blockOn arg");
    }

    #[test]
    fn test_parse_empty() {
        let mut p = Parser::new("");
        let unit = p.parse_translation_unit();
        assert!(unit.is_some());
        assert_eq!(unit.unwrap().decls.len(), 0);
    }

    #[test]
    fn test_parse_integer_var() {
        let mut p = Parser::new("int x = 42;");
        let unit = p.parse_translation_unit().unwrap();
        assert_eq!(unit.decls.len(), 1);
        let decl = &unit.decls[0];
        assert_eq!(decl.kind, CstDeclKind::Variable);
        if let CstDeclData::Variable { ref var_type, .. } = decl.data {
            assert!(var_type.is_some());
        }
    }

    #[test]
    fn test_parse_function() {
        let mut p = Parser::new("int foo() { return 0; }");
        let unit = p.parse_translation_unit().unwrap();
        assert_eq!(unit.decls.len(), 1, "expected 1 decl, got {}, err={}", unit.decls.len(), p.last_error());
        let decl = &unit.decls[0];
        assert_eq!(decl.kind, CstDeclKind::Function);
    }

    #[test]
    fn test_parse_function_with_param() {
        let mut p = Parser::new("int foo(int val) { return val; }");
        let unit = p.parse_translation_unit().unwrap();
        assert_eq!(unit.decls.len(), 1, "expected 1 decl, got {}, err={}", unit.decls.len(), p.last_error());
        let decl = &unit.decls[0];
        assert_eq!(decl.kind, CstDeclKind::Function);
    }

    #[test]
    fn test_parse_failed() {
        let mut p = Parser::new("struct { int x; }");
        let unit = p.parse_translation_unit();
        assert!(unit.is_some() || p.has_error());
    }

    #[test]
    fn test_debug_jeti_alloc() {
        let source = r#"id jeti_alloc(struct NPClass *cls);"#;
        let mut p = Parser::new(source);
        let unit = p.parse_translation_unit().unwrap();
        println!("decls: {}", unit.decls.len());
        for d in &unit.decls {
            println!("kind: {:?}", d.kind);
            println!("name: {:?}", d.name);
            if let jeti_cst::CstDeclData::Function { ref return_type, ref params, .. } = d.data {
                println!("return_type: {:?}", return_type);
                if let Some(ref p) = params {
                    let mut q: Option<&Box<jeti_cst::CstParam>> = Some(p);
                    while let Some(param) = q {
                        println!("  param name: {:?}", param.name);
                        println!("  param type: {:?}", param.par_type);
                        if let Some(ref t) = param.par_type {
                            println!("    prim: {:?}", t.prim);
                            println!("    name: {:?}", t.name);
                            println!("    is_struct: {}", t.is_struct);
                            println!("    is_pointer: {}", t.is_pointer);
                        }
                        q = param.next.as_ref();
                    }
                }
            }
        }
        assert_eq!(unit.decls.len(), 1);
    }
}