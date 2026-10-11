// Binarios protegidos de macOS ("Apple Protect", Dont Steal Mac OS X.kext). Algunos programas del sistema (loginwindow,
// Finder, Dock...) llevan el segmento __TEXT con SG_PROTECTED_VERSION_1: salvo las 3 primeras páginas del archivo, su
// código está cifrado. En un Mac lo descifra el núcleo al cargar cada página (unprotect_dsmos_segment ->
// dsmos_page_transform) con la clave que da el SMC (claves OSK0 y OSK1: 64 bytes); un Hackintosh la obtiene de
// VirtualSMC/FakeSMC. Aquí se descifra al cargar el ejecutable (load_macho), con el mismo esquema que el núcleo para la
// marca 0x2e69cf40: Blowfish en modo CBC, cada página de 4 KiB por separado con vector inicial cero y la clave de 64 bytes.
//
// La clave no va en el repositorio: se lee del archivo que indique TAHOE_OSK_FILE (tools/macos-shell.sh usa
// ~/.config/tahoe-run/osk si existe). Sin ella el programa se carga tal cual y caerá al ejecutar código cifrado.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "tahoe.h"
#include "bf_pi.h"

struct bf { uint32_t p[18], s[4][256]; };
static uint32_t bf_f(const struct bf* k, uint32_t x) {
	return ((k->s[0][x >> 24] + k->s[1][(x >> 16) & 0xff]) ^ k->s[2][(x >> 8) & 0xff]) + k->s[3][x & 0xff];
}
static void bf_enc(const struct bf* k, uint32_t* l, uint32_t* r) {
	uint32_t a = *l, b = *r;
	for (int i = 0; i < 16; i++) { a ^= k->p[i]; b ^= bf_f(k, a); uint32_t t = a; a = b; b = t; }
	{ uint32_t t = a; a = b; b = t; }
	b ^= k->p[16]; a ^= k->p[17];
	*l = a; *r = b;
}
static void bf_dec(const struct bf* k, uint32_t* l, uint32_t* r) {
	uint32_t a = *l, b = *r;
	for (int i = 17; i > 1; i--) { a ^= k->p[i]; b ^= bf_f(k, a); uint32_t t = a; a = b; b = t; }
	{ uint32_t t = a; a = b; b = t; }
	b ^= k->p[1]; a ^= k->p[0];
	*l = a; *r = b;
}
// Preparación de la clave de Blowfish: la clave (hasta 72 bytes, aquí 64) se repite sobre el P-array y se cifra en cadena.
static void bf_key(struct bf* k, const uint8_t* key, size_t n) {
	memcpy(k->p, bf_pi, sizeof k->p);
	memcpy(k->s, bf_pi + 18, sizeof k->s);
	for (int i = 0, j = 0; i < 18; i++) {
		uint32_t d = 0;
		for (int b = 0; b < 4; b++) { d = (d << 8) | key[j]; j = (j + 1) % (int)n; }
		k->p[i] ^= d;
	}
	uint32_t l = 0, r = 0;
	for (int i = 0; i < 18; i += 2) { bf_enc(k, &l, &r); k->p[i] = l; k->p[i + 1] = r; }
	for (int s = 0; s < 4; s++)
		for (int i = 0; i < 256; i += 2) { bf_enc(k, &l, &r); k->s[s][i] = l; k->s[s][i + 1] = r; }
}
static uint32_t be32(const uint8_t* p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }
static void put_be32(uint8_t* p, uint32_t v) { p[0] = v >> 24; p[1] = v >> 16; p[2] = v >> 8; p[3] = v; }
static void bf_cbc_dec_page(const struct bf* k, uint8_t* pg, size_t len) {
	uint8_t iv[8] = { 0 }, c[8];
	for (size_t o = 0; o + 8 <= len; o += 8) {
		memcpy(c, pg + o, 8);
		uint32_t l = be32(pg + o), r = be32(pg + o + 4);
		bf_dec(k, &l, &r);
		put_be32(pg + o, l); put_be32(pg + o + 4, r);
		for (int i = 0; i < 8; i++) pg[o + i] ^= iv[i];
		memcpy(iv, c, 8);
	}
}

#define DSMOS_UNPROTECTED_HEADER 0x3000u   // APPLE_UNPROTECTED_HEADER_SIZE de XNU: 3 páginas sin cifrar
#define DSMOS_MAGIC_BLOWFISH     0x2e69cf40u
// seg: el segmento ya proyectado (escribible); fileoff/filesize: los del segmento dentro del Mach-O.
void dsmos_unprotect(uint8_t* seg, uint64_t fileoff, uint64_t filesize, const char* path) {
	uint64_t start = fileoff >= DSMOS_UNPROTECTED_HEADER ? 0 : DSMOS_UNPROTECTED_HEADER - fileoff;
	if (start >= filesize) return;
	uint32_t magic; memcpy(&magic, seg + start, 4);
	if (magic != DSMOS_MAGIC_BLOWFISH) { logf_("    %s: segmento protegido con marca 0x%08x desconocida: sin descifrar\n", path, magic); return; }
	const char* kf = getenv("TAHOE_OSK_FILE");
	uint8_t key[64];
	int fd = kf ? open(kf, O_RDONLY | O_CLOEXEC) : -1;
	ssize_t n = fd >= 0 ? read(fd, key, sizeof key) : -1;
	if (fd >= 0) close(fd);
	if (n != (ssize_t)sizeof key) { logf_("    %s: código protegido y sin clave OSK (TAHOE_OSK_FILE): no se puede descifrar\n", path); return; }
	static struct bf k;                                       // 4 KiB: fuera de la pila
	bf_key(&k, key, sizeof key);
	memset(key, 0, sizeof key);
	for (uint64_t o = start; o < filesize; o += 4096) bf_cbc_dec_page(&k, seg + o, filesize - o < 4096 ? filesize - o : 4096);
	memset(&k, 0, sizeof k);
}
