#!/bin/sh
# 一次跑完所有範例：內建 5 個證明 examples（ex1 And / ex2 Or / ex3 Eq / ex4 歸納 / ex5 加法交換律）
set -e
cd "$(dirname "$0")"
cargo run -q
