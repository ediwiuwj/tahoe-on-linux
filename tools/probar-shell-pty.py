#!/usr/bin/env python3
"""Prueba shell interactiva en PTY privada, sin abrir ventanas."""
import argparse
import errno
import os
from pathlib import Path
import pty
import select
import signal
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, required=True)
    p.add_argument('--run', type=Path, required=True)
    p.add_argument('--salida', type=Path, required=True)
    p.add_argument('--ctrl-c', action='store_true', help='Interrumpir en el prompt y comprobar que la shell continúa')
    p.add_argument('--repeticiones', type=int, default=1)
    a = p.parse_args()
    if not 1 <= a.repeticiones <= 100:
        p.error('--repeticiones debe estar entre 1 y 100')
    root, run = a.root.resolve(), a.run.resolve()
    a.salida.mkdir(parents=True, exist_ok=True)
    env = dict(PATH='/usr/bin:/bin:/usr/sbin:/sbin', HOME='/var/root', USER='root',
               LOGNAME='root', SHELL='/bin/bash', LANG='C', TERM='xterm', PS1='TAHOE> ',
               TAHOE_ROOT=str(root), TAHOE_CACHE_DIR=str(a.salida.resolve()/'cache'),
               TAHOE_LOGFILE=str(a.salida.resolve()/'pty.log'))
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(run, [str(run), str(root/'System/Library/dyld/dyld_shared_cache_x86_64'),
                       str(root/'usr/lib/dyld'), str(root/'bin/bash'),
                       '--noprofile', '--norc', '-i'], env)
    salida = bytearray()
    terminado = False
    retorno = None
    interrupciones = 0
    verificar_desde = None
    try:
        if a.ctrl_c:
            os.write(fd, b"set +H\nprintf 'LISTA_%s\\n' OK\n")
        else:
            os.write(fd, b"set +H\nprintf 'PTY_%s\\n' OK; /bin/sleep 1 & p=$!; jobs; wait $p; printf 'WAIT_%s\\n' OK; exit\n")
        limite = time.monotonic() + 20 + (a.repeticiones * .6 if a.ctrl_c else 0)
        while time.monotonic() < limite and len(salida) < 1024 * 1024:
            if select.select([fd], [], [], .2)[0]:
                try:
                    b = os.read(fd, 8192)
                    if not b:
                        break
                    salida.extend(b)
                except OSError as e:
                    if e.errno == errno.EIO:
                        break
                    raise
            marcador = b'LISTA_OK' if not interrupciones else f'CTRL_{interrupciones}_OK'.encode()
            if (a.ctrl_c and interrupciones < a.repeticiones and verificar_desde is None
                    and marcador in salida and salida.endswith(b'TAHOE> ')):
                os.write(fd, b'\x03')
                interrupciones += 1
                verificar_desde = time.monotonic() + .2
            if verificar_desde is not None and time.monotonic() >= verificar_desde:
                comando = f"printf 'CTRL_{interrupciones}_%s\\n' OK"
                if interrupciones == a.repeticiones:
                    comando += '; exit 0'
                os.write(fd, (comando+'\n').encode())
                verificar_desde = None
            r, estado = os.waitpid(pid, os.WNOHANG)
            if r:
                terminado = True
                retorno = os.waitstatus_to_exitcode(estado)
                print('estado_shell=', retorno)
                break
    finally:
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        if not terminado:
            _, estado = os.waitpid(pid, 0)
            retorno = os.waitstatus_to_exitcode(estado)
        os.close(fd)
        (a.salida/'consola.txt').write_bytes(salida)
    if a.ctrl_c:
        bien = (retorno == 0 and interrupciones == a.repeticiones
                and f'CTRL_{a.repeticiones}_OK'.encode() in salida)
    else:
        bien = retorno == 0 and b'PTY_OK' in salida and b'WAIT_OK' in salida
    print('PTY Ctrl-C:' if a.ctrl_c else 'PTY y espera interactiva:', 'APROBADO' if bien else 'FALLO')
    return 0 if bien else 1


if __name__ == '__main__':
    raise SystemExit(main())
