#!/usr/bin/env python3
# mensajes.py [-t] [-s programa] [registro]: conversación de mensajes Mach a partir de la traza «MSG» de tahoe-run
# (procesos nombrados en /dev/shm/tahoe-msg-trace; tools/diag/discos.sh la activa con MSGS='prog1 prog2...').
# Por omisión lee ~/.cache/tahoe-diag/discos.log.
#
# Para cada envío dice si llegó a su destino (hay una recepción de ese remitente con el mismo puerto e id) y, si pedía
# respuesta (puerto de respuesta), si alguien la envió a ese puerto. Al final, el resumen por pareja de programas y la
# lista de lo que se quedó a medias: envíos que nadie recibió y peticiones sin respuesta, que es donde se cuelga una
# cadena (diskutil -> storagekitd -> diskarbitrationd -> fskitd...).
#   -t           además, la conversación completa en orden (una línea por mensaje)
#   -s prog      solo los mensajes en los que participa ese programa (puede repetirse)
#   -p pid|prog  sigue un proceso: todos sus envíos y recepciones en orden (con un nombre, todos los pid con ese programa,
#                p. ej. el diskarbitrationd de arranque y el relanzado)
# Un destino sin traza (no está en la lista) sale como «(sin traza)»: se sabe que se envió, no si llegó.
import os, re, sys
from collections import Counter, defaultdict

args = sys.argv[1:]
full = "-t" in args
solo = set()
follow = set()
rest = []
i = 0
while i < len(args):
    if args[i] == "-t": pass
    elif args[i] == "-s" and i + 1 < len(args): solo.add(args[i + 1]); i += 1
    elif args[i] == "-p" and i + 1 < len(args): follow.add(args[i + 1]); i += 1
    else: rest.append(args[i])
    i += 1
path = rest[0] if rest else os.path.expanduser(os.environ.get("TAHOE_DIAG_DIR", "~/.cache/tahoe-diag") + "/discos.log")

snd_re = re.compile(r"MSG <(\d+)\.(\d+)> envía dest=(0x[0-9a-f]+)\(->(-?\d+)\) resp=(0x[0-9a-f]+) id=(-?\d+) tam=(\d+) bits=(0x[0-9a-f]+)( y espera)?(?: \| (.*))?")
rcv_re = re.compile(r"MSG <(\d+)\.(\d+)> recibe en=(0x[0-9a-f]+) dest=(0x[0-9a-f]+) resp=(0x[0-9a-f]+) id=(-?\d+) tam=(\d+) de=(-?\d+) -> (0x[0-9a-f]+)")
fail_re = re.compile(r"MSG <(\d+)\.(\d+)> envío a (0x[0-9a-f]+) id=(-?\d+) falló (0x[0-9a-f]+)")
prog_re = re.compile(r"<(\d+)> cargado (\S+) .*tipo=2")

progs = {}
sends, recvs = [], []
traced = set()
with open(path, "rb") as f:
    for n, raw in enumerate(f, 1):
        l = raw.decode("utf-8", "replace")
        m = prog_re.search(l)
        if m and m.group(2) != "dyld": progs[m.group(1)] = m.group(2)
        m = snd_re.search(l)
        if m:
            pid, tid, dest, to, resp, mid, size, bits, wait, txt = m.groups()
            traced.add(pid)
            sends.append(dict(n=n, pid=pid, tid=tid, dest=dest, to=to, resp=resp, id=int(mid), size=int(size),
                              bits=bits, wait=bool(wait), txt=(txt or "")[:90], prog=progs.get(pid, "?"), recv=None, reply=None))
            continue
        m = fail_re.search(l)
        if m:                             # el envío anterior de ese hilo no entró en la cola (llena con plazo 0, destino muerto)
            pid, tid, dest, mid, kr = m.groups()
            for x in reversed(sends[-50:]):
                if x["pid"] == pid and x["tid"] == tid and x["dest"] == dest and x["id"] == int(mid): x["fail"] = kr; break
            continue
        m = rcv_re.search(l)
        if m:
            pid, tid, en, dest, resp, mid, size, de, res = m.groups()
            traced.add(pid)
            recvs.append(dict(n=n, pid=pid, tid=tid, dest=dest, resp=resp, id=int(mid), size=int(size), de=de, res=res, used=False))

if not sends and not recvs:
    sys.exit(f"sin líneas MSG en {path} (¿se activó /dev/shm/tahoe-msg-trace con los nombres?)")

def name(pid): return f"{progs.get(pid, '?')}[{pid}]"

# llegada: la primera recepción posterior, del mismo remitente, al mismo puerto y con el mismo id
by_key = defaultdict(list)
for r in recvs: by_key[(r["de"], r["dest"], r["id"])].append(r)
for s in sends:
    if s.get("fail"): continue
    for r in by_key.get((s["pid"], s["dest"], s["id"]), []):
        if not r["used"] and r["n"] > s["n"]: r["used"] = True; s["recv"] = r; break
# respuesta: un envío posterior cuyo destino es el puerto de respuesta de la petición
by_dest = defaultdict(list)
for s in sends: by_dest[s["dest"]].append(s)
# o, aunque quien responde no tenga traza, una recepción posterior del que preguntó en ese puerto de respuesta
rx_by_port = defaultdict(list)
for r in recvs: rx_by_port[(r["pid"], r["dest"])].append(r)
for s in sends:
    if s["resp"] != "0x0":
        s["reply"] = next((x for x in by_dest.get(s["resp"], []) if x["n"] > s["n"]), None) or \
                     next((r for r in rx_by_port.get((s["pid"], s["resp"]), []) if r["n"] > s["n"] and r["res"] == "0x0"), None)

