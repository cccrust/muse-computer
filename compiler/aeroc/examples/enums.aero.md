# enums.aero 解說

`enum` + `match` + `Option[T]` 全家桶：帶值變體、`gc enum`、guard、or-pattern、字面值比對。

## 重點語法

- `enum Expr Num(Int), Add(Int, Int), Neg(Int), Zero end`：類 Rust 枚舉。
- `gc enum Tree Leaf(Int), Pair(gc Tree, gc Tree) end`：變體可遞迴持有 GC 指標，樹狀結構不斷鏈。
- `match v case ... end`：陳述式；支援 `|`（or-pattern）、`if` guard、`_` 通配、巢狀 pattern、字面值（`0`、`"bob"`）、`Some(x)/None`。
- 漏分支時 rustc E0004 會被 aeroc 轉為 `E0200` 指回 `.aero`。

## 逐段解說

1. `eval`：對 `Expr` 四種變體解構求值，`Add(2,3)`=5、`Neg(7)`=-7、`Zero`=0。
2. `describe`：展示字面值分支（`0`）、or-pattern（`1|2|3`）、guard（`x if x < 0`）、通配（`_`）。
3. `find`：線性搜尋回傳 `Option[Int]`，找到回 `Some(i)` 否則 `None`；`main` 用 `match` 解包兩次（命中印索引， miss 印 `"missing"`）。
4. 字串 `match`：`"bob"` 命中第二分支印 2。
5. `sum_tree` + `managed`：GC 樹 `Pair(Leaf(1), Leaf(2))` 遞迴求和得 3。

## 執行與預期輸出

```sh
aeroc run examples/enums.aero
# 5|-7|0|zero|small|negative|big|1|missing|2|3|
```

## 對應 Rust（`enums.rust`）

- `enum` 直譯為 Rust `enum`；`gc enum` 的遞迴欄位包 `Gc<GcCell<T>>`。
- `match` 直譯為 Rust `match`；guard/or-pattern 原樣保留。

## 完整註解程式碼（`enums.aero` 全文）

```aero
// enum / match / Option[T] / guard / or-pattern / 字串比對 / gc enum 全家桶。
// 執行：aeroc run examples/enums.aero（預期輸出見 enums.aero.md）

// 普通枚舉：四種變體——帶一個 Int、帶兩個 Int、一元負號、無負載。
// 漏分支時 rustc E0004 會被 aeroc 轉為 E0200 指回 .aero。
enum Expr
  Num(Int),
  Add(Int, Int),
  Neg(Int),
  Zero
end

// GC 枚舉：變體可遞迴持有 `gc Tree`，樹節點不斷鏈（tracing GC 回收）。
gc enum Tree
  Leaf(Int),
  Pair(gc Tree, gc Tree)
end

// 對 Expr 求值：`&Expr` 共享借用傳入，match 解構各變體。
// 一元負號寫成 `0 - x`。
fn eval(e: &Expr) -> Int
  match e
    case Expr::Num(n)
      return n;
    case Expr::Add(a, b)
      return a + b;
    case Expr::Neg(x)
      return 0 - x;
    case Expr::Zero
      return 0;
  end
end

// 四種 match 技巧：字面值分支、or-pattern、guard、通配。
fn describe(n: Int) -> String
  match n
    case 0
      return "zero";           // Int 字面值比對
    case 1 | 2 | 3
      return "small";          // `|`：任一命中即進此分支
    case x if x < 0
      return "negative";       // 綁定 x + guard 條件
    case _
      return "big";            // 通配：以上皆非
  end
end

// 線性搜尋：命中回 Some(索引)，找無回 None。
fn find(items: &Array[Int], target: Int) -> Option[Int]
  let mut i = 0;
  while i < items.len()
    if items[i] == target
      return Some(i);
    end
    i = i + 1;
  end
  return None;
end

// 遞迴求 GC 樹總和：Leaf 回值，Pair 分治相加。
fn sum_tree(t: gc Tree) -> Int
  match t
    case Tree::Leaf(n)
      return n;
    case Tree::Pair(l, r)
      return sum_tree(l) + sum_tree(r);
  end
end

fn main() -> Unit
  // eval 三例：Add(2,3)=5、Neg(7)=-7、Zero=0。
  let e1 = Expr::Add(2, 3);
  let e2 = Expr::Neg(7);
  let z = Expr::Zero;
  println(eval(&e1));          // 5
  println(eval(&e2));          // -7
  println(eval(&z));           // 0

  // describe 四例：zero / small / negative / big。
  println(describe(0));        // zero
  println(describe(2));        // small（or-pattern）
  println(describe(-4));       // negative（guard）
  println(describe(99));       // big（通配）

  // Option 解包：find(6) 命中索引 1；find(9) 落 None。
  let xs = [5, 6, 7];
  match find(&xs, 6)
    case Some(i) println(i);   // 1
    case None println("missing");
  end
  match find(&xs, 9)
    case Some(i) println(i);
    case None println("missing"); // missing
  end

  // 字串 match：命中 "bob" 印 2。
  let name = "bob";
  match name
    case "alice" println(1);
    case "bob" println(2);
    case _ println(3);
  end

  // GC 樹求和：Pair(Leaf(1), Leaf(2)) = 3。
  managed {
    let a: gc Tree = Tree::Leaf(1);
    let b: gc Tree = Tree::Leaf(2);
    let root: gc Tree = Tree::Pair(a, b);
    println(sum_tree(root));   // 3
  }
end
```
