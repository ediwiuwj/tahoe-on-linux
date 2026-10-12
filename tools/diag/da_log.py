#!/usr/bin/env python3
# da_log.py [registro]: resume el registro de depuración de diskarbitrationd (/var/log/diskarbitrationd.log, que solo
# se escribe si diskarbitrationd corre con -d y el archivo ya existe; tools/diag/discos.sh lo prepara). Sin argumento
# lee la entrada estándar. Si el registro tiene varios arranques ("server has been started"), resume el último.
#
# Qué dice:
#   - cada disco: creado, sondeado (con qué sistema de archivos y resultado) y si se anunció a algún cliente ("disk
#     appeared"). Un disco creado que nunca se anuncia está atascado en las etapas de diskarbitrationd.
#   - cada cliente (diskutil, storagekitd...): las retrollamadas "idle" que registró y si se le enviaron. storagekitd
#     espera la suya con un semáforo sin plazo (DARegisterIdleCallback): si no llega, diskutil se queda colgado.
#   - un veredicto de una línea.
#
# Referencia (Recovery que funcionaba): los cuatro discos (disk0, disk0s1, disk1, disk1s1) se anuncian; solo disk1s1
# se sondea (apfs, success, con «fsck status 65»): los otros no tienen sistema de archivos y se anuncian sin sondeo.
import re, sys

lines = (open(sys.argv[1], errors="replace") if len(sys.argv) > 1 else sys.stdin).read().splitlines()
start = max([i for i, l in enumerate(lines) if "server has been started" in l] or [0])
lines = lines[start:]

disks = {}                  # disco -> {"creado", "sondeo": [..], "anunciado": set(clientes)}
def disk(d): return disks.setdefault(d, {"creado": False, "sondeo": [], "anunciado": set()})
cb = {}                     # (sesión, id) -> tipo
idle = {}                   # sesión -> {"pedidas": n, "enviadas": n}
sesiones = []               # en orden de aparición
cur = None                  # sesión de la cabecera actual ("cliente -> diskarbitrationd" o al revés)
hora_fin = ""

for l in lines:
    m = re.match(r"(\d\d:\d\d:\d\d) ", l)
    if m: hora_fin = m.group(1)
    m = re.search(r"(\S+ \[\d+\]:\d+) -> diskarbitrationd", l) or re.search(r"diskarbitrationd \[\d+\] -> (\S+ \[\d+\]:\d+)", l)
    if m:
        cur = m.group(1)
        if cur not in sesiones: sesiones.append(cur)
        continue
    m = re.search(r"created disk, id = (\S+)\.$", l)
    if m: disk(m.group(1))["creado"] = True; continue
    m = re.search(r"probed disk, id = (\S+), with (\S+), (\S+)\.$", l)
    if m: disk(m.group(1))["sondeo"].append(f"{m.group(2)}:{m.group(3)}"); continue
    m = re.search(r"registered callback, id = (\S+), kind = ([^.]+)\.", l)
    if m and "unregistered" not in l:
        cb[(cur, m.group(1))] = m.group(2)
        if m.group(2) == "idle": idle.setdefault(cur, {"pedidas": 0, "enviadas": 0})["pedidas"] += 1
        continue
    m = re.search(r"dispatched callback, id = (\S+), kind = ([^,.]+)(?:, disk = (\S+?))?\.$", l)
    if m:
        if m.group(2) == "disk appeared" and m.group(3): disk(m.group(3))["anunciado"].add(cur.split(" ")[0])
        if m.group(2) == "idle": idle.setdefault(cur, {"pedidas": 0, "enviadas": 0})["enviadas"] += 1

if not disks and not sesiones:
    sys.exit("registro vacío o sin el formato de diskarbitrationd -d")

print(f"== discos ({len(disks)}), registro hasta las {hora_fin}")
atascados = []
for d in sorted(disks):
    v = disks[d]
    an = ", ".join(sorted(v["anunciado"])) or "-"
    print(f"   {d:<14} creado={'sí' if v['creado'] else 'no'}  sondeo={' '.join(v['sondeo']) or '-':<26} anunciado a: {an}")
    if v["creado"] and not v["anunciado"] and any(k == "disk appeared" for k in cb.values()): atascados.append(d)
print("== avisos «idle» por sesión")
pend = []
for s in sesiones:
    if s in idle:
        i = idle[s]; ok = i["enviadas"] >= 1
        print(f"   {s:<32} pedidas={i['pedidas']} enviadas={i['enviadas']}{'' if ok else '   <- SIN RESPUESTA'}")
        if not ok: pend.append(s.split(" ")[0])
if not idle: print("   (ningún cliente pidió el aviso idle)")

if not any(k == "disk appeared" for k in cb.values()) and not idle:
    print("VEREDICTO: sin datos: en este arranque ningún cliente pidió discos ni el aviso idle (¿se lanzó diskutil?)")
elif atascados or pend:
    print("VEREDICTO: diskarbitrationd no llega a «idle»: " +
          (f"discos sin anunciar {', '.join(atascados)}" if atascados else "todos los discos anunciados") +
          (f"; esperan el aviso: {', '.join(sorted(set(pend)))}" if pend else ""))
else:
    print("VEREDICTO: correcto (discos anunciados y avisos idle enviados)")
