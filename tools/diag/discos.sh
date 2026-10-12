#!/bin/bash
# discos.sh ['orden de diskutil']: diagnóstico de diskutil en un arranque (por omisión «diskutil list»).
#   1. arranca launchd (tools/macos-shell.sh --launchd) y relanza diskarbitrationd con -d y su registro creado
#      (/var/log/diskarbitrationd.log: sin -d no escribe y sin el archivo no lo crea);
#   2. lanza la orden con un plazo (PLAZO s, 40) y anota si terminó y su salida;
#   3. muestra los procesos de discos vivos (diskutil, storagekitd, diskarbitrationd, fskitd, diskmanagementd) y el
#      resumen del registro con da_log.py: qué discos se anunciaron y qué avisos «idle» quedaron sin enviar.
# TAHOE_ROOT: sistema de macOS (por omisión la Recovery). ESPERA: segundos de arranque antes de empezar (15).
# PREP: órdenes de macOS que se ejecutan antes de relanzar diskarbitrationd (para pruebas A/B: restaurar estado de
# arranques antiguos, relanzar otro servicio...). Ejemplo, con una copia de /private/var del volumen de datos en disco
# hecha en la raíz como /.datos-viejos:
#   PREP='cp -Rp /.datos-viejos/folders/. /var/folders/; launchctl kickstart -k system/com.apple.fskitd' discos.sh
# MSGS: programas cuyos mensajes Mach se trazan (/dev/shm/tahoe-msg-trace); al final, mensajes.py dice qué envíos no
# llegaron y qué peticiones se quedaron sin respuesta. Ejemplo:
#   MSGS='diskutil storagekitd diskarbitrationd fskitd extensionkitservice' discos.sh
# IOKS: programas cuya actividad IOKit se traza (/dev/shm/tahoe-iokit-trace: peticiones, respuestas y avisos de
# notificación); al final, iokit.py la resume por proceso. Ejemplo: IOKS=diskarbitrationd SIN_REINICIO=1 discos.sh
# DEPURAR=1: arranque en modo depuración (macos-shell.sh --depurar): recoveryosd no reinicia al fallar. Siempre se
# muestra el final de /var/log/recoveryos.log y si recoveryosd terminó.
# SIN_REINICIO=1: no relanza diskarbitrationd (se queda el del arranque, sin su registro -d; relanzarlo cambia el
# escenario). PAUSA: segundos entre relanzarlo y lanzar la orden (8).
# El registro de este arranque queda en ~/.cache/tahoe-diag/discos.log.
# Todo queda en ~/.cache/tahoe-diag/discos.out (la salida entera de macOS) y siempre se limpia el arranque.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; mkdir -p "$D"
ORDEN="${1:-diskutil list}"; PLAZO="${PLAZO:-40}"; E="${ESPERA:-15}"; PAUSA="${PAUSA:-8}"
export TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/base/macOS Base System}"
DA=system/com.apple.diskarbitrationd
# Dentro de macOS: la orden va en segundo plano y su código de salida se apunta al terminar; si sigue viva al vencer
# el plazo se dice («COLGADA»). Las secciones van marcadas con @@ para separarlas aquí.
SCRIPT="${PREP:+$PREP
}${SIN_REINICIO:+: }touch /var/log/diskarbitrationd.log
${SIN_REINICIO:+: }launchctl debug $DA -- /usr/libexec/diskarbitrationd -d >/dev/null 2>&1
${SIN_REINICIO:+: }launchctl kickstart -k $DA >/dev/null 2>&1
sleep $PAUSA
( $ORDEN > /tmp/.discos.out 2>&1; echo \"@@salida \$?\" >> /tmp/.discos.out ) &
i=0; while [ \$i -lt $PLAZO ] && ! grep -q '^@@salida' /tmp/.discos.out 2>/dev/null; do sleep 1; i=\$((i+1)); done
grep -q '^@@salida' /tmp/.discos.out || echo '@@salida COLGADA' >> /tmp/.discos.out
echo @@procesos; ps axo pid,stat,command | grep -E 'diskutil|storagekitd|diskarbitrationd|fskitd|diskmanagementd' | grep -v grep
echo @@orden; cat /tmp/.discos.out
echo @@recovery; tail -12 /var/log/recoveryos.log
echo @@registro; cat /var/log/diskarbitrationd.log"
LOG="${TAHOE_LOGFILE:-/tmp/tahoe-shell-$USER.log}"
desde=$(( $( [ -f "$LOG" ] && wc -l < "$LOG" || echo 0) + 1 ))      # el registro es común: se toma solo lo de este arranque
if [ -n "$MSGS" ]; then echo "$MSGS" > /dev/shm/tahoe-msg-trace; else rm -f /dev/shm/tahoe-msg-trace; fi
if [ -n "$IOKS" ]; then echo "$IOKS" > /dev/shm/tahoe-iokit-trace; else rm -f /dev/shm/tahoe-iokit-trace; fi
TAHOE_BOOT_WAIT=$E timeout $(( E + PLAZO + 90 )) "$DIAG/../macos-shell.sh" ${DEPURAR:+--depurar} --launchd -c "$SCRIPT" > "$D/discos.out" 2>&1
r=$?
rm -f /dev/shm/tahoe-msg-trace /dev/shm/tahoe-iokit-trace
tail -n +"$desde" "$LOG" > "$D/discos.log" 2>/dev/null
"$DIAG/clean.sh" > /dev/null 2>&1
sec() { awk -v s="@@$1" '$0 == s {on=1; next} /^@@(procesos|orden|recovery|registro)$/ {on=0} on' "$D/discos.out"; }
if ! grep -q '^@@registro$' "$D/discos.out"; then
	echo "el arranque no llegó a la orden (código $r):"; tail -5 "$D/discos.out"; exit 1
fi
[ -n "$PREP" ] && echo "== preparación: $PREP"
echo "== $ORDEN: $(grep -o '^@@salida .*' "$D/discos.out" | sed 's/@@salida /salida /')"
sec orden | grep -v '^@@salida' | head -15 | sed 's/^/   /'
echo "== recoveryosd: $(grep -ao 'fin <[0-9]*> \[recoveryosd\] [^ ]*' "$D/discos.log" | tail -1 || true)"
sec recovery | sed 's/^/   /'
echo "== procesos de discos vivos al final"
sec procesos | sed 's/^/   /'
if [ -n "$SIN_REINICIO" ]; then echo "== (SIN_REINICIO: diskarbitrationd sin registro -d)"
elif [ -z "$(sec registro)" ]; then echo "== el registro -d de diskarbitrationd salió vacío (¿no llegó a relanzarse?)"
else sec registro | python3 "$DIAG/da_log.py"; fi
[ -n "$MSGS" ] && python3 "$DIAG/mensajes.py" "$D/discos.log"
[ -n "$IOKS" ] && python3 "$DIAG/iokit.py" "$D/discos.log"
echo "(salida completa: $D/discos.out; registro: $D/discos.log)"
