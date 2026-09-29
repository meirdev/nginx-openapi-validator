#!/usr/bin/env bash
# Build the browser demo: compile crates/oav-wasm to WebAssembly and generate
# its JavaScript bindings into demo/pkg/.
#
# Requires the wasm32-unknown-unknown target and a wasm-bindgen CLI whose
# version matches the wasm-bindgen crate pinned in crates/oav-wasm/Cargo.toml:
#
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.129 --locked
#
# wasm-opt (from binaryen), if installed, is used to shrink the output.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/demo/pkg"

cargo build --manifest-path "$root/Cargo.toml" -p oav-wasm \
    --target wasm32-unknown-unknown --profile wasm-release --locked

wasm-bindgen --target web --no-typescript --out-dir "$out" \
    "$root/target/wasm32-unknown-unknown/wasm-release/oav_wasm.wasm"

cp "$root/example/api-spec.json" "$out/api-spec.json"

if command -v wasm-opt >/dev/null; then
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int \
        -o "$out/oav_wasm_bg.wasm" "$out/oav_wasm_bg.wasm"
fi

echo "Built $out. Serve demo/ over HTTP, e.g.: python3 -m http.server -d demo 8080"
