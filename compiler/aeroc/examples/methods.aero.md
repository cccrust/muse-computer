# methods.aero 解說

`impl` 區塊入門：靜態建構函式、`&self` / `&mut self` 方法、管線呼叫方法、GC 物件上的方法。

## 重點語法

- `impl Counter ... end`：為結構附加方法。
- `fn new(start: Int) -> Counter`：無 `self` 即靜態函式，以 `Counter::new(10)` 呼叫。
- `fn inc(&mut self)`：可變方法；`fn get(&self)`：唯讀方法。
- `c |> get`：管線可接方法，等價 `c.get()`。
- `gc struct Account` 配 `impl Account`：GC 物件的方法呼叫會自動加 `borrow()/borrow_mut()`。

## 逐段解說

1. `Counter`：`new(10)` 起始，兩次 `inc()` 得 12，`add(5)` 得 17，管線 `c |> get` 再讀一次 17。
2. `Account`（GC）：`acc` 與 `alias` 指向同一 `GcCell`，兩次 `deposit(100)/(50)` 都累加到同一餘額 150；`history.len()`=2 證明共享。

## 執行與預期輸出

```sh
aeroc run examples/methods.aero
# 12|17|17|150|2|
```

## 對應 Rust（`methods.rust`）

- `impl` 直譯為 Rust `impl`；`&mut self`/`&self` 保留。
- GC 方法呼叫點展開為 `acc.borrow_mut().deposit(...)` 形式；`alias = acc` 譯為 `Gc::clone`。

## 完整註解程式碼（`methods.aero` 全文）

```aero
// impl 區塊入門：靜態建構函式、&self / &mut self 方法、管線調方法、GC 物件上的方法。
// 執行：aeroc run examples/methods.aero（預期輸出見 methods.aero.md）

// Zero-GC 計數器：mut 欄位允許方法內修改 self.n。
struct Counter { mut n: Int } end

impl Counter
  // 靜態函式（無 self）：以 `Counter::new(10)` 呼叫，等價 Rust 的關聯函式。
  fn new(start: Int) -> Counter
    return Counter { n: start };
  end

  // 可變方法：`&mut self` 才能寫 self.n；回傳 Unit（過程）。
  fn inc(&mut self) -> Unit
    self.n = self.n + 1;
  end

  // 唯讀方法：`&self` 只讀不寫，回傳當前計數。
  fn get(&self) -> Int
    return self.n;
  end

  // 可變方法兼回傳值：累加 k 後把新值一併回傳。
  fn add(&mut self, k: Int) -> Int
    self.n = self.n + k;
    return self.n;
  end
end

// GC 託管帳戶：餘額 + 交易紀錄；多把手共享同一 GcCell。
gc struct Account { mut balance: Int, mut history: Array[Int] } end

impl Account
  // 可變方法：餘額累加並把金額推入 history（GC 下自動 borrow_mut）。
  fn deposit(&mut self, amt: Int) -> Unit
    self.balance = self.balance + amt;
    self.history.push(amt);
  end

  // 唯讀方法：查詢餘額（自動 borrow）。
  fn total(&self) -> Int
    return self.balance;
  end
end

fn main() -> Unit
  // 靜態建構 + 可變呼叫：new(10) -> inc -> inc 得 12。
  let mut c = Counter::new(10);
  c.inc();
  c.inc();
  println(c.get());            // 12
  println(c.add(5));           // 12 + 5 = 17
  // 管線調方法：`c |> get` 等價 `c.get()`，得 17。
  println(c |> get);

  // GC 物件的方法呼叫：acc 與 alias 指向同一帳戶。
  managed {
    let acc = gc Account { balance: 0, history: [] };
    let alias = acc;           // Gc::clone：第二把手
    acc.deposit(100);          // 餘額 100，history [100]
    alias.deposit(50);         // 同一帳戶再存 50，餘額 150，history [100, 50]
    println(acc.total());      // 150
    println(alias.history.len()); // 交易筆數 2
  }
end
```
