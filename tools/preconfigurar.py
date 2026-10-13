#!/usr/bin/env python3
"""preconfigurar.py: deja el macOS completo configurado como lo hacen las empresas, sin pasar por el Asistente de
configuración, para probar con un usuario normal y no dudar de si algo falla «porque macOS no está configurado».

  preconfigurar.py aplicar <raíz> <usuario> [nombre completo] [idioma]   (idioma por omisión es_MX)
  preconfigurar.py deshacer <raíz>

Se ejecuta desde tools/preconfigurar.sh, dentro de un espacio de usuarios de Linux en el que se es root (igual que
tahoe-run): parte de /private/var/db/dslocal no es legible para el usuario de Linux.

aplicar:
  1. Copia de seguridad de lo que se toca en ~/.config/tahoe-run/preconfig/ (fuera del repositorio).
  2. Crea un administrador como lo haría sysadminctl: registro en dslocal (uid 501, grupo staff 20, shell zsh, UUID
     propio), contraseña con SALTED-SHA512-PBKDF2 (lo que guarda macOS en ShadowHashData; la contraseña se pide aquí y no
     se guarda en ningún sitio), alta en el grupo admin y carpeta /Users/<usuario> copiada de la plantilla de macOS.
  3. Idioma y región en /Library/Preferences/.GlobalPreferences.plist (AppleLanguages, AppleLocale).
  4. /private/var/db/.AppleSetupDone: el Asistente de configuración se da por hecho. El índice de dslocal (sqlindex) se
     aparta para que opendirectoryd lo rehaga con el usuario nuevo.
  Si algo falla a mitad, se deshace lo hecho.
deshacer: restaura la copia de seguridad, borra el usuario y su carpeta y quita .AppleSetupDone.
"""
import getpass
import hashlib
import json
import os
import plistlib
import shutil
import sys
import time
import uuid

GUARDA = os.path.expanduser("~/.config/tahoe-run/preconfig")
DSLOCAL = "private/var/db/dslocal/nodes/Default"
GLOBAL = "Library/Preferences/.GlobalPreferences.plist"
SETUPDONE = "private/var/db/.AppleSetupDone"
UID = 501
ITERACIONES = 45000                          # del orden de lo que usa macOS para las cuentas locales


def leer(p):
    with open(p, "rb") as f:
        return plistlib.load(f)


def escribir(p, d):
    tmp = p + ".tmp"
    with open(tmp, "wb") as f:
        plistlib.dump(d, f, fmt=plistlib.FMT_BINARY)
    os.replace(tmp, p)


def respaldo(raiz, rels):
    os.makedirs(GUARDA, exist_ok=True)
    estado = os.path.join(GUARDA, "estado.json")
    if os.path.exists(estado):
        sys.exit("ya hay una preconfiguración aplicada (usa «deshacer» antes de volver a aplicar)")
    copias = {}
    for rel in rels:
        src = os.path.join(raiz, rel)
        if os.path.exists(src):
            dst = os.path.join(GUARDA, rel.replace("/", "__"))
            shutil.copy2(src, dst)
            copias[rel] = dst
        else:
            copias[rel] = None                   # no existía: al deshacer se borra
    return estado, copias


def aplicar(raiz, usuario, nombre, idioma):
    if not usuario.isascii() or not usuario.replace("_", "").isalnum() or usuario[0].isdigit() or usuario != usuario.lower():
        sys.exit("el nombre corto debe ser minúsculas, letras y números (p. ej. «ana»)")
    users = os.path.join(raiz, DSLOCAL, "users")
    if os.path.exists(os.path.join(raiz, "Users", usuario)):
        sys.exit(f"ya existe /Users/{usuario}")
    if os.path.exists(os.path.join(users, usuario + ".plist")):
        sys.exit(f"el usuario {usuario} ya existe en este macOS")
    for f in os.listdir(users):                  # uid libre
        try:
            if str(UID) in leer(os.path.join(users, f)).get("uid", []):
                sys.exit(f"el uid {UID} ya está en uso ({f})")
        except Exception:
            pass
    clave = getpass.getpass(f"Contraseña para {usuario} en macOS: ")
    if clave != getpass.getpass("Repítela: "):
        sys.exit("no coinciden")
    if not clave:
        sys.exit("la contraseña no puede estar vacía")

    # El índice de dslocal (sqlindex) se aparta como hacen las herramientas de despliegue: opendirectoryd lo rehace al
    # arrancar con el usuario nuevo. Se devuelve al deshacer.
    indices = [f"{DSLOCAL}/{n}" for n in ("sqlindex", "sqlindex-shm", "sqlindex-wal")]
    rels = [f"{DSLOCAL}/groups/admin.plist", GLOBAL, SETUPDONE] + indices
    estado, copias = respaldo(raiz, rels)
    with open(estado, "w") as f:               # apuntado antes de tocar nada: si algo falla, se puede deshacer
        json.dump({"usuario": usuario, "casa": f"/Users/{usuario}", "copias": copias, "fecha": time.strftime("%Y-%m-%d %H:%M")}, f, indent=1)
    try:
        hacer(raiz, usuario, nombre, idioma, clave, indices)
    except BaseException:
        print("algo falló: se deshace lo hecho", file=sys.stderr)
        deshacer(raiz)
        raise
    print(f"listo: usuario «{usuario}» (uid {UID}, administrador), idioma {idioma}, Asistente de configuración saltado.")
    print("para volver atrás: tools/preconfigurar.sh deshacer")


