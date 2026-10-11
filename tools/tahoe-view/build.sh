#!/bin/bash
# Compila el visor (necesita SDL3: paquete sdl3 / libsdl3-dev).
set -euo pipefail
cd "$(dirname "$0")"
cc -O2 -Wall -Wextra -o tahoe-view tahoe-view.c $(pkg-config --cflags --libs sdl3)
