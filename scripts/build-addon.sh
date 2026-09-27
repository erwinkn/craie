#!/bin/sh
# Builds the N-API addon and copies the shared library to
# craie-node.node at the repo root, where loadBindings() finds it.
# `--release` builds optimized (for measurements and demos).
set -e
cd "$(dirname "$0")/.."

profile=debug
if [ "$1" = "--release" ]; then
  profile=release
  cargo build --release -p craie-node
else
  cargo build -p craie-node
fi
dir=${CARGO_TARGET_DIR:-target}/$profile
case "$(uname -s)" in
  Darwin) src=$dir/libcraie_node.dylib ;;
  Linux)  src=$dir/libcraie_node.so ;;
  *)      echo "unsupported platform: $(uname -s)" >&2; exit 1 ;;
esac
cp "$src" craie-node.node
# Adhoc-signed dylibs can fault a stale signature page after `cp`
# (SIGKILL "Code Signature Invalid"); refresh the signature on Darwin.
if [ "$(uname -s)" = Darwin ]; then codesign -s - -f craie-node.node; fi
