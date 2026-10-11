// Firma de código: los derechos (entitlements) de un proceso salen de la firma real de su ejecutable.
//
// En XNU, csops(CS_OPS_ENTITLEMENTS_BLOB / CS_OPS_DER_ENTITLEMENTS_BLOB) copia el blob de derechos de la firma que el
// núcleo validó al cargar el binario. Aquí se lee lo mismo del archivo: el comando LC_CODE_SIGNATURE apunta a un
// "superblob" (big endian) con un índice de ranuras; la 5 son los derechos en XML (0xfade7171) y la 7 en DER
// (0xfade7172). Muchos servicios consultan los derechos de quien les habla (csops_audittoken sobre el pid del cliente).
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "tahoe.h"

// Directorio de /proc con los datos de un proceso. Si su hilo principal ya terminó (dispatch_main, pthread_exit) y quedan
// otros, el líder es un zombi con exe, cmdline y environ vacíos: se usa el de un hilo vivo (/proc/<pid>/task/<tid>), que
// comparte memoria y ejecutable con el proceso. Devuelve 0, o -1 si el proceso no existe o ya no le queda ningún hilo.
static char proc_state(const char* dir) {
	char p[128], b[512]; snprintf(p, sizeof p, "%s/stat", dir);
	int fd = open(p, O_RDONLY | O_CLOEXEC); if (fd < 0) return 0;
	ssize_t n = read(fd, b, sizeof b - 1); close(fd);
	if (n <= 0) return 0;
	b[n] = 0;
	char* e = strrchr(b, ')');
	return e && e[1] == ' ' ? e[2] : 0;
}
int proc_dir(int pid, char* out, size_t cap) {
	if (pid <= 0) pid = (int)getpid();
	snprintf(out, cap, "/proc/%d", pid);
	char st = proc_state(out);
	if (!st) return -1;
	if (st != 'Z' && st != 'X') return 0;
	char tdir[64]; snprintf(tdir, sizeof tdir, "/proc/%d/task", pid);
	DIR* d = opendir(tdir); struct dirent* de; int found = -1;
	while (d && found < 0 && (de = readdir(d))) {
		if (de->d_name[0] == '.' || atoi(de->d_name) == pid) continue;
		char td[128]; snprintf(td, sizeof td, "%s/%s", tdir, de->d_name);
		char ts = proc_state(td);
		if (ts && ts != 'Z' && ts != 'X') { snprintf(out, cap, "%s", td); found = 0; }
	}
	if (d) closedir(d);
	return found;
}
// Ruta (en el anfitrión) del ejecutable de macOS de un proceso: tahoe-run <caché> <dyld> <ejecutable> [args...].
int exe_host_path(int pid, char* out, size_t cap) {
	char dir[128], p[160], buf[8192];
	if (proc_dir(pid, dir, sizeof dir) != 0) return -1;
	snprintf(p, sizeof p, "%s/cmdline", dir);
	ssize_t n = -1;
	int fd = open(p, O_RDONLY | O_CLOEXEC);
	if (fd >= 0) { n = read(fd, buf, sizeof buf - 1); close(fd); }
	if (n <= 0) return -1;
	buf[n] = 0;
	const char* a = buf; int i = 0;
	while (a < buf + n && i < 3) { a += strlen(a) + 1; i++; }
	if (a >= buf + n || !*a) return -1;
	snprintf(out, cap, "%s", a);
	return 0;
}

static uint32_t rd32(const uint8_t* p) { uint32_t v; memcpy(&v, p, 4); return v; }
static uint32_t be32(const uint8_t* p) { return ntohl(rd32(p)); }

