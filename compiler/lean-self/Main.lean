/-!
# MiniLang → 堆疊式 IR 編譯器（stage-0，用 Lean 4 寫成）

這是「自舉（self-hosting）」的第 0 階段：

  stage0 : Lean 寫的編譯器       編譯 compiler.mini        → IR₀
  stage1 : IR₀ 在 VM 上執行       編譯 compiler.mini        → IR₁
  stage2 : IR₁ 在 VM 上執行       編譯 compiler.mini        → IR₂

  自舉成功 ⇔ IR₀ = IR₁ = IR₂（位元組完全相同，達到不動點）

`compiler.mini` 是用 MiniLang 自己寫的 MiniLang 編譯器。

## MiniLang v2

  fn f(a, b) { ... return e; }          函式（可遞迴、可前向參照）
  array buf[100];                       全域陣列（連續記憶體格）
  let x = e;  x = e;  buf[i] = e;       全域變數 / 區域變數 / 陣列存取
  print e;   getc()   putc(c)   halt()  輸出整數、讀/寫字元、停機
  if / else if / else,  while,  return
  'a' '\n'   "字串"                     字元常數、字串常數（回傳位址，以 0 結尾）
  + - * / %  == != < <= > >=  && ||  ! -
  p[i]                                  p 若是區域變數 / 參數，視為位址（指標）

## IR（堆疊機）  每條指令 = (opcode, operand)

   0 push n    1 load k     2 store k    3 mload      4 mstore
   5 bin op    6 neg        7 not        8 print      9 jmp t
  10 jz t     11 call t    12 enter n   13 ret       14 pop
  15 getc     16 putc      17 halt

IR 文字格式 = 一串整數（每行一個）：  記憶體大小, op₀, arg₀, op₁, arg₁, ...
-/

namespace Mini

/-! ## 1. 詞法分析 -/

inductive Tok where
  | num (n : Nat)
  | ident (s : String)
  | str (s : String)
  | sym (s : String)
  | eof
  deriving BEq, Repr, Inhabited

def Tok.toStr : Tok → String
  | .num n   => toString n
  | .ident s => s
  | .str s   => "\"" ++ s ++ "\""
  | .sym s   => s
  | .eof     => "<eof>"

def isIdChar (c : Char) : Bool := c.isAlphanum || c == '_'

def escChar (e : Char) : Nat :=
  if e == 'n' then 10 else if e == 't' then 9 else e.toNat

partial def lexLoop (cs : List Char) (acc : Array Tok) : Except String (Array Tok) :=
  match cs with
  | [] => pure (acc.push .eof)
  | '/' :: '/' :: r => lexLoop (r.dropWhile (· != '\n')) acc
  | '=' :: '=' :: r => lexLoop r (acc.push (.sym "=="))
  | '!' :: '=' :: r => lexLoop r (acc.push (.sym "!="))
  | '<' :: '=' :: r => lexLoop r (acc.push (.sym "<="))
  | '>' :: '=' :: r => lexLoop r (acc.push (.sym ">="))
  | '&' :: '&' :: r => lexLoop r (acc.push (.sym "&&"))
  | '|' :: '|' :: r => lexLoop r (acc.push (.sym "||"))
  | '\'' :: '\\' :: e :: '\'' :: r => lexLoop r (acc.push (.num (escChar e)))
  | '\'' :: '\\' :: _ => throw "lex error: bad character literal"
  | '\'' :: c :: '\'' :: r => lexLoop r (acc.push (.num c.toNat))
  | '"' :: r =>
    let body := r.takeWhile (· != '"')
    let rest := r.dropWhile (· != '"')
    match rest with
    | [] => throw "lex error: unterminated string"
    | _ :: rest' => lexLoop rest' (acc.push (.str (String.mk body)))
  | c :: r =>
    if c.isWhitespace then
      lexLoop r acc
    else if c.isDigit then
      let digits := String.mk (cs.takeWhile Char.isDigit)
      lexLoop (cs.dropWhile Char.isDigit) (acc.push (.num digits.toNat!))
    else if c.isAlpha || c == '_' then
      let name := String.mk (cs.takeWhile isIdChar)
      lexLoop (cs.dropWhile isIdChar) (acc.push (.ident name))
    else if "+-*/%()<>={};[],!".contains c then
      lexLoop r (acc.push (.sym c.toString))
    else
      throw s!"lex error: unexpected character '{c}'"

