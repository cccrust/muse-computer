//! Micro-Lean4 kernel：whnf（含 delta/beta/zeta/iota + Nat/Bool/Eq 歸約）、
//! defeq（含 hole 指派 = 簡易 unification）、bidirectional infer/check、stdlib。
//!
//! 簡化聲明：Type-in-Type（`Type : Type`，不做 universe 檢查）；
//! theorem 的 proof 不參與 delta（只有 def/abbrev 可歸約）。

use crate::ast::*;
use std::collections::HashMap;

pub struct Def {
    pub ty: Expr,
    pub val: Option<Expr>,
    pub reducible: bool,
}

pub struct Env {
    pub defs: HashMap<String, Def>,
}

pub struct Elab {
    pub next_hole: usize,
    pub assign: HashMap<usize, Expr>,
    pub warnings: Vec<String>,
}

pub type Ctx = Vec<(String, Expr)>;

impl Elab {
    pub fn new(next_hole: usize) -> Self {
        Elab {
            next_hole,
            assign: HashMap::new(),
            warnings: vec![],
        }
    }
    pub fn fresh(&mut self) -> usize {
        let h = self.next_hole;
        self.next_hole += 1;
        h
    }
    pub fn snapshot(&self) -> HashMap<usize, Expr> {
        self.assign.clone()
    }
    pub fn restore(&mut self, s: HashMap<usize, Expr>) {
        self.assign = s;
    }
}

// ---------- zonk ----------

pub fn zonk(el: &Elab, e: &Expr) -> Expr {
    match e {
        Expr::Hole(i) => match el.assign.get(i) {
            Some(v) => zonk(el, v),
            None => e.clone(),
        },
        Expr::App(f, a) => app(zonk(el, f), zonk(el, a)),
        Expr::Lam(x, an, t, b) => Expr::Lam(
            x.clone(),
            *an,
            Box::new(zonk(el, t)),
            Box::new(zonk(el, b)),
        ),
        Expr::Pi(x, an, t, b) => Expr::Pi(
            x.clone(),
            *an,
            Box::new(zonk(el, t)),
            Box::new(zonk(el, b)),
        ),
        Expr::Let(x, t, v, b) => Expr::Let(
            x.clone(),
            Box::new(zonk(el, t)),
            Box::new(zonk(el, v)),
            Box::new(zonk(el, b)),
        ),
        Expr::Eq(t, a, b) => mk_eq(zonk(el, t), zonk(el, a), zonk(el, b)),
        Expr::And(a, b) => Expr::And(Box::new(zonk(el, a)), Box::new(zonk(el, b))),
        Expr::Or(a, b) => Expr::Or(Box::new(zonk(el, a)), Box::new(zonk(el, b))),
        Expr::Ite(c, t, f) => Expr::Ite(
            Box::new(zonk(el, c)),
            Box::new(zonk(el, t)),
            Box::new(zonk(el, f)),
        ),
        _ => e.clone(),
    }
}

// ---------- whnf ----------

const WHNF_FUEL: i32 = 2000;

pub fn whnf(env: &Env, el: &Elab, e: &Expr) -> Expr {
    let mut fuel = WHNF_FUEL;
    whnf_fuel(env, el, e, &mut fuel)
}

fn is_nat_zero(e: &Expr) -> bool {
    matches!(e, Expr::Const(s) if s == "Nat.zero")
}

fn as_succ(e: &Expr) -> Option<Expr> {
    if let Expr::App(f, a) = e {
        if let Expr::Const(s) = f.as_ref() {
            if s == "Nat.succ" {
                return Some(a.as_ref().clone());
            }
        }
    }
    None
}

