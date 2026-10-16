// Type primitives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypePrim {
    Void, Char, Short, Int, Long, LongLong,
    Float, Double, Bool, Signed, Unsigned,
    Id, Class, Sel, Instancetype,
    Named, Param,
}

// Aggregate tag keyword. The parser records which of the three C keywords
// introduced a tag; codegen must echo the same one back (`enum Mode m;` may
// NOT be rendered `struct Mode m;` — the tags are incompatible types in C).
// `None` means "no keyword recorded": paired with `is_struct` it renders as
// `struct`, which is what every struct-tagged type has always done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    None,
    Struct,
    Union,
    Enum,
}

impl TagKind {
    /// The C keyword to emit before a tag name. `None` renders as `struct`:
    /// types constructed before this field existed (and every type that is
    /// genuinely a struct) are tagged `None`, so their output is unchanged.
    pub fn keyword(self) -> &'static str {
        match self {
            TagKind::Union => "union",
            TagKind::Enum => "enum",
            TagKind::None | TagKind::Struct => "struct",
        }
    }
}

/// Nullability of a pointer type — ObjC's `nullable` / `nonnull` / unspecified
/// triple (PEP, see `doc/nullability_plan.md`).
///
/// Deliberately a THREE-state enum, not a bool. The third state is what makes
/// the whole design work: `Unspecified` ("nobody said") and `NullUnspecified`
/// ("I explicitly decline to say") must be distinguishable, otherwise adding
/// an annotation retroactively changes the meaning of untouched declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Nullability {
    /// No annotation. In an `NS_ASSUME_NONNULL` region this is coerced to
    /// `Nonnull` by the parser.
    #[default]
    Unspecified,
    Nonnull,
    Nullable,
    /// `_Null_unspecified` — explicit "I don't care", an opt-out from the
    /// region's default. Distinct from `Unspecified` by design.
    NullUnspecified,
}

#[derive(Debug, Clone)]
pub struct CstType {
    pub prim: TypePrim,
    pub is_pointer: bool,
    pub is_const: bool,
    pub is_volatile: bool,
    pub is_block: bool,
    pub is_array: bool,
    pub is_struct: bool,
    pub tag: TagKind,
    pub is_fn_ptr: bool,
    /// C99 `_Complex` (§6.2.5p13): `float _Complex x`. Pure passthrough — the
    /// base type renders unchanged and `_Complex` is appended when emitting C.
    pub is_complex: bool,
    pub is_block_qual: bool,
    pub is_weak_qual: bool,
    pub is_unsigned: bool,
    pub array_size: i32,
    /// Symbolic array size identifier (e.g. `MAX_CHILDREN` in
    /// `FSNode *_children[MAX_CHILDREN];`). When Some, codegen emits the
    /// named size rather than `[]` (flexible array member).
    pub array_size_name: Option<String>,
    pub subtype: Option<Box<CstType>>,
    pub name: Option<String>,
    pub block_name: Option<String>,
    pub block_params: Option<Box<CstType>>,
    pub next: Option<Box<CstType>>,
    pub protocols: Vec<String>,
    pub type_args: Vec<CstType>,
    /// Nullability annotation (`nullable` / `nonnull` / unspecified). Purely a
    /// compile-time checker input — codegen never reads it, so the annotation
    /// costs zero at runtime (same contract as ObjC's).
    pub nulls: Nullability,
}

impl CstType {
    pub fn new(prim: TypePrim) -> Self {
        CstType {
            prim,
            is_pointer: false, is_const: false, is_volatile: false,
            is_block: false, is_array: false, is_struct: false,
            tag: TagKind::None,
            is_fn_ptr: false,
            is_complex: false,
            is_block_qual: false, is_weak_qual: false, is_unsigned: false,
            array_size: 0,
            subtype: None, name: None, block_name: None,
            block_params: None, next: None,
            protocols: Vec::new(), type_args: Vec::new(),
            array_size_name: None,
            nulls: Nullability::Unspecified,
        }
    }
}

