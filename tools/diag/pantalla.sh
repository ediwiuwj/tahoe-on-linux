#!/bin/bash
# pantalla.sh [segundos] [cada]: arranca el macOS completo (launchd, sin orden) durante <segundos> (300) y cada <cada> s
# (30) guarda la pantalla emulada en PNG (captura.py) y anota qué procesos gráficos están vivos (WindowServer,
# loginwindow, Dock, Finder, SystemUIServer...). Sirve para ver hasta dónde llega el escritorio sin abrir el visor.
#   TAHOE_ROOT: el macOS (por omisión el completo); TAHOE_RUN: otro binario; DEPURAR=1: modo depuración.
# Salida en $TAHOE_DIAG_DIR/pantalla (por omisión ~/.cache/tahoe-diag/pantalla): t<seg>.png, pantalla.txt y el registro
# de este arranque (pantalla.log). Siempre limpia.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; O="$D/pantalla"; rm -rf "$O"; mkdir -p "$O"
export TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/completo/macOS}"
T="${1:-300}"; C="${2:-30}"
LOG="${TAHOE_LOGFILE:-/tmp/tahoe-shell-$USER.log}"
if [ -f "$LOG" ] && [ "$(stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt $(( ${TAHOE_LOG_MAX_MB:-300} * 1024 * 1024 )) ]; then : > "$LOG"; fi
desde=$(( $( [ -f "$LOG" ] && wc -l < "$LOG" || echo 0) + 1 ))
( TAHOE_BOOT_WAIT=5 timeout $(( T + 60 )) "$DIAG/../macos-shell.sh" ${DEPURAR:+--depurar} --launchd -c "sleep $(( T + 30 ))" \
	> "$O/arranque.out" 2>&1 & )
GRAF="WindowServer|loginwindow|Dock|Finder|SystemUIServer|ControlCenter|WindowManager|NotificationCenter|Language Chooser|Setup Assistant|universalaccessd"
for (( t = C; t <= T; t += C )); do
	sleep "$C"
	# programas gráficos cargados en este arranque y si siguen vivos (el registro dice «cargado» y «fin <pid>»)
	vivos=$(tail -n +"$desde" "$LOG" 2>/dev/null | LC_ALL=C grep -aoE "<[0-9]+> cargado ($GRAF) |fin <[0-9]+>" |
		awk '/cargado/ {gsub(/[<>]/,"",$1); p[$1]=$3} /^fin/ {gsub(/[<>]/,"",$2); delete p[$2]} END {for (k in p) printf "%s(%s) ", p[k], k}')
	cap=$(python3 "$DIAG/captura.py" "$O/t$t.png" 2>&1 | sed 's/ (.*//')
	echo "t=$t: $cap | vivos: ${vivos:-ninguno}" | tee -a "$O/pantalla.txt"
	pgrep -x tahoe-run > /dev/null || { echo "t=$t: el arranque terminó" | tee -a "$O/pantalla.txt"; break; }
done
tail -n +"$desde" "$LOG" > "$O/pantalla.log" 2>/dev/null
"$DIAG/clean.sh" > /dev/null 2>&1
grep -a "guardián" "$O/arranque.out" | head -1