fn whnf_fuel(env: &Env, el: &Elab, e: &Expr, fuel: &mut i32) -> Expr {
    *fuel -= 1;
    if *fuel <= 0 {
        return zonk(el, e);
    }
    let e = zonk(el, e);
    match e {
        Expr::App(f, a) => {
            let (hd, mut args) = {
                let mut as_ = vec![*a.clone()];
                let mut h = *f.clone();
                while let Expr::App(f2, a2) = h {
                    as_.push(*a2);
                    h = *f2;
                }
                as_.reverse();
                (h, as_)
            };
            let mut head = whnf_fuel(env, el, &hd, fuel);
            loop {
                // beta
                if matches!(head, Expr::Lam(..)) {
                    if args.is_empty() {
                        return finish_app(head, args);
                    }
                    if let Expr::Lam(x, _, _, body) = head {
                        let arg = args.remove(0);
                        let r = subst(&x, &arg, &body);
                        if args.is_empty() {
                            return whnf_fuel(env, el, &r, fuel);
                        } else {
                            let rw = whnf_fuel(env, el, &r, fuel);
                            // r 的 whnf 可能是 Lam（繼續 beta）或中性 head
                            match rw {
                                Expr::Lam(..) => {
                                    head = rw;
                                    continue;
                                }
                                _ => {
                                    return finish_app(rw, args);
                                }
                            }
                        }
                    } else {
                        unreachable!()
                    }
                }
                if let Expr::Const(c) = &head {
                    // 原子 Nat.add / Nat.mul / recursor / Eq.subst
                    if let Some(r) = prim_reduce(env, el, c, &mut args, fuel) {
                        // 歸約成功：r 可能還能繼續（head 位置）
                        let rw = whnf_fuel(env, el, &r, fuel);
                        match rw {
                            Expr::Lam(..) => {
                                head = rw;
                                continue;
                            }
                            Expr::Const(_) => {
                                head = rw;
                                continue;
                            }
                            _ => {
                                return finish_app(rw, args);
                            }
                        }
                    }
                    // 一般 def 的 delta（只有 reducible 才展開；theorem 不展開）
                    if let Some(d) = env.defs.get(c) {
                        if d.reducible {
                            if let Some(v) = &d.val {
                                *fuel -= 1;
                                head = whnf_fuel(env, el, v, fuel);
                                continue;
                            }
                        }
                    }
                    return finish_app(Expr::Const(c.clone()), args);
                }
                return finish_app(head, args);
            }
        }
        Expr::Let(x, _, v, b) => {
            let r = subst(&x, &v, &b);
            whnf_fuel(env, el, &r, fuel)
        }
        Expr::Const(c) => match env.defs.get(c.as_str()) {
            Some(d) if d.reducible => match &d.val {
                Some(v) => whnf_fuel(env, el, &v.clone(), fuel),
                None => Expr::Const(c),
            },
            _ => Expr::Const(c),
        },
        Expr::Ite(c, t, f) => {
            let wc = whnf_fuel(env, el, &c, fuel);
            if let Expr::Const(s) = &wc {
                if s == "true" {
                    return whnf_fuel(env, el, &t, fuel);
                }
                if s == "false" {
                    return whnf_fuel(env, el, &f, fuel);
                }
            }
            Expr::Ite(Box::new(wc), t, f)
        }
        _ => e,
    }
}

fn finish_app(head: Expr, args: Vec<Expr>) -> Expr {
    mk_apps(head, args)
}

/// 對 `Const(c)` + args 做 primitive 歸約。成功回傳 Some(歸約結果) 並
/// 消耗（drain）用掉的 args；參數不足或卡住回傳 None。
fn prim_reduce(
    env: &Env,
    el: &Elab,
    c: &str,
    args: &mut Vec<Expr>,
    fuel: &mut i32,
) -> Option<Expr> {
    match c {
        "Nat.add" => {
            if args.len() < 2 {
                return None;
            }
            let b = whnf_fuel(env, el, &args[1], fuel);
            if is_nat_zero(&b) {
                let a = args.remove(0);
                args.remove(0);
                Some(a)
            } else if let Some(m) = as_succ(&b) {
                let a = args[0].clone();
                args.remove(0);
                args.remove(0);
                let inner = mk_apps(Expr::Const("Nat.add".to_string()), vec![a, m]);
                Some(app(Expr::Const("Nat.succ".to_string()), inner))
            } else {
                None
            }
        }
        "Nat.mul" => {
            if args.len() < 2 {
                return None;
            }
            let b = whnf_fuel(env, el, &args[1], fuel);
            if is_nat_zero(&b) {
                args.remove(0);
                args.remove(0);
                Some(Expr::Const("Nat.zero".to_string()))
            } else if let Some(m) = as_succ(&b) {
                let a = args[0].clone();
                args.remove(0);
                args.remove(0);
                let inner_mul = mk_apps(Expr::Const("Nat.mul".to_string()), vec![a.clone(), m]);
                Some(mk_apps(Expr::Const("Nat.add".to_string()), vec![inner_mul, a]))
            } else {
                None
            }
        }
        "Nat.rec" => {
            if args.len() < 4 {
                return None;
            }
            let n = whnf_fuel(env, el, &args[3], fuel);
            if is_nat_zero(&n) {
                let z = args[1].clone();
                for _ in 0..4 {
                    args.remove(0);
                }
                Some(z)
            } else if let Some(m) = as_succ(&n) {
                let (motive, z, s) = (args[0].clone(), args[1].clone(), args[2].clone());
                for _ in 0..4 {
                    args.remove(0);
                }
                let rec_m = mk_apps(
                    Expr::Const("Nat.rec".to_string()),
                    vec![motive, z, s.clone(), m.clone()],
                );
                Some(mk_apps(s, vec![m, rec_m]))
            } else {
                None
            }
        }
        "Bool.rec" => {
            if args.len() < 4 {
                return None;
            }
            let b = whnf_fuel(env, el, &args[3], fuel);
            if let Expr::Const(s) = &b {
                if s == "true" || s == "false" {
                    let r = args[if s == "true" { 1 } else { 2 }].clone();
                    for _ in 0..4 {
                        args.remove(0);
                    }
                    return Some(r);
                }
            }
            None
        }
        "Eq.subst" => {
            if args.len() < 6 {
                return None;
            }
            let h = whnf_fuel(env, el, &args[4], fuel);
            if matches!(h, Expr::Rfl) {
                let pa = args[5].clone();
                for _ in 0..6 {
                    args.remove(0);
                }
                Some(pa)
            } else {
                None
            }
        }
        _ => None,
    }
}