def lex (src : String) : Except String (Array Tok) :=
  lexLoop src.toList #[]

/-! ## 2. AST -/

inductive BinOp where
  | add | sub | mul | div | mod
  | eq | ne | lt | le | gt | ge
  deriving BEq, Repr, Inhabited

def BinOp.code : BinOp → Nat
  | .add => 0 | .sub => 1 | .mul => 2 | .div => 3 | .mod => 4
  | .eq => 5 | .ne => 6 | .lt => 7 | .le => 8 | .gt => 9 | .ge => 10

def BinOp.ofCode : Nat → Option BinOp
  | 0 => some .add | 1 => some .sub | 2 => some .mul | 3 => some .div | 4 => some .mod
  | 5 => some .eq | 6 => some .ne | 7 => some .lt | 8 => some .le | 9 => some .gt
  | 10 => some .ge
  | _ => none

def BinOp.toStr : BinOp → String
  | .add => "add" | .sub => "sub" | .mul => "mul" | .div => "div" | .mod => "mod"
  | .eq => "eq" | .ne => "ne" | .lt => "lt" | .le => "le" | .gt => "gt" | .ge => "ge"

inductive Expr where
  | num (n : Int)
  | str (s : String)
  | var (x : String)
  | idx (x : String) (i : Expr)
  | neg (e : Expr)
  | not (e : Expr)
  | bin (op : BinOp) (l r : Expr)
  | and (l r : Expr)
  | or (l r : Expr)
  | call (f : String) (args : List Expr)
  deriving Inhabited

inductive Stmt where
  | decl (x : String) (e : Expr)
  | assign (x : String) (e : Expr)
  | assignIdx (x : String) (i e : Expr)
  | print (e : Expr)
  | ret (e : Expr)
  | callStmt (e : Expr)
  | ite (c : Expr) (t : List Stmt) (f : Option (List Stmt))
  | loop (c : Expr) (body : List Stmt)
  deriving Inhabited

inductive Item where
  | fn (name : String) (params : List String) (body : List Stmt)
  | arr (name : String) (size : Nat)
  | stmt (s : Stmt)
  deriving Inhabited

/-! ## 3. 遞迴下降語法分析 -/

structure PState where
  toks : Array Tok
  pos  : Nat

abbrev P := StateT PState (Except String)

def peek : P Tok := do
  let s ← get
  return s.toks.getD s.pos .eof

def advance : P Unit :=
  modify fun s => { s with pos := s.pos + 1 }

def expectSym (c : String) : P Unit := do
  let t ← peek
  if t == .sym c then advance
  else throw s!"parse error: expected '{c}' but got '{t.toStr}'"

def expectIdent : P String := do
  match ← peek with
  | .ident s =>
    advance
    return s
  | t => throw s!"parse error: expected identifier but got '{t.toStr}'"

def cmpOp? : Tok → Option BinOp
  | .sym "==" => some .eq | .sym "!=" => some .ne
  | .sym "<"  => some .lt | .sym "<=" => some .le
  | .sym ">"  => some .gt | .sym ">=" => some .ge
  | _ => none

def addOp? : Tok → Option BinOp
  | .sym "+" => some .add | .sym "-" => some .sub | _ => none

def mulOp? : Tok → Option BinOp
  | .sym "*" => some .mul | .sym "/" => some .div | .sym "%" => some .mod | _ => none

