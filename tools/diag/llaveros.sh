#!/bin/bash
# llaveros.sh: prueba de los llaveros en el macOS completo. Crea un llavero de usuario y guarda y lee una contraseña
# (funciona desde el 14 de octubre de 2026), y prueba el llavero del sistema: «systemkeychain -t» (desbloqueo con
# /var/db/SystemKey; funciona desde los firmlinks: «System unlock is working», comprobado el 24 de octubre de 2026) y
# show-keychain-info. Con el PC ahogado (mucha espera de E/S o de swap) los arranques fallan y esta prueba da errores de
# llavero sin que el llavero tenga la culpa: mirar «top» (wa) antes de buscar en el código. Siempre limpia.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"; REPO="$(cd "$DIAG/../.." && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; O="$D/llaveros"; rm -rf "$O"; mkdir -p "$O"
cd "$REPO" || exit 1
TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/completo/macOS}" TAHOE_BOOT_WAIT=10 timeout 400 tools/macos-shell.sh --launchd -c "sleep 60; rm -f /tmp/t.keychain*; \
 security create-keychain -p x /tmp/t.keychain; echo create rc=\$?; \
 security add-generic-password -a a -s s -w w /tmp/t.keychain; echo add rc=\$?; \
 security find-generic-password -s s /tmp/t.keychain > /dev/null; echo find rc=\$?; \
 /usr/sbin/systemkeychain -v -t; echo systemkeychain rc=\$?; \
 security show-keychain-info /Library/Keychains/System.keychain; echo info rc=\$?" > "$O/out" 2>&1
"$DIAG/clean.sh" > /dev/null 2>&1
grep -av "^launchd\|Alarm" "$O/out"
