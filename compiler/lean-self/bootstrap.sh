#!/bin/sh
# 一鍵自舉：建置 stage0（Lean），再讓 compiler.mini 編譯自己兩次，比對三份 IR。
set -e
lake build
M=.lake/build/bin/minic
mkdir -p build
$M compile compiler.mini                  > build/stage0.ir   # Lean 編譯器 → IR₀
$M run build/stage0.ir < compiler.mini    > build/stage1.ir   # IR₀ 在 VM 上編譯自己 → IR₁
$M run build/stage1.ir < compiler.mini    > build/stage2.ir   # IR₁ 在 VM 上編譯自己 → IR₂
cmp build/stage0.ir build/stage1.ir && cmp build/stage1.ir build/stage2.ir \
  && echo "自舉成功：stage0 = stage1 = stage2（不動點）"
sha256sum build/stage*.ir
# 內建的完整驗證（含範例程式比對）
$M bootstrap compiler.mini examples/*.mini
