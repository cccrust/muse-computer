# demo.aero 解說

綜合展示範例：Zero-GC 值語義、管線 `|>`、lambda、`managed`/`gc` 循環引用、FFI、`unsafe`。

## 重點語法

- `extern "C" { fn labs(x: Int) -> Int; } end`：宣告 C FFI（對應 libc `labs`）。
- `struct Point { ... } end`：Zero-GC 結構（stack / owned value）。
- `gc struct Node { mut value: Int, mut links: Array[gc Node] } end`：GC 託管結構，可形成循環引用。
- `3 |> add(4)`：管線，等價 `add(3, 4)`；可鏈接 `5 |> double |> double`。
- `|x: Int| -> Int x + 1 end`：lambda。
- `managed { ... }`：進入 GC 作用域；`gc Node { ... }` 在其中配置。
- `unsafe { ... } end`：不安全塊，呼叫 FFI 必須包在裡面。

## 逐段解說

1. `make_point / add / double / sum`：純 Zero-GC 函式。`sum(items: &Array[Int])` 以 `&` 借用陣列、`for i in items` 迭代。
2. `bump(n: &mut Node)` / `show(n: &Node)`：可變借用 vs 共享借用；GC 物件一樣可用 `&mut`/`&`。
3. `main` 前半（Zero-GC）：建點並求和、`sum(&xs)`、`|>` 管線、lambda `inc(41)` 得 42、字串 `+` 串接、`while` 計數到 3。
4. `main` 中段（Managed-GC）：`a`、`b` 互指（`a.links.push(b); b.links.push(a)`）—— Rust 借用檢查器下寫不出來，靠 `gc` crate 的 cycle-collecting GC 才安全。`alias = a` 是多重可變共享（同一 `GcCell` 的第二個把手），改 `alias.value` 即改 `a.value`。`clone_to_owned()` 把 GC 物件拷貝回 Zero-GC 值。離開 `managed` 後手動 `std::gc::collect()`。
5. `main` 末段（FFI）：`unsafe` 內呼叫 `labs(0 - 5)`（注意一元 `-` 寫成 `0 - 5`），得 5。

## 執行與預期輸出

```sh
aeroc run examples/demo.aero
# 7|10|7|20|42|hello, aero|3|111|112|1|112|5|
```

逐行：`p.x+p.y`=7、`sum`=10、管線 7、雙重 double 20、lambda 42、字串、`while` 結果 3、GC `a.value`=111、`bump` 後 112、`links.len()`=1、拷貝值 112、FFI 5。

## 對應 Rust（`demo.rust`）

- `Point` 帶整套 `Clone/PartialEq/serde` derive；`Node` 變成含 `Gc<GcCell<Node>>` 向量的結構。
- 管線 `3 |> add(4)` 直接內聯成 `add(3, 4)`；lambda 變成 `Rc::new(move |x: i64| ...)`。
- GC 段的 `.borrow()` / `.borrow_mut()` 读写、`force_collect()` 皆由編譯器自動插入。

## 完整註解程式碼（`demo.aero` 全文）

```aero
// Aero 綜合展示（demo）：一次看懂 Zero-GC、管線、lambda、GC 循環引用、FFI、unsafe。
// 執行：aeroc run examples/demo.aero（預期輸出見 demo.aero.md）

// FFI 宣告：引入 C 函式 labs（整數取絕對值）。
// extern "C" 區塊直通 Rust 的 extern "C"；呼叫時必須包在 unsafe 內。
extern "C" { fn labs(x: Int) -> Int; } end

// Zero-GC 結構：在 stack / owned value 上分配，無 GC 開銷，離開作用域即釋放。
struct Point { x: Int, y: Int } end

// GC 託管結構：以 `gc` 標記，底層為 Gc<GcCell<T>>。
// mut 欄位可在共享把手下修改；Array[gc Node] 讓節點互指形成循環引用（Rust 借用檢查器寫不出來，靠 cycle-collecting GC 回收）。
gc struct Node { mut value: Int, mut links: Array[gc Node] } end

// 建構 Point 的普通函式（Zero-GC 值以 move 語義回傳）。
fn make_point(x: Int, y: Int) -> Point
  return Point { x: x, y: y };
end

// 供管線演示的雙參數函式：`3 |> add(4)` 等價於 `add(3, 4)`（左值填入首參）。
fn add(a: Int, b: Int) -> Int
  return a + b;
end

// 供管線鏈接演示：`5 |> double |> double` 等價於 `double(double(5))`。
fn double(n: Int) -> Int
  return n * 2;
end

// 以共享借用遍歷陣列求和：`&Array[Int]` 只借不拿所有權，`for i in items` 逐元迭代。
fn sum(items: &Array[Int]) -> Int
  let mut total = 0;
  for i in items
    total = total + i;
  end
  return total;
end

// 以可變借用修改 GC 節點：`&mut Node` 保證同一時間唯一可寫。
fn bump(n: &mut Node) -> Unit
  n.value = n.value + 1;
end

// 以共享借用讀取 GC 節點並列印。
fn show(n: &Node) -> Unit
  println(n.value);
end

fn main() -> Unit
  // --- Zero-GC 區：全是 owned value / 借用，無 GC 參與 ---
  let p = make_point(3, 4);
  println(p.x + p.y);          // 3 + 4 = 7
  let xs = [1, 2, 3, 4];       // 陣列字面值
  println(sum(&xs));           // 借用傳入，得 10
  let r: Int = 3 |> add(4);    // 管線：add(3, 4) = 7
  println(r);
  println(5 |> double |> double); // double(double(5)) = 20
  // lambda：`|參數| -> 回傳型 函式體 end`，此處 x+1。
  let inc = |x: Int| -> Int x + 1 end;
  println(inc(41));            // 42
  // 字串 `+` 為串接（owned String 相加）。
  let greeting = "hello" + ", " + "aero";
  println(greeting);           // hello, aero
  // while 迴圈：無括號，以 end 結尾。
  let mut i = 0;
  while i < 3
    i = i + 1;
  end
  println(i);                  // 3

  // --- Managed-GC 區：雙向循環引用 + 多重可變共享 ---
  managed {
    // gc 配置兩個節點（Gc::new(GcCell::new(...)) 由編譯器產生）。
    let a = gc Node { value: 1, links: [] };
    let b = gc Node { value: 2, links: [] };
    a.links.push(b);           // a -> b
    b.links.push(a);           // b -> a：循環！只有 tracing GC 能回收
    a.value = a.value + 10;    // 1 + 10 = 11（自動展開為 borrow_mut 寫入）
    let alias = a;             // 第二個把手（Gc::clone），指向同一 GcCell
    alias.value = alias.value + 100; // 經 alias 改即改 a，得 111
    println(a.value);          // 111
    bump(&mut a);              // 可變借用傳入，得 112
    show(&a);                  // 共享借用讀取，印 112
    println(a.links.len());    // 鄰接數 1
    // 把 GC 物件拷貝回 Zero-GC 值（owned clone），之後與 GC 無關。
    let copy: Node = a.clone_to_owned();
    println(copy.value);       // 112
  }
  // 離開 managed 作用域後手動觸發一次 GC（force_collect）。
  std::gc::collect();

  // --- FFI + unsafe 區：呼叫 C 的 labs ---
  unsafe {
    // 一元負號寫成 `0 - 5`；labs(-5) = 5。
    let v = labs(0 - 5);
    println(v);                // 5
  } end
end
```
