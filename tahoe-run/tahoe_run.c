// tahoe-run: lanza un programa de macOS 26 (x86_64) sobre el kernel de Linux, haciendo de "kernel".
//
// Hace lo que XNU hace al ejecutar un proceso:
//   1. mapea la dyld_shared_cache en sus direcciones fijas (la "shared region"),
//   2. fabrica el commpage de macOS 26,
//   3. carga el dyld de Tahoe y el programa (Mach-O) y monta la pila [mh][argc][argv][envp][apple],
//   4. intercepta las syscalls (PR_SET_SYSCALL_USER_DISPATCH) y las emula en un manejador de SIGSYS.
//
// Uso: tahoe-run <dyld_shared_cache_x86_64> <dyld> <programa> [args...]
// Variables: TAHOE_TRACE=1 registra todas las syscalls; por defecto solo las no implementadas.
//
// Hito actual: arrancar dyld y ver qué pide. Todo el código es propio (sin código de Darling).
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <linux/prctl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/mman.h>
#include <sys/personality.h>
#include <sys/prctl.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <pwd.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

#include "sysnames.h"
#include <sched.h>
#include "tahoe.h"

#ifndef SYS_USER_DISPATCH
#define SYS_USER_DISPATCH 2
#endif
#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

#define COMMPAGE     0x00007fffffe00000UL
// dyld se carga por debajo de la región compartida (0x7ff800000000-0x7ffffe600000 en XNU): la caché de un macOS completo
// (principal + 6 subcachés) la llena casi entera.
#define DYLD_SLIDE   0x00007ff000000000UL
#define STACK_SIZE   (8UL << 20)

int trace_all;
__thread int g_watch;   // diagnóstico: traza completa de las llamadas de un hilo concreto (ver thread_main)

// ---------------------------------------------------------------- utilidades
static int logfd = 2;   // descriptor privado del registro: el programa (p. ej. launchd) puede redirigir el 2 a /dev/null
void logf_(const char* fmt, ...) {
	char buf[1024];
	va_list ap;
	va_start(ap, fmt);
	int n = vsnprintf(buf, sizeof buf, fmt, ap);
	va_end(ap);
	if (n > 0) {
		ssize_t r = write(logfd, buf, n < (int)sizeof buf ? n : (int)sizeof buf - 1);
		(void)r;
	}
}
#define LOG(...) logf_("tahoe-run: " __VA_ARGS__)
#define DIE(...) do { LOG(__VA_ARGS__); logf_("\n"); _exit(111); } while (0)


// ---------------------------------------------------------------- errno de Linux -> Darwin
int darwin_errno(int e) {
	if (e >= 1 && e <= 34 && e != EAGAIN) return e;  // coinciden hasta ERANGE, salvo EAGAIN
	switch (e) {
	case EAGAIN: return 35;        case EINPROGRESS: return 36;   case EALREADY: return 37;
	case ENOTSOCK: return 38;      case EDESTADDRREQ: return 39;  case EMSGSIZE: return 40;
	case EPROTOTYPE: return 41;    case ENOPROTOOPT: return 42;   case EPROTONOSUPPORT: return 43;
	case EOPNOTSUPP: return 102;   case EAFNOSUPPORT: return 47;  case EADDRINUSE: return 48;
	case EADDRNOTAVAIL: return 49; case ENETDOWN: return 50;      case ENETUNREACH: return 51;
	case ECONNABORTED: return 53;  case ECONNRESET: return 54;    case ENOBUFS: return 55;
	case EISCONN: return 56;       case ENOTCONN: return 57;      case ETIMEDOUT: return 60;
	case ECONNREFUSED: return 61;  case ELOOP: return 62;         case ENAMETOOLONG: return 63;
	case EHOSTUNREACH: return 65;  case ENOTEMPTY: return 66;     case EDQUOT: return 69;
	case ESTALE: return 70;        case ENOLCK: return 77;        case ENOSYS: return 78;
	case EOVERFLOW: return 84;     case ECANCELED: return 89;     case EIDRM: return 90;
	case ENOMSG: return 91;        case EILSEQ: return 92;        case EBADMSG: return 94;
	case EDEADLK: return 11;
	default: return e;
	}
}
enum { DARWIN_ENOSYS = 78, DARWIN_EINVAL = 22, DARWIN_EBADF = 9, DARWIN_ENOENT = 2 };


// ---------------------------------------------------------------- lectura segura de la memoria del programa
// Un puntero erróneo del programa no debe tumbar al lanzador: se copia con process_vm_readv.
// El acceso a la memoria del programa va por /proc/self/mem: devuelve un error limpio si la dirección no
// existe. (process_vm_readv/writev sobre el propio proceso fallaba con EINVAL dentro del manejador.)
// El descriptor se reserva en un número alto para no desplazar los descriptores que ve el programa.
static int memfd = -1;
static void memfd_init(void) {
	int fd = open("/proc/self/mem", O_RDWR | O_CLOEXEC);
	if (fd < 0) DIE("no puedo abrir /proc/self/mem: %s", strerror(errno));
	memfd = fcntl(fd, F_DUPFD_CLOEXEC, FD_INTERNAL_BASE);
	close(fd);
	if (memfd < 0) DIE("no puedo reservar el descriptor de /proc/self/mem: %s", strerror(errno));
}
// Tras fork: /proc/self/mem queda ligado a la memoria de quien lo abrió, así que el hijo heredaba el del padre y leía y
// escribía la memoria del padre (respuestas MIG incluidas). Se reabre en el mismo número.
void memfd_reopen(void) {
	int fd = open("/proc/self/mem", O_RDWR | O_CLOEXEC);
	if (fd < 0) DIE("hijo: no puedo abrir /proc/self/mem: %s", strerror(errno));
	if (dup3(fd, memfd, O_CLOEXEC) < 0) DIE("hijo: dup3 de /proc/self/mem: %s", strerror(errno));
	close(fd);
}
ssize_t safe_read(uint64_t addr, void* buf, size_t len) {
	size_t done = 0;
	while (done < len) {
		ssize_t n = pread(memfd, (char*)buf + done, len - done, (off_t)(addr + done));
		if (n <= 0) return done ? (ssize_t)done : -1;
		done += n;
	}
	return (ssize_t)done;
}
ssize_t safe_write(uint64_t addr, const void* buf, size_t len) {
	size_t done = 0;
	while (done < len) {
		ssize_t n = pwrite(memfd, (const char*)buf + done, len - done, (off_t)(addr + done));
		if (n <= 0) return done ? (ssize_t)done : -1;
		done += n;
	}
	return (ssize_t)done;
}
int safe_string(uint64_t addr, char* out, size_t max) {
	if (!addr) { out[0] = 0; return -1; }
	size_t n = 0;
	while (n + 1 < max) {
		char ch;
		if (safe_read(addr + n, &ch, 1) != 1) { out[n] = 0; return n ? 0 : -1; }
		out[n++] = ch;
		if (!ch) return 0;
	}
	out[n] = 0;
	return 0;
}
const char* tahoe_root;
char cache_guest_path[1024];
const char *g_cache_path, *g_dyld_path, *g_self_exe;
uint64_t cache_ino;   // TAHOE_ROOT: raíz del sistema de archivos de macOS 26 (el recovery extraído)

static void strlcpy_(char* d, const char* s, size_t n) { size_t i = 0; for (; i + 1 < n && s[i]; i++) d[i] = s[i]; if (n) d[i] = 0; }

// ---------------------------------------------------------------- shared region (caché)
static char** g_argv;
extern char** environ;

// ---------------------------------------------------------------- slide de la caché (formato v2)
// Los punteros de __DATA/__DATA_CONST se guardan codificados: el kernel los convierte en punteros reales al
// mapear la caché, incluso con slide 0. Es dyld_cache_slide_info2 (common/DyldSharedCache.cpp, rebaseChainV2).
struct slide_info2 {
	uint32_t version, page_size, page_starts_offset, page_starts_count, page_extras_offset, page_extras_count;
	uint64_t delta_mask, value_add;
};
static void rebase_chain_v2(uint8_t* page, uint32_t start, uint64_t slide, const struct slide_info2* si) {
	const uint64_t delta_mask = si->delta_mask, value_mask = ~delta_mask;
	const unsigned delta_shift = __builtin_ctzll(delta_mask) - 2;
	uint32_t off = start, delta = 1;
	while (delta != 0) {
		uint64_t* loc = (uint64_t*)(page + off);
		uint64_t raw = *loc;
		delta = (uint32_t)((raw & delta_mask) >> delta_shift);
		uint64_t value = raw & value_mask;
		if (value != 0) value += si->value_add + slide;
		*loc = value;
		off += delta;
	}
}
static void apply_slide_v2(uint8_t* data, const struct slide_info2* si, uint64_t slide) {
	const uint16_t* starts = (const uint16_t*)((const uint8_t*)si + si->page_starts_offset);
	const uint16_t* extras = (const uint16_t*)((const uint8_t*)si + si->page_extras_offset);
	for (uint32_t i = 0; i < si->page_starts_count; i++) {
		uint8_t* page = data + (uint64_t)si->page_size * i;
		uint16_t entry = starts[i];
		if (entry == 0x4000) continue;                                   // sin rebase
		if (entry & 0x8000) {                                            // varias cadenas en esta página
			uint16_t idx = entry & 0x3FFF;
			for (int done = 0; !done; idx++) {
				uint16_t info = extras[idx];
				rebase_chain_v2(page, (info & 0x3FFF) * 4, slide, si);
				done = info & 0x8000;
			}
		} else {
			rebase_chain_v2(page, entry * 4, slide, si);
		}
	}
}
static void apply_cache_slide(int fd, const char* path) {
	uint32_t off, cnt;
	if (pread(fd, &off, 4, 0x138) != 4 || pread(fd, &cnt, 4, 0x13c) != 4 || !off || !cnt || cnt > 16) return;
	struct { uint64_t address, size, fileoff, slide_off, slide_size, flags; uint32_t maxprot, initprot; } m[16];
	if (pread(fd, m, cnt * sizeof m[0], off) != (ssize_t)(cnt * sizeof m[0])) return;
	for (uint32_t i = 0; i < cnt; i++) {
		if (!m[i].slide_size) continue;
		struct slide_info2* si = malloc(m[i].slide_size);
		if (pread(fd, si, m[i].slide_size, m[i].slide_off) != (ssize_t)m[i].slide_size) DIE("slide info ilegible en %s", path);
		if (si->version != 2) DIE("%s: versión de slide info %u no soportada (solo v2)", path, si->version);
		mprotect((void*)m[i].address, m[i].size, PROT_READ | PROT_WRITE);
		apply_slide_v2((uint8_t*)m[i].address, si, 0);
		mprotect((void*)m[i].address, m[i].size, m[i].initprot & 7);
		free(si);
	}
}

// Aplicar el slide de la caché toca ~60 MB de páginas de datos. Hecho en cada proceso, 40 procesos copian 2.4 GB y la máquina
// acaba en swap. Se hace UNA vez: las páginas ya corregidas se guardan en un archivo (mismas posiciones que la caché original) y
// cada proceso las mapea MAP_PRIVATE desde él, así que comparten la caché de páginas del núcleo y solo copian lo que escriben.
static int slid_open(const char* path, int create_flags, char* out, size_t cap) {
	struct stat st;
	if (stat(path, &st) != 0) return -1;
	// El directorio lo fija el primer proceso y se hereda por TAHOE_CACHE_DIR: los servicios que lanza launchd no tienen HOME,
	// y sin esto cada uno recalculaba el slide (45 MB privados por proceso) y escribía su propia copia.
	const char* dir = getenv("TAHOE_CACHE_DIR");
	char d[512];
	if (!dir) {
		const char* h = getenv("HOME");
		struct passwd* pw = h ? NULL : getpwuid(getuid());
		snprintf(d, sizeof d, "%s/.cache/tahoe-run", h ? h : (pw ? pw->pw_dir : "/tmp"));
		setenv("TAHOE_CACHE_DIR", d, 1);
		dir = getenv("TAHOE_CACHE_DIR");
	}
	mkdir(dir, 0755);
	const char* base = strrchr(path, '/'); base = base ? base + 1 : path;
	snprintf(out, cap, "%s/%s.%llx.%llx.slid", dir, base, (unsigned long long)st.st_size, (unsigned long long)st.st_mtime);
	return open(out, create_flags, 0644);
}

