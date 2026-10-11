#!/bin/bash
# clean.sh: termina todos los procesos de tahoe-run, para el guardián de memoria y borra los archivos de /dev/shm de tahoe-run
# (regiones de puertos, semáforos, banderas de diagnóstico). Imprime lo que queda vivo y la RAM disponible.
pkill -9 -x tahoe-run; pkill -9 -x unshare; touch /dev/shm/tahoe-guard-stop; sleep 1.5
rm -f /dev/shm/tahoe-guard-stop /dev/shm/tahoe-me-* /dev/shm/tahoe.* /dev/shm/tahoe-ports-* /dev/shm/tahoe-hid-* /dev/shm/sem.tahoe-* \
	/dev/shm/tahoe-stacks /dev/shm/tahoe-stacks-all /dev/shm/tahoe-mach-trace /dev/shm/tahoe-iokit-trace /dev/shm/tahoe-break /dev/shm/tahoe-peek
echo "limpio: tahoe-run vivos=$(pgrep -c -x tahoe-run) disponible=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)MB"
