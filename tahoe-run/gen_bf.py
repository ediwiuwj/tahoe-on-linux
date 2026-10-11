#!/usr/bin/env python3
# gen_bf.py: constantes iniciales de Blowfish (P-array y cajas S): son los dígitos hexadecimales de la parte fraccionaria de
# pi, en orden (18 + 4*256 palabras de 32 bits). Se calculan con la fórmula de Machin en aritmética entera, sin tablas.
N = 18 + 4 * 256
bits = N * 32 + 64
def arctan_inv(x, one):
    # arctan(1/x) * one por su serie
    s = t = one // x; x2 = x * x; k = 1; sign = -1
    while t:
        t //= x2; k += 2
        s += sign * (t // k); sign = -sign
    return s
one = 1 << (bits + 32)
pi = 16 * arctan_inv(5, one) - 4 * arctan_inv(239, one)
frac = pi - (3 * one)
words = []
for i in range(N):
    frac *= 1 << 32
    w = frac // one; frac -= w * one
    words.append(w)
assert words[0] == 0x243F6A88 and words[17] == 0x8979FB1B and words[18] == 0xD1310BA6
print("// Generado por gen_bf.py: dígitos hexadecimales de pi (constantes iniciales de Blowfish)")
print("static const uint32_t bf_pi[%d] = {" % N)
for i in range(0, N, 8):
    print("\t" + ", ".join("0x%08x" % w for w in words[i:i + 8]) + ",")
print("};")
