#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$ROOT_DIR/target/ci_$$"
export CARGO_INCREMENTAL=0
export RUSTFLAGS="-C codegen-units=1"

echo "Using CARGO_TARGET_DIR=$CARGO_TARGET_DIR"

cargo fmt --all --check
cargo clippy --all-targets --all-features -- -A clippy::approx_constant
cargo test -j 1
