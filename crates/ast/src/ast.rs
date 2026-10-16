use jeti_cst::{CstParam, CstType, Nullability, TagKind, TypePrim};

// ─── Type node ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AstType {
    pub prim: TypePrim,
    pub is_pointer: bool,
    pub is_const: bool,
    pub is_block: bool,
    pub is_fn_ptr: bool,
    pub is_array: bool,
    pub is_struct: bool,
    /// Which C keyword introduced this tag (`struct` / `union` / `enum`).
    /// Codegen must echo it verbatim — `enum Mode` rendered as `struct Mode`
    /// is an incompatible-type error in C.
    pub tag: TagKind,
    pub is_unsigned: bool,
    /// C99 `_Complex` (§6.2.5p13) — codegen echoes the keyword after the base
    /// type (`float _Complex`).
    pub is_complex: bool,
    pub array_size: i32,
    /// Symbolic array size identifier when the source used a macro/enum
    /// constant (e.g. `FSNode *_children[MAX_CHILDREN];`). When Some, codegen
    /// emits `T[MAX_CHILDREN]` rather than `T[]` (flexible array member),
    /// which C forbids outside the trailing field.
    pub array_size_name: Option<String>,
    pub subtype: Option<Box<AstType>>,
    pub block_params: Option<Box<AstType>>,
    pub block_name: Option<String>,
    pub next: Option<Box<AstType>>,
    pub type_args: Vec<AstType>,
    pub name: Option<String>,
    pub class_ref: Option<String>,
    pub protocol_ref: Option<String>,
    pub protocol_refs: Vec<String>,
    /// Nullability annotation, carried through from `CstType` for the checker.
    /// Codegen never reads it (zero runtime cost, same contract as ObjC).
    pub nulls: Nullability,
}

impl AstType {
    pub fn new(prim: TypePrim) -> Self {
        AstType {
            prim, is_pointer: false, is_const: false, is_block: false,
            is_fn_ptr: false, is_array: false, is_struct: false, tag: TagKind::None, is_unsigned: false, is_complex: false, array_size: 0,
            subtype: None, block_params: None, block_name: None, next: None,
            type_args: Vec::new(), name: None,
            class_ref: None, protocol_ref: None, protocol_refs: Vec::new(),
            array_size_name: None,
            nulls: Nullability::Unspecified,
        }
    }

    /// Convert a CST type into an AST type, resolving the class reference for
    /// pointer-to-named-class types (`Foo *`).
    pub fn from_cst_type(ct: &CstType) -> AstType {
        let mut t = AstType::new(ct.prim);
        t.is_pointer = ct.is_pointer;
        t.is_const = ct.is_const;
        t.is_unsigned = ct.is_unsigned;
        t.is_complex = ct.is_complex;
        t.is_struct = ct.is_struct;
        t.tag = ct.tag;
        t.is_block = ct.is_block;
        t.is_fn_ptr = ct.is_fn_ptr;
        t.nulls = ct.nulls;
        t.block_name = ct.block_name.clone();
        t.block_params = ct.block_params.as_ref().map(|bp| Box::new(AstType::from_cst_type(bp)));
        if let Some(ref name) = ct.name {
            t.name = Some(name.clone());
            t.class_ref = Some(name.clone());
        }
        if ct.is_pointer {
            if let Some(ref sub) = ct.subtype {
                if let Some(ref sname) = sub.name {
                    t.name = Some(sname.clone());
                    t.class_ref = Some(sname.clone());
                }
                t.prim = sub.prim;
            }
        }
        t
    }