static void map_one(const char* path) {
	int fd = open(path, O_RDONLY);
	if (fd < 0) DIE("no puedo abrir %s: %s", path, strerror(errno));
	uint8_t h[0x20];
	if (pread(fd, h, sizeof h, 0) != sizeof h || memcmp(h, "dyld_v1", 7) != 0) DIE("%s no es una dyld_shared_cache", path);
	uint32_t off = *(uint32_t*)(h + 0x10), cnt = *(uint32_t*)(h + 0x14);
	struct { uint64_t addr, size, fileoff; uint32_t maxprot, initprot; } m[16];
	if (cnt > 16 || pread(fd, m, cnt * sizeof m[0], off) != (ssize_t)(cnt * sizeof m[0])) DIE("tabla de mapeos ilegible en %s", path);

	// ¿Qué mapeos llevan slide? (los mismos que recorre apply_cache_slide)
	uint32_t soff = 0, scnt = 0;
	struct { uint64_t address, size, fileoff, slide_off, slide_size, flags; uint32_t maxprot, initprot; } sm[16];
	int has_slide[16] = { 0 };
	if (pread(fd, &soff, 4, 0x138) == 4 && pread(fd, &scnt, 4, 0x13c) == 4 && soff && scnt && scnt <= 16 &&
	    pread(fd, sm, scnt * sizeof sm[0], soff) == (ssize_t)(scnt * sizeof sm[0]))
		for (uint32_t i = 0; i < scnt && i < cnt; i++) has_slide[i] = sm[i].slide_size != 0;

	char slidpath[1024];
	int sfd = slid_open(path, O_RDONLY, slidpath, sizeof slidpath);        // ya existe: mapear los datos corregidos
	int have_slid = sfd >= 0;
	for (uint32_t i = 0; i < cnt; i++) {
		int from = (have_slid && has_slide[i]) ? sfd : fd;
		void* p = mmap((void*)m[i].addr, m[i].size, m[i].initprot, MAP_PRIVATE | MAP_FIXED_NOREPLACE, from, m[i].fileoff);
		if (p == MAP_FAILED) {
			if (errno == EEXIST && !getenv("TAHOE_REEXEC")) {
				// algo del proceso ya ocupa esa dirección (ASLR): reintentar sin aleatorización
				setenv("TAHOE_REEXEC", "1", 1);
				personality(personality(0xffffffff) | ADDR_NO_RANDOMIZE);
				// por su ruta y no por /proc/self/exe: si no, Linux llama al proceso "exe" y las herramientas que buscan
				// tahoe-run (limpieza, guardián de memoria, diagnóstico) dejan de verlo
				execve(g_self_exe ? g_self_exe : "/proc/self/exe", g_argv, environ);
			}
			DIE("mmap de la caché falló en 0x%lx (+0x%lx): %s", m[i].addr, m[i].size, strerror(errno));
		}
	}
	if (!have_slid) {
		apply_cache_slide(fd, path);
		// Publicar el resultado para los demás procesos: archivo temporal y rename atómico (si dos procesos compiten, gana uno).
		char tmp[1100];
		snprintf(tmp, sizeof tmp, "%s.%d.tmp", slidpath, (int)getpid());
		int tf = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
		if (tf >= 0) {
			struct stat st; fstat(fd, &st);
			int ok = ftruncate(tf, st.st_size) == 0;
			for (uint32_t i = 0; ok && i < cnt; i++) {
				if (!has_slide[i]) continue;
				// la memoria está en RW o R según initprot; los mapeos con slide son legibles
				uint64_t done = 0;
				while (done < m[i].size) {
					ssize_t w = pwrite(tf, (void*)(m[i].addr + done), m[i].size - done, m[i].fileoff + done);
					if (w <= 0) { ok = 0; break; }
					done += (uint64_t)w;
				}
			}
			close(tf);
			if (ok && rename(tmp, slidpath) == 0) { if (trace_all) logf_("    caché con slide guardada en %s\n", slidpath); }
			else unlink(tmp);
		}
	}
	if (sfd >= 0) close(sfd);
	close(fd);
}

