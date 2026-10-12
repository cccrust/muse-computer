//! 以 chumsky 0.9 組合子直接實作規格書的 EBNF（scannerless，無獨立 Lexer）。

use crate::ast::*;
use chumsky::prelude::*;

type E = Simple<char>;

const KEYWORDS: &[&str] = &[
    "import", "crate", "extern", "struct", "gc", "fn", "async", "await", "let", "mut", "managed",
    "unsafe", "for", "in", "while", "return", "move", "end", "true", "false", "if", "else", "impl", "as", "match", "case", "enum",
    "trait", "for",
];

// ───────────────────────── 詞法級輔助 ─────────────────────────

/// 空白 + `// 行註解`
type B<T> = BoxedParser<'static, char, T, E>;

fn ws() -> B<()> {
    let comment = just::<_, _, E>("//")
        .then(take_until(just('\n').ignored().or(end())))
        .ignored();
    filter(|c: &char| c.is_whitespace())
        .ignored()
        .or(comment)
        .repeated()
        .ignored()
        .boxed()
}

fn sym(s: &'static str) -> B<()> {
    just::<char, _, E>(s).ignored().padded_by(ws()).boxed()
}

fn kw(s: &'static str) -> B<()> {
    text::keyword::<_, _, E>(s).padded_by(ws()).boxed()
}

/// 單獨的 `=`（不是 `==`）
fn assign_op() -> B<()> {
    just::<char, _, E>('=')
        .then_ignore(just('=').not().rewind())
        .ignored()
        .padded_by(ws())
        .boxed()
}

fn ident() -> B<String> {
    text::ident::<char, E>()
        .try_map(|s: String, span| {
            if KEYWORDS.contains(&s.as_str()) {
                Err(Simple::custom(span, format!("`{s}` 是保留字，不能當識別字")))
            } else {
                Ok(s)
            }
        })
        .padded_by(ws())
        .boxed()
}

fn string_lit() -> B<String> {
    let escape = just('\\').ignore_then(choice((
        just('\\'),
        just('"'),
        just('n').to('\n'),
        just('t').to('\t'),
        just('r').to('\r'),
    )));
    just('"')
        .ignore_then(
            filter(|c: &char| *c != '"' && *c != '\\')
                .or(escape)
                .repeated()
                .collect::<String>(),
        )
        .then_ignore(just('"'))
        .padded_by(ws())
        .boxed()
}

/// `(Identifier "::")* Identifier`，允許 `gc::collect` 這種以 gc 開頭的路徑
fn path() -> B<Vec<String>> {
    let seg = ident().or(text::keyword::<_, _, E>("gc").to("gc".to_string()).padded_by(ws()));
    seg.then_ignore(sym("::"))
        .repeated()
        .then(ident())
        .map(|(mut head, last)| {
            head.push(last);
            head
        })
        .boxed()
}

// ───────────────────────── 型別 ─────────────────────────

