//! 簡化 Lean 語法的 lexer + parser。
//!
//! 支援：
//! - 命令：`def` / `theorem` / `example` / `axiom` / `abbrev` / `#check` / `#eval`
//! - 項：`fun` / `forall` / `let .. := ..; ..` / `if then else` / `->` / `/\` `\/` / `=` / `+` `*` / 函數套用
//! - 證明塊：`by` + 縮排敏感的 tactic 序列（`intro/exact/apply/rfl/simp/rw/cases/...`）
//! - unicode 糖：`∀ λ → ∧ ∨ ¬ ≠ ←`

use crate::ast::{mk_eq, Ann, Command, Expr, Param, Tactic, nat_lit};

#[derive(Clone, Debug)]
pub enum Tok {
    Ident(String),
    Num(u64),
    Op(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Colon,
    Comma,
    Semi,
    Hash(String),
    Underscore,
    NL(i64),
    EOF,
}

// ---------- lexer（實作見檔尾 lex_fixed，正確處理 unicode byte index） ----------

fn read_ident(ch: &[char], mut i: usize) -> (String, usize) {
    let mut s = String::new();
    while i < ch.len() && (ch[i].is_alphanumeric() || ch[i] == '_' || ch[i] == '\'') {
        s.push(ch[i]);
        i += 1;
    }
    // dotted: Nat.zero / And.intro
    loop {
        if i < ch.len() && ch[i] == '.' {
            if i + 1 < ch.len() && (ch[i + 1].is_alphabetic() || ch[i + 1] == '_') {
                s.push('.');
                i += 1;
                while i < ch.len() && (ch[i].is_alphanumeric() || ch[i] == '_' || ch[i] == '\'') {
                    s.push(ch[i]);
                    i += 1;
                }
            } else {
                break;
            }
        } else {
            break;
        }
    }
    (s, i)
}

// ---------- parser ----------

const COMMANDS: &[&str] = &["def", "theorem", "example", "axiom", "abbrev"];
/// 這些 ident 永遠不能當函數套用的參數開頭
const STOPWORDS: &[&str] = &[
    "with", "then", "else", "by", "forall", "fun", "let", "if", "def", "theorem", "example",
    "axiom", "abbrev", "show",
];

pub struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    line: usize,
    next_hole: usize,
    cur_indent: i64,
    multi_line: bool,
}

