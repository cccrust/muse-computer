-- ex4：Nat 歸納法 — 0 + n = n
-- 展示：induction（zero/succ 兩 goals）/ simp 正規化 / rw 改寫 / rfl 收尾

def double : Nat -> Nat := fun n : Nat => n + n

theorem zero_add : forall n : Nat, 0 + n = n := by
  intro n
  induction n with k ih
  rfl
  simp
  rw [ih]
  rfl

#check zero_add
#eval double 2
#eval 2 + 3