fn type_parser() -> B<Type> {
    recursive(|ty: Recursive<char, Type, E>| {
        let kind = choice((
            kw("Int").to(TypeKind::Int),
            kw("Float").to(TypeKind::Float),
            kw("String").to(TypeKind::Str),
            kw("Bool").to(TypeKind::Bool),
            kw("Unit").to(TypeKind::Unit),
            kw("RawPointer").to(TypeKind::RawPointer),
            kw("Array")
                .ignore_then(ty.clone().delimited_by(sym("["), sym("]")))
                .map(|t| TypeKind::Array(Box::new(t))),
            kw("Dict")
                .ignore_then(
                    ty.clone()
                        .then_ignore(sym(","))
                        .then(ty.clone())
                        .delimited_by(sym("["), sym("]")),
                )
                .map(|(k, v)| TypeKind::Dict(Box::new(k), Box::new(v))),
            kw("Option")
                .ignore_then(ty.clone().delimited_by(sym("["), sym("]")))
                .map(|t| TypeKind::Option(Box::new(t))),
            kw("Future")
                .ignore_then(ty.clone().delimited_by(sym("["), sym("]")))
                .map(|t| TypeKind::Future(Box::new(t))),
            sym("&")
                .ignore_then(kw("mut").or_not())
                .then(ty.clone())
                .map(|(m, t)| TypeKind::Ref { mutable: m.is_some(), inner: Box::new(t) }),
            kw("async")
                .or_not()
                .then_ignore(kw("fn"))
                .then(
                    ty.clone()
                        .separated_by(sym(","))
                        .delimited_by(sym("("), sym(")")),
                )
                .then_ignore(sym("->"))
                .then(ty.clone())
                .map(|((a, params), ret)| TypeKind::Closure {
                    is_async: a.is_some(),
                    params,
                    ret: Box::new(ret),
                }),
            // 自訂泛型具現 `Pair[Int, String]`（內建 Array/Dict/Option/Future 已在上方優先匹配）
            path()
                .then(
                    ty.clone()
                        .separated_by(sym(","))
                        .delimited_by(sym("["), sym("]"))
                        .or_not(),
                )
                .map(|(p, args)| match args {
                    Some(a) if !a.is_empty() => TypeKind::Generic { base: p.join("::"), args: a },
                    _ => TypeKind::Custom(p.join("::")),
                }),
        ));
        kw("gc")
            .or_not()
            .then(kind)
            .map(|(g, kind)| Type { gc: g.is_some(), kind })
    })
    .boxed()
}

fn param_parser(ty: B<Type>) -> B<Param> {
    kw("mut")
        .or_not()
        .then(ident())
        .then_ignore(sym(":"))
        .then(ty)
        .map(|((m, name), ty)| Param { mutable: m.is_some(), name, ty })
        .boxed()
}

// ───────────────────────── Pattern ─────────────────────────

fn pattern_parser() -> B<Pattern> {
    recursive(|pat: Recursive<char, Pattern, E>| {
        let digits = filter(|c: &char| c.is_ascii_digit())
            .repeated()
            .at_least(1)
            .collect::<String>()
            .padded_by(ws());
        let int = sym("-")
            .or_not()
            .then(digits)
            .try_map(|(neg, s), span| {
                s.parse::<i64>()
                    .map(|n| Pattern::Int(if neg.is_some() { -n } else { n }))
                    .map_err(|_| Simple::custom(span, "整數字面值超出範圍"))
            });
        let atom = choice((
            kw("true").to(Pattern::Bool(true)).boxed(),
            kw("false").to(Pattern::Bool(false)).boxed(),
            string_lit().map(Pattern::Str).boxed(),
            int.boxed(),
            path()
                .then(
                    pat.clone()
                        .separated_by(sym(","))
                        .delimited_by(sym("("), sym(")"))
                        .or_not(),
                )
                .map(|(p, args)| match (p.len(), args) {
                    (1, None) if p[0] == "_" => Pattern::Wild,
                    (1, None) if p[0] == "None" => Pattern::Variant { path: p, args: vec![] },
                    (1, None) => Pattern::Bind(p[0].clone()),
                    (_, args) => Pattern::Variant { path: p, args: args.unwrap_or_default() },
                })
                .boxed(),
        ));
        atom.separated_by(sym("|"))
            .at_least(1)
            .map(|mut v| if v.len() == 1 { v.remove(0) } else { Pattern::Or(v) })
    })
    .boxed()
}

// ───────────────────────── 表達式 / 陳述式 ─────────────────────────

#[derive(Clone)]
enum Suffix {
    Await,
    Field(String),
    Call(Vec<Expr>),
    Index(Expr),
}

fn is_place(e: &Expr) -> bool {
    match e {
        Expr::Path(_) => true,
        Expr::Field(inner, _) => is_place(inner),
        Expr::Index(inner, _) => is_place(inner),
        _ => false,
    }
}

fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
    Expr::Binary { op, lhs: Box::new(l), rhs: Box::new(r) }
}

fn expr_stmt_parsers(ty: B<Type>, pat: B<Pattern>) -> (
    Recursive<'static, char, Expr, E>,
    Recursive<'static, char, Stmt, E>,
) {
    let mut expr = Recursive::<char, Expr, E>::declare();
    let mut stmt = Recursive::<char, Stmt, E>::declare();

    let params = param_parser(ty.clone()).separated_by(sym(","));
    let args = expr
        .clone()
        .separated_by(sym(","))
        .allow_trailing()
        .delimited_by(sym("("), sym(")"));

    // ── Literal ──
    let digits = filter(|c: &char| c.is_ascii_digit())
        .repeated()
        .at_least(1)
        .collect::<String>();
    let float = digits
        .clone()
        .then_ignore(just('.'))
        .then(digits.clone())
        .try_map(|(a, b), span| {
            format!("{a}.{b}")
                .parse::<f64>()
                .map(Expr::Float)
                .map_err(|_| Simple::custom(span, "非法的浮點數字面值"))
        })
        .padded_by(ws());
    let int = digits
        .try_map(|s, span| {
            s.parse::<i64>()
                .map(Expr::Int)
                .map_err(|_| Simple::custom(span, "整數字面值超出 Int(i64) 範圍"))
        })
        .padded_by(ws());
    let literal = choice((
        float.boxed(),
        int.boxed(),
        string_lit().map(Expr::Str).boxed(),
        kw("true").to(Expr::Bool(true)).boxed(),
        kw("false").to(Expr::Bool(false)).boxed(),
    ))
    .boxed();

    // ── Primary ──
    let mv = kw("move").ignore_then(ident()).map(Expr::Move).boxed();
    let borrow = sym("&")
        .ignore_then(kw("mut").or_not())
        .then(ident())
        .map(|(m, name)| Expr::Borrow { mutable: m.is_some(), name })
        .boxed();

    let struct_body = ident()
        .then_ignore(sym(":"))
        .then(expr.clone())
        .separated_by(sym(","))
        .allow_trailing()
        .delimited_by(sym("{"), sym("}"))
        .boxed();
    let gc_alloc = kw("gc")
        .ignore_then(path().map(|p| p.join("::")).then(struct_body.clone()))
        .map(|(name, fields)| Expr::GcAlloc { name, fields })
        .boxed();
    let struct_init = path()
        .map(|p| p.join("::"))
        .then(struct_body)
        .map(|(name, fields)| Expr::StructInit { name, fields })
        .boxed();

    let array = expr
        .clone()
        .separated_by(sym(","))
        .allow_trailing()
        .delimited_by(sym("["), sym("]"))
        .map(Expr::Array)
        .boxed();
    let dict = expr
        .clone()
        .then_ignore(sym(":"))
        .then(expr.clone())
        .separated_by(sym(","))
        .allow_trailing()
        .delimited_by(sym("{"), sym("}"))
        .map(Expr::Dict)
        .boxed();

    // Lambda：`|x: Int| -> Int  x + 1 end`  或  `... stmts ... end`
    let lambda_body = expr
        .clone()
        .then_ignore(kw("end"))
        .map(|e| vec![Stmt::Return(Some(e))])
        .or(stmt.clone().repeated().then_ignore(kw("end")))
        .boxed();
    let lambda = kw("async")
        .or_not()
        .then_ignore(sym("|"))
        .then(params)
        .then_ignore(sym("|"))
        .then_ignore(sym("->"))
        .then(ty.clone())
        .then(lambda_body)
        .map(|(((a, params), ret), body)| Expr::Lambda {
            is_async: a.is_some(),
            params,
            ret,
            body,
        })
        .boxed();

    let primary = choice((
        mv,
        borrow,
        gc_alloc,
        literal,
        lambda,
        struct_init,
        path().map(Expr::Path).boxed(),
        array,
        dict,
        expr.clone().delimited_by(sym("("), sym(")")).boxed(),
    ))
    .boxed();

    // ── Postfix ──
    let args = args.boxed();
    let suffix = choice((
        sym(".").ignore_then(kw("await")).to(Suffix::Await).boxed(),
        sym(".").ignore_then(ident()).map(Suffix::Field).boxed(),
        args.clone().map(Suffix::Call).boxed(),
        expr.clone().delimited_by(sym("["), sym("]")).map(Suffix::Index).boxed(),
    ))
    .boxed();
    let postfix = primary
        .then(suffix.repeated())
        .foldl(|lhs, s| match s {
            Suffix::Await => Expr::Await(Box::new(lhs)),
            Suffix::Field(f) => Expr::Field(Box::new(lhs), f),
            Suffix::Call(a) => Expr::Call(Box::new(lhs), a),
            Suffix::Index(i) => Expr::Index(Box::new(lhs), Box::new(i)),
        })
        .boxed();

    // ── 二元運算（優先序低→高：|> , || , ==/!= , 比較 , +- , */%）──
    let unary = choice((
        sym("-").to(UnOp::Neg).boxed(),
        sym("!").to(UnOp::Not).boxed(),
    ))
    .repeated()
    .then(postfix)
    .foldr(|op, e| Expr::Unary { op, expr: Box::new(e) })
    .boxed();
    let mul = unary
        .clone()
        .then(
            choice((
                sym("*").to(BinOp::Mul).boxed(),
                sym("/").to(BinOp::Div).boxed(),
                sym("%").to(BinOp::Rem).boxed(),
            ))
            .then(unary)
            .repeated(),
        )
        .foldl(|l, (op, r)| bin(op, l, r))
        .boxed();
    let add = mul
        .clone()
        .then(
            choice((sym("+").to(BinOp::Add).boxed(), sym("-").to(BinOp::Sub).boxed()))
                .then(mul)
                .repeated(),
        )
        .foldl(|l, (op, r)| bin(op, l, r))
        .boxed();
    let rel = add
        .clone()
        .then(
            choice((
                sym("<=").to(BinOp::Le).boxed(),
                sym(">=").to(BinOp::Ge).boxed(),
                sym("<").to(BinOp::Lt).boxed(),
                sym(">").to(BinOp::Gt).boxed(),
            ))
            .then(add)
            .repeated(),
        )
        .foldl(|l, (op, r)| bin(op, l, r))
        .boxed();
    let eq = rel
        .clone()
        .then(
            choice((sym("==").to(BinOp::Eq).boxed(), sym("!=").to(BinOp::Ne).boxed()))
                .then(rel)
                .repeated(),
        )
        .foldl(|l, (op, r)| bin(op, l, r))
        .boxed();
    let and = eq
        .clone()
        .then(sym("&&").to(BinOp::And).boxed().then(eq).repeated())
        .foldl(|l, (op, r)| bin(op, l, r))
        .boxed();
    let or = and
        .clone()
        .then(sym("||").to(BinOp::Or).boxed().then(and).repeated())
        .foldl(|l, (op, r)| bin(op, l, r))
        .boxed();
    let pipeline = or
        .then(sym("|>").ignore_then(path().map(|p| p.join("::"))).then(args.or_not()).repeated())
        .foldl(|lhs, (name, args)| Expr::Pipe { lhs: Box::new(lhs), name, args })
        .boxed();

    expr.define(pipeline);

    // ── Statements ──
    let block = stmt.clone().repeated();
    let braces = |p: Recursive<'static, char, Stmt, E>| {
        p.repeated().delimited_by(sym("{"), sym("}")).boxed()
    };

    let let_stmt = kw("let")
        .ignore_then(kw("mut").or_not())
        .then(ident())
        .then(sym(":").ignore_then(ty.clone()).or_not())
        .then_ignore(assign_op())
        .then(expr.clone())
        .then_ignore(sym(";"))
        .map(|(((m, name), ty), value)| Stmt::Let { mutable: m.is_some(), name, ty, value })
        .boxed();

    let managed = kw("managed")
        .ignore_then(braces(stmt.clone()))
        .map(Stmt::Managed)
        .boxed();
    let unsafe_ = kw("unsafe")
        .ignore_then(braces(stmt.clone()))
        .then_ignore(kw("end"))
        .map(Stmt::Unsafe)
        .boxed();
    let for_ = kw("for")
        .ignore_then(ident())
        .then_ignore(kw("in"))
        .then(expr.clone())
        .then(block.clone())
        .then_ignore(kw("end"))
        .map(|((var, iter), body)| Stmt::For { var, iter, body })
        .boxed();
    let while_ = kw("while")
        .ignore_then(expr.clone())
        .then(block.clone())
        .then_ignore(kw("end"))
        .map(|(cond, body)| Stmt::While { cond, body })
        .boxed();
    let if_ = kw("if")
        .ignore_then(expr.clone())
        .then(block.clone())
        .then(
            kw("else")
                .ignore_then(kw("if"))
                .ignore_then(expr.clone())
                .then(block.clone())
                .repeated(),
        )
        .then(kw("else").ignore_then(block.clone()).or_not())
        .then_ignore(kw("end"))
        .map(|(((c, b), elifs), else_body)| {
            let mut branches = vec![(c, b)];
            branches.extend(elifs);
            Stmt::If { branches, else_body }
        })
        .boxed();
    let case = kw("case")
        .ignore_then(pat.clone())
        .then(kw("if").ignore_then(expr.clone()).or_not())
        .then(block.clone())
        .map(|((pat, guard), body)| Arm { pat, guard, body })
        .boxed();
    let match_ = kw("match")
        .ignore_then(expr.clone())
        .then(case.repeated().at_least(1))
        .then_ignore(kw("end"))
        .map(|(scrut, arms)| Stmt::Match { scrut, arms })
        .boxed();
    let ret = kw("return")
        .ignore_then(expr.clone().or_not())
        .then_ignore(sym(";"))
        .map(Stmt::Return)
        .boxed();
    // Assign / ExprStmt 共用前綴，只 parse 一次 expression
    let simple = expr
        .clone()
        .then(assign_op().ignore_then(expr.clone()).or_not())
        .then_ignore(sym(";"))
        .try_map(|(lhs, rhs), span| match rhs {
            Some(value) if is_place(&lhs) => Ok(Stmt::Assign { target: lhs, value }),
            Some(_) => Err(Simple::custom(span, "賦值左側必須是變數或欄位")),
            None => Ok(Stmt::Expr(lhs)),
        })
        .boxed();

    stmt.define(
        choice((let_stmt, if_, match_, managed, unsafe_, for_, while_, ret, simple))
            .map_with_span(|s, span: std::ops::Range<usize>| Stmt::At(span.start, Box::new(s))),
    );
    (expr, stmt)
}

