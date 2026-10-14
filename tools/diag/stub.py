#!/usr/bin/env python3
# stub.py <dir_hex>...: destino de cada stub de la caché compartida de dyld ("jmp [rip+d32]", ff 25): lee el puntero de
# su entrada de la GOT en la caché y lo decodifica (puntero encadenado v5: base de la caché + 34 bits bajos), y da su
# imagen y símbolo con sym.py. Ejemplo de salida:
#   7ff80f458ef2 -> 0x7ff806c53924 IOKit _IOConnectCallMethod+0x0
# TAHOE_ROOT: sistema de macOS extraído (por omisión ~/macos26/base/macOS Base System).
import glob, os, struct, subprocess, sys
ROOT = os.environ.get("TAHOE_ROOT", os.path.expanduser("~/macos26/base/macOS Base System"))
B = next((os.path.join(ROOT, c) for c in ("System/Library/dyld/dyld_shared_cache_x86_64",       # Recovery
          "System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_x86_64h",    # macOS completo
          "System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_x86_64")
          if os.path.exists(os.path.join(ROOT, c))), None) or sys.exit("no encuentro la caché de dyld en " + ROOT)
BASE = 0x7ff800000000                                            # dirección de la caché (sin slide)
maps = []                                                        # (dirección, tamaño, archivo, desplazamiento)
for fn in [B] + sorted(glob.glob(B + ".[0-9]*")):
    if fn.endswith((".map", "symbols", ".atlas")): continue
    with open(fn, "rb") as f:
        h = f.read(0x200)
        mo, mc = struct.unpack_from("<II", h, 0x10)               # mappingOffset, mappingCount
        f.seek(mo)
        for _ in range(mc):
            a, s, o = struct.unpack_from("<QQQ", f.read(32))
            maps.append((a, s, fn, o))


def read(va, n):
    for a, s, fn, o in maps:
        if a <= va < a + s:
            with open(fn, "rb") as f:
                f.seek(o + va - a)
                return f.read(n)
    return None


out = []
for arg in sys.argv[1:]:
    va = int(arg, 16)
    ins = read(va, 6)
    if not ins or ins[:2] != b"\xff\x25":
        out.append((arg, None, "no es un stub jmp [rip+d32]")); continue
    got = va + 6 + struct.unpack_from("<i", ins, 2)[0]
    raw = read(got, 8)
    if not raw:
        out.append((arg, None, "GOT fuera de la caché")); continue
    v = struct.unpack("<Q", raw)[0]
    t = v if BASE <= v < BASE + (1 << 34) else BASE + (v & 0x3ffffffff)   # ya resuelto, o encadenado (v5)
    out.append((arg, t, None))
names = {}
targets = ["%#x" % t for _, t, _ in out if t]
if targets:
    r = subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), "sym.py"), *targets],
                       capture_output=True, text=True).stdout
    for l in r.splitlines():
        p = l.split(None, 1)
        if len(p) == 2: names[int(p[0], 16)] = p[1]
for a, t, err in out:
    print(f"{a} -> {err}" if err else f"{a} -> {t:#x} {names.get(t, '?')}")