// ---------- full normalize（#eval / simp 用） ----------

pub fn normalize(env: &Env, el: &Elab, e: &Expr) -> Expr {
    let mut budget = 50_000i64;
    norm(env, el, e, &mut budget)
}

fn norm(env: &Env, el: &Elab, e: &Expr, b: &mut i64) -> Expr {
    *b -= 1;
    if *b <= 0 {
        return zonk(el, e);
    }
    let w = whnf(env, el, e);
    match w {
        Expr::App(f, a) => app(norm(env, el, &f, b), norm(env, el, &a, b)),
        Expr::Lam(x, an, t, body) => Expr::Lam(
            x,
            an,
            Box::new(norm(env, el, &t, b)),
            Box::new(norm(env, el, &body, b)),
        ),
        Expr::Pi(x, an, t, body) => Expr::Pi(
            x,
            an,
            Box::new(norm(env, el, &t, b)),
            Box::new(norm(env, el, &body, b)),
        ),
        Expr::Let(x, t, v, body) => Expr::Let(
            x,
            Box::new(norm(env, el, &t, b)),
            Box::new(norm(env, el, &v, b)),
            Box::new(norm(env, el, &body, b)),
        ),
        Expr::Eq(t, a, c) => mk_eq(norm(env, el, &t, b), norm(env, el, &a, b), norm(env, el, &c, b)),
        Expr::And(a, c) => Expr::And(Box::new(norm(env, el, &a, b)), Box::new(norm(env, el, &c, b))),
        Expr::Or(a, c) => Expr::Or(Box::new(norm(env, el, &a, b)), Box::new(norm(env, el, &c, b))),
        Expr::Ite(c, t, f) => Expr::Ite(
            Box::new(norm(env, el, &c, b)),
            Box::new(norm(env, el, &t, b)),
            Box::new(norm(env, el, &f, b)),
        ),
        other => other,
    }
}

// ---------- defeq（含 hole 指派） ----------

pub fn defeq(env: &Env, el: &mut Elab, a: &Expr, b: &Expr) -> bool {
    deq(env, el, a, b, 300)
}