// Expression kinds
/// One link of a C99 designated-initializer designator chain:
/// `.field` or `[index]`. A full designator is a chain, e.g. `[2].y`.
#[derive(Debug, Clone)]
pub enum CstDesignator {
    Member(String),
    Index(Box<CstExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CstExprKind {
    Ident, Integer, Float, String, AtString, Char, Bool,
    Nil, Null, Self_, Super, Cmd,
    Selector, Encode, Protocol,
    ArrayLit, DictLit, NumberLit,
    DesignatedInit,
    Block, InitList,
    Unary, Binary, Ternary, Assign,
    Conditional, Cast, Sizeof, Typeof, Alignof,
    TypeLiteral,
    MessageSend, DotAccess, Arrow, Subscript,
    Call, Comma, Paren,
    /// `await <expr>` — suspension point inside an async method. The parser
    /// accepts `await` as a contextual keyword (it stays a legal C
    /// identifier elsewhere, e.g. `int await = 1;`).
    Await,
    /// `@(expr)` — boxed-expression literal. The parser cannot pick the
    /// `NPNumber` factory (it has no types), so the node is carried through
    /// to the checker, which rewrites it by the expression's static type.
    Boxed,
}

// Statement kinds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CstStmtKind {
    Expr, Compound, If, Switch, Case, Default,
    While, Do, For, ForIn,
    Break, Continue, Return, Goto, Label,
    Try, Catch, Finally, Throw,
    Synchronized, Autoreleasepool, NoArc, Decl, Asm, Defer,
}

// Declaration kinds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CstDeclKind {
    Function, Variable, Typedef,
    Struct, Union, Enum,
    ClassInterface, ClassImplementation,
    CategoryInterface, CategoryImplementation,
    Protocol, ForwardClass, ForwardProtocol,
    Method, Property, Ivar, IvarList,
    Namespace, Using, Asm, RawLine,
}

// Parameter
#[derive(Debug, Clone)]
pub struct CstParam {
    pub par_type: Option<Box<CstType>>,
    pub name: Option<String>,
    pub external_name: Option<String>,
    pub next: Option<Box<CstParam>>,
    pub attributes: Vec<String>,
}

// Expression node
#[derive(Debug, Clone)]
pub struct CstExpr {
    pub kind: CstExprKind,
    pub expr_type: Option<Box<CstType>>,
    pub line: usize,
    pub col: usize,
    /// Exclusive end of the node's extent, same conventions as `line` / `col`
    /// (line is an expanded-buffer line, column is already remapped to the
    /// source by the parser). `0` means "not stamped" — a synthesized node
    /// that never had tokens of its own. Together with `line`/`col` this is
    /// what `SourceMap::span_from_node` turns into a real file/byte span.
    pub end_line: usize,
    pub end_col: usize,
    pub data: CstExprData,
}

#[derive(Debug, Clone)]
pub enum CstExprData {
    Ident(String),
    Integer(i64),
    Float(f64),
    /// Float literal preserved verbatim (`2.0i`, `1e3j`) — imaginary suffixes
    /// (§6.4.4.2) carry semantics the f64 path would drop, so these pass
    /// through to C as raw text.
    FloatRaw(String),
    String(String),
    AtString(String),
    Char(u8),
    Bool(bool),
    Message {
        receiver: Box<CstExpr>,
        selector: String,
        args: Vec<CstExpr>,
    },
    Dot {
        object: Box<CstExpr>,
        property: String,
    },
    Arrow {
        object: Box<CstExpr>,
        property: String,
    },
    Subscript {
        object: Box<CstExpr>,
        key: Box<CstExpr>,
    },
    Call {
        callee: Box<CstExpr>,
        args: Vec<CstExpr>,
    },
    Binary {
        op: i32,
        left: Box<CstExpr>,
        right: Box<CstExpr>,
    },
    Assign {
        target: Box<CstExpr>,
        value: Box<CstExpr>,
    },
    Unary {
        op: i32,
        operand: Box<CstExpr>,
        is_postfix: bool,
    },
    Ternary {
        cond: Box<CstExpr>,
        true_expr: Box<CstExpr>,
        false_expr: Box<CstExpr>,
    },
    Cast {
        target_type: CstType,
        expr: Box<CstExpr>,
    },
    Comma(Vec<CstExpr>),
    Selector(String),
    Protocol(String),
    Encode(CstType),
    TypeLiteral(CstType),
    ArrayLit(Vec<CstExpr>),
    DictLit {
        keys: Vec<CstExpr>,
        values: Vec<CstExpr>,
    },
    NumberLit(Box<CstExpr>),
    Block {
        params: Option<Box<CstParam>>,
        param_count: usize,
        return_type: Option<Box<CstType>>,
        body: Option<Box<CstStmt>>,
        /// `async ^{...}` async block literal (async_soft_keywords_plan §5):
        /// return type written between `async` and `^`; evaluation/call
        /// yields `NPTask<T> *`. Plain `^{...}` blocks keep `false`.
        is_async: bool,
    },
    InitList(Vec<CstExpr>),
    /// A single C99 designated initializer entry inside an `InitList`:
    /// `.field = expr`, `[index] = expr`, or a chain like `[2].y = 6`.
    /// The value expression is stored in `expr`; `designators` is the chain
    /// written before `=` (at least one entry).
    DesignatedInit {
        designators: Vec<CstDesignator>,
        expr: Box<CstExpr>,
    },
    Sizeof {
        type_expr: CstType,
        expr: Option<Box<CstExpr>>,
    },
    Alignof(CstType),
    Typeof(CstType),
    Paren(Box<CstExpr>),
    /// `await <expr>` — the awaited value (a message send / call that may
    /// suspend this method).
    Await {
        inner: Box<CstExpr>,
    },
    /// `@(expr)` — the boxed expression, before type-directed desugaring.
    Boxed(Box<CstExpr>),
}

