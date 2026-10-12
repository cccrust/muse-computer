//! Aero AST → Rust（以 `quote!` 組裝 TokenStream，再由 syn + prettyplease 驗證/排版）。
//!
//! 設計重點
//! * **Zero-GC 模式**：1:1 對應 Rust 的所有權/借用，Borrow Checker 完全外包給 rustc。
//! * **Managed-GC 模式**：`gc T` → `gc::Gc<gc::GcCell<T>>`（現成的 `gc` crate，
//!   具備 cycle-collecting tracing GC），並依「輕量型別推導」自動插入
//!   `.borrow()` / `.borrow_mut()` / `.clone()`（Auto-Deref、共享、clone_to_owned）。

use crate::ast::*;
use crate::loader::Module;
use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::{format_ident, quote};
use std::collections::{BTreeMap, HashMap, HashSet};

enum Callee {
    Fn(String),
    Method(String, String),
    Variant(String, String),
    Unknown,
}

struct ModInfo {
    prefix: String,
    aliases: HashMap<String, usize>,
    chars: Vec<char>,
}

#[derive(Clone)]
struct FnSig {
    params: Vec<Param>,
    ret: Type,
    is_async: bool,
    self_kind: Option<SelfKind>,
    generics: Vec<GenericParam>,
}

/// Aero 內建自動特徵 → Rust trait 邊界的對應表。
/// 回傳 None 代表不是內建特徵（應為使用者自訂 trait，原樣保留）。
/// 注意：`Add/Sub/...` 不在這裡——它們的 `Output = Self` 只在 trait 定義內合法，
/// 自由函數/結構的邊界必須寫成 `Output = T`，由 `generics_decl` 專門處理。
fn builtin_bounds(aero: &str) -> Option<Vec<TokenStream>> {
    let b: Vec<TokenStream> = match aero {
        "Clone" => vec![quote!(::std::clone::Clone)],
        "Equatable" => vec![quote!(::std::cmp::PartialEq)],
        "Comparable" => vec![quote!(::std::cmp::PartialOrd), quote!(::std::cmp::PartialEq)],
        "Hashable" => vec![quote!(::std::hash::Hash), quote!(::std::cmp::Eq)],
        "Serializable" => {
            vec![quote!(::serde::Serialize), quote!(::serde::de::DeserializeOwned)]
        }
        "Display" => vec![quote!(::std::fmt::Display)],
        "Debug" => vec![quote!(::std::fmt::Debug)],
        "Default" => vec![quote!(::std::default::Default)],
        "Iterator" => vec![quote!(::std::iter::Iterator)],
        _ => return None,
    };
    Some(b)
}

