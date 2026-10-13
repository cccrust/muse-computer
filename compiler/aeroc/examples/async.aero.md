# async.aero 解說

最小非同步範例：`async fn`、`.await`、回傳閉包的高階函式。

## 重點語法

- `async fn fetch(n: Int) -> Int`：非同步函式。
- `fetch(4).await`：以 `.await` 取值（注意是後綴點語法）。
- `fn make_adder(base: Int) -> fn(Int) -> Int`：回傳函式型別。
- 閉包捕獲 `base` 時需包在 `managed` 內（讓捕獲的環境走 GC/`Rc`，避免借用逃逸）。

## 逐段解說

1. `fetch`：`n * 10`，`fetch(4).await` 得 40。
2. `make_adder(5)` 回傳 `|x| x + 5`，`add5(37)` 得 42。

## 執行與預期輸出

```sh
aeroc run examples/async.aero
# 40|42|
```

## 對應 Rust（`async.rust`）

- `async fn` 直譯為 Rust `async fn`；`main` 亦為 `async`（runtime 由 aeroc 產碼自動接 tokio）。
- `.await` 直通；回傳的閉包譯為 `Rc<dyn Fn>` 類型。

## 完整註解程式碼（`async.aero` 全文）

```aero
// 最小非同步範例：async fn、.await、回傳閉包的高階函式。
// 執行：aeroc run examples/async.aero（預期輸出見 async.aero.md）

// 非同步函式：直通 Rust async fn；呼叫方以 `.await` 取值。
async fn fetch(n: Int) -> Int
  return n * 10;
end

// 高階函式：回傳 `fn(Int) -> Int`（函式型別）。
// 閉包捕獲了參數 base，須包在 managed 內讓捕獲環境走 GC/Rc，避免借用逃逸。
fn make_adder(base: Int) -> fn(Int) -> Int
  managed {
    // lambda：`|x: Int| -> Int x + base end`，base 被捕獲進閉包。
    return |x: Int| -> Int x + base end;
  }
end

// 非同步入口：async main 由產碼自動接上 runtime。
async fn main() -> Unit
  // fetch(4).await = 40（後綴點語法）。
  let v = fetch(4).await;
  println(v);                  // 40
  // make_adder(5) 得加 5 函式；add5(37) = 42。
  let add5 = make_adder(5);
  println(add5(37));           // 42
end
```