// La "región dinámica" de la caché la construye launchd en macOS y la copia shared_region_map_and_slide_2_np.
// Diseño (DyldSharedCache::DynamicRegion, versión 3): marca, FileIdTuple, desplazamientos de rutas, flags.
static void map_dynamic_region(const char* main_path) {
	uint64_t off, maxsize;
	int fd = open(main_path, O_RDONLY);
	if (fd < 0 || pread(fd, &off, 8, 0x1f0) != 8 || pread(fd, &maxsize, 8, 0x1f8) != 8) DIE("no puedo leer dynamicDataOffset de la cabecera");
	struct stat st;
	fstat(fd, &st);
	close(fd);
	if (!off || !maxsize) return;
	uint8_t* p = mmap((void*)(CACHE_BASE + off), maxsize, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
	if (p == MAP_FAILED) DIE("región dinámica en 0x%lx: %s", CACHE_BASE + off, strerror(errno));
	cache_ino = st.st_ino;
	{
		size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
		const char* g = (rl && !strncmp(main_path, tahoe_root, rl) && main_path[rl] == '/') ? main_path + rl : main_path;
		strlcpy_(cache_guest_path, g, sizeof cache_guest_path);
	}
	memcpy(p, "dyld_data    v3", 16);
	*(uint64_t*)(p + 16) = (uint64_t)st.st_dev;                   // fsid
	*(uint64_t*)(p + 24) = (uint64_t)st.st_ino;                   // fsobjid
	*(uint32_t*)(p + 36) = 80;                                    // _cachePathOffset = sizeof(DynamicRegion)
	strlcpy_((char*)p + 80, main_path, maxsize - 80);
	// En un macOS completo la caché viene en el cryptex del sistema (montado en Preboot): launchd anota también su raíz
	// (_osCryptexPathOffset), que dyld antepone a las rutas que busca fuera de la caché. En la Recovery no hay cryptex: 0.
	static const char cryptex[] = "/System/Volumes/Preboot/Cryptexes/OS";
	uint32_t coff = 0;
	if (!strncmp(cache_guest_path, cryptex, sizeof cryptex - 1) && cache_guest_path[sizeof cryptex - 1] == '/') {
		coff = 80 + (uint32_t)strlen(main_path) + 1;
		if (coff + sizeof cryptex <= maxsize) memcpy(p + coff, cryptex, sizeof cryptex); else coff = 0;
	}
	*(uint32_t*)(p + 32) = coff;                                  // _osCryptexPathOffset
	mprotect(p, maxsize, PROT_READ);
}

static void map_cache(const char* main_path) {
	map_one(main_path);
	map_dynamic_region(main_path);
	glob_t g;
	char pat[4096];
	snprintf(pat, sizeof pat, "%s.[0-9]*", main_path);
	if (glob(pat, 0, NULL, &g) == 0) {
		for (size_t i = 0; i < g.gl_pathc; i++) map_one(g.gl_pathv[i]);
		globfree(&g);
	}
}

// ---------------------------------------------------------------- commpage (macOS 26, x86_64)
// Diseño tomado de osfmk/i386/cpu_capabilities.h de XNU 12377.
uint64_t tsc_hz;                                         // frecuencia del TSC medida (setup_commpage)
static uint64_t tsc_hz_estimate(void) {
	struct timespec a, b;
	clock_gettime(CLOCK_MONOTONIC, &a);
	uint64_t t0 = __builtin_ia32_rdtsc();
	do { clock_gettime(CLOCK_MONOTONIC, &b); } while ((b.tv_sec - a.tv_sec) * 1000000000L + (b.tv_nsec - a.tv_nsec) < 30000000L);
	uint64_t t1 = __builtin_ia32_rdtsc();
	uint64_t ns = (b.tv_sec - a.tv_sec) * 1000000000UL + (b.tv_nsec - a.tv_nsec);
	return (t1 - t0) * 1000000000UL / ns;
}

static void setup_commpage(void) {
	uint8_t* p = mmap((void*)COMMPAGE, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
	if (p == MAP_FAILED) DIE("no puedo mapear el commpage: %s", strerror(errno));
	memcpy(p, "commpage 64-bit", 16);
	uint64_t caps = 0x00000001 | 0x2 | 0x4 | 0x8 | 0x20 /*cache64*/ | 0x100 | 0x1000 /*AES*/ | 0x04000000 | 0x10000000 | 0x40000000 | 0x80000000;
	long ncpu = sysconf(_SC_NPROCESSORS_ONLN);
	caps |= ((uint64_t)ncpu & 0xff) << 16;
	*(uint64_t*)(p + 0x10) = caps;
	*(uint16_t*)(p + 0x1E) = 14;                         // _COMM_PAGE_THIS_VERSION
	*(uint32_t*)(p + 0x20) = (uint32_t)caps;
	p[0x22] = (uint8_t)ncpu;
	*(uint16_t*)(p + 0x26) = 64;                         // línea de caché
	p[0x34] = (uint8_t)ncpu; p[0x35] = (uint8_t)(ncpu > 1 ? ncpu / 2 : 1); p[0x36] = (uint8_t)ncpu;
	*(uint64_t*)(p + 0x38) = (uint64_t)sysconf(_SC_PHYS_PAGES) * (uint64_t)sysconf(_SC_PAGESIZE);
	p[0x4D] = 12; p[0x4E] = 12;                          // desplazamientos de página (4 KiB)
	// nanotime: ns = ns_base + (((tsc - tsc_base) << shift) * scale >> 32)
	// La frecuencia del TSC se mide una vez (30 ms de espera activa) y se guarda en /dev/shm/tahoe-tsc-hz para los demás
	// procesos: medirla en cada arranque costaba 30 ms de CPU por proceso (dos por servicio: xpcproxy y el programa), y
	// en el arranque de macOS, con decenas de procesos a la vez, alargaba la avalancha de servicios.
	uint64_t hz = 0;
	{
		char b[32] = ""; int f = open("/dev/shm/tahoe-tsc-hz", O_RDONLY | O_CLOEXEC);
		if (f >= 0) { ssize_t n = read(f, b, sizeof b - 1); close(f); if (n > 0) { b[n] = 0; hz = strtoull(b, NULL, 10); } }
		if (hz < 100000000ULL || hz > 10000000000ULL) {          // sin guardar o absurda: medir y guardar
			hz = tsc_hz_estimate();
			char tmp[64]; snprintf(tmp, sizeof tmp, "/dev/shm/tahoe-tsc-hz.%d", (int)getpid());
			f = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
			if (f >= 0) { int n = snprintf(b, sizeof b, "%lu\n", (unsigned long)hz); if (write(f, b, (size_t)n) == n) { close(f); rename(tmp, "/dev/shm/tahoe-tsc-hz"); } else { close(f); unlink(tmp); } }
		}
	}
	tsc_hz = hz;
	struct timespec now;
	clock_gettime(CLOCK_MONOTONIC, &now);
	*(uint64_t*)(p + 0x50) = __builtin_ia32_rdtsc();
	*(uint32_t*)(p + 0x58) = (uint32_t)(((unsigned __int128)1000000000UL << 32) / hz);
	*(uint32_t*)(p + 0x5c) = 0;
	*(uint64_t*)(p + 0x60) = now.tv_sec * 1000000000UL + now.tv_nsec;
	*(uint32_t*)(p + 0x68) = 1;                          // generación (distinta de 0 = válido)
	mprotect(p, PAGE, PROT_READ);
	LOG("commpage listo (tsc ~ %lu MHz, %ld cpus)\n", hz / 1000000UL, ncpu);
}

// ---------------------------------------------------------------- cargador Mach-O
struct macho_hdr { uint32_t magic, cputype, cpusubtype, filetype, ncmds, sizeofcmds, flags, reserved; };
struct lcmd { uint32_t cmd, cmdsize; };
struct seg64 { uint32_t cmd, cmdsize; char segname[16]; uint64_t vmaddr, vmsize, fileoff, filesize; uint32_t maxprot, initprot, nsects, flags; };
#define LC_SEGMENT_64 0x19
#define LC_UNIXTHREAD 0x5
#define LC_MAIN       0x80000028

struct image { uint64_t mh; uint64_t entry; uint32_t filetype; uint64_t hi; };

static char prog_name[64] = "?";                      // programa de macOS de este proceso (para el registro)
static double lm_ms(const struct timespec* a) { struct timespec b; clock_gettime(CLOCK_MONOTONIC, &b); return (b.tv_sec - a->tv_sec) * 1e3 + (b.tv_nsec - a->tv_nsec) / 1e6; }
static void load_macho(const char* path, uint64_t slide, struct image* out) {
	struct timespec lm0; clock_gettime(CLOCK_MONOTONIC, &lm0); double t_map = 0, t_dsmos = 0, t_prot = 0;   // diagnóstico de velocidad
	int fd = open(path, O_RDONLY);
	if (fd < 0) DIE("no puedo abrir %s: %s", path, strerror(errno));
	uint64_t base_off = 0;
	uint8_t first[8];
	if (pread(fd, first, 8, 0) != 8) DIE("%s ilegible", path);
	if (first[0] == 0xca && first[1] == 0xfe && first[2] == 0xba && first[3] == 0xbe) {  // fat
		uint32_t n = __builtin_bswap32(*(uint32_t*)(first + 4));
		for (uint32_t i = 0; i < n; i++) {
			uint32_t fa[5];
			pread(fd, fa, sizeof fa, 8 + 20 * i);
			if (__builtin_bswap32(fa[0]) == 0x01000007) { base_off = __builtin_bswap32(fa[2]); break; }
		}
		if (!base_off) DIE("%s no tiene rebanada x86_64", path);
	}
	struct macho_hdr mh;
	pread(fd, &mh, sizeof mh, base_off);
	if (mh.magic != 0xfeedfacf || mh.cputype != 0x01000007) DIE("%s no es un Mach-O x86_64", path);
	uint8_t* cmds = malloc(mh.sizeofcmds);
	pread(fd, cmds, mh.sizeofcmds, base_off + sizeof mh);
	out->mh = 0; out->entry = 0; out->filetype = mh.filetype; out->hi = 0;
	uint64_t entryoff = 0;
	int have_main = 0;
	for (uint32_t i = 0, pos = 0; i < mh.ncmds; i++) {
		struct lcmd* lc = (struct lcmd*)(cmds + pos);
		if (lc->cmd == LC_SEGMENT_64) {
			struct seg64* s = (struct seg64*)lc;
			uint64_t addr = s->vmaddr + slide;
			if (s->vmsize == 0 || addr < 0x10000) { pos += lc->cmdsize; continue; }   // __PAGEZERO
			uint64_t maplen = round_up(s->filesize);
			int prot = s->initprot & 7;
			if (s->filesize) {
				struct timespec q; clock_gettime(CLOCK_MONOTONIC, &q);
				void* p = mmap((void*)addr, maplen, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_FIXED_NOREPLACE, fd, base_off + s->fileoff);
				if (p == MAP_FAILED) DIE("%s: mmap de %s en 0x%lx falló: %s", path, s->segname, addr, strerror(errno));
				if (s->vmsize > s->filesize && maplen > s->filesize) memset((void*)(addr + s->filesize), 0, maplen - s->filesize);
				t_map += lm_ms(&q); clock_gettime(CLOCK_MONOTONIC, &q);
				if (s->flags & 0x8) dsmos_unprotect((uint8_t*)addr, s->fileoff, s->filesize, path);   // SG_PROTECTED_VERSION_1
				t_dsmos += lm_ms(&q); clock_gettime(CLOCK_MONOTONIC, &q);
				mprotect((void*)addr, maplen, prot);
				t_prot += lm_ms(&q);
			}
			if (round_up(s->vmsize) > maplen) {
				void* p = mmap((void*)(addr + maplen), round_up(s->vmsize) - maplen, prot ? prot : PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
				if (p == MAP_FAILED) DIE("%s: relleno de %s falló: %s", path, s->segname, strerror(errno));
			}
			if (s->fileoff == 0 && s->filesize) out->mh = addr;
			if (addr + s->vmsize > out->hi) out->hi = addr + s->vmsize;
		} else if (lc->cmd == LC_UNIXTHREAD) {
			uint32_t* t = (uint32_t*)(lc + 1);                // flavor, count, estado
			if (t[0] == 4) out->entry = ((uint64_t*)(t + 2))[16] + slide;   // x86_THREAD_STATE64: rip es el registro 16
		} else if (lc->cmd == LC_MAIN) {
			entryoff = ((uint64_t*)(lc + 1))[0];
			have_main = 1;
		}
		pos += lc->cmdsize;
	}
	if (have_main && out->mh) out->entry = out->mh + entryoff;
	free(cmds);
	close(fd);
	if (!out->mh) DIE("%s: no se encontró el segmento __TEXT", path);
	if (access("/dev/shm/tahoe-log-time", F_OK) == 0)
		logf_("    carga <%d> %s: %.1f ms (mmap %.1f, dsmos %.1f, mprotect %.1f)\n", (int)getpid(), strrchr(path, '/') ? strrchr(path, '/') + 1 : path, lm_ms(&lm0), t_map, t_dsmos, t_prot);
	LOG("<%d> cargado %-28s mh=0x%lx entrada=0x%lx tipo=%u\n", (int)getpid(), strrchr(path, '/') ? strrchr(path, '/') + 1 : path, out->mh, out->entry, out->filetype);
	if (out->filetype == 2) snprintf(prog_name, sizeof prog_name, "%s", strrchr(path, '/') ? strrchr(path, '/') + 1 : path);
}

// ---------------------------------------------------------------- pila inicial
static uint64_t rnd64(void) { uint64_t v = 0; while (!v) { if (getrandom(&v, sizeof v, 0) != sizeof v) v = 0x9e3779b97f4a7c15UL; } return v; }

static uint64_t build_stack(uint64_t exe_mh, int argc, char** argv, char** envp, const char* exe_path) {
	uint8_t* lo = mmap((void*)(STACK_TOP - STACK_SIZE), STACK_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
	if (lo == MAP_FAILED) DIE("no puedo mapear la pila: %s", strerror(errno));
	// Parámetros "apple" que XNU pone al final de la pila y que libpthread, libc y dyld leen al arrancar.
	// ptr_munge, stack_guard y malloc_entropy son semillas aleatorias; el protector de pila lleva el byte bajo a cero.
	enum { NAPPLE = 5 };
	char apple[NAPPLE][1100];
	snprintf(apple[0], sizeof apple[0], "executable_path=%s", exe_path);
	snprintf(apple[1], sizeof apple[1], "main_stack=0x%lx,0x%lx,0x%lx,0x%lx", STACK_TOP, STACK_SIZE, STACK_TOP - STACK_SIZE, STACK_SIZE);
	snprintf(apple[2], sizeof apple[2], "ptr_munge=0x%lx", rnd64());
	snprintf(apple[3], sizeof apple[3], "stack_guard=0x%lx", rnd64() & ~0xffUL);
	snprintf(apple[4], sizeof apple[4], "malloc_entropy=0x%lx,0x%lx", rnd64(), rnd64());
	int nenv = 0;
	while (envp[nenv]) nenv++;
	// copiar las cadenas al final de la pila
	uint64_t sp = STACK_TOP - 64;
	char** av = calloc(argc + 1, sizeof(char*));
	char** ev = calloc(nenv + 1, sizeof(char*));
	char* ap[NAPPLE];
	for (int i = NAPPLE - 1; i >= 0; i--) { sp -= strlen(apple[i]) + 1; memcpy((void*)sp, apple[i], strlen(apple[i]) + 1); ap[i] = (char*)sp; }
	for (int i = nenv - 1; i >= 0; i--) { sp -= strlen(envp[i]) + 1; memcpy((void*)sp, envp[i], strlen(envp[i]) + 1); ev[i] = (char*)sp; }
	for (int i = argc - 1; i >= 0; i--) { sp -= strlen(argv[i]) + 1; memcpy((void*)sp, argv[i], strlen(argv[i]) + 1); av[i] = (char*)sp; }
	sp &= ~15UL;
	// [mh][argc][argv...][0][envp...][0][apple...][0]
	size_t words = 1 + 1 + argc + 1 + nenv + 1 + NAPPLE + 1;
	sp -= words * 8;
	sp &= ~15UL;
	uint64_t* w = (uint64_t*)sp;
	size_t k = 0;
	w[k++] = exe_mh;
	w[k++] = argc;
	for (int i = 0; i < argc; i++) w[k++] = (uint64_t)av[i];
	w[k++] = 0;
	for (int i = 0; i < nenv; i++) w[k++] = (uint64_t)ev[i];
	w[k++] = 0;
	for (int i = 0; i < NAPPLE; i++) w[k++] = (uint64_t)ap[i];
	w[k++] = 0;
	return sp;
}

// ---------------------------------------------------------------- emulación de syscalls
typedef emu_fn bsd_fn;

// BSD: devuelven >= 0 o -errno_de_Darwin
extern void ports_cleanup(void);
// Medida del coste de la emulación (diagnóstico de velocidad): llamadas atendidas por clase y ciclos del TSC dentro del
// manejador (on_sigsys); se registran al salir, junto a la CPU del proceso.
static uint64_t sc_count[4], sc_cycles;
// Cada salida queda en el registro (una línea): sin ella, un servicio que termina por su cuenta desaparece sin rastro.
static void show_frames(struct ctx* c);
// exit: con error y en un proceso de STACKS, también los marcos de quien sale (para saber por qué).
static long bsd_exit(struct ctx* c)  {
	struct rusage ru; getrusage(RUSAGE_SELF, &ru);       // y la CPU que gastó: el coste de los servicios que se relanzan
	                                                     // y cuántas llamadas atendió y el tiempo dentro del manejador
	logf_("    fin <%d> [%s] exit(%d) cpu=%.2fs llamadas bsd=%lu mach=%lu manejador=%.2fs\n", getpid(), prog_name, (int)c->a[0],
	      ru.ru_utime.tv_sec + ru.ru_stime.tv_sec + (ru.ru_utime.tv_usec + ru.ru_stime.tv_usec) / 1e6,
	      (unsigned long)sc_count[2], (unsigned long)sc_count[1], tsc_hz ? (double)sc_cycles / (double)tsc_hz : 0.0);
	if ((int)c->a[0] != 0 && diag_target()) show_frames(c);
	ports_cleanup(); _exit((int)c->a[0]);
}
static long bsd_getpid(struct ctx* c) { (void)c; return getpid(); }
static long bsd_issetugid(struct ctx* c) { (void)c; return 0; }
static long bsd_write(struct ctx* c) {
	// TAHOE_FD2LOG: lo que el programa escribe en stdout/stderr (p. ej. os_log con OS_ACTIVITY_DT_MODE) va al registro.
	static int fd2log = -1;
	// Solo si ese descriptor es /dev/null (lo que launchd da a sus servicios): si es una tubería o un archivo, la salida es de
	// quien lo lee (diskarbitrationd recoge así lo que imprime apfs.util -p; antes se iba al registro).
	if (fd2log < 0) fd2log = getenv("TAHOE_FD2LOG") != NULL;
	unsigned long fd = c->a[0];
	if (fd2log && (fd == 1 || fd == 2)) {
		struct stat st;
		if (fstat((int)fd, &st) == 0 && S_ISCHR(st.st_mode) && st.st_rdev == makedev(1, 3)) fd = (unsigned long)logfd;
	}
	long r = syscall(SYS_write, fd, c->a[1], c->a[2]);
	return r < 0 ? -darwin_errno(errno) : r;
}
static long bsd_read(struct ctx* c) {
	long r = syscall(SYS_read, c->a[0], c->a[1], c->a[2]);
	return r < 0 ? -darwin_errno(errno) : r;
}
static long bsd_close(struct ctx* c) {
	if ((int)c->a[0] >= FD_INTERNAL_BASE) return -9;                 // EBADF: descriptores del emulador (ver fd_hi)
	long r = syscall(SYS_close, c->a[0]);
	return r < 0 ? -darwin_errno(errno) : r;
}
// flags de open de Darwin -> Linux
int darwin_open_flags(int f) {
	int o = f & 3;                                   // O_RDONLY/O_WRONLY/O_RDWR coinciden
	if (f & 0x0004) o |= O_NONBLOCK;
	if (f & 0x0008) o |= O_APPEND;
	if (f & 0x0040) o |= O_ASYNC;
	if (f & 0x0080) o |= O_SYNC;
	if (f & 0x0100) o |= O_NOFOLLOW;
	if (f & 0x0200) o |= O_CREAT;
	if (f & 0x0400) o |= O_TRUNC;
	if (f & 0x0800) o |= O_EXCL;
	if (f & 0x20000) o |= O_NOCTTY;
	if (f & 0x100000) o |= O_DIRECTORY;
	if (f & 0x400000) o |= O_DSYNC;
	if (f & 0x1000000) o |= O_CLOEXEC;
	if (f & 0x200000) o |= O_NOFOLLOW;               // O_SYMLINK: ver open_symlink
	return o;
}
// O_SYMLINK (0x200000) de Darwin: si la ruta es un enlace simbólico, se abre el propio enlace (ditto lo usa para copiar
// enlaces que no apuntan a nada). Con O_NOFOLLOW Linux da ELOOP; entonces se abre el enlace con O_PATH (vale para fstat,
// readlinkat y fchown, que es lo que se hace con él).
long open_symlink(int dfd, const char* p, int dflags, int lflags, long fd) {
	if (fd >= 0 || !(dflags & 0x200000) || errno != ELOOP) return fd;
	return syscall(SYS_openat, dfd, p, O_PATH | O_NOFOLLOW | (lflags & O_CLOEXEC));
}
static long bsd_open(struct ctx* c) {
	char path[4096], full[4200];
	if (safe_string(c->a[0], path, sizeof path) != 0) return -14;          // EFAULT
	tahoe_resolve_strip(path);
	if (!strcmp(path, "/dev/console")) {                                  // sin consola real dentro del entorno
		// diagnóstico: si existe /dev/shm/tahoe-console, la consola es ese archivo (lo que launchd y otros escriben ahí)
		if (access("/dev/shm/tahoe-console", W_OK) == 0) {
			long f = syscall(SYS_open, "/dev/shm/tahoe-console", O_WRONLY | O_APPEND | O_CLOEXEC);
			if (f >= 0 && !(c->a[1] & 0x1000000)) fcntl((int)f, F_SETFD, 0);
			return f < 0 ? -darwin_errno(errno) : f;
		}
		strcpy(path, "/dev/null");
	}
	if (!strcmp(path, "/dev/fsevents")) { int f = fsevents_open(); return f < 0 ? -2 : f; }   // dispositivo de FSEvents emulado
	int flags = darwin_open_flags((int)c->a[1]);
	long fd = -1;
	if (tahoe_root && path[0] == '/' && !tahoe_host_passthrough(path)) {
		if (!tahoe_metal_path(path, full, sizeof full)) snprintf(full, sizeof full, "%s%s", tahoe_root, path);
		fd = open_symlink(AT_FDCWD, full, (int)c->a[1], flags, syscall(SYS_open, full, flags, (int)c->a[2]));
		if (fd < 0 && ((flags & O_CREAT) || !tahoe_host_passthrough(path))) goto done;   // fuera de /dev, solo la raíz de macOS
	}
	if (fd < 0) { char al[64]; fd = syscall(SYS_open, tahoe_dev_alias(path, al, sizeof al), flags, (int)c->a[2]); }
done:
	if (trace_all || fd < 0) logf_("    open(\"%s\") -> %ld%s%s\n", path, fd, fd < 0 ? " " : "", fd < 0 ? strerror(errno) : "");
	return fd < 0 ? -darwin_errno(errno) : fd;
}
// guarded_pwrite_np(fd, guard*, búfer, tamaño, desplazamiento): pwrite; la guarda no se comprueba.
static long bsd_guarded_pwrite(struct ctx* c) { long r = pwrite((int)c->a[0], (const void*)c->a[2], c->a[3], (off_t)c->a[4]); return r < 0 ? -darwin_errno(errno) : r; }
static long bsd_proc_info(struct ctx* c);
// proc_info_extended_id(callnum, pid, flavor, flags, ext_id, arg, buffer, buffersize): proc_info con una comprobación de
// identidad extra (flags PIF_COMPARE_*: versión o uniqueid del pid en ext_id) que aquí no hace falta. Los dos últimos van
// en la pila. La usa proc_pidpath_audittoken (airportd la pide de cada cliente XPC: si falla, rechaza la conexión y el
// cliente reconecta sin parar, decenas de miles de líneas de registro por minuto).
static long bsd_proc_info_ext(struct ctx* c) {
	struct ctx c2 = *c;
	c2.a[0] = c->a[0]; c2.a[1] = c->a[1];
	c2.a[2] = c->a[2];                                   // flavor
	c2.a[3] = c->a[5];                                   // arg
	c2.a[4] = ctx_arg(c, 6);                             // buffer
	c2.a[5] = ctx_arg(c, 7);                             // buffersize
	return bsd_proc_info(&c2);
}
// open_dprotected_np(ruta, oflags, clase, dpflags, mode): como open; la clase de protección de datos no existe aquí.
static long bsd_open_dprotected(struct ctx* c) {
	struct ctx c2 = *c;
	c2.a[2] = c->a[4];
	return bsd_open(&c2);
}
// getlogin(búfer, tamaño): el usuario de la sesión (los servicios corren como root).
static long bsd_getlogin(struct ctx* c) {
	static const char u[] = "root";
	if (c->a[1] < sizeof u) return -34;                   // ERANGE
	return safe_write(c->a[0], u, sizeof u) == (ssize_t)sizeof u ? 0 : -14;
}
static long bsd_enotsup(struct ctx* c) { (void)c; return -45; }   // ENOTSUP: funciones que este sistema no ofrece (personas)
// reboot(howto): solo launchd (pid 1 del espacio de procesos del arranque) apaga o reinicia. Aquí eso es terminar el
// arranque: al salir el pid 1, Linux termina todos los procesos de su espacio (el anfitrión no se toca). launchd aborta si
// reboot() vuelve con error (assertion en launchd + 284047), así que no vuelve. Los demás reciben EPERM, como un no-root.
static long bsd_reboot(struct ctx* c) {
	if (getpid() != 1) return -1;
	logf_("    reinicio pedido (reboot howto=0x%lx) por <%d> [%s]: fin del arranque\n", (unsigned long)c->a[0], getpid(), prog_name);
	ports_cleanup();
	_exit(0);
}
// guarded_open_np(ruta, guard*, guardflags, oflags, mode): como open; la guarda se ignora.
static long bsd_guarded_open(struct ctx* c) {
	struct ctx c2 = *c;
	c2.a[1] = c->a[3];
	c2.a[2] = c->a[4];
	return bsd_open(&c2);
}
static long bsd_guarded_close(struct ctx* c) { struct ctx c2 = *c; return bsd_close(&c2); }              // (fd, guard*)
static long bsd_getentropy(struct ctx* c) {
	long r = syscall(SYS_getrandom, c->a[0], c->a[1], 0);
	return r < 0 ? -darwin_errno(errno) : 0;
}
// La máscara de señales es virtual por ahora: aplicarla de verdad podría bloquear SIGSYS (la interceptación).
static uint32_t virtual_sigmask;
static long bsd_sigprocmask(struct ctx* c) {
	uint32_t set = 0, old = virtual_sigmask;
	if (c->a[1] && safe_read(c->a[1], &set, 4) != 4) return -14;
	if (c->a[1]) {
		if (c->a[0] == 1) virtual_sigmask |= set;          // SIG_BLOCK
		else if (c->a[0] == 2) virtual_sigmask &= ~set;     // SIG_UNBLOCK
		else if (c->a[0] == 3) virtual_sigmask = set;       // SIG_SETMASK
		else return -22;
	}
	if (c->a[2] && safe_write(c->a[2], &old, 4) != 4) return -14;
	return 0;
}
static void show_payload(const char* what, uint64_t ns, uint64_t code, uint64_t payload, uint64_t size, uint64_t reason) {
	char msg[512] = "", pl[600] = "";
	safe_string(reason, msg, sizeof msg);
	if (payload && size) {
		size_t n = size < sizeof pl - 1 ? size : sizeof pl - 1;
		if (safe_read(payload, pl, n) == (ssize_t)n) { for (size_t i = 0; i < n; i++) if (pl[i] && (pl[i] < 32 || pl[i] > 126)) pl[i] = '.'; pl[n] = 0; }
		else pl[0] = 0;
	}
	logf_("\n  === %s: espacio=%lu codigo=%lu\n  === motivo: \"%s\"\n  === carga: \"%s\"\n", what, ns, code, msg, pl);
}
// pid y marcos de la pila de macOS (direcciones de retorno, con tools/diag/sym.py) de quien aborta
static void show_frames(struct ctx* c) {
	logf_("  === pid %d, marcos (rbp):", (int)getpid());
	uint64_t fp = c->uc ? (uint64_t)c->uc->uc_mcontext.gregs[REG_RBP] : 0;
	for (int k = 0; k < 24 && fp; k++) {
		uint64_t fr[2];
		if (safe_read(fp, fr, 16) != 16 || !fr[1]) break;
		logf_(" 0x%lx", fr[1]);
		if (fr[0] <= fp) break;
		fp = fr[0];
	}
	logf_("\n");
}
static long bsd_abort_with_payload(struct ctx* c) {
	show_payload("abort_with_payload", c->a[0], c->a[1], c->a[2], c->a[3], c->a[4]);
	show_frames(c);
	_exit(134);
}
// os_fault_with_payload(espacio, código, carga, tamaño, motivo, banderas): un fallo que se informa sin terminar (libxpc,
// LaunchServices...). Se anota como abort_with_payload y se sigue.
static long bsd_os_fault_with_payload(struct ctx* c) {
	show_payload("os_fault_with_payload", c->a[0], c->a[1], c->a[2], c->a[3], c->a[4]);
	show_frames(c);
	return 0;
}
static long bsd_terminate_with_payload(struct ctx* c) {
	show_payload("terminate_with_payload", c->a[1], c->a[2], c->a[3], c->a[4], c->a[5]);
	show_frames(c);
	_exit(134);
}
// csrctl: consulta de SIP. op 0 = CSR_SYSCALL_CHECK; 0 significa "permitido" (como con SIP desactivado).
static long bsd_csrctl(struct ctx* c) { (void)c; return 0; }
// system_override(plazo, banderas): launchd quita o repone la limitación de CPU y de E/S del sistema (al arrancar o
// parar servicios en masa). En Linux no hay esa limitación: se acepta sin efecto. Si falla (ENOSYS), launchd aborta.
static long bsd_system_override(struct ctx* c) { (void)c; return 0; }
// proc_info: dyld usa la llamada 15 (SET_DYLD_IMAGES) para informar al kernel de las imágenes cargadas.
// Datos de un proceso desde /proc (nativo). El nombre es el del ejecutable de macOS (tahoe-run <caché> <dyld> <exe>).
struct pinfo { int pid, ppid, pgid, status, uid, gid, ruid, rgid, svuid, svgid, nfiles; char comm[17], name[33]; uint64_t start_sec, start_usec; };
static int proc_snapshot(int pid, struct pinfo* o) {
	memset(o, 0, sizeof *o);
	if (pid <= 0) pid = getpid();
	char dir[128], p[160], b[4096];
	if (proc_dir(pid, dir, sizeof dir) != 0) return -1;      // un hilo vivo si el hilo principal ya terminó
	snprintf(p, sizeof p, "%s/stat", dir);
	int fd = open(p, O_RDONLY | O_CLOEXEC);
	if (fd < 0) return -1;
	ssize_t n = read(fd, b, sizeof b - 1); close(fd);
	if (n <= 0) return -1;
	b[n] = 0;
	char* q = strrchr(b, ')');
	if (!q) return -1;
	char st = 0; unsigned long long start = 0; int ppid = 0, pgid = 0;
	// campos tras ")": estado ppid pgrp sesión tty tpgid flags ... (22: starttime)
	sscanf(q + 2, "%c %d %d %*d %*d %*d %*u %*u %*u %*u %*u %*u %*u %*d %*d %*d %*d %*d %*d %llu", &st, &ppid, &pgid, &start);
	o->pid = pid; o->ppid = ppid; o->pgid = pgid;
	o->status = st == 'Z' ? 5 : st == 'T' ? 4 : st == 'R' ? 2 : 3;          // SZOMB / SSTOP / SRUN / SSLEEP
	long hz = sysconf(_SC_CLK_TCK); if (hz <= 0) hz = 100;
	struct timespec now, up; clock_gettime(CLOCK_REALTIME, &now); clock_gettime(CLOCK_BOOTTIME, &up);
	double boot = (double)now.tv_sec + now.tv_nsec / 1e9 - (up.tv_sec + up.tv_nsec / 1e9), s = boot + (double)start / (double)hz;
	o->start_sec = (uint64_t)s; o->start_usec = (uint64_t)((s - (double)(uint64_t)s) * 1e6);
	snprintf(p, sizeof p, "%s/status", dir);
	FILE* f = fopen(p, "r");
	if (f) {
		char ln[256];
		while (fgets(ln, sizeof ln, f)) {
			if (!strncmp(ln, "Uid:", 4)) sscanf(ln + 4, "%d %d %d", &o->ruid, &o->uid, &o->svuid);
			else if (!strncmp(ln, "Gid:", 4)) sscanf(ln + 4, "%d %d %d", &o->rgid, &o->gid, &o->svgid);
		}
		fclose(f);
	}
	snprintf(p, sizeof p, "%s/cmdline", dir);
	fd = open(p, O_RDONLY | O_CLOEXEC);
	if (fd >= 0) {
		n = read(fd, b, sizeof b - 1); close(fd);
		if (n > 0) {
			b[n] = 0;
			const char* a = b; int i = 0;
			while (a < b + n && i < 3) { a += strlen(a) + 1; i++; }
			if (a < b + n) { const char* base = strrchr(a, '/'); base = base ? base + 1 : a; snprintf(o->comm, sizeof o->comm, "%s", base); snprintf(o->name, sizeof o->name, "%s", base); }
		}
	}
	return 0;
}
// struct proc_bsdinfo (136 bytes) / proc_bsdshortinfo (64 bytes)
static void put_bsdinfo(uint8_t* b, const struct pinfo* pi) {
	memset(b, 0, 136);
#define W32(off, v) do { uint32_t t_ = (uint32_t)(v); memcpy(b + (off), &t_, 4); } while (0)
	// pbi_flags: PROC_FLAG_LP64 (todos los procesos son de 64 bits; log lo usa para el tamaño de %ld) y SLEADER si lo es
	W32(0, 0x10 | (getsid(pi->pid) == pi->pid ? 0x20 : 0));
	W32(4, pi->status); W32(12, pi->pid); W32(16, pi->ppid); W32(20, pi->uid); W32(24, pi->gid); W32(28, pi->ruid); W32(32, pi->rgid);
	W32(36, pi->svuid); W32(40, pi->svgid); memcpy(b + 48, pi->comm, 16); memcpy(b + 64, pi->name, 32); W32(100, pi->pgid); W32(112, pi->pgid);
	memcpy(b + 120, &pi->start_sec, 8); memcpy(b + 128, &pi->start_usec, 8);
}
static void put_bsdshortinfo(uint8_t* b, const struct pinfo* pi) {
	memset(b, 0, 64);
	W32(0, pi->pid); W32(4, pi->ppid); W32(8, pi->pgid); W32(12, pi->status); memcpy(b + 16, pi->comm, 16);
	W32(36, pi->uid); W32(40, pi->gid); W32(44, pi->ruid); W32(48, pi->rgid); W32(52, pi->svuid); W32(56, pi->svgid);
#undef W32
}
// Señales por token de auditoría (proc_signal_with_audittoken, proc_signal_delegate, proc_terminate...): así manda launchd las
// señales a sus servicios (launchctl kill, kickstart -k, bootout). El pid va en val[5] del token del destino; en las llamadas
// delegadas el búfer lleva dos tokens, primero el de quien lo pide y luego el del destino.
static long proc_signal_call(struct ctx* c) {
	int call = (int)c->a[0];
	int pid = (int)c->a[1];
	if (call != 7) {                                                       // 7 = TERMINATE: el pid va en la llamada
		uint32_t tok[16] = { 0 };
		size_t sz = c->a[5] > sizeof tok ? sizeof tok : c->a[5];
		if (sz < 32 || safe_read(c->a[4], tok, sz) != (ssize_t)sz) return -DARWIN_EINVAL;
		pid = (int)((call >= 0x13 && sz >= 64) ? tok[8 + 5] : tok[5]);
	}
	int terminate = call == 7 || call == 0x12 || call == 0x14;
	int sig = terminate ? 15 /* SIGTERM */ : (int)c->a[2];
	if (pid <= 0) return -3;                                               // ESRCH
	int ls = sig ? darwin_sig_to_linux(sig) : 0;
	if (sig && ls <= 0) return -DARWIN_EINVAL;
	int r = kill(pid, ls);
	logf_("    proc_info 0x%x: señal %d -> pid %d%s\n", call, sig, pid, r ? " (falló)" : "");
	if (r != 0) return -darwin_errno(errno);
	return terminate ? sig : 0;                                            // proc_terminate devuelve la señal enviada
}
// proc_listpids (PROC_INFO_CALL_LISTPIDS): pids de los procesos de este arranque, filtrados por tipo (todos, grupo, uid, uid
// real, padre). Sin búfer devuelve el tamaño necesario. Sin ella, MacBuddy (Asistente de configuración) no podía mirar qué
// procesos hay, caía en sus comprobaciones antiguas y decidía «configuración inicial» aunque el sistema ya estuviera
// configurado: loginwindow intentaba lanzar el Asistente en vez de mostrar la ventana de inicio de sesión.
static long proc_listpids(struct ctx* c) {
	int type = (int)c->a[1]; uint32_t info = (uint32_t)c->a[2];
	uint64_t buf = c->a[4]; size_t cap = (size_t)c->a[5] / 4;
	DIR* d = opendir("/proc"); struct dirent* de;
	size_t n = 0, total = 0;
	while (d && (de = readdir(d))) {
		int pid = atoi(de->d_name);
		if (pid <= 0) continue;
		struct pinfo pi;
		if (proc_snapshot(pid, &pi) != 0) continue;
		int keep = type == 1 || (type == 2 && (uint32_t)pi.pgid == info) || (type == 4 && (uint32_t)pi.uid == info) ||
		           (type == 5 && (uint32_t)pi.ruid == info) || (type == 6 && (uint32_t)pi.ppid == info);
		if (!keep) continue;
		total++;
		if (buf && n < cap) { int32_t v = pid; if (safe_write(buf + n * 4, &v, 4) != 4) { closedir(d); return -D_EFAULT; } n++; }
	}
	if (d) closedir(d);
	if (!buf || !cap) return (long)((total + 16) * 4);     // como XNU: una estimación con margen
	return (long)(n * 4);
}
static long bsd_proc_info(struct ctx* c) {
	if ((int)c->a[0] == 15) return 0;
	if ((int)c->a[0] == 1) return proc_listpids(c);
	if ((int)c->a[0] == 7 || ((int)c->a[0] >= 0x11 && (int)c->a[0] <= 0x14)) return proc_signal_call(c);
	if ((int)c->a[0] == 5 || (int)c->a[0] == 8) return 0;                 // SETCONTROL / DIRTYCONTROL: sin efecto
	if ((int)c->a[0] == 9) {                                              // PIDRUSAGE: contadores a cero
		uint8_t z[512] = { 0 };
		size_t n = c->a[5] < sizeof z ? c->a[5] : sizeof z;
		return safe_write(c->a[4], z, n) == (ssize_t)n ? 0 : -D_EFAULT;
	}
	if ((int)c->a[0] == 2 && ((int)c->a[2] == 3 || (int)c->a[2] == 4 || (int)c->a[2] == 13)) {   // BSDINFO (136) / TASKINFO (96) / SHORTBSDINFO (64)
		uint8_t b[136] = { 0 };
		size_t want = (int)c->a[2] == 3 ? 136 : (int)c->a[2] == 13 ? 64 : 96;
		struct pinfo pi;
		if (proc_snapshot((int)c->a[1], &pi) != 0) return -3;   // ESRCH
		if ((int)c->a[2] == 3) put_bsdinfo(b, &pi);
		else if ((int)c->a[2] == 13) put_bsdshortinfo(b, &pi);
		if (c->a[5] < want) return -DARWIN_EINVAL;
		return safe_write(c->a[4], b, want) == (ssize_t)want ? (long)want : -D_EFAULT;
	}
	if ((int)c->a[0] == 2 && ((int)c->a[2] == 17 || (int)c->a[2] == 18)) {   // PIDUNIQIDENTIFIERINFO / BSDINFOWITHUNIQID
		uint8_t b[136 + 56] = { 0 };
		struct pinfo pi;
		if (proc_snapshot((int)c->a[1], &pi) != 0) return -3;   // ESRCH
		put_bsdinfo(b, &pi);
		uint64_t uniq = (uint64_t)pi.pid;
		size_t off = (int)c->a[2] == 18 ? 136 : 0, size = off + 56;
		exe_uuid(pi.pid, b + 136);                          // p_uuid: el LC_UUID del ejecutable
		memcpy(b + 136 + 16, &uniq, 8);                     // p_uniqueid
		if ((int)c->a[2] == 17) memcpy(b + 16, &uniq, 8);   // (en este sabor la estructura empieza en p_uuid)
		const uint8_t* src = (int)c->a[2] == 18 ? b : b + 136;
		if (c->a[5] < size) return -DARWIN_EINVAL;
		if (safe_write(c->a[4], src, size) != (ssize_t)size) return -D_EFAULT;
		return (long)size;
	}
	if ((int)c->a[0] == 2 && (int)c->a[2] == 11) {                         // PIDPATHINFO (proc_pidpath): ruta del ejecutable en macOS
		char hp[4096];
		if (exe_host_path((int)c->a[1], hp, sizeof hp) != 0) return -3;   // ESRCH
		size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
		const char* g = (rl && !strncmp(hp, tahoe_root, rl) && hp[rl] == '/') ? hp + rl : hp;
		size_t len = strlen(g) + 1;
		if (c->a[5] < 1024 /* PROC_PIDPATHINFO_SIZE */ || c->a[5] < len) return -DARWIN_EINVAL;
		return safe_write(c->a[4], g, len) == (ssize_t)len ? (long)len : -D_EFAULT;
	}
	if ((int)c->a[0] == 2 && (int)c->a[2] == 20) {                         // PIDCOALITIONINFO: una coalición por tipo (la 1)
		uint64_t b[5] = { 1, 1, 0, 0, 0 };                                  // coalition_id[RESOURCE, JETSAM], reservados
		if (c->a[5] < sizeof b) return -DARWIN_EINVAL;
		return safe_write(c->a[4], b, sizeof b) == (ssize_t)sizeof b ? (long)sizeof b : -D_EFAULT;
	}
	return -DARWIN_EINVAL;
}
static long bsd_thread_selfid(struct ctx* c) { (void)c; return syscall(SYS_gettid); }
static long bsd_shared_region_check_np(struct ctx* c) {
	// la caché ya está mapeada en su dirección preferida: devolvemos dónde empieza
	uint64_t base = CACHE_BASE;
	return safe_write(c->a[0], &base, 8) == 8 ? 0 : -14;      // EFAULT si el puntero no es válido
}

static bsd_fn bsd_table[BSD_NAMES_N] = {
	[1] = bsd_exit, [3] = bsd_read, [4] = bsd_write, [6] = bsd_close, [20] = bsd_getpid,
	[294] = bsd_shared_region_check_np, [327] = bsd_issetugid, [372] = bsd_thread_selfid, [483] = bsd_csrctl, [454] = bsd_system_override, [336] = bsd_proc_info,
	[5] = bsd_open, [216] = bsd_open_dprotected, [486] = bsd_guarded_pwrite, [545] = bsd_proc_info_ext, [49] = bsd_getlogin, [494] = bsd_enotsup, [55] = bsd_reboot, [441] = bsd_guarded_open, [442] = bsd_guarded_close, [48] = bsd_sigprocmask, [500] = bsd_getentropy, [520] = bsd_terminate_with_payload, [521] = bsd_abort_with_payload, [529] = bsd_os_fault_with_payload,
};


// ---------------------------------------------------------------- trampas Mach (primera versión)
// Los nombres de puerto son solo identificadores: lo que importa es que sean estables y distintos de cero.
#define PORT_TASK_SELF   0x103
#define PORT_THREAD_SELF 0x203
#define PORT_HOST_SELF   0x303
static uint32_t next_port = 0x1103;
enum { KERN_SUCCESS_ = 0, KERN_FAILURE_ = 5 };

typedef emu_fn mach_fn;
static long mach_task_self_trap(struct ctx* c)   { (void)c; return PORT_TASK_SELF; }
__thread uint32_t g_thread_port = PORT_THREAD_SELF;   // nombre del puerto del hilo actual (único por hilo)
static long mach_thread_self_trap(struct ctx* c) { (void)c; return g_thread_port; }
static long mach_host_self_trap(struct ctx* c)   { (void)c; return PORT_HOST_SELF; }
static long mach_reply_port_trap(struct ctx* c)  { (void)c; uint32_t p = next_port; next_port += 0x100; return p; }
static long mach_vm_protect_trap(struct ctx* c) {
	// (target, address, size, set_maximum, new_protection)
	if (c->a[3]) return KERN_SUCCESS_;                   // cambiar la protección máxima: sin efecto en Linux
	return mprotect((void*)c->a[1], round_up(c->a[2]), (int)c->a[4] & 7) == 0 ? KERN_SUCCESS_ : 2 /*KERN_PROTECTION_FAILURE*/;
}
// escritura segura de un valor de 64 bits en la memoria del programa
static int safe_write64(uint64_t addr, uint64_t v) { return safe_write(addr, &v, 8) == 8 ? 0 : -1; }
enum { KERN_INVALID_ADDRESS_ = 1, KERN_NO_SPACE_ = 3, KERN_INVALID_ARGUMENT_ = 4 };
#define VM_FLAGS_ANYWHERE  0x0001
#define VM_FLAGS_OVERWRITE 0x4000

// Reserva memoria anónima como mach_vm_allocate / mach_vm_map: *addr es entrada (si FIXED) y salida.
static long vm_alloc(uint64_t addr_ptr, uint64_t size, int flags, uint64_t mask, int prot) {
	uint64_t want = 0;
	if (safe_read(addr_ptr, &want, 8) != 8) return KERN_INVALID_ADDRESS_;
	if (!size) return KERN_INVALID_ARGUMENT_;
	size = round_up(size);
	void* p;
	if (flags & VM_FLAGS_ANYWHERE) {
		if (mask > PAGE - 1) {                            // alineación mayor que una página: reservar de más y recortar
			uint64_t align = mask + 1, total = size + align;
			uint8_t* q = mmap(NULL, total, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
			if (q == MAP_FAILED) return KERN_NO_SPACE_;
			uint64_t a = ((uint64_t)q + mask) & ~mask;
			if (a > (uint64_t)q) munmap(q, a - (uint64_t)q);
			if ((uint64_t)q + total > a + size) munmap((void*)(a + size), (uint64_t)q + total - (a + size));
			p = (void*)a;
		} else {
			p = mmap(NULL, size, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		}
	} else {
		int extra = (flags & VM_FLAGS_OVERWRITE) ? MAP_FIXED : MAP_FIXED_NOREPLACE;
		p = mmap((void*)want, size, prot, MAP_PRIVATE | MAP_ANONYMOUS | extra, -1, 0);
	}
	if (trace_all) logf_("    vm_alloc <%d> (size=0x%lx mask=0x%lx flags=0x%x prot=%d) -> %p\n", (int)getpid(), size, mask, flags, prot, p);
	if (p != MAP_FAILED && mask == 0x7fffff && size == 0x2000000 && (flags & VM_FLAGS_ANYWHERE)) {
		// Montón de continuaciones de libdispatch: elige el segmento de 8 MiB con el "número de CPU" que lee de SIDT. En macOS ese
		// número va en el LÍMITE de la IDT; en Linux el límite es siempre 0xfff, así que siempre usa el segmento 0xfff
		// (a base + 0xfff*8 MiB). Se reserva ese segmento, relleno de ceros y sin coste hasta que se toca.
		void* seg = mmap((uint8_t*)p + (0xfffULL << 23), 1ULL << 23, PROT_READ | PROT_WRITE,
		                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE | MAP_NORESERVE, -1, 0);
		if (seg == MAP_FAILED && trace_all) logf_("    vm_alloc: no se pudo reservar el segmento SIDT (%s)\n", strerror(errno));
	}
	if (p == MAP_FAILED) return KERN_NO_SPACE_;
	return safe_write64(addr_ptr, (uint64_t)p) == 0 ? KERN_SUCCESS_ : KERN_INVALID_ADDRESS_;
}
static long mach_vm_allocate_trap(struct ctx* c)   { return vm_alloc(c->a[1], c->a[2], (int)c->a[3], 0, PROT_READ | PROT_WRITE); }
static long mach_vm_map_trap(struct ctx* c)        { return vm_alloc(c->a[1], c->a[2], (int)c->a[4], c->a[3], (int)c->a[5] & 7); }
static long mach_vm_deallocate_trap(struct ctx* c) { return munmap((void*)c->a[1], round_up(c->a[2])) == 0 ? KERN_SUCCESS_ : KERN_INVALID_ADDRESS_; }
static long mach_nop_success(struct ctx* c)        { (void)c; return KERN_SUCCESS_; }
static long mach_not_supported(struct ctx* c)      { (void)c; return 46; /* KERN_NOT_SUPPORTED */ }
static long mach_timebase_info_trap(struct ctx* c) {
	uint32_t tb[2] = { 1, 1 };                         // el contador ya está en nanosegundos
	return safe_write(c->a[0], tb, 8) == 8 ? KERN_SUCCESS_ : KERN_INVALID_ADDRESS_;
}
// _kernelrpc_mach_vm_purgable_control_trap(tarea, dirección, control, *estado): la memoria no es purgable aquí; el
// estado es siempre VM_PURGABLE_NONVOLATILE (0), tanto al consultar como el anterior al cambiarlo.
static long mach_vm_purgable_trap(struct ctx* c) {
	int zero = 0;
	if (c->a[3] && safe_write(c->a[3], &zero, 4) != 4) return 4;   // KERN_INVALID_ARGUMENT
	return 0;
}
static mach_fn mach_table[MACH_NAMES_N] = {
	[10] = mach_vm_allocate_trap, [12] = mach_vm_deallocate_trap, [15] = mach_vm_map_trap,
	[18] = mach_nop_success, [88] = mach_not_supported, [19] = mach_nop_success, [21] = mach_nop_success, [24] = mach_nop_success, [25] = mach_nop_success,
	[89] = mach_timebase_info_trap,
	// macx_triggers(alto, bajo, flags, puerto): launchd activa al arrancar la compactación del compresor de memoria y el
	// intercambio (SWAP_COMPACT_ENABLE 0x20) y comprueba el resultado. La memoria la gestiona Linux: éxito sin efecto.
	[51] = mach_nop_success,
	[14] = mach_vm_protect_trap, [11] = mach_vm_purgable_trap,
	[26] = mach_reply_port_trap, [27] = mach_thread_self_trap, [28] = mach_task_self_trap, [29] = mach_host_self_trap,
};

// Los argumentos 7 y 8 de una syscall van en la pila del programa: [rsp] es la dirección de retorno.
uint64_t ctx_arg(struct ctx* c, int i) {
	if (i < 6) return c->a[i];
	uint64_t v = 0;
	safe_read((uint64_t)c->uc->uc_mcontext.gregs[REG_RSP] + 8 * (i - 5), &v, 8);
	return v;
}

void reg_bsd(unsigned num, emu_fn fn)  { if (num < BSD_NAMES_N) bsd_table[num] = fn; }
void reg_mach(unsigned num, emu_fn fn) { if (num < MACH_NAMES_N) mach_table[num] = fn; }

// Un selector por hilo: cada hilo de macOS es un hilo de Linux y el despacho se activa por hilo.
static __thread char selector = SYSCALL_DISPATCH_FILTER_ALLOW;
char tahoe_selector_get(void) { return selector; }
void tahoe_selector_set(char v) { selector = v; }

static void log_call(const char* kind, uint32_t num, const char* name, struct ctx* c, const char* verdict) {
	logf_("  [%s] <%d:%d> %s %u %s(0x%lx, 0x%lx, 0x%lx, 0x%lx)\n", verdict, getpid(), (int)(syscall(SYS_gettid) - getpid()), kind, num, name ? name : "?", c->a[0], c->a[1], c->a[2], c->a[3]);
}

// Diagnóstico: con /dev/shm/tahoe-watch-path (un texto), todos los procesos anotan las llamadas que crean, escriben, borran o
// renombran una ruta que lo contenga (para saber quién toca un archivo: el llavero del sistema se convertía en -db).
static void watch_path_call(unsigned num, struct ctx* c, long r) {
	static int on = -1; static char want[128];
	if (on < 0) { FILE* f = fopen("/dev/shm/tahoe-watch-path", "r"); on = 0; if (f) { if (fgets(want, sizeof want, f)) { want[strcspn(want, "\n")] = 0; on = want[0] != 0; } fclose(f); } }
	if (!on) return;
	int p1 = -1, p2 = -1;
	switch (num) {
	case 5: case 398: if (!(c->a[1] & 0x603)) return; p1 = 0; break;          // open/open_nocancel para escribir o crear
	case 463: if (!(c->a[2] & 0x603)) return; p1 = 1; break;                   // openat
	case 10: p1 = 0; break; case 472: p1 = 1; break;                           // unlink / unlinkat
	case 128: p1 = 0; p2 = 1; break; case 465: case 488: p1 = 1; p2 = 3; break; // rename / renameat / renameatx_np
	default: return;
	}
	char a[300] = "", b[300] = "";
	safe_string(c->a[p1], a, sizeof a); if (p2 >= 0) safe_string(c->a[p2], b, sizeof b);
	if (!strstr(a, want) && !strstr(b, want)) return;
	logf_("    VIGILA <%d:%ld> %s bsd %u \"%s\"%s%s%s -> %ld\n", (int)getpid(), (long)syscall(SYS_gettid), prog_name, num, a, p2 >= 0 ? " -> \"" : "", b, p2 >= 0 ? "\"" : "", r);
}
static void on_sigsys(int sig, siginfo_t* si, void* v) {
	(void)sig;
	selector = SYSCALL_DISPATCH_FILTER_ALLOW;   // dentro del manejador, las syscalls van directas a Linux
	uint64_t t_in = __builtin_ia32_rdtsc();
	ucontext_t* uc = v;
	greg_t* g = uc->uc_mcontext.gregs;
	if (si->si_code != SYS_USER_DISPATCH) DIE("SIGSYS inesperado (si_code=%d) en rip=0x%llx", si->si_code, (unsigned long long)g[REG_RIP]);
	struct ctx c = { .nr = g[REG_RAX], .a = { g[REG_RDI], g[REG_RSI], g[REG_RDX], g[REG_R10], g[REG_R8], g[REG_R9] }, .uc = uc };
	extern __thread struct ctx* volatile t_cur;
	t_cur = &c;                                                  // para el volcado de hilos (diag_dump_threads)
	uint32_t cls = c.nr >> 24, num = c.nr & 0xffffff;
	if (cls == 2) {                                              // syscall BSD
		const char* name = num < BSD_NAMES_N ? bsd_names[num] : NULL;
		long r;
		const char* verdict;
		if (num < BSD_NAMES_N && bsd_table[num]) { r = bsd_table[num](&c); verdict = "ok   "; }
		else { r = -DARWIN_ENOSYS; verdict = "FALTA"; }
		if (trace_all || g_watch || verdict[0] == 'F') log_call("bsd", num, name, &c, verdict);
		else if (diag_target() && ((r < 0 && r > -4096 && !c.raw_ret) || diag_all())) {   // diagnóstico: errores (o todo) del proceso indicado
			char v[24]; if (r < 0 && r > -4096 && !c.raw_ret) snprintf(v, sizeof v, "E%-4ld", -r); else snprintf(v, sizeof v, "=%lx", r);
			log_call("bsd", num, name, &c, v);
			int pa = -1;                                         // argumento con ruta: open, access, readlink, stat, lstat, getattrlist...
			if (num == 5 || num == 398 || num == 33 || num == 58 || num == 338 || num == 340 || num == 220 || num == 136 || num == 10 || num == 128 || num == 345) pa = 0;
			else if (num == 463 || num == 464 || num == 469 || num == 470 || num == 466) pa = 1;
			if (pa >= 0) { char path[200] = ""; if (safe_read(c.a[pa], path, sizeof path - 1) > 0) { path[199] = 0; logf_("        ruta: \"%s\"\n", path); } }
		}
		watch_path_call(num, &c, r);
		if (c.raw_ret) { g[REG_RAX] = (uint64_t)r; g[REG_EFL] &= ~1UL; }
		else if (r < 0 && r > -4096) { g[REG_RAX] = -r; g[REG_EFL] |= 1; }   // error: rax = errno, CF = 1
		else { g[REG_RAX] = r; g[REG_EFL] &= ~1UL; if (c.has_ret2) g[REG_RDX] = c.ret2; }
	} else if (cls == 1) {                                       // trampa Mach
		const char* name = num < MACH_NAMES_N ? mach_names[num] : NULL;
		if (num < MACH_NAMES_N && mach_table[num]) {
			g[REG_RAX] = mach_table[num](&c);
			if (trace_all || g_watch) log_call("mach", num, name, &c, "ok   ");
			else if (diag_target() && diag_all()) { char v[24]; snprintf(v, sizeof v, "=%llx", (unsigned long long)g[REG_RAX]); log_call("mach", num, name, &c, v); }
		} else {
			log_call("mach", num, name, &c, "FALTA");
			g[REG_RAX] = KERN_FAILURE_;
		}
	} else if (cls == 3) {                                       // machdep
		if (num == 3) {                                          // thread_fast_set_cthread_self: base de GS
			syscall(SYS_arch_prctl, 0x1001 /*ARCH_SET_GS*/, c.a[0]);
			g[REG_RAX] = 0;
			if (trace_all) log_call("machdep", num, "thread_fast_set_cthread_self", &c, "ok   ");
		} else {
			log_call("machdep", num, NULL, &c, "FALTA");
			g[REG_RAX] = 0;
		}
	} else {
		log_call("clase", (uint32_t)c.nr, NULL, &c, "FALTA");
		g[REG_RAX] = (uint64_t)-1;
	}
	t_cur = NULL;
	__atomic_add_fetch(&sc_count[cls & 3], 1, __ATOMIC_RELAXED);
	__atomic_add_fetch(&sc_cycles, __builtin_ia32_rdtsc() - t_in, __ATOMIC_RELAXED);
	selector = SYSCALL_DISPATCH_FILTER_BLOCK;
}

// rt_sigreturn debe ejecutarse desde la región exenta: restaurador propio
__asm__(".text\n.global tahoe_restorer\n.type tahoe_restorer,@function\ntahoe_restorer:\n\tmovq $15, %rax\n\tsyscall\n\t.size tahoe_restorer, .-tahoe_restorer\n");
extern void tahoe_restorer(void);
void* tahoe_restorer_addr(void) { return (void*)tahoe_restorer; }

// Linux NO hereda el syscall user dispatch en el hijo de un fork: sin esto, las syscalls de macOS del hijo se
// ejecutarían como syscalls reales de Linux y devolverían ENOSYS sin pasar por el manejador.
void reenable_dispatch(void) {
	if (prctl(PR_SET_SYSCALL_USER_DISPATCH, PR_SYS_DISPATCH_ON, (unsigned long)tahoe_restorer, 16, &selector) != 0)
		DIE("hijo: no puedo reactivar PR_SET_SYSCALL_USER_DISPATCH: %s", strerror(errno));
}

// Prepara un hilo nuevo de Linux (pila alterna para SIGSYS + despacho) y salta al código de macOS con los
// registros que XNU deja a thread_start: rdi=pthread, rsi=puerto, rdx=func, rcx=arg, r8=pila, r9=flags.
extern __thread uint32_t g_thread_port;
static __thread void* alt_stack;
// El hilo del invitado terminó y su función del anfitrión va a volver: se quita el despacho de syscalls y la pila alterna.
// Sube el límite de descriptores del anfitrión si hace falta para la zona del emulador (un servicio de launchd hereda el que
// le fijó su plist, p. ej. 256 o 4096, con el que F_DUPFD por encima de FD_INTERNAL_BASE fallaría).
void fd_limit_raise(void) {
	struct rlimit rl;
	if (getrlimit(RLIMIT_NOFILE, &rl) != 0 || rl.rlim_cur >= FD_INTERNAL_LIMIT) return;
	if (rl.rlim_max != RLIM_INFINITY && rl.rlim_max < FD_INTERNAL_LIMIT) rl.rlim_max = FD_INTERNAL_LIMIT;   // root puede subirlo
	rl.rlim_cur = FD_INTERNAL_LIMIT;
	if (setrlimit(RLIMIT_NOFILE, &rl) != 0) { getrlimit(RLIMIT_NOFILE, &rl); rl.rlim_cur = rl.rlim_max; setrlimit(RLIMIT_NOFILE, &rl); }
}
int fd_hi(int fd) {
	if (fd < 0 || fd >= FD_INTERNAL_BASE) return fd;
	int fl = fcntl(fd, F_GETFD);
	int n = fcntl(fd, (fl >= 0 && (fl & FD_CLOEXEC)) ? F_DUPFD_CLOEXEC : F_DUPFD, FD_INTERNAL_BASE);
	if (n < 0) return fd;
	close(fd);
	return n;
}
static __thread struct { void** slot; size_t size; } tls_lazies[16];
static __thread int n_tls_lazies;
void* tls_lazy(void** slot, size_t size) {
	if (*slot) return *slot;
	void* p = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (p == MAP_FAILED) DIE("tls_lazy: mmap de %zu bytes: %s", size, strerror(errno));
	*slot = p;
	if (n_tls_lazies < 16) { tls_lazies[n_tls_lazies].slot = slot; tls_lazies[n_tls_lazies].size = size; n_tls_lazies++; }
	return p;
}
void guest_thread_done(void) {
	for (int i = 0; i < n_tls_lazies; i++) { munmap(*tls_lazies[i].slot, tls_lazies[i].size); *tls_lazies[i].slot = NULL; }
	n_tls_lazies = 0;
	selector = SYSCALL_DISPATCH_FILTER_ALLOW;
	prctl(PR_SET_SYSCALL_USER_DISPATCH, PR_SYS_DISPATCH_OFF, 0, 0, 0);
	if (alt_stack) { stack_t ss = { .ss_flags = SS_DISABLE }; sigaltstack(&ss, NULL); munmap(alt_stack, 1 << 18); alt_stack = NULL; }
}
void __attribute__((noreturn)) enter_guest_thread(uint64_t rip, uint64_t rsp, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
	g_thread_port = (uint32_t)a1;
	if (!alt_stack) {                                    // un hilo reutilizado vuelve a entrar: la pila alterna se crea una sola vez
		alt_stack = mmap(NULL, 1 << 18, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		stack_t ss = { .ss_sp = alt_stack, .ss_size = 1 << 18 };
		if (sigaltstack(&ss, NULL) != 0) DIE("hilo: sigaltstack: %s", strerror(errno));
	}
	reenable_dispatch();
	{ extern __thread struct ctx* volatile t_cur; t_cur = NULL; }   // hilo nuevo o reutilizado: sin llamada en curso
	selector = SYSCALL_DISPATCH_FILTER_BLOCK;
	register uint64_t r8 __asm__("r8") = a4, r9 __asm__("r9") = a5, r10 __asm__("r10") = rsp;
	register uint64_t rax __asm__("rax") = rip;
	__asm__ volatile(
		"movq %%r10, %%rsp\n\t"
		"xorl %%ebx, %%ebx\n\txorl %%ebp, %%ebp\n\txorl %%r10d, %%r10d\n\txorl %%r11d, %%r11d\n\t"
		"xorl %%r12d, %%r12d\n\txorl %%r13d, %%r13d\n\txorl %%r14d, %%r14d\n\txorl %%r15d, %%r15d\n\t"
		"jmpq *%%rax\n\t" :: "D"(a0), "S"(a1), "d"(a2), "c"(a3), "r"(r8), "r"(r9), "r"(r10), "r"(rax) : "memory");
	__builtin_unreachable();
}

// SIGILL en código de macOS suele ser un ud2 de libsystem tras un error fatal: se vuelca el motivo y la pila.
static void on_sigill(int sig, siginfo_t* si, void* v) {
	(void)sig; (void)si;
	selector = SYSCALL_DISPATCH_FILTER_ALLOW;
	{
		const uint8_t* ip = (const uint8_t*)((ucontext_t*)v)->uc_mcontext.gregs[REG_RIP]; uint8_t b[4] = { 0 };
		safe_read((uint64_t)ip, b, 4);
		logf_("    SIGILL <%d> hilo %ld rip=%p (%02x %02x %02x %02x)\n", getpid(), (long)syscall(SYS_gettid), (void*)ip, b[0], b[1], b[2], b[3]);
	}
	diag_crash(v);
	diag_dump_threads();
	_exit(132);
}

// SIDT/SGDT: con UMIP la CPU las impide en espacio de usuario (#GP -> SIGSEGV). libdispatch usa SIDT como "número de CPU":
// en macOS los 12 bits bajos de la base de la IDT son el índice de CPU, y con él elige un segmento de su montón. Se emula
// devolviendo límite 0xfff y una base con el número de CPU actual (módulo 4: el montón reserva 4 segmentos de 8 MiB).
static int emulate_sidt(ucontext_t* uc) {
	greg_t* g = uc->uc_mcontext.gregs;
	const uint8_t* ip = (const uint8_t*)g[REG_RIP];
	size_t i = 0;
	while (ip[i] == 0x66 || ip[i] == 0x2e || ip[i] == 0x3e || ip[i] == 0x65 || ip[i] == 0x64) i++;       // prefijos
	if ((ip[i] & 0xf0) == 0x40) i++;                                                                        // REX
	if (ip[i] != 0x0f || ip[i + 1] != 0x01) return 0;
	uint8_t modrm = ip[i + 2];
	int reg = (modrm >> 3) & 7, mod = modrm >> 6, rm = modrm & 7;
	if ((reg != 0 && reg != 1) || mod == 3) return 0;                                                       // solo SGDT/SIDT con memoria
	size_t len = i + 3;
	uint64_t addr = 0;
	static const int regmap[16] = { REG_RAX, REG_RCX, REG_RDX, REG_RBX, REG_RSP, REG_RBP, REG_RSI, REG_RDI,
		REG_R8, REG_R9, REG_R10, REG_R11, REG_R12, REG_R13, REG_R14, REG_R15 };
	int rex = ((ip[i - 1] & 0xf0) == 0x40) ? ip[i - 1] : 0;
	if (rm == 4) {                                                                                          // SIB
		uint8_t sib = ip[len++];
		int base = (sib & 7) | ((rex & 1) ? 8 : 0), idx = ((sib >> 3) & 7) | ((rex & 2) ? 8 : 0), sc = sib >> 6;
		if (!(mod == 0 && (sib & 7) == 5)) addr += (uint64_t)g[regmap[base]];
		if (idx != 4) addr += (uint64_t)g[regmap[idx]] << sc;
		if (mod == 0 && (sib & 7) == 5) { int32_t d; memcpy(&d, ip + len, 4); len += 4; addr += (int64_t)d; }
	} else if (mod == 0 && rm == 5) {                                                                       // RIP relativo
		int32_t d; memcpy(&d, ip + len, 4); len += 4;
		addr = (uint64_t)g[REG_RIP] + len + (int64_t)d;
		mod = 3;                                                                                            // ya sumado: sin desplazamiento extra
	} else addr += (uint64_t)g[regmap[rm | ((rex & 1) ? 8 : 0)]];
	if (mod == 1) { int8_t d = (int8_t)ip[len++]; addr += (int64_t)d; }
	else if (mod == 2) { int32_t d; memcpy(&d, ip + len, 4); len += 4; addr += (int64_t)d; }
	int cpu = sched_getcpu();
	uint16_t limit = (uint16_t)(0x1000 + (cpu < 0 ? 0 : cpu) % 4);   // como XNU: 0x1000 + número de CPU
	uint64_t base = 0xffffff8000000000ULL | (uint64_t)((cpu < 0 ? 0 : cpu) % 4);
	memcpy((void*)addr, &limit, 2);
	memcpy((void*)(addr + 2), &base, 8);
	g[REG_RIP] += (greg_t)len;
	return 1;
}
static void on_sigsegv(int sig, siginfo_t* si, void* v) {
	selector = SYSCALL_DISPATCH_FILTER_ALLOW;
	if (si->si_code == 0x80 /*SI_KERNEL*/ && emulate_sidt(v)) { selector = SYSCALL_DISPATCH_FILTER_BLOCK; return; }
	if (rmap_fault((uint64_t)si->si_addr)) { selector = SYSCALL_DISPATCH_FILTER_BLOCK; return; }   // memoria que otro proceso mapeó aquí
	logf_("    %s <%d> hilo %ld dirección=%p rip=0x%llx\n", sig == SIGBUS ? "SIGBUS" : "SIGSEGV", getpid(), (long)syscall(SYS_gettid), si->si_addr, (unsigned long long)((ucontext_t*)v)->uc_mcontext.gregs[REG_RIP]);
	diag_crash(v);
	diag_dump_threads();
	_exit(139);
}

// Manejador del anfitrión que puede interrumpir código del invitado: su rt_sigreturn debe salir del restaurador propio
// (región exenta del despacho de syscalls); el de glibc acabaría interceptado por SIGSYS.
int tahoe_sigaction(int sig, void (*h)(int, siginfo_t*, void*)) {
	struct { void* h; unsigned long flags; void* restorer; unsigned long mask; } ks = {
		.h = (void*)h, .flags = SA_SIGINFO | SA_NODEFER | SA_ONSTACK | 0x04000000 /*SA_RESTORER*/, .restorer = tahoe_restorer, .mask = 0 };
	return (int)syscall(SYS_rt_sigaction, sig, &ks, NULL, 8);
}
static void install_dispatch(void) {
	tahoe_sigaction(SIGSEGV, on_sigsegv);                      // vuelve al invitado: restaurador propio (sidt, zona común)
	tahoe_sigaction(SIGBUS, on_sigsegv);                       // acceso fuera de un archivo proyectado: mismo diagnóstico
	struct sigaction sa = { .sa_sigaction = on_sigill, .sa_flags = SA_SIGINFO | SA_ONSTACK };
	sigaction(SIGILL, &sa, NULL);
	stack_t ss = { .ss_sp = mmap(NULL, 1 << 18, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), .ss_size = 1 << 18 };
	if (sigaltstack(&ss, NULL) != 0) DIE("sigaltstack: %s", strerror(errno));
	struct { void* h; unsigned long flags; void* restorer; unsigned long mask; } ks = {
		.h = on_sigsys, .flags = SA_SIGINFO | SA_NODEFER | SA_ONSTACK | 0x04000000 /*SA_RESTORER*/, .restorer = tahoe_restorer, .mask = 0 };
	if (syscall(SYS_rt_sigaction, SIGSYS, &ks, NULL, 8) != 0) DIE("rt_sigaction: %s", strerror(errno));
	if (prctl(PR_SET_SYSCALL_USER_DISPATCH, PR_SYS_DISPATCH_ON, (unsigned long)tahoe_restorer, 16, &selector) != 0)
		DIE("PR_SET_SYSCALL_USER_DISPATCH: %s (hace falta Linux >= 5.11)", strerror(errno));
}

// Número de CPU: libsystem_pthread, libsystem_malloc y libdispatch lo leen con "sidt [rsp-0x10]" (o, según cómo se compiló la
// caché, "sidt [rbp-0x10]", p. ej. en la del macOS completo) y se quedan con los
// 12 bits bajos del límite de la IDT, porque XNU pone en cada CPU límite = 0x1000 + número de CPU (mp_desc.c). En Linux
// el límite es 0xfff, así que salía la CPU 4095 y libdispatch indexaba su montón por CPU fuera de rango. Sin UMIP la
// instrucción no se puede atrapar, de modo que en nuestra copia privada de la caché se cambia por una de igual tamaño,
// "and dword [rsp-0x10], 0" / "and dword [rbp-0x10], 0": límite & 0xfff = 0, la CPU 0, válida siempre (solo cambia el
// reparto por CPU).
static void patch_cpu_number(void) {
	const uint64_t cache = 0x7ff800000000UL;
	uint32_t io = *(uint32_t*)(cache + 0x1c0), ic = *(uint32_t*)(cache + 0x1c4);
	static const char* libs[] = { "/libdispatch.dylib", "/libsystem_malloc.dylib", "/libsystem_pthread.dylib" };
	static const struct { uint8_t len, sidt[5], repl[5]; } pat[] = {
		{ 5, { 0x0f, 0x01, 0x4c, 0x24, 0xf0 }, { 0x83, 0x64, 0x24, 0xf0, 0x00 } },   // sidt [rsp-0x10]
		{ 4, { 0x0f, 0x01, 0x4d, 0xf0 },       { 0x83, 0x65, 0xf0, 0x00 } },         // sidt [rbp-0x10]
	};
	int patched = 0;
	for (uint32_t i = 0; i < ic && i < 10000; i++) {
		uint64_t mh = *(uint64_t*)(cache + io + i * 32ULL);
		const char* path = (const char*)(cache + *(uint32_t*)(cache + io + i * 32ULL + 24));
		size_t pl = strlen(path);
		int want = 0;
		for (size_t k = 0; k < sizeof libs / sizeof libs[0]; k++) { size_t ll = strlen(libs[k]); if (pl >= ll && !strcmp(path + pl - ll, libs[k])) want = 1; }
		if (!want || *(uint32_t*)mh != 0xfeedfacf) continue;
		uint64_t lc = mh + 32;
		for (uint32_t c = 0; c < *(uint32_t*)(mh + 16); c++) {
			struct seg64* sg = (struct seg64*)lc;
			if (sg->cmd == 0x19 && !strcmp(sg->segname, "__TEXT")) {
				uint8_t* a = (uint8_t*)sg->vmaddr; uint64_t n = sg->vmsize;
				for (uint64_t off = 0; off + 5 <= n; off++) {
					if (a[off] != 0x0f || a[off + 1] != 0x01) continue;
					int k = -1;
					for (int j = 0; j < (int)(sizeof pat / sizeof pat[0]); j++) if (!memcmp(a + off, pat[j].sidt, pat[j].len)) k = j;
					if (k < 0) continue;
					uint64_t pg = ((uint64_t)a + off) & ~(uint64_t)(PAGE - 1);
					if (mprotect((void*)pg, PAGE * 2, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) continue;
					memcpy(a + off, pat[k].repl, pat[k].len);
					mprotect((void*)pg, PAGE * 2, PROT_READ | PROT_EXEC);
					patched++;
				}
			}
			lc += ((struct lcmd*)lc)->cmdsize;
		}
	}
	if (trace_all || getpid() == 1) logf_("    número de CPU: %d sidt sustituidas\n", patched);
}

// ---------------------------------------------------------------- main
// El destino y la pila van por memoria global: ningún registro que se pone a cero puede contener el destino.
uint64_t g_entry_rip, g_entry_rsp;

static void __attribute__((noreturn)) enter(uint64_t rip, uint64_t rsp) {
	g_entry_rip = rip;
	g_entry_rsp = rsp;
	selector = SYSCALL_DISPATCH_FILTER_BLOCK;
	__asm__ volatile(
		"movq g_entry_rsp(%%rip), %%rsp\n\t"
		"xorl %%eax, %%eax\n\txorl %%ebx, %%ebx\n\txorl %%ecx, %%ecx\n\txorl %%edx, %%edx\n\t"
		"xorl %%esi, %%esi\n\txorl %%edi, %%edi\n\txorl %%ebp, %%ebp\n\t"
		"xorl %%r8d, %%r8d\n\txorl %%r9d, %%r9d\n\txorl %%r10d, %%r10d\n\txorl %%r11d, %%r11d\n\t"
		"xorl %%r12d, %%r12d\n\txorl %%r13d, %%r13d\n\txorl %%r14d, %%r14d\n\txorl %%r15d, %%r15d\n\t"
		"jmpq *g_entry_rip(%%rip)\n\t" ::: "memory");
	__builtin_unreachable();
}

int main(int argc, char** argv, char** envp) {
	if (argc < 4) {
		fprintf(stderr, "uso: %s <dyld_shared_cache_x86_64> <dyld> <programa> [args...]\n", argv[0]);
		return 2;
	}
	g_argv = argv;
	fd_limit_raise();                                   // antes de reservar descriptores en la zona del emulador
	{ int f = fcntl(2, F_DUPFD_CLOEXEC, FD_INTERNAL_BASE); if (f >= 0) logfd = f; }
	if (getenv("TAHOE_LOGFILE")) {          // los procesos hijos escriben su registro en un archivo común
		int f = open(getenv("TAHOE_LOGFILE"), O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
		if (f >= 0) { int g = fcntl(f, F_DUPFD_CLOEXEC, FD_INTERNAL_BASE); close(f); if (g >= 0) logfd = g; }
	}
	tahoe_root = getenv("TAHOE_ROOT");
	trace_all = getenv("TAHOE_TRACE") != NULL;
	const char *cache = argv[1], *dyld = argv[2], *exe = argv[3];
	g_cache_path = cache; g_dyld_path = dyld;
	static char self[4096];
	ssize_t sl = readlink("/proc/self/exe", self, sizeof self - 1);
	g_self_exe = sl > 0 ? (self[sl] = 0, self) : argv[0];
	// Fases del arranque de este proceso (diagnóstico de velocidad, con /dev/shm/tahoe-log-time): ms de cada una.
	struct timespec ft[8]; int nft = 0;
#define FASE() clock_gettime(CLOCK_MONOTONIC, &ft[nft++])
	FASE();
	if (getpid() == 1) sys_volumes_mount();                      // launchd: antes de que macOS mire sus volúmenes
	map_cache(cache); FASE();
	patch_cpu_number(); FASE();
	LOG("caché mapeada en 0x%lx\n", CACHE_BASE);
	setup_commpage(); FASE();
	struct image di, ei;
	load_macho(dyld, DYLD_SLIDE, &di);
	load_macho(exe, 0, &ei); FASE();
	diag_breakpoints();                                          // /dev/shm/tahoe-break en el proceso de /dev/shm/tahoe-stacks
	if (di.filetype != 7) DIE("%s no es un MH_DYLINKER", dyld);
	// execve relanza tahoe-run: TAHOE_ARGV0 permite que argv[0] del programa no sea su ruta. Se aplica después de mapear la
	// caché: si allí hay que relanzarse sin ASLR (g_argv), el programa a cargar tiene que seguir en argv[3].
	const char* argv0 = getenv("TAHOE_ARGV0");
	if (argv0) { argv[3] = (char*)argv0; unsetenv("TAHOE_ARGV0"); }
	char real[4096];
	if (!realpath(exe, real)) snprintf(real, sizeof real, "%s", exe);
	// el programa debe ver su ruta como macOS (/bin/ls), no la del host
	const char* guest = real;
	size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
	if (rl && !strncmp(real, tahoe_root, rl) && real[rl] == '/') guest = real + rl;
	// Arranque directo (la shell, launchd): argv[0] es la ruta del host; el programa la ve como en macOS (bash la usa en sus
	// mensajes y en $0). Tras un execve de macOS, TAHOE_ARGV0 ya trae el argv[0] que eligió quien lo lanzó.
	if (!argv0 && rl && !strncmp(argv[3], tahoe_root, rl) && argv[3][rl] == '/') argv[3] += rl;
	if (tahoe_root) {                                            // cwd inicial = "/" de macOS, salvo que ya estemos dentro (exec hereda el cwd)
		char cwd[4300]; size_t rl = strlen(tahoe_root);
		int inside = getcwd(cwd, sizeof cwd) && !strncmp(cwd, tahoe_root, rl) && (cwd[rl] == '/' || cwd[rl] == 0);
		if (!inside && chdir(tahoe_root) != 0) LOG("aviso: no puedo entrar en TAHOE_ROOT: %s\n", strerror(errno));
	}
	uint64_t sp = build_stack(ei.mh, argc - 3, argv + 3, envp, guest);
	memfd_init();
	emu_sysctl_init();
	emu_fileport_init();
	emu_fs_init();
	emu_mach_init();
	emu_proc_init();
	emu_kqueue_init();
	emu_net_init();
	emu_fs2_init();
	emu_port_init();
	emu_sem_init();
	emu_psynch_init();
	emu_cs_init();
	emu_cred_init();
	emu_fs_xattr_init();                                             // al final: sustituye los get/set*id provisionales
	// Las variantes *_nocancel de Darwin son iguales a las normales salvo por el punto de cancelación de hilos.
	static const struct { unsigned nocancel, normal; } alias[] = {
		// todas las variantes *_nocancel de syscalls.master (sin puntos de cancelación, mismo efecto)
		{ 395, 394 }, { 396, 3 }, { 397, 4 }, { 398, 5 }, { 399, 6 }, { 400, 7 }, { 401, 27 }, { 402, 28 }, { 403, 29 }, { 404, 30 },
		{ 405, 65 }, { 406, 92 }, { 407, 93 }, { 408, 95 }, { 409, 98 }, { 410, 111 }, { 411, 120 }, { 412, 121 }, { 413, 133 },
		{ 414, 153 }, { 415, 154 }, { 416, 173 }, { 417, 230 }, { 418, 260 }, { 419, 261 }, { 420, 271 }, { 421, 315 }, { 422, 330 },
		{ 423, 334 }, { 464, 463 }, { 542, 540 }, { 543, 541 },
	};
	for (size_t i = 0; i < sizeof alias / sizeof alias[0]; i++) if (bsd_table[alias[i].normal] && !bsd_table[alias[i].nocancel]) bsd_table[alias[i].nocancel] = bsd_table[alias[i].normal];
	install_dispatch(); FASE();
#define MS(a, b) ((ft[b].tv_sec - ft[a].tv_sec) * 1e3 + (ft[b].tv_nsec - ft[a].tv_nsec) / 1e6)
	if (access("/dev/shm/tahoe-log-time", F_OK) == 0)
		logf_("    arranque <%d>: caché %.1f ms, cpu %.1f, commpage %.1f, mach-o %.1f, resto %.1f, total %.1f\n", (int)getpid(),
		      MS(0, 1), MS(1, 2), MS(2, 3), MS(3, 4), MS(4, 5), MS(0, 5));
#undef MS
#undef FASE
	LOG("saltando a dyld (rip=0x%lx rsp=0x%lx)\n", di.entry, sp);
	enter(di.entry, sp);
}