fn deq(env: &Env, el: &mut Elab, a: &Expr, b: &Expr, d: i32) -> bool {
    if d <= 0 {
        return false;
    }
    let a = whnf(env, el, a);
    let b = whnf(env, el, b);
    match (&a, &b) {
        (Expr::Hole(h), _) => return assign_hole(el, *h, &b, env, d),
        (_, Expr::Hole(h)) => return assign_hole(el, *h, &a, env, d),
        _ => {}
    }
    // cumulativity（簡化）：Prop ⊆ Type。Nat.rec / False.elim 的 motive 需要它。
    if matches!((&a, &b), (Expr::Sort(Sort::Prop), Expr::Sort(Sort::Typ))) {
        return true;
    }
    match (a, b) {
        (Expr::Sort(s1), Expr::Sort(s2)) => s1 == s2,
        (Expr::Var(x), Expr::Var(y)) => x == y,
        (Expr::Const(x), Expr::Const(y)) => x == y,
        (Expr::Name(x), Expr::Name(y)) => x == y,
        (Expr::Name(x), Expr::Var(y)) | (Expr::Var(x), Expr::Name(y)) => x == y,
        (Expr::Name(x), Expr::Const(y)) | (Expr::Const(x), Expr::Name(y)) => x == y,
        (Expr::App(f1, a1), Expr::App(f2, a2)) => {
            deq(env, el, &f1, &f2, d - 1) && deq(env, el, &a1, &a2, d - 1)
        }
        (Expr::Lam(x1, _, t1, b1), Expr::Lam(x2, _, t2, b2)) => {
            if !deq(env, el, &t1, &t2, d - 1) {
                return false;
            }
            let f = fresh_name("eq");
            let fv = Expr::Var(f.clone());
            deq(env, el, &subst(&x1, &fv, &b1), &subst(&x2, &fv, &b2), d - 1)
        }
        (Expr::Pi(x1, _, t1, b1), Expr::Pi(x2, _, t2, b2)) => {
            if !deq(env, el, &t1, &t2, d - 1) {
                return false;
            }
            let f = fresh_name("eq");
            let fv = Expr::Var(f.clone());
            deq(env, el, &subst(&x1, &fv, &b1), &subst(&x2, &fv, &b2), d - 1)
        }
        (Expr::Let(x1, _, v1, b1), other) => deq(env, el, &subst(&x1, &v1, &b1), &other, d - 1),
        (other, Expr::Let(x2, _, v2, b2)) => deq(env, el, &other, &subst(&x2, &v2, &b2), d - 1),
        (Expr::Eq(t1, a1, b1), Expr::Eq(t2, a2, b2)) => {
            deq(env, el, &t1, &t2, d - 1) && deq(env, el, &a1, &a2, d - 1) && deq(env, el, &b1, &b2, d - 1)
        }
        (Expr::And(a1, b1), Expr::And(a2, b2)) | (Expr::Or(a1, b1), Expr::Or(a2, b2)) => {
            deq(env, el, &a1, &a2, d - 1) && deq(env, el, &b1, &b2, d - 1)
        }
        (Expr::Ite(c1, t1, f1), Expr::Ite(c2, t2, f2)) => {
            deq(env, el, &c1, &c2, d - 1)
                && deq(env, el, &t1, &t2, d - 1)
                && deq(env, el, &f1, &f2, d - 1)
        }
        (Expr::Rfl, Expr::Rfl) => true,
        (Expr::Sorry, Expr::Sorry) => true,
        _ => false,
    }
}

fn assign_hole(el: &mut Elab, h: usize, v: &Expr, _env: &Env, _d: i32) -> bool {
    if *v == Expr::Hole(h) {
        return true;
    }
    if hole_occurs(h, v, &el.assign) {
        return false; // occurs check
    }
    el.assign.insert(h, v.clone());
    true
}

// ---------- infer / check ----------

fn lookup_ctx<'a>(ctx: &'a [(String, Expr)], n: &str) -> Option<Expr> {
    ctx.iter().rev().find(|(x, _)| x == n).map(|(_, t)| t.clone())
}

fn ensure_sort(env: &Env, el: &mut Elab, t: &Expr) -> Result<Sort, String> {
    match whnf(env, el, t) {
        Expr::Sort(s) => Ok(s),
        other => Err(format!("expected a type (Sort), got `{}`", pretty(&other))),
    }
}

/// 給 driver 用的公開版本
pub fn ensure_sort_pub(env: &Env, el: &mut Elab, t: &Expr) -> Result<(), String> {
    ensure_sort(env, el, t).map(|_| ())
}

