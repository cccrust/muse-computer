//! Driver：把 parse 完的 commands 逐條交給 kernel/tactic 驗證，收集輸出。

use crate::ast::*;
use crate::kernel::{check, ensure_sort_pub, infer, normalize, resolve, std_env, zonk, Elab, Env};
use crate::parser;

/// 一個驗證 session（跨 commands 共享 Env：後面的定理可以用前面的引理）。
pub struct Session {
    pub env: Env,
    pub next_hole: usize,
    pub checked: usize,
}

impl Session {
    pub fn new() -> Self {
        Session { env: std_env(), next_hole: 0, checked: 0 }
    }

    pub fn run(&mut self, src: &str) -> Result<Vec<String>, String> {
        let (cmds, max_hole) = parser::parse(src)?;
        if max_hole > self.next_hole {
            self.next_hole = max_hole;
        }
        let mut out = vec![];
        for (cmd, line) in &cmds {
            match self.elab_command(cmd) {
                Ok(mut lines) => out.append(&mut lines),
                Err(e) => return Err(format!("line {}：{}", line, e)),
            }
        }
        Ok(out)
    }

    fn sync_elab(&mut self) -> Elab {
        Elab::new(self.next_hole)
    }
    fn reclaim(&mut self, el: &Elab, out: &mut Vec<String>) {
        if el.next_hole > self.next_hole {
            self.next_hole = el.next_hole;
        }
        for w in &el.warnings {
            out.push(w.clone());
        }
    }

