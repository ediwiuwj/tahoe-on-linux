#!/usr/bin/env python3
# dis.py <dir_hex> [bytes]: desensambla (objdump, sintaxis Intel) los bytes de la caché compartida de dyld de macOS que
# empiezan en esa dirección (200 bytes por omisión). Busca la dirección en las asignaciones de la caché y sus subcachés.
# TAHOE_ROOT: sistema de macOS extraído (por omisión ~/macos26/base/macOS Base System).
import glob, os, struct, subprocess, sys, tempfile
ROOT = os.environ.get("TAHOE_ROOT", os.path.expanduser("~/macos26/base/macOS Base System"))
B = os.path.join(ROOT, "System/Library/dyld/dyld_shared_cache_x86_64")
va = int(sys.argv[1], 16); n = int(sys.argv[2]) if len(sys.argv) > 2 else 200
for fn in [B] + sorted(glob.glob(B + ".[0-9]*")):
    if fn.endswith(".map") or fn.endswith("symbols"): continue
    f = open(fn, 'rb'); h = f.read(0x200)
    mo, mc = struct.unpack_from('<II', h, 0x10)                  # mappingOffset, mappingCount
    f.seek(mo)
    for i in range(mc):
        a, s, o = struct.unpack_from('<QQQ', f.read(32))           # address, size, fileOffset
        if a <= va < a + s:
            f.seek(o + (va - a)); d = f.read(n)
            with tempfile.NamedTemporaryFile(suffix=".bin") as tf:
                tf.write(d); tf.flush()
                print(subprocess.run(['objdump', '-D', '-b', 'binary', '-mi386:x86-64', '-Mintel', '--adjust-vma=%#x' % va, tf.name],
                                     capture_output=True, text=True).stdout)
            sys.exit()
print("no encontrado")
