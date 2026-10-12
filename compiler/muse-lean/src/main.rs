//! muse-lean CLI：不帶參數跑內建 5 個證明；帶檔名則逐檔驗證。

use muse_lean::driver::Session;

const EX1: &str = include_str!("../examples/ex1_and_comm.lean");
const EX2: &str = include_str!("../examples/ex2_or_comm.lean");
const EX3: &str = include_str!("../examples/ex3_eq_trans.lean");
const EX4: &str = include_str!("../examples/ex4_zero_add.lean");
const EX5: &str = include_str!("../examples/ex5_add_comm.lean");

const HELP: &str = "muse-lean：微型 Lean4 系統（Rust 實作）

用法：
  muse-lean                 跑內建 5 個證明 examples
  muse-lean file.lean ...   驗證指定檔案（每檔獨立 session）
  muse-lean --help          顯示此說明

簡化後的 Lean 語法：
  命令：
    def f : T := body            def f (x : A) : B := body
    theorem name : P := by ...   theorem name : P := term
    example : P := ...           axiom name : T
    #check term                  #eval term
  項：
    fun x : T => body            forall x : T, body    A -> B
    let x : T := v; body         if c then a else b
    a /\\ b   a \\/ b   a = b   a != b   ~a   a + b   a * b
    數字字面量（0, 1, 2, ...）、_（hole）、rfl、sorry、by ...
  tactics（by 後一行一個，或同行用 ; 分隔）：
    intro x y   exact e   apply e   rfl   assumption   trivial
    simp [lemmas]   rw [h1, <- h2]   cases h with a b   left / right
    constructor   have h : T := proof   induction n with k ih
    show T   case zero/succ/inl/inr   all_goals tac   sorry
  與 Lean4 的差異（刻意簡化）：
    - Type : Type（Type-in-Type），無 universe levels
    - cases 不清除原 hypothesis；induction 後用線性順序或 case 選分支
    - term-mode 的 implicit 參數要手寫 _（tactic apply 會自動解）
    - 不支援 match / calc / instance / structure / import
";

fn run_one(title: &str, src: &str) -> bool {
    println!("=== {} ===", title);
    let mut s = Session::new();
    match s.run(src) {
        Ok(lines) => {
            for l in lines {
                println!("  {}", l);
            }
            println!("  [{} checked OK]\n", s.checked);
            true
        }
        Err(e) => {
            println!("  ERROR: {}\n", e);
            false
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", HELP);
        return;
    }
    let mut ok = true;
    if args.is_empty() {
        ok &= run_one("ex1_and_comm.lean（And 交換律）", EX1);
        ok &= run_one("ex2_or_comm.lean（Or 交換律）", EX2);
        ok &= run_one("ex3_eq_trans.lean（等式 trans/symm）", EX3);
        ok &= run_one("ex4_zero_add.lean（歸納法 0+n=n）", EX4);
        ok &= run_one("ex5_add_comm.lean（加法交換律）", EX5);
    } else {
        for f in &args {
            match std::fs::read_to_string(f) {
                Ok(src) => {
                    ok &= run_one(f, &src);
                }
                Err(e) => {
                    println!("=== {} ===\n  無法讀檔：{}\n", f, e);
                    ok = false;
                }
            }
        }
    }
    if !ok {
        std::process::exit(1);
    }
}
