#!/usr/bin/env python3
"""Desensambla código x86_64 de una dyld_shared_cache de macOS (ipsw solo desensambla arm64).

Lee los bytes de la dirección pedida en el archivo de la caché que la mapea (principal o subcachés .01, .02...),
los pasa por `objdump -b binary -m i386:x86-64` y anota los destinos de call/jmp/lea con los nombres de las listas
de símbolos de ~/.cache/tahoe-syms (las que genera tahoe-run al arrancar).

Uso: dsc_dis.py <caché> <símbolo o 0xdirección> [bytes]   (por defecto 0x400 bytes)
"""
import glob
import os
import re
import struct
import subprocess
import sys
import tempfile


def mapeos(ruta):
    with open(ruta, 'rb') as f:
        h = f.read(0x40)
        off, cnt = struct.unpack_from('<II', h, 0x10)
        f.seek(off)
        crudo = f.read(32 * cnt)
    return [struct.unpack_from('<QQQII', crudo, 32 * i)[:3] for i in range(cnt)]   # dirección, tamaño, posición


def leer(cache, dir_, n):
    for arch in sorted(glob.glob(cache + '*')):
        if arch.endswith('.map') or arch.endswith('.atlas'):
            continue
        try:
            ms = mapeos(arch)
        except (OSError, struct.error):
            continue
        for addr, tam, pos in ms:
            if addr <= dir_ < addr + tam:
                with open(arch, 'rb') as f:
                    f.seek(pos + dir_ - addr)
                    return f.read(min(n, addr + tam - dir_))
    sys.exit(f'0x{dir_:x} no está en la caché')


def simbolos():
    tabla = {}
    for txt in glob.glob(os.path.expanduser('~/.cache/tahoe-syms/*/*.txt')):
        img = os.path.basename(txt)[:-4]
        with open(txt, errors='replace') as f:
            for ln in f:
                m = re.match(r'(0x[0-9a-f]+): \([^)]*\)\s+(\S.*)$', ln.strip())
                if m:
                    tabla.setdefault(int(m.group(1), 16), f'{m.group(2)} [{img}]')
    return tabla


def main():
    cache, quien = sys.argv[1], sys.argv[2]
    n = int(sys.argv[3], 0) if len(sys.argv) > 3 else 0x400
    sims = simbolos()
    if quien.startswith('0x'):
        dir_ = int(quien, 16)
    else:
        cands = [a for a, s in sims.items() if s.split(' [')[0] == quien]
        if not cands:
            sys.exit(f'no encuentro {quien}')
        dir_ = cands[0]
    datos = leer(cache, dir_, n)
    with tempfile.NamedTemporaryFile(suffix='.bin') as t:
        t.write(datos)
        t.flush()
        sal = subprocess.run(['objdump', '-D', '-b', 'binary', '-m', 'i386:x86-64', f'--adjust-vma=0x{dir_:x}', t.name],
                             capture_output=True, text=True).stdout
    def cadena(a):
        try:
            b = leer(cache, a, 200)
        except SystemExit:
            return None
        z = b.split(b'\0')[0]
        if 3 <= len(z) and all(32 <= c < 127 for c in z):
            return z.decode()
        return None

    def puntero(a):
        try:
            b = leer(cache, a, 8)
        except SystemExit:
            return None
        if len(b) < 8:
            return None
        return struct.unpack('<Q', b)[0] & 0xfffffffffff   # sin los bits altos de las cadenas de punteros

    for ln in sal.splitlines():
        m = re.search(r'0x([0-9a-f]{9,})', ln)
        if m:
            a = int(m.group(1), 16)
            if a in sims:
                ln += f'    ; {sims[a]}'
            else:
                c = cadena(a)
                q = puntero(a)
                if c:
                    ln += f'    ; "{c}"'
                elif q and q in sims:
                    ln += f'    ; -> {sims[q]}'
                elif q and cadena(q):
                    ln += f'    ; -> "{cadena(q)}"'
                elif q:
                    # CFString constante: {isa, flags, char*, longitud}
                    r = puntero(a + 16)
                    if r and cadena(r):
                        ln += f'    ; @"{cadena(r)}"'
        m2 = re.match(r'\s*([0-9a-f]+):', ln)
        if m2 and int(m2.group(1), 16) in sims and int(m2.group(1), 16) != dir_:
            print(f'--- {sims[int(m2.group(1), 16)]}')
        print(ln)


if __name__ == '__main__':
    main()
