#!/usr/bin/env python3
# caidas.py [registro]: resumen de las caídas de un arranque (por omisión ~/.cache/tahoe-diag/msh.log).
# Cuenta, por programa de macOS, los mensajes de aborto (abort <pid>: [librería] "motivo"), los motivos de los
# informes de caída, las señales (SIGSEGV, SIGBUS, SIGILL) y las salidas con estado distinto de 0. Los volcados de "espera" y "traza" (STACKS) no son caídas.
import collections, os, re, sys

log = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/.cache/tahoe-diag/msh.log")
name = {}
msgs = collections.Counter()
sigs = collections.Counter()
exits = collections.Counter()
for l in open(log, errors="replace"):
    m = re.match(r"tahoe-run: <(\d+)> cargado (\S+)", l)
    if m and m.group(2) != "dyld":
        name[m.group(1)] = m.group(2)        # el último programa cargado por ese pid
        continue
    m = re.search(r'abort <(\d+)>: \[([^\]]+)\] "(.*)"', l)
    if m and m.group(3) != "abort() called":
        msgs[(name.get(m.group(1), "?"), m.group(3)[:110])] += 1
        continue
    m = re.search(r'=== motivo: "(.*)"', l)
    if m:
        msgs[("?", m.group(1)[:110])] += 1
    m = re.search(r"fin <(\d+)> \[([^\]]*)\] exit\((-?\d+)\)", l)
    if m:
        if m.group(3) != "0":
            exits[(m.group(2), f"exit({m.group(3)})")] += 1
        continue
    m = re.search(r"(SIGSEGV|SIGBUS|SIGILL) <(\d+)>", l)
    if m:
        sigs[(name.get(m.group(2), "?"), m.group(1))] += 1
for (n, msg), c in msgs.most_common(40):
    print(f"{c:3} {n:28} {msg}")
for (n, sig), c in sigs.most_common(20):
    print(f"{c:3} {n:28} {sig}")
for (n, st), c in exits.most_common(20):
    print(f"{c:3} {n:28} {st}")
if not msgs and not sigs and not exits:
    print("sin caídas")
