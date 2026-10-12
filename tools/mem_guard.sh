#!/bin/bash
# Guardián de memoria para las pruebas: si la RAM disponible baja de TAHOE_GUARD_MB (600 MB por omisión; con zram la PC
# aún responde ahí) o el swap en disco usado crece más de 1 GB
# desde que arrancó (el que ya usaban otros programas no cuenta), mata todo tahoe-run. El swap en zram (RAM comprimida)
# no cuenta: no hace trabajar al disco y lo que ocupa ya se descuenta de la RAM disponible.
# Parar con: touch /dev/shm/tahoe-guard-stop
swap_used() { awk 'NR > 1 && $1 !~ /zram/ {u += $4} END {print int(u / 1024)}' /proc/swaps; }   # KiB usados en disco
sw0=$(swap_used)
while [ ! -e /dev/shm/tahoe-guard-stop ]; do
  av=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
  sw=$(swap_used)
  if [ "$av" -lt "${TAHOE_GUARD_MB:-600}" ] || [ "$sw" -gt $((sw0 + 1000)) ]; then
    pkill -9 -x tahoe-run; pkill -9 unshare
    echo "guardian: disponible=${av}MB swap=${sw}MB -> procesos de tahoe-run terminados" >> ${TMPDIR:-/tmp}/tahoe-guard.log
  fi
  for p in $(pgrep -x tahoe-run); do a=$(awk '/RssAnon/{print int($2/1024)}' /proc/$p/status 2>/dev/null); if [ "${a:-0}" -gt 700 ]; then echo "guardián: pid $p anon=${a}MB -> procesos de tahoe-run terminados" | tee -a ${TMPDIR:-/tmp}/tahoe-guard.log >&2; pkill -9 -x tahoe-run; pkill -9 unshare; fi; done
  sleep 1
done
