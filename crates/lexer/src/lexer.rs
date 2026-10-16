use crate::token::{KeywordKind, Token, TokenKind, INT_SUFFIX_L, INT_SUFFIX_LL, INT_SUFFIX_U};

const KW_TABLE: &[(&str, KeywordKind)] = &[
    ("@interface", KeywordKind::AtInterface),
    ("@implementation", KeywordKind::AtImplementation),
    ("@end", KeywordKind::AtEnd),
    ("@property", KeywordKind::AtProperty),
    ("@synthesize", KeywordKind::AtSynthesize),
    ("@dynamic", KeywordKind::AtDynamic),
    ("@selector", KeywordKind::AtSelector),
    ("@encode", KeywordKind::AtEncode),
    ("@protocol", KeywordKind::AtProtocol),
    ("@optional", KeywordKind::AtOptional),
    ("@required", KeywordKind::AtRequired),
    ("@class", KeywordKind::AtClass),
    ("@try", KeywordKind::AtTry),
    ("@defer", KeywordKind::AtDefer),
    ("@catch", KeywordKind::AtCatch),
    ("@finally", KeywordKind::AtFinally),
    ("@throw", KeywordKind::AtThrow),
    ("@throws", KeywordKind::AtThrows),
    ("@synchronized", KeywordKind::AtSynchronized),
    ("@autoreleasepool", KeywordKind::AtAutoreleasepool),
    ("@public", KeywordKind::AtPublic),
    ("@package", KeywordKind::AtPackage),
    ("@protected", KeywordKind::AtProtected),
    ("@private", KeywordKind::AtPrivate),
    ("@defs", KeywordKind::AtDefs),
    ("@namespace", KeywordKind::AtNamespace),
    ("@endnamespace", KeywordKind::AtEndNamespace),
    ("@using", KeywordKind::AtUsing),
    ("@noarc", KeywordKind::AtNoArc),
    ("readwrite", KeywordKind::AtReadwrite),
    ("readonly", KeywordKind::AtReadonly),
    ("weak", KeywordKind::AtWeak),
    ("strong", KeywordKind::AtStrong),
    ("assign", KeywordKind::AtAssign),
    ("retain", KeywordKind::AtRetain),
    ("copy", KeywordKind::AtCopy),
    ("nonatomic", KeywordKind::AtNonatomic),
    ("getter", KeywordKind::AtGetter),
    ("setter", KeywordKind::AtSetter),
    ("YES", KeywordKind::Yes),
    ("NO", KeywordKind::No),
    ("nil", KeywordKind::Nil),
    ("NULL", KeywordKind::Null),
    ("self", KeywordKind::Self_),
    ("super", KeywordKind::Super),
    ("_cmd", KeywordKind::Cmd),
    ("__block", KeywordKind::Block),
    ("__weak", KeywordKind::Weak),
    ("__strong", KeywordKind::Strong),
    ("__autoreleasing", KeywordKind::Autoreleasing),
    ("__unsafe_unretained", KeywordKind::UnsafeUnretained),
    ("instancetype", KeywordKind::Instancetype),
    ("id", KeywordKind::Id),
    ("Class", KeywordKind::Class),
    ("SEL", KeywordKind::Sel),
    ("BOOL", KeywordKind::Bool),
    ("IMP", KeywordKind::Imp),
    ("NPZone", KeywordKind::NpZone),
    ("return", KeywordKind::Return),
    ("if", KeywordKind::If),
    ("else", KeywordKind::Else),
    ("switch", KeywordKind::Switch),
    ("case", KeywordKind::Case),
    ("default", KeywordKind::Default),
    ("while", KeywordKind::While),
    ("do", KeywordKind::Do),
    ("for", KeywordKind::For),
    // `in` is deliberately NOT a keyword: ObjC/clang treat it as a contextual
    // keyword recognised only inside a for-in header (position-based), so C
    // identifiers named `in` (variables, struct fields, `x.in` member access)
    // stay legal — jeti is a C superset. The parser matches the token TEXT
    // `in` at the for-header position instead.
    ("break", KeywordKind::Break),
    ("continue", KeywordKind::Continue),
    ("goto", KeywordKind::Goto),
    ("sizeof", KeywordKind::Sizeof),
    ("typeof", KeywordKind::Typeof),
    ("struct", KeywordKind::Struct),
    ("union", KeywordKind::Union),
    ("enum", KeywordKind::Enum),
    ("typedef", KeywordKind::Typedef),
    ("void", KeywordKind::Void),
    ("char", KeywordKind::Char),
    ("short", KeywordKind::Short),
    ("int", KeywordKind::Int),
    ("long", KeywordKind::Long),
    ("float", KeywordKind::Float),
    ("double", KeywordKind::Double),
    // C99 `_Complex` (§6.2.5p13) — a type qualifier-ish keyword that follows
    // `float`/`double` (`float _Complex x`). Kept as a keyword so
    // `is_declaration_start` treats `float _Complex x` as a declaration.
    ("_Complex", KeywordKind::Complex),
    ("signed", KeywordKind::Signed),
    ("unsigned", KeywordKind::Unsigned),
    ("const", KeywordKind::Const),
    ("volatile", KeywordKind::Volatile),
    ("static", KeywordKind::Static),
    ("extern", KeywordKind::Extern),
    ("auto", KeywordKind::Auto),
    ("register", KeywordKind::Register),
    ("inline", KeywordKind::Inline),
    ("restrict", KeywordKind::Restrict),
    ("#import", KeywordKind::Import),
    ("#include", KeywordKind::Include),
    ("#define", KeywordKind::Define),
    ("#ifdef", KeywordKind::Ifdef),
    ("#ifndef", KeywordKind::Ifndef),
    ("#endif", KeywordKind::Endif),
    ("#pragma", KeywordKind::Pragma),
    // `_Pragma` (ISO/IEC 9899:2011 §6.10.9, C99) is the operator spelling of a
    // pragma. It shares `#pragma`'s handling: the parser parks the whole source
    // line in a RawLine node and codegen re-emits it verbatim at the same
    // position, so the C compiler applies it exactly where it was written.
    ("_Pragma", KeywordKind::Pragma),
    ("#if", KeywordKind::If),
    ("#else", KeywordKind::Else),
    ("#elif", KeywordKind::Elif),
    ("#undef", KeywordKind::Undef),
    ("asm", KeywordKind::Asm),
    ("__asm", KeywordKind::Asm),
    ("__asm__", KeywordKind::Asm),
    ("__volatile", KeywordKind::Volatile),
    ("__volatile__", KeywordKind::Volatile),
    ("__typeof", KeywordKind::Typeof),
    ("__typeof__", KeywordKind::Typeof),
    ("__const", KeywordKind::Const),
    ("__const__", KeywordKind::Const),
    ("__inline", KeywordKind::Inline),
    ("__inline__", KeywordKind::Inline),
    ("__signed", KeywordKind::Signed),
    ("__signed__", KeywordKind::Signed),
    ("__restrict", KeywordKind::Restrict),
    ("__restrict__", KeywordKind::Restrict),
    ("__extension__", KeywordKind::Extension),
    ("__alignof", KeywordKind::Alignof),
    ("__alignof__", KeywordKind::Alignof),
];

