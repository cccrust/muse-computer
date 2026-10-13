# MiniLang 自舉編譯器（Lean 4）

```
lake build                 # 建置 stage0（Lean 寫的編譯器 + VM）
./bootstrap.sh             # 三階段自舉並驗證不動點
.lake/build/bin/minic run examples/fib.mini </dev/null
.lake/build/bin/minic list examples/fib.mini      # 看 IR 組合語言
```

| 檔案 | 說明 |
|---|---|
| `Main.lean` | stage0：Lean 寫的詞法/語法分析、IR 產生器、堆疊 VM、自舉驅動 |
| `compiler.mini` | 用 MiniLang 寫的 MiniLang 編譯器（單趟、不建 AST） |
| `examples/` | 範例程式（遞迴、陣列、字串、I/O、短路求值） |

注意：`minic run` 會先讀完整個 stdin，沒有輸入時請加 `</dev/null`。
