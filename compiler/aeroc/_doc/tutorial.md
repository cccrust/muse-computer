# Aero Tutorial

從零開始學 Aero：一個轉譯到 Rust 的小語言，為 LLM 寫程式而設計，
但人類讀起來也像偽代碼。本教學所有完整程式都可在 `compiler/aeroc/` 下存檔執行。

```bash
cd compiler/aeroc
cargo build --release
```

約定：每個 `aero` 程式碼塊都是一個完整檔案（例如存成 `hello.aero`），跑法都是：

```bash
./target/release/aeroc run hello.aero
```

本篇分兩部分：**上篇 §§0–8** 走一遍操作流（函式、控制流、所有權、trait、
crates.io、除錯）；**下篇 §§9–17** 漫遊型別系統。形式語法見 `_doc/grammar.md`，
設計取捨見 `_doc/design.md`。

---

# 上篇：操作流

---

## 0. 心智模型（先讀這段，少踩一半坑）

1. **Aero 不是直譯器，是 Rust 轉譯器。** 你的程式會變成正規 Rust，再交給 `rustc`
   編譯。Zero-GC 模式的速度就是 Rust（兩者同一後端，實測無差距）。
2. **三條語法鐵律**：每個陳述式以 `;` 結尾；每個區塊以 `end` 閉合；
   縮排不具意義。違反就是 `E0001`。
3. **兩種記憶體模式**：預設 Zero-GC（所有權＋借用，跟 Rust 同規則，
   檢查外包給 rustc）；需要共享或循環引用時才用 `managed`＋`gc`（第 3 章）。

先確認工具鏈正常：

```bash
./target/release/aeroc run examples/demo.aero   # 應印出 12 行數字與字串
./test.sh                                        # 完整測試，預期全過
```

---

## 1. 第一個程式：函式、變數、管線

```aero
fn add(a: Int, b: Int) -> Int
  return a + b;
end

fn main() -> Unit
  println(add(3, 4));
  println(5 |> add(4));
  let inc = |x: Int| -> Int x + 1 end;
  println(inc(41));
end
```

預期輸出：`7`、`9`、`42`。

要點：

- 型別只有幾種：`Int`、`Float`、`String`、`Bool`、`Unit`（即空資訊型別，
  當回傳型別時可理解為「無回傳值」）、`Array[T]`、`Dict[K, V]`、`Option[T]`。
  函式寫 `fn 名(參數) -> 回傳型 ... end`。各型別詳見下篇。
- `let` 預設不可變，要改加 `mut`：`let mut i = 0;`。型別標註可省略（會推導），也可寫：`let x: Int = 3;`。
- `x |> f(a)` 是「把 x 當第一個參數餵給 f」，即 `f(x, a)`。`5 |> add(4)` 就是 `add(5, 4)`。
- `|x: Int| -> Int ... end` 是 lambda（閉包），可存、可傳。
- `println` 吃任何可顯示的值；字串相加用 `+`（`"a" + "b"`）。
- 註解是 `//`。

常見第一錯：忘記 `;` 或 `end` → `E0001`，看錯誤行附近補上即可。

---

## 2. 控制流：if、while、for、match

```aero
fn classify(n: Int) -> String
  if n < 0
    return "negative";
  else if n == 0
    return "zero";
  else
    return "positive";
  end
end

fn main() -> Unit
  let mut i = 0;
  while i < 3
    println(classify(0 - 2 + i));
    i = i + 1;
  end
  let xs = [10, 20, 30];
  for x in &xs
    println(x);
  end
end
```

要點：`if/else if/else` 以 `end` 收尾；`while` 同理；`for x in 容器 ... end`
（`&xs` 表示借用來巡，容器本身不被吃掉）。`&&`、`||`、一元 `-`、`!`、索引 `a[i]` 都有。

### match 與 enum（Aero 最強的分派工具）

```aero
enum Shape
  Circle(Int),
  Rect(Int, Int),
  Dot
end

fn area(s: &Shape) -> Int
  match s
    case Shape::Circle(r)
      return 3 * r * r;
    case Shape::Rect(w, h)
      return w * h;
    case Shape::Dot
      return 0;
  end
end
```

要點：`case` 可接字面值（`case 1 | 2 | 3`）、繫結（`case Some(x)`）、萬用（`case _`）、
守衛（`case x if x < 0`）。漏分支時 rustc 會擋下並轉成 Aero 的 `E0200`，
照提示補 `case _` 即可。`Option[T]` 就是 `Some(x)`／`None`，
查表找不到、開頭元素這類「可能沒有」一律用它（見 `examples/enums.aero` 的 `find`）。

