#!/usr/bin/env python3
# yaa.py: extrae los archivos Apple Archive (YAA/AA01) de un paquete de actualización de macOS (payloadv2 de un OTA o del
# instalador completo) en una carpeta de Linux, conservando lo que tahoe-run necesita:
#   - archivos, directorios, enlaces simbólicos y duros, con sus permisos y fechas;
#   - enlaces simbólicos absolutos guardados dentro de la raíz (el kernel de Linux los sigue; tahoe-run quita el prefijo en
#     readlink), como deja tools/fix_symlinks.sh;
#   - banderas BSD en el atributo user.tahoe.flags (sin UF_COMPRESSED: los datos se escriben sin comprimir);
#   - atributos extendidos de macOS como user.<nombre>;
#   - propietario distinto de root en user.tahoe.owner ("uid:gid"), porque sin ser root no se puede aplicar.
#   tools/yaa.py <paquete.zip> <raíz destino> [miembro...]     # por omisión, AssetData/payloadv2/payload.NNN en orden
#   tools/yaa.py --enlaces <paquete.zip> <raíz destino>        # solo los enlaces duros de links.txt
# Formato: cada miembro es un flujo pbzx (bloques xz) con entradas: "YAA1"/"AA01", u16 tamaño de cabecera y campos
# CLAVE(3)+subtipo(1): 1/2/4/8 enteros, P ruta (u16+bytes), T fecha (s+ns, 12 bytes), S fecha (8), A/B/C tamaño de un
# bloque de datos (u16/u32/u64) que va detrás de la cabecera, F/G/H/I sumas (20/32/48/64 bytes).
import lzma, os, struct, sys, zipfile

FIELD_LEN = {"1": 1, "2": 2, "4": 4, "8": 8, "T": 12, "S": 8, "F": 20, "G": 32, "H": 48, "I": 64}
BLOB_LEN = {"A": 2, "B": 4, "C": 8}
UF_COMPRESSED = 0x20

def pbzx_chunks(f):
    if f.read(4) != b"pbzx": raise SystemExit("no es pbzx")
    f.read(8)
    while True:
        h = f.read(16)
        if len(h) < 16: return
        usize, csize = struct.unpack(">QQ", h)
        d = f.read(csize)
        yield lzma.decompress(d) if (csize < usize or d[:6] == b"\xfd7zXZ\x00") else d

class Extractor:
    def __init__(self, root):
        self.root = os.path.abspath(root); self.dirs = []; self.n = 0; self.skipped = {}
    def host(self, p):
        p = p.decode("utf-8", "surrogateescape").lstrip("/")
        return os.path.join(self.root, p) if p else self.root
    def xattrs(self, path, blob):
        i = 0
        while i + 4 <= len(blob):
            total = struct.unpack_from("<I", blob, i)[0]
            if total < 5: break
            ent = blob[i + 4:i + total]; z = ent.index(b"\0")
            name, value = ent[:z].decode("utf-8", "surrogateescape"), ent[z + 1:]
            if name not in ("com.apple.decmpfs", "com.apple.ResourceFork"):
                try: os.setxattr(path, "user." + name, value, follow_symlinks=False)
                except OSError: pass
            i += total
    def entry(self, f, data, xat):
        t = chr(f.get("TYP", 0)); path = f.get("PAT", b""); hp = self.host(path)
        mode = f.get("MOD", 0o644); flags = f.get("FLG", 0) & ~UF_COMPRESSED
        if t == "D":
            os.makedirs(hp, exist_ok=True); self.dirs.append((hp, mode, f.get("MTM")))
        elif t == "F":
            os.makedirs(os.path.dirname(hp), exist_ok=True)
            if os.path.lexists(hp): os.chmod(hp, 0o644, follow_symlinks=False) if not os.path.islink(hp) else os.unlink(hp)
            with open(hp, "wb") as o: o.write(data)
            os.chmod(hp, mode & 0o7777)
        elif t == "L":
            os.makedirs(os.path.dirname(hp), exist_ok=True)
            tgt = f.get("LNK", b"").decode("utf-8", "surrogateescape")
            if tgt.startswith("/"): tgt = self.root + tgt
            if os.path.lexists(hp): os.unlink(hp)
            os.symlink(tgt, hp)
        elif t == "H":
            os.makedirs(os.path.dirname(hp), exist_ok=True)
            src = self.host(f.get("LNK", b""))
            if os.path.lexists(hp): os.unlink(hp)
            os.link(src, hp)
        else:
            self.skipped[t] = self.skipped.get(t, 0) + 1; return
        if t != "D" and t != "H" and "MTM" in f:
            s, ns = f["MTM"]; os.utime(hp, ns=(s * 10**9 + ns, s * 10**9 + ns), follow_symlinks=False)
        if flags and t in "DF":
            try: os.setxattr(hp, "user.tahoe.flags", struct.pack("<I", flags))
            except OSError: pass
        uid, gid = f.get("UID", 0), f.get("GID", 0)
        if (uid or gid) and t in "DF":
            try: os.setxattr(hp, "user.tahoe.owner", f"{uid}:{gid}".encode())
            except OSError: pass
        if xat and t in "DF": self.xattrs(hp, xat)
        self.n += 1
    def finish(self):
        for hp, mode, mtm in reversed(self.dirs):          # de dentro afuera: los modos de solo lectura, al final
            try:
                os.chmod(hp, mode & 0o7777)
                if mtm: os.utime(hp, ns=(mtm[0] * 10**9 + mtm[1],) * 2)
            except OSError: pass

