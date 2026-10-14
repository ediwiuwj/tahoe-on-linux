#!/bin/bash
# memoria.sh <programa> [t1] [t2]: ¿qué memoria crece en un proceso de macOS? Arranca launchd (tools/macos-shell.sh
# --launchd), busca el proceso por el programa de macOS que cargó (como hang.sh) y cada 5 s anota su RssAnon; en t1 y t2
# (30 y 60 s desde el arranque) guarda /proc/<pid>/smaps y al final memoria.py dice qué regiones son nuevas o crecieron.
# Siempre limpia. Variables:
#   TAHOE_ROOT  sistema de macOS (por omisión la Recovery); TAHOE_RUN otro binario (A/B); DEPURAR=1 modo depuración
#   MSGS/IOKS   programas cuyos mensajes Mach / actividad IOKit se trazan (como en discos.sh); al final se resumen
#               con mensajes.py e iokit.py
# Salida en ~/.cache/tahoe-diag: memoria.txt (muestreo), smaps.1, smaps.2, memoria.log (registro de este arranque).
DIAG="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
D="${TAHOE_DIAG_DIR:-$HOME/.cache/tahoe-diag}"; mkdir -p "$D"
PROG="${1:?indica el programa de macOS (p. ej. WindowServer)}"; T1="${2:-30}"; T2="${3:-60}"
export TAHOE_ROOT="${TAHOE_ROOT:-$HOME/macos26/base/macOS Base System}"
LOG="${TAHOE_LOGFILE:-/tmp/tahoe-shell-$USER.log}"
# como macos-shell.sh: si el registro pasa de TAHOE_LOG_MAX_MB se vacía ya (si lo vaciara él, «desde» quedaría fuera)
if [ -f "$LOG" ] && [ "$(stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt $(( ${TAHOE_LOG_MAX_MB:-300} * 1024 * 1024 )) ]; then : > "$LOG"; fi
desde=$(( $( [ -f "$LOG" ] && wc -l < "$LOG" || echo 0) + 1 ))
if [ -n "$MSGS" ]; then echo "$MSGS" > /dev/shm/tahoe-msg-trace; else rm -f /dev/shm/tahoe-msg-trace; fi
if [ -n "$IOKS" ]; then echo "$IOKS" > /dev/shm/tahoe-iokit-trace; else rm -f /dev/shm/tahoe-iokit-trace; fi
rm -f "$D/smaps.1" "$D/smaps.2"; : > "$D/memoria.txt"
( TAHOE_BOOT_WAIT=5 timeout $(( T2 + 60 )) "$DIAG/../macos-shell.sh" ${DEPURAR:+--depurar} --launchd -c "sleep $(( T2 + 30 ))" \
	> "$D/memoria.out" 2>&1 & )
# Pid de Linux del proceso cuyo programa de macOS (el último ejecutable que cargó, tipo=2) es $PROG.
pid_de() {
	local ns s n p
	for p in $(pgrep -x tahoe-run); do ns=$(readlink /proc/$p/ns/pid 2>/dev/null) && [ -n "$ns" ] && break; done
	[ -z "$ns" ] && return
	local progs; progs=$(tail -n +"$desde" "$LOG" 2>/dev/null | LC_ALL=C grep -ao "<[0-9]*> cargado [^ ]* .* tipo=2" |
		awk '{gsub(/[<>]/,"",$1); p[$1]=$3} END{for (k in p) print k, p[k]}')
	for s in /proc/[0-9]*/status; do
		[ "$(readlink ${s%/status}/ns/pid 2>/dev/null)" = "$ns" ] || continue
		n=$(awk '/^NSpid/{if (NF > 2) print $NF}' "$s" 2>/dev/null)
		[ -n "$n" ] && [ "$(echo "$progs" | awk -v n="$n" '$1==n{print $2}')" = "$PROG" ] && { s=${s#/proc/}; echo "${s%/status} $n"; return; }
	done
}
t=0
while [ $t -lt $T2 ]; do
	sleep 5; t=$((t + 5))
	read -r P N < <(pid_de)
	if [ -z "$P" ]; then echo "t=$t $PROG no está vivo" >> "$D/memoria.txt"; continue; fi
	echo "t=$t pid=$P (macOS $N) $(awk '/^RssAnon|^RssShmem|^VmSize/{printf "%s %d MB  ", $1, $2/1024}' /proc/$P/status 2>/dev/null)" >> "$D/memoria.txt"
	[ $t -ge "$T1" ] && [ ! -f "$D/smaps.1" ] && cp /proc/$P/smaps "$D/smaps.1" 2>/dev/null && echo "$N" > "$D/smaps.pid"
done
[ -n "$P" ] && cp /proc/$P/smaps "$D/smaps.2" 2>/dev/null
[ -n "$P" ] && [ "$N" != "$(cat "$D/smaps.pid" 2>/dev/null)" ] && echo "(aviso: el pid cambió entre las fotos: $PROG se relanzó)" >> "$D/memoria.txt"
"$DIAG/clean.sh" > /dev/null 2>&1
rm -f /dev/shm/tahoe-msg-trace /dev/shm/tahoe-iokit-trace
tail -n +"$desde" "$LOG" > "$D/memoria.log" 2>/dev/null
echo "== muestreo de $PROG"; cat "$D/memoria.txt"
grep -a "guardián" "$D/memoria.out" | head -2
if [ -f "$D/smaps.1" ] && [ -f "$D/smaps.2" ]; then echo "== smaps entre t=$T1 y t=$T2"; python3 "$DIAG/memoria.py" "$D/smaps.1" "$D/smaps.2"
else echo "== no hay dos fotos de smaps"; fi
[ -n "$MSGS" ] && python3 "$DIAG/mensajes.py" "$D/memoria.log" | head -60
[ -n "$IOKS" ] && python3 "$DIAG/iokit.py" "$D/memoria.log" | head -60
echo "(muestreo: $D/memoria.txt; fotos: $D/smaps.1 $D/smaps.2; registro: $D/memoria.log)"
