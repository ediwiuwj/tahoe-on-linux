#!/usr/bin/env python3
# sym.py <dir_hex>...: imagen, símbolo y desplazamiento de direcciones de la caché compartida de dyld de macOS.
# Los símbolos se sacan con `ipsw dyld symaddr` (https://github.com/blacktop/ipsw) y se guardan por imagen en
# ~/.cache/tahoe-syms. Sin ipsw, o si la imagen no tiene el símbolo, da imagen + desplazamiento.
# TAHOE_ROOT: sistema de macOS extraído (por omisión ~/macos26/base/macOS Base System).
import bisect, os, re, struct, subprocess, sys
ROOT = os.environ.get("TAHOE_ROOT", os.path.expanduser("~/macos26/base/macOS Base System"))
def find_cache(root):
    # la de la Recovery está en el sistema; la de un macOS completo, en el cryptex de Preboot (x86_64h en Haswell o posterior)
    for c in ("System/Library/dyld/dyld_shared_cache_x86_64",
              "System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_x86_64h",
              "System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_x86_64"):
        if os.path.exists(os.path.join(root, c)): return os.path.join(root, c)
    sys.exit("no encuentro la caché de dyld en " + root)
C = find_cache(ROOT)
# símbolos guardados por caché (la Recovery y el sistema completo son versiones distintas)
D = os.path.expanduser("~/.cache/tahoe-syms/%s-%x" % (os.path.basename(C), os.path.getsize(C))); os.makedirs(D, exist_ok=True)
f = open(C, 'rb'); io, ic = struct.unpack_from('<II', f.read(0x1c8), 0x1c0)   # imagesOffset, imagesCount
f.seek(io); t = f.read(ic * 32); im = []
for i in range(ic):
    a, mt, ino, po, pad = struct.unpack_from('<QQQII', t, i * 32)
    p = f.tell(); f.seek(po); n = f.read(300).split(b'\0')[0].decode(); f.seek(p); im.append((a, n))
im.sort(); ad = [a for a, _ in im]
cache = {}
def syms(name):
    if name in cache: return cache[name]
    fn = os.path.join(D, name + ".txt")
    if not os.path.exists(fn):
        try: out = subprocess.run(["ipsw", "dyld", "symaddr", C, "--image", name], capture_output=True, text=True).stdout
        except FileNotFoundError: out = ""
        if out: open(fn, "w").write(out)
    L = []
    if os.path.exists(fn):
        for ln in open(fn):
            m = re.match(r"0x([0-9a-f]+): \([^)]*\)\s+(\S+)", ln)
            if m: L.append((int(m.group(1), 16), m.group(2)))
    L.sort(); cache[name] = ([a for a, _ in L], L); return cache[name]
for s in sys.argv[1:]:
    v = int(s, 16); k = bisect.bisect(ad, v) - 1
    if k < 0 or not im[k][1]: print(s, "?"); continue
    name = im[k][1].split('/')[-1]
    A, L = syms(name); j = bisect.bisect(A, v) - 1
    print(s, name, (L[j][1] + "+0x%x" % (v - L[j][0])) if j >= 0 else hex(v - im[k][0]))