mutual
  partial def parseExpr : P Expr := parseOr

  partial def parseOr : P Expr := do
    let l ← parseAnd
    parseOrRest l

  partial def parseOrRest (l : Expr) : P Expr := do
    match ← peek with
    | .sym "||" =>
      advance
      let r ← parseAnd
      parseOrRest (.or l r)
    | _ => return l

  partial def parseAnd : P Expr := do
    let l ← parseCmp
    parseAndRest l

  partial def parseAndRest (l : Expr) : P Expr := do
    match ← peek with
    | .sym "&&" =>
      advance
      let r ← parseCmp
      parseAndRest (.and l r)
    | _ => return l

  partial def parseCmp : P Expr := do
    let l ← parseAdd
    match cmpOp? (← peek) with
    | some op =>
      advance
      let r ← parseAdd
      return .bin op l r
    | none => return l

  partial def parseAdd : P Expr := do
    let l ← parseMul
    parseAddRest l

  partial def parseAddRest (l : Expr) : P Expr := do
    match addOp? (← peek) with
    | some op =>
      advance
      let r ← parseMul
      parseAddRest (.bin op l r)
    | none => return l

  partial def parseMul : P Expr := do
    let l ← parseUnary
    parseMulRest l

  partial def parseMulRest (l : Expr) : P Expr := do
    match mulOp? (← peek) with
    | some op =>
      advance
      let r ← parseUnary
      parseMulRest (.bin op l r)
    | none => return l

  partial def parseUnary : P Expr := do
    match ← peek with
    | .sym "-" =>
      advance
      let e ← parseUnary
      return .neg e
    | .sym "!" =>
      advance
      let e ← parseUnary
      return .not e
    | _ => parseAtom

  partial def parseAtom : P Expr := do
    match ← peek with
    | .num n =>
      advance
      return .num (Int.ofNat n)
    | .str s =>
      advance
      return .str s
    | .ident x =>
      advance
      match ← peek with
      | .sym "(" =>
        advance
        let args ← parseArgs
        expectSym ")"
        return .call x args
      | .sym "[" =>
        advance
        let i ← parseExpr
        expectSym "]"
        return .idx x i
      | _ => return .var x
    | .sym "(" =>
      advance
      let e ← parseExpr
      expectSym ")"
      return e
    | t => throw s!"parse error: unexpected '{t.toStr}' in expression"

  partial def parseArgs : P (List Expr) := do
    match ← peek with
    | .sym ")" => return []
    | _ =>
      let e ← parseExpr
      let rest ← parseArgsRest
      return e :: rest

  partial def parseArgsRest : P (List Expr) := do
    match ← peek with
    | .sym "," =>
      advance
      let e ← parseExpr
      let rest ← parseArgsRest
      return e :: rest
    | _ => return []
end

mutual
  partial def parseBlock : P (List Stmt) := do
    expectSym "{"
    let ss ← parseStmts
    expectSym "}"
    return ss

  partial def parseStmts : P (List Stmt) := do
    match ← peek with
    | .sym "}" => return []
    | .eof => return []
    | _ =>
      let s ← parseStmt
      let rest ← parseStmts
      return s :: rest

  partial def parseStmt : P Stmt := do
    match ← peek with
    | .ident "let" =>
      advance
      let x ← expectIdent
      expectSym "="
      let e ← parseExpr
      expectSym ";"
      return .decl x e
    | .ident "print" =>
      advance
      let e ← parseExpr
      expectSym ";"
      return .print e
    | .ident "return" =>
      advance
      let e ← parseExpr
      expectSym ";"
      return .ret e
    | .ident "while" =>
      advance
      expectSym "("
      let c ← parseExpr
      expectSym ")"
      let body ← parseBlock
      return .loop c body
    | .ident "if" =>
      advance
      expectSym "("
      let c ← parseExpr
      expectSym ")"
      let t ← parseBlock
      match ← peek with
      | .ident "else" =>
        advance
        match ← peek with
        | .ident "if" =>
          let s ← parseStmt
          return .ite c t (some [s])
        | _ =>
          let f ← parseBlock
          return .ite c t (some f)
      | _ => return .ite c t none
    | .ident x =>
      advance
      match ← peek with
      | .sym "=" =>
        advance
        let e ← parseExpr
        expectSym ";"
        return .assign x e
      | .sym "[" =>
        advance
        let i ← parseExpr
        expectSym "]"
        expectSym "="
        let e ← parseExpr
        expectSym ";"
        return .assignIdx x i e
      | .sym "(" =>
        advance
        let args ← parseArgs
        expectSym ")"
        expectSym ";"
        return .callStmt (.call x args)
      | t => throw s!"parse error: unexpected '{t.toStr}' after '{x}'"
    | t => throw s!"parse error: unexpected '{t.toStr}' at start of statement"
end

partial def parseParamsRest : P (List String) := do
  match ← peek with
  | .sym "," =>
    advance
    let x ← expectIdent
    let rest ← parseParamsRest
    return x :: rest
  | _ => return []

def parseParams : P (List String) := do
  match ← peek with
  | .sym ")" => return []
  | _ =>
    let x ← expectIdent
    let rest ← parseParamsRest
    return x :: rest

def parseItem : P Item := do
  match ← peek with
  | .ident "fn" =>
    advance
    let name ← expectIdent
    expectSym "("
    let ps ← parseParams
    expectSym ")"
    let body ← parseBlock
    return .fn name ps body
  | .ident "array" =>
    advance
    let name ← expectIdent
    expectSym "["
    match ← peek with
    | .num n =>
      advance
      expectSym "]"
      expectSym ";"
      return .arr name n
    | t => throw s!"parse error: expected array size but got '{t.toStr}'"
  | _ =>
    let s ← parseStmt
    return .stmt s

