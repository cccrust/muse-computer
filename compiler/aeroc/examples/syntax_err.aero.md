# syntax_err.aero 解說

解析反例（編譯失敗是預期的）：缺分號，示範解析期錯誤 `E0001`。

## 內容

全文見下方「完整註解程式碼」；關鍵是第二行 `let x = 1` 缺少結尾 `;`，chumsky 解析器直接報錯。

## 預期行為

```sh
aeroc check examples/syntax_err.aero --json   # 失敗，exit != 0
# JSON 含 "error_code": "E0001"，位置指回第 2 行
```

本檔是唯一沒有同名 `.rust` 對照檔的範例（連 AST 都產不出來，無從 emit）。

## 完整註解程式碼（`syntax_err.aero` 全文）

```aero
// 解析反例（編譯失敗是預期的）：缺少分號，示範解析期錯誤 E0001。
// 檢查：aeroc check examples/syntax_err.aero --json 必失敗，JSON 含 E0001，指回第 2 行。
// 注意：本檔是唯一沒有同名 .rust 對照檔的範例（連 AST 都產不出來，無從 emit）。
fn main() -> Unit
  // 下行缺結尾 `;`：Aero 陳述式須以分號結尾，chumsky 解析器在此報錯。
  let x = 1
  println(x);
end
```
