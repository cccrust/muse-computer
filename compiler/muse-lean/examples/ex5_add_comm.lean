-- ex5：加法交換律（capstone：引理接力 + induction + rw）
-- 展示：引理如何被後續證明當 rw 規則重用

theorem zero_add : forall n : Nat, 0 + n = n := by
  intro n
  induction n with k ih
  rfl
  simp
  rw [ih]
  rfl

theorem add_zero : forall n : Nat, n + 0 = n := by
  intro n
  rfl

theorem succ_add : forall n m : Nat, Nat.succ n + m = Nat.succ (n + m) := by
  intro n
  intro m
  induction m with k ih
  rfl
  simp
  rw [ih]
  rfl

theorem add_succ : forall n m : Nat, n + Nat.succ m = Nat.succ (n + m) := by
  intro n
  intro m
  rfl

theorem add_comm : forall n m : Nat, n + m = m + n := by
  intro n
  intro m
  induction n with k ih
  rw [zero_add]
  rw [add_zero]
  rfl
  rw [succ_add]
  rw [ih]
  rw [add_succ]
  rfl

#check add_comm
#eval 2 + 3