partial def parseItems : P (List Item) := do
  match ← peek with
  | .eof => return []
  | _ =>
    let it ← parseItem
    let rest ← parseItems
    return it :: rest

def parse (toks : Array Tok) : Except String (List Item) := do
  let (items, _) ← parseItems.run { toks := toks, pos := 0 }
  return items

/-! ## 4. 自訂 IR -/

inductive Instr where
  | push (n : Int)
  | load (slot : Nat)
  | store (slot : Nat)
  | mload
  | mstore
  | bin (op : BinOp)
  | neg
  | not
  | print
  | jmp (t : Nat)
  | jz (t : Nat)
  | call (t : Nat)
  | enter (n : Nat)
  | ret
  | pop
  | getc
  | putc
  | halt
  deriving Repr, Inhabited, BEq

def Instr.encode : Instr → Int × Int
  | .push n  => (0, n)
  | .load k  => (1, (k : Int))
  | .store k => (2, (k : Int))
  | .mload   => (3, 0)
  | .mstore  => (4, 0)
  | .bin op  => (5, (op.code : Int))
  | .neg     => (6, 0)
  | .not     => (7, 0)
  | .print   => (8, 0)
  | .jmp t   => (9, (t : Int))
  | .jz t    => (10, (t : Int))
  | .call t  => (11, (t : Int))
  | .enter n => (12, (n : Int))
  | .ret     => (13, 0)
  | .pop     => (14, 0)
  | .getc    => (15, 0)
  | .putc    => (16, 0)
  | .halt    => (17, 0)

def Instr.decode (op a : Int) : Except String Instr :=
  let n := a.toNat
  match op with
  | 0  => pure (.push a)
  | 1  => pure (.load n)
  | 2  => pure (.store n)
  | 3  => pure .mload
  | 4  => pure .mstore
  | 5  =>
    match BinOp.ofCode n with
    | some o => pure (.bin o)
    | none => throw s!"bad binop code {a}"
  | 6  => pure .neg
  | 7  => pure .not
  | 8  => pure .print
  | 9  => pure (.jmp n)
  | 10 => pure (.jz n)
  | 11 => pure (.call n)
  | 12 => pure (.enter n)
  | 13 => pure .ret
  | 14 => pure .pop
  | 15 => pure .getc
  | 16 => pure .putc
  | 17 => pure .halt
  | _  => throw s!"bad opcode {op}"

def Instr.toStr : Instr → String
  | .push n  => s!"push   {n}"
  | .load k  => s!"load   ${k}"
  | .store k => s!"store  ${k}"
  | .mload   => "mload"
  | .mstore  => "mstore"
  | .bin op  => op.toStr
  | .neg     => "neg"
  | .not     => "not"
  | .print   => "print"
  | .jmp t   => s!"jmp    @{t}"
  | .jz t    => s!"jz     @{t}"
  | .call t  => s!"call   @{t}"
  | .enter n => s!"enter  {n}"
  | .ret     => "ret"
  | .pop     => "pop"
  | .getc    => "getc"
  | .putc    => "putc"
  | .halt    => "halt"

structure Program where
  code    : Array Instr
  memsize : Nat

/-- 人類可讀的組合語言列表 -/
def Program.dump (p : Program) : String :=
  let lines := (List.range p.code.size).map fun i => s!"{i}:\t{(p.code[i]!).toStr}"
  s!"; memory = {p.memsize} cells, {p.code.size} instructions\n" ++ String.intercalate "\n" lines

/-- 機器可讀的 IR 串流：記憶體大小，然後每條指令兩個整數 -/
def Program.toStream (p : Program) : String := Id.run do
  let mut s := toString p.memsize ++ "\n"
  for i in p.code do
    let (op, a) := i.encode
    s := s ++ toString op ++ "\n" ++ toString a ++ "\n"
  return s

def decodePairs : List Int → Array Instr → Except String (Array Instr)
  | [], acc => pure acc
  | op :: a :: rest, acc => do
    let i ← Instr.decode op a
    decodePairs rest (acc.push i)
  | _, _ => throw "malformed IR: odd number of integers"

