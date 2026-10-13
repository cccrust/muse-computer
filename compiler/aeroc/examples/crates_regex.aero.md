# crates_regex.aero 解說

crates.io 範例：`regex` 方法鏈直通，展示 `&str` 借用慣例。

## 重點語法

- `import crate "regex" version = "1";`
- `regex::Regex::new(&pat).unwrap()`：構造＋`Result` 解包＋方法鏈全直通。

## 逐段解說

Rust API 取 `&str` 時，Aero 的字串字面值是 owned `String`，須先綁給具名變數再 `&` 借用（`&String` 可 deref 成 `&str`）。之後 `re.is_match(&a)`（`"abc123"` 含數字→`true`）、`re.is_match(&b)`（`"abc"`→`false`）。

## 執行與預期輸出

```sh
aeroc run examples/crates_regex.aero
# true|false|
```

## 完整註解程式碼（`crates_regex.aero` 全文）

```aero
// crates.io 範例：regex 方法鏈直通，展示 `&str` 借用慣例。
// 執行：aeroc run examples/crates_regex.aero（預期輸出見 crates_regex.aero.md）

// 宣告外部依賴：regex v1（`Regex::new(..).unwrap()` 的方法鏈原樣直通 Rust，無需 binding 層）。
import crate "regex" version = "1";

fn main() -> Unit
  // Rust API 取 &str 時：Aero 字串字面值是 owned String，
  // 須先綁給具名變數再 `&` 借用（&String 可 deref 成 &str，直接寫 &"[0-9]+" 不合法）。
  let pat = "[0-9]+";
  // 編譯正則：Regex::new 回 Result，以 .unwrap() 解包（失敗即 panic）。
  let re = regex::Regex::new(&pat).unwrap();
  let a = "abc123";
  let b = "abc";
  // is_match 取 &str：含數字串命中，不含則否。
  println(re.is_match(&a));    // true
  println(re.is_match(&b));    // false
end
```
