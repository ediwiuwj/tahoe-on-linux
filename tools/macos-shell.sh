#!/bin/bash
# Abre la shell de macOS (el bash del sistema base de macOS 26) en esta terminal, ejecutada sobre tahoe-run.
#
#   tools/macos-shell.sh                 # shell interactiva de login
#   tools/macos-shell.sh -c 'sw_vers'    # una orden
#
# TAHOE_ROOT: carpeta con el sistema base de macOS extraído (por omisión ~/macos26/base/macOS Base System).
# El entorno se limpia: el programa ve un entorno de macOS (PATH, HOME=/var/root, usuario root), no el de Linux.
# Sin launchd: las órdenes que necesitan servicios del sistema (launchctl, defaults...) aún no funcionan aquí.
ROOT="${TAHOE_ROOT:-$HOME/macos26/base/macOS Base System}"
RUN="$(cd "$(dirname "$(readlink -f "$0")")/../tahoe-run" && pwd)/tahoe-run"
if [ ! -x "$RUN" ]; then echo "falta $RUN: compila con tahoe-run/build.sh" >&2; exit 1; fi
if [ ! -f "$ROOT/System/Library/dyld/dyld_shared_cache_x86_64" ]; then echo "no encuentro el sistema base de macOS en: $ROOT (define TAHOE_ROOT)" >&2; exit 1; fi
[ $# -eq 0 ] && set -- -l
exec env -i \
	PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME=/var/root USER=root LOGNAME=root SHELL=/bin/bash \
	TERM="${TERM:-xterm-256color}" LANG="${LANG:-es_ES.UTF-8}" \
	TAHOE_ROOT="$ROOT" TAHOE_LOGFILE="${TAHOE_LOGFILE:-/tmp/tahoe-shell-$USER.log}" \
	"$RUN" "$ROOT/System/Library/dyld/dyld_shared_cache_x86_64" "$ROOT/usr/lib/dyld" "$ROOT/bin/bash" "$@"