// Statement node
#[derive(Debug, Clone)]
pub struct CstStmt {
    pub kind: CstStmtKind,
    pub line: usize,
    pub column: usize,
    /// Exclusive end of the node's extent (see `CstExpr::end_line`); `0` = not
    /// stamped.
    pub end_line: usize,
    pub end_column: usize,
    pub data: CstStmtData,
}

/// One pattern arm of a `SwitchPat` (`case <pattern> when <guard>: body`).
#[derive(Debug, Clone)]
pub struct CstArm {
    pub pattern: CstPattern,
    /// `when` guard expression, applied after the pattern matches (and after
    /// any type binding is in scope). `None` = unconditional.
    pub guard: Option<Box<CstExpr>>,
    pub body: Box<CstStmt>,
    pub line: usize,
    pub column: usize,
    /// Exclusive end of the arm's extent (see `CstStmt::end_line`).
    pub end_line: usize,
    pub end_column: usize,
}

/// Case pattern kinds (M1). `Const` covers plain constants AND ObjC object
/// literals (`@1`, `@"x"`, `@YES`) — value-vs-identity equality is decided at
/// lowering (M1-f), not in the grammar.
#[derive(Debug, Clone)]
pub enum CstPattern {
    /// Compile-time constant (or ObjC object literal) — `case 1:` / `case @1:`
    Const(Box<CstExpr>),
    /// Dangling comparison against the switch subject — `case > 10:`,
    /// `case > 0 && < 100:`. Parser pre-binds nothing; the pattern crate
    /// splices the subject into the dangling operand slots.
    Cond(Box<CstExpr>),
    /// Type test + binding — `case NSString *s:` (declaration-shaped).
    Bind { ty: Box<CstType>, name: String },
}

#[derive(Debug, Clone)]
pub enum CstStmtData {
    Expr(CstExpr),
    Compound(Vec<CstStmt>),
    If {
        cond: Box<CstExpr>,
        then_branch: Box<CstStmt>,
        else_branch: Option<Box<CstStmt>>,
    },
    Switch {
        expr: Box<CstExpr>,
        body: Box<CstStmt>,
    },
    /// `switch` whose body contains at least one pattern arm (condition,
    /// type binding, or ObjC literal) — lowered to goto/if dispatch by the
    /// pattern crate (Step 3.95). Plain-constant switches stay on `Switch`.
    SwitchPat {
        expr: Box<CstExpr>,
        /// Flat arm list; the brace structure of the C switch body is not
        /// preserved (arms are labels, never nested scopes in jeti).
        arms: Vec<CstArm>,
        has_default: bool,
        /// Body of the `default:` arm, grouped with its fallthrough siblings
        /// by the flat-arm collector. The pattern crate emits it as a
        /// `__jeti_case_d` labeled block; `None` = no default (or the default
        /// body was not captured, e.g. the single-arm wrapper nodes).
        default_body: Option<Box<CstStmt>>,
    },
    Case {
        value: Box<CstExpr>,
        body: Box<CstStmt>,
    },
    Default(Box<CstStmt>),
    While {
        cond: Box<CstExpr>,
        body: Box<CstStmt>,
    },
    Do {
        body: Box<CstStmt>,
        cond: Box<CstExpr>,
    },
    For {
        init: Option<Box<CstStmt>>,
        cond: Option<Box<CstExpr>>,
        incr: Option<Box<CstExpr>>,
        body: Box<CstStmt>,
    },
    ForIn {
        var: Box<CstExpr>,
        collection: Box<CstExpr>,
        body: Box<CstStmt>,
    },
    Return(Option<Box<CstExpr>>),
    Goto(String),
    Label(String),
    Throw(Option<Box<CstExpr>>),
    Try {
        try_block: Box<CstStmt>,
        catches: Vec<CstStmt>,
        finally_block: Option<Box<CstStmt>>,
    },
    Catch {
        param: CstParam,
        body: Box<CstStmt>,
    },
    Finally(Box<CstStmt>),
    Synchronized {
        lock: Box<CstExpr>,
        body: Box<CstStmt>,
    },
    Autoreleasepool(Box<CstStmt>),
    NoArc(Box<CstStmt>),
    /// `@defer { ... }` — opaque wrapper until the defer pass splices the body
    /// into every exit of the enclosing block (crates/defer, pipeline Step 3.9).
    Defer(Box<CstStmt>),
    Asm {
        is_volatile: bool,
        is_goto: bool,
        template: String,
        outputs: Vec<CstAsmOperand>,
        inputs: Vec<CstAsmOperand>,
        clobbers: Vec<String>,
        labels: Vec<String>,
    },
    Decl(CstDecl),
}

