//! Tactics 解釋器：hole-based proof 組裝 + 各 tactic 的證明項構造。
//!
//! 模型：整體 proof 是一棵含 Hole 的 Expr；每個 open goal 對應一個 hole id。
//! tactic 把 hole 填成部分證明項（含新的 holes），並把新 subgoals 拼回 goals[0] 的位置。
//! 最後 `run_tactics` 會用 kernel 全盤驗證，保證 tactic 的重構正確。

use crate::ast::*;
use crate::kernel::{defeq, infer, instantiate_pis, normalize, resolve, whnf, zonk, check, Elab, Env};
use std::collections::HashSet;

#[derive(Clone)]
pub struct Goal {
    pub hole: usize,
    pub hyps: Vec<(String, Expr)>,
    pub target: Expr,
    pub tag: Option<String>,
}

pub fn run_tactics(
    env: &Env,
    el: &mut Elab,
    hyps0: Vec<(String, Expr)>,
    target0: Expr,
    tacs: &[Tactic],
) -> Result<Expr, String> {
    let root = el.fresh();
    let mut goals = vec![Goal {
        hole: root,
        hyps: hyps0.clone(),
        target: zonk(el, &target0),
        tag: None,
    }];
    for (i, tac) in tacs.iter().enumerate() {
        run_single(env, el, &mut goals, tac)
            .map_err(|e| format!("tactic {} `{}` 失敗：{}", i + 1, pretty_tactic(tac), e))?;
    }
    if !goals.is_empty() {
        let mut msg = format!("還有 {} 個未解 goal", goals.len());
        for (i, g) in goals.iter().enumerate().take(3) {
            msg += &format!(
                "\n  goal {} [{}]:\n    hyps: {}\n    ⊢ {}",
                i + 1,
                g.tag.clone().unwrap_or_else(|| "-".to_string()),
                hyps_str(&g.hyps),
                pretty(&zonk(el, &g.target))
            );
        }
        return Err(msg);
    }
    let pf = zonk(el, &Expr::Hole(root));
    if has_hole(&pf) {
        return Err("proof 含有未解 metavariable（?m…），不完整".to_string());
    }
    // 最終 proof 的 Name 全部解析（hyps 都已被 binder 抽象，只剩全局名）
    let pf = resolve(env, &mut vec![], &pf).map_err(|e| format!("proof 名稱解析失敗：{}", e))?;
    // 注意：此處不做 kernel check，呼叫端（check 的 By 分支，進而 driver）一定會再驗證；
    // 重複驗證只會讓 sorry 警告重複出現。
    Ok(pf)
}

