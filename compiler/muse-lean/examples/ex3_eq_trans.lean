-- ex3：等式推理 — trans / symm
-- 展示：implicit binder {A : Type} / apply 多元引理（unification 自動解隱含參數）

theorem my_trans : forall {A : Type} (a b c : A), a = b -> b = c -> a = c := by
  intro A
  intro a
  intro b
  intro c
  intro h1
  intro h2
  apply Eq.trans
  exact h1
  exact h2

theorem my_symm : forall {A : Type} (a b : A), a = b -> b = a := by
  intro A
  intro a
  intro b
  intro h
  apply Eq.symm
  exact h

-- term-mode：rfl 證定義等式（beta 歸約後兩邊相同）
example : (fun x : Nat => x) 2 = 2 := rfl

#check my_trans
#check my_symm
