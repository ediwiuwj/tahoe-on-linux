#!/usr/bin/env python3
"""memoria.py <smaps.1> <smaps.2>: qué memoria anónima creció entre dos fotos de /proc/<pid>/smaps del mismo proceso.

Dice el total de Anonymous en cada foto y luego:
  - regiones nuevas (no estaban en la primera), agrupadas por tamaño, permisos y nombre: muchas iguales indican
    reservas que no se liberan (mmap / mach_vm_allocate / datos fuera de línea de mensajes);
  - regiones que ya estaban y crecieron (el malloc del programa, una pila...), las que más.
Las direcciones van en hexadecimal para buscarlas en el registro o con vmmap/sym.py.
"""
import re
import sys
from collections import defaultdict

CAB = re.compile(r'^([0-9a-f]+)-([0-9a-f]+) (\S+) \S+ \S+ \S+\s*(.*)$')


def leer(ruta):
    reg = {}
    cur = None
    for linea in open(ruta, errors='replace'):
        m = CAB.match(linea)
        if m:
            ini, fin = int(m.group(1), 16), int(m.group(2), 16)
            cur = {'ini': ini, 'tam': fin - ini, 'perm': m.group(3), 'nombre': m.group(4) or '[anónima]', 'anon': 0}
            reg[ini] = cur
        elif cur and linea.startswith('Anonymous:'):
            cur['anon'] = int(linea.split()[1])
    return reg


def mb(kb):
    return f'{kb / 1024:.1f} MB'


def main():
    a, b = leer(sys.argv[1]), leer(sys.argv[2])
    ta, tb = sum(r['anon'] for r in a.values()), sum(r['anon'] for r in b.values())
    print(f'Anonymous: {mb(ta)} -> {mb(tb)} ({mb(tb - ta)} más), regiones {len(a)} -> {len(b)}')

    nuevas = [r for k, r in b.items() if k not in a]
    grupos = defaultdict(lambda: [0, 0, []])
    for r in nuevas:
        g = grupos[(r['tam'], r['perm'], r['nombre'])]
        g[0] += 1
        g[1] += r['anon']
        g[2].append(r['ini'])
    print(f'\nregiones nuevas: {len(nuevas)}, con {mb(sum(r["anon"] for r in nuevas))} anónimos')
    for (tam, perm, nombre), (n, anon, dirs) in sorted(grupos.items(), key=lambda x: -x[1][1])[:15]:
        ej = ' '.join(hex(d) for d in sorted(dirs)[:3])
        print(f'  {n:5d} × {tam // 1024:8d} KB {perm} {nombre[:40]:40s} anon={mb(anon):>9s}  p. ej. {ej}')

    crecen = []
    for k, r in b.items():
        if k in a and r['anon'] > a[k]['anon']:
            crecen.append((r['anon'] - a[k]['anon'], r, a[k]))
    crecen.sort(key=lambda x: -x[0])
    print(f'\nregiones que crecieron: {len(crecen)}, con {mb(sum(c[0] for c in crecen))} más')
    for d, r, ra in crecen[:15]:
        cambio = f' (tamaño {ra["tam"] // 1024} -> {r["tam"] // 1024} KB)' if ra['tam'] != r['tam'] else ''
        print(f'  +{mb(d):>9s} {hex(r["ini"])} {r["tam"] // 1024:8d} KB {r["perm"]} {r["nombre"][:40]}{cambio}')

    idas = [r for k, r in a.items() if k not in b]
    if idas:
        print(f'\nregiones liberadas: {len(idas)}, con {mb(sum(r["anon"] for r in idas))} anónimos')


if __name__ == '__main__':
    main()