/// `T: Add` 在泛型參數處的 Rust 邊界（`Output = T`；自由函數中沒有 `Self` 可用）
fn arith_bound(aero: &str, param: &Ident) -> Option<TokenStream> {
    match aero {
        "Add" => Some(quote!(::std::ops::Add<Output = #param>)),
        "Sub" => Some(quote!(::std::ops::Sub<Output = #param>)),
        "Mul" => Some(quote!(::std::ops::Mul<Output = #param>)),
        "Div" => Some(quote!(::std::ops::Div<Output = #param>)),
        "Rem" => Some(quote!(::std::ops::Rem<Output = #param>)),
        _ => None,
    }
}

/// 以實參代入泛型形參（`Pair[T]` 具現後的欄位檢查用）
fn subst_type(t: &Type, sub: &HashMap<String, Type>) -> Type {
    let kind = match &t.kind {
        TypeKind::Custom(n) => {
            if let Some(a) = sub.get(n) {
                return {
                    let mut r = a.clone();
                    r.gc = t.gc || r.gc;
                    r
                };
            }
            TypeKind::Custom(n.clone())
        }
        TypeKind::Generic { base, args } => TypeKind::Generic {
            base: base.clone(),
            args: args.iter().map(|a| subst_type(a, sub)).collect(),
        },
        TypeKind::Array(e) => TypeKind::Array(Box::new(subst_type(e, sub))),
        TypeKind::Dict(k, v) => {
            TypeKind::Dict(Box::new(subst_type(k, sub)), Box::new(subst_type(v, sub)))
        }
        TypeKind::Future(e) => TypeKind::Future(Box::new(subst_type(e, sub))),
        TypeKind::Option(e) => TypeKind::Option(Box::new(subst_type(e, sub))),
        TypeKind::Ref { mutable, inner } => {
            TypeKind::Ref { mutable: *mutable, inner: Box::new(subst_type(inner, sub)) }
        }
        TypeKind::Closure { is_async, params, ret } => TypeKind::Closure {
            is_async: *is_async,
            params: params.iter().map(|p| subst_type(p, sub)).collect(),
            ret: Box::new(subst_type(ret, sub)),
        },
        k => k.clone(),
    };
    Type { gc: t.gc, kind }
}

pub struct Output {
    pub tokens: TokenStream,
    /// crate 名 → (version, features)
    pub deps: BTreeMap<String, (String, Vec<String>)>,
    pub touched_gc: bool,
    /// 需要 derive(Trace) 的型別（全域名稱）
    pub gc_types: HashSet<String>,
}

pub struct CodeGen {
    structs: HashMap<String, StructDecl>,
    enums: HashMap<String, EnumDecl>,
    traits: HashMap<String, TraitDecl>,
    /// 手動 `impl Trait for Type`（(trait全域名, 型別全域名) → 方法）
    manual_impls: HashMap<(String, String), Vec<FunctionDecl>>,
    /// 固有 `impl Type` 的方法（型別全域名 → 方法名一覽，供自動橋接用）
    inherent_methods: HashMap<String, Vec<FunctionDecl>>,
    type_names: HashSet<String>,
    fns: HashMap<String, FnSig>,
    methods: HashMap<String, FnSig>,
    externs: HashSet<String>,
    scopes: Vec<HashMap<String, Type>>,
    ret_stack: Vec<Type>,
    managed: usize,
    /// None = 探測模式（全部 derive）；Some(集合) = 只對集合內的型別 derive Trace
    trace_set: Option<HashSet<String>>,
    gc_used: HashSet<String>,
    touched_gc: bool,
    /// 有任何型別/邊界需要 serde（Serializable 自動衍生或邊界引用）時自動加入依賴
    needs_serde: bool,
    main_async: bool,
    mods: Vec<ModInfo>,
    cur: usize,
    tmp: usize,
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "else", "enum", "if", "impl", "loop", "match", "mod", "pub",
    "ref", "static", "trait", "type", "use", "where", "dyn", "abstract", "become", "box", "do",
    "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
];

fn id(s: &str) -> Ident {
    if RUST_KEYWORDS.contains(&s) {
        Ident::new_raw(s, Span::call_site())
    } else {
        Ident::new(s, Span::call_site())
    }
}

/// 回傳 usize 的常見方法，統一轉成 Aero 的 Int(i64)
const USIZE_METHODS: &[&str] = &["len", "count", "capacity"];
/// 對 gc 物件只需要共享借用的唯讀方法
const READONLY_METHODS: &[&str] = &[
    "len", "get", "iter", "contains", "contains_key", "is_empty", "first", "last", "clone",
    "to_string", "keys", "values", "starts_with", "ends_with", "trim", "to_uppercase",
    "to_lowercase",
];

fn is_copy(t: &Type) -> bool {
    !t.gc && matches!(t.kind, TypeKind::Int | TypeKind::Float | TypeKind::Bool | TypeKind::Unit)
}

/// derive(Trace) 時需要被忽略的欄位型別（閉包 / 原始指標 / Future）
fn needs_ignore_trace(t: &Type) -> bool {
    match &t.kind {
        TypeKind::Closure { .. } | TypeKind::RawPointer | TypeKind::Future(_) => true,
        TypeKind::Array(e) => needs_ignore_trace(e),
        TypeKind::Dict(k, v) => needs_ignore_trace(k) || needs_ignore_trace(v),
        TypeKind::Ref { inner, .. } => needs_ignore_trace(inner),
        TypeKind::Generic { args, .. } => args.iter().any(needs_ignore_trace),
        _ => false,
    }
}

impl CodeGen {
    pub fn generate(mods: &[Module]) -> Output {
        // 先以 derive(Trace) 試跑；若整個程式根本沒碰到 GC，就關掉 derive 再跑一次，
        // 讓純 Zero-GC 程式完全不依賴 gc crate（對應規格書的「GC 模組完全不載入」）。
        let probe = Self::run(mods, None);
        if !probe.touched_gc {
            return Self::run(mods, Some(HashSet::new()));
        }
        Self::run(mods, Some(probe.gc_types))
    }

    fn run(modules: &[Module], trace_set: Option<HashSet<String>>) -> Output {
        let mut cg = CodeGen {
            structs: HashMap::new(),
            enums: HashMap::new(),
            traits: HashMap::new(),
            manual_impls: HashMap::new(),
            inherent_methods: HashMap::new(),
            type_names: HashSet::new(),
            fns: HashMap::new(),
            methods: HashMap::new(),
            externs: HashSet::new(),
            scopes: vec![],
            ret_stack: vec![],
            managed: 0,
            trace_set,
            gc_used: HashSet::new(),
            touched_gc: false,
            needs_serde: false,
            main_async: false,
            mods: modules
                .iter()
                .map(|m| ModInfo {
                    prefix: m.prefix.clone(),
                    aliases: m.aliases.clone(),
                    chars: m.src.chars().collect(),
                })
                .collect(),
            cur: 0,
            tmp: 0,
        };
        let mut deps: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();

        // Pass 0：所有型別名稱（含模組前綴）；trait 名亦為名義符號，供邊界解析用
        for (mi, m) in modules.iter().enumerate() {
            for item in &m.prog.items {
                match &item.decl {
                    Decl::Struct(s) => {
                        cg.type_names.insert(format!("{}{}", cg.mods[mi].prefix, s.name));
                    }
                    Decl::Enum(e) => {
                        cg.type_names.insert(format!("{}{}", cg.mods[mi].prefix, e.name));
                    }
                    Decl::Trait(t) => {
                        cg.type_names.insert(format!("{}{}", cg.mods[mi].prefix, t.name));
                    }
                    _ => {}
                }
            }
        }
        // Pass 1：收集宣告；型別一律正規化成「已加前綴的全域名稱」
        for (mi, m) in modules.iter().enumerate() {
            cg.cur = mi;
            let pre = cg.mods[mi].prefix.clone();
            for item in &m.prog.items {
                match &item.decl {
                    Decl::Struct(s) => {
                        let mut d = s.clone();
                        d.name = format!("{pre}{}", s.name);
                        for g in &mut d.generics {
                            g.bounds = g.bounds.iter().map(|b| cg.rt(b)).collect();
                        }
                        for f in &mut d.fields {
                            f.ty = cg.norm(&f.ty);
                        }
                        cg.structs.insert(d.name.clone(), d);
                    }
                    Decl::Enum(e) => {
                        let mut d = e.clone();
                        d.name = format!("{pre}{}", e.name);
                        for g in &mut d.generics {
                            g.bounds = g.bounds.iter().map(|b| cg.rt(b)).collect();
                        }
                        for v in &mut d.variants {
                            v.fields = v.fields.iter().map(|t| cg.norm(t)).collect();
                        }
                        cg.enums.insert(d.name.clone(), d);
                    }
                    Decl::Trait(t) => {
                        let mut d = t.clone();
                        d.name = format!("{pre}{}", t.name);
                        for mth in &mut d.methods {
                            for p in &mut mth.params {
                                p.ty = cg.norm(&p.ty);
                            }
                            mth.ret = cg.norm(&mth.ret);
                        }
                        cg.traits.insert(d.name.clone(), d);
                    }
                    _ => {}
                }
            }
        }
        for (mi, m) in modules.iter().enumerate() {
            cg.cur = mi;
            let pre = cg.mods[mi].prefix.clone();
            for item in &m.prog.items {
                match &item.decl {
                    Decl::Function(f) => {
                        if mi == 0 && f.name == "main" {
                            cg.main_async = f.is_async;
                        }
                        let sig = cg.sig_of(&f.params, &f.ret, f.is_async, None, &f.generics);
                        cg.fns.insert(format!("{pre}{}", f.name), sig);
                    }
                    Decl::Extern(b) => {
                        for f in &b.items {
                            cg.externs.insert(f.name.clone());
                            let sig = cg.sig_of(&f.params, &f.ret, false, None, &[]);
                            cg.fns.insert(f.name.clone(), sig);
                        }
                    }
                    Decl::Impl(im) => {
                        let tname = cg.rt(&im.name);
                        for mth in &im.methods {
                            let sig =
                                cg.sig_of(&mth.params, &mth.ret, mth.is_async, mth.self_kind, &mth.generics);
                            cg.methods.insert(format!("{tname}::{}", mth.name), sig);
                        }
                        let normed: Vec<FunctionDecl> =
                            im.methods.iter().map(|m| cg.norm_fn(m)).collect();
                        cg.inherent_methods.entry(tname).or_default().extend(normed);
                    }
                    Decl::ImplTrait(it) => {
                        let tname = cg.rt(&it.trait_name);
                        let tyname = cg.rt(&it.type_name);
                        for mth in &it.methods {
                            let sig =
                                cg.sig_of(&mth.params, &mth.ret, mth.is_async, mth.self_kind, &mth.generics);
                            cg.methods.insert(format!("{tyname}::{}", mth.name), sig);
                        }
                        let normed: Vec<FunctionDecl> =
                            it.methods.iter().map(|m| cg.norm_fn(m)).collect();
                        cg.manual_impls.entry((tname, tyname)).or_default().extend(normed);
                    }
                    Decl::Import(i) => {
                        let e = deps
                            .entry(i.name.clone())
                            .or_insert((i.version.clone().unwrap_or_else(|| "*".into()), vec![]));
                        for f in &i.features {
                            if !e.1.contains(f) {
                                e.1.push(f.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // Pass 2：產生 items
        let mut items = vec![];
        for (mi, m) in modules.iter().enumerate() {
            cg.cur = mi;
            for item in &m.prog.items {
                let attr = item.attr.as_ref().map(|p| {
                    let segs: Vec<Ident> = p.iter().map(|s| id(s)).collect();
                    quote!(#[#(#segs)::*])
                });
                let body = match &item.decl {
                    Decl::Import(_) | Decl::ImportMod { .. } => continue,
                    Decl::Extern(b) => cg.extern_block(b),
                    Decl::Struct(s) => cg.struct_decl(s),
                    Decl::Enum(e) => cg.enum_decl(e),
                    Decl::Trait(t) => cg.trait_decl(t),
                    Decl::Function(f) => cg.function(f, None),
                    Decl::Impl(im) => {
                        let tname = cg.rt(&im.name);
                        let name = cg.type_tokens(&tname);
                        let (gdecl, guse) = cg.impl_header(&tname);
                        let ms: Vec<_> = im
                            .methods
                            .iter()
                            .map(|mth| {
                                let mk = cg.marker(mth.off);
                                let f = cg.function(mth, Some(&tname));
                                quote!(#mk #f)
                            })
                            .collect();
                        quote!(impl #gdecl #name #guse { #(#ms)* })
                    }
                    Decl::ImplTrait(it) => {
                        let tname = cg.rt(&it.trait_name);
                        let tyname = cg.rt(&it.type_name);
                        cg.impl_trait_block(&tname, &tyname, &it.methods)
                    }
                };
                let mk = cg.marker(item.off);
                items.push(quote!(#mk #attr #body));
            }
        }
        // Pass 3：自動橋接——固有方法已覆蓋 trait 時自動產生 `impl Trait for Type`，
        // LLM 永遠不用手寫 impl（Curry-Howard：編譯器自動補上命題的證明項）
        items.extend(cg.auto_bridge());

        if cg.main_async {
            let e = deps.entry("tokio".into()).or_insert(("1".into(), vec![]));
            for f in ["macros", "rt-multi-thread"] {
                if !e.1.iter().any(|x| x == f || x == "full") {
                    e.1.push(f.into());
                }
            }
        }
        if cg.touched_gc {
            deps.entry("gc".into()).or_insert(("0.5".into(), vec!["derive".into()]));
        }
        if cg.needs_serde {
            deps.entry("serde".into()).or_insert(("1".into(), vec!["derive".into()]));
        }

        let tokens = quote! {
            #![allow(dead_code, unused_variables, unused_mut, unused_parens, unused_unsafe,
                     unused_imports, non_snake_case, non_camel_case_types)]

            /// Aero 內建小型 runtime（對應 std::io 的最小子集）
            mod aero_rt {
                pub fn println<T: ::std::fmt::Display>(v: T) { ::std::println!("{}", v); }
                pub fn print<T: ::std::fmt::Display>(v: T) { ::std::print!("{}", v); }
                pub fn to_str<T: ::std::fmt::Display>(v: T) -> ::std::string::String { v.to_string() }
            }
            use aero_rt::*;

            #(#items)*
        };
        let gc_types = cg.gc_closure();
        Output { tokens, deps, touched_gc: cg.touched_gc, gc_types }
    }

    // ───────────────────────── 模組 / 符號解析 ─────────────────────────

    fn traced(&self, mangled: &str) -> bool {
        self.trace_set.as_ref().map_or(true, |s| s.contains(mangled))
    }

    fn custom_names(t: &Type, out: &mut Vec<String>) {
        match &t.kind {
            TypeKind::Custom(n) => out.push(n.clone()),
            TypeKind::Generic { base, args } => {
                out.push(base.clone());
                for a in args {
                    Self::custom_names(a, out);
                }
            }
            TypeKind::Array(e) | TypeKind::Future(e) | TypeKind::Option(e) => Self::custom_names(e, out),
            TypeKind::Ref { inner, .. } => Self::custom_names(inner, out),
            TypeKind::Dict(k, v) => {
                Self::custom_names(k, out);
                Self::custom_names(v, out);
            }
            _ => {}
        }
    }

    /// gc 宣告的型別 + 被 `gc T` 使用的型別，以及它們欄位/變體可達的所有型別
    fn gc_closure(&self) -> HashSet<String> {
        let mut set: HashSet<String> = self.gc_used.clone();
        for (n, s) in &self.structs {
            if s.gc {
                set.insert(n.clone());
            }
        }
        for (n, e) in &self.enums {
            if e.gc {
                set.insert(n.clone());
            }
        }
        loop {
            let mut add = vec![];
            for n in &set {
                let mut names = vec![];
                if let Some(s) = self.structs.get(n) {
                    s.fields.iter().for_each(|f| Self::custom_names(&f.ty, &mut names));
                }
                if let Some(e) = self.enums.get(n) {
                    e.variants.iter().for_each(|v| v.fields.iter().for_each(|t| Self::custom_names(t, &mut names)));
                }
                for c in names {
                    if self.type_names.contains(&c) && !set.contains(&c) {
                        add.push(c);
                    }
                }
            }
            if add.is_empty() {
                break;
            }
            set.extend(add);
        }
        set
    }

    fn prefix(&self) -> String {
        self.mods[self.cur].prefix.clone()
    }

    fn sig_of(
        &self,
        params: &[Param],
        ret: &Type,
        is_async: bool,
        self_kind: Option<SelfKind>,
        generics: &[GenericParam],
    ) -> FnSig {
        FnSig {
            params: params
                .iter()
                .map(|p| Param { mutable: p.mutable, name: p.name.clone(), ty: self.norm(&p.ty) })
                .collect(),
            ret: self.norm(ret),
            is_async,
            self_kind,
            generics: generics
                .iter()
                .map(|g| GenericParam {
                    name: g.name.clone(),
                    bounds: g.bounds.iter().map(|b| self.rt(b)).collect(),
                })
                .collect(),
        }
    }

    /// 把方法宣告正規化（全域名稱），供跨模組的自動橋接使用
    fn norm_fn(&self, f: &FunctionDecl) -> FunctionDecl {
        let mut d = f.clone();
        for g in &mut d.generics {
            g.bounds = g.bounds.iter().map(|b| self.rt(b)).collect();
        }
        for p in &mut d.params {
            p.ty = self.norm(&p.ty);
        }
        d.ret = self.norm(&d.ret);
        d
    }

    /// 解析路徑開頭：`alias::Name` → (目標模組前綴 + Name, 2)；否則本模組 → (前綴 + 名稱, 1)
    fn sym_of(&self, segs: &[String]) -> (String, usize) {
        if segs.len() >= 2 {
            if let Some(&mi) = self.mods[self.cur].aliases.get(&segs[0]) {
                return (format!("{}{}", self.mods[mi].prefix, segs[1]), 2);
            }
        }
        let local = format!("{}{}", self.prefix(), segs[0]);
        if self.type_names.contains(&local) || self.fns.contains_key(&local) {
            return (local, 1);
        }
        (segs[0].clone(), 1)
    }

    /// 型別名稱 → 全域（已加前綴）名稱；解析不到就原樣保留（例如外部 crate 的型別）
    fn rt(&self, n: &str) -> String {
        let segs: Vec<String> = n.split("::").map(String::from).collect();
        let (sym, used) = self.sym_of(&segs);
        if used == segs.len() && self.type_names.contains(&sym) {
            sym
        } else {
            n.to_string()
        }
    }

    fn norm(&self, t: &Type) -> Type {
        let kind = match &t.kind {
            TypeKind::Custom(n) => TypeKind::Custom(self.rt(n)),
            TypeKind::Generic { base, args } => TypeKind::Generic {
                base: self.rt(base),
                args: args.iter().map(|a| self.norm(a)).collect(),
            },
            TypeKind::Array(e) => TypeKind::Array(Box::new(self.norm(e))),
            TypeKind::Dict(k, v) => TypeKind::Dict(Box::new(self.norm(k)), Box::new(self.norm(v))),
            TypeKind::Future(e) => TypeKind::Future(Box::new(self.norm(e))),
            TypeKind::Option(e) => TypeKind::Option(Box::new(self.norm(e))),
            TypeKind::Ref { mutable, inner } => {
                TypeKind::Ref { mutable: *mutable, inner: Box::new(self.norm(inner)) }
            }
            TypeKind::Closure { is_async, params, ret } => TypeKind::Closure {
                is_async: *is_async,
                params: params.iter().map(|p| self.norm(p)).collect(),
                ret: Box::new(self.norm(ret)),
            },
            k => k.clone(),
        };
        Type { gc: t.gc, kind }
    }

    fn type_tokens(&self, n: &str) -> TokenStream {
        let r = self.rt(n);
        let segs: Vec<Ident> = r.split("::").map(id).collect();
        quote!(#(#segs)::*)
    }

    /// 路徑（函式 / 型別成員 / 列舉變體）解析
    fn resolve_callee(&self, p: &[String]) -> Callee {
        if p.is_empty() {
            return Callee::Unknown;
        }
        let (sym, used) = self.sym_of(p);
        let rest = &p[used.min(p.len())..];
        if rest.is_empty() {
            if self.fns.contains_key(&sym) {
                return Callee::Fn(sym);
            }
        } else if rest.len() == 1 && self.type_names.contains(&sym) {
            if self.methods.contains_key(&format!("{sym}::{}", rest[0])) {
                return Callee::Method(sym, rest[0].clone());
            }
            if self
                .enums
                .get(&sym)
                .map_or(false, |e| e.variants.iter().any(|v| v.name == rest[0]))
            {
                return Callee::Variant(sym, rest[0].clone());
            }
        }
        Callee::Unknown
    }

    // ───────────────────────── 來源位置標記 ─────────────────────────

    /// char offset → (行, 列)（略過前導空白與註解）
    fn loc(&self, mut off: usize) -> (usize, usize) {
        let ch = &self.mods[self.cur].chars;
        loop {
            while off < ch.len() && ch[off].is_whitespace() {
                off += 1;
            }
            if off + 1 < ch.len() && ch[off] == '/' && ch[off + 1] == '/' {
                while off < ch.len() && ch[off] != '\n' {
                    off += 1;
                }
            } else {
                break;
            }
        }
        let (mut line, mut col) = (1, 1);
        for &c in ch.iter().take(off) {
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// `#[aero_at(行, 列)]`：排版後由 driver 剝除並轉成對照表，rustc 看不到它
    fn marker(&self, off: usize) -> TokenStream {
        let (l, c) = self.loc(off);
        let (l, c) = (Literal::usize_unsuffixed(l), Literal::usize_unsuffixed(c));
        let f = Literal::usize_unsuffixed(self.cur);
        quote!(#[aero_at(#f, #l, #c)])
    }

    // ───────────────────────── 型別 ─────────────────────────

    fn ty(&mut self, t: &Type) -> TokenStream {
        let inner = match &t.kind {
            TypeKind::Int => quote!(i64),
            TypeKind::Float => quote!(f64),
            TypeKind::Str => quote!(::std::string::String),
            TypeKind::Bool => quote!(bool),
            TypeKind::Unit => quote!(()),
            TypeKind::RawPointer => quote!(*mut ::std::ffi::c_void),
            TypeKind::Array(e) => {
                let e = self.ty(e);
                quote!(::std::vec::Vec<#e>)
            }
            TypeKind::Dict(k, v) => {
                let (k, v) = (self.ty(k), self.ty(v));
                quote!(::std::collections::HashMap<#k, #v>)
            }
            TypeKind::Future(t) => {
                let t = self.ty(t);
                quote!(::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = #t>>>)
            }
            TypeKind::Ref { mutable, inner } => {
                let i = self.ty(inner);
                if *mutable { quote!(&mut #i) } else { quote!(& #i) }
            }
            TypeKind::Closure { is_async, params, ret } => {
                let ps: Vec<_> = params.iter().map(|p| self.ty(p)).collect();
                let r = self.ty(ret);
                if *is_async {
                    quote!(::std::rc::Rc<dyn Fn(#(#ps),*) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = #r>>>>)
                } else {
                    quote!(::std::rc::Rc<dyn Fn(#(#ps),*) -> #r>)
                }
            }
            TypeKind::Custom(n) => self.type_tokens(n),
            TypeKind::Generic { base, args } => {
                let b = self.type_tokens(base);
                let a: Vec<_> = args.iter().map(|x| self.ty(x)).collect();
                quote!(#b<#(#a),*>)
            }
            TypeKind::Option(t) => {
                let t = self.ty(t);
                quote!(::std::option::Option<#t>)
            }
        };
        if t.gc {
            self.touched_gc = true;
            match &t.kind {
                TypeKind::Custom(n) => {
                    let r = self.rt(n);
                    self.gc_used.insert(r);
                }
                TypeKind::Generic { base, .. } => {
                    let r = self.rt(base);
                    self.gc_used.insert(r);
                }
                _ => {}
            }
            quote!(::gc::Gc<::gc::GcCell<#inner>>)
        } else {
            inner
        }
    }

    fn param_tokens(&mut self, p: &Param) -> TokenStream {
        let m = if p.mutable { quote!(mut) } else { quote!() };
        let n = id(&p.name);
        let t = self.ty(&p.ty);
        quote!(#m #n: #t)
    }

    // ───────────────────────── Trait / 泛型：證明與邊界 ─────────────────────────

    /// Aero 邊界名 → Rust 邊界 TokenStream（內建特徵查表；使用者 trait 則解析為全域名）。
    /// 引用到 Serializable 時順手標記需要 serde 依賴。
    fn bound_tokens(&mut self, aero: &str) -> Vec<TokenStream> {
        if aero == "Serializable" {
            self.needs_serde = true;
        }
        if let Some(v) = builtin_bounds(aero) {
            return v;
        }
        let t = self.type_tokens(aero);
        vec![quote!(#t)]
    }

    /// `fn f[T: Add + Clone, U]` → `<T: Add...\> , U>` 的泛型參數段（含前導 `<>` 轉為 quote 片段）
    fn generics_decl(&mut self, gs: &[GenericParam]) -> TokenStream {
        if gs.is_empty() {
            return quote!();
        }
        let ps: Vec<_> = gs
            .iter()
            .map(|g| {
                let n = id(&g.name);
                let mut bs: Vec<TokenStream> = vec![];
                for b in &g.bounds {
                    if let Some(a) = arith_bound(b, &n) {
                        bs.push(a);
                    } else {
                        bs.extend(self.bound_tokens(b));
                    }
                }
                if bs.is_empty() { quote!(#n) } else { quote!(#n: #(#bs)+*) }
            })
            .collect();
        quote!(<#(#ps),*>)
    }

    /// 型別參考處的泛型實參 `<T, U>`（定義處的宣告用 generics_decl）
    fn generics_use(&self, gs: &[GenericParam]) -> TokenStream {
        if gs.is_empty() {
            return quote!();
        }
        let ps: Vec<_> = gs.iter().map(|g| id(&g.name)).collect();
        quote!(<#(#ps),*>)
    }

    /// 結構化證明：型別 `t` 是否滿足 Aero 命題 `prop`（trait）？
    /// 基本型查表；容器遞迴檢查元素；自訂型別檢查所有欄位/變體；
    /// 泛型參數看其邊界；遞迴型別以 coinduction 視為成立（Rust derive 本來就支援）。
    /// `env`: 作用域內的泛型參數 → 其邊界；`seen`: 遞迴守衛。
    fn provable(
        &self,
        t: &Type,
        prop: &str,
        env: &HashMap<String, Vec<String>>,
        seen: &mut HashSet<String>,
    ) -> bool {
        // 命題蘊含：Comparable ⇒ Equatable；Hashable ⇒ Equatable
        let props: &[&str] = match prop {
            "Equatable" => &[prop, "Comparable", "Hashable"],
            _ => std::slice::from_ref(&prop),
        };
        props.iter().any(|p| self.provable_one(t, p, env, seen))
    }

    fn provable_one(
        &self,
        t: &Type,
        prop: &str,
        env: &HashMap<String, Vec<String>>,
        seen: &mut HashSet<String>,
    ) -> bool {
        if t.gc {
            // `gc T` 在 Rust 是 `Gc<GcCell<T>>`：只有 Clone（共享控制代碼），
            // 沒有 PartialEq/Hash/Serialize 等實作——結構化證明到此為止，交給 rustc。
            return prop == "Clone";
        }
        match &t.kind {
            TypeKind::Int => matches!(
                prop,
                "Clone" | "Equatable" | "Comparable" | "Hashable" | "Serializable" | "Add"
                    | "Sub" | "Mul" | "Div" | "Rem" | "Default" | "Display" | "Debug"
            ),
            TypeKind::Float => matches!(
                prop,
                "Clone" | "Comparable" | "Serializable" | "Add" | "Sub" | "Mul" | "Div"
                    | "Rem" | "Default" | "Display" | "Debug"
            ),
            TypeKind::Str => matches!(
                prop,
                "Clone" | "Equatable" | "Comparable" | "Hashable" | "Serializable" | "Default"
                    | "Display" | "Debug"
            ),
            TypeKind::Bool => matches!(
                prop,
                "Clone" | "Equatable" | "Comparable" | "Hashable" | "Serializable" | "Default"
                    | "Display" | "Debug"
            ),
            TypeKind::Unit => matches!(
                prop,
                "Clone" | "Equatable" | "Comparable" | "Hashable" | "Serializable" | "Default"
                    | "Debug"
            ),
            TypeKind::RawPointer | TypeKind::Closure { .. } | TypeKind::Future(_) => {
                matches!(prop, "Clone")
            }
            TypeKind::Array(e) | TypeKind::Option(e) => {
                if prop == "Default" {
                    return true; // Vec::new / None
                }
                self.provable(e, prop, env, seen)
            }
            TypeKind::Dict(k, v) => {
                if prop == "Default" {
                    return true;
                }
                // HashMap 沒有 PartialOrd/Hash 實作
                if matches!(prop, "Comparable" | "Hashable") {
                    return false;
                }
                self.provable(k, prop, env, seen) && self.provable(v, prop, env, seen)
            }
            TypeKind::Ref { inner, .. } => self.provable(inner, prop, env, seen),
            TypeKind::Generic { base, args } => {
                let b = self.rt(base);
                // 以實參代入形參後檢查欄位
                if let Some(s) = self.structs.get(&b).cloned() {
                    if !seen.insert(format!("{b}#{prop}")) {
                        return true;
                    }
                    let sub: HashMap<String, Type> = s
                        .generics
                        .iter()
                        .map(|g| g.name.clone())
                        .zip(args.iter().cloned())
                        .collect();
                    let mut ext = env.clone();
                    for g in &s.generics {
                        ext.entry(g.name.clone()).or_insert_with(|| g.bounds.clone());
                    }
                    let r = s.fields.iter().all(|f| {
                        let st = subst_type(&f.ty, &sub);
                        self.provable(&st, prop, &ext, seen)
                    });
                    seen.remove(&format!("{b}#{prop}"));
                    return r;
                }
                if let Some(e) = self.enums.get(&b).cloned() {
                    if !seen.insert(format!("{b}#{prop}")) {
                        return true;
                    }
                    let sub: HashMap<String, Type> = e
                        .generics
                        .iter()
                        .map(|g| g.name.clone())
                        .zip(args.iter().cloned())
                        .collect();
                    let mut ext = env.clone();
                    for g in &e.generics {
                        ext.entry(g.name.clone()).or_insert_with(|| g.bounds.clone());
                    }
                    let r = e.variants.iter().all(|v| {
                        v.fields.iter().all(|f| self.provable(&subst_type(f, &sub), prop, &ext, seen))
                    });
                    seen.remove(&format!("{b}#{prop}"));
                    return r;
                }
                false
            }
            TypeKind::Custom(n) => {
                let r = self.rt(n);
                if r == "Self" {
                    return false; // Self 在定義處無法結構化判定，交給 rustc
                }
                if let Some(bs) = env.get(&r).or_else(|| env.get(n)) {
                    // 泛型參數：邊界即證明（Builtin 查表 + 蘊含已在 provable 處理）
                    return bs.iter().any(|b| b == prop)
                        || (prop == "Equatable"
                            && bs.iter().any(|b| b == "Comparable" || b == "Hashable"));
                }
                if let Some(s) = self.structs.get(&r).cloned() {
                    if !s.generics.is_empty() {
                        return false; // 未具現的泛型：無法判定，交給 rustc 單態化時檢查
                    }
                    if !seen.insert(format!("{r}#{prop}")) {
                        return true;
                    }
                    let res = s.fields.iter().all(|f| self.provable(&f.ty, prop, env, seen));
                    seen.remove(&format!("{r}#{prop}"));
                    return res;
                }
                if let Some(e) = self.enums.get(&r).cloned() {
                    if !e.generics.is_empty() {
                        return false;
                    }
                    if !seen.insert(format!("{r}#{prop}")) {
                        return true;
                    }
                    let res = e
                        .variants
                        .iter()
                        .all(|v| v.fields.iter().all(|f| self.provable(f, prop, env, seen)));
                    seen.remove(&format!("{r}#{prop}"));
                    return res;
                }
                // 未知外部型別：只有 rustc 知道，保守回 false（不自動 derive，rustc 會報錯指回）
                false
            }
        }
    }

    /// 某 struct/enum 可自動衍生的 Rust derive 一覽（結構化證明的結論）。
    /// 含 serde（Serializable 可證時自動加依賴）；呼叫者只需拼進 `#[derive(..)]`。
    fn auto_derives(&mut self, name: &str) -> Vec<TokenStream> {
        let mut out: Vec<TokenStream> = vec![];
        let mut env = HashMap::new();
        let seen = HashSet::new();
        // 泛型定義：以自身邊界為 env，derive 仍可無條件加上（Rust derive 會自動加條件界）
        let fields_ok = |fs: &[Type]| {
            fs.iter().all(|f| {
                !matches!(&f.kind, TypeKind::Closure { .. } | TypeKind::RawPointer | TypeKind::Future(_))
            })
        };
        let has_fields = if let Some(s) = self.structs.get(name) {
            for g in &s.generics {
                env.insert(g.name.clone(), g.bounds.clone());
            }
            fields_ok(&s.fields.iter().map(|f| f.ty.clone()).collect::<Vec<_>>())
        } else if let Some(e) = self.enums.get(name) {
            for g in &e.generics {
                env.insert(g.name.clone(), g.bounds.clone());
            }
            fields_ok(&e.variants.iter().flat_map(|v| v.fields.clone()).collect::<Vec<_>>())
        } else {
            return out;
        };
        if !has_fields {
            return out;
        }
        let probe = |prop: &str| {
            let mut seen = seen.clone();
            if let Some(s) = self.structs.get(name) {
                if s.generics.is_empty() {
                    s.fields.iter().all(|f| self.provable(&f.ty, prop, &env, &mut seen))
                } else {
                    // 泛型：每個欄位在「假設形參滿足其邊界」下可證（derive 的條件界會強制）
                    s.fields.iter().all(|f| self.provable(&f.ty, prop, &env, &mut seen))
                }
            } else if let Some(e) = self.enums.get(name) {
                e.variants
                    .iter()
                    .all(|v| v.fields.iter().all(|f| self.provable(f, prop, &env, &mut seen)))
            } else {
                false
            }
        };
        // 注意：含 gc 欄位的型別 Trace 已由另一路徑處理；此處只管純 Rust derive
        if probe("Equatable") || probe("Comparable") || probe("Hashable") {
            out.push(quote!(::std::cmp::PartialEq));
        }
        if probe("Comparable") {
            out.push(quote!(::std::cmp::PartialOrd));
        }
        if probe("Hashable") {
            out.push(quote!(::std::hash::Hash));
            out.push(quote!(::std::cmp::Eq));
        }
        if probe("Debug") {
            out.push(quote!(::std::fmt::Debug));
        }
        if probe("Default") && self.structs.contains_key(name) {
            // 只給 struct 加 Default（enum 的 derive(Default) 需要 #[default] 標記，不自動加）；
            // 有 gc 欄位時不加（Gc 沒有 Default 實作）
            let has_gc = if let Some(s) = self.structs.get(name) {
                s.fields.iter().any(|f| f.ty.gc)
            } else {
                false
            };
            if !has_gc {
                out.push(quote!(::std::default::Default));
            }
        }
        if probe("Serializable") {
            self.needs_serde = true;
            out.push(quote!(::serde::Serialize));
            out.push(quote!(::serde::Deserialize));
        }
        out
    }

    // ───────────────────────── 頂層宣告 ─────────────────────────

    fn extern_block(&mut self, b: &ExternBlock) -> TokenStream {
        let abi = Literal::string(&b.abi);
        let fns: Vec<_> = b
            .items
            .iter()
            .map(|f| {
                let n = id(&f.name);
                let ps: Vec<_> = f.params.iter().map(|p| self.param_tokens(p)).collect();
                let ret = self.ret_tokens(&f.ret);
                quote!(fn #n(#(#ps),*) #ret;)
            })
            .collect();
        quote!(extern #abi { #(#fns)* })
    }

    fn enum_decl(&mut self, e: &EnumDecl) -> TokenStream {
        let mangled = format!("{}{}", self.prefix(), e.name);
        let traced = self.traced(&mangled);
        let name = id(&mangled);
        let gdecl = self.generics_decl(&e.generics);
        // 結構化自動衍生：編譯器證明欄位滿足命題後自動加上對應 derive（LLM 零負擔）
        let extra: Vec<TokenStream> = self.auto_derives(&mangled);
        let mut derives: Vec<TokenStream> = vec![quote!(Clone)];
        if traced {
            derives.push(quote!(::gc::Trace));
            derives.push(quote!(::gc::Finalize));
        }
        derives.extend(extra);
        let derive = quote!(#[derive(#(#derives),*)]);
        let vs: Vec<_> = e
            .variants
            .iter()
            .map(|v| {
                let vn = id(&v.name);
                if v.fields.is_empty() {
                    quote!(#vn)
                } else {
                    let fs: Vec<_> = v
                        .fields
                        .iter()
                        .map(|t| {
                            let ignore = if traced && needs_ignore_trace(t) {
                                quote!(#[unsafe_ignore_trace])
                            } else {
                                quote!()
                            };
                            let tt = self.ty(t);
                            quote!(#ignore #tt)
                        })
                        .collect();
                    quote!(#vn(#(#fs),*))
                }
            })
            .collect();
        if e.gc {
            self.touched_gc = true;
        }
        quote!(#derive pub enum #name #gdecl { #(#vs),* })
    }

    fn struct_decl(&mut self, s: &StructDecl) -> TokenStream {
        let mangled = format!("{}{}", self.prefix(), s.name);
        let traced = self.traced(&mangled);
        let name = id(&mangled);
        let gdecl = self.generics_decl(&s.generics);
        let extra: Vec<TokenStream> = self.auto_derives(&mangled);
        let mut derives: Vec<TokenStream> = vec![quote!(Clone)];
        if traced {
            derives.push(quote!(::gc::Trace));
            derives.push(quote!(::gc::Finalize));
        }
        derives.extend(extra);
        let derive = quote!(#[derive(#(#derives),*)]);
        let fields: Vec<_> = s
            .fields
            .iter()
            .map(|f| {
                let n = id(&f.name);
                let t = self.ty(&f.ty);
                let ignore = if traced && needs_ignore_trace(&f.ty) {
                    quote!(#[unsafe_ignore_trace])
                } else {
                    quote!()
                };
                quote!(#ignore pub #n: #t)
            })
            .collect();
        if s.gc {
            self.touched_gc = true;
        }
        quote!(#derive pub struct #name #gdecl { #(#fields),* })
    }

    fn ret_tokens(&mut self, t: &Type) -> TokenStream {
        if matches!(t.kind, TypeKind::Unit) && !t.gc {
            quote!()
        } else {
            let t = self.ty(t);
            quote!(-> #t)
        }
    }

    fn function(&mut self, f: &FunctionDecl, owner: Option<&str>) -> TokenStream {
        let mut scope: HashMap<String, Type> =
            f.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
        if let (Some(k), Some(o)) = (&f.self_kind, owner) {
            let base = Type::plain(TypeKind::Custom(o.to_string()));
            let t = match k {
                SelfKind::Ref => Type::plain(TypeKind::Ref { mutable: false, inner: Box::new(base) }),
                SelfKind::RefMut => Type::plain(TypeKind::Ref { mutable: true, inner: Box::new(base) }),
                SelfKind::Value { .. } => base,
            };
            scope.insert("self".into(), t);
        }
        self.scopes.push(scope);
        self.ret_stack.push(f.ret.clone());
        let body = self.stmts(&f.body);
        self.ret_stack.pop();
        self.scopes.pop();

        let name = if owner.is_some() { id(&f.name) } else { id(&format!("{}{}", self.prefix(), f.name)) };
        let gdecl = self.generics_decl(&f.generics);
        let ps: Vec<_> = f.params.iter().map(|p| self.param_tokens(p)).collect();
        let ret = self.ret_tokens(&f.ret);
        let asyncness = if f.is_async { quote!(async) } else { quote!() };
        let attr = if f.name == "main" && f.is_async && self.cur == 0 && owner.is_none() { quote!(#[tokio::main]) } else { quote!() };
        let self_tok = match &f.self_kind {
            None => vec![],
            Some(SelfKind::Ref) => vec![quote!(&self)],
            Some(SelfKind::RefMut) => vec![quote!(&mut self)],
            Some(SelfKind::Value { mutable: true }) => vec![quote!(mut self)],
            Some(SelfKind::Value { mutable: false }) => vec![quote!(self)],
        };
        quote!(#attr #asyncness fn #name #gdecl (#(#self_tok,)* #(#ps),*) #ret { #body })
    }

    /// `impl Type` 的標頭：泛型型別自動帶上定義處的邊界（`impl<T: Clone> Pair<T>`）
    fn impl_header(&mut self, tname: &str) -> (TokenStream, TokenStream) {
        if let Some(s) = self.structs.get(tname).cloned() {
            if !s.generics.is_empty() {
                let gdecl = self.generics_decl(&s.generics);
                let guse = self.generics_use(&s.generics);
                return (gdecl, guse);
            }
        }
        if let Some(e) = self.enums.get(tname).cloned() {
            if !e.generics.is_empty() {
                let gdecl = self.generics_decl(&e.generics);
                let guse = self.generics_use(&e.generics);
                return (gdecl, guse);
            }
        }
        (quote!(), quote!())
    }

    /// 還原模組前綴（`geo__Point` → `Point`），供內建特徵名比對用
    fn unmangle(&self, n: &str) -> String {
        for m in &self.mods {
            let p = &m.prefix;
            if !p.is_empty() && n.starts_with(p) {
                return n[p.len()..].to_string();
            }
        }
        n.to_string()
    }

    /// `trait Name ... end` → Rust trait（命題的規格；證明由編譯器自動衍生或手動 impl 提供）
    fn trait_decl(&mut self, t: &TraitDecl) -> TokenStream {
        let mangled = format!("{}{}", self.prefix(), t.name);
        let name = id(&mangled);
        let ms: Vec<_> = t
            .methods
            .iter()
            .map(|m| {
                let n = id(&m.name);
                let ps: Vec<_> = m.params.iter().map(|p| self.param_tokens(p)).collect();
                let ret = self.ret_tokens(&m.ret);
                let self_tok: Vec<TokenStream> = match &m.self_kind {
                    None => vec![],
                    Some(SelfKind::Ref) => vec![quote!(&self)],
                    Some(SelfKind::RefMut) => vec![quote!(&mut self)],
                    Some(SelfKind::Value { mutable: true }) => vec![quote!(mut self)],
                    Some(SelfKind::Value { mutable: false }) => vec![quote!(self)],
                };
                quote!(fn #n(#(#self_tok,)* #(#ps),*) #ret;)
            })
            .collect();
        quote!(pub trait #name { #(#ms)* })
    }

    /// 手動 `impl Trait for Type ... end`（逃生艙）
    fn impl_trait_block(&mut self, trait_mangled: &str, type_mangled: &str, methods: &[FunctionDecl]) -> TokenStream {
        let tr = self.type_tokens(trait_mangled);
        let ty = self.type_tokens(type_mangled);
        let (gdecl, guse) = self.impl_header(type_mangled);
        let ms: Vec<_> = methods
            .iter()
            .map(|mth| {
                let mk = self.marker(mth.off);
                let f = self.function(mth, Some(type_mangled));
                quote!(#mk #f)
            })
            .collect();
        quote!(impl #gdecl #tr for #ty #guse { #(#ms)* })
    }

    /// 自動橋接：若某型別的固有方法已覆蓋 trait 的全部方法，
    /// 自動產生 `impl Trait for Type`（委派給固有方法）——LLM 永遠不用手寫 impl。
    /// 已有手動 impl 的 (trait, 型別) 對則跳過；內建特徵走 derive，不管這裡。
    fn auto_bridge(&mut self) -> Vec<TokenStream> {
        // 先快照，避免借用衝突
        let traits: Vec<TraitDecl> = self.traits.values().cloned().collect();
        let mut types: Vec<String> = self.structs.keys().cloned().collect();
        types.extend(self.enums.keys().cloned());
        types.sort();
        let mut out = vec![];
        for tr in &traits {
            // 與自動 derive 衝突的內建名（Clone/PartialEq 系/serde 系）不做橋接，derive 已是證明
            if matches!(
                self.unmangle(&tr.name).as_str(),
                "Clone" | "Equatable" | "Comparable" | "Hashable" | "Serializable" | "Debug"
                    | "Default"
            ) {
                continue;
            }
            for ty in &types {
                if self.manual_impls.contains_key(&(tr.name.clone(), ty.clone())) {
                    continue;
                }
                let inherent = self.inherent_methods.get(ty).cloned().unwrap_or_default();
                let covers = tr.methods.iter().all(|tm| {
                    inherent.iter().any(|im| {
                        im.name == tm.name
                            && im.self_kind.is_some() == tm.self_kind.is_some()
                            && im.params.len() == tm.params.len()
                    })
                });
                if !covers {
                    continue;
                }
                let tr_tok = self.type_tokens(&tr.name);
                let ty_tok = self.type_tokens(ty);
                let (gdecl, guse) = self.impl_header(ty);
                let mut ms = vec![];
                for tm in &tr.methods {
                    let im = inherent.iter().find(|m| m.name == tm.name).cloned().unwrap_or_else(|| FunctionDecl {
                        off: 0,
                        is_async: false,
                        name: tm.name.clone(),
                        generics: vec![],
                        self_kind: tm.self_kind,
                        params: tm.params.clone(),
                        ret: tm.ret.clone(),
                        body: vec![],
                    });
                    let n = id(&tm.name);
                    let mg = self.generics_decl(&im.generics);
                    let ps: Vec<_> = im.params.iter().map(|p| self.param_tokens(p)).collect();
                    let ret = self.ret_tokens(&im.ret);
                    let self_tok: Vec<TokenStream> = match &tm.self_kind {
                        None => vec![],
                        Some(SelfKind::Ref) => vec![quote!(&self)],
                        Some(SelfKind::RefMut) => vec![quote!(&mut self)],
                        Some(SelfKind::Value { mutable: true }) => vec![quote!(mut self)],
                        Some(SelfKind::Value { mutable: false }) => vec![quote!(self)],
                    };
                    // 委派給固有方法（以 Self:: 限定，避開 trait 方法自身的遞迴解析）
                    let fwd_self: Vec<TokenStream> = match &tm.self_kind {
                        None => vec![],
                        _ => vec![quote!(self)],
                    };
                    let arg_names: Vec<Ident> = im.params.iter().map(|p| id(&p.name)).collect();
                    let turbo = if im.generics.is_empty() {
                        quote!()
                    } else {
                        let gs: Vec<Ident> = im.generics.iter().map(|g| id(&g.name)).collect();
                        quote!(::<#(#gs),*>)
                    };
                    ms.push(quote!(
                        fn #n #mg (#(#self_tok,)* #(#ps),*) #ret {
                            Self #turbo :: #n (#(#fwd_self,)* #(#arg_names),*)
                        }
                    ));
                }
                out.push(quote!(impl #gdecl #tr_tok for #ty_tok #guse { #(#ms)* }));
            }
        }
        out
    }

    // ───────────────────────── 作用域 / 型別推導 ─────────────────────────

    fn declare(&mut self, name: &str, t: Type) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), t);
        }
    }

    fn lookup(&self, name: &str) -> Option<Type> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }

    fn gc_vars_in_scope(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = vec![];
        for s in self.scopes.iter().rev() {
            for (k, v) in s {
                if seen.insert(k.clone()) && v.strip_refs().gc && !v.is_ref() {
                    out.push(k.clone());
                }
            }
        }
        out.sort();
        out
    }

    /// 輕量型別推導：只求「夠用來決定 gc 的 borrow / clone / 升級」，推不出來就回 None。
    fn infer(&self, e: &Expr) -> Option<Type> {
        Some(match e {
            Expr::Int(_) => Type::plain(TypeKind::Int),
            Expr::Float(_) => Type::plain(TypeKind::Float),
            Expr::Str(_) => Type::plain(TypeKind::Str),
            Expr::Bool(_) => Type::plain(TypeKind::Bool),
            Expr::Path(p) if p.len() == 1 => self.lookup(&p[0])?,
            Expr::Move(n) => self.lookup(n)?,
            Expr::Borrow { mutable, name } => Type::plain(TypeKind::Ref {
                mutable: *mutable,
                inner: Box::new(self.lookup(name)?),
            }),
            Expr::StructInit { name, .. } => Type::plain(TypeKind::Custom(self.rt(name))),
            Expr::GcAlloc { name, .. } => Type { gc: true, kind: TypeKind::Custom(self.rt(name)) },
            Expr::Path(p) if p.len() >= 2 => match self.resolve_callee(p) {
                Callee::Variant(en, _) => Type::plain(TypeKind::Custom(en)),
                _ => return None,
            },
            Expr::Array(es) => {
                Type::plain(TypeKind::Array(Box::new(self.infer(es.first()?)?)))
            }
            Expr::Field(r, f) => {
                let rt = self.infer(r)?;
                match &rt.strip_refs().kind {
                    TypeKind::Custom(n) => {
                        self.structs.get(&self.rt(n))?.fields.iter().find(|x| &x.name == f)?.ty.clone()
                    }
                    _ => return None,
                }
            }
            Expr::Call(callee, args) => match &**callee {
                Expr::Path(p) => match self.resolve_callee(p) {
                    Callee::Fn(m) => Self::sig_ret(self.fns.get(&m)?),
                    Callee::Method(t, n) => Self::sig_ret(self.methods.get(&format!("{t}::{n}"))?),
                    Callee::Variant(en, _) => Type::plain(TypeKind::Custom(en)),
                    Callee::Unknown => {
                        if p.len() == 1 && p[0] == "Some" && args.len() == 1 {
                            Type::plain(TypeKind::Option(Box::new(self.infer(&args[0])?)))
                        } else {
                            return None;
                        }
                    }
                },
                Expr::Field(recv, m) if m == "clone_to_owned" && args.is_empty() => {
                    let mut t = self.infer(recv)?.strip_refs().clone();
                    t.gc = false;
                    t
                }
                Expr::Field(recv, m) => Self::sig_ret(&self.method_sig(recv, m)?),
                _ => return None,
            },
            Expr::Pipe { name, .. } => {
                let segs: Vec<String> = name.split("::").map(String::from).collect();
                match self.resolve_callee(&segs) {
                    Callee::Fn(m) => Self::sig_ret(self.fns.get(&m)?),
                    _ => return None,
                }
            }
            Expr::Await(inner) => match self.infer(inner)?.kind {
                TypeKind::Future(t) => *t,
                _ => return None,
            },
            Expr::Binary { op, lhs, .. } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => self.infer(lhs)?,
                _ => Type::plain(TypeKind::Bool),
            },
            Expr::Dict(kvs) => {
                let (k, v) = kvs.first()?;
                Type::plain(TypeKind::Dict(Box::new(self.infer(k)?), Box::new(self.infer(v)?)))
            }
            Expr::Unary { op, expr } => match op {
                UnOp::Neg => self.infer(expr)?,
                UnOp::Not => Type::plain(TypeKind::Bool),
            },
            Expr::Index(r, _) => {
                let t = self.infer(r)?;
                match &t.strip_refs().kind {
                    TypeKind::Array(e) => (**e).clone(),
                    TypeKind::Dict(_, v) => (**v).clone(),
                    _ => return None,
                }
            }
            Expr::Lambda { is_async, params, ret, .. } => Type::plain(TypeKind::Closure {
                is_async: *is_async,
                params: params.iter().map(|p| p.ty.clone()).collect(),
                ret: Box::new(ret.clone()),
            }),
            _ => return None,
        })
    }

    fn pat_tokens(&mut self, p: &Pattern) -> TokenStream {
        match p {
            Pattern::Wild => quote!(_),
            Pattern::Bind(n) => {
                let n = id(n);
                quote!(#n)
            }
            Pattern::Int(i) => {
                let l = Literal::i64_unsuffixed(i.abs());
                if *i < 0 { quote!(-#l) } else { quote!(#l) }
            }
            Pattern::Str(s) => quote!(#s),
            Pattern::Bool(b) => quote!(#b),
            Pattern::Variant { path, args } => {
                let ps: Vec<_> = args.iter().map(|a| self.pat_tokens(a)).collect();
                let head = match self.resolve_callee(path) {
                    Callee::Variant(en, v) => {
                        let (en, v) = (id(&en), id(&v));
                        quote!(#en::#v)
                    }
                    _ => {
                        let segs: Vec<Ident> = path.iter().map(|s| id(s)).collect();
                        quote!(#(#segs)::*)
                    }
                };
                if args.is_empty() { head } else { quote!(#head(#(#ps),*)) }
            }
            Pattern::Or(v) => {
                let ps: Vec<_> = v.iter().map(|a| self.pat_tokens(a)).collect();
                quote!(#(#ps)|*)
            }
        }
    }

    /// 把 pattern 綁定的變數（連同由列舉/Option 推得的型別）宣告進目前作用域
    fn bind_pattern(&mut self, p: &Pattern, expected: Option<Type>, by_ref: bool) {
        match p {
            Pattern::Bind(n) => {
                if let Some(t) = expected {
                    // 以參考比對時（`match &x` / `&T` 參數），綁定出來的是參考
                    let t = if by_ref && !t.is_ref() && !t.gc {
                        Type::plain(TypeKind::Ref { mutable: false, inner: Box::new(t) })
                    } else {
                        t
                    };
                    self.declare(n, t);
                }
            }
            Pattern::Variant { path, args } => {
                let payload: Option<Vec<Type>> = match self.resolve_callee(path) {
                    Callee::Variant(en, v) => self
                        .enums
                        .get(&en)
                        .and_then(|d| d.variants.iter().find(|x| x.name == v))
                        .map(|x| x.fields.clone()),
                    _ if path.len() == 1 && path[0] == "Some" => expected
                        .as_ref()
                        .and_then(|t| match &t.strip_refs().kind {
                            TypeKind::Option(i) => Some(vec![(**i).clone()]),
                            _ => None,
                        }),
                    _ => None,
                };
                for (i, a) in args.iter().enumerate() {
                    let t = payload.as_ref().and_then(|v| v.get(i).cloned());
                    self.bind_pattern(a, t, by_ref);
                }
            }
            Pattern::Or(v) => {
                if let Some(f) = v.first() {
                    self.bind_pattern(f, expected, by_ref);
                }
            }
            _ => {}
        }
    }

    fn sig_ret(sig: &FnSig) -> Type {
        if sig.is_async {
            Type::plain(TypeKind::Future(Box::new(sig.ret.clone())))
        } else {
            sig.ret.clone()
        }
    }

    /// 以接收者的（推導）型別找出使用者定義的方法簽章
    fn method_sig(&self, recv: &Expr, m: &str) -> Option<FnSig> {
        let t = self.infer(recv)?;
        match &t.strip_refs().kind {
            TypeKind::Custom(n) => self.methods.get(&format!("{}::{m}", self.rt(n))).cloned(),
            _ => None,
        }
    }

    /// 該表達式（剝掉參考後）是否為 gc 型別
    fn is_gc(&self, e: &Expr) -> bool {
        self.infer(e).map_or(false, |t| t.strip_refs().gc)
    }

    /// 欄位路徑中是否有「穿過 gc 物件」
    fn involves_gc(&self, e: &Expr) -> bool {
        match e {
            Expr::Field(r, _) | Expr::Index(r, _) => self.is_gc(r) || self.involves_gc(r),
            _ => false,
        }
    }

    /// Zero-GC → GC 升級：目標是 `gc T` 而來源是已知的非 gc 值
    fn coerce(&mut self, tokens: TokenStream, from: Option<Type>, to: &Type) -> TokenStream {
        if to.gc {
            if let Some(f) = from {
                if !f.gc && !f.is_ref() {
                    self.touched_gc = true;
                    return quote!(::gc::Gc::new(::gc::GcCell::new(#tokens)));
                }
            }
        }
        tokens
    }

    // ───────────────────────── 陳述式 ─────────────────────────

    fn stmts(&mut self, body: &[Stmt]) -> TokenStream {
        let v: Vec<_> = body.iter().map(|s| self.stmt(s)).collect();
        quote!(#(#v)*)
    }

    fn scoped_block(&mut self, body: &[Stmt]) -> TokenStream {
        self.scopes.push(HashMap::new());
        let t = self.stmts(body);
        self.scopes.pop();
        t
    }

    fn stmt(&mut self, s: &Stmt) -> TokenStream {
        match s {
            Stmt::At(off, inner) => {
                let mk = self.marker(*off);
                let t = self.stmt(inner);
                quote!(#mk #t)
            }
            Stmt::Match { scrut, arms } => {
                let sty = self.infer(scrut);
                let is_gc = sty.as_ref().map_or(false, |t| t.strip_refs().gc);
                let is_str = matches!(&sty, Some(t) if matches!(t.strip_refs().kind, TypeKind::Str));
                let s = if is_gc {
                    // gc 值：以快照（clone）比對，避免比對期間持有 RefCell 借用
                    let h = self.gc_handle(scrut);
                    quote!(&{ let __aero_s = (*#h.borrow()).clone(); __aero_s })
                } else if is_str {
                    let e = self.expr(scrut);
                    quote!((#e).as_str())
                } else {
                    self.expr(scrut)
                };
                let by_ref = is_gc || sty.as_ref().map_or(false, |t| t.is_ref());
                let expected = sty.as_ref().map(|t| {
                    let mut x = t.strip_refs().clone();
                    x.gc = false;
                    x
                });
                let arms_t: Vec<_> = arms
                    .iter()
                    .map(|a| {
                        self.scopes.push(HashMap::new());
                        self.bind_pattern(&a.pat, expected.clone(), by_ref);
                        let p = self.pat_tokens(&a.pat);
                        let g = a.guard.as_ref().map(|g| {
                            let g = self.expr(g);
                            quote!(if #g)
                        });
                        let b = self.stmts(&a.body);
                        self.scopes.pop();
                        quote!(#p #g => { #b })
                    })
                    .collect();
                quote!(match #s { #(#arms_t)* })
            }
            Stmt::If { branches, else_body } => {
                let mut out = quote!();
                for (i, (c, b)) in branches.iter().enumerate() {
                    let c = self.expr(c);
                    let b = self.scoped_block(b);
                    out = if i == 0 {
                        quote!(if #c { #b })
                    } else {
                        quote!(#out else if #c { #b })
                    };
                }
                if let Some(b) = else_body {
                    let b = self.scoped_block(b);
                    out = quote!(#out else { #b });
                }
                out
            }
            Stmt::Let { mutable, name, ty, value } => {
                let inferred = self.infer(value);
                let mut v = self.expr(value);
                let mut var_ty = ty.clone().or_else(|| inferred.clone());
                if let Some(t) = ty {
                    v = self.coerce(v, inferred.clone(), t);
                } else if self.managed > 0 && matches!(value, Expr::Call(..) | Expr::Pipe { .. }) {
                    // Zero-GC 函數回傳值 → managed 變數：自動升級為 GC 記憶體
                    if let Some(t) = &inferred {
                        if !t.gc && matches!(&t.kind, TypeKind::Custom(n) if self.type_names.contains(n)) {
                            let gt = Type { gc: true, kind: t.kind.clone() };
                            v = self.coerce(v, inferred.clone(), &gt);
                            var_ty = Some(gt);
                        }
                    }
                }
                let n = id(name);
                let m = if *mutable { quote!(mut) } else { quote!() };
                let tt = ty.as_ref().map(|t| {
                    let t = self.ty(t);
                    quote!(: #t)
                });
                if let Some(t) = var_ty {
                    self.declare(name, t);
                }
                quote!(let #m #n #tt = #v;)
            }
            Stmt::Assign { target, value } => {
                let from = self.infer(value);
                let mut rhs = self.expr(value);
                if let Some(tt) = self.infer(target) {
                    rhs = self.coerce(rhs, from, &tt);
                }
                if self.involves_gc(target) {
                    // 先算右值與索引，再取 borrow_mut，避免 `a.v = a.v + 1` 的 RefCell 重入 panic
                    let mut pre = vec![];
                    let t2 = self.hoist_indices(target, &mut pre);
                    let lhs = self.place(&t2);
                    quote!({ #(#pre)* let __aero_rhs = #rhs; #lhs = __aero_rhs; })
                } else {
                    let lhs = self.place(target);
                    quote!(#lhs = #rhs;)
                }
            }
            Stmt::Managed(body) => {
                self.managed += 1;
                let b = self.scoped_block(body);
                self.managed -= 1;
                quote!({ #b })
            }
            Stmt::Unsafe(body) => {
                let b = self.scoped_block(body);
                quote!(unsafe { #b })
            }
            Stmt::For { var, iter, body } => {
                let it_ty = self.infer(iter);
                let (it, elem) = self.for_iter(iter, it_ty);
                self.scopes.push(HashMap::new());
                if let Some(t) = elem {
                    self.declare(var, t);
                }
                let b = self.stmts(body);
                self.scopes.pop();
                let v = id(var);
                quote!(for #v in #it { #b })
            }
            Stmt::While { cond, body } => {
                let c = self.expr(cond);
                let b = self.scoped_block(body);
                quote!(while #c { #b })
            }
            Stmt::Return(e) => match e {
                Some(e) => {
                    let from = self.infer(e);
                    let mut t = self.expr(e);
                    if let Some(rt) = self.ret_stack.last().cloned() {
                        t = self.coerce(t, from, &rt);
                    }
                    quote!(return #t;)
                }
                None => quote!(return;),
            },
            Stmt::Expr(e) => {
                let t = self.expr(e);
                quote!(#t;)
            }
        }
    }

    /// `for x in <iter>`：gc 容器先 snapshot（避免迴圈期間持有 RefCell 借用）
    fn for_iter(&mut self, iter: &Expr, ty: Option<Type>) -> (TokenStream, Option<Type>) {
        let elem_of = |t: &Type| match &t.strip_refs().kind {
            TypeKind::Array(e) => Some((**e).clone()),
            _ => None,
        };
        let elem = ty.as_ref().and_then(elem_of);
        let it = if ty.as_ref().map_or(false, |t| t.strip_refs().gc) {
            let name = match iter {
                Expr::Borrow { name, .. } | Expr::Move(name) => Some(name.clone()),
                Expr::Path(p) if p.len() == 1 => Some(p[0].clone()),
                _ => None,
            };
            match name {
                Some(n) => {
                    let n = id(&n);
                    quote!({ let __aero_it = #n.borrow().clone(); __aero_it })
                }
                None => self.expr(iter),
            }
        } else {
            self.expr(iter)
        };
        (it, elem)
    }

    // ───────────────────────── 欄位存取 / 借用 ─────────────────────────

    /// 取得 gc 物件的「控制代碼」（Gc 指標本身），不會讓 RefCell 借用跨出該表達式
    fn gc_handle(&mut self, e: &Expr) -> TokenStream {
        match e {
            Expr::Path(p) if p.len() == 1 => {
                let n = id(&p[0]);
                quote!(#n)
            }
            Expr::Borrow { name, .. } | Expr::Move(name) => {
                let n = id(name);
                quote!(#n)
            }
            Expr::Field(..) => {
                let c = self.chain(e, false);
                quote!({ let __aero_h = #c.clone(); __aero_h })
            }
            other => {
                let t = self.expr(other);
                quote!((#t))
            }
        }
    }

    /// 欄位路徑（不做最後的 clone）。`last_mut`：最後一次穿越 gc 時使用 borrow_mut。
    fn chain(&mut self, e: &Expr, last_mut: bool) -> TokenStream {
        match e {
            Expr::Field(r, f) => {
                let f = id(f);
                if self.is_gc(r) {
                    let h = self.gc_handle(r);
                    if last_mut {
                        quote!(#h.borrow_mut().#f)
                    } else {
                        quote!(#h.borrow().#f)
                    }
                } else {
                    let c = self.chain(r, last_mut);
                    quote!(#c.#f)
                }
            }
            Expr::Index(r, i) => {
                let ix = self.index_tokens(r, i);
                if self.is_gc(r) {
                    let h = self.gc_handle(r);
                    if last_mut {
                        quote!(#h.borrow_mut()[#ix])
                    } else {
                        quote!(#h.borrow()[#ix])
                    }
                } else {
                    let c = self.chain(r, last_mut);
                    quote!(#c[#ix])
                }
            }
            Expr::Path(p) if p.len() == 1 => {
                let n = id(&p[0]);
                quote!(#n)
            }
            other => self.expr(other),
        }
    }

    fn index_tokens(&mut self, recv: &Expr, idx: &Expr) -> TokenStream {
        let is_dict = matches!(self.infer(recv), Some(t) if matches!(t.strip_refs().kind, TypeKind::Dict(..)));
        let i = self.expr(idx);
        if is_dict { quote!(&#i) } else { quote!((#i) as usize) }
    }

    /// 把賦值目標中的索引運算式先算成暫存變數
    fn hoist_indices(&mut self, e: &Expr, pre: &mut Vec<TokenStream>) -> Expr {
        match e {
            Expr::Index(r, i) => {
                let r2 = self.hoist_indices(r, pre);
                let ity = self.infer(i);
                let tok = self.expr(i);
                self.tmp += 1;
                let name = format!("__aero_ix{}", self.tmp);
                let ident = id(&name);
                pre.push(quote!(let #ident = #tok;));
                if let Some(t) = ity {
                    self.declare(&name, t);
                }
                Expr::Index(Box::new(r2), Box::new(Expr::Path(vec![name])))
            }
            Expr::Field(r, f) => Expr::Field(Box::new(self.hoist_indices(r, pre)), f.clone()),
            other => other.clone(),
        }
    }

    /// 賦值左側（place expression）
    fn place(&mut self, e: &Expr) -> TokenStream {
        self.chain(e, true)
    }

    // ───────────────────────── 表達式 ─────────────────────────

    fn path_tokens(&self, p: &[String]) -> TokenStream {
        let joined = p.join("::");
        if joined == "gc::collect" || joined == "std::gc::collect" {
            return quote!(::gc::force_collect);
        }
        let segs: Vec<Ident> = p.iter().map(|s| id(s)).collect();
        quote!(#(#segs)::*)
    }

    fn fields_tokens(&mut self, name: &str, fields: &[(String, Expr)]) -> TokenStream {
        let name = self.rt(name);
        let decl = self.structs.get(&name).cloned();
        let inits: Vec<_> = fields
            .iter()
            .map(|(f, e)| {
                let from = self.infer(e);
                let mut v = self.expr(e);
                if let Some(d) = &decl {
                    if let Some(fd) = d.fields.iter().find(|x| &x.name == f) {
                        v = self.coerce(v, from, &fd.ty);
                    }
                }
                let f = id(f);
                quote!(#f: #v)
            })
            .collect();
        let n = self.type_tokens(&name);
        quote!(#n { #(#inits),* })
    }

    fn expr(&mut self, e: &Expr) -> TokenStream {
        match e {
            Expr::Int(n) => {
                let l = Literal::i64_suffixed(*n);
                quote!(#l)
            }
            Expr::Float(f) => {
                let l = Literal::f64_suffixed(*f);
                quote!(#l)
            }
            Expr::Str(s) => quote!(::std::string::String::from(#s)),
            Expr::Bool(b) => quote!(#b),
            Expr::Path(p) => {
                if p.len() >= 2 {
                    match self.resolve_callee(p) {
                        Callee::Variant(en, v) => {
                            let (en, v) = (id(&en), id(&v));
                            return quote!(#en::#v);
                        }
                        Callee::Fn(m) => {
                            let m = id(&m);
                            return quote!(#m);
                        }
                        _ => {}
                    }
                }
                let t = self.path_tokens(p);
                if p.len() == 1 {
                    if let Some(Type { kind: TypeKind::Ref { inner, .. }, .. }) = self.lookup(&p[0]) {
                        if is_copy(&inner) {
                            return quote!((*#t));
                        }
                    }
                }
                // GC 模式：把 gc 變數當值使用 = 共享（複製控制代碼）；要轉移請用 `move`
                if p.len() == 1 && self.lookup(&p[0]).map_or(false, |t| t.gc && !t.is_ref()) {
                    quote!(#t.clone())
                } else {
                    t
                }
            }
            Expr::Move(n) => {
                let n = id(n);
                quote!(#n)
            }
            Expr::Borrow { mutable, name } => {
                let n = id(name);
                if *mutable { quote!(&mut #n) } else { quote!(&#n) }
            }
            Expr::StructInit { name, fields } => self.fields_tokens(name, fields),
            Expr::GcAlloc { name, fields } => {
                self.touched_gc = true;
                let r = self.rt(name);
                self.gc_used.insert(r);
                let s = self.fields_tokens(name, fields);
                quote!(::gc::Gc::new(::gc::GcCell::new(#s)))
            }
            Expr::Array(es) => {
                let v: Vec<_> = es.iter().map(|e| self.expr(e)).collect();
                quote!(::std::vec![#(#v),*])
            }
            Expr::Dict(kvs) => {
                if kvs.is_empty() {
                    quote!(::std::collections::HashMap::new())
                } else {
                    let v: Vec<_> = kvs
                        .iter()
                        .map(|(k, v)| {
                            let (k, v) = (self.expr(k), self.expr(v));
                            quote!((#k, #v))
                        })
                        .collect();
                    quote!(::std::collections::HashMap::from([#(#v),*]))
                }
            }
            Expr::Lambda { is_async, params, ret, body } => self.lambda(*is_async, params, ret, body),
            Expr::Binary { op, lhs, rhs } => {
                let is_str = matches!(self.infer(lhs), Some(t) if matches!(t.strip_refs().kind, TypeKind::Str));
                let (l, r) = (self.expr(lhs), self.expr(rhs));
                match op {
                    BinOp::Add if is_str => quote!(::std::format!("{}{}", #l, #r)),
                    BinOp::Add => quote!((#l + #r)),
                    BinOp::Sub => quote!((#l - #r)),
                    BinOp::Mul => quote!((#l * #r)),
                    BinOp::Div => quote!((#l / #r)),
                    BinOp::Rem => quote!((#l % #r)),
                    BinOp::Lt => quote!((#l < #r)),
                    BinOp::Le => quote!((#l <= #r)),
                    BinOp::Gt => quote!((#l > #r)),
                    BinOp::Ge => quote!((#l >= #r)),
                    BinOp::Eq => quote!((#l == #r)),
                    BinOp::Ne => quote!((#l != #r)),
                    BinOp::And => quote!((#l && #r)),
                    BinOp::Or => quote!((#l || #r)),
                }
            }
            // x |> f(a)  ⇒  f(x, a)（f 為已知函式）；否則 ⇒ x.f(a)（方法鏈）
            Expr::Pipe { lhs, name, args } => {
                let rest = args.clone().unwrap_or_default();
                let segs: Vec<String> = name.split("::").map(String::from).collect();
                let call = if !matches!(self.resolve_callee(&segs), Callee::Unknown) {
                    let mut all = vec![(**lhs).clone()];
                    all.extend(rest);
                    Expr::Call(Box::new(Expr::Path(segs)), all)
                } else {
                    Expr::Call(Box::new(Expr::Field(lhs.clone(), name.clone())), rest)
                };
                self.expr(&call)
            }
            Expr::Await(inner) => {
                let t = self.expr(inner);
                quote!((#t).await)
            }
            Expr::Field(r, f) => {
                let c = self.chain(e, false);
                // 穿過 gc 讀取非 Copy 欄位 → clone（gc 欄位 clone = 共享控制代碼）
                let _ = (r, f);
                if self.involves_gc(e) {
                    match self.infer(e) {
                        Some(t) if !is_copy(&t) => quote!(#c.clone()),
                        _ => c,
                    }
                } else {
                    c
                }
            }
            Expr::Index(..) => {
                let c = self.chain(e, false);
                match self.infer(e) {
                    Some(t) if !is_copy(&t) => quote!(#c.clone()),
                    _ => c,
                }
            }
            Expr::Unary { op, expr } => {
                let t = self.expr(expr);
                match op {
                    UnOp::Neg => quote!((-#t)),
                    UnOp::Not => quote!((!#t)),
                }
            }
            Expr::Call(callee, args) => self.call(callee, args),
        }
    }

    fn call_args(&mut self, args: &[Expr], params: Option<&[Param]>) -> Vec<TokenStream> {
        args.iter()
            .enumerate()
            .map(|(i, a)| {
                let pt = params.and_then(|p| p.get(i)).map(|p| p.ty.clone());
                // GC → Zero-GC（Borrow）：`&gc_var` 餵給要 `&T` 的函式 ⇒ Auto-Deref
                if let (Expr::Borrow { mutable, name }, Some(pt)) = (a, &pt) {
                    let var_is_gc = self.lookup(name).map_or(false, |t| t.gc && !t.is_ref());
                    let wants_plain_ref =
                        matches!(&pt.kind, TypeKind::Ref { inner, .. } if !inner.gc);
                    if var_is_gc && wants_plain_ref {
                        let n = id(name);
                        return if *mutable {
                            quote!(&mut *#n.borrow_mut())
                        } else {
                            quote!(&*#n.borrow())
                        };
                    }
                }
                if let (Expr::Path(p), Some(pt)) = (a, &pt) {
                    if p.len() == 1 && pt.is_ref() && self.lookup(&p[0]).map_or(false, |t| t.is_ref()) {
                        let n = id(&p[0]);
                        return quote!(#n);
                    }
                }
                let from = self.infer(a);
                let t = self.expr(a);
                match &pt {
                    Some(pt) => self.coerce(t, from, pt),
                    None => t,
                }
            })
            .collect()
    }

    fn call(&mut self, callee: &Expr, args: &[Expr]) -> TokenStream {
        match callee {
            Expr::Path(p) => match self.resolve_callee(p) {
                Callee::Fn(m) => {
                    let sig = self.fns.get(&m).cloned();
                    let a = self.call_args(args, sig.as_ref().map(|s| s.params.as_slice()));
                    let f = id(&m);
                    let call = quote!(#f(#(#a),*));
                    if self.externs.contains(&m) { quote!(unsafe { #call }) } else { call }
                }
                Callee::Method(t, n) => {
                    let sig = self.methods.get(&format!("{t}::{n}")).cloned();
                    let a = self.call_args(args, sig.as_ref().map(|s| s.params.as_slice()));
                    let (t, n) = (self.type_tokens(&t), id(&n));
                    quote!(#t::#n(#(#a),*))
                }
                Callee::Variant(en, v) => {
                    let params: Vec<Param> = self
                        .enums
                        .get(&en)
                        .and_then(|d| d.variants.iter().find(|x| x.name == v))
                        .map(|x| {
                            x.fields
                                .iter()
                                .map(|t| Param { mutable: false, name: String::new(), ty: t.clone() })
                                .collect()
                        })
                        .unwrap_or_default();
                    let a = self.call_args(args, Some(&params));
                    let (en, v) = (id(&en), id(&v));
                    quote!(#en::#v(#(#a),*))
                }
                Callee::Unknown => {
                    let a = self.call_args(args, None);
                    let f = self.path_tokens(p);
                    quote!(#f(#(#a),*))
                }
            },
            Expr::Field(recv, m) => self.method_call(recv, m, args),
            other => {
                let c = self.expr(other);
                let a = self.call_args(args, None);
                quote!((#c)(#(#a),*))
            }
        }
    }

    fn method_call(&mut self, recv: &Expr, m: &str, args: &[Expr]) -> TokenStream {
        // 跨模式：gc T → T 的所有權轉移必須顯式 `.clone_to_owned()`
        if m == "clone_to_owned" && args.is_empty() {
            return if self.is_gc(recv) {
                let h = self.gc_handle(recv);
                quote!((*#h.borrow()).clone())
            } else {
                let r = self.expr(recv);
                quote!(#r.clone())
            };
        }
        let mid = id(m);
        let sig = self.method_sig(recv, m);
        let a = self.call_args(args, sig.as_ref().map(|s| s.params.as_slice()));
        // 已知方法：依 &self / &mut self 決定借用種類；未知方法才退回名單
        let readonly = match &sig {
            Some(s) => !matches!(s.self_kind, Some(SelfKind::RefMut)),
            None => READONLY_METHODS.contains(&m),
        };
        let cast = USIZE_METHODS.contains(&m);

        let (recv_tokens, hoist) = if self.is_gc(recv) {
            let h = self.gc_handle(recv);
            let b = if readonly { quote!(borrow) } else { quote!(borrow_mut) };
            (quote!(#h.#b()), true)
        } else if self.involves_gc(recv) {
            (self.chain(recv, !readonly), true)
        } else if matches!(recv, Expr::Path(_) | Expr::Field(..) | Expr::Index(..)) {
            (self.chain(recv, false), false)
        } else {
            (self.expr(recv), false)
        };

        let call = if hoist {
            // 先求值參數再取得 RefCell 借用，避免重入 panic
            let names: Vec<Ident> = (0..a.len()).map(|i| format_ident!("__aero_a{}", i)).collect();
            quote!({ #(let #names = #a;)* #recv_tokens.#mid(#(#names),*) })
        } else {
            quote!(#recv_tokens.#mid(#(#a),*))
        };
        if cast { quote!((#call as i64)) } else { call }
    }

    fn lambda(&mut self, is_async: bool, params: &[Param], ret: &Type, body: &[Stmt]) -> TokenStream {
        // 逃逸閉包：managed 內捕獲到的 gc 變數以「複製控制代碼」方式進入閉包
        let shadows: Vec<_> = if self.managed > 0 {
            self.gc_vars_in_scope()
                .iter()
                .map(|n| {
                    let n = id(n);
                    quote!(let #n = #n.clone();)
                })
                .collect()
        } else {
            vec![]
        };

        self.scopes.push(params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect());
        self.ret_stack.push(ret.clone());
        let b = self.stmts(body);
        self.ret_stack.pop();
        self.scopes.pop();

        let ps: Vec<_> = params.iter().map(|p| self.param_tokens(p)).collect();
        let r = self.ty(ret);
        let closure = if is_async {
            quote!(::std::rc::Rc::new(move |#(#ps),*| -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = #r>>> {
                ::std::boxed::Box::pin(async move { #b })
            }))
        } else {
            quote!(::std::rc::Rc::new(move |#(#ps),*| -> #r { #b }))
        };
        if shadows.is_empty() {
            closure
        } else {
            quote!({ #(#shadows)* #closure })
        }
    }
}
