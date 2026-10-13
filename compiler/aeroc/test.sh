#!/bin/bash
# aeroc 完整測試：正例 run（輸出逐字比對）＋ 反例 check（錯誤碼斷言）＋ 產碼結構斷言
# 用法：./test.sh（exit 0 = 全過，exit 1 = 有失敗）
cd "$(dirname "$0")"

PASS=0
FAIL=0
AEROC=./target/release/aeroc

ok()  { PASS=$((PASS + 1)); echo "  ok   - $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL - $1"; [ -n "${2:-}" ] && echo "         $2"; }

echo "== build"
if cargo build --release 2>&1 | tail -1; then
  ok "cargo build --release"
else
  bad "cargo build --release"; exit 1
fi
[ -x "$AEROC" ] || { bad "找無 $AEROC"; exit 1; }

# ── 正例：run 成功且 stdout（以 | 連接每行，含結尾 |）完全相符 ──
expect_run() { # $1=.aero $2=預期輸出
  local name out rc
  name=$(basename "$1" .aero)
  out=$("$AEROC" run "$1" --out-dir "target/aero/t_$name" 2>/dev/null | tr '\n' '|'); rc=$?
  if [ $rc -eq 0 ] && [ "$out" = "$2" ]; then ok "run $1";
  else bad "run $1" "rc=$rc want=[$2] got=[$out]"; fi
}

echo "== 正例 run"
expect_run examples/demo.aero    '7|10|7|20|42|hello, aero|3|111|112|1|112|5|'
expect_run examples/methods.aero '12|17|17|150|2|'
expect_run examples/enums.aero   '5|-7|0|zero|small|negative|big|1|missing|2|3|'
expect_run examples/async.aero   '40|42|'
expect_run examples/syntax2.aero 'negative|zero|positive|50|109|and-ok|or-else|5|7|5|18|'
expect_run examples/traits.aero  '12|12|10|3|true|'
expect_run examples/crates_json.aero '{"x":3,"y":4}|7|'
expect_run examples/crates_regex.aero 'true|false|'
expect_run examples/modules/main.aero '25|81|12|12|0|103|true|false|'

# ── axum web server：編譯、背景啟動、打兩個端點、比對、關閉 ──
echo "== axum web server"
if ! command -v curl >/dev/null; then
  bad "axum server" "缺 curl，跳過"
else
  "$AEROC" build examples/axum_server.aero --out-dir target/aero/t_axum >/dev/null 2>&1
  BIN=target/aero/t_axum/target/debug/axum_server
  if [ ! -x "$BIN" ]; then
    bad "axum server" "編譯失敗，找無 $BIN"
  else
    "$BIN" >/dev/null 2>&1 & SRV=$!
    for _ in $(seq 1 20); do
      curl -sf --max-time 2 http://127.0.0.1:18080/health >/dev/null 2>&1 && break
      sleep 0.5
    done
    ROOT=$(curl -s --max-time 5 http://127.0.0.1:18080/)
    HEALTH=$(curl -s --max-time 5 http://127.0.0.1:18080/health)
    kill $SRV 2>/dev/null; wait 2>/dev/null
    if [ "$ROOT" = "hello, aero" ] && [ "$HEALTH" = "ok" ]; then ok "axum server（/ 與 /health）";
    else bad "axum server" "/=[$ROOT] /health=[$HEALTH]"; fi
  fi
fi

# ── 反例：check 必須失敗，且 JSON 含預期錯誤碼 ──
expect_check_fail() { # $1=.aero $2=錯誤碼
  local name out rc
  name=$(basename "$1" .aero)
  out=$("$AEROC" check "$1" --json --out-dir "target/aero/n_$name" 2>/dev/null); rc=$?
  if [ $rc -ne 0 ] && echo "$out" | grep -q "\"error_code\": \"$2\""; then ok "check $1 -> $2";
  else bad "check $1 -> $2" "rc=$rc out=$(echo "$out" | head -c 200)"; fi
}

echo "== 反例 check"
expect_check_fail examples/moved.aero E0102            # move 後再用
expect_check_fail examples/syntax_err.aero E0001       # 解析失敗
expect_check_fail examples/trait_bound_fail.aero E0301 # 未滿足 Add 命題

# ── 產碼結構：trait 機制的三個可見證據 ──
echo "== 產碼結構"
EMIT=$("$AEROC" emit examples/traits.aero)
check_grep() { # $1=pattern $2=說明
  if echo "$EMIT" | grep -q "$1"; then ok "$2"; else bad "$2" "缺 [$1]"; fi
}
check_grep 'pub trait Measurable'              'trait 定義產出'
check_grep 'fn sum<T.*Add.*Clone'              '泛型約束邊界產出'
check_grep 'pub struct Pair<T.*Clone'          '泛型結構產出'
check_grep 'PartialOrd'                        '自動衍生（Comparable 證明）產出'
check_grep 'serde::Serialize'                  '自動衍生（Serializable 證明）產出'
check_grep 'impl Measurable for Point'         '自動橋接 impl 產出'

echo "== 結果：$PASS 過 / $FAIL 敗"
[ "$FAIL" -eq 0 ]
