#!/bin/bash
# inventario_total.sh: ejecuta TODOS los programas de /bin, /sbin, /usr/bin y /usr/sbin del macOS (por omisión el
# completo, ~/macos26/completo/macOS) dentro de la shell de macOS unida a launchd, uno a uno, sin argumentos, con la
# entrada en /dev/null y un plazo (PLAZO s, 6). Cada resultado se escribe al momento en <raíz>/private/var/tmp/inventario.txt
# («n|programa|pid|código|segundos|primera línea de la salida»), así que si el arranque se corta (guardián de memoria) se
# retoma con DESDE=<n> sin perder lo hecho. Al final, tools/diag/inventario.py lo clasifica con el registro de tahoe-run.
#   ESPERA: segundos de arranque antes de empezar (180). PLAZO: segundos por programa (6). DESDE: primer número (1).
#   TAHOE_RUN: otro binario de tahoe-run. Siempre limpia.
# Se saltan los que acaban o alteran el arranque (reboot, halt, shutdown, killall, pkill...) y los servicios de launchd del propio sistema
# (su programa está en un plist de LaunchDaemons/LaunchAgents): otra copia sin argumentos estorbaría a la que ya corre.
TOOLS="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; mkdir -p "$D"
export TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/completo/macOS}"
R="$TAHOE_ROOT"; E="${ESPERA:-180}"; PLAZO="${PLAZO:-6}"; DESDE="${DESDE:-1}"
LISTA="$R/private/var/tmp/inventario-lista.txt"; OUT="$R/private/var/tmp/inventario.txt"
[ -d "$R/private/var/tmp" ] || { echo "no encuentro $R/private/var/tmp" >&2; exit 1; }
# Lista: programas ejecutables, menos los saltados.
python3 - "$R" > "$LISTA" <<'EOF'
import glob, os, plistlib, sys
R = sys.argv[1]
salta = {"/sbin/reboot", "/sbin/halt", "/sbin/shutdown", "/sbin/fastboot", "/sbin/fasthalt", "/sbin/launchd",
         "/sbin/init", "/usr/sbin/sshd", "/usr/sbin/httpd", "/usr/sbin/apachectl", "/usr/sbin/postfix",
         "/usr/bin/ssh-agent", "/usr/sbin/newsyslog", "/usr/bin/killall", "/usr/bin/pkill"}
normales = {"/bin/bash", "/usr/bin/true", "/usr/bin/csrutil", "/sbin/pfctl", "/sbin/mount_nfs", "/usr/sbin/asr",
            "/usr/sbin/vsdbutil"}
for f in glob.glob(R + "/System/Library/LaunchDaemons/*.plist") + glob.glob(R + "/System/Library/LaunchAgents/*.plist"):
    try:
        p = plistlib.load(open(f, "rb"))
    except Exception:
        continue
    prog = p.get("Program") or (p.get("ProgramArguments") or [None])[0]
    if prog and prog not in normales:
        salta.add(prog)
for d in ("/bin", "/sbin", "/usr/bin", "/usr/sbin"):
    for n in sorted(os.listdir(R + d)):
        g = d + "/" + n
        h = R + g
        if os.path.isfile(h) and not os.path.islink(h) and os.access(h, os.X_OK):
            print(("SALTA " if g in salta else "") + g)
EOF
total=$(wc -l < "$LISTA")
[ "$DESDE" = 1 ] && : > "$OUT"
echo "inventario: $total programas (desde el $DESDE), plazo ${PLAZO}s; resultados en $OUT"
# Dentro de macOS: cada programa en su propio grupo de procesos (set -m) para poder matarlo con sus hijos.
SCRIPT="set -m; i=0
while read -r l; do
 i=\$((i+1)); [ \$i -lt $DESDE ] && continue
 case \"\$l\" in SALTA\\ *) echo \"\$i|\${l#SALTA }|-|saltada|0|\" >> /private/var/tmp/inventario.txt; continue;; esac
 f=/private/var/tmp/.inv.out; rm -f \$f
 t0=\$SECONDS
 ( \"\$l\" < /dev/null > \$f 2>&1 ) & p=\$!
 k=0; while kill -0 \$p 2>/dev/null && [ \$k -lt $((PLAZO * 5)) ]; do sleep 0.2; k=\$((k+1)); done
 if kill -0 \$p 2>/dev/null; then kill -9 -- -\$p 2>/dev/null; kill -9 \$p 2>/dev/null; wait \$p 2>/dev/null; r=COLGADA
 else wait \$p; r=\$?; fi
 s=\$(head -c 2000 \$f 2>/dev/null | tr -d '\\r' | grep -v '^\\s*\$' | head -1 | tr '|' '/' | cut -c1-160)
 echo \"\$i|\$l|\$p|\$r|\$((SECONDS-t0))|\$s\" >> /private/var/tmp/inventario.txt
done < /private/var/tmp/inventario-lista.txt
echo @@fin"
TAHOE_BOOT_WAIT=$E timeout -s TERM -k 10 $(( E + total * (PLAZO + 3) + 300 )) \
	"$TOOLS/macos-shell.sh" --depurar --launchd -c "$SCRIPT" > "$D/inventario_total.out" 2>&1
LOG="${TAHOE_LOGFILE:-/tmp/tahoe-shell-$USER.log}"
n=$(LC_ALL=C grep -an "<1> cargado launchd " "$LOG" | tail -1 | cut -d: -f1)
[ -n "$n" ] && tail -n +"$n" "$LOG" > "$D/inventario_total.log"
cp "$OUT" "$D/inventario_total.txt" 2>/dev/null
"$TOOLS/diag/clean.sh" > /dev/null 2>&1
grep -a "guardián" "$D/inventario_total.out" | head -1
echo "hechos: $(wc -l < "$OUT") de $total (si faltan, sigue con DESDE=$(( $(wc -l < "$OUT") + 1 )))"
python3 "$TOOLS/diag/inventario.py" "$D/inventario_total.txt" "$D/inventario_total.log"
