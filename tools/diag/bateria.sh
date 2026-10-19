#!/bin/bash
# bateria.sh [binario_anterior]: la batería fija que se pasa antes de dar por bueno un cambio. Con binario_anterior, antes
# hace la comparación A/B del inventario corto (ab_recorrido.sh). Luego, con el tahoe-run del repo:
#   1. diskutil list en la Recovery (discos.sh): debe decir «VEREDICTO: correcto» (cuelga a veces también sin cambios);
#   2. el inventario corto (shell_survey.sh, 79 órdenes) en el macOS completo, dos veces: el arranque varía mucho;
#   3. la memoria de WindowServer (memoria.sh, ~76 MB estable);
#   4. cargo test --lib en tahoe-run/nucleo.
# Salida en $TAHOE_DIAG_DIR/bateria (por omisión ~/.cache/tahoe-diag/bateria) y un resumen al final. Siempre limpia.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"; REPO="$(cd "$DIAG/../.." && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; O="$D/bateria"; rm -rf "$O"; mkdir -p "$O"
COMPLETO="${TAHOE_COMPLETO:-$HOME/macos26/completo/macOS}"
cd "$REPO" || exit 1
[ -n "$1" ] && "$DIAG/ab_recorrido.sh" "$1" > "$O/ab.txt" 2>&1
DEPURAR=1 "$DIAG/discos.sh" > "$O/discos.txt" 2>&1; "$DIAG/clean.sh" > /dev/null 2>&1
for i in 1 2; do TAHOE_ROOT="$COMPLETO" DEPURAR=1 tools/shell_survey.sh > "$O/recorrido$i.txt" 2>&1; "$DIAG/clean.sh" > /dev/null 2>&1; done
TAHOE_ROOT="$COMPLETO" DEPURAR=1 "$DIAG/memoria.sh" WindowServer 60 120 > "$O/memoria.txt" 2>&1; "$DIAG/clean.sh" > /dev/null 2>&1
(cd tahoe-run/nucleo && cargo test --lib 2>&1 | grep "test result") > "$O/cargo.txt"
[ -s "$O/ab.txt" ] && cat "$O/ab.txt"
grep -a -m1 "VEREDICTO\|COLGADA" "$O/discos.txt"
for i in 1 2; do echo "recorrido $i: OK=$(grep -c '^OK' "$O/recorrido$i.txt") COLGADA=$(grep -c '^COLGADA' "$O/recorrido$i.txt") ERR=$(grep -c '^ERR' "$O/recorrido$i.txt")"; done
grep "t=1[01][05]" "$O/memoria.txt" | tail -2
cat "$O/cargo.txt"
echo "procesos tahoe-run vivos al final: $(pgrep -c -x tahoe-run)"
