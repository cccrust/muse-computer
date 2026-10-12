//! Micro-Lean4 AST: terms, tactics, commands + helpers (subst / free vars / pretty printer).
//! 簡化設計：named 變數 + capture-avoiding substitution；Type-in-Type（不做 universe 檢查）。

use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Prop,
    Typ,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ann {
    Explicit,
    Implicit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Sort(Sort),
    /// parse-time 未解析的名字（elaboration 時依 ctx/env 轉成 Var/Const）
    Name(String),
    Var(String),
    Const(String),
    App(Box<Expr>, Box<Expr>),
    Lam(String, Ann, Box<Expr>, Box<Expr>),
    Pi(String, Ann, Box<Expr>, Box<Expr>),
    Let(String, Box<Expr>, Box<Expr>, Box<Expr>),
    Hole(usize),
    Eq(Box<Expr>, Box<Expr>, Box<Expr>), // Eq(ty, lhs, rhs)：ty 明確攜帶，unification 才能解出隱含型別參數
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Ite(Box<Expr>, Box<Expr>, Box<Expr>),
    Rfl,
    Sorry,
    By(Vec<Tactic>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tactic {
    Intro(Vec<String>),
    Exact(Expr),
    Apply(Expr),
    Rfl,
    Simp(Vec<Expr>),
    /// (reverse?, rule)
    Rw(Vec<(bool, Expr)>),
    /// cases h with names...
    Cases(String, Vec<String>),
    Left,
    Right,
    Constructor,
    Trivial,
    Have(String, Expr, Expr),
    Induction(String, Vec<String>),
    Show(Expr),
    Assumption,
    Sorry,
    /// case tag：把對應 goal 移到最前面
    Case(String),
    AllGoals(Box<Tactic>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub names: Vec<String>,
    pub ann: Ann,
    pub ty: Expr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Def {
        name: String,
        params: Vec<Param>,
        ty: Option<Expr>,
        body: Expr,
    },
    Theorem {
        name: Option<String>,
        params: Vec<Param>,
        ty: Expr,
        body: Expr,
    },
    Axiom {
        name: String,
        ty: Expr,
    },
    Check(Expr),
    Eval(Expr),
}

// ---------- constructors ----------

pub fn app(f: Expr, a: Expr) -> Expr {
    Expr::App(Box::new(f), Box::new(a))
}

pub fn mk_apps(mut f: Expr, args: Vec<Expr>) -> Expr {
    for a in args {
        f = app(f, a);
    }
    f
}

pub fn collect_apps(e: &Expr) -> (&Expr, Vec<&Expr>) {
    let mut args = vec![];
    let mut cur = e;
    while let Expr::App(f, a) = cur {
        args.push(a.as_ref());
        cur = f.as_ref();
    }
    args.reverse();
    (cur, args)
}

pub fn mk_pis(params: &[Param], body: Expr) -> Expr {
    let mut out = body;
    for p in params.iter().rev() {
        for n in p.names.iter().rev() {
            out = Expr::Pi(n.clone(), p.ann, Box::new(p.ty.clone()), Box::new(out));
        }
    }
    out
}

pub fn mk_lams(params: &[Param], body: Expr) -> Expr {
    let mut out = body;
    for p in params.iter().rev() {
        for n in p.names.iter().rev() {
            out = Expr::Lam(n.clone(), p.ann, Box::new(p.ty.clone()), Box::new(out));
        }
    }
    out
}

/// Eq(ty, lhs, rhs) 建構子
pub fn mk_eq(ty: Expr, l: Expr, r: Expr) -> Expr {
    Expr::Eq(Box::new(ty), Box::new(l), Box::new(r))
}

/// 數字字面量 desugar 成 Nat.succ chain
pub fn nat_lit(n: u64) -> Expr {
    let mut e = Expr::Const("Nat.zero".to_string());
    for _ in 0..n {
        e = app(Expr::Const("Nat.succ".to_string()), e);
    }
    e
}

/// succ-chain 認得回數字（pretty / eval 用）
pub fn as_nat(e: &Expr) -> Option<u64> {
    match e {
        Expr::Const(s) if s == "Nat.zero" => Some(0),
        Expr::App(f, a) => {
            if let Expr::Const(s) = f.as_ref() {
                if s == "Nat.succ" {
                    return as_nat(a).map(|n| n + 1);
                }
            }
            None
        }
        _ => None,
    }
}

// ---------- free vars / subst ----------

pub fn free_vars(e: &Expr) -> HashSet<String> {
    let mut s = HashSet::new();
    fv(e, &mut s);
    s
}

fn fv(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Sort(_) | Expr::Const(_) | Expr::Hole(_) | Expr::Rfl | Expr::Sorry => {}
        Expr::Name(n) | Expr::Var(n) => {
            out.insert(n.clone());
        }
        Expr::App(f, a) => {
            fv(f, out);
            fv(a, out);
        }
        Expr::Lam(x, _, t, b) | Expr::Pi(x, _, t, b) => {
            fv(t, out);
            let mut inner = HashSet::new();
            fv(b, &mut inner);
            inner.remove(x);
            out.extend(inner);
        }
        Expr::Let(x, t, v, b) => {
            fv(t, out);
            fv(v, out);
            let mut inner = HashSet::new();
            fv(b, &mut inner);
            inner.remove(x);
            out.extend(inner);
        }
        Expr::Eq(t, a, b) => {
            fv(t, out);
            fv(a, out);
            fv(b, out);
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            fv(a, out);
            fv(b, out);
        }
        Expr::Ite(c, t, f) => {
            fv(c, out);
            fv(t, out);
            fv(f, out);
        }
        Expr::By(_) => {}
    }
}

static FRESH_CTR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn fresh_name(base: &str) -> String {
    let i = FRESH_CTR.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{}${}", base, i)
}

pub fn fresh_avoiding(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    let mut i = 1usize;
    loop {
        let c = format!("{}_{}", base, i);
        if !taken.contains(&c) {
            return c;
        }
        i += 1;
    }
}

/// capture-avoiding substition: [x := v]e
pub fn subst(x: &str, v: &Expr, e: &Expr) -> Expr {
    match e {
        Expr::Sort(_) | Expr::Const(_) | Expr::Hole(_) | Expr::Rfl | Expr::Sorry => e.clone(),
        Expr::Name(n) if n == x => v.clone(),
        Expr::Name(_) => e.clone(),
        Expr::Var(n) if n == x => v.clone(),
        Expr::Var(_) => e.clone(),
        Expr::App(f, a) => app(subst(x, v, f), subst(x, v, a)),
        Expr::Lam(y, an, t, b) => {
            let t2 = subst(x, v, t);
            if y == x {
                return Expr::Lam(y.clone(), *an, Box::new(t2), b.clone());
            }
            let fv_v = free_vars(v);
            if fv_v.contains(y) {
                let mut taken = free_vars(b);
                taken.extend(fv_v);
                taken.insert(x.to_string());
                let y2 = fresh_avoiding(y, &taken);
                let b2 = subst(y, &Expr::Var(y2.clone()), b);
                Expr::Lam(y2, *an, Box::new(t2), Box::new(subst(x, v, &b2)))
            } else {
                Expr::Lam(y.clone(), *an, Box::new(t2), Box::new(subst(x, v, b)))
            }
        }
        Expr::Pi(y, an, t, b) => {
            let t2 = subst(x, v, t);
            if y == x {
                return Expr::Pi(y.clone(), *an, Box::new(t2), b.clone());
            }
            let fv_v = free_vars(v);
            if fv_v.contains(y) {
                let mut taken = free_vars(b);
                taken.extend(fv_v);
                taken.insert(x.to_string());
                let y2 = fresh_avoiding(y, &taken);
                let b2 = subst(y, &Expr::Var(y2.clone()), b);
                Expr::Pi(y2, *an, Box::new(t2), Box::new(subst(x, v, &b2)))
            } else {
                Expr::Pi(y.clone(), *an, Box::new(t2), Box::new(subst(x, v, b)))
            }
        }
        Expr::Let(y, t, val, b) => {
            let t2 = subst(x, v, t);
            let val2 = subst(x, v, val);
            if y == x {
                return Expr::Let(y.clone(), Box::new(t2), Box::new(val2), b.clone());
            }
            let fv_v = free_vars(v);
            if fv_v.contains(y) {
                let mut taken = free_vars(b);
                taken.extend(fv_v);
                taken.insert(x.to_string());
                let y2 = fresh_avoiding(y, &taken);
                let b2 = subst(y, &Expr::Var(y2.clone()), b);
                Expr::Let(y2, Box::new(t2), Box::new(val2), Box::new(subst(x, v, &b2)))
            } else {
                Expr::Let(y.clone(), Box::new(t2), Box::new(val2), Box::new(subst(x, v, b)))
            }
        }
        Expr::Eq(t, a, b) => Expr::Eq(
            Box::new(subst(x, v, t)),
            Box::new(subst(x, v, a)),
            Box::new(subst(x, v, b)),
        ),
        Expr::And(a, b) => Expr::And(Box::new(subst(x, v, a)), Box::new(subst(x, v, b))),
        Expr::Or(a, b) => Expr::Or(Box::new(subst(x, v, a)), Box::new(subst(x, v, b))),
        Expr::Ite(c, t, f) => Expr::Ite(
            Box::new(subst(x, v, c)),
            Box::new(subst(x, v, t)),
            Box::new(subst(x, v, f)),
        ),
        Expr::By(ts) => Expr::By(ts.clone()),
    }
}

/// 把 hole id 換成 expr（tactic proof 組裝用）
pub fn subst_hole(h: usize, v: &Expr, e: &Expr) -> Expr {
    match e {
        Expr::Hole(i) if *i == h => v.clone(),
        Expr::Hole(_) | Expr::Sort(_) | Expr::Const(_) | Expr::Var(_) | Expr::Name(_) | Expr::Rfl | Expr::Sorry => {
            e.clone()
        }
        Expr::App(f, a) => app(subst_hole(h, v, f), subst_hole(h, v, a)),
        Expr::Lam(x, an, t, b) => Expr::Lam(
            x.clone(),
            *an,
            Box::new(subst_hole(h, v, t)),
            Box::new(subst_hole(h, v, b)),
        ),
        Expr::Pi(x, an, t, b) => Expr::Pi(
            x.clone(),
            *an,
            Box::new(subst_hole(h, v, t)),
            Box::new(subst_hole(h, v, b)),
        ),
        Expr::Let(x, t, val, b) => Expr::Let(
            x.clone(),
            Box::new(subst_hole(h, v, t)),
            Box::new(subst_hole(h, v, val)),
            Box::new(subst_hole(h, v, b)),
        ),
        Expr::Eq(t, a, b) => Expr::Eq(
            Box::new(subst_hole(h, v, t)),
            Box::new(subst_hole(h, v, a)),
            Box::new(subst_hole(h, v, b)),
        ),
        Expr::And(a, b) => Expr::And(Box::new(subst_hole(h, v, a)), Box::new(subst_hole(h, v, b))),
        Expr::Or(a, b) => Expr::Or(Box::new(subst_hole(h, v, a)), Box::new(subst_hole(h, v, b))),
        Expr::Ite(c, t, f) => Expr::Ite(
            Box::new(subst_hole(h, v, c)),
            Box::new(subst_hole(h, v, t)),
            Box::new(subst_hole(h, v, f)),
        ),
        Expr::By(ts) => Expr::By(ts.clone()),
    }
}

pub fn has_hole(e: &Expr) -> bool {
    match e {
        Expr::Hole(_) => true,
        Expr::Sort(_) | Expr::Const(_) | Expr::Var(_) | Expr::Name(_) | Expr::Rfl | Expr::Sorry => false,
        Expr::App(f, a) => has_hole(f) || has_hole(a),
        Expr::Lam(_, _, t, b) | Expr::Pi(_, _, t, b) => has_hole(t) || has_hole(b),
        Expr::Let(_, t, v, b) => has_hole(t) || has_hole(v) || has_hole(b),
        Expr::Eq(t, a, b) => has_hole(t) || has_hole(a) || has_hole(b),
        Expr::And(a, b) | Expr::Or(a, b) => has_hole(a) || has_hole(b),
        Expr::Ite(c, t, f) => has_hole(c) || has_hole(t) || has_hole(f),
        Expr::By(_) => false,
    }
}

pub fn hole_occurs(h: usize, e: &Expr, assign: &HashMap<usize, Expr>) -> bool {
    match e {
        Expr::Hole(i) => {
            if *i == h {
                return true;
            }
            match assign.get(i) {
                Some(v) => hole_occurs(h, v, assign),
                None => false,
            }
        }
        Expr::Sort(_) | Expr::Const(_) | Expr::Var(_) | Expr::Name(_) | Expr::Rfl | Expr::Sorry => false,
        Expr::App(f, a) => hole_occurs(h, f, assign) || hole_occurs(h, a, assign),
        Expr::Lam(_, _, t, b) | Expr::Pi(_, _, t, b) => {
            hole_occurs(h, t, assign) || hole_occurs(h, b, assign)
        }
        Expr::Let(_, t, v, b) => {
            hole_occurs(h, t, assign) || hole_occurs(h, v, assign) || hole_occurs(h, b, assign)
        }
        Expr::Eq(t, a, b) => {
            hole_occurs(h, t, assign) || hole_occurs(h, a, assign) || hole_occurs(h, b, assign)
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            hole_occurs(h, a, assign) || hole_occurs(h, b, assign)
        }
        Expr::Ite(c, t, f) => hole_occurs(h, c, assign) || hole_occurs(h, t, assign) || hole_occurs(h, f, assign),
        Expr::By(_) => false,
    }
}

// ---------- pretty printer ----------

pub fn pretty(e: &Expr) -> String {
    pretty_prec(e, 0)
}

fn par(s: String, inner: u8, outer: u8) -> String {
    if inner < outer {
        format!("({})", s)
    } else {
        s
    }
}

fn pretty_prec(e: &Expr, outer: u8) -> String {
    if let Some(n) = as_nat(e) {
        return n.to_string();
    }
    match e {
        Expr::Sort(Sort::Prop) => "Prop".to_string(),
        Expr::Sort(Sort::Typ) => "Type".to_string(),
        Expr::Name(n) | Expr::Var(n) | Expr::Const(n) => n.clone(),
        Expr::Hole(i) => format!("?m{}", i),
        Expr::Rfl => "rfl".to_string(),
        Expr::Sorry => "sorry".to_string(),
        Expr::By(_) => "by ...".to_string(),
        Expr::Eq(_, a, b) => par(
            format!("{} = {}", pretty_prec(a, 5), pretty_prec(b, 6)),
            5,
            outer,
        ),
        Expr::And(a, b) => par(
            format!("{} /\\ {}", pretty_prec(a, 4), pretty_prec(b, 5)),
            4,
            outer,
        ),
        Expr::Or(a, b) => par(
            format!("{} \\/ {}", pretty_prec(a, 3), pretty_prec(b, 4)),
            3,
            outer,
        ),
        Expr::Ite(c, t, f) => par(
            format!(
                "if {} then {} else {}",
                pretty_prec(c, 2),
                pretty_prec(t, 2),
                pretty_prec(f, 2)
            ),
            2,
            outer,
        ),
        Expr::App(_, _) => {
            let (hd, args) = collect_apps(e);
            // Nat.add / Nat.mul 中綴糖
            if let Expr::Const(h) = hd {
                if (h == "Nat.add" || h == "HAdd.hAdd") && args.len() == 2 {
                    let op = 6u8;
                    return par(
                        format!("{} + {}", pretty_prec(args[0], op), pretty_prec(args[1], op + 1)),
                        op,
                        outer,
                    );
                }
                if h == "Nat.mul" && args.len() == 2 {
                    let op = 7u8;
                    return par(
                        format!("{} * {}", pretty_prec(args[0], op), pretty_prec(args[1], op + 1)),
                        op,
                        outer,
                    );
                }
            }
            let mut s = pretty_prec(hd, 9);
            for a in args {
                s.push(' ');
                s.push_str(&pretty_prec(a, 10));
            }
            par(s, 9, outer)
        }
        Expr::Lam(x, an, t, b) => {
            let (open, close) = if *an == Ann::Implicit { ("{", "}") } else { ("(", ")") };
            par(
                format!("fun {}{} : {}{} => {}", open, x, pretty(t), close, pretty_prec(b, 1)),
                1,
                outer,
            )
        }
        Expr::Pi(x, an, t, b) => {
            if !free_vars(b).contains(x) && *an == Ann::Explicit {
                let op = 2u8; // -> 最低
                return par(
                    format!("{} -> {}", pretty_prec(t, 3), pretty_prec(b, 2)),
                    op,
                    outer,
                );
            }
            let (open, close) = if *an == Ann::Implicit { ("{", "}") } else { ("(", ")") };
            par(
                format!(
                    "forall {}{} : {}{}, {}",
                    open,
                    x,
                    pretty(t),
                    close,
                    pretty_prec(b, 1)
                ),
                1,
                outer,
            )
        }
        Expr::Let(x, t, v, b) => par(
            format!("let {} : {} := {}; {}", x, pretty(t), pretty(v), pretty_prec(b, 1)),
            1,
            outer,
        ),
    }
}

pub fn pretty_tactic(t: &Tactic) -> String {
    match t {
        Tactic::Intro(xs) => format!("intro {}", xs.join(" ")),
        Tactic::Exact(e) => format!("exact {}", pretty(e)),
        Tactic::Apply(e) => format!("apply {}", pretty(e)),
        Tactic::Rfl => "rfl".to_string(),
        Tactic::Simp(args) => {
            if args.is_empty() {
                "simp".to_string()
            } else {
                format!(
                    "simp [{}]",
                    args.iter().map(pretty).collect::<Vec<_>>().join(", ")
                )
            }
        }
        Tactic::Rw(rs) => {
            let parts: Vec<String> = rs
                .iter()
                .map(|(rev, e)| if *rev { format!("<- {}", pretty(e)) } else { pretty(e) })
                .collect();
            format!("rw [{}]", parts.join(", "))
        }
        Tactic::Cases(h, ns) => {
            if ns.is_empty() {
                format!("cases {}", h)
            } else {
                format!("cases {} with {}", h, ns.join(" "))
            }
        }
        Tactic::Left => "left".to_string(),
        Tactic::Right => "right".to_string(),
        Tactic::Constructor => "constructor".to_string(),
        Tactic::Trivial => "trivial".to_string(),
        Tactic::Have(n, t, v) => format!("have {} : {} := {}", n, pretty(t), pretty(v)),
        Tactic::Induction(n, ns) => {
            if ns.is_empty() {
                format!("induction {}", n)
            } else {
                format!("induction {} with {}", n, ns.join(" "))
            }
        }
        Tactic::Show(e) => format!("show {}", pretty(e)),
        Tactic::Assumption => "assumption".to_string(),
        Tactic::Sorry => "sorry".to_string(),
        Tactic::Case(t) => format!("case {}", t),
        Tactic::AllGoals(t) => format!("all_goals {}", pretty_tactic(t)),
    }
}

/// 收集所有子項（含自己），pre-order（rw pattern matching 用）
pub fn subterms(e: &Expr) -> Vec<Expr> {
    let mut out = vec![];
    go(e, &mut out);
    out
}

fn go(e: &Expr, out: &mut Vec<Expr>) {
    out.push(e.clone());
    match e {
        Expr::App(f, a) => {
            go(f, out);
            go(a, out);
        }
        Expr::Lam(_, _, t, b) | Expr::Pi(_, _, t, b) => {
            go(t, out);
            go(b, out);
        }
        Expr::Let(_, t, v, b) => {
            go(t, out);
            go(v, out);
            go(b, out);
        }
        Expr::Eq(t, a, b) => {
            go(t, out);
            go(a, out);
            go(b, out);
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            go(a, out);
            go(b, out);
        }
        Expr::Ite(c, t, f) => {
            go(c, out);
            go(t, out);
            go(f, out);
        }
        _ => {}
    }
}

/// syntactic 相等下的全域取代（rw 實際改寫用；match 階段已用 defeq 找到實例）
pub fn replace_syntactic(target: &Expr, pat: &Expr, rep: &Expr) -> Expr {
    if target == pat {
        return rep.clone();
    }
    match target {
        Expr::App(f, a) => app(
            replace_syntactic(f, pat, rep),
            replace_syntactic(a, pat, rep),
        ),
        Expr::Lam(x, an, t, b) => Expr::Lam(
            x.clone(),
            *an,
            Box::new(replace_syntactic(t, pat, rep)),
            Box::new(replace_syntactic(b, pat, rep)),
        ),
        Expr::Pi(x, an, t, b) => Expr::Pi(
            x.clone(),
            *an,
            Box::new(replace_syntactic(t, pat, rep)),
            Box::new(replace_syntactic(b, pat, rep)),
        ),
        Expr::Let(x, t, v, b) => Expr::Let(
            x.clone(),
            Box::new(replace_syntactic(t, pat, rep)),
            Box::new(replace_syntactic(v, pat, rep)),
            Box::new(replace_syntactic(b, pat, rep)),
        ),
        Expr::Eq(t, a, b) => Expr::Eq(
            Box::new(replace_syntactic(t, pat, rep)),
            Box::new(replace_syntactic(a, pat, rep)),
            Box::new(replace_syntactic(b, pat, rep)),
        ),
        Expr::And(a, b) => Expr::And(
            Box::new(replace_syntactic(a, pat, rep)),
            Box::new(replace_syntactic(b, pat, rep)),
        ),
        Expr::Or(a, b) => Expr::Or(
            Box::new(replace_syntactic(a, pat, rep)),
            Box::new(replace_syntactic(b, pat, rep)),
        ),
        Expr::Ite(c, t, f) => Expr::Ite(
            Box::new(replace_syntactic(c, pat, rep)),
            Box::new(replace_syntactic(t, pat, rep)),
            Box::new(replace_syntactic(f, pat, rep)),
        ),
        _ => target.clone(),
    }
}