// Desplazamiento de la rebanada x86_64 dentro del archivo (0 si no es universal). -1 si no se puede leer.
static int64_t slice_offset(int fd) {
	uint8_t h[4096];
	if (pread(fd, h, sizeof h, 0) < 32) return -1;
	if (be32(h) != 0xcafebabe) return 0;
	uint32_t nf = be32(h + 4);
	for (uint32_t i = 0; i < nf && 8 + (i + 1) * 20 <= sizeof h; i++)
		if (be32(h + 8 + i * 20) == 0x01000007) return be32(h + 8 + i * 20 + 8);
	return -1;
}
// Busca en la firma del ejecutable el blob de la ranura indicada. Devuelve malloc() con el blob (cabecera incluida).
static uint8_t* cs_blob(int pid, uint32_t slot, uint32_t* len) {
	char path[4096];
	if (exe_host_path(pid, path, sizeof path) != 0) return NULL;
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0) return NULL;
	uint8_t h[4096];
	uint64_t base = 0;
	if (pread(fd, h, sizeof h, 0) < 32) { close(fd); return NULL; }
	if (be32(h) == 0xcafebabe) {                              // binario universal: rebanada x86_64
		uint32_t nf = be32(h + 4);
		for (uint32_t i = 0; i < nf && 8 + (i + 1) * 20 <= sizeof h; i++)
			if (be32(h + 8 + i * 20) == 0x01000007) { base = be32(h + 8 + i * 20 + 8); break; }
		if (!base || pread(fd, h, sizeof h, (off_t)base) < 32) { close(fd); return NULL; }
	}
	uint8_t* out = NULL;
	if (rd32(h) == 0xfeedfacf) {
		uint32_t ncmds = rd32(h + 16), sizeofcmds = rd32(h + 20);
		uint8_t* lc = malloc(sizeofcmds);
		if (lc && pread(fd, lc, sizeofcmds, (off_t)base + 32) == (ssize_t)sizeofcmds) {
			uint32_t off = 0;
			for (uint32_t i = 0; i < ncmds && off + 8 <= sizeofcmds; i++) {
				uint32_t cmd = rd32(lc + off), cs = rd32(lc + off + 4);
				if (cs < 8) break;
				if (cmd == 0x1d && off + 16 <= sizeofcmds) {          // LC_CODE_SIGNATURE: dataoff, datasize
					uint32_t doff = rd32(lc + off + 8), dsz = rd32(lc + off + 12);
					uint8_t* sb = malloc(dsz);
					if (sb && dsz >= 12 && pread(fd, sb, dsz, (off_t)(base + doff)) == (ssize_t)dsz && be32(sb) == 0xfade0cc0) {
						uint32_t count = be32(sb + 8);
						for (uint32_t k = 0; k < count && 12 + (k + 1) * 8 <= dsz; k++) {
							uint32_t type = be32(sb + 12 + k * 8), bo = be32(sb + 12 + k * 8 + 4);
							if (type != slot || bo + 8 > dsz) continue;
							uint32_t bl = be32(sb + bo + 4);
							if (bl < 8 || bo + bl > dsz) break;
							out = malloc(bl);
							if (out) { memcpy(out, sb + bo, bl); *len = bl; }
							break;
						}
					}
					free(sb);
					break;
				}
				off += cs;
			}
		}
		free(lc);
	}
	close(fd);
	return out;
}

// LC_UUID del ejecutable de macOS de un proceso (p_uuid de proc_pidinfo): logd lo anota en cada mensaje para encontrar después
// el texto de formato del programa. 0 si se encontró.
int exe_uuid(int pid, uint8_t out[16]) {
	char path[4096];
	if (exe_host_path(pid, path, sizeof path) != 0) return -1;
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0) return -1;
	int64_t base = slice_offset(fd);
	uint8_t h[32]; int r = -1;
	if (base >= 0 && pread(fd, h, 32, (off_t)base) == 32 && rd32(h) == 0xfeedfacf) {
		uint32_t ncmds = rd32(h + 16), sizeofcmds = rd32(h + 20);
		uint8_t* lc = sizeofcmds < (1u << 20) ? malloc(sizeofcmds) : NULL;
		if (lc && pread(fd, lc, sizeofcmds, (off_t)base + 32) == (ssize_t)sizeofcmds) {
			for (uint32_t i = 0, off = 0; i < ncmds && off + 8 <= sizeofcmds; i++) {
				uint32_t cmd = rd32(lc + off), cs = rd32(lc + off + 4);
				if (cs < 8) break;
				if (cmd == 0x1b && cs >= 24 && off + 24 <= sizeofcmds) { memcpy(out, lc + off + 8, 16); r = 0; break; }   // LC_UUID
				off += cs;
			}
		}
		free(lc);
	}
	close(fd);
	return r;
}

