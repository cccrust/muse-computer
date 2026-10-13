# trait_bound_fail.aero 解說

trait bound 反例（編譯失敗是預期的）：以不滿足 `Add` 命題的型別具現泛型，示範 `E0301`。

## 內容

- `sum[T: Add + Clone]` 要求元素可加。
- `struct NoAdd { v: Bool } end`：欄位全是 `Bool`，編譯器的結構化證明推不出 `Add`。
- `sum(xs)`（`xs: Array[NoAdd]`）單態化時 rustc 報 E0277，aeroc 轉成 `E0301`。

## 預期行為

```sh
aeroc check examples/trait_bound_fail.aero --json   # 失敗，exit != 0
# JSON 含 "error_code": "E0301"，位置指回 sum(xs) 那一行
```

對照組是 `traits.aero`（`Int` 滿足 `Add+Clone`，可通過）。

## 對應 Rust（`trait_bound_fail.rust`）

`emit` 仍會產出 `fn sum<T: Add + Clone>` 與 `struct NoAdd`（無 `Add` derive），錯誤在 rustc 單態化階段才爆出，再由 aeroc 回指。

## 完整註解程式碼（`trait_bound_fail.aero` 全文）

```aero
// trait bound 反例（編譯失敗是預期的）：以不滿足 `Add` 命題的型別具現泛型。
// 檢查：aeroc check examples/trait_bound_fail.aero --json 必失敗，JSON 含 E0301（見 .md）。
// 對照組是 traits.aero（Int 滿足 Add+Clone，可通過）。

// 泛型求和：要求元素型別 T 同時滿足 Add（可 +）與 Clone（索引讀取需複製）。
fn sum[T: Add + Clone](items: Array[T]) -> T
  let mut total = items[0];
  let mut i = 1;
  while i < items.len()
    total = total + items[i];  // 具現時此處要求 T: Add
    i = i + 1;
  end
  return total;
end

// NoAdd 的欄位全是 Bool：Bool 無 Add 實作，編譯器的結構化證明推不出 Add 命題，
// 故不會為 NoAdd 自動衍生 Add 相關實作。
struct NoAdd { v: Bool } end

fn main() -> Unit
  let xs = [NoAdd { v: true }];
  // 以 T=NoAdd 具現 sum：單態化時 rustc 報 E0277，aeroc 轉成 E0301 指回此行。
  println(sum(xs));
end
```