---

## 3. 資料與方法：struct、impl、gc

```aero
struct Counter { mut n: Int } end

impl Counter
  fn new(start: Int) -> Counter
    return Counter { n: start };
  end

  fn inc(&mut self) -> Unit
    self.n = self.n + 1;
  end

  fn get(&self) -> Int
    return self.n;
  end
end

fn main() -> Unit
  let mut c = Counter::new(10);
  c.inc();
  println(c.get());
end
```

預期輸出：`11`。要點：`impl Type ... end` 放方法，接收者三種——`&self`（唯讀）、
`&mut self`（可改）、`self`（吃掉）；`Type::new(..)` 是靜態函式（無接收者）。

### 什麼時候用 gc

預設 Zero-GC 下，每個值同一時間只有一個主人（見第 4 章）。需要**共享**
（多個別名同時可改）或**循環引用**時，才請出 GC：

```aero
gc struct Node { mut value: Int, mut links: Array[gc Node] } end

fn main() -> Unit
  managed {
    let a = gc Node { value: 1, links: [] };
    let b = gc Node { value: 2, links: [] };
    a.links.push(b);
    b.links.push(a);   // 循環引用：Zero-GC 做不到，GC 可以
    println(a.value);
  }
end
```

要點：`gc struct` 宣告＋`gc Type { ... }` 配置＋包在 `managed { ... }` 裡使用，
三件缺一不可。`gc` 變數當值用是共享（便宜），要拿回所有權才用
`.clone_to_owned()`。跨越 gc 讀欄位會自動處理借用，你照直覺寫就好。

---

## 4. 所有權三兄弟：move、`&`、`&mut`

這是 Zero-GC 唯一的心智負擔，規則只有三條：

1. 傳值即轉移主人（move），舊名不能再用。
2. `&x` 是借用（唯讀，可多個共存）；`&mut x` 是可變借用（同一時間只能一個）。
3. 函式簽章決定語義：`fn f(u: User)` 吃掉，`fn f(u: &User)` 只借。

下面是**反例**（`examples/moved.aero`，故意寫錯的）：

```aero
fn consume(u: User) -> Unit
  println(u.name.clone_to_owned());
end

fn main() -> Unit
  let user_data = User { name: "bob" };
  consume(move user_data);
  consume(move user_data);   // 錯！user_data 已在上一行送出
end
```

`check` 它，會得到指回本檔的診斷（`--json` 給 LLM 自修用）：

```bash
./target/release/aeroc check examples/moved.aero --json
# {"error_code": "E0102", "location": {"line": 10, ...}, ...}
```

**修正**：把簽章改成借用，呼叫處跟著改（完整可跑程式）：

```aero
struct User { age: Int } end

fn greet(u: &User) -> Unit
  println(u.age);
end

fn main() -> Unit
  let user_data = User { age: 30 };
  greet(&user_data);
  greet(&user_data);
end
```

預期輸出：`30`、`30`。

附帶一條** String 欄位規則**：非 `Copy` 的欄位（`String`、`Array`…）
不能從 `&` 借用中直接搬走，要複製一份出來：

```aero
struct User { name: String } end

fn greet_name(u: &User) -> Unit
  println(u.name.clone_to_owned());
end

fn main() -> Unit
  let user_data = User { name: "bob" };
  greet_name(&user_data);
  println(user_data.name.clone_to_owned() + "!");
end
```

預期輸出：`bob`、`bob!`。（`Int` 這類 `Copy` 型別不受此限，
前半的 `u.age` 才能直讀。）

---

## 5. trait 與自動特徵：證明交給編譯器

Aero 的 trait 不是物件導向介面，是「型態滿足某命題的證明」。
**你永遠不用手寫 `impl Trait for Type`**：只要固有方法長齊了，
編譯器自動產生橋接。

```aero
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

fn main() -> Unit
  let p = Point { x: 3, y: 4 };
  println(describe(&p));
  println(p == Point { x: 3, y: 4 });
end
```

預期輸出：`12`、`true`。

三件事一次看懂：

1. **`trait` 是命題規格**：方法只寫簽章（以 `;` 結尾），身體由各型別的
   `impl` 提供。`[M: Measurable]` 唸作「M 滿足 Measurable 命題」。
2. **自動橋接**：`Point` 有 `area(&self)`，編譯器自動補上
   `impl Measurable for Point`（`emit` 看得到）。真的需要手寫時才用
   `impl Trait for Type ... end`（逃生艙；存在時自動橋接讓路）。
