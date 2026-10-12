//! 整合測試：5 個 examples 必須全過；錯誤證明必須被拒絕。

use muse_lean::driver::Session;

fn ok(src: &str) -> Vec<String> {
    let mut s = Session::new();
    s.run(src).unwrap_or_else(|e| panic!("應該通過卻失敗：{}\n----\n{}", e, src))
}

fn err(src: &str) -> String {
    let mut s = Session::new();
    match s.run(src) {
        Ok(lines) => panic!("應該失敗卻通過：{:?}\n----\n{}", lines, src),
        Err(e) => e,
    }
}

#[test]
fn ex1_and_comm() {
    let out = ok(include_str!("../examples/ex1_and_comm.lean"));
    assert!(out.iter().any(|l| l.contains("ok theorem and_comm")));
}

#[test]
fn ex2_or_comm() {
    let out = ok(include_str!("../examples/ex2_or_comm.lean"));
    assert!(out.iter().any(|l| l.contains("ok theorem or_comm")));
}

#[test]
fn ex3_eq_trans() {
    let out = ok(include_str!("../examples/ex3_eq_trans.lean"));
    assert!(out.iter().any(|l| l.contains("ok theorem my_trans")));
    assert!(out.iter().any(|l| l.contains("ok theorem my_symm")));
}

#[test]
fn ex4_zero_add() {
    let out = ok(include_str!("../examples/ex4_zero_add.lean"));
    assert!(out.iter().any(|l| l.contains("ok theorem zero_add")));
    assert!(out.iter().any(|l| l.contains("#eval double 2 => 4")));
    assert!(out.iter().any(|l| l.contains("#eval 2 + 3 => 5")));
}

#[test]
fn ex5_add_comm() {
    let out = ok(include_str!("../examples/ex5_add_comm.lean"));
    for name in ["zero_add", "add_zero", "succ_add", "add_succ", "add_comm"] {
        assert!(out.iter().any(|l| l.contains(name)), "missing {}", name);
    }
}

#[test]
fn rejects_false_eq() {
    let e = err("example : 0 = 1 := rfl");
    assert!(e.contains("rfl"), "unexpected: {}", e);
}

#[test]
fn rejects_wrong_proof_term() {
    let e = err("example : True /\\ False := by\n  constructor\n  exact True.intro\n  exact True.intro\n");
    assert!(e.contains("type mismatch") || e.contains("不合"), "unexpected: {}", e);
}

#[test]
fn rejects_unsolved_goals() {
    let e = err("theorem t : True /\\ True := by\n  constructor\n  exact True.intro\n");
    assert!(e.contains("未解"), "unexpected: {}", e);
}

#[test]
fn rejects_rw_non_eq() {
    let e = err("theorem t : True := by\n  rw [True.intro]\n");
    assert!(e.contains("不是等式"), "unexpected: {}", e);
}

#[test]
fn rejects_unknown_ident() {
    let e = err("#check nosuchthing");
    assert!(e.contains("unknown"), "unexpected: {}", e);
}

#[test]
fn rejects_unknown_tactic() {
    let e = err("theorem t : True := by\n  frobnicate\n");
    assert!(e.contains("unknown tactic"), "unexpected: {}", e);
}

#[test]
fn sorry_passes_with_warning() {
    let out = ok("theorem t : False := by\n  sorry\n");
    assert!(out.iter().any(|l| l.contains("sorry")), "missing warning: {:?}", out);
}

#[test]
fn def_compute() {
    let out = ok("def যা : Nat := 21\n#eval যা\n");
    assert!(out.iter().any(|l| l.contains("=> 21")), "{:?}", out);
}
