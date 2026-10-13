# axum_server.aero 解說

crates.io 實戰：以 `axum`＋`tokio` 開 HTTP server，證明 Aero 可直接駕馭原生 Rust 生態。

## 重點語法

- 雙依賴：`import crate "axum" version = "0.7";`＋`import crate "tokio" version = "1", features = ["full"];`。
- handler 就是普通 `async fn hello() -> String`，無需框架宏。
- `axum::Router::new().route(...)`、`axum::routing::get(...)`、`tokio::net::TcpListener::bind(...).await.unwrap()`、`axum::serve(listener, app).await` 全直通。

## `&str` 慣例

`route` 要 `&str`，字面值須先綁變數再借用（完整程式碼見下方，如 `let root = "/";`、`let health_path = "/health";`）。

## 執行與驗證（見 `test.sh`）

```sh
aeroc build examples/axum_server.aero --out-dir target/aero/t_axum
./target/aero/t_axum/target/debug/axum_server &   # 常駐不退，僅 build 不 run
curl http://127.0.0.1:18080/        # hello, aero
curl http://127.0.0.1:18080/health  # ok
```

`/` 回 `hello, aero`，`/health` 回 `ok`。`run.sh` 只編譯不啟動，避免卡住。

## 完整註解程式碼（`axum_server.aero` 全文）

```aero
// 以 crates.io 的 axum 開 web server：證明 Aero 可直接駕馭原生 Rust 生態。
// Aero 這側只有 `import crate`＋路徑直通呼叫，其餘全是原生 Rust API：
// Router / routing::get / TcpListener / serve 原樣直達，handler 就是普通 async fn。
// 驗證：僅 build 不 run（server 常駐不退），curl 兩個端點（見 axum_server.aero.md）。

// 宣告依賴：axum 0.7（web 框架）+ tokio 1 full（async runtime）。
import crate "axum" version = "0.7";
import crate "tokio" version = "1", features = ["full"];

// 路由 handler：普通 async fn，回 String 即回 200 純文字。
async fn hello() -> String
  return "hello, aero";
end

// 健康檢查 handler：回 "ok"。
async fn health() -> String
  return "ok";
end

async fn main() -> Unit
  // axum 的 route 要 `&str`：Aero 字串字面值是 owned String，
  // 先綁給具名變數再 `&` 借用（Zero-GC 借用語義直通，&String 可 deref 成 &str）。
  let root = "/";
  let health_path = "/health";
  // 建 Router：`/` 綁 hello，`/health` 綁 health（方法鏈直通）。
  let app = axum::Router::new().route(&root, axum::routing::get(hello)).route(&health_path, axum::routing::get(health));
  // 綁定監聽埠：bind 是 async 且回傳 Result，故先 .await 再 .unwrap()。
  let listener = tokio::net::TcpListener::bind("127.0.0.1:18080").await.unwrap();
  println("listening on 18080");
  // 啟動服務：serve 也是 async，以 .await 驅動（此 future 永不結束，故只能 build 驗證）。
  axum::serve(listener, app).await;
end
```