#[derive(Debug, Clone)]
pub struct CstAsmOperand {
    pub name: Option<String>,
    pub constraint: String,
    pub expr: Box<CstExpr>,
}

// Declaration node
#[derive(Debug, Clone)]
pub struct CstDecl {
    pub kind: CstDeclKind,
    pub line: usize,
    pub column: usize,
    /// Exclusive end of the node's extent (see `CstExpr::end_line`); `0` = not
    /// stamped.
    pub end_line: usize,
    pub end_column: usize,
    pub name: Option<String>,
    pub next: Option<Box<CstDecl>>,
    pub data: CstDeclData,
    /// Raw `__attribute__((...))` spellings (e.g. `packed`, `format(printf, 1, 2)`).
    pub attributes: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum CstDeclData {
    Function {
        return_type: Option<Box<CstType>>,
        params: Option<Box<CstParam>>,
        has_variadic: bool,
        body: Option<Box<CstStmt>>,
        /// Trailing `throws` / `throws(T)` annotation (declaration position).
        /// `None` = not annotated. Compile-time only — never emitted to C.
        throws: Option<Box<CstType>>,
        /// `async` return-type modifier (doc/async_nptask_plan.md). Always
        /// false for plain C functions — the modifier is method syntax;
        /// kept on the node for uniformity.
        async_marker: bool,
    },
    Variable {
        var_type: Option<Box<CstType>>,
        initializer: Option<Box<CstExpr>>,
        is_static: bool,
        is_extern: bool,
        is_const: bool,
        is_block_qual: bool,
        is_weak: bool,
    },
    Typedef {
        alias_type: Option<Box<CstType>>,
        struct_fields: Vec<CstDecl>,
    },
    Aggregate {
        fields: Vec<CstDecl>,
        is_union: bool,
    },
    Enum {
        members: Vec<String>,
        values: Vec<CstExpr>,
    },
    Class {
        superclass: Option<String>,
        category_name: Option<String>,
        protocols: Vec<String>,
        type_params: Vec<String>,
        /// Generic bounds: (type-param name, bound name) in declaration order
        /// (`@interface Box<T : Greetable>` → `[("T", "Greetable")]`). The
        /// ObjC `id<...>` shell is stripped by the parser; a bound name is a
        /// protocol or a class (resolved by the checker via the symbol table).
        type_bounds: Vec<(String, String)>,
        ivars: Vec<CstDecl>,
        properties: Vec<CstDecl>,
        methods: Vec<CstDecl>,
        impl_vars: Vec<CstDecl>,
    },
    ProtocolData {
        protocols: Vec<String>,
        methods: Vec<CstDecl>,
        is_optional: bool,
    },
    Forward(Vec<String>),
    Property {
        prop_type: Option<Box<CstType>>,
        getter: Option<String>,
        setter: Option<String>,
        is_readonly: bool,
        is_weak: bool,
        is_assign: bool,
        is_retain: bool,
        is_copy: bool,
        is_nonatomic: bool,
        is_dynamic: bool,
    },
    Ivar {
        ivar_type: Option<Box<CstType>>,
        iboutlet: bool,
        is_weak: bool,
    },
    Method {
        is_class_method: bool,
        return_type: Option<Box<CstType>>,
        params: Option<Box<CstParam>>,
        has_variadic: bool,
        body: Option<Box<CstStmt>>,
        /// Trailing `throws` / `throws(T)` annotation (declaration position,
        /// before `;` or `{`). `None` = not annotated. Compile-time only —
        /// never emitted to C. Distinct from the `@throw` statement.
        throws: Option<Box<CstType>>,
        /// `async` return-type modifier (see Function / doc/async_nptask_plan.md).
        async_marker: bool,
    },
    Namespace(Vec<CstDecl>),
    Using {
        fqn: String,
        alias: Option<String>,
    },
    Asm {
        is_volatile: bool,
        is_goto: bool,
        template: String,
        outputs: Vec<CstAsmOperand>,
        inputs: Vec<CstAsmOperand>,
        clobbers: Vec<String>,
        labels: Vec<String>,
    },
    /// A raw C line passed through verbatim (e.g. `#pragma mark - Foo`).
    RawLine(String),
}

// Translation unit
#[derive(Debug, Clone)]
pub struct TranslationUnit {
    pub decls: Vec<CstDecl>,
    pub filename: String,
}

#[derive(Debug, Clone)]
pub struct CstParamList {
    pub params: Vec<CstParam>,
}