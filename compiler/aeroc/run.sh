#!/bin/bash
# 跑全部案例：emit 示範 + 依序 run 所有正例 + 反例的 LLM 診斷 JSON 示範
set -e
cd "$(dirname "$0")"
cargo build --release
./target/release/aeroc emit  examples/demo.aero > /dev/null    # 只驗證產生的 Rust 合法
for f in demo methods enums async syntax2 traits crates_json crates_regex; do
  echo "=== run examples/$f.aero"
  ./target/release/aeroc run   examples/$f.aero
done
echo "=== run examples/modules/main.aero"
./target/release/aeroc run   examples/modules/main.aero
echo "=== build examples/axum_server.aero（server 會常駐不退，僅編譯驗證）"
./target/release/aeroc build examples/axum_server.aero
echo "    手動啟動：./target/aero/axum_server/target/debug/axum_server"
echo "    再開一窗：curl http://127.0.0.1:18080/ ; curl http://127.0.0.1:18080/health"
echo "=== check examples/moved.aero --json（反例：預期失敗，示範 §7 診斷）"
./target/release/aeroc check examples/moved.aero --json || true
echo "=== check examples/trait_bound_fail.aero --json（反例：預期 E0301）"
./target/release/aeroc check examples/trait_bound_fail.aero --json || true
