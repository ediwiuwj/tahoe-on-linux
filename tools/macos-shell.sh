#!/bin/bash
# Abre la shell de macOS (el bash del sistema base de macOS 26) en esta terminal, ejecutada sobre tahoe-run.
#
#   tools/macos-shell.sh                          # shell interactiva de login, sin launchd
#   tools/macos-shell.sh -c 'sw_vers'             # una orden
#   tools/macos-shell.sh --launchd                # shell unida a launchd (launchctl, defaults... hablan con sus servicios)
#   tools/macos-shell.sh --launchd -c 'launchctl list'
#   tools/macos-shell.sh --launchd --depurar      # modo depuración (ver abajo)
#   tools/macos-shell.sh --launchd --pantalla     # además abre el visor de la pantalla de macOS (tools/tahoe-view)
#
# TAHOE_RUN: binario de tahoe-run a usar (por omisión, el del repo).
# TAHOE_NICE: prioridad de macOS en Linux (nice, por omisión 0, la normal; p. ej. 10 para que moleste menos al escritorio).
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
#
# --pantalla: abre el visor tools/tahoe-view (Rust; compílalo con `cargo build --release` en esa carpeta), que muestra en una ventana la
# memoria de vídeo emulada en la que dibuja WindowServer (/dev/shm/tahoe-fb-<id>). Implica --launchd.
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
# El registro crece con cada arranque (las herramientas toman de él solo el último) y /tmp suele ser tmpfs, es decir, RAM:
# pasado TAHOE_LOG_MAX_MB (300) se vacía antes de arrancar. Lleno, /tmp daba «Disk quota exceeded» y no arrancaba nada.
if [ -f "$LOG" ] && [ "$(stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt $(( ${TAHOE_LOG_MAX_MB:-300} * 1024 * 1024 )) ]; then : > "$LOG"; fi

WITH_LAUNCHD= DEBUG_ENV=()
while [ "$1" = --launchd ] || [ "$1" = --depurar ] || [ "$1" = --pantalla ]; do
	[ "$1" = --launchd ] && WITH_LAUNCHD=1
	[ "$1" = --pantalla ] && WITH_LAUNCHD=1 WITH_VIEW=1
	[ "$1" = --depurar ] && DEBUG_ENV=(TAHOE_DEBUG_NOREBOOT=1)
	shift
done
[ $# -eq 0 ] && set -- -l

# Clave OSK del SMC (64 bytes) para descifrar los programas protegidos de macOS (loginwindow, Finder, Dock...): no va en el
# repositorio; si existe ~/.config/tahoe-run/osk (o TAHOE_OSK_FILE), tahoe-run la usa (ver tahoe-run/emu_dsmos.c).
OSK="${TAHOE_OSK_FILE:-$HOME/.config/tahoe-run/osk}"
OSK_ENV=(); [ -r "$OSK" ] && OSK_ENV=(TAHOE_OSK_FILE="$(readlink -f "$OSK")")
# Identidad del equipo (modelo, número de serie, MLB, UUID, ROM) generada con macserial por tools/smbios.sh: tampoco va en el
# repositorio. Sin ella, tahoe-run se presenta como un MacBookPro16,1 ficticio.
SMBIOS="${TAHOE_SMBIOS_FILE:-$HOME/.config/tahoe-run/smbios.json}"
[ -r "$SMBIOS" ] && OSK_ENV+=(TAHOE_SMBIOS_FILE="$(readlink -f "$SMBIOS")")
# Argumentos de arranque de macOS (NVRAM boot-args y kern.bootargs), p. ej. TAHOE_BOOTARGS="useIOP=0".
[ -n "$TAHOE_BOOTARGS" ] && OSK_ENV+=(TAHOE_BOOTARGS="$TAHOE_BOOTARGS")
[ -n "$TAHOE_GPU" ] && OSK_ENV+=(TAHOE_GPU="$TAHOE_GPU")          # otra identidad PCI para la GPU emulada
# Camino acelerado de la pantalla (AGDC/IOPresentment): con TAHOE_AGDC=1 el GPUWrangler da el ID del acelerador
# emulado, con lo que CoreDisplay intenta presentar por la GPU en vez de por software (ver emu_iokit.inc).
[ -n "$TAHOE_AGDC" ] && OSK_ENV+=(TAHOE_AGDC=1)
# Con TAHOE_FB_INTEL=1 el framebuffer emulado se presenta como el de una Intel, con lo que CoreDisplay usa el camino
# AGDC/IOPresentment en vez del clásico por software (ver emu_iokit.inc).
[ -n "$TAHOE_FB_INTEL" ] && OSK_ENV+=(TAHOE_FB_INTEL=1)
# Driver Metal (tahoe-metal): carpeta del anfitrión con TahoeMetal.bundle; con ella macOS ve un IOAccelerator con ese driver.
# Con él arranca también el servidor de GPU de Linux (tahoe-gpu servir, Vulkan), al que el driver manda los comandos por
# el socket TAHOE_GPU_SOCK (por omisión /dev/shm/tahoe-gpu-<uid>.sock).
GPU_SRV=
if [ -n "$TAHOE_METAL" ]; then
	OSK_ENV+=(TAHOE_METAL="$(readlink -f "$TAHOE_METAL")")
	TAHOE_GPU_SOCK="${TAHOE_GPU_SOCK:-/dev/shm/tahoe-gpu-$(id -u).sock}"
	OSK_ENV+=(TAHOE_GPU_SOCK="$TAHOE_GPU_SOCK")
fi
MACOS_ENV=(PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME=/var/root USER=root LOGNAME=root SHELL=/bin/bash
	TERM="${TERM:-xterm-256color}" LANG="${LANG:-es_ES.UTF-8}"
	TAHOE_ROOT="$ROOT" TAHOE_CACHE_DIR="$CACHE" TAHOE_LOGFILE="$LOG" "${DEBUG_ENV[@]}" "${OSK_ENV[@]}")
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
	[ -n "$VIEW" ] && kill "$VIEW" 2>/dev/null
	[ -n "$GPU_SRV" ] && kill "$GPU_SRV" 2>/dev/null && rm -f "$TAHOE_GPU_SOCK"
	if [ -n "$REGION" ]; then                         # memoria de vídeo y superficies de este arranque (RAM en /dev/shm)
		local id=${REGION#/dev/shm/tahoe-ports-}
		rm -f "/dev/shm/tahoe-fb-$id" "/dev/shm/tahoe-fbcursor-$id" "/dev/shm/tahoe-surfaces-$id" /dev/shm/tahoe-surf-*-"$id"
	fi
	touch /dev/shm/tahoe-guard-stop; wait "$GUARD" 2>/dev/null
	rm -f /dev/shm/tahoe-guard-stop
}
trap stop_launchd EXIT
trap 'exit 130' INT TERM HUP                       # para que también se limpie al interrumpir
start_gpu_server() {
	[ -n "$TAHOE_METAL" ] || return
	local srv="$TOOLS/../tahoe-gpu/target/release/tahoe-gpu"
	if [ ! -x "$srv" ]; then echo "falta $srv: compila tahoe-gpu (cargo build --release)" >&2; return; fi
	rm -f "$TAHOE_GPU_SOCK"
	"$srv" servir "$TAHOE_GPU_SOCK" < /dev/null >> "$LOG.gpu" 2>&1 & GPU_SRV=$!
}
if [ -z "$L" ]; then
	start_gpu_server
	rm -f /dev/shm/tahoe-guard-stop
	"$TOOLS/mem_guard.sh" & GUARD=$!
	setsid env -i "${MACOS_ENV[@]}" TAHOE_FD2LOG=1 \
		unshare --pid --fork --user --map-root-user --mount-proc \
		nice -n "${TAHOE_NICE:-0}" "$RUN" "$DYLD_CACHE" "$ROOT/usr/lib/dyld" "$ROOT/sbin/launchd" \
		< /dev/null > /dev/null 2>&1 &
	U=$! STARTED=1
	for _ in $(seq 300); do L=$(find_launchd); [ -n "$L" ] && REGION=$(find_region "$L") && [ -n "$REGION" ] && break; sleep 0.2; done
	if [ -z "$L" ] || [ -z "$REGION" ]; then echo "launchd no arrancó (registro: $LOG)" >&2; touch /dev/shm/tahoe-guard-stop; exit 1; fi
	echo "launchd arrancado (pid $L en Linux); esperando ${TAHOE_BOOT_WAIT:-10} s a los servicios..." >&2
	sleep "${TAHOE_BOOT_WAIT:-10}"
	# la región se apunta antes de esperar: si launchd muere en la espera (un abort suyo), se dice eso y se limpia igual
	if [ -z "$(find_region "$L")" ]; then
		echo "launchd (pid $L) terminó durante la espera; mira el final de $LOG (abort, assertion)" >&2; exit 1
	fi
else
	REGION=$(find_region "$L")
	if [ -z "$REGION" ]; then echo "no encuentro la región de puertos de launchd (pid $L)" >&2; exit 1; fi
fi

if [ -n "$WITH_VIEW" ]; then
	VIEWER="$TOOLS/tahoe-view/target/release/tahoe-view"
	if [ -x "$VIEWER" ]; then
		"$VIEWER" "/dev/shm/tahoe-fb-${REGION#/dev/shm/tahoe-ports-}" < /dev/null > /dev/null 2>&1 & VIEW=$!
	else
		echo "falta el visor: compílalo con «cargo build --release» en tools/tahoe-view" >&2
	fi
fi

T=$(ls /proc/$L/task | grep -vx "$L" | head -1)       # un hilo vivo: con el principal terminado, /proc/<pid>/ns no se abre
# TAHOE_TRACE_SHELL=1: traza completa de las llamadas al sistema solo de la shell y de lo que lance (no de todo el arranque)
TRACE_ENV=(); [ -n "$TAHOE_TRACE_SHELL" ] && TRACE_ENV=(TAHOE_TRACE=1)
nsenter --target "${T:-$L}" --user --mount --pid --preserve-credentials -- \
	env -i "${MACOS_ENV[@]}" "${TRACE_ENV[@]}" TAHOE_PORTS="$REGION" TAHOE_PORTS_OWNER="${REGION#/dev/shm/tahoe-ports-}" \
	"$RUN" "${SHELL_ARGS[@]}"