def Program.ofStream (s : String) : Except String Program := do
  let lines := (s.splitOn "\n").filter (· ≠ "")
  let nums ← lines.mapM fun l =>
    match l.toInt? with
    | some n => pure n
    | none => throw s!"malformed IR line '{l}'"
  match nums with
  | [] => throw "empty IR"
  | m :: rest =>
    if m < 0 then
      throw s!"self-hosted compiler reported an error: code/position = {rest}"
    else
      let code ← decodePairs rest #[]
      return { code := code, memsize := m.toNat }

/-! ## 5. 運算語意 -/

def binEval (op : BinOp) (a b : Int) : Except String Int :=
  match op with
  | .add => pure (a + b)
  | .sub => pure (a - b)
  | .mul => pure (a * b)
  | .div => if b == 0 then throw "division by zero" else pure (a / b)
  | .mod => if b == 0 then throw "modulo by zero" else pure (a % b)
  | .eq  => pure (if a == b then 1 else 0)
  | .ne  => pure (if a != b then 1 else 0)
  | .lt  => pure (if a < b then 1 else 0)
  | .le  => pure (if a ≤ b then 1 else 0)
  | .gt  => pure (if a > b then 1 else 0)
  | .ge  => pure (if a ≥ b then 1 else 0)

/-! ## 6. 程式碼產生：AST → IR -/

structure Func where
  name  : String
  pc    : Nat
  arity : Nat

structure CState where
  code    : Array Instr := #[]
  globals : List (String × Nat) := []
  heap    : Nat := 0
  locals  : List (String × Nat) := []
  lcount  : Nat := 0
  inFn    : Bool := false
  funcs   : List Func := []
  sites   : Array (Nat × String × Nat) := #[]

abbrev C := StateT CState (Except String)

def emit (i : Instr) : C Nat := do
  let s ← get
  set { s with code := s.code.push i }
  return s.code.size

def emitI (i : Instr) : C Unit := do
  let _ ← emit i

def here : C Nat := do
  return (← get).code.size

/-- 回填跳躍／呼叫／enter 的運算元 -/
def patch (idx target : Nat) : C Unit :=
  modify fun s =>
    { s with code := s.code.modify idx fun i =>
        match i with
        | .jmp _   => .jmp target
        | .jz _    => .jz target
        | .call _  => .call target
        | .enter _ => .enter target
        | other    => other }

def findLocal (x : String) : C (Option Nat) := do
  return ((← get).locals.find? (·.1 == x)).map (·.2)

def findGlobal (x : String) : C (Option Nat) := do
  return ((← get).globals.find? (·.1 == x)).map (·.2)

def declareGlobal (x : String) (size : Nat) : C Nat := do
  let s ← get
  if (s.globals.find? (·.1 == x)).isSome then
    throw s!"compile error: global '{x}' already declared"
  set { s with globals := (x, s.heap) :: s.globals, heap := s.heap + size }
  return s.heap

def declareLocal (x : String) : C Nat := do
  let s ← get
  if (s.locals.find? (·.1 == x)).isSome then
    throw s!"compile error: local '{x}' already declared"
  set { s with locals := (x, s.lcount) :: s.locals, lcount := s.lcount + 1 }
  return s.lcount

/-- 推入「x 的底址」：區域變數 → 它的值（指標）；全域 → 常數位址 -/
def pushBase (x : String) : C Unit := do
  match ← findLocal x with
  | some k => emitI (.load k)
  | none =>
    match ← findGlobal x with
    | some a => emitI (.push (a : Int))
    | none => throw s!"compile error: undefined variable '{x}'"

