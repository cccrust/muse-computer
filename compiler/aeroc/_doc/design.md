# Aero 設計想法（Design）

本文回答「為什麼 Aero 長這樣」。每個決策先講**要解決的問題**，
再講**選了什麼**，最後講**代價**——代價誠實寫，因為它們就是
`_doc/grammar.md` 第 7 章那些限制的來源。

相關文件：教學 `_doc/tutorial.md`；語法 `_doc/grammar.md`；用法 `README.md`。

---

## 0. 一句話定位

**Aero 是「給 LLM 寫、轉譯成 Rust、人類讀得懂」的薄語法層。**

設計目標只有三個，按優先序：

1. **LLM 寫出來一次就對**（語法寬容、錯誤可自修）。
2. **零成本轉譯**（Zero-GC 程式就是 Rust，不為抽象付執行期代價）。
3. **直通 Rust 生態**（`crates.io` 零 binding 層可用）。

非目標：取代 Rust；做自己的優化後端；做自己的型別理論創新。
Aero 的每一行複雜設計，幾乎都是「把難題丟給 rustc」的包裝。

---

## 1. 為什麼是轉譯，而不是新後端

寫一個語言最貴的三樣東西：後端優化、借用檢查、標準函式庫生態。
Aero 三樣全不寫：

- **後端**：產出正規 Rust，優化（`opt-level = 3`、`lto = true`、
  單 codegen unit，見 `driver.rs`）與機器碼生成全是 rustc＋LLVM 的。
  實測 1 千萬次 push＋加總與手寫 Rust 同秒（見 benchmark 討論）。
- **借用檢查**：不實作，直接把 Aero 所有權語義 1:1 映射到 Rust 語義，
  讓 rustc 當 Borrow Checker。`E0382`→`E0102` 這類映射只是翻譯層。
- **生態**：`import crate` 只是在產生的 `Cargo.toml` 加一行依賴，
  呼叫是路徑直通。axum 範例證明：15 行 Aero 就能開站，
  背後是整個 `crates.io`。

代價：Aero 永遠帶著 Rust 的影子——`&str` 借用規則、`Result` 要處理、
`String` 欄位不能從共享借用搬走。這些在教學裡寫成「鐵律」，
本質是「轉譯器的誠實」：不發明語義，只翻譯語義。凡是 Rust 要求顯式的，
Aero 也要求顯式，否則轉譯就不再是零成本的對應。

---

## 2. 雙記憶體模式：預設零成本，逃生用 GC

LLM 最不擅長的就是所有權迷宮，但 Rust 證明了所有權值得學。
折衷是**預設 Zero-GC**（全套所有權語義，速度＝Rust），
**按需進入 `managed`＋`gc`**（共享與循環引用）：

```aero
managed {
  let a = gc Node { value: 1, links: [] };   // 三件套：gc 宣告＋gc 配置＋managed 區
}
```

設計細節：

- **GC 不自己寫**，用現成 `gc` crate（cycle-collecting tracing GC），
  `gc T` 即 `Gc<GcCell<T>>`。自己寫 GC 是無底洞，借一個夠用的。
- **編譯器自動插借用**：穿越 gc 的讀寫按「輕量型別推導」插入
  `.borrow()`／`.borrow_mut()`／`.clone()`，LLM 照直覺寫欄位即可。
- **兩次掃描決定依賴**：先探測程式是否碰 GC，純 Zero-GC 程式
  **完全不依賴 gc crate**（`CodeGen::generate` 的 probe＋重跑）。
  依賴是按需出現的，不是預設稅。
- **RefCell 重入是已知敵人**：`a.v = a.v + 1` 這類自讀自寫，
  產碼時先把右值與索引算成暫存再取 `borrow_mut`
 （`hoist_indices`），外加迴圈與 match 的快照策略。不能全解，
 但把最常見的觸發條件都擋掉了。

代價：GC 模式的速度與心智成本（`clone_to_owned` 的顯式跨越、
`Gc` 只有 `Clone` 證明）。立場很明：**拿 GC 換開發速度是自願的**，
預設路徑永遠是零成本的。

---

## 3. 為 LLM 設計的語法面

LLM 寫程式的錯誤有統計傾向：括號配對、縮排敏感、分號遺漏、
啞鈴型樣板（`impl Trait for Struct` 的重複勞動）。Aero 的語法回應：

- **`end` 結尾、縮排無義**：`if/while/for/match/fn` 一律 `end` 收尾。
  比花括號多幾個字元，但 LLM 配對成功率高得多，且錯誤定位精確到陳述式。
- **陳述式一律 `;`**：解析器看到缺分號立刻停，錯誤不會滾雪球。
  這是 `E0001` 好讀的主因。
- **管線 `|>` 與短 lambda**：`5 |> add(4)`、`|x: Int| -> Int x + 1 end`
  把「函式組合」壓成一行，減少中間變數命名（LLM 的命名是噪音源）。
- **match 是陳述式**：表達式版 match 需要型別統一推導，陳述式版
  讓每條分支各自 `return`，推導壓力歸零；漏分支由 rustc `E0004` 轉 `E0200` 補上。
- **手寫 `impl` 極小化**：固有方法照寫，trait 橋接編譯器補（下一章）。

