#!/bin/bash
# preconfigurar.sh: configura el macOS completo como lo hacen las empresas (usuario administrador, idioma y región, sin
# Asistente de configuración), o lo deshace. Ver tools/preconfigurar.py.
#
#   tools/preconfigurar.sh aplicar <usuario> ["Nombre Completo"] [idioma, p. ej. es_MX]
#   tools/preconfigurar.sh deshacer
#
# TAHOE_ROOT: el macOS (por omisión ~/macos26/completo/macOS). La contraseña se pide al aplicar y no se guarda.
# Se ejecuta como root dentro de un espacio de usuarios (como tahoe-run): parte de dslocal no la puede leer el usuario de
# Linux. Con macOS arrancado no se toca nada.
TOOLS="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
ROOT="${TAHOE_ROOT:-$HOME/macos26/completo/macOS}"
if pgrep -x tahoe-run > /dev/null; then echo "hay un macOS arrancado (tahoe-run): ciérralo antes" >&2; exit 1; fi
[ -d "$ROOT/private/var/db/dslocal" ] || { echo "no encuentro un macOS en $ROOT (define TAHOE_ROOT)" >&2; exit 1; }
exec unshare --user --map-root-user python3 "$TOOLS/preconfigurar.py" "$1" "$ROOT" "${@:2}"
