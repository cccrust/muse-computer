# modules/geometry.aero 解說

被 `main.aero` 以 `as geo` 引入的幾何模組：結構＋`impl`＋enum＋跨模組呼叫。

## 內容

- `import "util/mathx.aero" as mx;`：模組可再 import（路徑相對於本檔，故為 `util/` 下）。
- `struct Point`＋`impl Point`：`new` 建構、`dist2(&self, other: &Point)` 以 `mx::square` 求平方距離。
- `enum Shape Circle(Int), Rect(Int, Int), Dot end`＋`fn area(s: &Shape)`：`match` 三分支，圓面積以 3·r² 近似。
- `fn helper() -> Int` 回 1：與 `main`/`mathx` 的同名函式共存，合併時加前綴區分。

## 被誰引用

`main.aero` 以 `geo::Point::new`、`geo::Point::dist2`、`geo::Shape::Circle`、`geo::area`、`geo::helper` 存取。

## 完整註解程式碼（`geometry.aero` 全文）

```aero
// 幾何模組：被 main.aero 以 `as geo` 引入（geo::Point、geo::area …）。
// 展示重點：模組內再 import、impl 方法、enum + match、同名函式共存。

// 模組可再 import：路徑相對於本檔，故為 util/ 下的 mathx.aero；別名 mx。
import "util/mathx.aero" as mx;

// Zero-GC 點結構。
struct Point { x: Int, y: Int } end

impl Point
  // 靜態建構：geo::Point::new(x, y)。
  fn new(x: Int, y: Int) -> Point
    return Point { x: x, y: y };
  end

  // 平方距離：dx²+dy²（調用 mx::square，示範跨模組函式呼叫）。
  fn dist2(&self, other: &Point) -> Int
    let dx = mx::square(self.x - other.x);
    let dy = mx::square(self.y - other.y);
    return dx + dy;
  end
end

// 圖形枚舉：圓（半徑）、矩形（寬高）、點（無負載）。
enum Shape
  Circle(Int),
  Rect(Int, Int),
  Dot
end

// 以 match 求面積：圓以 3·r² 近似（同樣調 mx::square）。
fn area(s: &Shape) -> Int
  match s
    case Shape::Circle(r)
      return 3 * mx::square(r);
    case Shape::Rect(w, h)
      return w * h;
    case Shape::Dot
      return 0;
  end
end

// 本模組的 helper（回 1）：三個同名 helper 之一，合併時加 geometry__ 前綴區分。
fn helper() -> Int
  return 1;
end
```
