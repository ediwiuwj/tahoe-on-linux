#!/usr/bin/env python3
# entrada.py <orden>...: manda teclado y ratón al macOS que corre (el mismo socket que usa tahoe-view, ver
# tools/tahoe-view/src/entrada.rs). Para probar sin la ventana. Órdenes:
#   tecla <uso hex>      pulsa y suelta esa tecla (uso HID de la página 7: 0x28 Intro, 0x2c espacio, 0x04 «a»...)
#   texto <letras>       letras a-z, cifras y espacios (distribución estadounidense, sin mayúsculas)
#   mueve <x> <y>        el puntero a ese píxel de la pantalla de macOS
#   clic <x> <y>         mueve y hace clic con el botón izquierdo
#   rueda <dy>           líneas de rueda (positivo hacia arriba)
#   espera <s>           pausa
# El socket es /dev/shm/tahoe-input-<id> del arranque más reciente (o TAHOE_INPUT).
import glob, os, socket, struct, sys, time

def ruta():
    if os.environ.get("TAHOE_INPUT"): return os.environ["TAHOE_INPUT"]
    s = sorted(glob.glob("/dev/shm/tahoe-input-*"), key=os.path.getmtime)
    if not s: sys.exit("entrada.py: no hay /dev/shm/tahoe-input-* (¿WindowServer arrancado?)")
    return s[-1]

S = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM); R = ruta()
def manda(t, a, b, c): S.sendto(struct.pack("<Iiii", t, a, b, c), R)
def tecla(u): manda(1, 7, u, 1); time.sleep(0.03); manda(1, 7, u, 0); time.sleep(0.03)
def uso(ch):
    if "a" <= ch <= "z": return 4 + ord(ch) - ord("a")
    if "1" <= ch <= "9": return 0x1e + ord(ch) - ord("1")
    return {"0": 0x27, " ": 0x2c, "\n": 0x28}.get(ch)

a = sys.argv[1:]
while a:
    o = a.pop(0)
    if o == "tecla": tecla(int(a.pop(0), 16))
    elif o == "texto":
        for ch in a.pop(0):
            u = uso(ch)
            if u is None: sys.exit(f"entrada.py: «{ch}» no está en la tabla")
            tecla(u)
    elif o == "mueve": x, y = int(a.pop(0)), int(a.pop(0)); manda(2, x, y, 0)
    elif o == "clic": x, y = int(a.pop(0)), int(a.pop(0)); manda(2, x, y, 0); time.sleep(0.05); manda(2, x, y, 1); time.sleep(0.08); manda(2, x, y, 0)
    elif o == "rueda": manda(3, 0, int(a.pop(0)), 0)
    elif o == "espera": time.sleep(float(a.pop(0)))
    else: sys.exit(f"entrada.py: orden desconocida «{o}»")
