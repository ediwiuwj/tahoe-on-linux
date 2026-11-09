#!/usr/bin/env python3
# gen_tbd.py <símbolos> <salida>: descripciones .tbd de las bibliotecas de macOS para enlazar programas en Rust (o C)
# hechos aquí para el macOS de tahoe-run, sin el SDK de Apple. <símbolos> es la carpeta de listados de símbolos de la
# caché de dyld (~/.cache/tahoe-syms/<caché>/, una por imagen: «0xdirección: (tipo) símbolo»); <salida> recibe
# libSystem.tbd (con todo lo que libSystem.B.dylib reexporta: libsystem_*, libdyld, libdispatch, libxpc...), los
# alias libc/libm/libiconv y libobjc.tbd. Se genera a partir del macOS de cada cual: no se sube nada de Apple.
# Uso con Cargo: ver tools/macos-rs/LEEME.md.
import glob, os, re, sys

if len(sys.argv) != 3: sys.exit(__doc__ if __doc__ else "uso: gen_tbd.py <símbolos> <salida>")
src, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
SISTEMA = ["libsystem_", "libdyld", "libunwind", "libcompiler_rt", "libdispatch", "libxpc", "libcache", "libcommonCrypto",
           "libcopyfile", "libremovefile", "libkeymgr", "libquarantine", "libmacho", "libcorecrypto"]

def exportados(f):
    s = set()
    for l in open(f, errors="replace"):
        m = re.match(r"0x([0-9a-f]+):\s+\(([^)]*)\)\s+(\S+)", l)
        if not m or int(m.group(1), 16) == 0 or "private" in m.group(2): continue
        k = m.group(2)
        if "regular" in k or "external" in k or "re-exported" in k or "resolver" in k: s.add(m.group(3))
    return s

def tbd(nombre, instalacion, simbolos, objc=()):
    with open(os.path.join(out, nombre), "w") as o:
        o.write("--- !tapi-tbd\ntbd-version: 4\ntargets: [ x86_64-macos ]\n")
        o.write(f"install-name: '{instalacion}'\ncurrent-version: 1\nexports:\n  - targets: [ x86_64-macos ]\n")
        o.write("    symbols: [ " + ", ".join(f"'{x}'" for x in sorted(simbolos)) + " ]\n")
        if objc: o.write("    objc-classes: [ " + ", ".join(sorted(objc)) + " ]\n")
        o.write("...\n")

sistema = set()
for f in glob.glob(os.path.join(src, "*.txt")):
    if any(os.path.basename(f).startswith(p) for p in SISTEMA): sistema |= exportados(f)
tbd("libSystem.tbd", "/usr/lib/libSystem.B.dylib", sistema)
for alias in ("libc", "libm", "libiconv", "libpthread"):
    d = os.path.join(out, alias + ".tbd")
    if os.path.lexists(d): os.remove(d)
    os.symlink("libSystem.tbd", d)
objc = exportados(os.path.join(src, "libobjc.A.dylib.txt"))
clases = {x[len("_OBJC_CLASS_$_"):] for x in objc if x.startswith("_OBJC_CLASS_$_")}
tbd("libobjc.tbd", "/usr/lib/libobjc.A.dylib", {x for x in objc if not x.startswith(("_OBJC_CLASS_$_", "_OBJC_METACLASS_$_"))}, clases)
print(f"{out}: libSystem ({len(sistema)} símbolos), libobjc ({len(objc)})")
