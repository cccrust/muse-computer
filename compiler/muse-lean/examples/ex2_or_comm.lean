-- ex2：命題邏輯 — Or 交換律
-- 展示：cases 分支出兩個 goals / case 選擇要證的分支 / left-right 翻面
-- 注意：cases 之後 goals 順序是 [inl, inr]，這裡故意先證 inr 再證 inl

theorem or_comm : forall a b : Prop, a \/ b -> b \/ a := by
  intro a
  intro b
  intro h
  cases h with hl hr
  case inr
  apply Or.inl
  exact hr
  case inl
  apply Or.inr
  exact hl

#check or_comm
