#!/bin/bash
# cola.sh <salida> <herramienta de tools/diag> [argumentos...]: espera a que no quede ningún arranque de tahoe-run ni otra
# herramienta de diagnóstico en marcha y lanza la indicada en segundo plano, con su salida en <salida>. Dos arranques a la
# vez se pisan (región de puertos, memoria de la PC), así que los diagnósticos se encolan con esto, uno detrás de otro.
# Las variables de entorno (STACKS, IOK, ESPERA, TAHOE_ROOT...) pasan tal cual.
#   tools/diag/cola.sh /tmp/ws.out hang.sh true 'WindowServer hidd' 40
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
if [ "$1" != --ya ]; then                                   # desligado de la terminal (o de ssh): sobrevive al cerrarla
	[ -x "$DIAG/$2" ] || { echo "no existe tools/diag/$2" >&2; exit 1; }
	setsid -f "$0" --ya "$@" > "$1" 2>&1 < /dev/null
	echo "encolado: $2 (salida en $1)"; exit 0
fi
shift; tool=$2; shift 2
# el patrón con [h] no se encuentra a sí mismo en la línea de órdenes de este bucle
while pgrep -x tahoe-run >/dev/null || pgrep -f "tools/diag/(msh|hang)\.s[h] " >/dev/null; do sleep 3; done
cd "$DIAG/../.." && exec "$DIAG/$tool" "$@"
