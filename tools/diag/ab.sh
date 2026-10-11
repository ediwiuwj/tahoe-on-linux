#!/bin/bash
# ab.sh <N> [binario]: N arranques de la shell unida a launchd (orden: sleep 8) y, por cada uno, el número de procesos que
# llegaron a cargarse y las caídas por proceso y señal. Sirve para comparar dos versiones de tahoe-run antes de dar por
# bueno un cambio:  ab.sh 4 /ruta/al/tahoe-run-anterior ; ab.sh 4
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; L="$D/msh.log"
G="env LC_ALL=C grep -a"
for i in $(seq "$1"); do
	TAHOE_RUN="${2:-}" "$DIAG/msh.sh" "sleep 8" 12 40 > /dev/null 2>&1
	c=$(for l in $($G -n "^ *SIG[A-Z]* <[0-9]*>" "$L" | $G -v SIGSYS | cut -d: -f1); do
		p=$(sed -n "${l}p" "$L" | sed 's/.*<\([0-9]*\)>.*/\1/'); s=$(sed -n "${l}p" "$L" | awk '{print $1}')
		n=$($G "<$p> cargado" "$L" | $G -v " dyld " | tail -1 | awk '{print $4}'); echo "$n:$s"; done | sort | uniq -c | awk '{printf "%s×%s ", $2, $1}')
	echo "corrida $i: procesos=$($G -c "cargado dyld" "$L") caídas: $c"
done