// SHA-1 y SHA-256 (FIPS 180-4), para el cdhash: el hash del CodeDirectory de la firma, truncado a 20 bytes.
#define ROTL(x, n) (((x) << (n)) | ((x) >> (32 - (n))))
#define ROTR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))
static void hash_pad(const uint8_t* m, size_t len, void (*blk)(uint32_t*, const uint8_t*), uint32_t* st) {
	size_t i = 0;
	for (; i + 64 <= len; i += 64) blk(st, m + i);
	uint8_t t[128] = { 0 };
	size_t r = len - i;
	memcpy(t, m + i, r); t[r] = 0x80;
	size_t tl = r + 9 <= 64 ? 64 : 128;
	uint64_t bits = (uint64_t)len * 8;
	for (int k = 0; k < 8; k++) t[tl - 1 - k] = (uint8_t)(bits >> (8 * k));
	blk(st, t); if (tl == 128) blk(st, t + 64);
}
static void sha1_blk(uint32_t* h, const uint8_t* p) {
	uint32_t w[80], a = h[0], b = h[1], c = h[2], d = h[3], e = h[4];
	for (int i = 0; i < 16; i++) w[i] = be32(p + 4 * i);
	for (int i = 16; i < 80; i++) w[i] = ROTL(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
	for (int i = 0; i < 80; i++) {
		uint32_t f, k;
		if (i < 20) { f = (b & c) | (~b & d); k = 0x5a827999; }
		else if (i < 40) { f = b ^ c ^ d; k = 0x6ed9eba1; }
		else if (i < 60) { f = (b & c) | (b & d) | (c & d); k = 0x8f1bbcdc; }
		else { f = b ^ c ^ d; k = 0xca62c1d6; }
		uint32_t t = ROTL(a, 5) + f + e + k + w[i];
		e = d; d = c; c = ROTL(b, 30); b = a; a = t;
	}
	h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e;
}
static const uint32_t K256[64] = {
	0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
	0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
	0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
	0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
	0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
	0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
	0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2 };
static void sha256_blk(uint32_t* h, const uint8_t* p) {
	uint32_t w[64], v[8];
	for (int i = 0; i < 16; i++) w[i] = be32(p + 4 * i);
	for (int i = 16; i < 64; i++) {
		uint32_t s0 = ROTR(w[i - 15], 7) ^ ROTR(w[i - 15], 18) ^ (w[i - 15] >> 3);
		uint32_t s1 = ROTR(w[i - 2], 17) ^ ROTR(w[i - 2], 19) ^ (w[i - 2] >> 10);
		w[i] = w[i - 16] + s0 + w[i - 7] + s1;
	}
	memcpy(v, h, 32);
	for (int i = 0; i < 64; i++) {
		uint32_t S1 = ROTR(v[4], 6) ^ ROTR(v[4], 11) ^ ROTR(v[4], 25), ch = (v[4] & v[5]) ^ (~v[4] & v[6]);
		uint32_t t1 = v[7] + S1 + ch + K256[i] + w[i];
		uint32_t S0 = ROTR(v[0], 2) ^ ROTR(v[0], 13) ^ ROTR(v[0], 22), mj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
		uint32_t t2 = S0 + mj;
		v[7] = v[6]; v[6] = v[5]; v[5] = v[4]; v[4] = v[3] + t1; v[3] = v[2]; v[2] = v[1]; v[1] = v[0]; v[0] = t1 + t2;
	}
	for (int i = 0; i < 8; i++) h[i] += v[i];
}
// cdhash: 20 primeros bytes del hash (del tipo que dice el propio CodeDirectory) del blob del CodeDirectory
static void cd_hash20(const uint8_t* cd, uint32_t len, uint8_t out[20]) {
	uint32_t st[8];
	uint8_t full[32];
	if (cd[37] == 1) {                                           // CS_HASHTYPE_SHA1
		uint32_t i1[5] = { 0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0 };
		memcpy(st, i1, sizeof i1);
		hash_pad(cd, len, sha1_blk, st);
		for (int i = 0; i < 5; i++) { uint32_t b = htonl(st[i]); memcpy(full + 4 * i, &b, 4); }
	} else {                                                     // SHA256 (y SHA256 truncado): lo habitual
		uint32_t i2[8] = { 0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19 };
		memcpy(st, i2, sizeof i2);
		hash_pad(cd, len, sha256_blk, st);
		for (int i = 0; i < 8; i++) { uint32_t b = htonl(st[i]); memcpy(full + 4 * i, &b, 4); }
	}
	memcpy(out, full, 20);
}

// Como csops_copy_token de XNU: si no cabe, solo la cabecera con la longitud necesaria y ERANGE.
static long copy_token(const uint8_t* blob, uint32_t len, uint64_t uaddr, uint64_t usize) {
	if (usize < 8) return -34;                                   // ERANGE
	if (!blob) { uint8_t z[8] = { 0 }; return safe_write(uaddr, z, 8) == 8 ? 0 : -D_EFAULT; }   // sin derechos
	if (usize < len) {
		uint8_t hdr[8]; memcpy(hdr, blob, 4); uint32_t bl = htonl(len); memcpy(hdr + 4, &bl, 4);
		return safe_write(uaddr, hdr, 8) == 8 ? -34 : -D_EFAULT;
	}
	return safe_write(uaddr, blob, len) == (ssize_t)len ? 0 : -D_EFAULT;
}

// Núcleo común de csops y csops_audittoken. pid 0 = el propio proceso.
static long cs_ops(int pid, int op, uint64_t uaddr, uint64_t usize) {
	switch (op) {
	case 0: {                                                    // CS_OPS_STATUS: sin banderas (como un binario sin restricciones)
		uint32_t zero = 0;
		if (usize < 4) return -D_EINVAL;
		return safe_write(uaddr, &zero, 4) == 4 ? 0 : -D_EFAULT;
	}
	case 7: case 16: {                                           // ENTITLEMENTS_BLOB (XML, ranura 5) / DER_ENTITLEMENTS_BLOB (ranura 7)
		uint32_t len = 0;
		uint8_t* b = cs_blob(pid, op == 7 ? 5 : 7, &len);
		long r = copy_token(b, len, uaddr, usize);
		free(b);
		return r;
	}
	case 11: case 14: {                                          // IDENTITY / TEAMID: cadenas del CodeDirectory (ranura 0)
		uint32_t len = 0;
		uint8_t* cd = cs_blob(pid, 0, &len);
		const char* s = NULL;
		if (cd && len >= 52 && be32(cd) == 0xfade0c02) {
			uint32_t off = op == 11 ? be32(cd + 20) : (be32(cd + 8) >= 0x20200 ? be32(cd + 48) : 0);
			if (off && off < len && memchr(cd + off, 0, len - off)) s = (const char*)cd + off;
		}
		long r;
		if (!s) r = op == 14 ? -D_ENOENT : copy_token(NULL, 0, uaddr, usize);
		else {                                                   // cabecera falsa {0, longitud total} y la cadena detrás
			uint32_t sl = (uint32_t)strlen(s) + 1, tot = sl + 8;
			uint8_t* b = malloc(tot);
			memset(b, 0, 8); uint32_t bl = htonl(tot); memcpy(b + 4, &bl, 4); memcpy(b + 8, s, sl);
			r = copy_token(b, tot, uaddr, usize);
			free(b);
		}
		free(cd);
		return r;
	}
	case 5: {                                                    // CDHASH: del CodeDirectory de mejor hash (SHA-256 si lo hay)
		if (usize < 20) return -D_EINVAL;
		uint32_t len = 0, best_len = 0;
		uint8_t* best = cs_blob(pid, 0, &best_len);              // el principal (ranura 0) y los alternativos (0x1000...)
		for (uint32_t slot = 0x1000; slot < 0x1005; slot++) {
			uint8_t* alt = cs_blob(pid, slot, &len);
			if (!alt) continue;
			int rank_alt = len > 37 ? (alt[37] == 1 ? 1 : alt[37] == 3 ? 2 : alt[37] == 2 ? 3 : alt[37] == 4 ? 4 : 0) : 0;
			int rank_best = best && best_len > 37 ? (best[37] == 1 ? 1 : best[37] == 3 ? 2 : best[37] == 2 ? 3 : best[37] == 4 ? 4 : 0) : 0;
			if (rank_alt > rank_best) { free(best); best = alt; best_len = len; } else free(alt);
		}
		if (!best || best_len < 44 || be32(best) != 0xfade0c02) { free(best); return -D_EINVAL; }   // sin firma
		uint8_t h[20];
		cd_hash20(best, best_len, h);
		free(best);
		return safe_write(uaddr, h, 20) == 20 ? 0 : -D_EFAULT;
	}
	case 6: {                                                    // PIDOFFSET: dónde empieza en el archivo la rebanada que se ejecuta
		char path[4096];
		if (usize < 8) return -D_EINVAL;
		if (exe_host_path(pid, path, sizeof path) != 0) return -3;  // ESRCH
		int fd = open(path, O_RDONLY | O_CLOEXEC);
		if (fd < 0) return -3;
		int64_t off = slice_offset(fd);
		close(fd);
		if (off < 0) return -D_EINVAL;
		uint64_t v = (uint64_t)off;
		return safe_write(uaddr, &v, 8) == 8 ? 0 : -D_EFAULT;
	}
	default:
		logf_("    csops: operación %d sin implementar\n", op);
		return -D_EINVAL;
	}
}

static long bsd_csops(struct ctx* c) { return cs_ops((int)c->a[0], (int)c->a[1], c->a[2], c->a[3]); }
// csops_audittoken(pid, operación, búfer, tamaño, token*)
static long bsd_csops_audittoken(struct ctx* c) { return cs_ops((int)c->a[0], (int)c->a[1], c->a[2], c->a[3]); }

void emu_cs_init(void) { reg_bsd(169, bsd_csops); reg_bsd(170, bsd_csops_audittoken); }