partial def compileExpr : Expr → C Unit
  | .num n => emitI (.push n)
  | .str s => do
    let st ← get
    let a := st.heap
    set { st with heap := a + s.length + 1 }
    let mut i := 0
    for ch in s.toList do
      emitI (.push ((a + i : Nat) : Int))
      emitI (.push (ch.toNat : Int))
      emitI .mstore
      i := i + 1
    emitI (.push (a : Int))
  | .var x => do
    match ← findLocal x with
    | some k => emitI (.load k)
    | none =>
      match ← findGlobal x with
      | some a =>
        emitI (.push (a : Int))
        emitI .mload
      | none => throw s!"compile error: undefined variable '{x}'"
  | .idx x i => do
    pushBase x
    compileExpr i
    emitI (.bin .add)
    emitI .mload
  | .neg e => do
    compileExpr e
    emitI .neg
  | .not e => do
    compileExpr e
    emitI .not
  | .bin op l r => do
    compileExpr l
    compileExpr r
    emitI (.bin op)
  | .and l r => do
    compileExpr l
    let j1 ← emit (.jz 0)
    compileExpr r
    emitI (.push 0)
    emitI (.bin .ne)
    let j2 ← emit (.jmp 0)
    patch j1 (← here)
    emitI (.push 0)
    patch j2 (← here)
  | .or l r => do
    compileExpr l
    let j1 ← emit (.jz 0)
    emitI (.push 1)
    let j2 ← emit (.jmp 0)
    patch j1 (← here)
    compileExpr r
    emitI (.push 0)
    emitI (.bin .ne)
    patch j2 (← here)
  | .call f args => do
    if f == "getc" then
      unless args.isEmpty do throw "compile error: getc() takes no arguments"
      emitI .getc
    else if f == "putc" then
      match args with
      | [a] =>
        compileExpr a
        emitI .putc
      | _ => throw "compile error: putc(c) takes exactly one argument"
    else if f == "halt" then
      unless args.isEmpty do throw "compile error: halt() takes no arguments"
      emitI .halt
    else
      for a in args do compileExpr a
      emitI (.push (args.length : Int))
      let pc ← emit (.call 0)
      modify fun s => { s with sites := s.sites.push (pc, f, args.length) }

partial def compileStmt : Stmt → C Unit
  | .decl x e => do
    if (← get).inFn then
      compileExpr e
      let k ← declareLocal x
      emitI (.store k)
    else
      let a ← declareGlobal x 1
      emitI (.push (a : Int))
      compileExpr e
      emitI .mstore
  | .assign x e => do
    match ← findLocal x with
    | some k =>
      compileExpr e
      emitI (.store k)
    | none =>
      match ← findGlobal x with
      | some a =>
        emitI (.push (a : Int))
        compileExpr e
        emitI .mstore
      | none => throw s!"compile error: undefined variable '{x}'"
  | .assignIdx x i e => do
    pushBase x
    compileExpr i
    emitI (.bin .add)
    compileExpr e
    emitI .mstore
  | .print e => do
    compileExpr e
    emitI .print
  | .ret e => do
    unless (← get).inFn do throw "compile error: 'return' outside function"
    compileExpr e
    emitI .ret
  | .callStmt e => do
    compileExpr e
    emitI .pop
  | .ite c t f => do
    compileExpr c
    let j1 ← emit (.jz 0)
    for s in t do compileStmt s
    match f with
    | none => patch j1 (← here)
    | some fs =>
      let j2 ← emit (.jmp 0)
      patch j1 (← here)
      for s in fs do compileStmt s
      patch j2 (← here)
  | .loop c body => do
    let start ← here
    compileExpr c
    let j ← emit (.jz 0)
    for s in body do compileStmt s
    emitI (.jmp start)
    patch j (← here)

def compileItem : Item → C Unit
  | .arr name size => do
    let _ ← declareGlobal name size
  | .stmt s => compileStmt s
  | .fn name ps body => do
    modify fun s => { s with locals := [], lcount := 0 }
    for p in ps do
      let _ ← declareLocal p
    if (← get).funcs.any (·.name == name) then
      throw s!"compile error: function '{name}' already defined"
    let skip ← emit (.jmp 0)
    let start ← here
    modify fun s =>
      { s with funcs := ⟨name, start, ps.length⟩ :: s.funcs, inFn := true }
    let en ← emit (.enter 0)
    for s in body do compileStmt s
    emitI (.push 0)
    emitI .ret
    let lc := (← get).lcount
    patch en (lc - ps.length)
    patch skip (← here)
    modify fun s => { s with locals := [], lcount := 0, inFn := false }

def resolveCalls : C Unit := do
  let st ← get
  for (pc, f, n) in st.sites do
    match st.funcs.find? (·.name == f) with
    | none => throw s!"compile error: undefined function '{f}'"
    | some fn =>
      if fn.arity != n then
        throw s!"compile error: '{f}' expects {fn.arity} argument(s), got {n}"
      patch pc fn.pc

def compileProgram (items : List Item) : Except String Program := do
  let act : C Unit := do
    for it in items do compileItem it
    emitI .halt
    resolveCalls
  let ((), st) ← act.run {}
  return { code := st.code, memsize := st.heap }

/-- 完整前端：原始碼 → IR -/
def compile (src : String) : Except String Program := do
  let toks ← lex src
  let items ← parse toks
  compileProgram items