# Subsistemas MIG que aparecen en estas trazas (id = base del subsistema + número de rutina).
MIG = {1000: "notifyd", 11600: "logd", 20000: "configd", 400: "bootstrap", 3200: "mach_port", 3400: "host_priv",
       200: "host", 3800: "thread_act", 4800: "mach_vm", 2400: "clock", 6100: "DiskArbitration"}

def kind(mid):
    if mid in (64, 65, 66, 69, 70, 71, 72):
        return {64: "aviso", 65: "aviso PORT_DELETED", 66: "aviso", 69: "aviso PORT_DESTROYED", 70: "aviso NO_SENDERS",
                71: "aviso SEND_ONCE", 72: "aviso DEAD_NAME"}[mid]
    if mid & 0x10000000 or mid & 0x40000000: return "XPC"
    for base in sorted(MIG, reverse=True):
        if base <= mid < base + 100: return f"MIG {MIG[base]}#{mid - base}"
    return f"MIG {mid}" if mid >= 100 else f"id {mid}"

# 0x10000004 MACH_SEND_TIMED_OUT: cola llena con plazo (en los puertos de despertar de CFRunLoop/dispatch, de cola 1,
# es lo normal: ya hay un despertar pendiente). 0x10000003 INVALID_DEST: el destino ya no existe.
FALLOS = {"0x10000004": "cola llena (plazo agotado)", "0x10000003": "destino inválido"}

def state(s):
    if s.get("fail"): return "no enviado: " + FALLOS.get(s["fail"], s["fail"])
    if s["to"] not in traced: llegada = "(sin traza)"
    elif s["recv"]: llegada = "recibido" if s["recv"]["res"] == "0x0" else f"recibido con error {s['recv']['res']}"
    else: llegada = "NO RECIBIDO"
    if s["resp"] == "0x0": return llegada
    return llegada + (", respondido" if s["reply"] else ", SIN RESPUESTA")

# Petición que espera respuesta: el puerto de respuesta va como derecho de envío único (MAKE/MOVE_SEND_ONCE, 21/18),
# o el remitente se queda esperando en la misma llamada. Los avisos que se registran (puerto de respuesta con derecho de
# envío normal) no cuentan: se contestan cuando pasa algo, si pasa.
def espera_respuesta(s):
    return s["resp"] != "0x0" and (s["wait"] or ((int(s["bits"], 16) >> 8) & 0x1f) in (18, 21))

def interesa(s): return not solo or progs.get(s["pid"]) in solo or progs.get(s["to"]) in solo

print(f"== procesos con traza: {', '.join(sorted({name(p) for p in traced}))}")
if full:
    print("== conversación")
    for s in sends:
        if interesa(s):
            print(f"   {s['n']:>7} {name(s['pid']):>26} -> {name(s['to']):<26} {kind(s['id']):<16} {state(s)}"
                  + (f"   «{s['txt']}»" if s["txt"] else ""))

if follow:
    pids = {p for p in traced if p in follow or progs.get(p) in follow}
    print(f"== seguimiento de {', '.join(sorted(name(p) for p in pids)) or '(nadie con traza)'}")
    ev = [("->", x) for x in sends if x["pid"] in pids] + [("<-", r) for r in recvs if r["pid"] in pids]
    for d, x in sorted(ev, key=lambda e: e[1]["n"]):
        if d == "->":
            print(f"   {x['n']:>7} {name(x['pid'])}.{x['tid']} envía a {name(x['to'])} {kind(x['id'])} tam={x['size']}: {state(x)}"
                  + (f"   «{x['txt']}»" if x["txt"] else ""))
        else:
            err = "" if x["res"] == "0x0" else f" ERROR {x['res']}"
            print(f"   {x['n']:>7} {name(x['pid'])}.{x['tid']} recibe de {name(x['de'])} {kind(x['id'])} tam={x['size']} en {x['dest']}{err}")

print("== por pareja (mensajes, sin recibir, sin respuesta)")
pairs = Counter(); lost = Counter(); unans = Counter()
for s in sends:
    if not interesa(s): continue
    k = (name(s["pid"]), name(s["to"]))
    pairs[k] += 1
    if s["to"] in traced and not s["recv"] and not s.get("fail"): lost[k] += 1
    if s["resp"] != "0x0" and not s["reply"]: unans[k] += 1
for k, v in pairs.most_common(30):
    print(f"   {k[0]:>26} -> {k[1]:<26} {v:>5} {lost[k]:>5} {unans[k]:>5}")

print("== a medias (lo que puede colgar una cadena)")
malos = [s for s in sends if interesa(s) and s["pid"] != s["to"] and
         not s.get("fail") and ((s["to"] in traced and not s["recv"]) or (espera_respuesta(s) and not s["reply"]))]
for s in malos[-40:]:
    print(f"   línea {s['n']}: {name(s['pid'])} -> {name(s['to'])} {kind(s['id'])} (id {s['id']}): {state(s)}"
          + (f"   «{s['txt']}»" if s["txt"] else ""))
if not malos: print("   nada: todo lo enviado llegó y las peticiones que esperaban tuvieron respuesta")
