# syntax2.aero 解說

新規格外語法合集：`if / else if / else`、`&&`/`||`、一元 `-`/`!`、陣列與字典索引。

## 重點語法

- `if c ... else if c ... else ... end`（以 `end` 結尾，無大括號）。
- `&&`、`||`、`!`、一元 `-`（寫成 `-3` 或 `0 - x` 皆可）。
- `a[i]`：陣列讀寫皆可，`ys[0] = 99` 即原地修改。
- `{ "a": 1, "b": 2 }`：字典字面值，`d["b"]` 取值。
- `Box { items: [...] }` 欄位再索引：`b.items[2]`。
- GC 物件欄位內的陣列亦可索引賦值：`bag.nums[i] = ...`。

## 逐段解說

1. `classify`：三分支 `if/else if/else`，分別回傳 `"negative"/"zero"/"positive"`。
2. `xs[1] + xs[2]`：索引讀取（20+30=50）。
3. `ys[0] = 99; ys[1] = ys[1] * 5`：索引寫入，99+10=109。
4. `!(...) && ...`：布林組合，成立則印 `and-ok`；`false || (1 > 2)` 走 `else` 印 `or-else`。
5. `d["b"] - -3`：字典取值 2 減 -3 得 5。
6. `b.items[2]`：結構欄位＋索引，得 7。
7. `managed` 段：`gc Bag` 內的 `nums` 陣列做 `bag.nums[1] + bag.nums[2]`（=5）與 `while` 迴圈逐元 `*2`，總和 2+8+6=18。

## 執行與預期輸出

```sh
aeroc run examples/syntax2.aero
# negative|zero|positive|50|109|and-ok|or-else|5|7|5|18|
```

## 對應 Rust（`syntax2.rust`）

- `if/else` 直譯為 Rust `if/else`；`&&`/`||`/`!` 直通。
- 索引讀寫直譯為 `v[i]`；GC 下自動展開為 `bag.borrow().nums[i]` / `borrow_mut()` 寫入。
- 字典字面值譯為 `HashMap` 構造。

## 完整註解程式碼（`syntax2.aero` 全文）

```aero
// 新語法合集：if / else if / else、&& / ||、一元 - 與 !、陣列索引 a[i]、字典索引。
// 執行：aeroc run examples/syntax2.aero（預期輸出見 syntax2.aero.md）

// Zero-GC 結構：mut 欄位允許 `ys[0] = ...` 這類原地修改。
struct Box { mut items: Array[Int] } end
// GC 結構：託管陣列欄位，索引讀寫會自動加 borrow / borrow_mut。
gc struct Bag { mut nums: Array[Int] } end

// 三分支條件：`if ... else if ... else ... end`（無大括號，以 end 結尾）。
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
  // if/else-if/else 三種回傳。
  println(classify(-5));       // negative（一元 - 字面值）
  println(classify(0));        // zero
  println(classify(7));        // positive

  // 陣列索引讀取：xs[1] + xs[2] = 20 + 30 = 50。
  let xs = [10, 20, 30];
  println(xs[1] + xs[2]);

  // 陣列索引寫入：需 `mut` 綁定；ys 變 [99, 10, 3]，99 + 10 = 109。
  let mut ys = [1, 2, 3];
  ys[0] = 99;
  ys[1] = ys[1] * 5;
  println(ys[0] + ys[1]);

  // `!` 取反 + `&&` 且：!(10 > 100) 為真且 30 == 30，故 ok 為真。
  let ok = !(xs[0] > 100) && xs[2] == 30;
  // 單分支 if（無 else）：成立才執行。
  if ok
    println("and-ok");
  end
  // `||` 或 + if/else：false || false 為假，走 else 印 or-else。
  if false || (1 > 2)
    println("never");
  else
    println("or-else");
  end

  // 字典字面值與字典索引：d["b"] = 2，`- -3` 為減負三，得 2 - (-3) = 5。
  let d = { "a": 1, "b": 2 };
  println(d["b"] - -3);

  // 結構欄位再索引：Box.items[2] = 7。
  let b = Box { items: [5, 6, 7] };
  println(b.items[2]);

  // GC 物件內的陣列索引讀寫（自動 borrow_mut）。
  managed {
    let bag = gc Bag { nums: [1, 2, 3] };
    // nums[1] = 2 + 3 = 5。
    bag.nums[1] = bag.nums[1] + bag.nums[2];
    println(bag.nums[1]);      // 5
    // while 逐元翻倍：[1,5,3] -> [2,10,6]。
    let mut i = 0;
    while i < 3
      bag.nums[i] = bag.nums[i] * 2;
      i = i + 1;
    end
    // 2 + 10 + 6 = 18。
    println(bag.nums[0] + bag.nums[1] + bag.nums[2]);
  }
end
```