pub fn infer(env: &Env, el: &mut Elab, ctx: &[(String, Expr)], e: &Expr) -> Result<Expr, String> {
    match e {
        Expr::Sort(Sort::Prop) => Ok(Expr::Sort(Sort::Typ)),
        Expr::Sort(Sort::Typ) => Ok(Expr::Sort(Sort::Typ)), // Type-in-Type（簡化）
        Expr::Name(n) => match lookup_ctx(ctx, n) {
            Some(t) => Ok(t),
            None => match env.defs.get(n) {
                Some(d) => Ok(d.ty.clone()),
                None => Err(format!("unknown identifier `{}`", n)),
            },
        },
        Expr::Var(x) => match lookup_ctx(ctx, x) {
            Some(t) => Ok(t),
            None => Err(format!("unbound variable `{}`", x)),
        },
        Expr::Const(c) => match env.defs.get(c) {
            Some(d) => Ok(d.ty.clone()),
            None => Err(format!("unknown constant `{}`", c)),
        },
        Expr::App(f, a) => {
            let tf = infer(env, el, ctx, f)?;
            match whnf(env, el, &tf) {
                Expr::Pi(x, _, dom, cod) => {
                    check(env, el, ctx, a, &dom)?;
                    Ok(subst(&x, a, &cod))
                }
                other => Err(format!(
                    "not a function：`{}` 的型別是 `{}`",
                    pretty(f),
                    pretty(&other)
                )),
            }
        }
        Expr::Lam(x, an, ty, body) => {
            if matches!(ty.as_ref(), Expr::Hole(_)) {
                return Err(format!(
                    "`fun {}` 缺少型別標註（micro-Lean 無法推導 binder 型別，請寫 `fun {} : T => ...`）",
                    x, x
                ));
            }
            let tys = infer(env, el, ctx, ty)?;
            ensure_sort(env, el, &tys)?;
            let mut ctx2 = ctx.to_vec();
            ctx2.push((x.clone(), ty.as_ref().clone()));
            let tb = infer(env, el, &ctx2, body)?;
            Ok(Expr::Pi(x.clone(), *an, ty.clone(), Box::new(tb)))
        }
        Expr::Pi(x, _, dom, cod) => {
            let ds = infer(env, el, ctx, dom)?;
            ensure_sort(env, el, &ds)?;
            let mut ctx2 = ctx.to_vec();
            ctx2.push((x.clone(), dom.as_ref().clone()));
            let cs = infer(env, el, &ctx2, cod)?;
            let s = ensure_sort(env, el, &cs)?;
            Ok(if s == Sort::Prop {
                Expr::Sort(Sort::Prop)
            } else {
                Expr::Sort(Sort::Typ)
            })
        }
        Expr::Let(x, t, v, b) => {
            let vt = if matches!(t.as_ref(), Expr::Hole(_)) {
                infer(env, el, ctx, v)?
            } else {
                let ts = infer(env, el, ctx, t)?;
                ensure_sort(env, el, &ts)?;
                check(env, el, ctx, v, t)?;
                t.as_ref().clone()
            };
            let mut ctx2 = ctx.to_vec();
            ctx2.push((x.clone(), vt));
            let bt = infer(env, el, &ctx2, b)?;
            Ok(subst(x, v, &bt))
        }
        Expr::Hole(h) => Err(format!("stuck metavariable ?m{}（需要更多型別資訊）", h)),
        Expr::Eq(ty, a, b) => {
            // l 的型別 ta 必須是個 Type；r 必須同型；ty 與 ta 做 unification
            //（ty 是 parser 放的 hole 時，這裡把它指派為 ta）
            let ta = infer(env, el, ctx, a)?;
            let tas = infer(env, el, ctx, &ta)?;
            ensure_sort(env, el, &tas)?;
            check(env, el, ctx, b, &ta)?;
            if !defeq(env, el, ty, &ta) {
                return Err(format!(
                    "`=` 的型別參數 `{}` 與兩邊實際型別 `{}` 不合",
                    pretty(ty),
                    pretty(&ta)
                ));
            }
            Ok(Expr::Sort(Sort::Prop))
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            check(env, el, ctx, a, &Expr::Sort(Sort::Prop))?;
            check(env, el, ctx, b, &Expr::Sort(Sort::Prop))?;
            Ok(Expr::Sort(Sort::Prop))
        }
        Expr::Ite(c, t, f) => {
            check(env, el, ctx, c, &Expr::Const("Bool".to_string()))?;
            let tt = infer(env, el, ctx, t)?;
            check(env, el, ctx, f, &tt)?;
            Ok(tt)
        }
        Expr::Rfl => Err("`rfl` 需要預期型別（請用在 exact/rfl/show 或 theorem 證明位置）".to_string()),
        Expr::Sorry => Err("`sorry` 需要預期型別".to_string()),
        Expr::By(_) => Err("`by` 需要預期型別".to_string()),
    }
}