fn hyps_str(hyps: &[(String, Expr)]) -> String {
    hyps.iter()
        .map(|(n, t)| format!("{} : {}", n, pretty(t)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn take_goal(el: &mut Elab, goals: &mut Vec<Goal>) -> Result<Goal, String> {
    if goals.is_empty() {
        return Err("沒有 goal（前面的 tactic 已把所有 goal 關閉）".to_string());
    }
    let mut g = goals.remove(0);
    g.target = zonk(el, &g.target);
    g.hyps = g.hyps.into_iter().map(|(n, t)| (n, zonk(el, &t))).collect();
    Ok(g)
}

fn ensure_fresh(hyps: &[(String, Expr)], n: &str) -> Result<(), String> {
    if hyps.iter().any(|(x, _)| x == n) {
        Err(format!(
            "名字 `{}` 已經存在（micro-Lean 不允許 shadowing，請換個名字）",
            n
        ))
    } else {
        Ok(())
    }
}

fn find_hyp(hyps: &[(String, Expr)], n: &str) -> Result<Expr, String> {
    hyps.iter()
        .rev()
        .find(|(x, _)| x == n)
        .map(|(_, t)| t.clone())
        .ok_or_else(|| {
            format!(
                "找不到 `{}`（現有 hypotheses：{}）",
                n,
                hyps.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>().join(", ")
            )
        })
}

fn c(s: &str) -> Expr {
    Expr::Const(s.to_string())
}

/// 收集 expr 內未指派的 holes（已 zonk 追蹤）
fn holes_in(el: &Elab, e: &Expr) -> Vec<usize> {
    let mut out = vec![];
    go(el, e, &mut out);
    fn go(el: &Elab, e: &Expr, out: &mut Vec<usize>) {
        match e {
            Expr::Hole(h) => match el.assign.get(h) {
                Some(v) => go(el, &v.clone(), out),
                None => {
                    if !out.contains(h) {
                        out.push(*h);
                    }
                }
            },
            Expr::App(f, a) => {
                go(el, f, out);
                go(el, a, out);
            }
            Expr::Lam(_, _, t, b) | Expr::Pi(_, _, t, b) => {
                go(el, t, out);
                go(el, b, out);
            }
            Expr::Let(_, t, v, b) => {
                go(el, t, out);
                go(el, v, out);
                go(el, b, out);
            }
            Expr::Eq(t, a, b) => {
                go(el, t, out);
                go(el, a, out);
                go(el, b, out);
            }
            Expr::And(a, b) | Expr::Or(a, b) => {
                go(el, a, out);
                go(el, b, out);
            }
            Expr::Ite(c, t, f) => {
                go(el, c, out);
                go(el, t, out);
                go(el, f, out);
            }
            _ => {}
        }
    }
    out
}

/// tactic 項的名稱解析（相對目前 goal 的 hyps；不進入 By，留給巢狀 run_tactics）
fn res(env: &Env, hyps: &[(String, Expr)], e: &Expr) -> Result<Expr, String> {
    let mut bound: Vec<String> = hyps.iter().map(|(n, _)| n.clone()).collect();
    resolve(env, &mut bound, e)
}

// ---------- run_single ----------

fn run_single(env: &Env, el: &mut Elab, goals: &mut Vec<Goal>, tac: &Tactic) -> Result<(), String> {
    match tac {
        Tactic::AllGoals(inner) => {
            let cur = std::mem::take(goals);
            if cur.is_empty() {
                return Err("沒有 goal".to_string());
            }
            for g in cur {
                goals.push(g);
                run_single(env, el, goals, inner)?;
            }
            Ok(())
        }
        Tactic::Case(tag) => {
            let pos = goals.iter().position(|g| g.tag.as_deref() == Some(tag.as_str()));
            match pos {
                Some(0) => Ok(()),
                Some(i) => {
                    let g = goals.remove(i);
                    goals.insert(0, g);
                    Ok(())
                }
                None => Err(format!(
                    "找不到 tag 為 `{}` 的 goal（現有 tags：{}）",
                    tag,
                    goals
                        .iter()
                        .map(|g| g.tag.clone().unwrap_or_else(|| "-".to_string()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        Tactic::Intro(xs) => do_intro(env, el, goals, xs),
        Tactic::Exact(e) => {
            let g = take_goal(el, goals)?;
            let e_r = res(env, &g.hyps, e)?;
            check(env, el, &g.hyps, &e_r, &g.target)
                .map_err(|e| format!("exact 的證明項型別不合：{}", e))?;
            let p = zonk(el, &e_r);
            el.assign.insert(g.hole, p);
            Ok(())
        }
        Tactic::Apply(e) => do_apply(env, el, goals, e),
        Tactic::Rfl => {
            let g = take_goal(el, goals)?;
            check(env, el, &g.hyps, &Expr::Rfl, &g.target)?;
            el.assign.insert(g.hole, Expr::Rfl);
            Ok(())
        }
        Tactic::Assumption => {
            let g = take_goal(el, goals)?;
            for (n, t) in &g.hyps {
                let snap = el.snapshot();
                if defeq(env, el, t, &g.target) {
                    el.assign.insert(g.hole, Expr::Var(n.clone()));
                    return Ok(());
                }
                el.restore(snap);
            }
            Err(format!(
                "assumption 找不到相符 hypothesis（目標 `{}`；hyps：{}）",
                pretty(&g.target),
                hyps_str(&g.hyps)
            ))
        }
        Tactic::Trivial => {
            // 依序嘗試，失敗要完整回溯（含 goals）
            let saved_goals = goals.clone();
            let mut last_err = String::new();
            for attempt in [&Tactic::Assumption, &Tactic::Rfl, &Tactic::Constructor] {
                let mut tmp = saved_goals.clone();
                let snap = el.snapshot();
                match run_single(env, el, &mut tmp, attempt) {
                    Ok(()) => {
                        *goals = tmp;
                        return Ok(());
                    }
                    Err(e) => {
                        last_err = e;
                        el.restore(snap);
                    }
                }
            }
            Err(format!("trivial 失敗（試過 assumption / rfl / constructor）：{}", last_err))
        }
        Tactic::Constructor => do_constructor(env, el, goals),
        Tactic::Left => do_side(env, el, goals, true),
        Tactic::Right => do_side(env, el, goals, false),
        Tactic::Show(e) => {
            let g = take_goal(el, goals)?;
            let e_r = res(env, &g.hyps, e)?;
            let e = &e_r;
            let t = infer(env, el, &g.hyps, e)
                .map_err(|x| format!("show 的型別推導失敗：{}", x))?;
            // 必須是 Type/Prop
            match whnf(env, el, &t) {
                Expr::Sort(_) => {}
                other => return Err(format!("show 的不是型別：`{}`", pretty(&other))),
            }
            if !defeq(env, el, e, &g.target) {
                return Err(format!(
                    "show：`{}` 與目前目標 `{}` 不相等",
                    pretty(e),
                    pretty(&g.target)
                ));
            }
            goals.insert(0, Goal { hole: g.hole, hyps: g.hyps, target: zonk(el, e), tag: g.tag });
            Ok(())
        }
        Tactic::Sorry => {
            let g = take_goal(el, goals)?;
            el.warnings.push(format!(
                "warning: goal `{}` 用 sorry 承認",
                pretty(&g.target)
            ));
            // sorry 一併寬恕該 goal 內尚未解的 holes（指派為 sorry；與 sorry 語義一致）。
            // 注意：若先 sorry 再 exact 共用 hole 的 goal，exact 可能因 sorry 污染而失敗；
            // 建議先 exact 再 sorry，或全 sorry。
            for h in holes_in(el, &g.target) {
                el.assign.insert(h, Expr::Sorry);
            }
            el.assign.insert(g.hole, Expr::Sorry);
            Ok(())
        }
        Tactic::Have(n, t, v) => {
            let g = take_goal(el, goals)?;
            ensure_fresh(&g.hyps, n)?;
            let t_r = res(env, &g.hyps, t)?;
            let v_r = res(env, &g.hyps, v)?;
            let (t, v) = (&t_r, &v_r);
            let tz = zonk(el, t);
            // T 必須是型別
            let ts = infer(env, el, &g.hyps, &tz)
                .map_err(|x| format!("have 的型別推導失敗：{}", x))?;
            match whnf(env, el, &ts) {
                Expr::Sort(_) => {}
                other => return Err(format!("have 的不是型別：`{}`", pretty(&other))),
            }
            check(env, el, &g.hyps, v, &tz)
                .map_err(|x| format!("have 的證明項型別不合：{}", x))?;
            let (tz2, vz) = (zonk(el, &tz), zonk(el, v));
            let nh = el.fresh();
            el.assign.insert(
                g.hole,
                Expr::Let(n.clone(), Box::new(tz2.clone()), Box::new(vz), Box::new(Expr::Hole(nh))),
            );
            let mut hyps = g.hyps;
            hyps.push((n.clone(), tz2));
            goals.insert(0, Goal { hole: nh, hyps, target: g.target, tag: g.tag });
            Ok(())
        }
        Tactic::Rw(rs) => {
            for (rev, r) in rs {
                do_rewrite(env, el, goals, *rev, r)?;
            }
            Ok(())
        }
        Tactic::Simp(ls) => do_simp(env, el, goals, ls),
        Tactic::Cases(h, ns) => do_cases(env, el, goals, h, ns),
        Tactic::Induction(n, ns) => do_induction(env, el, goals, n, ns),
    }
}

// ---------- intro ----------

fn do_intro(env: &Env, el: &mut Elab, goals: &mut Vec<Goal>, xs: &[String]) -> Result<(), String> {
    let mut g = take_goal(el, goals)?;
    let mut lams: Vec<(String, Ann, Expr)> = vec![];
    let mut target = g.target;
    for x in xs {
        match whnf(env, el, &target) {
            Expr::Pi(y, an, dom, cod) => {
                ensure_fresh(&g.hyps, x)?;
                let domz = zonk(el, &dom);
                lams.push((x.clone(), an, domz.clone()));
                g.hyps.push((x.clone(), domz));
                target = subst(&y, &Expr::Var(x.clone()), &cod);
            }
            other => {
                return Err(format!(
                    "`intro {}` 失敗：目標不是 forall/->，而是 `{}`",
                    x,
                    pretty(&other)
                ))
            }
        }
    }
    let nh = el.fresh();
    let mut p = Expr::Hole(nh);
    for (x, an, dom) in lams.into_iter().rev() {
        p = Expr::Lam(x, an, Box::new(dom), Box::new(p));
    }
    el.assign.insert(g.hole, p);
    goals.insert(0, Goal { hole: nh, hyps: g.hyps, target: zonk(el, &target), tag: g.tag });
    Ok(())
}

// ---------- apply ----------

fn do_apply(env: &Env, el: &mut Elab, goals: &mut Vec<Goal>, e: &Expr) -> Result<(), String> {
    let g = take_goal(el, goals)?;
    let e_r = res(env, &g.hyps, e)?;
    let e = &e_r;
    let ty = infer(env, el, &g.hyps, e)
        .map_err(|x| format!("apply 的引數型別推導失敗：{}", x))?;
    let (args, concl) = instantiate_pis(env, el, &ty);
    let snap = el.snapshot();
    if !defeq(env, el, &concl, &g.target) {
        el.restore(snap);
        return Err(format!(
            "apply：`{}` 的結論 `{}` 與目標 `{}` 不合",
            pretty(e),
            pretty(&zonk(el, &concl)),
            pretty(&g.target)
        ));
    }
    let ez = zonk(el, e);
    let mut subgoals = vec![];
    let mut pf_args = vec![];
    for (_, an, dom, h) in &args {
        let z = zonk(el, &Expr::Hole(*h));
        pf_args.push(z.clone());
        if !matches!(z, Expr::Hole(_)) {
            continue; // 已被 unification 解掉
        }
        if *an == Ann::Implicit {
            // 隱含參數不開新 goal，留給後續 tactic 的 unification（如 exact）去解；
            // 若到最後都沒解掉，run_tactics 會報 unsolved hole
            continue;
        }
        let nh = el.fresh();
        // 子 goal：證明 dom（把 hole 轉成新的 goal hole）
        // 注意：proof 項用原 hole，被解時自然帶入；子 goal 用新 hole + 等式連結
        el.assign.insert(*h, Expr::Hole(nh));
        subgoals.push(Goal {
            hole: nh,
            hyps: g.hyps.clone(),
            target: zonk(el, dom),
            tag: None,
        });
    }
    let pf = mk_apps(ez, pf_args);
    let pfz = zonk(el, &pf);
    el.assign.insert(g.hole, pfz);
    // 保持參數順序
    for sg in subgoals.into_iter().rev() {
        goals.insert(0, sg);
    }
    Ok(())
}

// ---------- constructor / left / right ----------

fn do_constructor(env: &Env, el: &mut Elab, goals: &mut Vec<Goal>) -> Result<(), String> {
    let g = take_goal(el, goals)?;
    match whnf(env, el, &g.target) {
        Expr::And(a, b) => {
            let (a, b) = (zonk(el, &a), zonk(el, &b));
            let h1 = el.fresh();
            let h2 = el.fresh();
            let pf = mk_apps(c("And.intro"), vec![a.clone(), b.clone(), Expr::Hole(h1), Expr::Hole(h2)]);
            el.assign.insert(g.hole, pf);
            goals.insert(
                0,
                Goal { hole: h1, hyps: g.hyps.clone(), target: a, tag: None },
            );
            goals.insert(
                1,
                Goal { hole: h2, hyps: g.hyps, target: b, tag: None },
            );
            Ok(())
        }
        Expr::Const(s) if s == "True" => {
            el.assign.insert(g.hole, c("True.intro"));
            Ok(())
        }
        Expr::Eq(_, _, _) => {
            check(env, el, &g.hyps, &Expr::Rfl, &g.target)?;
            el.assign.insert(g.hole, Expr::Rfl);
            Ok(())
        }
        Expr::Or(_, _) => Err("目標是 `\\/`，請用 left / right（constructor 無法決定邊）".to_string()),
        other => {
            // 試 False.elim
            for (n, t) in &g.hyps {
                if matches!(whnf(env, el, t), Expr::Const(s) if s == "False") {
                    let pf = mk_apps(c("False.elim"), vec![zonk(el, &g.target), Expr::Var(n.clone())]);
                    el.assign.insert(g.hole, pf);
                    return Ok(());
                }
            }
            Err(format!("constructor 不知道怎麼證 `{}`", pretty(&other)))
        }
    }
}

fn do_side(env: &Env, el: &mut Elab, goals: &mut Vec<Goal>, is_left: bool) -> Result<(), String> {
    let g = take_goal(el, goals)?;
    match whnf(env, el, &g.target) {
        Expr::Or(a, b) => {
            let (a, b) = (zonk(el, &a), zonk(el, &b));
            let nh = el.fresh();
            let head = if is_left { "Or.inl" } else { "Or.inr" };
            let pf = mk_apps(c(head), vec![a.clone(), b.clone(), Expr::Hole(nh)]);
            el.assign.insert(g.hole, pf);
            goals.insert(
                0,
                Goal {
                    hole: nh,
                    hyps: g.hyps,
                    target: if is_left { a } else { b },
                    tag: g.tag,
                },
            );
            Ok(())
        }
        other => Err(format!(
            "`{}` 需要目標是 `\\/`，而是 `{}`",
            if is_left { "left" } else { "right" },
            pretty(&other)
        )),
    }
}

// ---------- Eq rule 前處理 ----------

struct EqRule {
    a_ty: Expr,
    lhs: Expr,
    rhs: Expr,
    pf: Expr,
}

/// elaborate rw/simp 的 rule：推導型別 → 實例化 Pi 前綴 → 要求結論是等式。
/// 回傳 (ty, lhs, rhs, args)。內含 holes，由後續 match 決定指派。
fn elaborate_eq_rule(
    env: &Env,
    el: &mut Elab,
    ctx: &[(String, Expr)],
    rule: &Expr,
    what: &str,
) -> Result<(Expr, Expr, Expr, Vec<(String, Ann, Expr, usize)>), String> {
    let ty = infer(env, el, ctx, rule)
        .map_err(|x| format!("{} 的 rule 型別推導失敗：{}", what, x))?;
    let (args, concl) = instantiate_pis(env, el, &ty);
    match whnf(env, el, &zonk(el, &concl)) {
        Expr::Eq(t, l, r) => Ok(((*t).clone(), (*l).clone(), (*r).clone(), args)),
        other => Err(format!(
            "{} 的 rule 不是等式：`{}` 的型別是 `{}`",
            what,
            pretty(rule),
            pretty(&other)
        )),
    }
}

/// 在 match 成功後，把 rule 的 holes 定案，產生可用的 EqRule
fn finalize_rule(
    env: &Env,
    el: &mut Elab,
    ctx: &[(String, Expr)],
    rule: &Expr,
    args: &[(String, Ann, Expr, usize)],
    t: &Expr,
    l: &Expr,
    r: &Expr,
    outer_snap: &std::collections::HashMap<usize, Expr>,
) -> Result<EqRule, String> {
    let inst_t = zonk(el, t);
    let inst_l = zonk(el, l);
    let inst_r = zonk(el, r);
    // mop-up：把（已部分指派的）結論與 Eq(inst_t, inst_l, inst_r) 統一，解掉型別參數（如 A）
    let concl_guess = mk_eq(inst_t.clone(), inst_l.clone(), inst_r.clone());
    let ty = infer(env, el, ctx, rule).map_err(|e| format!("rule 重推導失敗：{}", e))?;
    let (args2, concl) = instantiate_pis(env, el, &ty);
    let _ = defeq(env, el, &zonk(el, &concl), &concl_guess);
    // 用第二次實例化的 args 重建 proof（holes 已被 mop-up 指派）
    let arg_exprs: Vec<Expr> = args2.iter().map(|(_, _, _, h)| zonk(el, &Expr::Hole(*h))).collect();
    let pf = zonk(el, &mk_apps(zonk(el, rule), arg_exprs));
    let _ = args;
    if has_hole(&pf) {
        el.restore(outer_snap.clone());
        return Err(format!(
            "無法推斷 rule `{}` 的全部參數（剩餘 holes），請把等式寫得更具體",
            pretty(rule)
        ));
    }
    Ok(EqRule { a_ty: inst_t, lhs: inst_l, rhs: inst_r, pf })
}

/// 一階結構比對（rw/simp 的 pattern matching 用）。
/// 刻意不用 whnf：避免 pattern（如 `?n + 0`）被歸約成裸 hole 而誤配萬物。
/// 成功時指派 holes；呼叫端負責 snapshot/restore。
fn match_pat(env: &Env, el: &mut Elab, pat: &Expr, s: &Expr) -> bool {
    let (p, t) = (zonk(el, pat), zonk(el, s));
    match (p, t) {
        (Expr::Hole(h), v) => {
            if v == Expr::Hole(h) {
                return true;
            }
            if hole_occurs(h, &v, &el.assign) {
                return false;
            }
            el.assign.insert(h, v);
            true
        }
        (Expr::App(f1, a1), Expr::App(f2, a2)) => {
            match_pat(env, el, &f1, &f2) && match_pat(env, el, &a1, &a2)
        }
        (Expr::Lam(x1, _, d1, b1), Expr::Lam(x2, _, d2, b2))
        | (Expr::Pi(x1, _, d1, b1), Expr::Pi(x2, _, d2, b2)) => {
            if !match_pat(env, el, &d1, &d2) {
                return false;
            }
            let f = fresh_name("m");
            let fv = Expr::Var(f);
            match_pat(env, el, &subst(&x1, &fv, &b1), &subst(&x2, &fv, &b2))
        }
        (Expr::Eq(t1, a1, b1), Expr::Eq(t2, a2, b2)) => {
            match_pat(env, el, &t1, &t2)
                && match_pat(env, el, &a1, &a2)
                && match_pat(env, el, &b1, &b2)
        }
        (Expr::And(a1, b1), Expr::And(a2, b2)) | (Expr::Or(a1, b1), Expr::Or(a2, b2)) => {
            match_pat(env, el, &a1, &a2) && match_pat(env, el, &b1, &b2)
        }
        (Expr::Ite(c1, t1, f1), Expr::Ite(c2, t2, f2)) => {
            match_pat(env, el, &c1, &c2)
                && match_pat(env, el, &t1, &t2)
                && match_pat(env, el, &f1, &f2)
        }
        (p, t) => p == t,
    }
}

fn match_first(env: &Env, el: &mut Elab, pattern: &Expr, target: &Expr) -> bool {
    for s in subterms(target) {
        let snap = el.snapshot();
        if match_pat(env, el, pattern, &s) {
            return true;
        }
        el.restore(snap);
    }
    false
}

/// 用 EqRule 改寫 goal.target（含 proof 包裝）。回傳新的 target。
fn apply_rewrite(
    env: &Env,
    el: &mut Elab,
    ctx: &[(String, Expr)],
    goal_hole: usize,
    target: &Expr,
    rule: &EqRule,
    rev: bool,
) -> Result<(Expr, usize), String> {
    // 目標必須是 Prop（Eq.subst 的 motive 落在 Prop）
    let ts = infer(env, el, ctx, target).map_err(|e| format!("目標型別推導失敗：{}", e))?;
    match whnf(env, el, &ts) {
        Expr::Sort(_) => {}
        other => {
            return Err(format!(
                "rw/simp 只能改寫命題/型別目標（Eq.subst 的 motive 落在 Prop），目標型別是 `{}`",
                pretty(&other)
            ))
        }
    }
    // !rev: P = fun x => target[lhs->x]; new = P rhs;
    //       step = subst A P rhs lhs (symm pf) new : P lhs
    // rev : P = fun x => target[rhs->x]; new = P lhs;
    //       step = subst A P lhs rhs pf new : P rhs
    let (abs_src, new_side) = if rev {
        (rule.rhs.clone(), rule.lhs.clone())
    } else {
        (rule.lhs.clone(), rule.rhs.clone())
    };
    // abs_src：要被抽象掉的 pattern；from/to：Eq.subst 的方向
    // !rev: P = fun x => target[lhs->x]; new = P rhs; step = subst A P rhs lhs (symm pf) new.
    // rev : P = fun x => target[rhs->x]; new = P lhs; step = subst A P lhs rhs pf new.
    let new_target = replace_syntactic(target, &abs_src, &new_side);
    if &new_target == target {
        return Err("改寫前後目標完全相同（pattern 是定義相等但沒有語法實例？請先用 simp 正規化）".to_string());
    }
    let xf = fresh_name("rw");
    let abs_body = replace_syntactic(target, &abs_src, &Expr::Var(xf.clone()));
    let motive = Expr::Lam(xf, Ann::Explicit, Box::new(rule.a_ty.clone()), Box::new(abs_body));
    let nh = el.fresh();
    let step = if rev {
        mk_apps(
            c("Eq.subst"),
            vec![
                rule.a_ty.clone(),
                motive,
                rule.lhs.clone(),
                rule.rhs.clone(),
                rule.pf.clone(),
                Expr::Hole(nh),
            ],
        )
    } else {
        let symm = mk_apps(
            c("Eq.symm"),
            vec![
                rule.a_ty.clone(),
                rule.lhs.clone(),
                rule.rhs.clone(),
                rule.pf.clone(),
            ],
        );
        mk_apps(
            c("Eq.subst"),
            vec![
                rule.a_ty.clone(),
                motive,
                rule.rhs.clone(),
                rule.lhs.clone(),
                symm,
                Expr::Hole(nh),
            ],
        )
    };
    el.assign.insert(goal_hole, step);
    Ok((zonk(el, &new_target), nh))
}

fn do_rewrite(env: &Env, el: &mut Elab, goals: &mut Vec<Goal>, rev: bool, rule: &Expr) -> Result<(), String> {
    let g = take_goal(el, goals)?;
    let rule_r = res(env, &g.hyps, rule)?;
    let rule = &rule_r;
    let (ety, lhs, rhs, args) = elaborate_eq_rule(env, el, &g.hyps, rule, "rw")?;
    let outer = el.snapshot();
    // pattern（含 holes）找第一個 definitional 匹配
    let pat0 = zonk(el, if rev { &rhs } else { &lhs });
    if !match_first(env, el, &pat0, &g.target) {
        el.restore(outer);
        return Err(format!(
            "pattern `{}` 在目標 `{}` 中找不到{}",
            pretty(&pat0),
            pretty(&g.target),
            if rev { "（reverse 方向）" } else { "" }
        ));
    }
    let erule2 = finalize_rule(env, el, &g.hyps, rule, &args, &ety, &lhs, &rhs, &outer)?;
    let (new_target, nh) = apply_rewrite(env, el, &g.hyps, g.hole, &g.target, &erule2, rev)
        .map_err(|e| {
            el.restore(outer);
            e
        })?;
    goals.insert(0, Goal { hole: nh, hyps: g.hyps, target: new_target, tag: g.tag });
    Ok(())
}

// ---------- simp ----------

fn do_simp(env: &Env, el: &mut Elab, goals: &mut Vec<Goal>, lemmas: &[Expr]) -> Result<(), String> {
    let g = take_goal(el, goals)?;
    let mut cur_target = normalize(env, el, &g.target);
    let mut cur_hole = g.hole;
    let ctx = g.hyps.clone();
    for lem in lemmas {
        let lem_r = res(env, &ctx, lem)?;
        let lem = &lem_r;
        let (ety, lhs, rhs, args) = elaborate_eq_rule(env, el, &ctx, lem, "simp")?;
        for _ in 0..30 {
            let outer = el.snapshot();
            let pat0 = zonk(el, &lhs);
            if !match_first(env, el, &pat0, &cur_target) {
                el.restore(outer);
                break;
            }
            let erule2 = match finalize_rule(env, el, &ctx, lem, &args, &ety, &lhs, &rhs, &outer) {
                Ok(r) => r,
                Err(_) => {
                    el.restore(outer);
                    break;
                }
            };
            match apply_rewrite(env, el, &ctx, cur_hole, &cur_target, &erule2, false) {
                Ok((nt, nh)) => {
                    cur_target = normalize(env, el, &nt);
                    cur_hole = nh;
                }
                Err(_) => {
                    el.restore(outer);
                    break;
                }
            }
        }
    }
    cur_target = zonk(el, &cur_target);
    // 嘗試關閉
    match whnf(env, el, &cur_target) {
        Expr::Eq(_, a, b) => {
            let snap = el.snapshot();
            if defeq(env, el, &a, &b) {
                el.assign.insert(cur_hole, Expr::Rfl);
                return Ok(());
            }
            el.restore(snap);
        }
        Expr::Const(s) if s == "True" => {
            el.assign.insert(cur_hole, c("True.intro"));
            return Ok(());
        }
        _ => {}
    }
    for (n, t) in &ctx {
        let snap = el.snapshot();
        if defeq(env, el, t, &cur_target) {
            el.assign.insert(cur_hole, Expr::Var(n.clone()));
            return Ok(());
        }
        el.restore(snap);
    }
    goals.insert(0, Goal { hole: cur_hole, hyps: g.hyps, target: cur_target, tag: g.tag });
    Ok(())
}

// ---------- cases / induction 共用 ----------

/// 把 hyps 中 n 之後的 dependents 抽出（revert），回傳 (base, reverted)
fn generalize_var(
    hyps: &[(String, Expr)],
    n: &str,
) -> Result<(Vec<(String, Expr)>, Vec<(String, Expr)>), String> {
    let pos = hyps
        .iter()
        .position(|(x, _)| x == n)
        .ok_or_else(|| format!("`{}` 不是 hypothesis/variable", n))?;
    let mut rev_set: HashSet<String> = HashSet::new();
    rev_set.insert(n.to_string());
    let mut reverted_rev: Vec<(String, Expr)> = vec![];
    let mut kept_rev: Vec<(String, Expr)> = vec![];
    for (x, t) in hyps[pos + 1..].iter().rev() {
        let fvs = free_vars(t);
        if fvs.intersection(&rev_set).next().is_some() {
            reverted_rev.push((x.clone(), t.clone()));
            rev_set.insert(x.clone());
        } else {
            kept_rev.push((x.clone(), t.clone()));
        }
    }
    reverted_rev.reverse();
    kept_rev.reverse();
    let mut base: Vec<(String, Expr)> = hyps[..pos].to_vec();
    base.extend(kept_rev);
    Ok((base, reverted_rev))
}

/// motive = fun nf : Base => Pi reverted. target[nf/var]
fn build_motive(
    nf: &str,
    base_ty: Expr,
    reverted: &[(String, Expr)],
    target: &Expr,
    var: &str,
) -> (Expr, Expr) {
    let mut body = subst(var, &Expr::Var(nf.to_string()), target);
    for (x, t) in reverted.iter().rev() {
        let t2 = subst(var, &Expr::Var(nf.to_string()), t);
        body = Expr::Pi(x.clone(), Ann::Explicit, Box::new(t2), Box::new(body));
    }
    let motive = Expr::Lam(nf.to_string(), Ann::Explicit, Box::new(base_ty), Box::new(body.clone()));
    (motive, body)
}

fn do_cases(
    env: &Env,
    el: &mut Elab,
    goals: &mut Vec<Goal>,
    h: &str,
    ns: &[String],
) -> Result<(), String> {
    let g = take_goal(el, goals)?;
    let ht = find_hyp(&g.hyps, h)?;
    match whnf(env, el, &ht) {
        Expr::And(a, b) => {
            let (a, b) = (zonk(el, &a), zonk(el, &b));
            let (n1, n2) = match ns.len() {
                0 => ("left".to_string(), "right".to_string()),
                2 => (ns[0].clone(), ns[1].clone()),
                _ => return Err("And 的 cases 需要 `with l r`（兩個名字）".to_string()),
            };
            ensure_fresh(&g.hyps, &n1)?;
            ensure_fresh(&g.hyps, &n2)?;
            let l_app = mk_apps(c("And.left"), vec![a.clone(), b.clone(), Expr::Var(h.to_string())]);
            let r_app = mk_apps(c("And.right"), vec![a.clone(), b.clone(), Expr::Var(h.to_string())]);
            let nh = el.fresh();
            el.assign.insert(
                g.hole,
                Expr::Let(
                    n1.clone(),
                    Box::new(a.clone()),
                    Box::new(l_app),
                    Box::new(Expr::Let(
                        n2.clone(),
                        Box::new(b.clone()),
                        Box::new(r_app),
                        Box::new(Expr::Hole(nh)),
                    )),
                ),
            );
            let mut hyps = g.hyps;
            hyps.push((n1, a));
            hyps.push((n2, b));
            goals.insert(0, Goal { hole: nh, hyps, target: g.target, tag: g.tag });
            Ok(())
        }
        Expr::Or(a, b) => {
            let (a, b) = (zonk(el, &a), zonk(el, &b));
            let (n1, n2) = match ns.len() {
                0 => ("l".to_string(), "r".to_string()),
                2 => (ns[0].clone(), ns[1].clone()),
                _ => return Err("Or 的 cases 需要 `with l r`（每分支一個名字）".to_string()),
            };
            ensure_fresh(&g.hyps, &n1)?;
            ensure_fresh(&g.hyps, &n2)?;
            let h1 = el.fresh();
            let h2 = el.fresh();
            let pf = mk_apps(
                c("Or.elim"),
                vec![
                    a.clone(),
                    b.clone(),
                    zonk(el, &g.target),
                    Expr::Lam(n1.clone(), Ann::Explicit, Box::new(a.clone()), Box::new(Expr::Hole(h1))),
                    Expr::Lam(n2.clone(), Ann::Explicit, Box::new(b.clone()), Box::new(Expr::Hole(h2))),
                    Expr::Var(h.to_string()),
                ],
            );
            el.assign.insert(g.hole, pf);
            let mut hyps1 = g.hyps.clone();
            hyps1.push((n1, a));
            let mut hyps2 = g.hyps;
            hyps2.push((n2, b));
            goals.insert(
                0,
                Goal { hole: h1, hyps: hyps1, target: g.target.clone(), tag: Some("inl".to_string()) },
            );
            goals.insert(
                1,
                Goal { hole: h2, hyps: hyps2, target: g.target, tag: Some("inr".to_string()) },
            );
            Ok(())
        }
        Expr::Const(s) if s == "Nat" => {
            // cases (Nat var)：zero / succ，請用 generalize（與 induction 同）
            let k = match ns.len() {
                0 => "k".to_string(),
                1 => ns[0].clone(),
                _ => return Err("Nat 的 cases 只要 `with k`（succ 分支的新變數名）".to_string()),
            };
            ensure_fresh(&g.hyps, &k)?;
            let (base, reverted) = generalize_var(&g.hyps, h)?;
            let nf = fresh_name("m");
            let (motive, body) = build_motive(&nf, c("Nat"), &reverted, &g.target, h);
            let zero_t = zonk(el, &subst(&nf, &c("Nat.zero"), &body));
            let kvar = Expr::Var(k.clone());
            let succ_expr = app(c("Nat.succ"), kvar.clone());
            let succ_t = zonk(el, &subst(&nf, &succ_expr, &body));
            let hz = el.fresh();
            let hs = el.fresh();
            let ih_ty = zonk(el, &subst(&nf, &kvar, &body));
            let step = Expr::Lam(
                k.clone(),
                Ann::Explicit,
                Box::new(c("Nat")),
                Box::new(Expr::Lam(
                    fresh_name("ih"),
                    Ann::Explicit,
                    Box::new(ih_ty),
                    Box::new(Expr::Hole(hs)),
                )),
            );
            let pf = mk_apps(c("Nat.rec"), vec![motive, Expr::Hole(hz), step, Expr::Var(h.to_string())]);
            el.assign.insert(g.hole, pf);
            let mut hyps_s = base.clone();
            hyps_s.push((k, c("Nat")));
            goals.insert(0, Goal { hole: hz, hyps: base, target: zero_t, tag: Some("zero".to_string()) });
            goals.insert(1, Goal { hole: hs, hyps: hyps_s, target: succ_t, tag: Some("succ".to_string()) });
            Ok(())
        }
        Expr::Const(s) if s == "Bool" => {
            if !ns.is_empty() {
                return Err("Bool 的 cases 不需要 with 名字".to_string());
            }
            let (base, reverted) = generalize_var(&g.hyps, h)?;
            let nf = fresh_name("m");
            let (motive, body) = build_motive(&nf, c("Bool"), &reverted, &g.target, h);
            let t_t = zonk(el, &subst(&nf, &c("true"), &body));
            let f_t = zonk(el, &subst(&nf, &c("false"), &body));
            let h1 = el.fresh();
            let h2 = el.fresh();
            let pf = mk_apps(
                c("Bool.rec"),
                vec![motive, Expr::Hole(h1), Expr::Hole(h2), Expr::Var(h.to_string())],
            );
            el.assign.insert(g.hole, pf);
            goals.insert(0, Goal { hole: h1, hyps: base.clone(), target: t_t, tag: Some("true".to_string()) });
            goals.insert(1, Goal { hole: h2, hyps: base, target: f_t, tag: Some("false".to_string()) });
            Ok(())
        }
        other => Err(format!(
            "cases 只支援 And / Or / Nat / Bool，`{}` 的型別是 `{}`",
            h,
            pretty(&other)
        )),
    }
}

fn do_induction(
    env: &Env,
    el: &mut Elab,
    goals: &mut Vec<Goal>,
    n: &str,
    ns: &[String],
) -> Result<(), String> {
    let g = take_goal(el, goals)?;
    let nt = find_hyp(&g.hyps, n)?;
    match whnf(env, el, &nt) {
        Expr::Const(s) if s == "Nat" => {},
        other => return Err(format!("induction 的對象必須是 Nat，`{}` 是 `{}`", n, pretty(&other))),
    }
    let (k, ih) = match ns.len() {
        0 => ("k".to_string(), "ih".to_string()),
        2 => (ns[0].clone(), ns[1].clone()),
        _ => return Err("induction 的 with 需要 0 或 2 個名字：`with k ih`".to_string()),
    };
    ensure_fresh(&g.hyps, &k)?;
    ensure_fresh(&g.hyps, &ih)?;
    let (base, reverted) = generalize_var(&g.hyps, n)?;
    // k/ih 若與 reverted 撞名也算 fresh（base 已扣掉 reverted；reverted 的名字仍在 scope 外？不—reverted 已被包進 motive，不在 ctx）
    for (x, _) in &reverted {
        if x == &k || x == &ih {
            return Err(format!("induction 名字 `{}` 與 revert 的 hypothesis 撞名", x));
        }
    }
    let nf = fresh_name("m");
    let (motive, body) = build_motive(&nf, c("Nat"), &reverted, &g.target, n);
    let zero_t = zonk(el, &subst(&nf, &c("Nat.zero"), &body));
    let kvar = Expr::Var(k.clone());
    let succ_expr = app(c("Nat.succ"), kvar.clone());
    let succ_t = zonk(el, &subst(&nf, &succ_expr, &body));
    let ih_ty = zonk(el, &subst(&nf, &kvar, &body));
    let hz = el.fresh();
    let hs = el.fresh();
    let step = Expr::Lam(
        k.clone(),
        Ann::Explicit,
        Box::new(c("Nat")),
        Box::new(Expr::Lam(ih.clone(), Ann::Explicit, Box::new(ih_ty.clone()), Box::new(Expr::Hole(hs)))),
    );
    let pf = mk_apps(c("Nat.rec"), vec![motive, Expr::Hole(hz), step, Expr::Var(n.to_string())]);
    el.assign.insert(g.hole, pf);
    let mut hyps_s = base.clone();
    hyps_s.push((k, c("Nat")));
    hyps_s.push((ih, ih_ty));
    goals.insert(0, Goal { hole: hz, hyps: base, target: zero_t, tag: Some("zero".to_string()) });
    goals.insert(1, Goal { hole: hs, hyps: hyps_s, target: succ_t, tag: Some("succ".to_string()) });
    Ok(())
}
