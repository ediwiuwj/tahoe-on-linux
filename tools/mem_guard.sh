#!/bin/bash
# Guardián de memoria para las pruebas: si la RAM disponible baja de 900 MB o el swap usado crece más de 1 GB desde que
# arrancó (el swap que ya usaban otros programas no cuenta), mata todo tahoe-run.
# Parar con: touch /dev/shm/tahoe-guard-stop
swap_used() { awk '/SwapTotal/{t=$2}/SwapFree/{f=$2}END{print int((t-f)/1024)}' /proc/meminfo; }
sw0=$(swap_used)
while [ ! -e /dev/shm/tahoe-guard-stop ]; do
  av=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
  sw=$(swap_used)
  if [ "$av" -lt 900 ] || [ "$sw" -gt $((sw0 + 1000)) ]; then
    pkill -9 -x tahoe-run; pkill -9 unshare
    echo "guardian: disponible=${av}MB swap=${sw}MB -> procesos de tahoe-run terminados" >> ${TMPDIR:-/tmp}/tahoe-guard.log
  fi
  for p in $(pgrep -x tahoe-run); do a=$(awk '/RssAnon/{print int($2/1024)}' /proc/$p/status 2>/dev/null); if [ "${a:-0}" -gt 700 ]; then echo "guardián: pid $p anon=${a}MB -> terminado" >&2; pkill -9 -x tahoe-run; pkill -9 unshare; fi; done
  sleep 1
done
