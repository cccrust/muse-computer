//! Aero AST —— 與規格書 EBNF 一一對應。

#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub gc: bool,
    pub kind: TypeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Int,
    Float,
    Str,
    Bool,
    Unit,
    RawPointer,
    Array(Box<Type>),
    Dict(Box<Type>, Box<Type>),
    Future(Box<Type>),
    Option(Box<Type>),
    Ref { mutable: bool, inner: Box<Type> },
    Closure { is_async: bool, params: Vec<Type>, ret: Box<Type> },
    Custom(String),
    /// `Pair[Int, String]` 這類自訂泛型具現（base 尚未正規化，codegen 時再 rt+泛型處理）
    Generic { base: String, args: Vec<Type> },
}

impl Type {
    pub fn plain(kind: TypeKind) -> Self {
        Type { gc: false, kind }
    }
    /// 剝掉所有 `&` / `&mut`
    pub fn strip_refs(&self) -> &Type {
        match &self.kind {
            TypeKind::Ref { inner, .. } => inner.strip_refs(),
            _ => self,
        }
    }
    pub fn is_ref(&self) -> bool {
        matches!(self.kind, TypeKind::Ref { .. })
    }
}

/// 泛型參數 `T: Add + Clone` —— Curry-Howard 視角的「邏輯約束命題」。
/// `bounds` 內每個字串都是一個 trait 名（尚未正規化）。
#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub off: usize,
    pub name: String,
    pub self_kind: Option<SelfKind>,
    pub params: Vec<Param>,
    pub ret: Type,
}

/// `trait Comparable ... end` —— 型態滿足特定命題的證明規格（predicate / witness spec）。
#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: String,
    /// 為 true 時代表編譯器可自動衍生（auto-trait / derived trait），無需手動 impl
    pub is_auto: bool,
    pub methods: Vec<TraitMethod>,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<TopLevel>,
}

#[derive(Debug, Clone)]
pub struct TopLevel {
    pub off: usize,
    pub attr: Option<Vec<String>>,
    pub decl: Decl,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Import(ImportCrate),
    /// `import "file.aero" as alias;`
    ImportMod { path: String, alias: Option<String> },
    Enum(EnumDecl),
    Extern(ExternBlock),
    Struct(StructDecl),
    Function(FunctionDecl),
    Impl(ImplDecl),
    /// `trait Name ... end`
    Trait(TraitDecl),
    /// `impl Trait for Type ... end`（手動逃生艙；一般情況由編譯器自動橋接，無需手寫）
    ImplTrait(ImplTraitDecl),
}

#[derive(Debug, Clone)]
pub struct ImplTraitDecl {
    pub trait_name: String,
    pub type_name: String,
    pub methods: Vec<FunctionDecl>,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub gc: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantDecl>,
}

#[derive(Debug, Clone)]
pub struct VariantDecl {
    pub name: String,
    pub fields: Vec<Type>,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wild,
    Bind(String),
    Int(i64),
    Str(String),
    Bool(bool),
    Variant { path: Vec<String>, args: Vec<Pattern> },
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone)]
pub struct Arm {
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct ImplDecl {
    pub name: String,
    pub methods: Vec<FunctionDecl>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelfKind {
    Value { mutable: bool },
    Ref,
    RefMut,
}

#[derive(Debug, Clone)]
pub struct ImportCrate {
    pub name: String,
    pub version: Option<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExternBlock {
    pub abi: String,
    pub items: Vec<ExternFn>,
}

#[derive(Debug, Clone)]
pub struct ExternFn {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub gc: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub mutable: bool,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub off: usize,
    pub is_async: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub self_kind: Option<SelfKind>,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub mutable: bool,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { mutable: bool, name: String, ty: Option<Type>, value: Expr },
    Assign { target: Expr, value: Expr },
    Managed(Vec<Stmt>),
    Unsafe(Vec<Stmt>),
    For { var: String, iter: Expr, body: Vec<Stmt> },
    While { cond: Expr, body: Vec<Stmt> },
    Return(Option<Expr>),
    Expr(Expr),
    Match { scrut: Expr, arms: Vec<Arm> },
    If { branches: Vec<(Expr, Vec<Stmt>)>, else_body: Option<Vec<Stmt>> },
    /// 帶原始碼位置（字元 offset）的陳述式，供錯誤回溯用
    At(usize, Box<Stmt>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Rem,
    Lt, Le, Gt, Ge,
    Eq, Ne,
    And, Or,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Path(Vec<String>),
    Move(String),
    Borrow { mutable: bool, name: String },
    StructInit { name: String, fields: Vec<(String, Expr)> },
    GcAlloc { name: String, fields: Vec<(String, Expr)> },
    Array(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Lambda { is_async: bool, params: Vec<Param>, ret: Type, body: Vec<Stmt> },
    Unary { op: UnOp, expr: Box<Expr> },
    Index(Box<Expr>, Box<Expr>),
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Pipe { lhs: Box<Expr>, name: String, args: Option<Vec<Expr>> },
    Field(Box<Expr>, String),
    Call(Box<Expr>, Vec<Expr>),
    Await(Box<Expr>),
}