    /// Rebuild a CST type from an AST type — the inverse of `from_cst_type`
    /// on the surface that round-trips through it (primitives, pointers,
    /// named/struct types, generics, arrays). Used by lowering passes that
    /// must hand AST-typed parameters back to CST-shaped consumers (e.g. the
    /// async block-literal thunk builder re-creates a `CstParam` chain from
    /// the elaborator-normalized `Vec<(AstType, String)>`).
    pub fn to_cst_type(&self) -> CstType {
        let mut ct = CstType::new(self.prim);
        ct.is_pointer = self.is_pointer;
        ct.is_const = self.is_const;
        ct.is_block = self.is_block;
        ct.is_array = self.is_array;
        ct.is_struct = self.is_struct;
        ct.is_fn_ptr = self.is_fn_ptr;
        ct.is_complex = self.is_complex;
        ct.is_unsigned = self.is_unsigned;
        ct.tag = self.tag;
        ct.array_size = self.array_size;
        ct.array_size_name = self.array_size_name.clone();
        ct.block_name = self.block_name.clone();
        ct.block_params = self.block_params.as_ref().map(|bp| Box::new(bp.to_cst_type()));
        ct.protocols = self.protocol_refs.clone();
        ct.type_args = self.type_args.iter().map(|t| t.to_cst_type()).collect();
        ct.nulls = self.nulls;
        // from_cst_type pulls a pointer's target name up into the outer type;
        // mirror that by pushing the name back down into the subtype so the
        // round-trip is stable.
        if self.is_pointer {
            if let Some(ref name) = self.name {
                let mut sub = CstType::new(self.prim);
                sub.name = Some(name.clone());
                sub.is_struct = self.is_struct;
                sub.type_args = ct.type_args.clone();
                ct.subtype = Some(Box::new(sub));
            }
        }
        ct.name = self.name.clone();
        ct
    }

    /// True if this type is a pointer-to-object, `id`, `instancetype`, or `Class`.
    pub fn is_object(&self) -> bool {
        self.is_pointer
            || self.prim == TypePrim::Id
            || self.prim == TypePrim::Instancetype
            || self.prim == TypePrim::Class
    }
}

// ─── Expression kinds ────────────────────────────────────────────────────────

