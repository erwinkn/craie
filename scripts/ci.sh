#!/bin/sh
# Everything CI runs, in order. Fails on the first error.
#
#   scripts/ci.sh
#
# The wasm32 check keeps the portable crates free of desktop-only
# facilities (ARCHITECTURE.md §1, §19). The harness tests include the
# release-graph and layer-map checks; the release graph is the macOS
# one on any host, so fetch every target's packages first.
#
# Clippy lints only the host's cfg. Allowed lints, with reasons, are in
# `[workspace.lints.clippy]` in the root Cargo.toml.
set -e
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo fetch --locked
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test
cargo check --target wasm32-unknown-unknown \
  -p craie-core -p craie-scene -p craie-render -p craie-text
bun test --cwd packages/bridge
packages/bridge/node_modules/.bin/tsc --noEmit -p packages/bridge
packages/bridge/node_modules/.bin/tsc --noEmit -p packages/react
echo "ci: ok"
