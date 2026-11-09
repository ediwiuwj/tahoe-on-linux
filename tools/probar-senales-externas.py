#!/usr/bin/env python3
"""Señales desde Linux a Bash Tahoe, con puntos seguros (kill -0) explícitos."""
import argparse
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, required=True)
    p.add_argument('--run', type=Path, required=True)
    p.add_argument('--salida', type=Path, required=True)
    a = p.parse_args()
    root, run = a.root.resolve(), a.run.resolve()
    a.salida.mkdir(parents=True, exist_ok=True)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resultados = []
    for nombre, signo in [('USR1', signal.SIGUSR1), ('USR2', signal.SIGUSR2)]:
        archivo = a.salida/(nombre+'.txt')
        env = dict(PATH='/usr/bin:/bin', HOME='/var/root', USER='root', LANG='C',
                   TAHOE_ROOT=str(root), TAHOE_CACHE_DIR=str(a.salida.resolve()/'cache'),
                   TAHOE_LOGFILE=str(a.salida.resolve()/(nombre+'.log')))
        # La shell debe seguir haciendo syscalls: no prueba entrega en bucles
        # puramente CPU ni interrupción de operaciones bloqueantes.
        codigo = ("trap 'printf EXTERNA_OK; exit 0' " + nombre +
                  "; printf LISTA; while :; do kill -0 $$ || exit 3; done")
        cmd = [str(run), str(root/'System/Library/dyld/dyld_shared_cache_x86_64'),
               str(root/'usr/lib/dyld'), str(root/'bin/bash'),
               '--noprofile', '--norc', '-c', codigo]
        enviado = False
        with archivo.open('w') as out:
            proc = subprocess.Popen(cmd, env=env, stdout=out, stderr=subprocess.STDOUT,
                                    start_new_session=True)
            try:
                limite = time.monotonic()+10
                while time.monotonic()<limite and proc.poll() is None:
                    if b'LISTA' in archivo.read_bytes():
                        os.kill(proc.pid, signo)
                        enviado = True
                        break
                    time.sleep(.02)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
            finally:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait()
        texto = archivo.read_text(errors='replace')
        bien = enviado and proc.returncode == 0 and 'EXTERNA_OK' in texto
        resultado = dict(senal=nombre, enviado=enviado, aprobado=bien, retorno=proc.returncode)
        resultados.append(resultado)
        print(json.dumps(resultado), flush=True)
    (a.salida/'resultados.json').write_text(json.dumps(resultados, indent=2)+'\n')
    return 0 if all(r['aprobado'] for r in resultados) else 1


if __name__ == '__main__':
    raise SystemExit(main())
