#!/bin/sh
# Frame-cost benchmark sweep — craie counterpart of gpui-react's
# scripts/measure-frames.ts. Dumps real React commits per row count,
# then replays them natively.
#
#   scripts/measure-framebench.sh [out-dir] [rows ...]
#
# Writes <out>/wire/*-flow-<rows>.bin inputs and <out>/result-<rows>.json
# per row count. Only the `flow` scene exists today; list
# virtualization and a native scroll/input path are planned work, at
# which point this gains `list` and a wheel-dispatch phase.

set -e
cd "$(dirname "$0")/.."

OUT=${1:-/tmp/craie-framebench}
shift 2>/dev/null || true
ROWS=${@:-"100 1000 5000"}

mkdir -p "$OUT/wire"
cargo build --release --example framebench

for rows in $ROWS; do
  bun examples/js/dump-framebench.tsx "$OUT/wire" "$rows"
  ./target/release/examples/framebench "$OUT/wire" "$rows" \
    --reps 3 --json "$OUT/result-flow-$rows.json"
done

echo "results in $OUT"