pub fn check(env: &Env, el: &mut Elab, ctx: &[(String, Expr)], e: &Expr, expected: &Expr) -> Result<(), String> {
    let exp = whnf(env, el, expected);
    match (e, &exp) {
        (Expr::Lam(x, _, ty1, body), Expr::Pi(y, _, dom, cod)) => {
            if !matches!(ty1.as_ref(), Expr::Hole(_)) && !defeq(env, el, ty1, dom) {
                return Err(format!(
                    "fun binder 型別不合：標註 `{}` vs 預期 `{}`",
                    pretty(ty1),
                    pretty(dom)
                ));
            }
            let f = open_name(ctx, x);
            let fv = Expr::Var(f.clone());
            let mut ctx2 = ctx.to_vec();
            ctx2.push((f, dom.as_ref().clone()));
            check(env, el, &ctx2, &subst(x, &fv, body), &subst(y, &fv, cod))
        }
        (Expr::Let(x, t, v, b), _) => {
            let vt = if matches!(t.as_ref(), Expr::Hole(_)) {
                infer(env, el, ctx, v)?
            } else {
                check(env, el, ctx, v, t)?;
                t.as_ref().clone()
            };
            let mut ctx2 = ctx.to_vec();
            ctx2.push((x.clone(), vt));
            check(env, el, &ctx2, b, &exp)
        }
        (Expr::Hole(h), _) => {
            if let Some(cur) = el.assign.get(h).cloned() {
                check(env, el, ctx, &cur, &exp)
            } else {
                if hole_occurs(*h, &exp, &el.assign) {
                    return Err("occurs check 失敗".to_string());
                }
                el.assign.insert(*h, exp.clone());
                Ok(())
            }
        }
        (Expr::Rfl, Expr::Eq(_, a, b)) => {
            if defeq(env, el, a, b) {
                Ok(())
            } else {
                Err(format!(
                    "`rfl` 失敗：`{}` 與 `{}` 在定義上不相等（提示：先 simp/rw 化簡）",
                    pretty(a),
                    pretty(b)
                ))
            }
        }
        (Expr::Rfl, other) => Err(format!("`rfl` 只能證明等式，目標是 `{}`", pretty(other))),
        (Expr::Sorry, _) => {
            el.warnings.push("warning: `sorry` 被接受為任意證明（axiom，后續證明不可信）".to_string());
            Ok(())
        }
        (Expr::By(tacs), _) => {
            let pf = crate::tactic::run_tactics(env, el, ctx.to_vec(), exp.clone(), tacs)?;
            check(env, el, ctx, &pf, &exp)
        }
        _ => {
            let t = infer(env, el, ctx, e)?;
            if defeq(env, el, &t, &exp) {
                Ok(())
            } else {
                Err(format!(
                    "type mismatch：需要 `{}`，但得到 `{}`",
                    pretty(&exp),
                    pretty(&t)
                ))
            }
        }
    }
}

/// 把 Pi 前綴全部實例化成 fresh holes，回傳 (args, conclusion)
pub fn instantiate_pis(
    env: &Env,
    el: &mut Elab,
    ty: &Expr,
) -> (Vec<(String, Ann, Expr, usize)>, Expr) {
    let mut args = vec![];
    let mut cur = whnf(env, el, ty);
    loop {
        match cur {
            Expr::Pi(x, an, dom, cod) => {
                let h = el.fresh();
                let domz = zonk(el, &dom);
                args.push((x.clone(), an, domz, h));
                cur = whnf(env, el, &subst(&x, &Expr::Hole(h), &cod));
            }
            _ => break,
        }
    }
    (args, cur)
}

