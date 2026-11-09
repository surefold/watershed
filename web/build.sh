#!/usr/bin/env bash
# Builds the interactive demo into web/dist.
#
#   cargo install wasm-bindgen-cli --version 0.2.130   # once
#   rustup target add wasm32-unknown-unknown            # once
#   web/build.sh
#   python3 -m http.server -d web/dist 8000             # then open http://localhost:8000
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --locked --release --target wasm32-unknown-unknown -p watershed-web
rm -rf web/dist
mkdir -p web/dist/pkg
wasm-bindgen --target web --no-typescript --out-dir web/dist/pkg \
  target/wasm32-unknown-unknown/release/watershed_web.wasm

# demo.html holds the page's content; wrap it in a full document.
{
  printf '<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
  printf '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n'
  printf '<style>:root{padding-top:env(safe-area-inset-top,0px);padding-bottom:env(safe-area-inset-bottom,0px)}body{margin:0}[hidden]{display:none!important}</style>\n'
  printf '</head>\n<body>\n'
  cat web/demo.html
  printf '\n</body>\n</html>\n'
} > web/dist/index.html

ls -la web/dist web/dist/pkg
