#!/bin/bash
# fix_symlinks.sh [raíz] [--aplicar]: repara los enlaces simbólicos absolutos del sistema de macOS extraído.
# En macOS muchos enlaces son absolutos (/etc/rc.cdrom -> /usr/libexec/rc_script_wrapper). En Linux es el kernel quien los
# sigue, así que deben apuntar a la ruta dentro de la raíz (raíz + destino); tahoe-run le quita ese prefijo a readlink y el
# programa ve el destino original. Al extraer, algunos quedaron apuntando a una ruta del anfitrión que no existe (p. ej. sin
# la carpeta «macOS Base System»): sin ellos no arranca, entre otros, /etc/rc.cdrom (las tareas de arranque de la Recovery).
# Solo toca enlaces absolutos ROTOS cuyo destino se encuentre dentro de la raíz; sin --aplicar solo los lista.
#   tools/fix_symlinks.sh                       # raíz por omisión: ~/macos26/base/macOS Base System
#   tools/fix_symlinks.sh "$TAHOE_ROOT" --aplicar
ROOT="${1:-$HOME/macos26/base/macOS Base System}"
[ "$1" = "--aplicar" ] && ROOT="$HOME/macos26/base/macOS Base System"
APLICAR=0; for a in "$@"; do [ "$a" = "--aplicar" ] && APLICAR=1; done
ROOT="$(cd "$ROOT" && pwd)" || exit 1
n=0; sin=0
while IFS= read -r -d '' l; do
	t="$(readlink "$l")"
	case "$t" in /*) ;; *) continue;; esac
	[ -e "$l" ] && continue                                   # el enlace funciona: no se toca
	case "$t" in "$ROOT"/*) rest="${t#"$ROOT"}";; *) rest="$t";; esac
	# el destino de macOS es el sufijo más largo de la ruta que exista dentro de la raíz (quita prefijos del anfitrión)
	# (de más largo a más corto, con al menos dos componentes para no casar por un nombre suelto)
	nuevo=""; s="$rest"
	while :; do
		case "$s" in /*/*) ;; *) break;; esac
		if [ -e "$ROOT$s" ] || [ -L "$ROOT$s" ]; then nuevo="$ROOT$s"; break; fi
		s="/${s#/*/}"
	done
	# sin destino dentro de la raíz (también falta en macOS, p. ej. /etc/localtime): si la ruta es la de la carpeta que contiene
	# la raíz (el prefijo que dejó la extracción), se pone la raíz en su lugar y el enlace queda colgando como en macOS
	PADRE="$(dirname "$ROOT")"
	case "$t" in "$ROOT"/*) ;; "$PADRE"/*) [ -z "$nuevo" ] && nuevo="$ROOT${t#"$PADRE"}";; esac
	rel="${l#"$ROOT"}"
	if [ -z "$nuevo" ]; then sin=$((sin + 1)); echo "sin destino: $rel -> $t"; continue; fi
	echo "$rel -> ${nuevo#"$ROOT"}"
	[ $APLICAR = 1 ] && ln -sfn "$nuevo" "$l"
	n=$((n + 1))
done < <(find "$ROOT" -xdev -type l -print0)
echo "enlaces a reparar: $n (sin destino en la raíz: $sin)$([ $APLICAR = 1 ] && echo ', reparados' || echo '; con --aplicar se reparan')"
