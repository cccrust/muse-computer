# Aero 語法（Grammar）

本文件是 Aero 語法的規範性描述，與 `src/parser.rs`（chumsky 組合子，scannerless）
逐條對應。若範例與實作衝突，以實作為準——歡迎拿 `aeroc emit` 驗證
（產碼能過 `syn` 解析即語法成立）。

EBNF 記法：`"lit"` 字面關鍵字、`[...]` 可選、`(...)*` 零或多次、`(...)+`
一或多次、`|` 選擇。**空白與 `//` 行註解可出現在任何兩個記號之間**
（parser 是 scannerless，每個記號前後自動吃空白）。

相關文件：教學見 `_doc/tutorial.md`；轉譯與錯誤碼見 `README.md`。

---

## 0. 詞法：保留字、識別字、字面值

保留字共 31 個，不可當識別字：

```
import  crate  extern  struct  gc  fn  async  await  let  mut  managed
unsafe  for  in  while  return  move  end  true  false  if  else  impl
as  match  case  enum  trait  for
```

注意：

- 型別名（`Int`、`String`、`Array`…）**不是**保留字，只是上下文關鍵字；
  `self`／`Self` 也不是（`self` 只在接收者位置有意義，`Self` 是普通名字，
  在 trait 簽章裡指「實作此 trait 的型別」）。
- 唯一以 `gc` 開頭仍合法的路徑是 `gc::collect`（直通 `::gc::force_collect`）。
- 識別字：字母或底線開頭，後接字母、數字、底線（chumsky `ident` 規則再扣掉保留字）。

字面值：

```
int_lit    := digits                      // 十進位，須在 i64 範圍內，否則解析錯
float_lit  := digits "." digits            // 無指數、無前導點：寫 0.5 不寫 .5
string_lit := '"' (任意字元 | '\\' | '\"' | '\n' | '\t' | '\r')* '"'
bool_lit   := "true" | "false"
```

`=` 與 `==` 由解析器明確區分：賦值只認「單獨的 `=`」（後面不緊跟另一個 `=`），
所以 `a = b`、`a == b` 永遠不會混淆。

---

## 1. 型別

```ebnf
type      := "gc"? type_kind
type_kind := "Int" | "Float" | "String" | "Bool" | "Unit" | "RawPointer"
           | "Array" "[" type "]"
           | "Dict" "[" type "," type "]"
           | "Option" "[" type "]"
           | "Future" "[" type "]"
           | "&" "mut"? type
           | "async"? "fn" "(" [type ("," type)*] ")" "->" type   // 閉包型別
           | path ["[" type ("," type)* "]"]                     // 自訂型別＋泛型具現
path      := Ident ("::" Ident)*
```

例：`Int`、`&mut Array[gc Node]`、`Option[Result]`（不存在的組合也能寫，
錯留給 rustc）、`Pair[Int, String]`、`async fn (Int) -> String`。

規則：

- `gc T` 表示「GC 堆上的 T」，轉譯為 `Gc<GcCell<…>>`。
- `Array`／`Dict`／`Option`／`Future` 優先於通用 `path[…]` 匹配，
  所以 `Array[Int]` 永遠是 Vec，不會被當成泛型具現。
- `path[…]` 無參數時就是普通自訂型別（`Point`）；有參數時是泛型具現
  （`Pair[Int]`）。單個大寫名字如 `T` 在泛型作用域內會被當成形參，
  見第 6 章。

---

## 2. 樣式（match 的 case 用）

```ebnf
pattern  := atom ("|" atom)*                 // `|` 是或樣式
atom     := "true" | "false" | string_lit | ["-"] digits
          | "_"                              // 萬用
          | "None"                           // 僅限單段無參數時
          | Ident                            // 單段無參數＝綁定（`case x`）
          | path ["(" [pattern ("," pattern)*] ")"]   // 變體或嵌套
```

例：`_`、`1 | 2 | 3`、`Some(x)`、`Shape::Circle(r)`、`Tree::Pair(l, r)`、
`"bob"`、`-4`。注意：

- `case x` 的 `x` 是**綁定**（不是比較），比較字串請加引號。
- 變體參數內可再嵌套樣式。
- 結構體樣式（`Point { x }`）**不支援**，要拆欄位請先 `let` 出來。
- float 字面值樣式不支援。

---

## 3. 表達式（優先序由低到高）

```
|>              管線（左結合，可連串）
||              邏輯或
&&              邏輯與
==  !=          相等（可連寫 a == b == c，左結合）
<  <=  >  >=    比較
+  -            加減（字串 + 字串是拼接）
*  /  %         乘除餘
-  !            一元（右結合，可疊加，如 !!x）
.await  .field  (args)  [idx]    後綴（左結合）
primary         見下
```

### primary（按解析器嘗試順序）

```ebnf
primary := "move" Ident                        // 轉移所有權
         | "&" "mut"? Ident                    // 只借具名變數，不能借字面值
         | "gc" path "{" inits "}"             // GC 配置
         | literal
         | lambda
         | path "{" inits "}"                  // 結構體初始化
         | path                                // 變數或路徑（含 axum::Router::new 這類）
         | "[" [expr ("," expr)* [","]] "]"    // 陣列字面值
         | "{" [expr ":" expr ("," expr ":" expr)* [","]] "}"  // 字典；{} 是空字典
         | "(" expr ")"
inits   := Ident ":" expr ("," Ident ":" expr)* [","]
lambda  := "async"? "|" [param ("," param)*] "|" "->" type
           (expr "end" | stmt* "end")          // 短體：|x: Int| -> Int x + 1 end
```

注意：

- `Foo { … }` 永遠是結構體初始化（有路徑前綴才算），裸 `{…}` 永遠是字典。
  `{}` 必須加標註才編譯得過：`let d: Dict[String, Int] = {};`。
