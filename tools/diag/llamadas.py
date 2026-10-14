#!/usr/bin/env python3
# llamadas.py <dir_inicio_hex> <bytes> [función...]: lista las llamadas de un trozo de la caché de dyld (normalmente el
# código de un framework) a las funciones indicadas (por omisión, las de los user clients de IOKit: IOConnectCallMethod,
# IOConnectCallAsyncMethod, IOConnectCallStructMethod, IOConnectCallScalarMethod e IOConnectTrap0..6), con el selector o
# índice de trampa (lo que se carga en esi antes de la llamada) y la función de macOS que la contiene. Sirve para deducir
# la interfaz de un user client sin su código fuente:
#   tools/diag/llamadas.py 7ff80f445000 81000          (IOSurface.framework)
# Usa dis.py (desensamblar), stub.py (destino de cada stub) y sym.py (símbolos). TAHOE_ROOT como en ellos.
import os, re, subprocess, sys

DIAG = os.path.dirname(os.path.abspath(__file__))
WANT = sys.argv[3:] or ["IOConnectCallMethod", "IOConnectCallAsyncMethod", "IOConnectCallStructMethod",
                        "IOConnectCallScalarMethod"] + [f"IOConnectTrap{i}" for i in range(7)]


def run(*a):
    return subprocess.run([sys.executable, *a], capture_output=True, text=True).stdout


asm = run(os.path.join(DIAG, "dis.py"), sys.argv[1], sys.argv[2]).splitlines()
calls = []                      # (dirección, destino, carga de esi más reciente)
esi = "?"
for l in asm:
    m = re.match(r"\s*([0-9a-f]+):\s+(?:[0-9a-f]{2} )+\s*(.*)", l)
    if not m:
        continue
    ins = m.group(2)
    if re.match(r"mov\s+esi,", ins):
        esi = ins.split(",", 1)[1].strip()
    elif re.match(r"xor\s+esi,esi", ins):
        esi = "0x0"
    elif ins.startswith("call"):
        t = re.search(r"0x([0-9a-f]+)", ins)
        if t:
            calls.append((m.group(1), t.group(1), esi))
        esi = "?"                # una llamada estropea esi: el selector se carga justo antes de la suya
    elif re.match(r"(jmp|ret)", ins):
        esi = "?"

# Destinos: cada stub distinto se resuelve una vez.
targets = sorted({t for _, t, _ in calls})
dest = {}
out = run(os.path.join(DIAG, "stub.py"), *targets) if targets else ""
for l in out.splitlines():
    m = re.match(r"([0-9a-f]+) -> 0x[0-9a-f]+ \S+ _?(\S+?)\+0x0", l)
    if m:
        dest[m.group(1)] = m.group(2)
hits = [(a, dest[t], s) for a, t, s in calls if dest.get(t) in WANT]
if not hits:
    sys.exit("ninguna llamada a " + ", ".join(WANT))
names = {}
for l in run(os.path.join(DIAG, "sym.py"), *("0x" + a for a, _, _ in hits)).splitlines():
    p = l.split(None, 2)
    if len(p) == 3:
        names[p[0][2:]] = p[2]
rows = sorted(hits, key=lambda h: (h[1], int(h[2], 16) if h[2].startswith("0x") else 1 << 40))
for a, f, s in rows:
    print(f"{f:28} {s:>6}  {names.get(a, '?')}  (0x{a})")
