#!/bin/sh
# 測試：跑全部整合測試（5 個 examples 正例 + 反例必須被拒絕）
set -e
cd "$(dirname "$0")"
cargo test
