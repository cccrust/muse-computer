# modules/util/mathx.aero 解說

巢狀目錄模組（`modules/util/` 下）：最簡函式庫，被兩處引用。

## 內容

- `fn square(n: Int) -> Int`：平方。
- `fn helper() -> Int` 回 100：第三個同名 `helper`，與 `geometry`/`main` 的版本共存。

## 被誰引用

- `geometry.aero` 以別名 `mx` 引用：`mx::square`（`dist2`、`area` 內部）。
- `main.aero` 以預設別名（檔名）引用：`mathx::square(9)`、`mathx::helper()`。

## 完整註解程式碼（`mathx.aero` 全文）

```aero
// 巢狀目錄模組（modules/util/ 下）：最簡函式庫，被 geometry 與 main 兩處引用。
// geometry 以別名 mx 引用（mx::square）；main 以預設別名（檔名）引用（mathx::square）。

// 平方：dist2、area、main 的共用底層。
fn square(n: Int) -> Int
  return n * n;
end

// 本模組的 helper（回 100）：三個同名 helper 之一，合併時加 mathx__ 前綴區分。
fn helper() -> Int
  return 100;
end
```