/// One link of a C99 designated-initializer designator chain:
/// `.field` or `[index]`. A full designator is a chain, e.g. `[2].y`.
#[derive(Debug, Clone)]
pub enum AstDesignator {
    Member(String),
    Index(Box<AstExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstExprKind {
    Int, Float, Char, String, AtString, Bool,
    Nil, Null, Self_, Super, Selector,
    VarRef, IvarRef, PropRef,
    MsgSend, FuncCall,
    Unary, Binary, Assign, Cast,
    BlockLit, ArrayLit, InitList, DictLit, DesignatedInit,
    Subscript, Comma, Paren, Sizeof, Alignof, Ternary, TypeLiteral,
    /// `await <expr>` — suspension point (async methods only). The async
    /// desugar pass splits the enclosing body at these nodes.
    Await,
    /// `@(expr)` — boxed-expression literal. The checker rewrites it into the
    /// `NPNumber` factory matching the expression's static type; a codegen
    /// fallback arm keeps `-fno-checker` from emitting bad C.
    Boxed,
}

#[derive(Debug, Clone)]
pub struct AstExpr {
    pub kind: AstExprKind,
    pub expr_type: Option<Box<AstType>>,
    pub line: usize,
    pub col: usize,
    /// Exclusive end of the node's extent, same convention as `line` / `col`
    /// (flattened-buffer line, source column). `0` = no source origin — a node
    /// the compiler synthesized. Resolve the pair with
    /// `SourceMap::span_from_node`; this is the shared position model, not a
    /// second one (`docs/plans/source_locations_debug_lsp_plan.md`).
    pub end_line: usize,
    pub end_col: usize,
    pub data: AstExprData,
}

#[derive(Debug, Clone)]
pub enum AstExprData {
    Int(i64),
    Float(f64),
    /// Imaginary-suffixed float literal (`2.0i`) preserved verbatim.
    FloatRaw(String),
    Char(u8),
    String(String),
    AtString(String),
    Bool(bool),
    VarRef { sym: Option<String>, name: String },
    IvarRef { ivar: Option<String>, cls: Option<String>, obj: Box<AstExpr> },
    PropRef { prop: Option<String>, cls: Option<String>, obj: Box<AstExpr>, name: String, is_arrow: bool },
    MsgSend {
        receiver: Box<AstExpr>,
        method: Option<String>,
        vtable_index: i32,
        is_class_method: bool,
        is_super: bool,
        super_name: Option<String>,
        selector: String,
        args: Vec<AstExpr>,
    },
    FuncCall {
        func: Option<String>,
        name: String,
        callee: Option<Box<AstExpr>>,
        args: Vec<AstExpr>,
    },
    Unary { op: i32, operand: Box<AstExpr>, is_postfix: bool },
    Binary { op: i32, left: Box<AstExpr>, right: Box<AstExpr> },
    Assign { target: Box<AstExpr>, value: Box<AstExpr> },
    Cast { target_type: AstType, expr: Box<AstExpr> },
    ArrayLit(Vec<AstExpr>),
    InitList(Vec<AstExpr>),
    /// A single C99 designated-initializer entry inside an `InitList`:
    /// `.field = expr` / `[index] = expr` / a chain like `[2].y = 6`.
    DesignatedInit {
        designators: Vec<AstDesignator>,
        expr: Box<AstExpr>,
    },
    DictLit { keys: Vec<AstExpr>, values: Vec<AstExpr> },
    Comma(Vec<AstExpr>),
    /// `(expr)` — a grouping the user wrote explicitly. The elaborator used to
    /// unwrap these transparently, which dropped the grouping: `(a = b) != c`
    /// was emitted as `a = b != c` (assignment swallowed the comparison —
    /// `while ((v = va_arg(ap, int)) != 0)` miscompiled). Keeping the node lets
    /// codegen re-emit the parentheses.
    Paren(Box<AstExpr>),
    Subscript { object: Box<AstExpr>, key: Box<AstExpr> },
    Sizeof { type_expr: AstType, expr: Option<Box<AstExpr>> },
    Alignof(AstType),
    /// Async block literal `async T? ^…` (async_soft_keywords_plan §5):
    /// carries its own `is_async` so a plain block never inherits async
    /// context (§5.3-1: `await` inside a plain block is an error).
    Block { params: Vec<(AstType, String)>, return_type: Option<Box<AstType>>, body: Option<Box<AstStmt>>, is_async: bool },
    Ternary { cond: Box<AstExpr>, then: Box<AstExpr>, else_: Box<AstExpr> },
    Selector(String),
    TypeLiteral(AstType),
    /// `await <expr>` — the awaited expression (message send / call).
    Await {
        inner: Box<AstExpr>,
    },
    /// `@(expr)` — the boxed expression, before the checker rewrites it into
    /// the `NPNumber` factory matching its static type.
    Boxed(Box<AstExpr>),
}

// ─── Statement kinds ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstStmtKind {
    Expr, Compound, If, Switch, Case, Default,
    While, Do, For, ForIn,
    Break, Continue, Return, Goto, Label,
    Throw, Try, Catch, Finally,
    Synchronized, Autoreleasepool, NoArc, Decl, Asm, Defer,
    /// `switch` containing at least one pattern arm — lowered to goto/if
    /// dispatch by the pattern crate (Step 3.95), before ARC sees it.
    SwitchPat,
}

#[derive(Debug, Clone)]
pub struct AstStmt {
    pub kind: AstStmtKind,
    pub line: usize,
    pub col: usize,
    /// Exclusive end of the node's extent (see [`AstExpr::end_line`]).
    pub end_line: usize,
    pub end_col: usize,
    pub data: AstStmtData,
}

/// One pattern arm of a `SwitchPat` (`case <pattern> when <guard>: body`).
#[derive(Debug, Clone)]
pub struct AstArm {
    pub pattern: AstPattern,
    /// `when` guard, checked after the pattern matches (and after any type
    /// binding is in scope). `None` = unconditional.
    pub guard: Option<Box<AstExpr>>,
    pub body: Box<AstStmt>,
    pub line: usize,
    pub col: usize,
    /// Exclusive end of the node's extent (see [`AstExpr::end_line`]).
    pub end_line: usize,
    pub end_col: usize,
}

/// Case pattern kinds (M1). `Const` covers plain constants AND ObjC object
/// literals (`@1`, `@"x"`, `@YES`) — value-vs-identity equality is decided at
/// lowering (pattern crate), not in the grammar.
#[derive(Debug, Clone)]
pub enum AstPattern {
    /// Compile-time constant or ObjC object literal.
    Const(Box<AstExpr>),
    /// Dangling comparison against the switch subject (`case > 10:`,
    /// `case > 0 && < 100:`). Binary nodes carry an empty-Ident left operand
    /// as the subject placeholder; spliced at lowering.
    Cond(Box<AstExpr>),
    /// Type test + binding — `case NSString *s:`.
    Bind { ty: Box<AstType>, name: String },
}

#[derive(Debug, Clone)]
pub enum AstStmtData {
    Expr(AstExpr),
    Compound(Vec<AstStmt>),
    If { cond: Box<AstExpr>, then: Box<AstStmt>, else_: Option<Box<AstStmt>> },
    Switch { expr: Box<AstExpr>, body: Box<AstStmt> },
    /// Pattern-dispatch switch (see AstStmtKind::SwitchPat). Flat arm list;
    /// lowered by the pattern crate before ARC.
    SwitchPat { expr: Box<AstExpr>, arms: Vec<AstArm>, has_default: bool, default_body: Option<Box<AstStmt>> },
    Case { value: Box<AstExpr>, body: Box<AstStmt> },
    Default(Box<AstStmt>),
    While { cond: Box<AstExpr>, body: Box<AstStmt> },
    Do { body: Box<AstStmt>, cond: Box<AstExpr> },
    For { init: Option<Box<AstStmt>>, cond: Option<Box<AstExpr>>, incr: Option<Box<AstExpr>>, body: Box<AstStmt> },
    ForIn { var: Box<AstExpr>, collection: Box<AstExpr>, body: Box<AstStmt> },
    Return(Option<Box<AstExpr>>),
    Goto(String),
    Label(String),
    Throw(Option<Box<AstExpr>>),
    Try { try_block: Box<AstStmt>, catches: Vec<AstStmt>, finally_block: Option<Box<AstStmt>> },
    Catch { param: CstParam, body: Box<AstStmt> },
    Finally(Box<AstStmt>),
    Synchronized { lock: Box<AstExpr>, body: Box<AstStmt> },
    Autoreleasepool(Box<AstStmt>),
    NoArc(Box<AstStmt>),
    /// `@defer { ... }` — consumed by the defer pass (pipeline Step 3.9),
    /// which splices the body into every exit of the enclosing block.
    Defer(Box<AstStmt>),
    Asm {
        is_volatile: bool,
        is_goto: bool,
        template: String,
        outputs: Vec<AstAsmOperand>,
        inputs: Vec<AstAsmOperand>,
        clobbers: Vec<String>,
        labels: Vec<String>,
    },
    Decl(AstDecl),
}

#[derive(Debug, Clone)]
pub struct AstAsmOperand {
    pub name: Option<String>,
    pub constraint: String,
    pub expr: AstExpr,
}

// ─── Declaration kinds ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstDeclKind {
    Class, Method, Ivar, Property,
    Function, Variable, Protocol,
    Typedef, Struct, Union, Enum, Namespace, Asm, RawLine,
    ForwardClass,
}

