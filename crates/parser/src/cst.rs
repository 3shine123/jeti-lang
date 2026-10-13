use std::fmt;

// Type primitives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypePrim {
    Void, Char, Short, Int, Long, LongLong,
    Float, Double, Bool, Signed, Unsigned,
    Id, Class, Sel, Instancetype,
    Named, Param,
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
    pub is_block_qual: bool,
    pub is_weak_qual: bool,
    pub is_unsigned: bool,
    pub array_size: i32,
    pub subtype: Option<Box<CstType>>,
    pub name: Option<String>,
    pub block_name: Option<String>,
    pub block_params: Option<Box<CstType>>,
    pub next: Option<Box<CstType>>,
    pub protocols: Vec<String>,
    pub type_args: Vec<CstType>,
}

impl CstType {
    pub fn new(prim: TypePrim) -> Self {
        CstType {
            prim,
            is_pointer: false, is_const: false, is_volatile: false,
            is_block: false, is_array: false, is_struct: false,
            is_block_qual: false, is_weak_qual: false, is_unsigned: false,
            array_size: 0,
            subtype: None, name: None, block_name: None,
            block_params: None, next: None,
            protocols: Vec::new(), type_args: Vec::new(),
        }
    }
}

// Expression kinds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CstExprKind {
    Ident, Integer, Float, String, Char, Bool,
    Nil, Null, Self_, Super, Cmd,
    Selector, Encode, Protocol,
    ArrayLit, DictLit, NumberLit,
    Block, InitList,
    Unary, Binary, Ternary, Assign,
    Conditional, Cast, Sizeof, Typeof,
    MessageSend, DotAccess, Subscript,
    Call, Comma, Paren,
}

// Statement kinds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CstStmtKind {
    Expr, Compound, If, Switch, Case, Default,
    While, Do, For, ForIn,
    Break, Continue, Return, Goto, Label,
    Try, Catch, Finally, Throw,
    Synchronized, Autoreleasepool, Decl,
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
    Namespace, Using,
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
    pub data: CstExprData,
}

#[derive(Debug, Clone)]
pub enum CstExprData {
    Ident(String),
    Integer(i64),
    Float(f64),
    String(String),
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
    },
    InitList(Vec<CstExpr>),
    Sizeof {
        type_expr: CstType,
        expr: Option<Box<CstExpr>>,
    },
    Typeof(CstType),
    Paren(Box<CstExpr>),
}

// Statement node
#[derive(Debug, Clone)]
pub struct CstStmt {
    pub kind: CstStmtKind,
    pub line: usize,
    pub column: usize,
    pub data: CstStmtData,
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
    Decl(CstDecl),
}

// Declaration node
#[derive(Debug, Clone)]
pub struct CstDecl {
    pub kind: CstDeclKind,
    pub line: usize,
    pub column: usize,
    pub name: Option<String>,
    pub next: Option<Box<CstDecl>>,
    pub data: CstDeclData,
}

#[derive(Debug, Clone)]
pub enum CstDeclData {
    Function {
        return_type: Option<Box<CstType>>,
        params: Option<Box<CstParam>>,
        has_variadic: bool,
        body: Option<Box<CstStmt>>,
        /// Trailing `throws` / `throws(T)` annotation (declaration position).
        /// `None` = not annotated; `Some(T)` = `throws(T)`; `Some(void)` = bare
        /// `throws` ("declared to throw, type unstated"). Compile-time only —
        /// never emitted to C.
        throws: Option<Box<CstType>>,
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
    },
    Method {
        is_class_method: bool,
        return_type: Option<Box<CstType>>,
        params: Option<Box<CstParam>>,
        has_variadic: bool,
        body: Option<Box<CstStmt>>,
        /// Trailing `throws` / `throws(T)` annotation (declaration position,
        /// before `;` or `{`). `None` = not annotated; `Some(T)` = `throws(T)`;
        /// `Some(void)` = bare `throws` ("declared to throw, type unstated").
        /// Compile-time only — never emitted to C. Distinct from the `@throw`
        /// statement.
        throws: Option<Box<CstType>>,
    },
    Namespace(Vec<CstDecl>),
    Using {
        fqn: String,
        alias: Option<String>,
    },
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