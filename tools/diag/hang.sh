#!/bin/bash
# hang.sh '<orden de macOS>' <proceso> [segundos]: lanza la orden con msh.sh y, pasados los segundos (20) tras el arranque,
# vuelca cada hilo del proceso de macOS llamado <proceso> (estado, llamada de Linux en curso, wchan y la instrucción,
# simbolizada con sym.py si está en la caché de macOS o con addr2line si es código de tahoe-run). Siempre limpia.
# Admite las variables de diagnóstico de msh.sh (el registro queda en msh.log). Con STACKS=<proceso> (el mismo), además
# las pilas de macOS de cada hilo, simbolizadas.
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; mkdir -p "$D"
RUN="${TAHOE_RUN:-$DIAG/../../tahoe-run/tahoe-run}"
( "$DIAG/msh.sh" "$1" 12 $(( ${3:-20} + 40 )) > "$D/hang.out" 2>&1 & )
sleep $(( 12 + 4 + ${3:-20} ))
# Línea de órdenes desde un hilo vivo: si el hilo principal terminó, /proc/<pid>/cmdline sale vacío
# (y no vale [ -s ]: los archivos de /proc dicen tener tamaño 0).
cmdl() { local t c; for t in /proc/$1/task/*; do c=$(tr '\0' ' ' < $t/cmdline 2>/dev/null); [ -n "$c" ] && { echo "$c"; return; }; done; }
for P in $(pgrep -x tahoe-run); do
	cmdl $P 2>/dev/null | sed 's/.*dyld //' | LC_ALL=C grep -q -E "/$2( |$)" || continue
	echo "== pid $P (en macOS $(awk '/^NSpid/{print $NF}' /proc/$P/status)): $(cmdl $P | sed 's/.*dyld //' | cut -c1-100)"
	for t in /proc/$P/task/*; do
		s=$(cat $t/syscall 2>/dev/null); pc=$(echo "$s" | awk '{print $NF}'); nr=$(echo "$s" | awk '{print $1}')
		w=$(cat $t/wchan 2>/dev/null); st=$(awk '{print $3}' $t/stat)
		case "$pc" in 0x7ff8*|0x7ff9*|0x7ffa*|0x7ffb*|0x7ffc*|0x7ffd*|0x7ffe*) where=$(python3 "$DIAG/sym.py" ${pc#0x} 2>/dev/null | cut -d' ' -f2-);;
			0x*) where=$(addr2line -f -s -e "$RUN" "$pc" 2>/dev/null | paste -sd' ');; *) where=;; esac
		echo "   $(basename $t) $st nr=$nr wchan=$w pc=$pc $where"
	done
	# Pilas de macOS de cada hilo: si el proceso es el de STACKS, SIGRTMIN+5 hace que cada hilo escriba en el registro sus
	# marcos (rbp) y la llamada en curso (on_dump_all, emu_proc.c); se simbolizan con sym.py.
	if [ -n "$STACKS" ]; then
		N=$(awk '/^NSpid/{print $NF}' /proc/$P/status)
		kill -s RTMIN+5 "$P" 2>/dev/null; sleep 1
		LC_ALL=C grep -a "HILO <$N:" "$D/msh.log" | sort -s -t: -k2,2n | tail -n 120 | while read -r l; do
			case "$l" in
				*"marcos (rbp):"*) python3 "$DIAG/sym.py" ${l#*marcos (rbp):} 2>/dev/null | sed 's/^/        /';;
				*) echo "   ${l:0:200}";;
			esac
		done
	fi
done
sleep 2; while pgrep -f "diag/msh.sh" > /dev/null; do sleep 1; done
echo "--- salida de la orden:"; tail -8 "$D/hang.out"