/// Name 解析：Name(n) → bound 內則 Var，否則全局 Const，未知則錯。
/// 不進入 By（tactic 內容由執行它的 run_tactics 在已知 hyps 下解析）。
pub fn resolve(env: &Env, bound: &mut Vec<String>, e: &Expr) -> Result<Expr, String> {
    match e {
        Expr::Name(n) => {
            if bound.contains(n) {
                Ok(Expr::Var(n.clone()))
            } else if env.defs.contains_key(n) {
                Ok(Expr::Const(n.clone()))
            } else {
                Err(format!("unknown identifier `{}`", n))
            }
        }
        Expr::Var(_) | Expr::Const(_) | Expr::Sort(_) | Expr::Hole(_) | Expr::Rfl | Expr::Sorry => {
            Ok(e.clone())
        }
        Expr::App(f, a) => Ok(app(resolve(env, bound, f)?, resolve(env, bound, a)?)),
        Expr::Lam(x, an, t, b) => {
            let t2 = resolve(env, bound, t)?;
            bound.push(x.clone());
            let b2 = resolve(env, bound, b);
            bound.pop();
            Ok(Expr::Lam(x.clone(), *an, Box::new(t2), Box::new(b2?)))
        }
        Expr::Pi(x, an, t, b) => {
            let t2 = resolve(env, bound, t)?;
            bound.push(x.clone());
            let b2 = resolve(env, bound, b);
            bound.pop();
            Ok(Expr::Pi(x.clone(), *an, Box::new(t2), Box::new(b2?)))
        }
        Expr::Let(x, t, v, b) => {
            let t2 = resolve(env, bound, t)?;
            let v2 = resolve(env, bound, v)?;
            bound.push(x.clone());
            let b2 = resolve(env, bound, b);
            bound.pop();
            Ok(Expr::Let(x.clone(), Box::new(t2), Box::new(v2), Box::new(b2?)))
        }
        Expr::Eq(t, a, b) => Ok(mk_eq(resolve(env, bound, t)?, resolve(env, bound, a)?, resolve(env, bound, b)?)),
        Expr::And(a, b) => Ok(Expr::And(
            Box::new(resolve(env, bound, a)?),
            Box::new(resolve(env, bound, b)?),
        )),
        Expr::Or(a, b) => Ok(Expr::Or(
            Box::new(resolve(env, bound, a)?),
            Box::new(resolve(env, bound, b)?),
        )),
        Expr::Ite(c, t, f) => Ok(Expr::Ite(
            Box::new(resolve(env, bound, c)?),
            Box::new(resolve(env, bound, t)?),
            Box::new(resolve(env, bound, f)?),
        )),
        Expr::By(ts) => Ok(Expr::By(ts.clone())),
    }
}

/// 開啟 binder：ctx 沒撞名就保留原名（tactic 才能用原名指到它），否則 freshen。
pub fn open_name(ctx: &[(String, Expr)], x: &str) -> String {
    if ctx.iter().any(|(y, _)| y == x) {
        let taken: std::collections::HashSet<String> =
            ctx.iter().map(|(y, _)| y.clone()).collect();
        fresh_avoiding(x, &taken)
    } else {
        x.to_string()
    }
}

// ---------- stdlib ----------

fn c(s: &str) -> Expr {
    Expr::Const(s.to_string())
}
fn v(s: &str) -> Expr {
    Expr::Var(s.to_string())
}
fn pi_(x: &str, an: Ann, ty: Expr, body: Expr) -> Expr {
    Expr::Pi(x.to_string(), an, Box::new(ty), Box::new(body))
}
fn ex_(x: &str, ty: Expr, body: Expr) -> Expr {
    pi_(x, Ann::Explicit, ty, body)
}
fn im_(x: &str, ty: Expr, body: Expr) -> Expr {
    pi_(x, Ann::Implicit, ty, body)
}
fn tprop() -> Expr {
    Expr::Sort(Sort::Prop)
}
/// Eq(ty, lhs, rhs)
fn eq_(ty: Expr, l: Expr, r: Expr) -> Expr {
    mk_eq(ty, l, r)
}
fn ttype() -> Expr {
    Expr::Sort(Sort::Typ)
}
fn nat() -> Expr {
    c("Nat")
}

