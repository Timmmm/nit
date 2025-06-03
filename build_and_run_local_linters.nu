#!/usr/bin/env nu

cargo build --release --target wasm32-wasip2 --package lint_case_conflict
cargo build --release --target wasm32-wasip2 --package lint_deny
cargo build --release --target wasm32-wasip2 --package lint_executable_shebang
cargo build --release --target wasm32-wasip2 --package lint_json_format
cargo build --release --target wasm32-wasip2 --package lint_merge_conflicts
cargo build --release --target wasm32-wasip2 --package lint_regex
cargo build --release --target wasm32-wasip2 --package lint_tabs
cargo build --release --target wasm32-wasip2 --package lint_whitespace

# cargo run -- set-metadata --metadata lints/lint_case_conflict/metadata.json      target/wasm32-wasip2/release/lint_case_conflict.wasm
cargo run -- set-metadata --metadata lints/lint_deny/metadata.json               target/wasm32-wasip2/release/lint_deny.wasm
# cargo run -- set-metadata --metadata lints/lint_executable_shebang/metadata.json target/wasm32-wasip2/release/lint_executable_shebang.wasm
cargo run -- set-metadata --metadata lints/lint_json_format/metadata.json        target/wasm32-wasip2/release/lint_json_format.wasm
# cargo run -- set-metadata --metadata lints/lint_merge_conflicts/metadata.json    target/wasm32-wasip2/release/lint_merge_conflicts.wasm
cargo run -- set-metadata --metadata lints/lint_regex/metadata.json              target/wasm32-wasip2/release/lint_regex.wasm
# cargo run -- set-metadata --metadata lints/lint_tabs/metadata.json               target/wasm32-wasip2/release/lint_tabs.wasm
cargo run -- set-metadata --metadata lints/lint_whitespace/metadata.json         target/wasm32-wasip2/release/lint_whitespace.wasm

cargo run -- --config .nit_local.json5 run
