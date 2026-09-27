#!/bin/sh
# E19, the event round trip under load (EXPERIMENTS.md): the native probe
# clicks the app 1000 times per load, 30 to 70 ms apart, and each run
# prints p50/p95/p99 per phase (bench/e19/report.py). Windowed: keep the
# window in front while it runs (macOS stops drawing a covered window).
# CRAIE_HEADLESS=1 runs offscreen.
#
#   sh bench/e19.sh              (LOADS="idle stream" to run some loads)
set -e
cd "$(dirname "$0")/.."
pnpm build:native:release
pnpm --dir bench/e19 build
out=${TMPDIR:-/tmp}/craie-e19
mkdir -p "$out"
frame=${CRAIE_HEADLESS:+headless}
frame=${frame:-windowed}
for load in ${LOADS:-idle stream gc stream,gc}; do
  name=$(echo "$load" | tr , +)
  rm -f "$out/$name.csv" "$out/$name"-*.csv
  E19_LOAD=$load CRAIE_E19="$out/$name.csv" node bench/e19/dist/host.mjs
  python3 bench/e19/report.py "$out/$name.csv" "$load ($(uname -sm), $frame)" $frame
done