/-! ## 7. 虛擬機

為了讓陣列能「就地更新」，把所有狀態拆成獨立參數，並用尾遞迴迴圈。
-/

def failAt (out : String) (pc : Nat) (msg : String) : String × Option String :=
  (out, some s!"runtime error at pc {pc}: {msg}")

partial def exec (code : Array Instr) (input : ByteArray) (limit : Nat)
    (pc fp ip steps : Nat) (stack mem : Array Int) (calls : Array Nat)
    (out : String) : String × Option String :=
  if steps ≥ limit then failAt out pc "step limit exceeded" else
  match code[pc]? with
  | none => failAt out pc "pc out of range"
  | some ins =>
    let next := pc + 1
    let steps := steps + 1
    match ins with
    | .push n =>
      exec code input limit next fp ip steps (stack.push n) mem calls out
    | .load k =>
      match stack[fp + k]? with
      | some v => exec code input limit next fp ip steps (stack.push v) mem calls out
      | none => failAt out pc "bad local slot"
    | .store k =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some v =>
        let st := stack.pop
        if fp + k < st.size then
          exec code input limit next fp ip steps (st.set! (fp + k) v) mem calls out
        else failAt out pc "bad local slot"
    | .mload =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some a =>
        if a < 0 then failAt out pc s!"negative address {a}" else
        match mem[a.toNat]? with
        | some v => exec code input limit next fp ip steps (stack.pop.push v) mem calls out
        | none => failAt out pc s!"memory read out of range: {a}"
    | .mstore =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some v =>
        let st1 := stack.pop
        match st1.back? with
        | none => failAt out pc "stack underflow"
        | some a =>
          if a < 0 ∨ a.toNat ≥ mem.size then
            failAt out pc s!"memory write out of range: {a}"
          else
            exec code input limit next fp ip steps st1.pop (mem.set! a.toNat v) calls out
    | .bin op =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some b =>
        let st1 := stack.pop
        match st1.back? with
        | none => failAt out pc "stack underflow"
        | some a =>
          match binEval op a b with
          | .ok r => exec code input limit next fp ip steps (st1.pop.push r) mem calls out
          | .error e => failAt out pc e
    | .neg =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some a => exec code input limit next fp ip steps (stack.pop.push (-a)) mem calls out
    | .not =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some a =>
        exec code input limit next fp ip steps
          (stack.pop.push (if a == 0 then 1 else 0)) mem calls out
    | .print =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some a =>
        exec code input limit next fp ip steps stack.pop mem calls (out ++ toString a ++ "\n")
    | .jmp t => exec code input limit t fp ip steps stack mem calls out
    | .jz t =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some c =>
        exec code input limit (if c == 0 then t else next) fp ip steps stack.pop mem calls out
    | .call t =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some n =>
        let st := stack.pop
        if n < 0 ∨ n.toNat > st.size then failAt out pc "bad argument count"
        else
          let newfp := st.size - n.toNat
          exec code input limit t newfp ip steps st mem ((calls.push next).push fp) out
    | .enter n =>
      exec code input limit next fp ip steps (Nat.repeat (fun s => s.push 0) n stack) mem calls out
    | .ret =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some v =>
        if calls.size < 2 then failAt out pc "ret outside function" else
        let oldfp := calls.back!
        let c1 := calls.pop
        let rpc := c1.back!
        exec code input limit rpc oldfp ip steps ((stack.shrink fp).push v) mem c1.pop out
    | .pop =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some _ => exec code input limit next fp ip steps stack.pop mem calls out
    | .getc =>
      if ip < input.size then
        exec code input limit next fp (ip + 1) steps
          (stack.push (Int.ofNat input[ip]!.toNat)) mem calls out
      else
        exec code input limit next fp ip steps (stack.push (-1)) mem calls out
    | .putc =>
      match stack.back? with
      | none => failAt out pc "stack underflow"
      | some c =>
        exec code input limit next fp ip steps (stack.pop.push 0) mem calls
          (out.push (Char.ofNat c.toNat))
    | .halt => (out, none)

def defaultLimit : Nat := 4000000000

/-- 執行 IR；回傳 (標準輸出內容, 錯誤訊息?) -/
def run (p : Program) (input : ByteArray := ByteArray.empty)
    (limit : Nat := defaultLimit) : String × Option String :=
  exec p.code input limit 0 0 0 0 #[] (Array.replicate p.memsize 0) #[] ""

end Mini