3. **自動衍生**：`p == ...` 能成立，是因為編譯器證明了
   `Point` 的欄位全是 `Int`，自動派生 `PartialEq`（連同 `PartialOrd`、
   `Hash`、`Debug`、`Default`、`serde` 一起，`emit` 可見）。
   內建特徵名可直接當邊界：`Clone`、`Equatable`、`Comparable`、
   `Hashable`、`Serializable`、`Add`/`Sub`/`Mul`/`Div`/`Rem`、
   `Display`、`Debug`、`Default`、`Iterator`。
   例如 `fn sum[T: Add + Clone](items: Array[T]) -> T`（泛型下索引讀取會
   `.clone()`，所以算術約束通常要並列 `+ Clone`）。
   泛型結構：`struct Pair[T: Clone] { first: T, second: T } end`，使用
   `Pair[Int]`。完整示範見 `examples/traits.aero`。

---

## 6. crates.io：站在 Rust 生態上

一行 `import crate` 加依賴，原生路徑直接呼叫，沒有 binding 層要寫：

```aero
import crate "serde_json" version = "1";

struct Point { x: Int, y: Int } end

fn main() -> Unit
  let p = Point { x: 3, y: 4 };
  let s = serde_json::to_string(&p).unwrap();
  println(&s);
  let q: Point = serde_json::from_str(&s).unwrap();
  println(q.x + q.y);
end
```

預期輸出：`{"x":3,"y":4}`、`7`。注意 `from_str` 的具現型別是靠
`let q: Point` 標註反推的——想不起來就加標註。

**兩條互操作鐵律**（都是「如實轉發 Rust 語義」，不是 bug）：

- Rust API 取 `&str` 時：字面值先綁給變數再 `&` 借用
  （`&String` 會自動轉成 `&str`）。見 `examples/crates_regex.aero`。
- 回傳 `Result` 的调用：接 `.unwrap()`（見上例；`Option` 同理）。

### axum web server（15 行開站）

`examples/axum_server.aero` 全文，handler 就是普通 `async fn`
（`async main` 會自動長出 `#[tokio::main]`）：

```aero
import crate "axum" version = "0.7";
import crate "tokio" version = "1", features = ["full"];

async fn hello() -> String
  return "hello, aero";
end

async fn health() -> String
  return "ok";
end

async fn main() -> Unit
  let root = "/";
  let health_path = "/health";
  let app = axum::Router::new().route(&root, axum::routing::get(hello)).route(&health_path, axum::routing::get(health));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:18080").await.unwrap();
  println("listening on 18080");
  axum::serve(listener, app).await;
end
```

啟動（server 常駐，`run.sh` 只做編譯驗證，啟動用下面兩步）：

```bash
./target/release/aeroc build examples/axum_server.aero
./target/aero/axum_server/target/debug/axum_server &   # 看到 listening on 18080
curl http://127.0.0.1:18080/        # hello, aero
curl http://127.0.0.1:18080/health  # ok
kill %1
```

---

## 7. 讀懂錯誤：先看 error_code

| 代碼 | 意思 | 怎麼辦 |
|---|---|---|
| `E0001` | 語法錯誤 | 檢查 `;`、`end`、括號 |
| `E0102` | move 後再用 | 改傳 `&x`，或收端改 `&T` |
| `E0103`/`E0104` | 借用衝突 | 同一時間只留一個 `&mut` |
| `E0105` | 借用活太久 | 逃逸的東西改用 `managed`＋`gc` |
| `E0200` | match 漏分支 | 補 `case _` |
| `E0301` | 型別不滿足 trait 邊界 | 加強 `[T: …]`，或補固有方法觸發橋接 |
| `E0302` | 找不到方法 | 在 `impl Type` 補同名方法 |
| `E0303` | 未知的 trait 名 | 先 `trait Name … end` 宣告 |
| `E0900` | 其他 rustc 錯誤 | 看 message 內的原文 |

工作流：`check` 比 `run` 快（只做型別檢查不產生 binary），
先 `check`，過了再 `run`。錯誤定位永遠指回你的 `.aero` 檔行列
（陳述式粒度），`--json` 則是給 LLM 自我修復的格式。

---

## 8. 練習題

1. **計分器**：把第 3 章的 `Counter` 加一個 `reset(&mut self)`（歸零）
   與 `add(&mut self, k: Int) -> Int`（加 k 並回傳），`main` 裡走一遍。
   提示：答案長得像 `examples/methods.aero` 的 `add`。
2. **求值器**：用第 2 章的 `match` 為
   `enum Expr / Num(Int) / Add(Int, Int) / Neg(Int) / Zero end`
   寫 `eval(e: &Expr) -> Int`。提示：`examples/enums.aero` 有完整版可對答案。