反面例子是 `managed { }` 不用 `end` 而 `unsafe { }` 要——歷史遺留的不對稱，
已在 grammar 標註，未來應統一（見第 9 章）。

---

## 4. Trait：Curry-Howard 視角的自動特徵

這是 Aero 唯一的「理論梗」，但它是為現實服務的：

- **邏輯視角**：型別是命題，trait 是「型態滿足某性質」的證明項，
  編譯器是證明檢查器。`fn sum[T: Add]` 唸作「對一切滿足可加命題的 T」。
- **現實視角**：`crates.io`（Tokio、Serde、axum…）全站在 trait 上。
  沒有 trait，轉譯的 Rust 接不上生態，大門關閉。
- **LLM 視角**：手寫 `impl` 是樣板地獄，所以**證明由編譯器結構化推導**：
  欄位全是 `Int` 的 struct，自動得 `PartialEq/PartialOrd/Hash/Debug/
  Default/Serialize`（`emit` 可見）；固有方法長齊的型別，自動得
  `impl Trait for Type` 橋接。LLM 只寫規格（`trait` 簽章）與本體（固有方法），
  證明項是產物，不是手寫稿。

關鍵決策是**驗證外包**：結構化證明只負責「充分條件→自動 derive」，
真正的判定（單態化後的邊界滿足）交給 rustc，失敗轉成 `E0301` 指回 `.aero`。
這跟借用檢查外包是同一哲學：**aeroc 從不證明它證不出的東西，
只把好證的部分自動化，難的部分翻譯錯誤**。

代價：第 7 章限制的半壁江山來自這裡——方法級泛型被忽略、
`for` 後不能是泛型具現、自訂型別不能實作 `Add`。
每一條都是「證明引擎還沒長出那塊」的誠實形狀。

---

## 5. crates.io 互操作：零 binding 層

`import crate "serde_json" version = "1";` 之後，
`serde_json::to_string(&p).unwrap()` 原樣直通——解析器把未知路徑
原樣送進產碼，rustc 做最終裁判。這表示：

- 任何 crate 的任何 API 第一天就能用，不等 binding。
- 代價是 Aero 幫不上型別忙：`&str` 要自己借、`Result` 要自己 unwrap、
  turbofish 寫不出來（靠 `let q: Point` 標註反推）。
  這些在教學第 6 章寫成鐵律，本質是「直通」的另一面。

`Future`、`Option`、`async main` 自動加 tokio 依賴等內建，
都是為了讓「直通」在常見路上零摩擦。axum 範例是這條路線的驗收測試，
也被收進 `test.sh`（背景啟動＋curl 雙端點）。

---

## 6. 錯誤即 API：給 LLM 自修的診斷

`aeroc check --json` 是規格 §7 的 LLM 診斷介面，設計成「可行動」：

- 每個宣告／陳述式產碼時帶 `#[aero_at(檔, 行, 列)]` 標記，
  排版後剝除並建成對照表，rustc 行號精確指回 `.aero`（陳述式粒度）。
- rustc 錯誤碼映射為 Aero 錯誤碼並附 `suggestion.action/patch`
 （如 E0382→E0102 附「改用 `&x`」），LLM 不需懂 rustc 行話。
- 解析錯（E0001）走 ariadne 漂亮輸出給人看，JSON 給機器看，
  同一錯誤兩種觀眾。

這是「寫出來一次就對」的另一半：第一次不對時，第二次的資訊要夠好。
`test.sh` 的反例段（E0102／E0001／E0301）就是這條管線的迴歸測試。

---

## 7. 刻意不做的事

- **不做型別推導的完全體**：`infer` 只求「夠決定 gc 借用與 clone」，
  其餘留給 rustc。完整推導是研究專案，不是工具專案。
- **不做運算子多載的表面語法**：自訂型別的 `+` 目前無解，
  寧可讓它明確失敗（E0301），也不發明半套語義。
- **不做 match 表達式、結構體樣式**：等價表達力已存在
 （分支內 `return`、先 `let` 拆欄位），語法糖排在正確性之後。
- **不隱藏 Rust 語義**：凡 rustc 要求顯式的，Aero 都顯式。
  「像 Rust 一樣快」的承諾，靠的就是這條不隱藏。

---

## 8. 效能立場（一句話＋一個數字）

Zero-GC＝Rust（1 千萬次 push＋加總同為 0.01–0.02s，同 profile 下噪音內無差異）；
用到 `gc` 就是用速度換開發速度，自願且顯式（`managed` 區塊是可見標記）。
編譯期代價是全量 cargo 構建＋LTO（慢在構建，不在執行），
`check` 先行的工作流就是為此設計的。

---

## 9. 未來方向（按價值排序）

1. **統一區塊結尾**：`managed` 補 `end`（或全改花括號），消滅不對稱。
2. **方法級泛型真正支援**：trait 方法的 `[T: …]` 不再靜默忽略。
3. **`impl Trait for Pair[T]`**：`for` 後接受泛型具現。
4. **含參 attribute**：`#[allow(dead_code)]` 類正確轉發。
5. **運算子實作面**：給自訂型別一條到 `std::ops` 的路（可能是
   `impl Add for Score` 的受限形式），讓練習題 3 的泛型版可解。
6. **match 表達式**：等前五項做完再考慮，純糖。