- 後綴可混串：`a.b().await`、`f(x)[0]`、`bind(..).await.unwrap()` 都合法。
- 管線：`x |> f(a)` 即 `f(x, a)`（f 為已知函式時），否則是方法呼叫
  `x.f(a)`；無參數 `x |> f` 即 `f(x)`。見 `examples/demo.aero` 的用法。

---

## 4. 陳述式（每句以 `;` 結尾，區塊以 `end` 閉合——除註明外）

```ebnf
stmt := "let" "mut"? Ident [":" type] "=" expr ";"
      | "managed" "{" stmt* "}"                    // 注意：無 end
      | "unsafe" "{" stmt* "}" "end"
      | "for" Ident "in" expr stmt* "end"
      | "while" expr stmt* "end"
      | "if" expr stmt* ("else" "if" expr stmt*)* ["else" stmt*] "end"
      | "match" expr ("case" pattern ["if" expr] stmt*)+ "end"   // 至少一個 case
      | "return" [expr] ";"
      | expr ["=" expr] ";"                        // 無 = 是求值句，有 = 是賦值
```

賦值左側只能是變數、欄位、索引（`a`、`a.b`、`a[i]`），否則是自訂解析錯
「賦值左側必須是變數或欄位」。`managed` 用花括號**不用** `end`，
`unsafe`、`if`、`for`、`while`、`match` 都要 `end`——這是不對稱的，
寫錯即 `E0001`。

---

## 5. 頂層宣告

```ebnf
program := top* EOF
top     := "#[" path "]"? decl     // attribute，如 #[inline]（僅無參數屬性可靠）
decl    := import_crate | import_mod | extern_block | struct_decl
         | enum_decl | trait_decl | impl_decl | function
```

```ebnf
import_crate := "import" "crate" string
                ["version" "=" string]
                ["," "features" "=" "[" [string ("," string)* [","]] "]"] ";"
import_mod   := "import" string ["as" Ident] ";"     // 省略 as 則以檔名為別名

extern_block := "extern" ("crate" Ident | string)? "{" extern_fn* "}" "end"
extern_fn    := "async"? "fn" Ident "(" [param ("," param)*] ")" "->" type ";"

struct_decl  := "gc"? "struct" Ident generics?
                "{" [field ("," field)* [","]] "}" "end"
field        := "mut"? Ident ":" type

enum_decl    := "gc"? "enum" Ident generics?
                variant ("," variant)* [","] "end"
variant      := Ident ["(" [type ("," type)*] ")"]

trait_decl   := "trait" Ident trait_method* "end"    // 方法只有簽章
trait_method := "fn" Ident generics?
                "(" [self_param ","]? [param ("," param)*] ")" "->" type ";"

impl_decl    := "impl" Ident ["for" Ident] function* "end"
               // 無 for＝固有方法；有 for＝手動 trait 實作（逃生艙）

function     := "async"? "fn" Ident generics?
                "(" [self_param ","]? [param ("," param)*] ")"
                "->" type stmt* "end"

self_param   := "&" "mut"? "self" | "mut"? "self"    // 頂層 fn 禁止 self
param        := "mut"? Ident ":" type
```

注意：

- `extern crate libc` 的 ABI 視為 `"C"`；`extern { … }` 預設也是 `"C"`。
  extern 函式呼叫會自動包 `unsafe`。
- `import "x.aero"` 路徑相對於引入檔、可省略副檔名、同檔只載入一次、
  允許循環引用；各模組符號加前綴後合併（見 `examples/modules/`）。
- `trait` 本體**不能**帶泛型參數（只有它的方法可以，且目前方法級泛型會被忽略——見第 7 章限制）。
- `impl A for B` 的 A、B 目前只接受單個識別字（泛型具現不行）。

---

## 6. 泛型與邊界

```ebnf
generics       := "[" generic_param ("," generic_param)* "]"
generic_param  := Ident [":" path ("+" path)*]      // 如 T: Add + Clone
```

出現在 `fn` 名、`struct` 名、`enum` 名、trait 方法名之後：

```aero
fn sum[T: Add + Clone](items: Array[T]) -> T ... end
struct Pair[T: Clone] { first: T, second: T } end
```

邊界名是內建特徵（`Clone`、`Equatable`、`Comparable`、`Hashable`、
`Serializable`、`Add`／`Sub`／`Mul`／`Div`／`Rem`、`Display`、`Debug`、
`Default`、`Iterator`）則映射到 Rust trait，否則視為使用者 trait
（須先 `trait` 宣告，跨模組自動加前綴解析）。驗證外包給 rustc：
不滿足即 `E0301`。

---

## 7. 已知限制（寫得出、但不如預期的）

1. trait 方法的自有泛型（`fn f[T: …](…)`) 會被**靜默忽略**。
2. `impl Trait for Pair[Int]` 不支援——`for` 後只能是裸型別名。
3. `match` 只是陳述式，不能當表達式回傳值（用 `return` 在分支裡返回）。
4. `#[attr]` 只對無參數屬性可靠（如 `#[inline]`）；`#[allow(dead_code)]`
   類含參屬性會產出錯誤 Rust。
5. `{}` 空字典須加型別標註，否則 rustc 推導失敗。
6. 字串字面值永遠是 owned `String`；要 `&str` 須綁變數再借用
   （`let p = "/"; f(&p)`），`_doc/tutorial.md` 第 6 章有例。
7. 非 `Copy` 欄位不能從 `&` 借用中搬走，用 `.clone_to_owned()`。

---

## 附：最小完整程式（貼上即跑）

```aero
fn main() -> Unit
  println("grammar ok");
end
```

```bash
./target/release/aeroc run grammar_ok.aero   # 印出 grammar ok
```