3. **分數加總**：為 `struct Score { v: Int } end` 寫一個具體函式
   `fn sum_scores(items: Array[Score]) -> Score`（注意：泛型版
   `sum[T: Add + Clone]` 吃不下 `Score`，因為 Aero 目前無法為自訂型別
   實作 Rust 的 `Add`——想想為什麼第 5 章說「算術約束通常要並列 Clone」
   之外，還得是 Rust 原生就懂 `+` 的型別）。提示：迴圈＋`.v` 直讀即可，
   骨架跟第 5 章的 `sum` 一樣。

做完跑 `./test.sh`，全過代表環境與理解都沒歪。

---

# 下篇：型別漫遊

上篇走過操作流，這篇走過型別系統：每種型別長什麼樣、轉譯成什麼 Rust、
有什麼坑。本篇的完整程式同樣全部實跑驗證。

---

## 9. 基本型別：四種加兩種

```aero
fn main() -> Unit
  let a: Int = 42;
  let b: Float = 3.14;
  let c: String = "hi";
  let d: Bool = true;
  println(a);
  println(b);
  println(c);
  println(d);
  println("done");
end
```

輸出：`42`、`3.14`、`hi`、`true`、`done`。

| Aero | Rust | 字面值規則 |
|---|---|---|
| `Int` | `i64` | 十進位，超出 i64 即解析錯 |
| `Float` | `f64` | `3.14` 可，`.5`、指數不可 |
| `String` | `String` | 見下——最重要的坑 |
| `Bool` | `bool` | `true`／`false` |

另兩種基本型別：空資訊型別（`Unit`，即 `()`——只有一個值、攜帶零位元資訊，
當回傳型別時直接寫 `-> Unit`，產碼省略箭頭）與 `RawPointer`（即 `*mut c_void`，
只能在 `unsafe` 裡經 FFI 取得，寫不出字面值）。
註：「Unit」是型別論的舊名（乘積型別的單位元），唸作「空資訊」即可，
不要聯想成「單位」；它跟 `None` 的區別見第 11 章——需要分支處理的
「沒有」用 `None`，連分支都不需要的「沒有」用 `Unit`。

**String 是 owned 的**：`"hi"` 一落地就是擁有的 `String`
（`String::from`），不是借用。這帶來兩條後果，初學者十有八九各踩一次：

1. Rust API 要 `&str` 時不能直傳字面值，先綁變數再借用：
   `let p = "/"; f(&p)`（`&String` 會自動轉成 `&str`）。
2. `String` 欄位不能從 `&` 借用中搬走，複製用 `.clone_to_owned()`。
   詳見本篇第 4 章。

---

## 10. 容器：Array 與 Dict

```aero
fn main() -> Unit
  let xs: Array[Int] = [1, 2, 3];
  println(xs.len());
  println(xs[0] + xs[2]);
  let d = { "a": 1, "b": 2 };
  println(d["a"] + d["b"]);
end
```

輸出：`3`、`4`、`3`。

| Aero | Rust | 說明 |
|---|---|---|
| `Array[T]` | `Vec<T>` | `[1, 2]`；`push`／`len`／`[i]` 直通 |
| `Dict[K, V]` | `HashMap<K, V>` | `{"a": 1}`；`{}` 是空字典 |

三條索引規則：

- 下標是 `Int`，產碼自動轉 `as usize`；`len()` 回傳也自動轉回 `Int`。
  你永遠不用寫轉型。
- 越界即 panic（跟 Rust 的 `v[i]` 一樣，不是 Aero 的錯，是語義直通）。
- 字典下標 `d[k]` 內部取 `&k`，字面值直傳即可（`d["a"]` 合法，
  規則只卡在「函式參數是 `&str`」時——見第 9 章）。
- `{}` 必須加標註（`let d: Dict[String, Int] = {};`），否則 rustc 推不出來。

---

## 11. 可能沒有：Option

```aero
fn lookup(m: &Dict[String, Int], k: String) -> Option[Int]
  return None;
end

fn main() -> Unit
  let d = { "a": 1, "b": 2 };
  match lookup(&d, "a")
    case Some(v) println(v);
    case None println(-1);
  end
end
```

輸出：`-1`（本例的 `lookup` 永遠回 `None`，重點是形狀）。

`Option[T]` 即 Rust 的 `Option<T>`，取值只許 `match`：
`Some(v)` 綁定、`None` 兜底。凡是「查無此物」（查表、搜尋、首元素）
一律回 `Option` 而不是 sentinel 值（如 `-1`）——這是 Aero 的風格約定，
`examples/enums.aero` 的 `find` 是標準示範。

