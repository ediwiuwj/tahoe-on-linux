#!/usr/bin/env python3
"""inventario.py <inventario.txt> [registro]: clasifica el resultado de tools/inventario_total.sh.

Cada línea del inventario: «n|programa|pid|código|segundos|primera línea de la salida». Con el registro de tahoe-run del
mismo arranque se añade, por pid, lo que el emulador anotó: llamadas sin implementar ([FALTA]), abortos y señales.
Categorías:
  funciona            sale con 0
  pide argumentos     sale con error mostrando su uso («usage», «illegal option»...): el programa arranca y responde
  error del emulador  el mensaje o el registro apuntan a tahoe-run (llamada sin implementar, función no implementada,
                      dirección inválida, librería o símbolo que falta...)
  se cae              muere por una señal
  cuelga              no termina en el plazo
  otro error          el resto (necesita permisos, hardware, configuración...)
  saltada             no se ejecutó (acaba el arranque o es un servicio de launchd)
"""
import re
import sys
from collections import Counter, defaultdict

USO = re.compile(r"usage|uso:|illegal option|unknown option|invalid option|requires? (an )?argument|missing|expected|"
                 r"not enough|too few|no (input|file|command|operand)|specify|options?:|syntax|help|must|need",
                 re.I)
EMU = re.compile(r"not implemented|function not implemented|library not loaded|symbol not found|bad system call|"
                 r"invalid address|init failed|\(os/kern\)|kern_return|mach_|failed to get|unable to (obtain|get|connect)|"
                 r"connection (to .*)?(failed|invalid|interrupted)|xpc|operation not supported", re.I)
SEÑALES = {4: "SIGILL", 5: "SIGTRAP", 6: "SIGABRT", 7: "SIGBUS", 8: "SIGFPE", 9: "SIGKILL", 10: "SIGBUS", 11: "SIGSEGV",
           12: "SIGSYS", 13: "SIGPIPE", 15: "SIGTERM"}


def registro(path):
    falta = defaultdict(set)
    causa = {}
    if not path:
        return falta, causa
    for l in open(path, errors="replace"):
        m = re.search(r"\[FALTA\] <(\d+):\d+> (\w+ \d+ \w+)", l)
        if m:
            falta[m.group(1)].add(m.group(2))
            continue
        m = re.search(r"abort <(\d+)>: (\[[^\]]+\] .*)", l)
        if m and m.group(1) not in causa:
            causa[m.group(1)] = m.group(2)[:120]
            continue
        m = re.search(r"(SIG[A-Z]+) <(\d+)> .*rip=(0x[0-9a-f]+)", l)
        if m and m.group(2) not in causa:
            causa[m.group(2)] = f"{m.group(1)} en {m.group(3)}"
    return falta, causa


def main():
    falta, causa = registro(sys.argv[2] if len(sys.argv) > 2 else None)
    cats = defaultdict(list)
    por_falta = Counter()
    for l in open(sys.argv[1], errors="replace"):
        p = l.rstrip("\n").split("|", 5)
        if len(p) < 6:
            continue
        n, prog, pid, rc, seg, out = p
        f = falta.get(pid, set())
        for x in f:
            por_falta[x] += 1
        if rc == "saltada":
            cat = "saltada"
        elif rc == "COLGADA":
            cat = "cuelga"
        elif rc == "0":
            cat = "funciona"
        elif rc.isdigit() and 128 < int(rc) <= 128 + 31:          # 128 + señal (255 es exit(-1), no una señal)
            cat = "se cae"
            out = f"{SEÑALES.get(int(rc) - 128, 'señal %d' % (int(rc) - 128))}: {causa.get(pid, out)}"
        elif EMU.search(out) or f:
            cat = "error del emulador"
            if f and not EMU.search(out):
                out = f"{out}  [FALTA: {', '.join(sorted(f))[:80]}]"
        elif USO.search(out):
            cat = "pide argumentos"
        else:
            cat = "otro error"
        cats[cat].append((prog, rc, out))
    total = sum(len(v) for v in cats.values())
    orden = ["funciona", "pide argumentos", "otro error", "error del emulador", "se cae", "cuelga", "saltada"]
    print(f"== {total} programas")
    for c in orden:
        print(f"   {c:20s} {len(cats[c]):5d}")
    if por_falta:
        print("== llamadas sin implementar ([FALTA]) y a cuántos programas afectan")
        for x, k in por_falta.most_common(20):
            print(f"   {k:4d}  {x}")
    for c in ["se cae", "cuelga", "error del emulador", "otro error"]:
        if cats[c]:
            print(f"== {c} ({len(cats[c])})")
            for prog, rc, out in cats[c]:
                print(f"   {prog:42s} {rc:8s} {out[:110]}")


if __name__ == "__main__":
    main()
