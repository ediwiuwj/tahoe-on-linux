#!/usr/bin/env python3
"""dsmos.py <macho> <salida>: copia de un ejecutable de macOS con el __TEXT protegido («Apple Protect»: loginwindow,
Finder, Dock...) ya descifrado, para poder desensamblarlo (llvm-objdump). El mismo esquema que emu_dsmos.c: Blowfish en
modo CBC por páginas de 4 KiB, vector inicial cero, salvo las 3 primeras páginas del archivo, con la clave OSK de 64 bytes
de TAHOE_OSK_FILE o ~/.config/tahoe-run/osk. Si el archivo es universal, se queda con la parte x86_64.
La copia descifrada es un binario de Apple: no se sube al repositorio (déjala en /tmp).
"""
import os
import re
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
UNPROTECTED = 0x3000
MAGIC = 0x2e69cf40
SG_PROTECTED_VERSION_1 = 0x8


def bf_pi():
    src = open(os.path.join(HERE, "..", "..", "tahoe-run", "bf_pi.h")).read()
    return [int(x, 16) for x in re.findall(r"0x([0-9a-f]{8})", src)]


class Blowfish:
    def __init__(self, key):
        pi = bf_pi()
        self.p = pi[:18]
        self.s = [pi[18 + 256 * i: 18 + 256 * (i + 1)] for i in range(4)]
        j = 0
        for i in range(18):
            d = 0
            for _ in range(4):
                d = (d << 8) | key[j]
                j = (j + 1) % len(key)
            self.p[i] ^= d
        l = r = 0
        for i in range(0, 18, 2):
            l, r = self.enc(l, r)
            self.p[i], self.p[i + 1] = l, r
        for s in range(4):
            for i in range(0, 256, 2):
                l, r = self.enc(l, r)
                self.s[s][i], self.s[s][i + 1] = l, r

    def f(self, x):
        s = self.s
        return (((s[0][x >> 24] + s[1][(x >> 16) & 0xff]) & 0xffffffff) ^ s[2][(x >> 8) & 0xff]) + s[3][x & 0xff] & 0xffffffff

    def enc(self, a, b):
        for i in range(16):
            a ^= self.p[i]
            b ^= self.f(a)
            a, b = b, a
        a, b = b, a
        return a ^ self.p[17], b ^ self.p[16]

    def dec(self, a, b):
        for i in range(17, 1, -1):
            a ^= self.p[i]
            b ^= self.f(a)
            a, b = b, a
        a, b = b, a
        return a ^ self.p[0], b ^ self.p[1]


def dec_page(bf, pg):
    out = bytearray(len(pg))
    iv = b"\0" * 8
    for o in range(0, len(pg) - 7, 8):
        c = pg[o:o + 8]
        l, r = struct.unpack(">II", c)
        l, r = bf.dec(l, r)
        p = struct.pack(">II", l, r)
        out[o:o + 8] = bytes(x ^ y for x, y in zip(p, iv))
        iv = c
    out[len(pg) - len(pg) % 8:] = pg[len(pg) - len(pg) % 8:]
    return bytes(out)


def thin_x86(d):
    if struct.unpack(">I", d[:4])[0] in (0xcafebabe,):
        n = struct.unpack(">I", d[4:8])[0]
        for i in range(n):
            ct, _, off, sz, _ = struct.unpack(">IIIII", d[8 + i * 20:28 + i * 20])
            if ct == 0x01000007:
                return d[off:off + sz]
        sys.exit("no hay parte x86_64")
    return d


def main():
    src, dst = sys.argv[1], sys.argv[2]
    kf = os.environ.get("TAHOE_OSK_FILE", os.path.expanduser("~/.config/tahoe-run/osk"))
    key = open(kf, "rb").read(64)
    if len(key) != 64:
        sys.exit("clave OSK incompleta")
    d = bytearray(thin_x86(open(src, "rb").read()))
    ncmds = struct.unpack("<I", d[16:20])[0]
    off = 32
    bf = Blowfish(key)
    for _ in range(ncmds):
        cmd, sz = struct.unpack("<II", d[off:off + 8])
        if cmd == 0x19:                                           # LC_SEGMENT_64
            name = d[off + 8:off + 24].rstrip(b"\0").decode()
            fileoff, filesize = struct.unpack("<QQ", d[off + 40:off + 56])
            flags = struct.unpack("<I", d[off + 68:off + 72])[0]
            if flags & SG_PROTECTED_VERSION_1:
                start = 0 if fileoff >= UNPROTECTED else UNPROTECTED - fileoff
                magic = struct.unpack("<I", d[fileoff + start:fileoff + start + 4])[0]
                if magic != MAGIC:
                    sys.exit(f"{name}: marca 0x{magic:08x} desconocida")
                for o in range(start, filesize, 4096):
                    a = fileoff + o
                    b = fileoff + min(o + 4096, filesize)
                    d[a:b] = dec_page(bf, bytes(d[a:b]))
                flags &= ~SG_PROTECTED_VERSION_1
                d[off + 68:off + 72] = struct.pack("<I", flags)
                print(f"{name}: descifradas {(filesize - start + 4095) // 4096} páginas")
        off += sz
    open(dst, "wb").write(d)


if __name__ == "__main__":
    main()