// ───────────────────────── 頂層宣告 ─────────────────────────

pub fn program_parser() -> B<Program> {
    let ty = type_parser();
    let (_expr, stmt) = expr_stmt_parsers(ty.clone(), pattern_parser());
    let params = || param_parser(type_parser()).separated_by(sym(","));

    // ── 泛型參數 `[T: Add + Clone, U]`：Curry-Howard 的邏輯約束命題 ──
    let bound_name = path().map(|p| p.join("::")).boxed();
    let generic_param = ident()
        .then(sym(":").ignore_then(bound_name.separated_by(sym("+")).at_least(1)).or_not())
        .map(|(name, bounds)| GenericParam { name, bounds: bounds.unwrap_or_default() })
        .boxed();
    let generics = generic_param
        .separated_by(sym(","))
        .delimited_by(sym("["), sym("]"))
        .or_not()
        .map(|g| g.unwrap_or_default())
        .boxed();
    let attribute = sym("#").ignore_then(path().delimited_by(sym("["), sym("]")));

    let import_crate = kw("import")
        .ignore_then(kw("crate"))
        .ignore_then(string_lit())
        .then(
            kw("version")
                .ignore_then(assign_op())
                .ignore_then(string_lit())
                .or_not(),
        )
        .then(
            sym(",")
                .ignore_then(kw("features"))
                .ignore_then(assign_op())
                .ignore_then(
                    string_lit()
                        .separated_by(sym(","))
                        .allow_trailing()
                        .delimited_by(sym("["), sym("]")),
                )
                .or_not(),
        )
        .then_ignore(sym(";"))
        .map(|((name, version), features)| {
            Decl::Import(ImportCrate { name, version, features: features.unwrap_or_default() })
        })
        .boxed();

    let import_mod = kw("import")
        .ignore_then(string_lit())
        .then(kw("as").ignore_then(ident()).or_not())
        .then_ignore(sym(";"))
        .map(|(path, alias)| Decl::ImportMod { path, alias })
        .boxed();
    let import = choice((import_crate, import_mod)).boxed();

    let variant = ident()
        .then(
            ty.clone()
                .separated_by(sym(","))
                .delimited_by(sym("("), sym(")"))
                .or_not(),
        )
        .map(|(name, fields)| VariantDecl { name, fields: fields.unwrap_or_default() })
        .boxed();
    let enum_decl = kw("gc")
        .or_not()
        .then_ignore(kw("enum"))
        .then(ident())
        .then(generics.clone())
        .then(variant.separated_by(sym(",")).allow_trailing())
        .then_ignore(kw("end"))
        .map(|(((gc, name), gs), variants)| {
            Decl::Enum(EnumDecl { gc: gc.is_some(), name, generics: gs, variants })
        })
        .boxed();

    let extern_fn = kw("async")
        .or_not()
        .ignore_then(kw("fn"))
        .ignore_then(ident())
        .then(params().delimited_by(sym("("), sym(")")))
        .then_ignore(sym("->"))
        .then(ty.clone())
        .then_ignore(sym(";"))
        .map(|((name, params), ret)| ExternFn { name, params, ret })
        .boxed();
    let extern_block = kw("extern")
        .ignore_then(
            kw("crate")
                .ignore_then(ident())
                .map(|_| "C".to_string())
                .or(string_lit())
                .or_not(),
        )
        .then(extern_fn.repeated().delimited_by(sym("{"), sym("}")))
        .then_ignore(kw("end"))
        .map(|(abi, items)| Decl::Extern(ExternBlock { abi: abi.unwrap_or_else(|| "C".into()), items }))
        .boxed();

    let field = kw("mut")
        .or_not()
        .then(ident())
        .then_ignore(sym(":"))
        .then(ty.clone())
        .map(|((m, name), ty)| Field { mutable: m.is_some(), name, ty })
        .boxed();
    let struct_decl = kw("gc")
        .or_not()
        .then_ignore(kw("struct"))
        .then(ident())
        .then(generics.clone())
        .then(
            field
                .separated_by(sym(","))
                .allow_trailing()
                .delimited_by(sym("{"), sym("}")),
        )
        .then_ignore(kw("end"))
        .map(|(((gc, name), gs), fields)| {
            Decl::Struct(StructDecl { gc: gc.is_some(), name, generics: gs, fields })
        })
        .boxed();

    // 方法接收者：`&self` / `&mut self` / `self` / `mut self`
    let self_param = choice((
        sym("&")
            .ignore_then(kw("mut").or_not())
            .then_ignore(kw("self"))
            .map(|m| if m.is_some() { SelfKind::RefMut } else { SelfKind::Ref })
            .boxed(),
        kw("mut")
            .or_not()
            .then_ignore(kw("self"))
            .map(|m| SelfKind::Value { mutable: m.is_some() })
            .boxed(),
    ))
    .then_ignore(sym(",").or_not())
    .boxed();
    let fn_decl = kw("async")
        .or_not()
        .then_ignore(kw("fn"))
        .then(ident())
        .then(generics.clone())
        .then_ignore(sym("("))
        .then(self_param.or_not())
        .then(params())
        .then_ignore(sym(")"))
        .then_ignore(sym("->"))
        .then(ty.clone())
        .then(stmt.repeated())
        .then_ignore(kw("end"))
        .map_with_span(|((((((a, name), gs), self_kind), params), ret), body), span: std::ops::Range<usize>| {
            FunctionDecl { off: span.start, is_async: a.is_some(), name, generics: gs, self_kind, params, ret, body }
        })
        .boxed();
    let function = fn_decl
        .clone()
        .try_map(|f, span| {
            if f.self_kind.is_some() {
                Err(Simple::custom(span, "`self` 只能用在 impl 區塊內的方法"))
            } else {
                Ok(Decl::Function(f))
            }
        })
        .boxed();
    let impl_decl = kw("impl")
        .ignore_then(ident())
        .then(kw("for").ignore_then(ident()).or_not())
        .then(fn_decl.repeated())
        .then_ignore(kw("end"))
        .map(|((name, target), methods)| match target {
            // `impl Trait for Type ... end`：手動提供證明（逃生艙；通常由編譯器自動橋接）
            Some(t) => Decl::ImplTrait(ImplTraitDecl { trait_name: name, type_name: t, methods }),
            // `impl Type ... end`：固有方法
            None => Decl::Impl(ImplDecl { name, methods }),
        })
        .boxed();

    // ── trait 定義：`trait Comparable  fn lt(self, other: Self) -> Bool;  end` ──
    // 方法只有簽章（以 `;` 結尾），身體由各型別的固有 impl 結構化提供，編譯器自動橋接。
    let self_param2 = choice((
        sym("&")
            .ignore_then(kw("mut").or_not())
            .then_ignore(kw("self"))
            .map(|m| if m.is_some() { SelfKind::RefMut } else { SelfKind::Ref })
            .boxed(),
        kw("mut")
            .or_not()
            .then_ignore(kw("self"))
            .map(|m| SelfKind::Value { mutable: m.is_some() })
            .boxed(),
    ))
    .then_ignore(sym(",").or_not())
    .boxed();
    let trait_method = kw("fn")
        .ignore_then(ident())
        .then(generics.clone())
        .then_ignore(sym("("))
        .then(self_param2.or_not())
        .then(params())
        .then_ignore(sym(")"))
        .then_ignore(sym("->"))
        .then(ty.clone())
        .then_ignore(sym(";"))
        .map_with_span(|((((name, _gs), self_kind), params), ret), span: std::ops::Range<usize>| {
            TraitMethod { off: span.start, name, self_kind, params, ret }
        })
        .boxed();
    let trait_decl = kw("trait")
        .ignore_then(ident())
        .then(trait_method.repeated())
        .then_ignore(kw("end"))
        .map(|(name, methods)| Decl::Trait(TraitDecl { name, is_auto: false, methods }))
        .boxed();

    let top = attribute
        .or_not()
        .then(choice((import, extern_block, struct_decl, enum_decl, trait_decl, impl_decl, function)))
        .map_with_span(|(attr, decl), span: std::ops::Range<usize>| TopLevel {
            off: span.start,
            attr,
            decl,
        })
        .boxed();

    ws().ignore_then(top.repeated())
        .then_ignore(end())
        .map(|items| Program { items })
        .boxed()
}

pub fn parse(src: &str) -> Result<Program, Vec<E>> {
    program_parser().parse(src)
}
