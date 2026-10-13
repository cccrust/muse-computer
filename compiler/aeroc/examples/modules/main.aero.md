# modules/main.aero 解說

多檔案模組入口：別名、巢狀目錄、同名函式共存、循環引用的總裝。

## 重點語法

- `import "geometry.aero" as geo;`：別名引用，以 `geo::Point`、`geo::area` 存取。
- `import "util/mathx.aero";`：省略 `as` 則以檔名（`mathx`）為別名；路徑相對於 import 它的檔案。
- `import "ping.aero";`：無別名即直接以 `ping::...` 存取（`ping::is_even`）。
- 同一檔案只載入一次；允許循環引用（`ping`↔`pong`）。

## 逐段解說

1. `geo::Point::new(0,0)`、`new(3,4)` 求平方距離 `dist2` = 9+16 = 25。
2. `mathx::square(9)` = 81。
3. 建 `[Circle(2), Rect(3,4), Dot]` 陣列，`for s in &shapes` 逐一 `geo::area`：3·4=12、3·4=12、0。
4. 三個同名 `helper` 共存：`geo::helper()`=1、`helper()`（本檔）=2、`mathx::helper()`=100，相加 103。實作是各模組符號加前綴（`geometry__helper` 等）後合併，根模組不加前綴。
5. `ping::is_even(10)`=`true`、`ping::is_even(7)`=`false`：經 `pong::is_odd` 互遞迴（偶數次遞迴回到 `is_even(0)`）。

## 執行與預期輸出

```sh
aeroc run examples/modules/main.aero   # 自動載入 import 的模組
# 25|81|12|12|0|103|true|false|
```

## 完整註解程式碼（`main.aero` 全文）

```aero
// 多檔案模組入口：別名、巢狀目錄、同名函式共存、循環引用的總裝。
// 執行：aeroc run examples/modules/main.aero（自動載入 import 的模組，預期輸出見 .md）。

// 別名引用 geometry.aero：之後以 geo::Point、geo::area 等存取。
// 路徑相對於 import 它的檔案；同一檔案只載入一次。
import "geometry.aero" as geo;
// 省略 as 則以檔名為別名：util/mathx.aero 以 mathx:: 存取（巢狀目錄亦可）。
import "util/mathx.aero";
// 無別名即以自身檔名存取：ping.aero 以 ping:: 存取。
import "ping.aero";

// 本檔的 helper（回 2）：與 geo::helper（1）、mathx::helper（100）同名共存。
// 實作是各模組符號加前綴（geometry__helper 等）後合併成單一 crate，根模組不加前綴。
fn helper() -> Int
  return 2;
end

fn main() -> Unit
  // 跨模組結構 + 方法：兩點 (0,0)、(3,4)，平方距離 9+16 = 25。
  let a = geo::Point::new(0, 0);
  let b = geo::Point::new(3, 4);
  println(a.dist2(&b));        // 25（dist2 內部又調 mx::square）
  // 巢狀模組函式：9² = 81。
  println(mathx::square(9));

  // 跨模組 enum：建三圖形，逐一求 area——Circle(2)=3·4=12、Rect(3,4)=12、Dot=0。
  let shapes = [geo::Shape::Circle(2), geo::Shape::Rect(3, 4), geo::Shape::Dot];
  for s in &shapes
    println(geo::area(s));
  end

  // 同名函式共存驗證：1 + 2 + 100 = 103。
  println(geo::helper() + helper() + mathx::helper());
  // 循環引用驗證：ping ↔ pong 互遞迴判奇偶（偶數次遞迴回到 is_even(0)）。
  println(ping::is_even(10));  // true
  println(ping::is_even(7));   // false
end
```
