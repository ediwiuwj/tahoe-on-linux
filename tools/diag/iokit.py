#!/usr/bin/env python3
# iokit.py [-t] [-p pid|programa] [registro]: resumen de la traza de IOKit de tahoe-run (líneas «IOKIT»). La traza se
# activa con /dev/shm/tahoe-iokit-trace: vacío, todos los procesos; con nombres de programas, solo esos
# (tools/diag/discos.sh lo hace con IOKS='diskarbitrationd ...'). Por omisión lee ~/.cache/tahoe-diag/discos.log.
#
# Por proceso:
#   - peticiones agrupadas (llamada, objeto, argumentos) con cuántas veces y qué respondieron (puerto y objeto, o kr);
#   - errores devueltos (kr distinto de 0);
#   - notificaciones registradas (add_notification: tipo y diccionario; add_interest_notification) y los avisos que se le
#     enviaron. Un registro sin ningún aviso es sospechoso cuando el cliente espera a que aparezca algo.
#   -t           además, todas las peticiones en orden, con su respuesta
#   -p pid|prog  solo ese proceso (puede repetirse)
import os, re, sys
from collections import OrderedDict, defaultdict

args = sys.argv[1:]
full = "-t" in args
follow, rest = set(), []
i = 0
while i < len(args):
    if args[i] == "-t": pass
    elif args[i] == "-p" and i + 1 < len(args): follow.add(args[i + 1]); i += 1
    else: rest.append(args[i])
    i += 1
path = rest[0] if rest else os.path.expanduser(os.environ.get("TAHOE_DIAG_DIR", "~/.cache/tahoe-diag") + "/discos.log")

req_re = re.compile(r"IOKIT <(\d+)> (\S+)\((\d+)\)(?: \[([^\]]*)\])?(.*)$")
rep_re = re.compile(r"IOKIT <(\d+)>   -> (\S+): (.*)$")
avi_re = re.compile(r"IOKIT <(\d+)> aviso tipo=(\d+) al puerto (0x[0-9a-f]+) \(notificación (0x[0-9a-f]+)\) -> (-?\d+)")
prog_re = re.compile(r"<(\d+)> cargado (\S+) .*tipo=2")

progs, reqs, avisos = {}, defaultdict(list), defaultdict(list)
pending = defaultdict(list)                   # (pid, llamada) -> peticiones aún sin respuesta (varios hilos se entremezclan)
with open(path, "rb") as f:
    for n, raw in enumerate(f, 1):
        l = raw.decode("utf-8", "replace").rstrip("\n")
        m = prog_re.search(l)
        if m and m.group(2) != "dyld": progs[m.group(1)] = m.group(2)
        m = rep_re.search(l)
        if m:
            k = (m.group(1), m.group(2))
            if pending[k]: pending[k].pop(0)["rep"] = m.group(3)
            continue
        m = avi_re.search(l)
        if m:
            pid, ty, wake, notif, r = m.groups()
            avisos[notif].append(dict(n=n, pid=pid, tipo=int(ty), r=int(r)))
            continue
        m = req_re.search(l)
        if m:
            pid, call, cid, obj, arg = m.groups()
            q = dict(n=n, pid=pid, call=call, id=int(cid), obj=obj or "", arg=arg.strip(), rep=None)
            reqs[pid].append(q); pending[(pid, call)].append(q)

if not reqs:
    sys.exit(f"sin líneas IOKIT en {path} (¿se activó /dev/shm/tahoe-iokit-trace?)")

def name(pid): return f"{progs.get(pid, '?')}[{pid}]"
def short(s, k=150): s = re.sub(r"\s+", " ", s); return s if len(s) <= k else s[:k] + "…"
def notif_port(rep):
    m = re.search(r"puerto (0x[0-9a-f]+)", rep or ""); return m.group(1) if m else None

pids = [p for p in reqs if not follow or p in follow or progs.get(p) in follow]
for pid in pids:
    rs = reqs[pid]
    print(f"== {name(pid)}: {len(rs)} peticiones")
    if full:
        for q in rs:
            print(f"   {q['n']:>7} {q['call']}" + (f" [{q['obj']}]" if q["obj"] else "") + (f" {short(q['arg'], 110)}" if q["arg"] else "")
                  + f"  -> {q['rep'] or '(sin respuesta anotada)'}")
    grp = OrderedDict()
    for q in rs:
        k = (q["call"], q["obj"], re.sub(r"0x[0-9a-f]{5,}", "N", q["arg"]))
        g = grp.setdefault(k, dict(c=0, reps=OrderedDict()))
        g["c"] += 1
        r = re.sub(r"puerto 0x[0-9a-f]+ ", "", q["rep"] or "?"); g["reps"][r] = g["reps"].get(r, 0) + 1
    print("   -- peticiones (veces, llamada, objeto, argumentos -> respuestas)")
    for (call, obj, arg), g in list(grp.items())[:60]:
        reps = ", ".join(f"{r}" + (f" ×{c}" if c > 1 else "") for r, c in list(g["reps"].items())[:4])
        print(f"   {g['c']:>4} {call}" + (f" [{obj}]" if obj else "") + (f" {short(arg, 100)}" if arg else "") + f"  -> {short(reps, 120)}")
    errs = [q for q in rs if q["rep"] and re.search(r"kr=0x(?!0 )[0-9a-f]+", q["rep"]) and "kr=0x0 " not in q["rep"] + " "]
    if errs:
        print("   -- errores devueltos")
        for q in errs[:20]:
            print(f"   {q['n']:>7} {q['call']}" + (f" [{q['obj']}]" if q["obj"] else "") + f" {short(q['arg'], 90)} -> {q['rep']}")
    notifs = [q for q in rs if q["call"] in ("service_add_notification", "service_add_notification_ool",
                                             "service_add_notification_bin", "service_add_interest_notification")
              or q["id"] in (2868, 2870, 2884, 2869)]
    if notifs:
        print("   -- notificaciones registradas (avisos enviados)")
        for q in notifs:
            p = notif_port(q["rep"]); av = avisos.get(p, []) if p else []
            tipos = ", ".join(sorted({str(a["tipo"]) for a in av})) or "-"
            print(f"   {q['n']:>7} {q['call']}" + (f" [{q['obj']}]" if q["obj"] else "") + f" {short(q['arg'], 100)}"
                  + f"\n            -> notificación {p or '?'}: {len(av)} avisos (tipos {tipos})" + ("   <- NINGÚN AVISO" if not av else ""))
