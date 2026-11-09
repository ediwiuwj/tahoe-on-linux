#!/usr/bin/env python3
"""Regresiones acotadas de la shell original; no compila ni inicia gráficos."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess

CASOS = {
    "builtin": ('test "$((7 * 6))" = 42 && printf OK', 'OK'),
    "version": ('/usr/bin/sw_vers -productVersion', '26.'),
    "proceso": ('/usr/bin/true; /usr/bin/false; test "$?" = 1 && printf OK', 'OK'),
    "tuberia": ('printf abc | /usr/bin/tr a-z A-Z', 'ABC'),
    "sustitucion": ('x=$(/usr/bin/printf hola); test "$x" = hola && printf OK', 'OK'),
    "subshell": ('x=padre; (x=hijo); test "$x" = padre && printf OK', 'OK'),
    "wait": ('/usr/bin/true & p=$!; wait "$p"; test "$?" = 0 && printf OK', 'OK'),
    "redireccion": ('f=/tmp/tahoe-shell-test-$$; printf prueba > "$f" && /bin/cat < "$f"', 'prueba'),
    "rm_disponible": ('test -x /bin/rm && printf OK', 'OK'),
    "crear_borrar": ('f=/tmp/tahoe-rm-$$; printf dato > "$f" && /bin/rm "$f" && test ! -e "$f" && printf OK', 'OK'),
    "append": ('f=/tmp/tahoe-append-$$; printf uno > "$f"; printf dos >> "$f"; x=$(/bin/cat "$f"); /bin/rm "$f"; test "$x" = unodos && printf OK', 'OK'),
    "descriptor": ('f=/tmp/tahoe-fd-$$; exec 7>"$f"; printf dato >&7; exec 7>&-; x=$(/bin/cat "$f"); /bin/rm "$f"; test "$x" = dato && printf OK', 'OK'),
    "pipe_estado": ('set -o pipefail; /usr/bin/false | /usr/bin/true; test "$?" = 1 && printf OK', 'OK'),
    "cadena_pipes": ('printf abc | /usr/bin/tr a-z A-Z | /bin/cat', 'ABC'),
    "directorio_temporal": ('d=/tmp/tahoe-dir-$$; /bin/mkdir "$d" && cd "$d" && printf dato > archivo && /bin/rm archivo && cd / && /bin/rmdir "$d" && printf OK', 'OK'),
    "directorio": ('cd /System && test "$PWD" = /System && printf OK', 'OK'),
    "trap": ('trap "printf TRAP_OK" USR1; kill -USR1 $$', 'TRAP_OK'),
    "trap_continuacion": ('n=0; trap "n=$((n+1))" USR1; kill -USR1 $$; test "$n" = 1 && printf OK', 'OK'),
    "trap_usr2": ("n=0; trap 'n=$((n+1))' USR2; kill -USR2 $$; test \"$n\" = 1 && printf OK", 'OK'),
    "trap_dos_senales": ("n=0; trap 'n=$((n+1))' USR1; trap 'n=$((n+10))' USR2; kill -USR1 $$; kill -USR2 $$; test \"$n\" = 11 && printf OK", 'OK'),
    "trap_orden_inverso": ("n=0; trap 'n=$((n+1))' USR1; trap 'n=$((n+10))' USR2; kill -USR2 $$; kill -USR1 $$; test \"$n\" = 11 && printf OK", 'OK'),
    "trap_cruzado": ("n=0; trap 'n=$((n+1)); kill -USR2 $$' USR1; trap 'n=$((n+10))' USR2; kill -USR1 $$; test \"$n\" = 11 && printf OK", 'OK'),
    "perl_basico": ("/usr/bin/perl -e 'print 6*7'", '42'),
    "trap_repetido": ("n=0; trap 'n=$((n+1))' USR1; kill -USR1 $$; kill -USR1 $$; test \"$n\" = 2 && printf OK", 'OK'),
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--run', type=Path, required=True)
    parser.add_argument('--salida', type=Path, required=True)
    args = parser.parse_args()
    root, run = args.root.resolve(), args.run.resolve()
    args.salida.mkdir(parents=True, exist_ok=True)
    env = dict(PATH='/usr/bin:/bin:/usr/sbin:/sbin', HOME='/var/root',
               USER='root', LOGNAME='root', SHELL='/bin/bash', LANG='C',
               TAHOE_ROOT=str(root), TAHOE_CACHE_DIR=str(args.salida.resolve()/'cache'))
    cmd = [str(run), str(root/'System/Library/dyld/dyld_shared_cache_x86_64'),
           str(root/'usr/lib/dyld'), str(root/'bin/bash'), '--noprofile', '--norc', '-c']
    resultados = []
    for nombre, (codigo, esperado) in CASOS.items():
        env['TAHOE_LOGFILE'] = str(args.salida.resolve()/(nombre+'.log'))
        # Cada caso tiene su propio grupo; nunca se termina un proceso ajeno.
        with (args.salida/(nombre+'.txt')).open('w') as out:
            p = subprocess.Popen(cmd+[codigo], env=env, stdout=out,
                                 stderr=subprocess.STDOUT, start_new_session=True)
            timeout = False
            try:
                p.wait(timeout=15)
            except subprocess.TimeoutExpired:
                timeout = True
            finally:
                try:
                    os.killpg(p.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                p.wait()
        texto = (args.salida/(nombre+'.txt')).read_text(errors='replace')
        bien = not timeout and p.returncode == 0 and esperado in texto
        resultados.append(dict(caso=nombre, aprobado=bien, retorno=p.returncode, timeout=timeout))
        print(json.dumps(resultados[-1]), flush=True)
    (args.salida/'resultados.json').write_text(json.dumps(resultados, indent=2)+'\n')
    return 0 if all(r['aprobado'] for r in resultados) else 1


if __name__ == '__main__':
    raise SystemExit(main())
