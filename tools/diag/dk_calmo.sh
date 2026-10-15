#!/bin/bash
# dk_calmo.sh [espera]: diskutil con el sistema ya asentado (no en pleno arranque, que tarda 2–3 min en calmarse).
# Arranca el macOS completo, espera a que launchd responda y luego «espera» segundos más (150), y cronometra órdenes de
# disco (repetidas) con su código de salida, tiempo y número de líneas. Siempre limpia.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"; REPO="$(cd "$DIAG/../.." && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; O="$D/dk_calmo"; rm -rf "$O"; mkdir -p "$O"
cd "$REPO" || exit 1
C="w=0; while ! perl -e \"alarm 8; exec @ARGV\" launchctl print system >/dev/null 2>&1 && [ \$w -lt 180 ]; do sleep 3; w=\$((w+3)); done; echo listo \$w; sleep ${1:-150}
for o in \"diskutil list\" \"diskutil list\" \"diskutil info /\" \"diskutil info /System/Volumes/Data\" \"diskutil apfs list\" \"diskutil info disk1s3\" \"diskutil list\" \"bless --info\" \"bless --info --getBoot\"; do s=\$(perl -MTime::HiRes=time -e \"printf q(%.1f), time\"); perl -e \"alarm 90; exec @ARGV\" sh -c \"\$o\" > /tmp/o.txt 2>&1; r=\$?; e=\$(perl -MTime::HiRes=time -e \"printf q(%.1f), time\"); echo \"rc=\$r t=\$(echo \"\$e - \$s\" | bc) lineas=\$(wc -l < /tmp/o.txt) \$o | \$(grep -m1 -i error /tmp/o.txt)\"; done"
TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/completo/macOS}" TAHOE_BOOT_WAIT=10 timeout 900 tools/macos-shell.sh --depurar --launchd -c "$C" > "$O/out" 2>&1
L=/tmp/tahoe-shell-$USER.log; n=$(grep -a -n "cargado launchd" "$L" | tail -1 | cut -d: -f1); tail -n +"$n" "$L" > "$O/boot.log"
"$DIAG/clean.sh" > /dev/null 2>&1
grep -a "^rc=\|^listo" "$O/out"
