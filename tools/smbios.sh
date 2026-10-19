#!/bin/bash
# smbios.sh: genera la identidad del equipo que presenta tahoe-run (como en un Hackintosh con OpenCore): modelo, placa
# (board-id), número de serie y MLB con macserial de OpenCore, y UUID y ROM aleatorios (no la MAC real). Se guarda en
# ~/.config/tahoe-run/smbios.json (o TAHOE_SMBIOS_FILE), fuera del repositorio: es única de cada instalación.
# tools/macos-shell.sh se la pasa a tahoe-run, que la pone en IOPlatformExpertDevice, hw.model/hw.target y la NVRAM (MLB, ROM).
#
#   tools/smbios.sh [modelo]          # por omisión MacBookPro16,2: portátil Intel con solo gráfica integrada, admitido por macOS 26
#
# Modelos Intel admitidos por macOS 26: MacBookPro16,1/16,2/16,4, iMac20,1/20,2, MacPro7,1. No sobrescribe un archivo existente.
set -e
MODELO="${1:-MacBookPro16,2}"
DEST="${TAHOE_SMBIOS_FILE:-$HOME/.config/tahoe-run/smbios.json}"
[ -e "$DEST" ] && { echo "ya existe $DEST: no se toca (bórralo para generar otro)" >&2; exit 1; }
declare -A PLACA=([MacBookPro16,1]=Mac-E1008331FDC96864 [MacBookPro16,2]=Mac-5F9802EFE386AA28 [MacBookPro16,4]=Mac-A61BADE1FDAD7B05
	[iMac20,1]=Mac-CFF7D910A743CAAF [iMac20,2]=Mac-AF89B6D9451A490B [MacPro7,1]=Mac-27AD2F918AE68F61)
[ -n "${PLACA[$MODELO]}" ] || { echo "modelo no admitido por macOS 26 en Intel: $MODELO" >&2; exit 1; }
OC="${XDG_CACHE_HOME:-$HOME/.cache}/tahoe-run/opencore"
MS="$OC/Utilities/macserial/macserial.linux"
if [ ! -x "$MS" ]; then
	mkdir -p "$OC"
	URL=$(curl -sL https://api.github.com/repos/acidanthera/OpenCorePkg/releases/latest | grep -o 'https://[^"]*RELEASE.zip' | head -1)
	curl -sL -o "$OC/oc.zip" "$URL" && (cd "$OC" && unzip -o -q oc.zip 'Utilities/macserial/*') && chmod +x "$MS"
fi
L=$("$MS" -m "$MODELO" -g -n 1 | grep '|' | head -1)
S=$(echo "$L" | cut -d'|' -f1 | tr -d ' '); M=$(echo "$L" | cut -d'|' -f2 | tr -d ' ')
[ -n "$S" ] && [ -n "$M" ] || { echo "macserial no dio número de serie" >&2; exit 1; }
U=$(tr a-f A-F < /proc/sys/kernel/random/uuid); R=$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n' | tr a-f A-F)
umask 077; mkdir -p "$(dirname "$DEST")"
printf '{\n "modelo": "%s",\n "placa": "%s",\n "serie": "%s",\n "mlb": "%s",\n "uuid": "%s",\n "rom": "%s",\n "origen": "macserial -m %s -g; uuid y rom aleatorios"\n}\n' \
	"$MODELO" "${PLACA[$MODELO]}" "$S" "$M" "$U" "$R" "$MODELO" > "$DEST"
echo "generado $DEST ($MODELO); número de serie: $("$MS" -i "$S" | grep -m1 SystemModel | sed 's/.*: //')"
