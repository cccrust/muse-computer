#!/bin/bash
# 跑全部案例：emit 示範 + 依序 run 所有正例 + 反例的 LLM 診斷 JSON 示範
set -e
cd "$(dirname "$0")"
cargo build --release
./target/release/aeroc emit  examples/demo.aero > /dev/null    # 只驗證產生的 Rust 合法
for f in demo methods enums async syntax2 traits; do
  echo "=== run examples/$f.aero"
  ./target/release/aeroc run   examples/$f.aero
done
echo "=== run examples/modules/main.aero"
./target/release/aeroc run   examples/modules/main.aero
echo "=== check examples/moved.aero --json（反例：預期失敗，示範 §7 診斷）"
./target/release/aeroc check examples/moved.aero --json || true
echo "=== check examples/trait_bound_fail.aero --json（反例：預期 E0301）"
./target/release/aeroc check examples/trait_bound_fail.aero --json || true
