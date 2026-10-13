# moved.aero 解說

所有權反例（編譯失敗是預期的）：`move` 後再使用，示範借用檢查器外包給 rustc＋錯誤指回 `.aero`。

## 內容

`consume(move user_data)` 把 `User` 的所有權移入函式；第二行再次 `consume(move user_data)` 時值已失效。

## 預期行為

```sh
aeroc check examples/moved.aero --json   # 失敗，exit != 0
# JSON 含 "error_code": "E0102"，位置指回第二個 consume 那一行
```

rustc 的 use-after-move 被 aeroc 的 `#[aero_at]` 對照表翻譯為 `E0102`（LLM 可讀診斷 JSON，見規格 §7）。

## 對應 Rust（`moved.rust`）

`move user_data` 直譯為 Rust 的 move 語義（按值傳參）；第二次使用即觸發 rustc `E0382` → 轉寫為 `E0102`。

## 完整註解程式碼（`moved.aero` 全文）

```aero
// 所有權反例（編譯失敗是預期的）：`move` 後再使用。
// 檢查：aeroc check examples/moved.aero --json 必失敗，JSON 含 E0102（見 moved.aero.md）。
// 原理：所有權驗證外包給 rustc，aeroc 以 #[aero_at] 對照表把 use-after-move 指回 .aero 行列。

// Zero-GC 結構：按值傳遞即移動所有權（非 Copy 型別）。
struct User { name: String } end

// 按值取參：呼叫 `consume(move u)` 時所有權移入函式，呼叫方失去 u。
fn consume(u: User) -> Unit
  println(u.name);
end

fn main() -> Unit
  let user_data = User { name: "bob" };
  // 第一次 move：合法，user_data 所有權移入 consume。
  consume(move user_data);
  // 第二次 move：非法！user_data 已失效，觸發 E0102（rustc E0382 轉寫）。
  consume(move user_data);
end
```
