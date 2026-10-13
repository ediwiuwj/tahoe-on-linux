#!/bin/bash
# hang.sh '<orden de macOS>' '<proceso> [proceso...]' [segundos]: lanza la orden con msh.sh y, pasados los segundos (20)
# tras el arranque, vuelca cada hilo de los procesos de macOS con esos nombres (estado, llamada de Linux en curso, wchan y
# la instrucción, simbolizada con sym.py si está en la caché de macOS o con addr2line si es código de tahoe-run). Los
# procesos se buscan por el programa de macOS que cargaron (el registro dice "<pid de macOS> cargado <programa>"; el pid
# de Linux se encuentra por NSpid), no por la línea de órdenes, que puede ser la del xpcproxy que lo lanzó. Si alguno no
# está vivo, dice qué se sabe de él por el registro (si llegó a cargarse, sus mensajes de aborto). Siempre limpia.
# Admite las variables de diagnóstico de msh.sh (el registro queda en msh.log). Con STACKS=<proceso> (el mismo), además
# las pilas de macOS de cada hilo, simbolizadas. ESPERA: segundos de arranque antes de la orden (12; el macOS completo
# necesita más, p. ej. 60).
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; mkdir -p "$D"
RUN="${TAHOE_RUN:-$DIAG/../../tahoe-run/tahoe-run}"
E=${ESPERA:-12}
# msh.sh apaga el arranque en cuanto acaba la orden: se alarga con un sleep para que siga vivo cuando llegue el volcado.
( "$DIAG/msh.sh" "$1; sleep $(( ${3:-20} + 30 ))" $E $(( E + ${3:-20} + 60 )) > "$D/hang.out" 2>&1 & )
sleep $(( E + 4 + ${3:-20} ))
# Línea de órdenes desde un hilo vivo: si el hilo principal terminó, /proc/<pid>/cmdline sale vacío
# (y no vale [ -s ]: los archivos de /proc dicen tener tamaño 0).
cmdl() { local t c; for t in /proc/$1/task/*; do c=$(tr '\0' ' ' < $t/cmdline 2>/dev/null); [ -n "$c" ] && { echo "$c"; return; }; done; }
# Pid de macOS -> programa que tiene cargado ahora (el último ejecutable, tipo=2, que cargó ese pid).
progs() { LC_ALL=C grep -ao "<[0-9]*> cargado [^ ]* .* tipo=2" "$D/msh.log" | awk '{gsub(/[<>]/,"",$1); p[$1]=$3} END{for (k in p) print k, p[k]}'; }
# Procesos (pid de Linux) cuyo programa de macOS es $1: se buscan por su pid de macOS (último campo de NSpid, en un
# espacio de PID hijo: el del arranque) y no por el nombre de Linux, que no siempre es tahoe-run.
NS=; for p in $(pgrep -x tahoe-run); do NS=$(readlink /proc/$p/ns/pid 2>/dev/null) && [ -n "$NS" ] && break; done
pids_of() {
	local s n
	for s in /proc/[0-9]*/status; do
		[ "$(readlink ${s%/status}/ns/pid 2>/dev/null)" = "$NS" ] || continue
		n=$(awk '/^NSpid/{if (NF > 2) print $NF}' "$s" 2>/dev/null)
		[ -n "$n" ] && [ "$(echo "$PROGS" | awk -v n="$n" '$1==n{print $2}')" = "$1" ] && { s=${s#/proc/}; echo "${s%/status}"; }
	done
}
# Lo que dice el registro de un proceso que no está vivo.
gone() {
	local n=$1 m
	m=$(LC_ALL=C grep -ao "<[0-9]*> cargado $n " "$D/msh.log" | grep -o "[0-9]*" | sort -un | paste -sd' ')
	if [ -z "$m" ]; then echo "== $n: no llegó a cargarse"; return; fi
	echo "== $n: no está vivo (pid de macOS $m)"
	for p in $m; do
		LC_ALL=C grep -a "abort <$p>: \[\|fin <$p> \|SIG[A-Z]* <$p>\|recolector: pid $p " "$D/msh.log" | sort -u | head -5 | sed 's/^ */   /'
		LC_ALL=C grep -a "<$p>" "$D/msh.log" | grep -av "IOKIT" | tail -3 | cut -c1-200 | sed 's/^ */   último: /'
	done
}
PROGS=$(progs)
for NAME in $2; do
	PIDS=$(pids_of "$NAME")
	[ -z "$PIDS" ] && { gone "$NAME"; continue; }
	for P in $PIDS; do
	echo "== $NAME: pid $P, \"$(cat /proc/$P/comm 2>/dev/null)\" en Linux (en macOS $(for t in /proc/$P/task/*; do awk '/^NSpid/{print $NF; exit}' "$t/status" 2>/dev/null; done | head -1)): $(cmdl $P | sed 's/.*dyld //' | cut -c1-100)"
	# Hilos iguales (mismo estado, llamada y sitio: p. ej. los de reserva de las colas, parados en poll) en una sola línea.
	for t in /proc/$P/task/*; do
		s=$(cat $t/syscall 2>/dev/null); pc=$(echo "$s" | awk '{print $NF}'); nr=$(echo "$s" | awk '{print $1}')
		[ -r $t/stat ] || continue                              # el hilo terminó mientras se miraba
		w=$(cat $t/wchan 2>/dev/null); st=$(awk '{print $3}' $t/stat 2>/dev/null)
		case "$pc" in 0x7ff8*|0x7ff9*|0x7ffa*|0x7ffb*|0x7ffc*|0x7ffd*|0x7ffe*) where=$(python3 "$DIAG/sym.py" ${pc#0x} 2>/dev/null | cut -d' ' -f2-);;
			0x*) where=$(addr2line -f -s -e "$RUN" "$pc" 2>/dev/null | paste -sd' ');; *) where=;; esac
		echo "$(basename $t) $st nr=$nr wchan=$w pc=$pc $where"
	done | awk '{t=$1; $1=""; if (!($0 in n)) o[++k]=$0; n[$0]++; f[$0]=f[$0] ? f[$0] : t}
		END{for (i=1; i<=k; i++) { h=(n[o[i]] > 1) ? n[o[i]] " hilos (" f[o[i]] "...):" : f[o[i]]; printf "   %s%s\n", h, o[i] }}' 
	# Pilas de macOS de cada hilo: si el proceso es el de STACKS, SIGRTMIN+5 hace que cada hilo escriba en el registro sus
	# marcos (rbp) y la llamada en curso (on_dump_all, emu_proc.c); se simbolizan con sym.py.
	if [ -n "$STACKS" ]; then
		# el pid de macOS se lee de un hilo vivo: si el principal ya terminó (zombi), /proc/<pid>/status puede no estar
		N=$(for t in /proc/$P/task/*; do awk '/^NSpid/{print $NF; exit}' "$t/status" 2>/dev/null; done | head -1)
		kill -s RTMIN+5 "$P" 2>/dev/null; sleep 1
		# los hilos que ejecutan código de tahoe-run sin llamada en curso (en reposo) se cuentan, no se listan
		LC_ALL=C grep -a "HILO <$N:" "$D/msh.log" | sort -s -t: -k2,2n | tail -n 400 > "$D/hilos.tmp"
		idle=$(LC_ALL=C grep -c "ejecutando rip=0x[0-9a-f]\{1,8\}$" "$D/hilos.tmp")
		LC_ALL=C grep -v "ejecutando rip=0x[0-9a-f]\{1,8\}$" "$D/hilos.tmp" | tail -n 150 | while read -r l; do
			case "$l" in
				*"marcos (rbp):"*) python3 "$DIAG/sym.py" ${l#*marcos (rbp):} 2>/dev/null | sed 's/^/        /';;
				*) echo "   ${l:0:200}";;
			esac
		done
		[ "${idle:-0}" -gt 0 ] && echo "   (+ $idle hilos en reposo dentro de tahoe-run)"
		rm -f "$D/hilos.tmp"
	fi
	done
done
sleep 2; while pgrep -f "diag/msh.sh" > /dev/null; do sleep 1; done
echo "--- salida de la orden:"; tail -8 "$D/hang.out"
