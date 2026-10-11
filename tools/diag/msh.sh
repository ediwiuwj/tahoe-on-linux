#!/bin/bash
# msh.sh '<órdenes de macOS>' [espera_arranque] [límite]: ejecuta órdenes en la shell de macOS unida a launchd
# (tools/macos-shell.sh --launchd), imprime su salida y SIEMPRE limpia al terminar (clean.sh).
#   espera_arranque: segundos que se deja arrancar a launchd antes de la orden (12); límite: segundos en total (60).
# El registro de tahoe-run queda en $TAHOE_DIAG_DIR/msh.log (por omisión ~/.cache/tahoe-diag).
#
# Diagnóstico opcional (variables de entorno; launchd no propaga el entorno, por eso son archivos de /dev/shm):
#   STACKS=<proceso>  errores de sus syscalls con ruta, pilas al bloquearse o caer
#   ALL=1             con STACKS: todas sus syscalls y trampas Mach
#   MACH=1            traza de mensajes Mach, workloops e hilos de las colas (MT, WL, THR)
#   IOK=1             traza de IOKit
#   CONSOLA=1         lo que se escribe en /dev/console (launchd anota ahí sus errores) se imprime al final
#   BRK='dir dir...'  int3 en esas direcciones (hex) del proceso de STACKS: registra los registros al pasar (BRK)
#   PEEK='expr...'    con BRK: lee 8 bytes en cada dirección o registro+desplazamiento (p. ej. rdi+0x50); con * delante
#                     (*rcx+0x10) muestra además el texto al que apunta ese valor (cadenas C, CFString constantes)
#   TAHOE_RUN=<bin>   otro binario de tahoe-run (comparar versiones)
#   TAHOE_DEPURAR=0   arranque normal; por omisión se usa el modo depuración (macos-shell.sh --depurar), en el que
#                     recoveryosd no reinicia si falla, para poder seguir mirando)
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; mkdir -p "$D"
rm -f "$D/msh.log" /dev/shm/tahoe-break /dev/shm/tahoe-peek /dev/shm/tahoe-stacks /dev/shm/tahoe-stacks-all /dev/shm/tahoe-mach-trace /dev/shm/tahoe-iokit-trace /dev/shm/tahoe-console
[ -n "$STACKS" ] && echo "$STACKS" > /dev/shm/tahoe-stacks
[ -n "$ALL" ] && touch /dev/shm/tahoe-stacks-all
[ -n "$MACH" ] && touch /dev/shm/tahoe-mach-trace
[ -n "$IOK" ] && touch /dev/shm/tahoe-iokit-trace
[ -n "$CONSOLA" ] && : > /dev/shm/tahoe-console
[ -n "$BRK" ] && printf "%s\n" $BRK > /dev/shm/tahoe-break
[ -n "$PEEK" ] && printf "%s\n" $PEEK > /dev/shm/tahoe-peek
TAHOE_RUN="${TAHOE_RUN:-}" TAHOE_LOGFILE="$D/msh.log" TAHOE_BOOT_WAIT="${2:-12}" timeout -s TERM -k 5 "${3:-60}" nice -n 10 \
	"$DIAG/../macos-shell.sh" --launchd $([ "${TAHOE_DEPURAR:-1}" = 0 ] || echo --depurar) -c "$1" < /dev/null 2>&1
echo "[salida=$?]"
[ -n "$CONSOLA" ] && { echo "--- consola ---"; cat /dev/shm/tahoe-console; cp /dev/shm/tahoe-console "$D/consola.log"; rm -f /dev/shm/tahoe-console; }
"$DIAG/clean.sh"
