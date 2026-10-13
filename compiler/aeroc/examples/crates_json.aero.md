# crates_json.aero 解說

crates.io 範例：`serde_json` 序列化往返，展示 `Serializable` 自動衍生＋原生路徑直通。

## 重點語法

- `import crate "serde_json" version = "1";`：宣告外部依賴（產碼進 `Cargo.toml`）。
- `serde_json::to_string(&p).unwrap()`：路徑與方法鏈原樣直通 Rust，無需 binding 層。
- `let q: Point = serde_json::from_str(&s).unwrap();`：具現型別由左側標註反推。

## 逐段解說

1. `Point { x: 3, y: 4 }` 的 `Serializable` 證明由編譯器自動完成（欄位全是 `Int`），emit 可見 serde derive。
2. `to_string` 得 `{"x":3,"y":4}` 並印出；`from_str` 反序列化回 `Point`，`q.x+q.y`=7。

## 執行與預期輸出

```sh
aeroc run examples/crates_json.aero
# {"x":3,"y":4}|7|
```

## 注意

`Result` 以 `.unwrap()` 處理；`&p`/`&s` 的借用語義直通 Rust。

## 完整註解程式碼（`crates_json.aero` 全文）

```aero
// crates.io 範例：serde_json 序列化往返，展示 Serializable 自動衍生＋原生路徑直通。
// 執行：aeroc run examples/crates_json.aero（預期輸出見 crates_json.aero.md）

// 宣告外部依賴：產碼時寫入 Cargo.toml（serde_json v1）。
// Point 的 Serializable 證明由編譯器自動衍生（見 emit 的 serde derive），
// 所以 to_string / from_str 可直接用，無需手寫任何 impl。
import crate "serde_json" version = "1";

// 普通結構：欄位全是 Int，故自動具備 Serializable（另含 Clone/Equatable/...）。
struct Point { x: Int, y: Int } end

fn main() -> Unit
  let p = Point { x: 3, y: 4 };
  // 序列化：`serde_json::to_string` 路徑直通 Rust；Result 以 .unwrap() 處理；`&p` 共享借用。
  let s = serde_json::to_string(&p).unwrap();
  println(&s);                 // {"x":3,"y":4}（借用列印）
  // 反序列化：具現型別由左側 `let q: Point` 標註反推（Rust 型別推導）；`&s` 借用 JSON 字串。
  let q: Point = serde_json::from_str(&s).unwrap();
  println(q.x + q.y);          // 3 + 4 = 7
end
```
