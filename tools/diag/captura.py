#!/usr/bin/env python3
# captura.py [salida.png] [archivo]: guarda en PNG la pantalla emulada de macOS (la memoria de vídeo en la que dibuja
# WindowServer: /dev/shm/tahoe-fb-<id>, por omisión la más reciente). Lee la cabecera "TFB1" de la última página del
# archivo (ancho, alto, bytes por fila, formato 0 = B, G, R, X) como tools/tahoe-view. Sin dependencias (zlib).
import glob, os, struct, sys, zlib

out = sys.argv[1] if len(sys.argv) > 1 else "pantalla.png"
files = [sys.argv[2]] if len(sys.argv) > 2 else sorted(glob.glob("/dev/shm/tahoe-fb-*"), key=os.path.getmtime)
if not files:
    sys.exit("no hay memoria de vídeo (/dev/shm/tahoe-fb-*): ¿está WindowServer en marcha?")
with open(files[-1], "rb") as f:
    data = f.read()
magic, w, h, bpr, fmt = struct.unpack_from("<4sIIII", data, len(data) - 4096)
if magic != b"TFB1" or fmt != 0:
    sys.exit("cabecera desconocida en " + files[-1])
rows = bytearray()
used = 0
for y in range(h):
    line = data[y * bpr: y * bpr + w * 4]
    rows.append(0)                                            # filtro PNG: ninguno
    px = bytearray(w * 3)
    px[0::3], px[1::3], px[2::3] = line[2::4], line[1::4], line[0::4]   # BGRX -> RGB
    used += any(line)
    rows += px


def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)


png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + \
      chunk(b"IDAT", zlib.compress(bytes(rows), 6)) + chunk(b"IEND", b"")
open(out, "wb").write(png)
print(f"{out}: {w}x{h}, {used} de {h} filas con algo dibujado ({files[-1]})")
