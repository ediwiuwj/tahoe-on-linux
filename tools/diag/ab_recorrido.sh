#!/bin/bash
# ab_recorrido.sh <binario_A> [veces]: compara dos tahoe-run con el inventario corto (shell_survey.sh) en el macOS
# completo, alternando A (el binario dado: la versión anterior) y B (el del repo), 2 veces por omisión. El arranque varía
# mucho de una vez a otra (54–62 de 79 con el mismo binario): solo cuenta una diferencia que se repita.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"; REPO="$(cd "$DIAG/../.." && pwd)"
A="${1:?indica el binario anterior de tahoe-run}"; N="${2:-2}"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; O="$D/ab_recorrido"; rm -rf "$O"; mkdir -p "$O"
COMPLETO="${TAHOE_COMPLETO:-$HOME/macos26/completo/macOS}"
cd "$REPO" || exit 1
for i in $(seq "$N"); do
	TAHOE_RUN="$A" TAHOE_ROOT="$COMPLETO" DEPURAR=1 tools/shell_survey.sh > "$O/A$i.txt" 2>&1; "$DIAG/clean.sh" > /dev/null 2>&1; sleep 5
	TAHOE_ROOT="$COMPLETO" DEPURAR=1 tools/shell_survey.sh > "$O/B$i.txt" 2>&1; "$DIAG/clean.sh" > /dev/null 2>&1; sleep 5
done
for f in "$O"/A*.txt "$O"/B*.txt; do echo "$(basename "$f" .txt): OK=$(grep -c '^OK' "$f") COLGADA=$(grep -c '^COLGADA' "$f") ERR=$(grep -c '^ERR' "$f")"; done
