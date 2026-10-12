-- ex1：命題邏輯 — And 交換律
-- 展示：def / term-mode 證明（rfl）/ intro / cases / apply / exact / #check

def myid : Nat -> Nat := fun x : Nat => x

example : myid 2 = 2 := rfl

theorem and_comm : forall a b : Prop, a /\ b -> b /\ a := by
  intro a
  intro b
  intro h
  cases h with left right
  apply And.intro
  exact right
  exact left

#check and_comm
