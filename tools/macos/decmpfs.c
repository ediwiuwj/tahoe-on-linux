// decmpfs: reconstruye archivos con compresión transparente de APFS/HFS+ (decmpfs) que un extractor dejó como tres piezas:
// el archivo vacío, "archivo:com.apple.decmpfs" (cabecera) y "archivo:com.apple.ResourceFork" (bloques comprimidos).
// Es un programa de macOS (se ejecuta dentro de tahoe-run) porque usa compression_decode_buffer de libcompression de
// Apple, la única que trae LZBITMAP. Lee de la entrada estándar las rutas de los archivos (una por línea), escribe cada
// uno descomprimido en su lugar y borra las dos piezas. Compilar: tools/macos/build.sh
//
// Cabecera decmpfs (16 bytes, little endian): "fpmc", tipo (u32), tamaño sin comprimir (u64); en los tipos «attr» los
// datos van detrás de la cabecera, en los «rsrc» en el resource fork: una tabla de desplazamientos u32 (bloques de 64 KB)
// seguida de los bloques. Un bloque que empieza por 0xFF (lzfse/lzbitmap) o 0x06 (lzvn) va sin comprimir.
typedef unsigned long size_t; typedef long ssize_t; typedef unsigned int u32; typedef unsigned long long u64;
int open(const char*, int, ...); ssize_t read(int, void*, size_t); ssize_t write(int, const void*, size_t); int close(int);
long lseek(int, long, int); void* malloc(size_t); void free(void*); int unlink(const char*); int printf(const char*, ...);
size_t strlen(const char*); void* memcpy(void*, const void*, size_t); int snprintf(char*, size_t, const char*, ...);
ssize_t getline(char**, size_t*, void*); extern void* __stdinp;
size_t compression_decode_buffer(unsigned char*, size_t, const unsigned char*, size_t, void*, int);
enum { ALG_ZLIB = 0x205, ALG_LZFSE = 0x801, ALG_LZVN = 0x900, ALG_LZBITMAP = 0x702 };
#define O_WRONLY 1
#define O_TRUNC 0x400
#define CHUNK 65536

static unsigned char* slurp(const char* p, long* n) {
	int fd = open(p, 0); if (fd < 0) return 0;
	long sz = lseek(fd, 0, 2); lseek(fd, 0, 0);
	unsigned char* b = malloc(sz ? sz : 1); long got = 0;
	while (got < sz) { ssize_t r = read(fd, b + got, sz - got); if (r <= 0) break; got += r; }
	close(fd); *n = got; return got == sz ? b : 0;
}
static int alg_of(u32 t) {
	switch (t) { case 3: case 4: return ALG_ZLIB; case 7: case 8: return ALG_LZVN; case 11: case 12: return ALG_LZFSE; case 13: case 14: return ALG_LZBITMAP; }
	return -1;
}
// un bloque: sin comprimir si empieza por la marca correspondiente; si no, decodificar (zlib de decmpfs lleva cabecera
// de 2 bytes de zlib que libcompression no quiere)
static long chunk(int alg, const unsigned char* in, long n, unsigned char* out, long cap) {
	if (n == 0) return 0;
	if ((alg == ALG_LZVN && in[0] == 0x06) || (alg != ALG_LZVN && alg != ALG_ZLIB && in[0] == 0xFF) || (alg == ALG_ZLIB && (in[0] & 0x0F) == 0x0F)) {
		if (n - 1 > cap) return -1; memcpy(out, in + 1, n - 1); return n - 1;
	}
	if (alg == ALG_ZLIB) { in += 2; n -= 2; }
	return (long)compression_decode_buffer(out, cap, in, n, 0, alg);
}
static int one(const char* path) {
	char hp[4096], rp[4096]; snprintf(hp, sizeof hp, "%s:com.apple.decmpfs", path); snprintf(rp, sizeof rp, "%s:com.apple.ResourceFork", path);
	long hn; unsigned char* h = slurp(hp, &hn);
	if (!h || hn < 16 || h[0] != 'f' || h[1] != 'p' || h[2] != 'm' || h[3] != 'c') { printf("cabecera mala: %s\n", path); return 1; }
	u32 type; u64 size; memcpy(&type, h + 4, 4); memcpy(&size, h + 8, 8);
	int alg = alg_of(type);
	unsigned char* out = malloc(size ? size : 1); long done = 0;
	if (type == 1) { memcpy(out, h + 16, size); done = size; }                      // sin comprimir, en la cabecera
	else if (alg < 0) { printf("tipo %u no soportado: %s\n", type, path); return 1; }
	else if (type % 2 == 1) { done = chunk(alg, h + 16, hn - 16, out, size); }        // datos tras la cabecera
	else {
		long rn; unsigned char* r = slurp(rp, &rn);
		if (!r) { printf("sin resource fork: %s\n", path); return 1; }
		if (type == 4) {                                                       // zlib: formato clásico de resource fork
			u32 doff = (u32)r[0] << 24 | r[1] << 16 | r[2] << 8 | r[3];
			const unsigned char* t = r + doff + 4; u32 cnt; memcpy(&cnt, t, 4);
			for (u32 i = 0; i < cnt && done >= 0; i++) { u32 o, l; memcpy(&o, t + 4 + i * 8, 4); memcpy(&l, t + 8 + i * 8, 4);
				long k = chunk(alg, t + o, l, out + done, size - done); if (k < 0) done = -1; else done += k; }
		} else {
			u32 first; memcpy(&first, r, 4); u32 cnt = first / 4 - 1;
			for (u32 i = 0; i < cnt && done >= 0; i++) { u32 a, b; memcpy(&a, r + i * 4, 4); memcpy(&b, r + i * 4 + 4, 4);
				long cap = size - done < CHUNK ? (long)(size - done) : CHUNK;
				long k = chunk(alg, r + a, (long)b - a, out + done, cap); if (k <= 0 && cap > 0) done = -1; else done += k; }
		}
		free(r);
	}
	if (done != (long)size) { printf("tamaño %ld en vez de %llu (tipo %u): %s\n", done, size, type, path); return 1; }
	int fd = open(path, O_WRONLY | O_TRUNC); if (fd < 0) { printf("no puedo escribir: %s\n", path); return 1; }
	long w = 0; while (w < done) { ssize_t k = write(fd, out + w, done - w); if (k <= 0) break; w += k; }
	close(fd); free(out); free(h);
	if (w != done) { printf("escritura corta: %s\n", path); return 1; }
	unlink(hp); unlink(rp);
	return 0;
}
int main(void) {
	char* line = 0; size_t cap = 0; ssize_t n; int ok = 0, mal = 0;
	while ((n = getline(&line, &cap, __stdinp)) > 0) {
		if (line[n - 1] == '\n') line[--n] = 0;
		if (n) { if (one(line)) mal++; else ok++; }
	}
	printf("descomprimidos %d, con error %d\n", ok, mal);
	return mal != 0;
}