---

## 12. 函式即值：閉包型別

```aero
fn apply(f: fn (Int) -> Int, x: Int) -> Int
  return f(x);
end

fn main() -> Unit
  let double = |x: Int| -> Int x * 2 end;
  println(apply(double, 21));
end
```

輸出：`42`。

- 閉包型別寫 `fn (參數型) -> 回傳型`，轉譯為 `Rc<dyn Fn…>`（可複製、可存、可傳）。
- 值寫 `|參數| -> 回傳型 單一表達式 end` 或多陳述式體（見本篇第 1 章）。
- 非同步版是 `async fn (A) -> R`， 一般只在 `async fn` 與 axum handler 裡遇到
  （`examples/axum_server.aero`）；`Future[T]` 型別本身很少手寫，
  它是 `async` 的回傳形狀，`.await` 拆開。

---

## 13. 借用型別：&T 與 &mut T

```aero
fn main() -> Unit
  let n = 5;
  let r: &Int = &n;
  println(r);
end
```

輸出：`5`。`&` 只能借**具名變數**（`&n` 可，`&5` 不可），
`&mut` 同理。完整三兄弟規則（move／`&`／`&mut`）在本篇第 4 章，
這裡只補一條型別視角的判據：

- `Int`、`Float`、`Bool`、`Unit` 是 `Copy` 的：經過 `&` 讀出來不用複製。
- 其他（`String`、`Array`、自訂 struct）經過 `&` 要拿走所有權時，
  必須顯式複製（`.clone_to_owned()`），或把函式簽章改成收 `&T`。
  gc 包住的（下一章）連複製語義都不同：複製的只是控制代碼（共享），
  要整份搬走才用 `.clone_to_owned()`。

---

## 14. 自訂型別：struct、enum、泛型具現、Self

```aero
struct Pair[T: Clone] { first: T, second: T } end
```

- `struct`／`enum` 即 Rust 同名項，欄位全公開（`pub`）。
- `Name[A, B]` 是泛型具現（`Pair[Int]`），定義處形參寫 `[T: Clone]`，
  邊界即「命題」，驗證交 rustc（詳見本篇第 5 章）。
- `Self` 只在 trait 簽章與方法內有意義，指「實作此 trait 的型別」。
- 單個大寫名（`T`）在泛型作用域內自動是形參，不必宣告。

完整可跑例：`examples/traits.aero`（`Point`＋`Pair`＋自動 derive 全餐）。

---

## 15. gc 修飾：堆上的型別

任何型別前可加 `gc`：`gc Node` 即 `Gc<GcCell<Node>>`。
它不是新類型，是**同一類型換了分配位置與訪問協議**
（每次穿越自動 `.borrow()`，讀值自動共享）：

```aero
gc struct Node { mut value: Int, mut links: Array[gc Node] } end
```

三件套缺一不可：`gc struct` 宣告＋`gc Node {…}` 配置＋`managed {…}` 使用區。
`gc` 包住的型別在證明引擎裡只剩 `Clone`（`PartialEq`／`Serialize` 等一概沒有），
要拿回 Zero-GC 世界用 `.clone_to_owned()`。完整例在 `examples/demo.aero`
的 `managed` 段。

---

## 16. 沒有的型別與對應寫法

| 想要 | Aero 寫法 |
|---|---|
| 元組 `(A, B)` | 小 struct（如 `Pair`），或 `Array`（同型時） |
| 定長陣列 `[T; N]` | `Array[T]` |
| `char` | 長度 1 的 `String` |
| 無號整數 | `Int`（`i64` 一統天下） |
| 切片、`&str` 型別 | 不存在此型別；`&String` 傳參時自動轉（第 9 章） |
| `Result[T, E]` | 不存在此型別；crate 回傳的 `Result` 以 `.unwrap()` 當場處理 |
| never 型別 | 不需要（ diverging 函式寫 `-> Unit` 即可） |

---

## 17. 型別標註何時可省、何時必須

可省（推導夠用）：多數 `let`（`let x = 3;` 即 `Int`）。
推導的設計目標很小——只求「夠決定 gc 借用與 clone」，其餘全交 rustc。

必須寫的三處：

1. `{}` 空字典：`let d: Dict[String, Int] = {};`
2. 泛型反推：`let q: Point = serde_json::from_str(&s).unwrap();`
   （`from_str` 的 `T` 靠標註決定，見 `examples/crates_json.aero`）。
3. 你希望 rustc 報錯位置更準時：標註是給檢查器的提示，不是負擔。
