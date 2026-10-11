#!/usr/bin/env python3
# datos_macos.py <raíz>: prepara el volumen de datos de un macOS completo extraído (tools/yaa.py), como hace el instalador
# en un Mac recién instalado:
#   1. /System/Volumes/Data se crea copiando la plantilla /System/Library/Templates/Data (con permisos, fechas, enlaces y
#      atributos extendidos), si aún no existe.
#   2. Los firmlinks de /usr/share/firmlinks (/Applications, /Library, /Users, /private...) unen el volumen del sistema con
#      el de datos. Linux no los tiene: cada uno se presenta como un enlace simbólico relativo hacia
#      System/Volumes/Data/<destino>. Si en el lado del sistema había contenido, se mueve antes al lado de datos.
# Es idempotente: se puede repetir sin estropear nada.
# Algunas carpetas de la plantilla son solo de root en macOS (p. ej. private/var/db/dslocal/nodes/Default, sin permiso de
# paso para nadie más): sin ser root, el script se vuelve a ejecutar en un espacio de usuario donde el usuario es root
# (unshare -r), igual que tahoe-run, y así puede leerlas; los archivos siguen siendo del usuario.
import os, shutil, sys

def copia(src, dst):
    if os.path.lexists(dst): return
    tmp = dst + ".copiando"                                # a medias no cuenta: se copia aparte y se renombra al final
    if os.path.lexists(tmp): shutil.rmtree(tmp)
    shutil.copytree(src, tmp, symlinks=True, copy_function=shutil.copy2)
    shutil.copystat(src, tmp)
    os.rename(tmp, dst)

def mezcla(src, dst):
    """Mueve el contenido de src (lado del sistema) a dst (lado de datos) sin pisar lo que ya haya en dst."""
    os.makedirs(dst, exist_ok=True)
    for n in os.listdir(src):
        a, b = os.path.join(src, n), os.path.join(dst, n)
        if os.path.isdir(a) and not os.path.islink(a) and os.path.isdir(b) and not os.path.islink(b): mezcla(a, b)
        elif not os.path.lexists(b): os.rename(a, b)
    shutil.rmtree(src)

def main():
    if os.getuid() != 0:
        os.execvp("unshare", ["unshare", "-r", sys.executable, os.path.abspath(__file__)] + sys.argv[1:])
    root = os.path.abspath(sys.argv[1])
    data = os.path.join(root, "System/Volumes/Data")
    tmpl = os.path.join(root, "System/Library/Templates/Data")
    if not os.path.isdir(data) or not os.listdir(data):
        if os.path.isdir(data): os.rmdir(data)
        copia(tmpl, data); print("volumen de datos creado desde la plantilla")
    n = 0
    for line in open(os.path.join(root, "usr/share/firmlinks")):
        if not line.strip(): continue
        sysp, datap = line.rstrip("\n").split("\t")
        s = os.path.join(root, sysp.lstrip("/")); d = os.path.join(data, datap)
        if os.path.islink(s): continue                                    # ya hecho
        if os.path.isdir(s): mezcla(s, d)
        else: os.makedirs(d, exist_ok=True)
        os.makedirs(os.path.dirname(s), exist_ok=True)
        os.symlink(os.path.relpath(d, os.path.dirname(s)), s)
        n += 1
    print(f"firmlinks: {n}")

if __name__ == "__main__":
    main()