/-! ## 8. 命令列與自舉驅動 -/

open Mini

def orDie (r : Except String α) : IO α :=
  match r with
  | .ok a => pure a
  | .error e => throw (IO.userError e)

def readAllStdin : IO ByteArray := do
  let stdin ← IO.getStdin
  let mut buf := ByteArray.empty
  repeat
    let chunk ← stdin.read 65536
    if chunk.isEmpty then break
    buf := buf ++ chunk
  return buf

def loadProgram (path : String) : IO Program := do
  let text ← IO.FS.readFile path
  if path.endsWith ".ir" then orDie (Program.ofStream text)
  else orDie (compile text)

/-- 用 IR 編譯器（在 VM 上跑）編譯 `src`，回傳 IR 串流文字 -/
def selfCompile (stage : String) (p : Program) (src : ByteArray) : IO String := do
  let t0 ← IO.monoMsNow
  let (out, err) := run p src
  let t1 ← IO.monoMsNow
  if let some e := err then
    throw (IO.userError s!"{stage}: {e}")
  IO.println s!"  {stage}: 產生 {out.length} 字元的 IR（VM 執行 {t1 - t0} ms）"
  return out

def bootstrap (compilerPath : String) (tests : List String) : IO UInt32 := do
  let src ← IO.FS.readFile compilerPath
  let srcBytes := src.toUTF8
  IO.println s!"=== 自舉測試：{compilerPath}（{src.length} 字元）==="
  -- stage 0
  let p0 ← orDie (compile src)
  let s0 := p0.toStream
  IO.println s!"stage0  Lean 編譯器編譯 compiler.mini → {p0.code.size} 條指令，記憶體 {p0.memsize} 格"
  -- stage 1
  IO.println "stage1  以 stage0 產生的 IR 在 VM 上編譯 compiler.mini："
  let s1 ← selfCompile "stage1" p0 srcBytes
  let p1 ← orDie (Program.ofStream s1)
  -- stage 2
  IO.println "stage2  以 stage1 產生的 IR 在 VM 上再編譯 compiler.mini："
  let s2 ← selfCompile "stage2" p1 srcBytes
  let p2 ← orDie (Program.ofStream s2)
  let ok01 := s0 == s1
  let ok12 := s1 == s2
  IO.println s!"  IR₀ = IR₁ ？ {if ok01 then "✔ 相同" else "✘ 不同"}"
  IO.println s!"  IR₁ = IR₂ ？ {if ok12 then "✔ 相同（不動點）" else "✘ 不同"}"
  let mut allOk := ok01 && ok12
  -- 用自舉出來的編譯器（stage2）編譯其他程式，並與 stage0 的結果比對
  if !tests.isEmpty then
    IO.println "--- 以自舉編譯器 (stage2) 編譯範例並與 stage0 比對 ---"
  for t in tests do
    let tsrc ← IO.FS.readFile t
    let want ← match compile tsrc with
      | .ok p => pure p.toStream
      | .error e => throw (IO.userError s!"{t}: {e}")
    let (got, err) := run p2 tsrc.toUTF8
    let same := err.isNone && got == want
    if !same then allOk := false
    IO.println s!"  {if same then "✔" else "✘"} {t}"
  IO.println (if allOk then "\n自舉成功 🎉" else "\n自舉失敗")
  return (if allOk then 0 else 1)

def usage : String :=
  "用法：minic <指令> <檔案> [...]
  compile <f.mini>        編譯，輸出機器可讀 IR（整數串流）
  list    <f.mini|f.ir>   輸出人類可讀的 IR 組合語言
  run     <f.mini|f.ir>   編譯（若是 .mini）並執行；程式的 stdin/stdout 即本程序的
  bootstrap <compiler.mini> [tests.mini ...]   三階段自舉並驗證不動點"

def main (args : List String) : IO UInt32 := do
  try
    match args with
    | ["compile", f] =>
      let p ← loadProgram f
      IO.print p.toStream
      return 0
    | ["list", f] =>
      let p ← loadProgram f
      IO.println p.dump
      return 0
    | ["run", f] =>
      let p ← loadProgram f
      let input ← readAllStdin
      let (out, err) := run p input
      IO.print out
      match err with
      | some e =>
        IO.eprintln e
        return 1
      | none => return 0
    | "bootstrap" :: c :: tests => bootstrap c tests
    | _ =>
      IO.eprintln usage
      return 2
  catch e =>
    IO.eprintln s!"{e}"
    return 1