    fn elab_command(&mut self, cmd: &Command) -> Result<Vec<String>, String> {
        let mut out = vec![];
        match cmd {
            Command::Def { name, params, ty, body } => {
                if self.env.defs.contains_key(name) {
                    return Err(format!("`{}` 已經定義過了", name));
                }
                let full_body = mk_lams(params, body.clone());
                let mut el = self.sync_elab();
                let full_ty = match ty {
                    Some(t) => mk_pis(params, t.clone()),
                    None => {
                        let rb = resolve(&self.env, &mut vec![], &full_body)
                            .map_err(|e| format!("def `{}` 名稱解析失敗：{}", name, e))?;
                        infer(&self.env, &mut el, &vec![], &rb)
                            .map_err(|e| format!("def `{}` 型別推導失敗：{}", name, e))?
                    }
                };
                let full_body_r = resolve(&self.env, &mut vec![], &full_body)
                    .map_err(|e| format!("def `{}` 名稱解析失敗：{}", name, e))?;
                let full_ty_r = resolve(&self.env, &mut vec![], &full_ty)
                    .map_err(|e| format!("def `{}` 型別名稱解析失敗：{}", name, e))?;
                let full_body = full_body_r;
                let full_ty = full_ty_r;
                // 型別本身必須是 Sort
                let ks = infer(&self.env, &mut el, &vec![], &full_ty)
                    .map_err(|e| format!("def `{}` 的型別不是型別：{}", name, e))?;
                ensure_sort_pub(&self.env, &mut el, &ks)
                    .map_err(|e| format!("def `{}` 的型別不是型別：{}", name, e))?;
                check(&self.env, &mut el, &vec![], &full_body, &full_ty)
                    .map_err(|e| format!("def `{}` 主體不合型別：{}", name, e))?;
                let (bz, tz) = (zonk(&el, &full_body), zonk(&el, &full_ty));
                if has_hole(&bz) || has_hole(&tz) {
                    return Err(format!("def `{}` 有未解的 `_`，請寫得更具體", name));
                }
                self.env.defs.insert(
                    name.clone(),
                    crate::kernel::Def { ty: tz.clone(), val: Some(bz), reducible: true },
                );
                self.checked += 1;
                out.push(format!("ok def {} : {}", name, pretty(&tz)));
                self.reclaim(&el, &mut out);
                Ok(out)
            }
            Command::Theorem { name, params, ty, body } => {
                let label = name.clone().unwrap_or_else(|| "example".to_string());
                if let Some(n) = name {
                    if self.env.defs.contains_key(n) {
                        return Err(format!("`{}` 已經定義過了", n));
                    }
                }
                let full_ty = mk_pis(params, ty.clone());
                let full_body = mk_lams(params, body.clone());
                let mut el = self.sync_elab();
                let full_ty = resolve(&self.env, &mut vec![], &full_ty)
                    .map_err(|e| format!("theorem `{}` 陳述名稱解析失敗：{}", label, e))?;
                let full_body = resolve(&self.env, &mut vec![], &full_body)
                    .map_err(|e| format!("theorem `{}` 名稱解析失敗：{}", label, e))?;
                let ks = infer(&self.env, &mut el, &vec![], &full_ty)
                    .map_err(|e| format!("theorem `{}` 的陳述不是型別：{}", label, e))?;
                ensure_sort_pub(&self.env, &mut el, &ks)
                    .map_err(|e| format!("theorem `{}` 的陳述不是型別：{}", label, e))?;
                check(&self.env, &mut el, &vec![], &full_body, &full_ty)
                    .map_err(|e| format!("theorem `{}` 證明失敗：{}", label, e))?;
                let (bz, tz) = (zonk(&el, &full_body), zonk(&el, &full_ty));
                if has_hole(&bz) {
                    return Err(format!("theorem `{}` 有未解 metavariable", label));
                }
                if let Some(n) = name {
                    self.env.defs.insert(
                        n.clone(),
                        crate::kernel::Def { ty: tz.clone(), val: Some(bz), reducible: false },
                    );
                }
                self.checked += 1;
                out.push(format!("ok theorem {} : {}", label, pretty(&tz)));
                self.reclaim(&el, &mut out);
                Ok(out)
            }
            Command::Axiom { name, ty } => {
                if self.env.defs.contains_key(name) {
                    return Err(format!("`{}` 已經定義過了", name));
                }
                let mut el = self.sync_elab();
                let ty_r = resolve(&self.env, &mut vec![], ty)
                    .map_err(|e| format!("axiom `{}` 名稱解析失敗：{}", name, e))?;
                let ty = &ty_r;
                let ks = infer(&self.env, &mut el, &vec![], ty)
                    .map_err(|e| format!("axiom `{}` 的型別不是型別：{}", name, e))?;
                ensure_sort_pub(&self.env, &mut el, &ks)
                    .map_err(|e| format!("axiom `{}` 的型別不是型別：{}", name, e))?;
                let tz = zonk(&el, ty);
                self.env.defs.insert(
                    name.clone(),
                    crate::kernel::Def { ty: tz.clone(), val: None, reducible: false },
                );
                out.push(format!("ok axiom {} : {}", name, pretty(&tz)));
                self.reclaim(&el, &mut out);
                Ok(out)
            }
            Command::Check(e) => {
                let mut el = self.sync_elab();
                let e_r = resolve(&self.env, &mut vec![], e).map_err(|x| format!("#check 失敗：{}", x))?;
                let e = &e_r;
                let t = infer(&self.env, &mut el, &vec![], e)
                    .map_err(|x| format!("#check 失敗：{}", x))?;
                out.push(format!("#check {} : {}", pretty(e), pretty(&zonk(&el, &t))));
                self.reclaim(&el, &mut out);
                Ok(out)
            }
            Command::Eval(e) => {
                let mut el = self.sync_elab();
                let e_r = resolve(&self.env, &mut vec![], e).map_err(|x| format!("#eval 失敗：{}", x))?;
                let e = &e_r;
                infer(&self.env, &mut el, &vec![], e).map_err(|x| format!("#eval 失敗：{}", x))?;
                let n = normalize(&self.env, &el, e);
                out.push(format!("#eval {} => {}", pretty(e), pretty(&n)));
                self.reclaim(&el, &mut out);
                Ok(out)
            }
        }
    }
}