pub fn parse(src: &str) -> Result<(Vec<(Command, usize)>, usize), String> {
    let toks = lex_fixed(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        line: 1,
        next_hole: 0,
        cur_indent: 0,
        multi_line: false,
    };
    let mut out = vec![];
    loop {
        p.skip_seps();
        if matches!(p.peek(), Tok::EOF) {
            break;
        }
        let ln = p.line;
        let c = p.parse_command()?;
        out.push((c, ln));
    }
    Ok((out, p.next_hole))
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }
    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        match &t {
            Tok::NL(ind) => {
                self.line += 1;
                self.cur_indent = *ind;
            }
            _ => {}
        }
        if !matches!(t, Tok::EOF) {
            self.pos += 1;
        }
        t
    }
    fn fresh_hole(&mut self) -> Expr {
        let h = self.next_hole;
        self.next_hole += 1;
        Expr::Hole(h)
    }
    fn skip_seps(&mut self) {
        loop {
            match self.peek() {
                Tok::NL(_) | Tok::Semi => {
                    self.bump();
                }
                _ => break,
            }
        }
    }
    /// top-level term 內允許縮排換行（indent > 0 視為空白）；空白行 NL(-1) 永遠略過
    fn ws(&mut self) {
        loop {
            match self.peek() {
                Tok::NL(ind) if *ind == -1 => {
                    self.bump();
                }
                Tok::NL(ind) if self.multi_line && *ind > 0 => {
                    self.bump();
                }
                _ => break,
            }
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<(), String> {
        self.ws();
        match self.peek() {
            Tok::Op(o) if o == op => {
                self.bump();
                Ok(())
            }
            t => Err(format!("expected `{}` found {:?}", op, t)),
        }
    }
    fn expect_colon(&mut self) -> Result<(), String> {
        self.ws();
        match self.peek() {
            Tok::Colon => {
                self.bump();
                Ok(())
            }
            t => Err(format!("expected `:` found {:?}", t)),
        }
    }
    fn expect_ident(&mut self) -> Result<String, String> {
        self.ws();
        match self.peek() {
            Tok::Ident(s) => {
                let r = s.clone();
                self.bump();
                Ok(r)
            }
            t => Err(format!("expected identifier, found {:?}", t)),
        }
    }

    // ----- commands -----

    fn parse_command(&mut self) -> Result<Command, String> {
        match self.peek() {
            Tok::Hash(w) => {
                let w = w.clone();
                self.bump();
                let saved = self.multi_line;
                self.multi_line = true;
                let e = self.parse_term()?;
                self.multi_line = saved;
                if w == "check" {
                    Ok(Command::Check(e))
                } else if w == "eval" {
                    Ok(Command::Eval(e))
                } else {
                    Err(format!("unknown command `#{}`（只支援 #check / #eval）", w))
                }
            }
            Tok::Ident(k) if COMMANDS.contains(&k.as_str()) => {
                let k = k.clone();
                self.bump();
                match k.as_str() {
                    "def" | "abbrev" => self.parse_def(),
                    "theorem" | "example" => {
                        let name = if k == "theorem" {
                            Some(self.expect_ident()?)
                        } else {
                            None
                        };
                        self.parse_theorem(name)
                    }
                    "axiom" => {
                        let name = self.expect_ident()?;
                        self.expect_colon()?;
                        let saved = self.multi_line;
                        self.multi_line = true;
                        let ty = self.parse_term()?;
                        self.multi_line = saved;
                        Ok(Command::Axiom { name, ty })
                    }
                    _ => unreachable!(),
                }
            }
            t => Err(format!(
                "line {}: expected def/theorem/example/axiom/#check/#eval, found {:?}",
                self.line, t
            )),
        }
    }

    fn parse_param_group(&mut self) -> Result<Param, String> {
        self.ws();
        match self.peek() {
            Tok::LParen | Tok::LBrace | Tok::LBrack => {
                let ann = match self.bump() {
                    Tok::LBrace => Ann::Implicit,
                    _ => Ann::Explicit,
                };
                let mut names = vec![];
                loop {
                    self.ws();
                    match self.peek() {
                        Tok::Ident(s) => {
                            names.push(s.clone());
                            self.bump();
                        }
                        _ => break,
                    }
                }
                if names.is_empty() {
                    return Err(format!("line {}: empty binder group", self.line));
                }
                self.expect_colon()?;
                let ty = self.parse_term()?;
                self.ws();
                match self.peek() {
                    Tok::RParen | Tok::RBrace | Tok::RBrack => {
                        self.bump();
                    }
                    t => return Err(format!("expected `)`/`}}`/`]` found {:?}", t)),
                }
                Ok(Param { names, ann, ty })
            }
            t => Err(format!("expected parameter group, found {:?}", t)),
        }
    }

    fn parse_def(&mut self) -> Result<Command, String> {
        let name = self.expect_ident()?;
        let mut params = vec![];
        loop {
            self.ws();
            match self.peek() {
                Tok::LParen | Tok::LBrace | Tok::LBrack => params.push(self.parse_param_group()?),
                _ => break,
            }
        }
        self.ws();
        let ty = match self.peek() {
            Tok::Colon => {
                self.bump();
                let saved = self.multi_line;
                self.multi_line = true;
                let t = self.parse_term()?;
                self.multi_line = saved;
                Some(t)
            }
            _ => None,
        };
        self.expect_op(":=")?;
        let saved = self.multi_line;
        self.multi_line = true;
        let body = self.parse_term()?;
        self.multi_line = saved;
        Ok(Command::Def { name, params, ty, body })
    }

    fn parse_theorem(&mut self, name: Option<String>) -> Result<Command, String> {
        let mut params = vec![];
        loop {
            self.ws();
            match self.peek() {
                Tok::LParen | Tok::LBrace | Tok::LBrack => params.push(self.parse_param_group()?),
                _ => break,
            }
        }
        self.expect_colon()?;
        let saved = self.multi_line;
        self.multi_line = true;
        let ty = self.parse_term()?;
        self.multi_line = saved;
        self.expect_op(":=")?;
        let saved = self.multi_line;
        self.multi_line = true;
        let body = self.parse_term()?;
        self.multi_line = saved;
        Ok(Command::Theorem { name, params, ty, body })
    }

    // ----- terms -----

    fn parse_term(&mut self) -> Result<Expr, String> {
        // `by` 優先（不能被 multi_line ws 吃掉語意）
        if let Tok::Ident(s) = self.peek() {
            if s == "by" {
                self.bump();
                let tacs = self.parse_by(self.cur_indent)?;
                return Ok(Expr::By(tacs));
            }
        }
        self.ws();
        if let Tok::Ident(s) = self.peek() {
            match s.as_str() {
                "forall" => return self.parse_forall(),
                "fun" => return self.parse_fun(),
                "let" => return self.parse_let(),
                "if" => return self.parse_ite(),
                "by" => {
                    self.bump();
                    let tacs = self.parse_by(self.cur_indent)?;
                    return Ok(Expr::By(tacs));
                }
                _ => {}
            }
        }
        self.parse_arrow()
    }

    fn parse_binders(&mut self, term: &str) -> Result<Vec<Param>, String> {
        let mut groups = vec![];
        let mut buf: Vec<String> = vec![];
        let flush_holes = |buf: &mut Vec<String>, groups: &mut Vec<Param>, p: &mut Parser| {
            if !buf.is_empty() {
                let mut names = vec![];
                std::mem::swap(&mut names, buf);
                // 每個名字獨立 hole（避免共享同一個 metavariable）
                for n in names {
                    groups.push(Param {
                        names: vec![n],
                        ann: Ann::Explicit,
                        ty: p.fresh_hole(),
                    });
                }
            }
        };
        loop {
            self.ws();
            match self.peek() {
                Tok::LParen | Tok::LBrace | Tok::LBrack => {
                    flush_holes(&mut buf, &mut groups, self);
                    groups.push(self.parse_param_group_as_binder()?);
                }
                Tok::Ident(s) if s != term && s != ":" => {
                    buf.push(s.clone());
                    self.bump();
                }
                Tok::Colon => {
                    self.bump();
                    let ty = self.parse_term()?;
                    if buf.is_empty() {
                        return Err(format!("line {}: `:` 前面沒有 binder 名字", self.line));
                    }
                    let mut names = vec![];
                    std::mem::swap(&mut names, &mut buf);
                    groups.push(Param { names, ann: Ann::Explicit, ty });
                }
                _ => break,
            }
            // 終止條件由呼叫端判斷（看到 term 關鍵字即停）
            if let Tok::Ident(s) = self.peek() {
                if s == term {
                    break;
                }
            }
            if let Tok::Op(o) = self.peek() {
                if o == term {
                    break;
                }
            }
        }
        flush_holes(&mut buf, &mut groups, self);
        Ok(groups)
    }

    fn parse_param_group_as_binder(&mut self) -> Result<Param, String> {
        self.parse_param_group()
    }

    fn parse_forall(&mut self) -> Result<Expr, String> {
        self.bump(); // forall
        let binders = self.parse_binders(",")?;
        match self.peek() {
            Tok::Comma => {
                self.bump();
            }
            t => return Err(format!("forall binder 後需要 `,` found {:?}", t)),
        }
        let body = self.parse_term()?;
        let mut out = body;
        for p in binders.iter().rev() {
            for n in p.names.iter().rev() {
                out = Expr::Pi(n.clone(), p.ann, Box::new(p.ty.clone()), Box::new(out));
            }
        }
        Ok(out)
    }

    fn parse_fun(&mut self) -> Result<Expr, String> {
        self.bump(); // fun
        let binders = self.parse_binders("=>")?;
        self.expect_op("=>")?;
        let body = self.parse_term()?;
        let mut out = body;
        for p in binders.iter().rev() {
            for n in p.names.iter().rev() {
                out = Expr::Lam(n.clone(), p.ann, Box::new(p.ty.clone()), Box::new(out));
            }
        }
        Ok(out)
    }

    fn parse_let(&mut self) -> Result<Expr, String> {
        self.bump(); // let
        let name = self.expect_ident()?;
        self.ws();
        let ty = match self.peek() {
            Tok::Colon => {
                self.bump();
                self.parse_term()?
            }
            _ => self.fresh_hole(),
        };
        self.expect_op(":=")?;
        let val = self.parse_term()?;
        self.ws();
        match self.peek() {
            Tok::Semi => {
                self.bump();
            }
            t => {
                return Err(format!(
                    "line {}: term-level let 需要 `;`（多行請用 tactic `have`），found {:?}",
                    self.line, t
                ))
            }
        }
        let body = self.parse_term()?;
        Ok(Expr::Let(name, Box::new(ty), Box::new(val), Box::new(body)))
    }

    fn parse_ite(&mut self) -> Result<Expr, String> {
        self.bump(); // if
        let c = self.parse_term()?;
        self.ws();
        match self.peek() {
            Tok::Ident(s) if s == "then" => {
                self.bump();
            }
            t => return Err(format!("expected `then` found {:?}", t)),
        }
        let t = self.parse_term()?;
        self.ws();
        match self.peek() {
            Tok::Ident(s) if s == "else" => {
                self.bump();
            }
            t => return Err(format!("expected `else` found {:?}", t)),
        }
        let f = self.parse_term()?;
        Ok(Expr::Ite(Box::new(c), Box::new(t), Box::new(f)))
    }

    fn parse_arrow(&mut self) -> Result<Expr, String> {
        let l = self.parse_or()?;
        self.ws();
        match self.peek() {
            Tok::Op(o) if o == "->" => {
                self.bump();
                let r = self.parse_arrow()?;
                Ok(Expr::Pi("_".to_string(), Ann::Explicit, Box::new(l), Box::new(r)))
            }
            _ => Ok(l),
        }
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut l = self.parse_and()?;
        loop {
            self.ws();
            match self.peek() {
                Tok::Op(o) if o == "\\/" => {
                    self.bump();
                    let r = self.parse_and()?;
                    l = Expr::Or(Box::new(l), Box::new(r));
                }
                _ => break,
            }
        }
        Ok(l)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut l = self.parse_eq()?;
        loop {
            self.ws();
            match self.peek() {
                Tok::Op(o) if o == "/\\" => {
                    self.bump();
                    let r = self.parse_eq()?;
                    l = Expr::And(Box::new(l), Box::new(r));
                }
                _ => break,
            }
        }
        Ok(l)
    }

    fn parse_eq(&mut self) -> Result<Expr, String> {
        let l = self.parse_add()?;
        self.ws();
        match self.peek() {
            Tok::Op(o) if o == "=" || o == "!=" => {
                let neg = o == "!=";
                self.bump();
                let r = self.parse_add()?;
                // Eq 的型別參數先放 hole，由 kernel infer 時從左邊推導並指派
                let e = mk_eq(self.fresh_hole(), l, r);
                if neg {
                    Ok(Expr::Pi(
                        "_".to_string(),
                        Ann::Explicit,
                        Box::new(e),
                        Box::new(Expr::Const("False".to_string())),
                    ))
                } else {
                    Ok(e)
                }
            }
            _ => Ok(l),
        }
    }

    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut l = self.parse_mul()?;
        loop {
            self.ws();
            match self.peek() {
                Tok::Op(o) if o == "+" => {
                    self.bump();
                    let r = self.parse_mul()?;
                    l = crate::ast::app(Expr::Const("Nat.add".to_string()), l);
                    l = crate::ast::app(l, r);
                }
                _ => break,
            }
        }
        Ok(l)
    }

    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut l = self.parse_unary()?;
        loop {
            self.ws();
            match self.peek() {
                Tok::Op(o) if o == "*" => {
                    self.bump();
                    let r = self.parse_unary()?;
                    l = crate::ast::app(Expr::Const("Nat.mul".to_string()), l);
                    l = crate::ast::app(l, r);
                }
                _ => break,
            }
        }
        Ok(l)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        self.ws();
        match self.peek() {
            Tok::Op(o) if o == "~" || o == "!" => {
                self.bump();
                let a = self.parse_unary()?;
                Ok(Expr::Pi(
                    "_".to_string(),
                    Ann::Explicit,
                    Box::new(a),
                    Box::new(Expr::Const("False".to_string())),
                ))
            }
            _ => self.parse_app(),
        }
    }

    fn is_atom_start(&self) -> bool {
        match self.peek() {
            Tok::Ident(s) => !STOPWORDS.contains(&s.as_str()),
            Tok::Num(_) | Tok::Underscore | Tok::LParen => true,
            _ => false,
        }
    }

    fn parse_app(&mut self) -> Result<Expr, String> {
        let mut f = self.parse_atom()?;
        while self.is_atom_start() {
            let a = self.parse_atom()?;
            f = crate::ast::app(f, a);
        }
        Ok(f)
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        self.ws();
        match self.peek() {
            Tok::Num(n) => {
                let n = *n;
                self.bump();
                Ok(nat_lit(n))
            }
            Tok::Underscore => {
                self.bump();
                Ok(self.fresh_hole())
            }
            Tok::LParen => {
                self.bump();
                let saved = self.multi_line;
                let e = self.parse_term()?;
                self.multi_line = saved;
                self.ws();
                match self.peek() {
                    Tok::RParen => {
                        self.bump();
                        Ok(e)
                    }
                    t => Err(format!("expected `)` found {:?}", t)),
                }
            }
            Tok::Ident(s) => {
                let s = s.clone();
                self.bump();
                match s.as_str() {
                    "Prop" => Ok(Expr::Sort(crate::ast::Sort::Prop)),
                    "Type" => Ok(Expr::Sort(crate::ast::Sort::Typ)),
                    "Nat" => Ok(Expr::Const("Nat".to_string())),
                    "Bool" => Ok(Expr::Const("Bool".to_string())),
                    "True" => Ok(Expr::Const("True".to_string())),
                    "False" => Ok(Expr::Const("False".to_string())),
                    "true" => Ok(Expr::Const("true".to_string())),
                    "false" => Ok(Expr::Const("false".to_string())),
                    "rfl" => Ok(Expr::Rfl),
                    "sorry" => Ok(Expr::Sorry),
                    _ => Ok(Expr::Name(s)),
                }
            }
            t => Err(format!("line {}: expected term, found {:?}", self.line, t)),
        }
    }

    // ----- by blocks / tactics -----

    /// parent：`by` 所在行的縮排；block 內 tactic 行縮排必須 > parent，且彼此相等
    fn parse_by(&mut self, parent: i64) -> Result<Vec<Tactic>, String> {
        // 跳過同行多餘空白（lexer 已處理）；看是否有換行（先略過空白行 NL(-1)）
        while matches!(self.peek(), Tok::NL(ind) if *ind == -1) {
            self.bump();
        }
        match self.peek() {
            Tok::NL(ind) => {
                let ind = *ind;
                if ind <= parent {
                    return Err(format!(
                        "line {}: `by` 後面需要縮排的 tactic（parent indent {}，got {}）",
                        self.line, parent, ind
                    ));
                }
                self.bump(); // 進入 block
                let base = ind;
                let mut tacs = vec![];
                loop {
                    match self.peek() {
                        Tok::EOF => break,
                        Tok::Ident(s) if COMMANDS.contains(&s.as_str()) => break,
                        Tok::NL(ind2) => {
                            let ind2 = *ind2;
                            self.bump();
                            if ind2 == -1 {
                                continue; // 空白行：留在 block 內
                            }
                            if ind2 != base {
                                // dedent：屬於外層（NL 已消耗不影響正確性）
                                break;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    tacs.push(self.parse_tactic()?);
                    match self.peek() {
                        Tok::Semi => {
                            self.bump();
                            continue;
                        }
                        Tok::NL(ind2) => {
                            let ind2 = *ind2;
                            if ind2 == base || ind2 == -1 {
                                self.bump();
                                continue;
                            } else {
                                break; // dedent：留給外層
                            }
                        }
                        Tok::EOF => break,
                        _ => {
                            // 同行接下一個 tactic？必須有 ; 分隔
                            if let Tok::Ident(s) = self.peek() {
                                if COMMANDS.contains(&s.as_str()) {
                                    break;
                                }
                            }
                            return Err(format!(
                                "line {}: 同一行多個 tactic 請用 `;` 分隔",
                                self.line
                            ));
                        }
                    }
                }
                if tacs.is_empty() {
                    return Err("`by` 後面沒有 tactic".to_string());
                }
                Ok(tacs)
            }
            _ => {
                // 同行：by tac1; tac2
                let mut tacs = vec![];
                tacs.push(self.parse_tactic()?);
                while matches!(self.peek(), Tok::Semi) {
                    self.bump();
                    // 同行不允許換行延續
                    if matches!(self.peek(), Tok::NL(_) | Tok::EOF) {
                        break;
                    }
                    tacs.push(self.parse_tactic()?);
                }
                Ok(tacs)
            }
        }
    }

    fn parse_expr_list(&mut self) -> Result<Vec<Expr>, String> {
        let mut out = vec![];
        loop {
            self.ws();
            if matches!(self.peek(), Tok::RBrack) {
                break;
            }
            out.push(self.parse_term()?);
            self.ws();
            match self.peek() {
                Tok::Comma => {
                    self.bump();
                }
                Tok::RBrack => break,
                t => return Err(format!("expected `,` or `]` found {:?}", t)),
            }
        }
        Ok(out)
    }

    fn parse_tactic(&mut self) -> Result<Tactic, String> {
        let saved = self.multi_line;
        self.multi_line = false;
        let r = self.parse_tactic_inner();
        self.multi_line = saved;
        r
    }

    fn parse_tactic_inner(&mut self) -> Result<Tactic, String> {
        self.ws();
        let kw = match self.peek() {
            Tok::Ident(s) => s.clone(),
            t => return Err(format!("line {}: expected tactic, found {:?}", self.line, t)),
        };
        match kw.as_str() {
            "intro" => {
                self.bump();
                let mut xs = vec![];
                while let Tok::Ident(s) = self.peek() {
                    xs.push(s.clone());
                    self.bump();
                }
                if xs.is_empty() {
                    return Err("`intro` 需要至少一個名字".to_string());
                }
                Ok(Tactic::Intro(xs))
            }
            "exact" => {
                self.bump();
                Ok(Tactic::Exact(self.parse_term()?))
            }
            "apply" => {
                self.bump();
                Ok(Tactic::Apply(self.parse_term()?))
            }
            "show" => {
                self.bump();
                Ok(Tactic::Show(self.parse_term()?))
            }
            "rfl" => {
                self.bump();
                Ok(Tactic::Rfl)
            }
            "left" => {
                self.bump();
                Ok(Tactic::Left)
            }
            "right" => {
                self.bump();
                Ok(Tactic::Right)
            }
            "constructor" => {
                self.bump();
                Ok(Tactic::Constructor)
            }
            "trivial" => {
                self.bump();
                Ok(Tactic::Trivial)
            }
            "assumption" => {
                self.bump();
                Ok(Tactic::Assumption)
            }
            "sorry" => {
                self.bump();
                Ok(Tactic::Sorry)
            }
            "simp" => {
                self.bump();
                if let Tok::Ident(s) = self.peek() {
                    if s == "only" {
                        self.bump();
                    }
                }
                self.ws();
                if matches!(self.peek(), Tok::LBrack) {
                    self.bump();
                    let xs = self.parse_expr_list()?;
                    self.ws();
                    match self.peek() {
                        Tok::RBrack => {
                            self.bump();
                        }
                        t => return Err(format!("simp: expected `]` found {:?}", t)),
                    }
                    Ok(Tactic::Simp(xs))
                } else {
                    Ok(Tactic::Simp(vec![]))
                }
            }
            "rw" => {
                self.bump();
                self.ws();
                match self.peek() {
                    Tok::LBrack => {
                        self.bump();
                    }
                    t => return Err(format!("rw 需要 `[...]` found {:?}", t)),
                }
                let mut rs = vec![];
                loop {
                    self.ws();
                    if matches!(self.peek(), Tok::RBrack) {
                        break;
                    }
                    let mut rev = false;
                    if let Tok::Op(o) = self.peek() {
                        if o == "<-" {
                            rev = true;
                            self.bump();
                        }
                    }
                    rs.push((rev, self.parse_term()?));
                    self.ws();
                    match self.peek() {
                        Tok::Comma => {
                            self.bump();
                        }
                        Tok::RBrack => break,
                        t => return Err(format!("rw: expected `,` or `]` found {:?}", t)),
                    }
                }
                self.ws();
                match self.peek() {
                    Tok::RBrack => {
                        self.bump();
                    }
                    t => return Err(format!("rw: expected `]` found {:?}", t)),
                }
                if rs.is_empty() {
                    return Err("`rw []` 是空的".to_string());
                }
                Ok(Tactic::Rw(rs))
            }
            "cases" => {
                self.bump();
                let h = self.expect_ident()?;
                let mut ns = vec![];
                if let Tok::Ident(s) = self.peek() {
                    if s == "with" {
                        self.bump();
                        while let Tok::Ident(n) = self.peek() {
                            ns.push(n.clone());
                            self.bump();
                        }
                    }
                }
                Ok(Tactic::Cases(h, ns))
            }
            "induction" => {
                self.bump();
                let n = self.expect_ident()?;
                let mut ns = vec![];
                if let Tok::Ident(s) = self.peek() {
                    if s == "with" {
                        self.bump();
                        while let Tok::Ident(x) = self.peek() {
                            ns.push(x.clone());
                            self.bump();
                        }
                    }
                }
                Ok(Tactic::Induction(n, ns))
            }
            "have" => {
                self.bump();
                let n = self.expect_ident()?;
                self.expect_colon()?;
                let ty = self.parse_term()?;
                self.expect_op(":=")?;
                let v = self.parse_term()?;
                Ok(Tactic::Have(n, ty, v))
            }
            "case" => {
                self.bump();
                let tag = self.expect_ident()?;
                Ok(Tactic::Case(tag))
            }
            "all_goals" => {
                self.bump();
                Ok(Tactic::AllGoals(Box::new(self.parse_tactic_inner()?)))
            }
            _ => Err(format!("line {}: unknown tactic `{}`", self.line, kw)),
        }
    }
}

// ---------- lex 修正：char index vs byte index ----------
// 上面的 lex 用了 src[byte..byte] 但走的是 char index；改寫成正確版本。

fn lex_fixed(src: &str) -> Result<Vec<Tok>, String> {
    let ch: Vec<char> = src.chars().collect();
    // byte offsets of each char
    let mut byte_of: Vec<usize> = vec![0; ch.len() + 1];
    let mut b = 0;
    for (i, c) in src.chars().enumerate() {
        byte_of[i] = b;
        b += c.len_utf8();
    }
    byte_of[ch.len()] = b;

    let mut toks = vec![];
    let mut i = 0;
    while i < ch.len() {
        let c = ch[i];
        match c {
            ' ' | '\t' | '\r' => {
                i += 1;
            }
            '\n' => {
                i += 1;
                let mut j = i;
                let mut indent: i64 = 0;
                while j < ch.len() && (ch[j] == ' ' || ch[j] == '\t') {
                    indent += if ch[j] == '\t' { 4 } else { 1 };
                    j += 1;
                }
                if j < ch.len() && ch[j] == '\n' {
                    // 空白行：發一個 NL(-1) 佔位（只為行號正確；parser 會略過它）
                    toks.push(Tok::NL(-1));
                    i = j;
                    continue;
                }
                if j >= ch.len() {
                    break;
                }
                if j + 1 < ch.len() && ch[j] == '-' && ch[j + 1] == '-' {
                    i = j;
                    continue;
                }
                if j + 1 < ch.len() && ch[j] == '/' && ch[j + 1] == '-' {
                    i = j;
                    continue;
                }
                i = j;
                toks.push(Tok::NL(indent));
            }
            '-' if i + 1 < ch.len() && ch[i + 1] == '-' => {
                while i < ch.len() && ch[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < ch.len() && ch[i + 1] == '-' => {
                let mut depth = 0;
                while i < ch.len() {
                    if i + 1 < ch.len() && ch[i] == '/' && ch[i + 1] == '-' {
                        depth += 1;
                        i += 2;
                    } else if i + 1 < ch.len() && ch[i] == '-' && ch[i + 1] == '/' {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                if depth != 0 {
                    return Err("unterminated /- -/ comment".to_string());
                }
            }
            '0'..='9' => {
                let mut j = i;
                while j < ch.len() && ch[j].is_ascii_digit() {
                    j += 1;
                }
                let n: u64 = src[byte_of[i]..byte_of[j]].parse().unwrap();
                toks.push(Tok::Num(n));
                i = j;
            }
            '?' => {
                let mut j = i + 1;
                while j < ch.len() && (ch[j].is_alphanumeric() || ch[j] == '_' || ch[j] == '\'') {
                    j += 1;
                }
                toks.push(Tok::Underscore);
                i = j;
            }
            '_' => {
                if i + 1 < ch.len() && (ch[i + 1].is_alphanumeric()) {
                    let (s, j) = read_ident(&ch, i);
                    toks.push(Tok::Ident(s));
                    i = j;
                } else {
                    toks.push(Tok::Underscore);
                    i += 1;
                }
            }
            'a'..='z' | 'A'..='Z' => {
                let (s, j) = read_ident(&ch, i);
                toks.push(Tok::Ident(s));
                i = j;
            }
            '#' => {
                let mut j = i + 1;
                while j < ch.len() && ch[j].is_ascii_alphabetic() {
                    j += 1;
                }
                toks.push(Tok::Hash(src[byte_of[i + 1]..byte_of[j]].to_string()));
                i = j;
            }
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            '{' => {
                toks.push(Tok::LBrace);
                i += 1;
            }
            '}' => {
                toks.push(Tok::RBrace);
                i += 1;
            }
            '[' => {
                toks.push(Tok::LBrack);
                i += 1;
            }
            ']' => {
                toks.push(Tok::RBrack);
                i += 1;
            }
            ':' => {
                if i + 1 < ch.len() && ch[i + 1] == '=' {
                    toks.push(Tok::Op(":=".to_string()));
                    i += 2;
                } else {
                    toks.push(Tok::Colon);
                    i += 1;
                }
            }
            ',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            ';' => {
                toks.push(Tok::Semi);
                i += 1;
            }
            '|' => {
                toks.push(Tok::Op("|".to_string()));
                i += 1;
            }
            '-' => {
                if i + 1 < ch.len() && ch[i + 1] == '>' {
                    toks.push(Tok::Op("->".to_string()));
                    i += 2;
                } else {
                    return Err("unsupported `-`（micro-Lean 沒有 Nat 減法）".to_string());
                }
            }
            '<' => {
                if i + 1 < ch.len() && ch[i + 1] == '-' {
                    toks.push(Tok::Op("<-".to_string()));
                    i += 2;
                } else {
                    return Err("unsupported `<`".to_string());
                }
            }
            '=' => {
                if i + 1 < ch.len() && ch[i + 1] == '>' {
                    toks.push(Tok::Op("=>".to_string()));
                    i += 2;
                } else {
                    toks.push(Tok::Op("=".to_string()));
                    i += 1;
                }
            }
            '+' => {
                toks.push(Tok::Op("+".to_string()));
                i += 1;
            }
            '*' => {
                toks.push(Tok::Op("*".to_string()));
                i += 1;
            }
            '!' => {
                if i + 1 < ch.len() && ch[i + 1] == '=' {
                    toks.push(Tok::Op("!=".to_string()));
                    i += 2;
                } else {
                    toks.push(Tok::Op("!".to_string()));
                    i += 1;
                }
            }
            '~' => {
                toks.push(Tok::Op("~".to_string()));
                i += 1;
            }
            '/' => {
                if i + 1 < ch.len() && ch[i + 1] == '\\' {
                    toks.push(Tok::Op("/\\".to_string()));
                    i += 2;
                } else {
                    return Err("unsupported `/`（是否想寫 `/\\`？）".to_string());
                }
            }
            '\\' => {
                if i + 1 < ch.len() && ch[i + 1] == '/' {
                    toks.push(Tok::Op("\\/".to_string()));
                    i += 2;
                } else {
                    return Err("unsupported `\\`（是否想寫 `\\/`？）".to_string());
                }
            }
            '.' => return Err("stray `.`".to_string()),
            '→' => {
                toks.push(Tok::Op("->".to_string()));
                i += 1;
            }
            '∧' => {
                toks.push(Tok::Op("/\\".to_string()));
                i += 1;
            }
            '∨' => {
                toks.push(Tok::Op("\\/".to_string()));
                i += 1;
            }
            '∀' => {
                toks.push(Tok::Ident("forall".to_string()));
                i += 1;
            }
            'λ' => {
                toks.push(Tok::Ident("fun".to_string()));
                i += 1;
            }
            '¬' => {
                toks.push(Tok::Op("~".to_string()));
                i += 1;
            }
            '≠' => {
                toks.push(Tok::Op("!=".to_string()));
                i += 1;
            }
            '←' => {
                toks.push(Tok::Op("<-".to_string()));
                i += 1;
            }
            '⇒' => {
                toks.push(Tok::Op("=>".to_string()));
                i += 1;
            }
            '"' => return Err("string literals 不支援（micro-Lean 無 String）".to_string()),
            '@' => return Err("`@` 不支援".to_string()),
            c if c.is_alphabetic() => {
                // unicode 識別字（如 α β γ）；注意本 arm 必須在 λ/∀ 等運算子之後
                let (s, j) = read_ident(&ch, i);
                toks.push(Tok::Ident(s));
                i = j;
            }
            _ => return Err(format!("unexpected character: {:?}", c)),
        }
    }
    toks.push(Tok::EOF);
    Ok(toks)
}