def parse(buf, pos, ex):
    """Procesa las entradas completas de buf desde pos; devuelve la posición de la primera incompleta."""
    while pos + 6 <= len(buf):
        if buf[pos:pos + 4] not in (b"YAA1", b"AA01"): raise SystemExit(f"cabecera desconocida {buf[pos:pos+4]!r}")
        hs = struct.unpack_from("<H", buf, pos + 4)[0]
        if pos + hs > len(buf): return pos
        q = pos + 6; f = {}; blobs = []
        while q < pos + hs:
            k = buf[q:q + 3].decode("latin1"); st = chr(buf[q + 3]); q += 4
            if st == "P":
                l = struct.unpack_from("<H", buf, q)[0]; f[k] = bytes(buf[q + 2:q + 2 + l]); q += 2 + l
            elif st in BLOB_LEN:
                n = BLOB_LEN[st]; blobs.append((k, int.from_bytes(buf[q:q + n], "little"))); q += n
            elif st == "T":
                s, ns = struct.unpack_from("<qI", buf, q); f[k] = (s, ns); q += 12
            elif st == "S":
                f[k] = (struct.unpack_from("<q", buf, q)[0], 0); q += 8
            elif st in FIELD_LEN:
                n = FIELD_LEN[st]
                if st in "1248": f[k] = int.from_bytes(buf[q:q + n], "little")
                q += n
            elif st == "*":
                pass
            else:
                raise SystemExit(f"subtipo desconocido {k}{st}")
        total = hs + sum(n for _, n in blobs)
        if pos + total > len(buf): return pos
        q = pos + hs; data = b""; xat = b""
        for k, n in blobs:
            if k == "DAT": data = bytes(buf[q:q + n])
            elif k == "XAT": xat = bytes(buf[q:q + n])
            q += n
        ex.entry(f, data, xat)
        pos += total
    return pos

def hardlinks(z, root):
    """links.txt: el paquete guarda cada archivo una sola vez; «=ruta» es el original y cada «+ruta» siguiente, un enlace
    duro a él."""
    try: txt = z.read("AssetData/payloadv2/links.txt").decode("utf-8", "surrogateescape")
    except KeyError: return
    src = None; n = falta = 0
    for line in txt.splitlines():
        if not line: continue
        op, rel = line[0], line[1:]
        hp = os.path.join(root, rel)
        if op == "=": src = hp
        elif op == "+" and src:
            if not os.path.lexists(src): falta += 1; continue
            os.makedirs(os.path.dirname(hp), exist_ok=True)
            if os.path.lexists(hp): os.unlink(hp)
            os.link(src, hp, follow_symlinks=False); n += 1
    print(f"enlaces duros: {n} (sin original: {falta})")

def main():
    if sys.argv[1] == "--enlaces":                     # solo links.txt, sobre una extracción ya hecha
        hardlinks(zipfile.ZipFile(sys.argv[2]), os.path.abspath(sys.argv[3])); return
    zpath, root = sys.argv[1], sys.argv[2]
    z = zipfile.ZipFile(zpath)
    members = sys.argv[3:] or sorted(n for n in z.namelist() if n.startswith("AssetData/payloadv2/payload.") and n.rsplit(".", 1)[1].isdigit())
    ex = Extractor(root)
    for m in members:
        buf = bytearray(); pos = 0
        for chunk in pbzx_chunks(z.open(m)):
            buf += chunk
            pos = parse(buf, pos, ex)
            if pos > (1 << 24): del buf[:pos]; pos = 0
        if pos != len(buf): print(f"{m}: {len(buf) - pos} bytes sin consumir al final", flush=True)
        print(f"{m}: hecho ({ex.n} entradas)", flush=True)
    ex.finish()
    print(f"total {ex.n} entradas; sin crear (tipo: cuántas): {ex.skipped}")
    if not sys.argv[3:]: hardlinks(z, ex.root)

if __name__ == "__main__":
    main()