def hacer(raiz, usuario, nombre, idioma, clave, indices):
    users = os.path.join(raiz, DSLOCAL, "users")
    # 2. el usuario (formato de dslocal: cada valor es una lista)
    sal = os.urandom(32)
    entropia = hashlib.pbkdf2_hmac("sha512", clave.encode(), sal, ITERACIONES, 128)
    shadow = plistlib.dumps({"SALTED-SHA512-PBKDF2": {"entropy": entropia, "iterations": ITERACIONES, "salt": sal}},
                            fmt=plistlib.FMT_BINARY)
    guid = str(uuid.uuid4()).upper()
    casa = f"/Users/{usuario}"
    escribir(os.path.join(users, usuario + ".plist"), {
        "name": [usuario], "realname": [nombre], "uid": [str(UID)], "gid": ["20"], "home": [casa],
        "shell": ["/bin/zsh"], "generateduid": [guid], "passwd": ["********"],
        "authentication_authority": [";ShadowHash;HASHLIST:<SALTED-SHA512-PBKDF2>"], "ShadowHashData": [shadow],
        "hint": [""], "_writers_passwd": [usuario], "_writers_hint": [usuario], "_writers_picture": [usuario],
        "_writers_realname": [usuario], "_writers_UserCertificate": [usuario], "_writers_jpegphoto": [usuario],
    })
    admin_p = os.path.join(raiz, DSLOCAL, "groups", "admin.plist")
    admin = leer(admin_p)
    admin.setdefault("users", []).append(usuario)
    admin.setdefault("groupmembers", []).append(guid)
    escribir(admin_p, admin)

    plantilla = os.path.join(raiz, "System/Library/User Template")
    destino = os.path.join(raiz, casa.lstrip("/"))
    shutil.copytree(os.path.join(plantilla, "Non_localized"), destino, symlinks=True)
    loc = os.path.join(plantilla, "Spanish.lproj")
    if os.path.isdir(loc):
        shutil.copytree(loc, destino, symlinks=True, dirs_exist_ok=True)

    # 3. idioma y región
    gp_p = os.path.join(raiz, GLOBAL)
    # vacía (como viene en la imagen) o sin crear: sin preferencias; si tiene algo que no se puede leer, falla (y se deshace)
    gp = leer(gp_p) if os.path.exists(gp_p) and os.path.getsize(gp_p) > 0 else {}
    lengua = idioma.split("_")[0]
    gp["AppleLanguages"] = [idioma.replace("_", "-"), lengua]
    gp["AppleLocale"] = idioma
    escribir(gp_p, gp)

    # 4. Asistente de configuración hecho
    open(os.path.join(raiz, SETUPDONE), "w").close()
    for rel in indices:
        p = os.path.join(raiz, rel)
        if os.path.exists(p):
            os.remove(p)


def deshacer(raiz):
    estado = os.path.join(GUARDA, "estado.json")
    if not os.path.exists(estado):
        sys.exit("no hay ninguna preconfiguración aplicada")
    e = json.load(open(estado))
    for rel, copia in e["copias"].items():
        dst = os.path.join(raiz, rel)
        if copia and os.path.exists(copia):
            shutil.copy2(copia, dst)
            os.remove(copia)
        elif os.path.exists(dst):
            os.remove(dst)
    u = os.path.join(raiz, DSLOCAL, "users", e["usuario"] + ".plist")
    if os.path.exists(u):
        os.remove(u)
    casa = os.path.join(raiz, e["casa"].lstrip("/"))
    if os.path.isdir(casa):
        shutil.rmtree(casa)
    os.remove(estado)
    print(f"deshecho: sin el usuario «{e['usuario']}», idioma y Asistente de configuración como estaban.")


if __name__ == "__main__":
    if len(sys.argv) >= 4 and sys.argv[1] == "aplicar":
        aplicar(sys.argv[2], sys.argv[3], sys.argv[4] if len(sys.argv) > 4 else sys.argv[3],
                sys.argv[5] if len(sys.argv) > 5 else "es_MX")
    elif len(sys.argv) == 3 and sys.argv[1] == "deshacer":
        deshacer(sys.argv[2])
    else:
        sys.exit(__doc__)
