#!/bin/sh
# Pulse, React on Craie against GPUI: the storm at each tile count, 15 s
# per run, each app in a window, one at a time, then a matched summary
# (bench/summarize.py). Keep each window in front while it runs: macOS
# stops drawing a covered window, and the run then reports nothing.
#
#   sh bench/compare.sh              (TILES="5000" to run one count)
set -e
cd "$(dirname "$0")/.."
pnpm build:native:release
pnpm --dir examples/pulse build
(cd bench/gpui-pulse && cargo build --release)
out=${TMPDIR:-/tmp}/pulse-compare
mkdir -p "$out"
run() { # label, log, command...
  label=$1; log=$2; shift 2
  env PULSE_LOG=1 PULSE_STORM=1 "$@" > "$log" 2>&1 &
  pid=$!
  sleep 15
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  python3 bench/summarize.py "$log" "$label"
}
for tiles in ${TILES:-2500 5000 10000}; do
  run "Craie $tiles" "$out/craie-$tiles.log" PULSE_TILES=$tiles node examples/pulse/dist/host.mjs
  run "GPUI elements $tiles" "$out/gpui-$tiles.log" PULSE_TILES=$tiles bench/gpui-pulse/target/release/gpui-pulse
  run "GPUI canvas $tiles" "$out/gpui-canvas-$tiles.log" PULSE_TILES=$tiles PULSE_CANVAS=1 bench/gpui-pulse/target/release/gpui-pulse
done
