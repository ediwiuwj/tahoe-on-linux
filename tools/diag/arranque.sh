#!/bin/bash
# arranque.sh [segundos]: arranca el macOS completo sin trazas durante esos segundos (240) y dice cuántos programas se
# cargan por cada 10 s y cuáles se cargan más veces (los servicios que se relanzan en bucle salen arriba).
# Necesita el registro con hora: aplicar antes docs/parches-sin-validar/log-con-hora.patch. Siempre limpia.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"; REPO="$(cd "$DIAG/../.." && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; O="$D/arranque"; rm -rf "$O"; mkdir -p "$O"
cd "$REPO" || exit 1
touch /dev/shm/tahoe-log-time
TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/completo/macOS}" TAHOE_BOOT_WAIT=10 timeout 600 tools/macos-shell.sh --launchd -c "sleep ${1:-240}" > "$O/out" 2>&1
rm -f /dev/shm/tahoe-log-time
L=/tmp/tahoe-shell-$USER.log; n=$(grep -a -n "cargado launchd" "$L" | tail -1 | cut -d: -f1); tail -n +"$n" "$L" > "$O/boot.log"
"$DIAG/clean.sh" > /dev/null 2>&1
python3 - "$O/boot.log" <<'PY'
import re, sys, collections
t0 = None; h = collections.Counter(); progs = collections.defaultdict(list)
for ln in open(sys.argv[1], errors="replace"):
    m = re.match(r"\[([0-9.]+)\] tahoe-run: <(\d+)> cargado (\S+)", ln)
    if not m: continue
    t = float(m.group(1)); t0 = t0 or t; p = m.group(3)
    if p in ("dyld", "xpcproxy"): continue
    h[int((t - t0) // 10)] += 1; progs[p].append(round(t - t0))
if not h: sys.exit("el registro no tiene hora: aplicar docs/parches-sin-validar/log-con-hora.patch")
print("programas cargados por cada 10 s:", " ".join("%d:%d" % (k * 10, h[k]) for k in sorted(h)))
print("total", sum(h.values()), "distintos", len(progs))
for p, v in sorted(progs.items(), key=lambda x: -len(x[1]))[:15]: print("%4d  %-30s %s" % (len(v), p, v[:12]))
PY
