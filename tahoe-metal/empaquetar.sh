#!/bin/bash
# Compila tahoe-metal y arma la carpeta que se le da a tahoe-run en TAHOE_METAL:
#   <salida>/TahoeMetal.bundle/Contents/{Info.plist,MacOS/TahoeMetal}   y   <salida>/metalprobe (programa de prueba)
# Uso: tahoe-metal/empaquetar.sh [salida]   (por omisión tahoe-metal/salida)
set -e
cd "$(dirname "$(readlink -f "$0")")"
OUT="${1:-salida}"
cargo build --release
B="$OUT/TahoeMetal.bundle/Contents"
mkdir -p "$B/MacOS"
cp bundle/Info.plist "$B/Info.plist"
cp target/x86_64-apple-darwin/release/libtahoe_metal.dylib "$B/MacOS/TahoeMetal"
cp target/x86_64-apple-darwin/release/metalprobe "$OUT/metalprobe"
echo "listo: $(readlink -f "$OUT")"
