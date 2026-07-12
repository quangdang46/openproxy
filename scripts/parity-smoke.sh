#!/usr/bin/env bash
# CI-stable 9router parity smoke (no live provider keys).
set -euo pipefail
cd "$(dirname "$0")/.."
echo "== parity smoke =="
cargo test -p openproxy --lib stream_flags -- --nocapture
cargo test -p openproxy --lib parity_tests -- --nocapture
cargo test -p openproxy --lib claude_format -- --nocapture
cargo test -p openproxy --lib combo -- --nocapture
cargo test -p openproxy --lib chat:: -- --nocapture
cargo test -p openproxy --lib error_config -- --nocapture
echo "OK"
