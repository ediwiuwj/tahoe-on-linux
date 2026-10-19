#!/usr/bin/env python3
# xar.py: lista o extrae archivos de un paquete xar de Apple (InstallAssistant.pkg, .pkg de instalación).
#   tools/xar.py lista  <paquete.pkg>
#   tools/xar.py saca   <paquete.pkg> <nombre> <destino>     # p. ej. SharedSupport.dmg
# Formato: cabecera (magic "xar!", tamaño de cabecera, versión, índice comprimido y sin comprimir, algoritmo de suma),
# índice XML comprimido con zlib y, a continuación, el montón con los datos de cada archivo (desplazamiento relativo).
# Los datos pueden ir sin comprimir (application/octet-stream) o con zlib/bzip2; se copian por bloques, sin cargarlos.
import bz2, hashlib, struct, sys, zlib
import xml.etree.ElementTree as ET

def toc(f):
    h = f.read(28)
    magic, hsize, ver, clen, ulen, cksum = struct.unpack(">4sHHQQI", h)
    if magic != b"xar!": sys.exit("no es un archivo xar")
    f.seek(hsize)
    x = zlib.decompress(f.read(clen))
    return ET.fromstring(x), hsize + clen

def files(root, base=""):
    for e in root.findall("file"):
        name = base + e.findtext("name", "")
        yield name, e
        yield from files(e, name + "/")

def main():
    op, pkg = sys.argv[1], sys.argv[2]
    with open(pkg, "rb") as f:
        t, heap = toc(f)
        tocnode = t.find("toc")
        for name, e in files(tocnode):
            d = e.find("data")
            if op == "lista":
                if d is None: print(f"{'':>14}  {name}/"); continue
                print(f"{int(d.findtext('size')):>14}  {name}  ({d.find('encoding').get('style')})")
            elif op == "saca" and name == sys.argv[3] and d is not None:
                off, length = int(d.findtext("offset")), int(d.findtext("length"))
                enc = d.find("encoding").get("style")
                sumtag = d.find("extracted-checksum")
                h = hashlib.new(sumtag.get("style")) if sumtag is not None else None
                dec = zlib.decompressobj() if "gzip" in enc or "zlib" in enc else bz2.BZ2Decompressor() if "bzip2" in enc else None
                f.seek(heap + off)
                left = length
                with open(sys.argv[4], "wb") as o:
                    while left:
                        b = f.read(min(left, 1 << 24)); left -= len(b)
                        if not b: sys.exit("archivo truncado")
                        if dec: b = dec.decompress(b)
                        if h: h.update(b)
                        o.write(b)
                if h:
                    ok = h.hexdigest() == sumtag.text.strip()
                    print(f"suma {sumtag.get('style')}: {'correcta' if ok else 'NO COINCIDE'}")
                    if not ok: sys.exit(1)
                return
    if op == "saca": sys.exit("no está: " + sys.argv[3])

if __name__ == "__main__":
    main()
