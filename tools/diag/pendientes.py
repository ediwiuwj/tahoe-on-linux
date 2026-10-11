#!/usr/bin/env python3
# pendientes.py [registro]: peticiones Mach que esperan respuesta y no la recibieron (quién espera a quién), a partir de un
# registro con traza de mensajes (tools/diag/msh.sh con MACH=1). Sirve para seguir una cadena de bloqueos: diskutil espera a
# storagekitd, que espera a powerd, que espera a configd...
#   tools/diag/pendientes.py [-v] [registro]      # por omisión ~/.cache/tahoe-diag/msh.log
# Cada línea: pid.hilo del que espera, nombre de su programa, pid y programa del que debe responder, id del mensaje
# (MIG: subsistema*100 + rutina; 0x10000000/0x40000000 son de XPC) y línea del registro. Las que esperan a launchd suelen
# ser normales (avisos que launchd guarda hasta que pase algo), y las de un proceso a sí mismo son internas; con -v se
# muestran también.
import os, re, sys

args = [a for a in sys.argv[1:] if a != "-v"]
path = args[0] if args else os.path.expanduser(os.environ.get("TAHOE_DIAG_DIR", "~/.cache/tahoe-diag") + "/msh.log")
send = re.compile(r"MT pid=(\d+)\.(\d+) send dest=(0x[0-9a-f]+)\(->(-?\d+)\) resp=(0x[0-9a-f]+) id=(\d+) size=\d+ rcv=(\d)")
name = re.compile(r"<(\d+)> cargado (\S+)")
progs, waiting = {}, {}                       # pid -> programa; puerto de respuesta -> petición pendiente
with open(path, "rb") as f:
    for n, raw in enumerate(f, 1):
        l = raw.decode("latin1")
        m = name.search(l)
        if m and m.group(2) != "dyld": progs[m.group(1)] = m.group(2)
        m = send.search(l)
        if not m: continue
        pid, tid, dest, to, resp, mid, rcv = m.groups()
        waiting.pop(dest, None)               # un envío a un puerto de respuesta la contesta
        if resp != "0x0": waiting[resp] = (pid, tid, to, int(mid), n)   # también XPC asíncrono (respuesta por otro hilo)
verbose = "-v" in sys.argv
for resp, (pid, tid, to, mid, n) in sorted(waiting.items(), key=lambda kv: kv[1][4]):
    if (to == "1" or to == pid) and not verbose: continue     # a launchd, o a sí mismo (avisos internos, registro)
    print(f"{pid}.{tid} {progs.get(pid, '?'):<22} espera a {to:>5} {progs.get(to, '?'):<22} id={mid:<10} (línea {n})")
