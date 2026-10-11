#!/bin/bash
# Abre la shell de macOS (el bash del sistema base de macOS 26) en esta terminal, ejecutada sobre tahoe-run.
#
#   tools/macos-shell.sh                          # shell interactiva de login, sin launchd
#   tools/macos-shell.sh -c 'sw_vers'             # una orden
#   tools/macos-shell.sh --launchd                # shell unida a launchd (launchctl, defaults... hablan con sus servicios)
#   tools/macos-shell.sh --launchd -c 'launchctl list'
#   tools/macos-shell.sh --launchd --depurar      # modo depuración (ver abajo)
#
# TAHOE_RUN: binario de tahoe-run a usar (por omisión, el del repo).
# TAHOE_ROOT: carpeta con el sistema base de macOS extraído (por omisión ~/macos26/base/macOS Base System).
# El entorno se limpia: el programa ve un entorno de macOS (PATH, HOME=/var/root, usuario root), no el de Linux.
#
# Con --launchd la shell es un proceso más del mismo arranque que launchd: si ya hay un launchd de tahoe-run corriendo
# se une a él; si no, lo arranca (en su propio espacio de PID, como PID 1, con el guardián de memoria) y lo detiene al
# salir. La shell entra en ese espacio de PID con nsenter y comparte la región de puertos Mach (TAHOE_PORTS): como no
# tiene puerto bootstrap propio usa el que launchd registró (mach_ports_register), igual que un proceso de macOS
# encuentra a launchd. TAHOE_BOOT_WAIT: segundos de espera para que arranquen los servicios (por omisión 10).
#
# --depurar: modo depuración. Pone en la NVRAM emulada ROS_SLEEP_ON_FAILURE, el ajuste de recoveryosd para que, si algo le
# obliga a reiniciar (hoy: el llavero de root), se quede dormido en vez de ejecutar /sbin/reboot y apagar todo el arranque.
# Sin la opción, el arranque es el normal, como en un Mac.
ROOT="${TAHOE_ROOT:-$HOME/macos26/base/macOS Base System}"
TOOLS="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
RUN="${TAHOE_RUN:-$(cd "$TOOLS/../tahoe-run" && pwd)/tahoe-run}"   # TAHOE_RUN: otro binario (comparar versiones)
if [ ! -x "$RUN" ]; then echo "falta $RUN: compila con tahoe-run/build.sh" >&2; exit 1; fi
# Caché de dyld: en la Recovery está en el propio sistema; en un macOS completo viene en el cryptex del sistema (Preboot), y en
# un procesador Haswell o posterior dyld usa la variante x86_64h, como en un Mac.
DYLD_CACHE=
for c in "$ROOT/System/Library/dyld/dyld_shared_cache_x86_64" \
	"$ROOT/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_x86_64h" \
	"$ROOT/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_x86_64"; do
	[ -f "$c" ] && { DYLD_CACHE="$c"; break; }
done
if [ -z "$DYLD_CACHE" ]; then echo "no encuentro el sistema de macOS en: $ROOT (define TAHOE_ROOT)" >&2; exit 1; fi
CACHE="${TAHOE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tahoe-run}"   # el slide de la caché de dyld se calcula una vez
LOG="${TAHOE_LOGFILE:-/tmp/tahoe-shell-$USER.log}"

WITH_LAUNCHD= DEBUG_ENV=()
while [ "$1" = --launchd ] || [ "$1" = --depurar ]; do
	[ "$1" = --launchd ] && WITH_LAUNCHD=1
	[ "$1" = --depurar ] && DEBUG_ENV=(TAHOE_DEBUG_NOREBOOT=1)
	shift
done
[ $# -eq 0 ] && set -- -l

MACOS_ENV=(PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME=/var/root USER=root LOGNAME=root SHELL=/bin/bash
	TERM="${TERM:-xterm-256color}" LANG="${LANG:-es_ES.UTF-8}"
	TAHOE_ROOT="$ROOT" TAHOE_CACHE_DIR="$CACHE" TAHOE_LOGFILE="$LOG" "${DEBUG_ENV[@]}")
SHELL_ARGS=("$DYLD_CACHE" "$ROOT/usr/lib/dyld" "$ROOT/bin/bash" "$@")

if [ -z "$WITH_LAUNCHD" ]; then
	exec env -i "${MACOS_ENV[@]}" "$RUN" "${SHELL_ARGS[@]}"
fi

# PID (en Linux) del launchd de tahoe-run: el proceso tahoe-run que es PID 1 en su espacio de PID.
find_launchd() {
	local p
	for p in $(pgrep -x tahoe-run); do
		[ "$(awk '/^NSpid/{print $NF}' /proc/$p/status 2>/dev/null)" = 1 ] && tr '\0' ' ' < /proc/$p/cmdline 2>/dev/null | grep -q '/sbin/launchd' && { echo "$p"; return; }
	done
}
# Región de puertos que usa ese launchd (la tiene mapeada). Se mira en sus hilos: el principal de launchd termina y
# entonces /proc/<pid>/maps queda vacío.
find_region() { cat /proc/$1/task/*/maps 2>/dev/null | grep -o '/dev/shm/tahoe-ports-[0-9a-f]*' | head -1; }

L=$(find_launchd)
STARTED=
stop_launchd() {
	[ -n "$STARTED" ] || return
	kill -9 "$L" $(pgrep -P "$U" 2>/dev/null) 2>/dev/null   # al morir el PID 1 del espacio, el núcleo termina a todos sus procesos
	touch /dev/shm/tahoe-guard-stop; wait "$GUARD" 2>/dev/null
	rm -f /dev/shm/tahoe-guard-stop
}
trap stop_launchd EXIT
trap 'exit 130' INT TERM HUP                       # para que también se limpie al interrumpir
if [ -z "$L" ]; then
	rm -f /dev/shm/tahoe-guard-stop
	"$TOOLS/mem_guard.sh" & GUARD=$!
	setsid env -i "${MACOS_ENV[@]}" TAHOE_FD2LOG=1 \
		unshare --pid --fork --user --map-root-user --mount-proc \
		nice -n 10 "$RUN" "$DYLD_CACHE" "$ROOT/usr/lib/dyld" "$ROOT/sbin/launchd" \
		< /dev/null > /dev/null 2>&1 &
	U=$! STARTED=1
	for _ in $(seq 50); do L=$(find_launchd); [ -n "$L" ] && [ -n "$(find_region "$L")" ] && break; sleep 0.2; done
	if [ -z "$L" ]; then echo "launchd no arrancó (registro: $LOG)" >&2; touch /dev/shm/tahoe-guard-stop; exit 1; fi
	echo "launchd arrancado (pid $L en Linux); esperando ${TAHOE_BOOT_WAIT:-10} s a los servicios..." >&2
	sleep "${TAHOE_BOOT_WAIT:-10}"
fi
REGION=$(find_region "$L")
if [ -z "$REGION" ]; then echo "no encuentro la región de puertos de launchd (pid $L)" >&2; exit 1; fi

T=$(ls /proc/$L/task | grep -vx "$L" | head -1)       # un hilo vivo: con el principal terminado, /proc/<pid>/ns no se abre
nsenter --target "${T:-$L}" --user --mount --pid --preserve-credentials -- \
	env -i "${MACOS_ENV[@]}" TAHOE_PORTS="$REGION" TAHOE_PORTS_OWNER="${REGION#/dev/shm/tahoe-ports-}" \
	"$RUN" "${SHELL_ARGS[@]}"