#[derive(Debug, Clone)]
pub struct AstDecl {
    pub kind: AstDeclKind,
    pub name: Option<String>,
    pub line: usize,
    pub col: usize,
    /// Exclusive end of the node's extent (see [`AstExpr::end_line`]).
    pub end_line: usize,
    pub end_col: usize,
    pub data: AstDeclData,
    /// Raw `__attribute__((...))` spellings carried from CST.
    pub attributes: Vec<String>,
}

#[derive(Debug, Clone)]
 pub enum AstDeclData {
     Class {
         cls_sym: Option<String>,
         super_name: Option<String>,
         /// Protocol names this class conforms to (`@interface X <P> ...`),
         /// resolved to FQN by the elaborator. Used by the checker's protocol
         /// conformance check (required methods must be implemented).
         protocols: Vec<String>,
         /// Declared generic type parameters in order (`@interface X<K, V>`
         /// → ["K", "V"]). Codegen uses them to pair each instantiation's
         /// type_args with the right Param sentinel (`/*K*/`, `/*V*/`) during
         /// monomorphization; the checker reads them from the symbol table.
         type_params: Vec<String>,
         /// Generic bounds: (type-param name, bound name) pairs from
         /// `@interface Box<T : Greetable>`. Consumed by the checker's
         /// instantiation-point conformance check; codegen ignores them —
         /// bounds produce no C (zero codegen, checker-only feature).
         type_bounds: Vec<(String, String)>,
         methods: Vec<AstDecl>,
         ivars: Vec<AstDecl>,
         properties: Vec<AstDecl>,
         impl_vars: Vec<AstDecl>,
         is_implementation: bool,
         /// True for a category (`@implementation Dog (Tricks)`). A category
         /// implementation must NOT claim class ownership (R2): its TU emits
         /// only the method definitions, never the strong class metadata —
         /// otherwise linking owner + category TUs fails with duplicate
         /// symbols (probes/xcat P0). See doc/categories_protocol_plan.md D1.
         is_category: bool,
     },
     Method {
         method_sym: Option<String>,
         is_class_method: bool,
         return_type: Option<Box<AstType>>,
         params: Option<Box<CstParam>>,
         has_variadic: bool,
         body: Option<Box<AstStmt>>,
         /// Trailing `throws` / `throws(T)` annotation from the declaration.
         /// `None` = not annotated; `Some(T)` = `throws(T)`; `Some(void)` = bare
         /// `throws` ("declared to throw, type unstated"). Compile-time only
         /// (checker reconciles it against `@throw` stmts) — never emitted to C.
         throws: Option<Box<AstType>>,
         /// `async` return-type modifier (doc/async_nptask_plan.md): set when
         /// the declaration is spelled `async NPTask<T>`.
         async_marker: bool,
     },
 Ivar {
          ivar_sym: Option<String>,
          ivar_type: Option<Box<AstType>>,
          is_weak: bool,
      },
     Property {
         prop_sym: Option<String>,
         prop_type: Option<Box<AstType>>,
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
    Function {
        func_sym: Option<String>,
        return_type: Option<Box<AstType>>,
        params: Option<Box<CstParam>>,
        body: Option<Box<AstStmt>>,
        has_variadic: bool,
        /// Trailing `throws` / `throws(T)` annotation from the declaration.
        /// `None` = not annotated; `Some(T)` = `throws(T)`; `Some(void)` = bare
        /// `throws` ("declared to throw, type unstated"). Compile-time only —
        /// never emitted to C.
        throws: Option<Box<AstType>>,
        /// `async` return-type modifier: the signature keeps the real
        /// `NPTask<T>` type and this flag marks it suspending. Compile-time
        /// metadata only — the emitted C signature is just `T` (the async M2
        /// driver already returns `T`).
        async_marker: bool,
    },
    Variable {
        var_type: Option<Box<AstType>>,
        init: Option<Box<AstExpr>>,
        is_static: bool,
        is_extern: bool,
        is_const: bool,
        is_block_qual: bool,
        is_weak: bool,
        next: Option<Box<AstDecl>>,
    },
    Typedef {
        aliased_type: Option<Box<AstType>>,
        struct_fields: Vec<AstDecl>,
    },
    Aggregate {
        fields: Vec<AstDecl>,
        /// `union U { ... }` vs `struct U { ... }`. The CST carries this,
        /// but the AST used to drop it, so codegen emitted every aggregate as
        /// `struct` — and a `union U2 u;` *use* then mismatched its own
        /// definition. Must survive to codegen.
        is_union: bool,
    },
    Enum {
        members: Vec<String>,
        values: Vec<AstExpr>,
    },
    Namespace(Vec<AstDecl>),
    ForwardClass {
        names: Vec<String>,
    },
    /// A `@protocol P <Parents>` declaration. Carries what the codegen needs
    /// to emit the static `NPProtocol` metadata (D5.2,
    /// doc/categories_protocol_plan.md): parent names, required/optional
    /// selector names in declaration order. Compile-time only — methods keep
    /// no param types here (the runtime query compares selector names).
    Protocol {
        parents: Vec<String>,
        required_methods: Vec<String>,
        optional_methods: Vec<String>,
    },
    Asm {
        is_volatile: bool,
        is_goto: bool,
        template: String,
        outputs: Vec<AstAsmOperand>,
        inputs: Vec<AstAsmOperand>,
        clobbers: Vec<String>,
        labels: Vec<String>,
    },
    /// A raw C line passed through verbatim (e.g. `#pragma mark - Foo`).
    RawLine(String),
}

// ─── Translation unit ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AstUnit {
    pub decls: Vec<AstDecl>,
    pub filename: String,
}