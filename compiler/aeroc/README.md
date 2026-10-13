# aeroc — Aero → Rust 轉譯器

    cargo build --release
    aeroc emit  examples/demo.aero                 # 只看產生的 Rust
    aeroc run   examples/modules/main.aero         # 轉譯 + cargo 編譯 + 執行（自動載入 import 的模組）
    aeroc check examples/moved.aero --json         # 規格 §7 的 LLM 診斷 JSON（位置為 .aero 檔/行/列）
    ./test.sh                                      # 完整測試：正例輸出比對＋反例錯誤碼＋產碼結構斷言
    ./run.sh                                       # 跑全部案例（所有正例＋反例示範）

  每個範例旁另有同名 `.rust` 對照檔（如 `traits.aero` → `traits.rust`），
  即 `aeroc emit` 的輸出（`syntax_err.aero` 為解析反例，無對照檔）；
  範例更新後以 `for f in $(find examples -name "*.aero"); do aeroc emit $f > ${f%.aero}.rust; done` 再生。

架構：chumsky(解析) → loader(多檔) → AST → quote!(產碼) → syn/prettyplease(驗證排版) → cargo/rustc
GC：現成的 `gc` crate（cycle-collecting tracing GC），`gc T` ⇒ `Gc<GcCell<T>>`

## 規格之外新增的語法
- `if c ... else if c ... else ... end`、`&&`、一元 `-` / `!`、索引 `a[i]`
- `impl Type ... end`：`&self` / `&mut self` / `self`，靜態函式 `Type::new()`
- **模組**：`import "geometry.aero" as geo;`（省略 `as` 則以檔名為別名）
  - 路徑相對於 import 它的檔案；同一檔案只載入一次；允許循環引用
  - 以 `geo::area(..)`、`geo::Point { .. }`、`geo::Point::new(..)`、`geo::Shape::Circle(..)` 存取
  - 實作：各模組符號加前綴（`geometry__area`）後合併成單一 crate；根模組不加前綴
- **enum**：`enum Shape  Circle(Int), Rect(Int, Int), Dot  end`（`gc enum` 亦可）
- **match**（陳述式）：
  ```
  match v
    case Shape::Circle(r) if r > 0 ... 
    case 1 | 2 | 3 ...
    case Some(x) ...
    case _ ...
  end
  ```
  支援 `_`、綁定、Int/String/Bool 字面值、`|`、`if` guard、巢狀 pattern；漏掉分支時 rustc 會報 E0004 → Aero `E0200`
- **Option[T]**：`Some(x)` / `None`
- **crates.io**：`import crate "axum" version = "0.7";` 加依賴，原生路徑直接呼叫
  （`axum::Router::new()`、方法鏈、`serde_json::to_string(&p).unwrap()` 全直通，無需 binding 層）
  - Rust API 取 `&str` 時：字面值先綁給變數再 `&` 借用（`&String` 可 deref 成 `&str`）；`Result` 以 `.unwrap()` 處理
  - 範例：`axum_server.aero`（axum web server，`/: hello, aero`、`/health: ok`）、
    `crates_json.aero`（serde 自動衍生往返）、`crates_regex.aero`
- **trait 與自動特徵（Auto-Traits）**：trait 是「型態滿足特定命題的證明」，LLM 永遠不用手寫 `impl Trait for Type`
  ```
  trait Measurable
    fn area(&self) -> Int;
  end
  struct Point { x: Int, y: Int } end
  impl Point
    fn area(&self) -> Int
      return self.x * self.y;
    end
  end
  fn describe[M: Measurable](m: &M) -> Int
    return m.area();
  end
  ```
  - 固有方法覆蓋 trait 全部方法時，編譯器自動產生 `impl Trait for Type` 橋接（手動 `impl Trait for Type ... end` 亦可，存在時自動橋接讓路）
  - 內建特徵直接用作邊界：`Clone`、`Equatable`、`Comparable`、`Hashable`、`Serializable`、`Add`/`Sub`/`Mul`/`Div`/`Rem`、`Display`、`Debug`、`Default`、`Iterator`
    （`fn sum[T: Add + Clone](items: Array[T]) -> T`；注意泛型下索引讀取會 `.clone()`，算術約束通常需並列 `Clone`）
  - 結構化自動衍生：欄位全部可證時自動加上對應 derive（`Comparable`→`PartialOrd`、`Serializable`→`serde` derive 並自動加依賴，等）
  - 泛型結構：`struct Pair[T: Clone] { first: T, second: T } end`，具現 `Pair[Int]`
  - 驗證外包給 rustc：E0277→`E0301`（邊界未滿足）、E0599→`E0302`（方法缺失）、未知 trait 名→`E0303`

## 錯誤回溯
每個宣告/陳述式在產碼時帶 `#[aero_at(檔案, 行, 列)]` 標記，排版後剝除並建立對照表；
rustc 的錯誤因此能指回正確的 .aero 檔案與行列（粒度為陳述式）。