pub fn std_env() -> Env {
    let mut defs = HashMap::new();
    let mut add = |name: &str, ty: Expr| {
        defs.insert(
            name.to_string(),
            Def { ty, val: None, reducible: false },
        );
    };

    // --- Nat ---
    add("Nat", ttype());
    add("Nat.zero", nat());
    add("Nat.succ", ex_("_", nat(), nat()));
    add("Nat.add", ex_("_", nat(), ex_("_", nat(), nat())));
    add("Nat.mul", ex_("_", nat(), ex_("_", nat(), nat())));
    // Nat.rec : (motive : Nat -> Type) -> (z : motive zero) -> (s : ∀ n, motive n -> motive (succ n)) -> (n : Nat) -> motive n
    {
        let motive_ty = ex_("_", nat(), ttype());
        let mz = app(v("motive"), c("Nat.zero"));
        let mn = app(v("motive"), v("n"));
        let msn = app(v("motive"), app(c("Nat.succ"), v("n")));
        let s_ty = ex_("n", nat(), ex_("ih", mn.clone(), msn));
        let ty = ex_(
            "motive",
            motive_ty,
            ex_("z", mz, ex_("s", s_ty, ex_("n", nat(), app(v("motive"), v("n"))))),
        );
        add("Nat.rec", ty);
    }
    // --- Bool ---
    add("Bool", ttype());
    add("true", c("Bool"));
    add("false", c("Bool"));
    {
        let motive_ty = ex_("_", c("Bool"), ttype());
        let ty = ex_(
            "motive",
            motive_ty,
            ex_(
                "t",
                app(v("motive"), c("true")),
                ex_(
                    "f",
                    app(v("motive"), c("false")),
                    ex_("b", c("Bool"), app(v("motive"), v("b"))),
                ),
            ),
        );
        add("Bool.rec", ty);
    }
    // --- True / False ---
    add("True", tprop());
    add("True.intro", c("True"));
    add("False", tprop());
    add("False.elim", ex_("C", ttype(), ex_("_", c("False"), v("C"))));
    // --- And ---
    add(
        "And.intro",
        ex_("a", tprop(), ex_("b", tprop(), ex_("l", v("a"), ex_("r", v("b"), Expr::And(Box::new(v("a")), Box::new(v("b"))))))),
    );
    add(
        "And.left",
        ex_("a", tprop(), ex_("b", tprop(), ex_("h", Expr::And(Box::new(v("a")), Box::new(v("b"))), v("a")))),
    );
    add(
        "And.right",
        ex_("a", tprop(), ex_("b", tprop(), ex_("h", Expr::And(Box::new(v("a")), Box::new(v("b"))), v("b")))),
    );
    // --- Or ---
    add(
        "Or.inl",
        ex_("a", tprop(), ex_("b", tprop(), ex_("h", v("a"), Expr::Or(Box::new(v("a")), Box::new(v("b")))))),
    );
    add(
        "Or.inr",
        ex_("a", tprop(), ex_("b", tprop(), ex_("h", v("b"), Expr::Or(Box::new(v("a")), Box::new(v("b")))))),
    );
    // Or.elim : (a b C : Prop) -> (a -> C) -> (b -> C) -> Or a b -> C
    add(
        "Or.elim",
        ex_(
            "a",
            tprop(),
            ex_(
                "b",
                tprop(),
                ex_(
                    "C",
                    tprop(),
                    ex_(
                        "inl",
                        ex_("_", v("a"), v("C")),
                        ex_("inr", ex_("_", v("b"), v("C")), ex_("h", Expr::Or(Box::new(v("a")), Box::new(v("b"))), v("C"))),
                    ),
                ),
            ),
        ),
    );
    // --- Eq ---
    // Eq.refl : {A : Type} {a : A} -> a = a
    add("Eq.refl", im_("A", ttype(), im_("a", v("A"), eq_(v("A"), v("a"), v("a")))));
    add(
        "Eq.symm",
        im_(
            "A",
            ttype(),
            im_(
                "a",
                v("A"),
                im_("b", v("A"), ex_("h", eq_(v("A"), v("a"), v("b")), eq_(v("A"), v("b"), v("a")))),
            ),
        ),
    );
    add(
        "Eq.trans",
        im_(
            "A",
            ttype(),
            im_(
                "a",
                v("A"),
                im_(
                    "b",
                    v("A"),
                    im_(
                        "c",
                        v("A"),
                        ex_(
                            "h1",
                            eq_(v("A"), v("a"), v("b")),
                            ex_("h2", eq_(v("A"), v("b"), v("c")), eq_(v("A"), v("a"), v("c"))),
                        ),
                    ),
                ),
            ),
        ),
    );
    // Eq.subst : {A} {P : A -> Prop} {a b} (h : a = b) (pa : P a) : P b
    add(
        "Eq.subst",
        im_(
            "A",
            ttype(),
            im_(
                "P",
                ex_("_", v("A"), tprop()),
                im_(
                    "a",
                    v("A"),
                    im_(
                        "b",
                        v("A"),
                        ex_(
                            "h",
                            eq_(v("A"), v("a"), v("b")),
                            ex_("pa", app(v("P"), v("a")), app(v("P"), v("b"))),
                        ),
                    ),
                ),
            ),
        ),
    );
    // congrArg : {A B} {f : A -> B} {a b} (h : a = b) : f a = f b
    add(
        "congrArg",
        im_(
            "A",
            ttype(),
            im_(
                "B",
                ttype(),
                im_(
                    "f",
                    ex_("_", v("A"), v("B")),
                    im_(
                        "a",
                        v("A"),
                        im_(
                            "b",
                            v("A"),
                            ex_(
                                "h",
                                eq_(v("A"), v("a"), v("b")),
                                eq_(v("B"), app(v("f"), v("a")), app(v("f"), v("b"))),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    );

    Env { defs }
}
