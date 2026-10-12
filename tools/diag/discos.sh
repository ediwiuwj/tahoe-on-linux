#!/bin/bash
# discos.sh ['orden de diskutil']: diagnóstico de diskutil en un arranque (por omisión «diskutil list»).
#   1. arranca launchd (tools/macos-shell.sh --launchd) y relanza diskarbitrationd con -d y su registro creado
#      (/var/log/diskarbitrationd.log: sin -d no escribe y sin el archivo no lo crea);
#   2. lanza la orden con un plazo (PLAZO s, 40) y anota si terminó y su salida;
#   3. muestra los procesos de discos vivos (diskutil, storagekitd, diskarbitrationd, fskitd, diskmanagementd) y el
#      resumen del registro con da_log.py: qué discos se anunciaron y qué avisos «idle» quedaron sin enviar.
# TAHOE_ROOT: sistema de macOS (por omisión la Recovery). ESPERA: segundos de arranque antes de empezar (15).
# Todo queda en ~/.cache/tahoe-diag/discos.out (la salida entera de macOS) y siempre se limpia el arranque.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; mkdir -p "$D"
ORDEN="${1:-diskutil list}"; PLAZO="${PLAZO:-40}"; E="${ESPERA:-15}"
export TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/base/macOS Base System}"
DA=system/com.apple.diskarbitrationd
# Dentro de macOS: la orden va en segundo plano y su código de salida se apunta al terminar; si sigue viva al vencer
# el plazo se dice («COLGADA»). Las secciones van marcadas con @@ para separarlas aquí.
SCRIPT="touch /var/log/diskarbitrationd.log
launchctl debug $DA -- /usr/libexec/diskarbitrationd -d >/dev/null 2>&1
launchctl kickstart -k $DA >/dev/null 2>&1
sleep 8
( $ORDEN > /tmp/.discos.out 2>&1; echo \"@@salida \$?\" >> /tmp/.discos.out ) &
i=0; while [ \$i -lt $PLAZO ] && ! grep -q '^@@salida' /tmp/.discos.out 2>/dev/null; do sleep 1; i=\$((i+1)); done
grep -q '^@@salida' /tmp/.discos.out || echo '@@salida COLGADA' >> /tmp/.discos.out
echo @@procesos; ps axo pid,stat,command | grep -E 'diskutil|storagekitd|diskarbitrationd|fskitd|diskmanagementd' | grep -v grep
echo @@orden; cat /tmp/.discos.out
echo @@registro; cat /var/log/diskarbitrationd.log"
TAHOE_BOOT_WAIT=$E timeout $(( E + PLAZO + 90 )) "$DIAG/../macos-shell.sh" --launchd -c "$SCRIPT" > "$D/discos.out" 2>&1
r=$?
"$DIAG/clean.sh" > /dev/null 2>&1
sec() { awk -v s="@@$1" '$0 == s {on=1; next} /^@@(procesos|orden|registro)$/ {on=0} on' "$D/discos.out"; }
if ! grep -q '^@@registro$' "$D/discos.out"; then
	echo "el arranque no llegó a la orden (código $r):"; tail -5 "$D/discos.out"; exit 1
fi
echo "== $ORDEN: $(grep -o '^@@salida .*' "$D/discos.out" | sed 's/@@salida /salida /')"
sec orden | grep -v '^@@salida' | head -15 | sed 's/^/   /'
echo "== procesos de discos vivos al final"
sec procesos | sed 's/^/   /'
sec registro | python3 "$DIAG/da_log.py"
echo "(salida completa: $D/discos.out)"
