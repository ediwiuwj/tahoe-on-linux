#!/usr/bin/env python3
"""Valida SI_USER de SIGINT externa y el pid/uid en siginfo Darwin."""
import argparse
import os
from pathlib import Path
import resource
import select
import signal
import subprocess
import time


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root',type=Path,required=True)
    p.add_argument('--run',type=Path,required=True)
    p.add_argument('--probe',type=Path,required=True)
    p.add_argument('--salida',type=Path,required=True)
    a=p.parse_args(); root=a.root.resolve(); run=a.run.resolve(); probe=a.probe.resolve()
    a.salida.mkdir(parents=True,exist_ok=True); resource.setrlimit(resource.RLIMIT_CORE,(0,0))
    env=dict(PATH='/usr/bin:/bin',HOME='/var/root',USER='root',LANG='C',
        TAHOE_ROOT=str(root),TAHOE_CACHE_DIR=str(a.salida.resolve()/'cache'),
        TAHOE_LOGFILE=str(a.salida.resolve()/'tahoe.log'))
    cmd=[str(run),str(root/'System/Library/dyld/dyld_shared_cache_x86_64'),
         str(root/'usr/lib/dyld'),str(probe),'externa-int',str(os.getpid()),str(os.getuid())]
    proc=subprocess.Popen(cmd,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,
                          start_new_session=True,text=True,bufsize=1)
    lines=[]; sent=False; deadline=time.monotonic()+15
    try:
        while time.monotonic()<deadline:
            ready,_,_=select.select([proc.stdout],[],[],.1)
            if ready:
                line=proc.stdout.readline()
                if line:
                    lines.append(line.rstrip()); print(line,end='',flush=True)
                    if 'SIGINT_LISTA' in line:
                        os.kill(proc.pid,signal.SIGINT); sent=True
            if proc.poll() is not None: break
        if proc.poll() is None: raise TimeoutError('El probe no terminó en 15 segundos')
    finally:
        if proc.poll() is None:
            try: os.killpg(proc.pid,signal.SIGKILL)
            except ProcessLookupError: pass
            proc.wait()
        proc.stdout.close()
    (a.salida/'salida.txt').write_text('\n'.join(lines)+'\n')
    ok=sent and proc.returncode==0 and any('SIGINT_EXTERNA_INFO_OK' in x for x in lines)
    print('SIGINT externa SI_USER:', 'APROBADO' if ok else f'FALLO (rc={proc.returncode})')
    return 0 if ok else 1

if __name__=='__main__':
    raise SystemExit(main())
