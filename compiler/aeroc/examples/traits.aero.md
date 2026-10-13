# traits.aero 解說

trait 與自動特徵（Auto-Traits）正例：LLM 永遠不用手寫 `impl Trait for Type`，編譯器自動橋接＋自動 derive。

## 重點語法

- `trait Measurable fn area(&self) -> Int; end`：宣告命題（需求一組方法）。
- 固有 `impl Point fn area(...)` 寫齊後，編譯器自動產生 `impl Measurable for Point`。
- `fn sum[T: Add + Clone](items: Array[T]) -> T`：泛型邊界即命題；`Add + Clone` 並列是因泛型下索引讀取會 `.clone()`。
- `struct Pair[T: Clone] { ... } end`：泛型結構，具現寫 `Pair[Int]`（此處推導省略）。
- `fn describe[M: Measurable](m: &M)`：使用者 trait 也可作邊界。
- 內建特徵（`Clone/Equatable/Comparable/Hashable/Serializable/...`）由編譯器做結構化證明：欄位全可證就自動 derive。

## 逐段解說

1. `Point::new(3,4)`＋`area` 得 12；經 `describe(&p)`（泛型＋trait bound）同樣得 12。
2. `sum([1,2,3,4])` 具現 `T=Int`（Int 滿足 `Add+Clone`）得 10。
3. `Pair { first: 1, second: 2 }` 相加得 3。
4. `p == Point::new(3,4)` 得 `true`：`==` 可用是因為編譯器已自動為 `Point` 衍生 `PartialEq`（`Equatable` 證明）。

## 執行與預期輸出

```sh
aeroc run examples/traits.aero
# 12|12|10|3|true|
```

## 對應 Rust（`traits.rust`）

`aeroc emit` 可見三個證據：`pub trait Measurable`、`fn sum<T: Add + Clone>`、`pub struct Pair<T: Clone>`，另有 `PartialOrd` / `serde::Serialize` derive 與自動橋接的 `impl Measurable for Point`（見 `test.sh` 的產碼結構斷言）。

## 完整註解程式碼（`traits.aero` 全文）

```aero
// trait / 自動特徵正例：Curry-Howard 視角——trait 是「型態滿足特定命題的證明」。
// - LLM 永遠不用手寫 `impl Trait for Type`：固有方法齊了，編譯器自動橋接
// - 內建特徵（Clone/Comparable/Serializable/...）由編譯器結構化證明並自動 derive
// - 泛型約束 `fn f[T: Bound]` 即邏輯命題，驗證外包給 rustc（同 Borrow Checker 策略）
// 執行：aeroc run examples/traits.aero（預期輸出見 traits.aero.md）

// 命題規格：可度量（有面積）。任何提供 `area(&self) -> Int` 的型別即滿足此命題。
trait Measurable
  fn area(&self) -> Int;
end

struct Point { x: Int, y: Int } end

// 只寫固有方法即可——`impl Measurable for Point` 由編譯器自動橋接產生（手寫亦可，自動橋接會讓路）。
impl Point
  // 靜態建構：Point::new(3, 4)。
  fn new(x: Int, y: Int) -> Point
    return Point { x: x, y: y };
  end

  // 滿足 Measurable 命題的關鍵方法：面積（此處以 x*y 示意）。
  fn area(&self) -> Int
    return self.x * self.y;
  end
end

// 泛型求和：T 必須具備可加性命題（Add）+ Clone。
// 並列 Clone 的原因：泛型下以索引讀取元素會 `.clone()`（見 README），算術約束通常需並列 Clone。
fn sum[T: Add + Clone](items: Array[T]) -> T
  let mut total = items[0];
  let mut i = 1;
  while i < items.len()
    total = total + items[i];
    i = i + 1;
  end
  return total;
end

// 泛型結構：形參邊界即命題（T 須 Clone 才能整體 Clone/比較/序列化）。
struct Pair[T: Clone] {
  first: T,
  second: T
}
end

// 使用者 trait 也可作泛型邊界：M 滿足 Measurable 命題即可呼叫 m.area()。
fn describe[M: Measurable](m: &M) -> Int
  return m.area();
end

fn main() -> Unit
  let p = Point::new(3, 4);
  println(p.area());           // 3 * 4 = 12（直接調固有方法）
  println(describe(&p));       // 12（經泛型 + Measurable 邊界調用）

  // sum 具現 T=Int（Int 滿足 Add+Clone）：1+2+3+4 = 10。
  let xs = [1, 2, 3, 4];
  println(sum(xs));

  // Pair 具現 T=Int：1 + 2 = 3。
  let pair = Pair { first: 1, second: 2 };
  println(pair.first + pair.second);

  // 自動衍生：Point 的欄位全是 Int，編譯器已自動證明
  // Equatable/Comparable/Hashable/Serializable 並 derive，對應 PartialEq/serde 等。
  // 故 `==` 可直接用（PartialEq 已自動衍生）。
  println(p == Point::new(3, 4)); // true
end
```
