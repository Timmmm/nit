#!/bin/bash -e

cargo build --release --target wasm32-wasip2 --package lint_whitespace
cargo run -- set-metadata --metadata lints/lint_whitespace/metadata.json         target/wasm32-wasip2/release/lint_whitespace.wasm
cargo run -- --config .nit_local.json5 run