fn lookup_keyword(s: &str) -> KeywordKind {
    for (kstr, kw) in KW_TABLE {
        if *kstr == s {
            return *kw;
        }
    }
    KeywordKind::None
}

pub struct Lexer<'a> {
    source: &'a str,
    pos: usize,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        let lexer = Lexer {
            source,
            pos: 0,
            line: 1,
            column: 1,
        };
        lexer
    }

    // Snapshot the full lexer position so a parser can try a speculative parse
    // and then rewind (e.g. when `<` might be a generic arg opener or a comparison).
    pub fn save_pos(&self) -> (usize, usize, usize) { (self.pos, self.line, self.column) }
    pub fn restore_pos(&mut self, saved: (usize, usize, usize)) {
        self.pos = saved.0; self.line = saved.1; self.column = saved.2;
    }

    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.pos).copied()
    }

    fn peek_next(&self) -> Option<u8> {
        self.source.as_bytes().get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let c = self.source.as_bytes().get(self.pos).copied()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    fn skip_line_comment(&mut self) {
        while self.pos < self.source.len() && self.source.as_bytes()[self.pos] != b'\n' {
            self.advance();
        }
    }

    fn skip_block_comment(&mut self) {
        while self.pos + 1 < self.source.len() {
            if self.source.as_bytes()[self.pos] == b'*' && self.source.as_bytes()[self.pos + 1] == b'/' {
                self.advance();
                self.advance();
                return;
            }
            self.advance();
        }
    }

    fn skip_whitespace(&mut self) {
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => break,
            };
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => {
                    self.advance();
                }
                b'/' => {
                    match self.peek_next() {
                        Some(b'/') => {
                            self.advance();
                            self.advance();
                            self.skip_line_comment();
                        }
                        Some(b'*') => {
                            self.advance();
                            self.advance();
                            self.skip_block_comment();
                        }
                        _ => break,
                    }
                }
                _ => break,
            }
        }
    }

    fn make_token(&self, kind: TokenKind, start: usize, length: usize, keyword: KeywordKind) -> Token {
        Token {
            kind,
            keyword,
            start,
            length,
            line: self.line,
            column: self.column.saturating_sub(length),
            char_val: 0,
            end_line: self.line,
            end_col: self.column,
            int_suffix: 0,
        }
    }

    fn make_eof(&self) -> Token {
        Token {
            kind: TokenKind::Eof,
            keyword: KeywordKind::None,
            start: self.pos,
            length: 0,
            line: self.line,
            column: self.column,
            char_val: 0,
            end_line: self.line,
            end_col: self.column,
            int_suffix: 0,
        }
    }

    fn read_string(&mut self, _start: usize) -> Token {
        let start = self.pos;
        while self.pos < self.source.len() && self.source.as_bytes()[self.pos] != b'"' {
            if self.source.as_bytes()[self.pos] == b'\\' {
                self.advance();
            }
            self.advance();
        }
        let end = self.pos;
        if self.pos < self.source.len() {
            self.advance();
        }
        Token {
            kind: TokenKind::String,
            keyword: KeywordKind::None,
            start,
            length: end - start,
            line: self.line,
            column: self.column.saturating_sub(end - start + 1),
            char_val: 0,
            end_line: self.line,
            end_col: self.column,
            int_suffix: 0,
        }
    }

    fn read_at_string(&mut self, _start: usize) -> Token {
        let start = self.pos;
        while self.pos < self.source.len() && self.source.as_bytes()[self.pos] != b'"' {
            if self.source.as_bytes()[self.pos] == b'\\' {
                self.advance();
            }
            self.advance();
        }
        let end = self.pos;
        if self.pos < self.source.len() {
            self.advance();
        }
        Token {
            kind: TokenKind::AtString,
            keyword: KeywordKind::None,
            start,
            length: end - start,
            line: self.line,
            column: self.column.saturating_sub(end - start + 1),
            char_val: 0,
            end_line: self.line,
            end_col: self.column,
            int_suffix: 0,
        }
    }

    pub fn next_token(&mut self) -> Token {
        self.skip_whitespace();

        let line = self.line;
        let col = self.column;
        let start = self.pos;

        let c = match self.peek() {
            Some(c) => c,
            None => return self.make_eof(),
        };

        // Identifier or keyword
        if c.is_ascii_alphabetic() || c == b'_' {
            while self.pos < self.source.len() {
                let nc = self.source.as_bytes()[self.pos];
                if nc.is_ascii_alphanumeric() || nc == b'_' {
                    self.advance();
                } else {
                    break;
                }
            }
            let token_text = &self.source[start..self.pos];
            let kw = lookup_keyword(token_text);
            let kind = if kw != KeywordKind::None {
                TokenKind::Keyword
            } else {
                TokenKind::Identifier
            };
            return self.make_token(kind, start, self.pos - start, kw);
        }

        // @-prefixed keywords / literals
        if c == b'@' {
            self.advance();
            match self.peek() {
                Some(b'"') => {
                    self.advance();
                    return self.read_at_string(start + 1);
                }
                Some(b'[') => {
                    self.advance(); // consume [
                    return Token {
                        kind: TokenKind::AtArray,
                        keyword: KeywordKind::None,
                        start,
                        length: 2,
                        line,
                        column: col,
                        char_val: 0,
                        end_line: self.line,
                        end_col: self.column,
                        int_suffix: 0,
                    };
                }
                Some(b'{') => {
                    self.advance(); // consume {
                    return Token {
                        kind: TokenKind::AtDict,
                        keyword: KeywordKind::None,
                        start,
                        length: 2,
                        line,
                        column: col,
                        char_val: 0,
                        end_line: self.line,
                        end_col: self.column,
                        int_suffix: 0,
                    };
                }
                Some(b'(') => {
                    self.advance(); // consume (
                    return Token {
                        kind: TokenKind::AtLParen,
                        keyword: KeywordKind::None,
                        start,
                        length: 2,
                        line,
                        column: col,
                        char_val: 0,
                        end_line: self.line,
                        end_col: self.column,
                        int_suffix: 0,
                    };
                }
                Some(b'\'') => {
                    // `@'c'` — boxed character literal (NPNumber char).
                    self.advance(); // consume opening '
                    let mut val = match self.advance() {
                        Some(v) => v,
                        None => 0,
                    };
                    if val == b'\\' {
                        match self.advance() {
                            Some(b'n') => val = b'\n',
                            Some(b't') => val = b'\t',
                            Some(b'r') => val = b'\r',
                            Some(b'0') => val = b'\0',
                            Some(b'b') => val = 8,
                            Some(b'f') => val = 12,
                            Some(b'v') => val = 11,
                            Some(b'\\') => val = b'\\',
                            Some(b'\'') => val = b'\'',
                            Some(b'"') => val = b'"',
                            Some(b'x') | Some(b'X') => {
                                let mut hex_val = 0u32;
                                for _ in 0..2 {
                                    match self.peek() {
                                        Some(d) if d.is_ascii_hexdigit() => {
                                            hex_val = hex_val * 16 + (d as char).to_digit(16).unwrap();
                                            self.advance();
                                        }
                                        _ => break,
                                    }
                                }
                                val = hex_val as u8;
                            }
                            _ => {}
                        }
                    }
                    if self.peek() == Some(b'\'') {
                        self.advance();
                    }
                    return Token {
                        kind: TokenKind::AtChar,
                        keyword: KeywordKind::None,
                        start,
                        length: self.pos - start,
                        line,
                        column: col,
                        char_val: val,
                        end_line: self.line,
                        end_col: self.column,
                        int_suffix: 0,
                    };
                }
                Some(nc) if nc.is_ascii_alphabetic() || nc == b'_' => {
                    while self.pos < self.source.len() {
                        let ncc = self.source.as_bytes()[self.pos];
                        if ncc.is_ascii_alphanumeric() || ncc == b'_' {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let token_text = &self.source[start..self.pos];
                    let kw = lookup_keyword(token_text);
                    if kw == KeywordKind::None {
                        // `@YES` / `@NO` / `@true` / `@false` — boxed BOOL
                        // literal. Any other `@Ident` stays a loud Error token
                        // rather than silently degrading.
                        let bool_val = match token_text {
                            "@YES" | "@true" => Some(1u8),
                            "@NO" | "@false" => Some(0u8),
                            _ => None,
                        };
                        if let Some(v) = bool_val {
                            return Token {
                                kind: TokenKind::AtBool,
                                keyword: KeywordKind::None,
                                start,
                                length: self.pos - start,
                                line,
                                column: col,
                                char_val: v,
                                end_line: self.line,
                                end_col: self.column,
                                int_suffix: 0,
                            };
                        }
                        return self.make_token(TokenKind::Error, start, self.pos - start, KeywordKind::None);
                    }
                    return self.make_token(TokenKind::Keyword, start, self.pos - start, kw);
                }
                Some(nc) if nc.is_ascii_digit() => {
                    // `@123` / `@1.5` — NPNumber boxing literal. The `@` has
                    // already been consumed; scan the numeric text.
                    while self.pos < self.source.len()
                        && self.source.as_bytes()[self.pos].is_ascii_digit() {
                        self.advance();
                    }
                    // Fractional part: only consume `.` when followed by a digit,
                    // so `@1.foo` never swallows the dot.
                    if self.source.as_bytes().get(self.pos) == Some(&b'.')
                        && self.source.as_bytes().get(self.pos + 1).map_or(false, |b| b.is_ascii_digit()) {
                        self.advance(); // '.'
                        while self.pos < self.source.len()
                            && self.source.as_bytes()[self.pos].is_ascii_digit() {
                            self.advance();
                        }
                    }
                    // Exponent: e/E [sign] digits.
                    if matches!(self.source.as_bytes().get(self.pos), Some(b'e') | Some(b'E')) {
                        let save = self.pos;
                        self.advance();
                        if matches!(self.source.as_bytes().get(self.pos), Some(b'+') | Some(b'-')) {
                            self.advance();
                        }
                        if self.source.as_bytes().get(self.pos).map_or(false, |b| b.is_ascii_digit()) {
                            while self.pos < self.source.len()
                                && self.source.as_bytes()[self.pos].is_ascii_digit() {
                                self.advance();
                            }
                        } else {
                            self.pos = save; // not actually an exponent
                        }
                    }
                    // Integer suffix (u/U/l/L, `@7L`): consumed and recorded in
                    // int_suffix, stripped from the token text — same rule as
                    // bare integer literals, so the boxed literal keeps its
                    // width instead of degrading to `int`.
                    let mut suf = 0;
                    let mut suf_bits: u8 = 0;
                    while let Some(n) = self.peek() {
                        match n {
                            b'u' | b'U' => { self.advance(); suf += 1; suf_bits |= INT_SUFFIX_U; }
                            b'l' | b'L' => {
                                self.advance(); suf += 1;
                                if suf_bits & (INT_SUFFIX_L | INT_SUFFIX_LL) != 0 { suf_bits |= INT_SUFFIX_LL; }
                                else { suf_bits |= INT_SUFFIX_L; }
                            }
                            _ => break,
                        }
                    }
                    return Token {
                        kind: TokenKind::AtNumber,
                        keyword: KeywordKind::None,
                        start: start + 1, // exclude the leading '@'
                        length: self.pos - (start + 1) - suf,
                        line,
                        column: col,
                        char_val: 0,
                        end_line: self.line,
                        end_col: self.column,
                        int_suffix: suf_bits,
                    };
                }
                _ => {
                    return self.make_token(TokenKind::Error, start, 1, KeywordKind::None);
                }
            }
        }

        // #-prefixed directives
        if c == b'#' {
            self.advance();
            while self.pos < self.source.len() {
                let nc = self.source.as_bytes()[self.pos];
                if nc.is_ascii_alphabetic() || nc == b'_' {
                    self.advance();
                } else {
                    break;
                }
            }
            let token_text = &self.source[start..self.pos];
            let kw = lookup_keyword(token_text);
            let kind = if kw != KeywordKind::None {
                TokenKind::Keyword
            } else {
                TokenKind::PpHash
            };
            return self.make_token(kind, start, self.pos - start, kw);
        }

        // String literal
        if c == b'"' {
            self.advance();
            return self.read_string(start + 1);
        }

        // Char literal
        if c == b'\'' {
            self.advance();
            let mut val = match self.advance() {
                Some(v) => v,
                None => 0,
            };
            if val == b'\\' {
                match self.advance() {
                    Some(b'n') => val = b'\n',
                    Some(b't') => val = b'\t',
                    Some(b'r') => val = b'\r',
                    Some(b'0') => val = b'\0',
                    Some(b'b') => val = 8,
                    Some(b'f') => val = 12,
                    Some(b'v') => val = 11,
                    Some(b'\\') => val = b'\\',
                    Some(b'\'') => val = b'\'',
                    Some(b'"') => val = b'"',
                    Some(b'x') | Some(b'X') => {
                        let mut hex_val = 0u32;
                        for _ in 0..2 {
                            match self.peek() {
                                Some(d) if d.is_ascii_hexdigit() => {
                                    hex_val = hex_val * 16 + (d as char).to_digit(16).unwrap();
                                    self.advance();
                                }
                                _ => break,
                            }
                        }
                        val = hex_val as u8;
                    }
                    _ => {}
                }
            }
            if self.peek() == Some(b'\'') {
                self.advance();
            }
            return Token {
                kind: TokenKind::Char,
                keyword: KeywordKind::None,
                start,
                length: self.pos - start,
                line,
                column: col,
                char_val: val,
                end_line: self.line,
                end_col: self.column,
                int_suffix: 0,
            };
        }

        // Number
        if c.is_ascii_digit() || (c == b'.' && self.peek_next().map_or(false, |d| d.is_ascii_digit())) {
            let mut is_float = c == b'.';

            // Detect 0x / 0X prefix
            if c == b'0' && self.peek_next().map_or(false, |n| n == b'x' || n == b'X') {
                self.advance();
                self.advance();
                while self.peek().map_or(false, |n| n.is_ascii_hexdigit()) {
                    self.advance();
                }
                let mut suf = 0;
                let mut suf_bits: u8 = 0;
                while let Some(n) = self.peek() {
                    match n {
                        b'u' | b'U' => { self.advance(); suf += 1; suf_bits |= INT_SUFFIX_U; }
                        b'l' | b'L' => {
                            self.advance(); suf += 1;
                            if suf_bits & (INT_SUFFIX_L | INT_SUFFIX_LL) != 0 { suf_bits |= INT_SUFFIX_LL; }
                            else { suf_bits |= INT_SUFFIX_L; }
                        }
                        _ => break,
                    }
                }
                let end = self.pos;
                let mut tok = self.make_token(TokenKind::Integer, start, end - start - suf, KeywordKind::None);
                tok.int_suffix = suf_bits;
                return tok;
            }

            if c == b'.' {
                self.advance();
            }

            loop {
                match self.peek() {
                    Some(n) if n.is_ascii_digit() => {
                        self.advance();
                    }
                    Some(b'.') if !is_float => {
                        is_float = true;
                        self.advance();
                    }
                    Some(b'e') | Some(b'E') => {
                        is_float = true;
                        self.advance();
                        if self.peek() == Some(b'+') || self.peek() == Some(b'-') {
                            self.advance();
                        }
                    }
                    Some(b'u') | Some(b'U') | Some(b'l') | Some(b'L') => {
                        let mut suf = 0;
                        let mut suf_bits: u8 = 0;
                        while let Some(n) = self.peek() {
                            match n {
                                b'u' | b'U' => { self.advance(); suf += 1; suf_bits |= INT_SUFFIX_U; }
                                b'l' | b'L' => {
                                    self.advance(); suf += 1;
                                    if suf_bits & (INT_SUFFIX_L | INT_SUFFIX_LL) != 0 { suf_bits |= INT_SUFFIX_LL; }
                                    else { suf_bits |= INT_SUFFIX_L; }
                                }
                                _ => break,
                            }
                        }
                        let end = self.pos;
                        let mut tok = self.make_token(TokenKind::Integer, start, end - start - suf, KeywordKind::None);
                        tok.int_suffix = suf_bits;
                        return tok;
                    }
                    Some(b'f') | Some(b'F') => {
                        // Float suffix: the token kind is already known, so the
                        // outer `is_float` flag is irrelevant on this path.
                        self.advance();
                        let end = self.pos;
                        return self.make_token(TokenKind::Float, start, end - start, KeywordKind::None);
                    }
                    Some(b'i') | Some(b'I') | Some(b'j') | Some(b'J') => {
                        // C99 imaginary suffix (§6.4.4.2): `2.0i`, `1e3J`.
                        // Consumed so the literal parses; the imaginary part is
                        // NOT modelled — the token is emitted as a plain float
                        // (known limitation, matches no downstream use yet).
                        self.advance();
                        let end = self.pos;
                        return self.make_token(TokenKind::Float, start, end - start, KeywordKind::None);
                    }
                    _ => break,
                }
            }
            let kind = if is_float { TokenKind::Float } else { TokenKind::Integer };
            let end = self.pos;
            return self.make_token(kind, start, end - start, KeywordKind::None);
        }

        // Operators and punctuation
        let kind = match c {
            b'+' => {
                if self.peek_next() == Some(b'+') { self.advance(); self.advance(); TokenKind::Incr }
                else if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::PlusAssign }
                else { self.advance(); TokenKind::Plus }
            }
            b'-' => {
                if self.peek_next() == Some(b'-') { self.advance(); self.advance(); TokenKind::Decr }
                else if self.peek_next() == Some(b'>') {
                    self.advance(); self.advance();
                    if self.peek() == Some(b'*') { self.advance(); TokenKind::ArrowStar }
                    else { TokenKind::Arrow }
                }
                else if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::MinusAssign }
                else { self.advance(); TokenKind::Minus }
            }
            b'*' => {
                if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::StarAssign }
                else { self.advance(); TokenKind::Star }
            }
            b'/' => {
                if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::SlashAssign }
                else { self.advance(); TokenKind::Slash }
            }
            b'%' => {
                if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::PercentAssign }
                else { self.advance(); TokenKind::Percent }
            }
            b'&' => {
                if self.peek_next() == Some(b'&') { self.advance(); self.advance(); TokenKind::LogicalAnd }
                else if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::AndAssign }
                else { self.advance(); TokenKind::Ampersand }
            }
            b'|' => {
                if self.peek_next() == Some(b'|') { self.advance(); self.advance(); TokenKind::LogicalOr }
                else if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::OrAssign }
                else { self.advance(); TokenKind::Pipe }
            }
            b'^' => {
                if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::XorAssign }
                else { self.advance(); TokenKind::Caret }
            }
            b'~' => { self.advance(); TokenKind::Tilde }
            b'!' => {
                if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::Neq }
                else { self.advance(); TokenKind::Exclam }
            }
            b'<' => {
                if self.peek_next() == Some(b'<') {
                    self.advance(); self.advance();
                    if self.peek() == Some(b'=') { self.advance(); TokenKind::LShiftAssign }
                    else { TokenKind::LShift }
                } else if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::Leq }
                else { self.advance(); TokenKind::Less }
            }
            b'>' => {
                if self.peek_next() == Some(b'>') {
                    self.advance(); self.advance();
                    if self.peek() == Some(b'=') { self.advance(); TokenKind::RShiftAssign }
                    else { TokenKind::RShift }
                } else if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::Geq }
                else { self.advance(); TokenKind::Greater }
            }
            b'=' => {
                if self.peek_next() == Some(b'=') { self.advance(); self.advance(); TokenKind::Eq }
                else { self.advance(); TokenKind::Assign }
            }
            b',' => { self.advance(); TokenKind::Comma }
            b'.' => {
                if self.peek_next() == Some(b'.') {
                    self.advance();
                    if self.peek() == Some(b'.') { self.advance(); self.advance(); TokenKind::Ellipsis }
                    else { self.advance(); TokenKind::Error }
                } else if self.peek_next().map_or(false, |d| d.is_ascii_digit()) {
                    self.advance();
                    while self.peek().map_or(false, |d| d.is_ascii_digit()) { self.advance(); }
                    return self.make_token(TokenKind::Float, start, self.pos - start, KeywordKind::None);
                } else {
                    self.advance(); TokenKind::Dot
                }
            }
            b';' => { self.advance(); TokenKind::Semicolon }
            b':' => {
                if self.peek_next() == Some(b':') { self.advance(); self.advance(); TokenKind::ColonColon }
                else { self.advance(); TokenKind::Colon }
            }
            b'?' => { self.advance(); TokenKind::Question }
            b'[' => { self.advance(); TokenKind::LBracket }
            b']' => { self.advance(); TokenKind::RBracket }
            b'{' => { self.advance(); TokenKind::LBrace }
            b'}' => { self.advance(); TokenKind::RBrace }
            b'(' => { self.advance(); TokenKind::LParen }
            b')' => { self.advance(); TokenKind::RParen }
            _ => { self.advance(); TokenKind::Error }
        };

        self.make_token(kind, start, self.pos - start, KeywordKind::None)
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        let mut l = Lexer::new("");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Eof);
    }

    #[test]
    fn test_identifiers() {
        let mut l = Lexer::new("foo bar _baz");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Identifier);
        assert_eq!(t.text("foo bar _baz"), "foo");

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Identifier);
        assert_eq!(t.text("foo bar _baz"), "bar");

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Identifier);
        assert_eq!(t.text("foo bar _baz"), "_baz");
    }

    #[test]
    fn test_keywords() {
        let mut l = Lexer::new("return int if @interface void");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.keyword, KeywordKind::Return);

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.keyword, KeywordKind::Int);

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.keyword, KeywordKind::If);

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.keyword, KeywordKind::AtInterface);

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.keyword, KeywordKind::Void);
    }

    #[test]
    fn test_integers() {
        let mut l = Lexer::new("42 0x1A 0XFF");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Integer);
        assert_eq!(t.text("42 0x1A 0XFF"), "42");

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Integer);
        assert_eq!(t.text("42 0x1A 0XFF"), "0x1A");

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Integer);
        assert_eq!(t.text("42 0x1A 0XFF"), "0XFF");
    }

    #[test]
    fn test_floats() {
        let mut l = Lexer::new("3.14 .5 1e10 2.5e-3");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Float);
        assert_eq!(t.text("3.14 .5 1e10 2.5e-3"), "3.14");

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Float);
        assert_eq!(t.text("3.14 .5 1e10 2.5e-3"), ".5");

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Float);
        assert_eq!(t.text("3.14 .5 1e10 2.5e-3"), "1e10");
    }

    #[test]
    fn test_strings() {
        let mut l = Lexer::new(r#""hello" "world""#);
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::String);
        assert_eq!(t.text(r#""hello" "world""#), "hello");

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::String);
        assert_eq!(t.text(r#""hello" "world""#), "world");
    }

    #[test]
    fn test_at_string() {
        let mut l = Lexer::new(r#"@"test""#);
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtString);
        assert_eq!(t.text(r#"@"test""#), "test");
    }

    #[test]
    fn test_chars() {
        let mut l = Lexer::new("'a' '\\n' '\\x41'");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Char);
        assert_eq!(t.char_val, b'a');

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Char);
        assert_eq!(t.char_val, b'\n');

        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Char);
        assert_eq!(t.char_val, 0x41);
    }

    #[test]
    fn test_operators() {
        let mut l = Lexer::new("+ - * / % ++ -- -> += -= *= /= == != < > <= >= << >> && || ! ~ & | ^");
        assert_eq!(l.next_token().kind, TokenKind::Plus);
        assert_eq!(l.next_token().kind, TokenKind::Minus);
        assert_eq!(l.next_token().kind, TokenKind::Star);
        assert_eq!(l.next_token().kind, TokenKind::Slash);
        assert_eq!(l.next_token().kind, TokenKind::Percent);
        assert_eq!(l.next_token().kind, TokenKind::Incr);
        assert_eq!(l.next_token().kind, TokenKind::Decr);
        assert_eq!(l.next_token().kind, TokenKind::Arrow);
        assert_eq!(l.next_token().kind, TokenKind::PlusAssign);
        assert_eq!(l.next_token().kind, TokenKind::MinusAssign);
        assert_eq!(l.next_token().kind, TokenKind::StarAssign);
        assert_eq!(l.next_token().kind, TokenKind::SlashAssign);
        assert_eq!(l.next_token().kind, TokenKind::Eq);
        assert_eq!(l.next_token().kind, TokenKind::Neq);
        assert_eq!(l.next_token().kind, TokenKind::Less);
        assert_eq!(l.next_token().kind, TokenKind::Greater);
        assert_eq!(l.next_token().kind, TokenKind::Leq);
        assert_eq!(l.next_token().kind, TokenKind::Geq);
        assert_eq!(l.next_token().kind, TokenKind::LShift);
        assert_eq!(l.next_token().kind, TokenKind::RShift);
        assert_eq!(l.next_token().kind, TokenKind::LogicalAnd);
        assert_eq!(l.next_token().kind, TokenKind::LogicalOr);
        assert_eq!(l.next_token().kind, TokenKind::Exclam);
        assert_eq!(l.next_token().kind, TokenKind::Tilde);
        assert_eq!(l.next_token().kind, TokenKind::Ampersand);
        assert_eq!(l.next_token().kind, TokenKind::Pipe);
        assert_eq!(l.next_token().kind, TokenKind::Caret);
    }

    #[test]
    fn test_punctuation() {
        // @[ @{ @( each produce TWO tokens: @-prefix (LBracket/LBrace/LParen) and the bracket itself
        let mut l = Lexer::new("( ) { } [ ] ; : :: , . ?");
        assert_eq!(l.next_token().kind, TokenKind::LParen);
        assert_eq!(l.next_token().kind, TokenKind::RParen);
        assert_eq!(l.next_token().kind, TokenKind::LBrace);
        assert_eq!(l.next_token().kind, TokenKind::RBrace);
        assert_eq!(l.next_token().kind, TokenKind::LBracket);
        assert_eq!(l.next_token().kind, TokenKind::RBracket);
        assert_eq!(l.next_token().kind, TokenKind::Semicolon);
        assert_eq!(l.next_token().kind, TokenKind::Colon);
        assert_eq!(l.next_token().kind, TokenKind::ColonColon);
        assert_eq!(l.next_token().kind, TokenKind::Comma);
        assert_eq!(l.next_token().kind, TokenKind::Dot);
        assert_eq!(l.next_token().kind, TokenKind::Question);
    }

    #[test]
    fn test_at_array_dict_num() {
        // @[ → AtArray, @{ → AtDict, @( → AtLParen (boxed expression)
        let mut l = Lexer::new("@[ @{ @(");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtArray);
        assert_eq!(t.text("@[ @{ @("), "@[");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtDict);
        assert_eq!(t.text("@[ @{ @("), "@{");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtLParen);
        assert_eq!(t.text("@[ @{ @("), "@(");
    }

    #[test]
    fn test_at_boxing_tokens() {
        let src = "@'A' @'\\n' @YES @NO @true @false @Foo";
        let mut l = Lexer::new(src);
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtChar);
        assert_eq!(t.char_val, b'A');
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtChar);
        assert_eq!(t.char_val, b'\n');
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtBool);
        assert_eq!(t.char_val, 1);
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::AtBool);
        assert_eq!(t.char_val, 0);
        assert_eq!(l.next_token().kind, TokenKind::AtBool); // @true
        assert_eq!(l.next_token().kind, TokenKind::AtBool); // @false
        // Unknown `@Ident` stays a loud error instead of silently degrading.
        assert_eq!(l.next_token().kind, TokenKind::Error);
    }

    #[test]
    fn test_comments() {
        let mut l = Lexer::new("a // line comment\nb /* block */ c");
        assert_eq!(l.next_token().text("a // line comment\nb /* block */ c"), "a");
        assert_eq!(l.next_token().text("a // line comment\nb /* block */ c"), "b");
        assert_eq!(l.next_token().text("a // line comment\nb /* block */ c"), "c");
    }

    #[test]
    fn test_preprocessor() {
        let mut l = Lexer::new("#include <stdio.h>");
        let t = l.next_token();
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.keyword, KeywordKind::Include);
    }

    #[test]
    fn test_param_tokens() {
        let src = "int foo(int x) { return x; }";
        let mut l = Lexer::new(src);
        assert_eq!(l.next_token().kind, TokenKind::Keyword);
        assert_eq!(l.next_token().kind, TokenKind::Identifier);
        assert_eq!(l.next_token().kind, TokenKind::LParen);
        assert_eq!(l.next_token().kind, TokenKind::Keyword);
        assert_eq!(l.next_token().kind, TokenKind::Identifier);
        assert_eq!(l.next_token().kind, TokenKind::RParen);
    }
}