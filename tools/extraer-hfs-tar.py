#!/usr/bin/env python3
"""Extrae un tar HFS en un directorio vacío, sin enlaces fuera de su raíz."""
import argparse
import os
from pathlib import Path, PurePosixPath
import tarfile


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('archivo', type=Path)
    p.add_argument('destino', type=Path)
    a = p.parse_args()
    destino = a.destino.resolve()
    if not destino.is_dir() or any(destino.iterdir()):
        raise SystemExit('El destino debe existir y estar vacío')

    def filtro(m, path):
        # hfstar puede guardar nombres desde /. Se elimina solo ese prefijo;
        # componentes .. y archivos especiales siguen prohibidos por data_filter.
        m.name = m.name.lstrip('/')
        if m.islnk():
            m.linkname = m.linkname.lstrip('/')
        elif m.issym() and m.linkname.startswith('/'):
            target = destino / m.linkname.lstrip('/')
            parent = destino / str(PurePosixPath(m.name).parent)
            m.linkname = os.path.relpath(target, parent)
        if m.ischr() or m.isblk() or m.isfifo():
            return None
        # No conservar bits setuid/setgid ni escritura de grupo/otros.
        return tarfile.data_filter(m, path)

    with tarfile.open(a.archivo) as t:
        t.extractall(destino, filter=filtro)
    print('Extracción terminada:', destino)


if __name__ == '__main__':
    main()
