// Memoria, archivos e identidad: las syscalls BSD de macOS más directas, traducidas a Linux.
//
// Las constantes y el diseño de las estructuras vienen de los encabezados de XNU 12377
// (bsd/sys/mman.h, fcntl.h, stat.h) y se escriben aquí a mano: no se usa código de Darling.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>
#include <unistd.h>

#include <linux/falloc.h>
#include <dirent.h>
#include "tahoe.h"

static long err(void) { return -darwin_errno(errno); }
#define RET(r) do { long r_ = (r); return r_ < 0 ? err() : r_; } while (0)

// dev_t de Darwin: mayor en los 8 bits altos, menor en los 24 bajos. Las pseudoterminales de Linux (mayores 136-143) son las
// ttys de macOS (mayor 16); el resto se recorta a 8 bits.
uint32_t darwin_dev(unsigned ma, unsigned mi) {
	if (ma >= 136 && ma <= 143) { mi += (ma - 136) * 256; ma = 16; }
	return ((ma & 0xff) << 24) | (mi & 0xffffff);
}
// Dueños vistos desde macOS: los archivos del usuario de Linux que ejecuta tahoe-run son los del sistema (root/wheel),
// como en una instalación real; fuera de un espacio de nombres de usuario ese uid no es 0.
uint32_t guest_uid(uint32_t u) { return u == (uint32_t)getuid() ? 0 : u; }
uint32_t guest_gid(uint32_t g) { return g == (uint32_t)getgid() ? 0 : g; }
// Rutas que el invitado ve del anfitrión: solo los dispositivos (/dev). Todo lo demás vive bajo TAHOE_ROOT; si no existe
// ahí, no existe (antes se caía a la ruta del anfitrión y el invitado veía, p. ej., /usr/bin/ls de Linux, y podía crear
// archivos fuera de su raíz).
int tahoe_host_passthrough(const char* p) { return !strcmp(p, "/dev") || !strncmp(p, "/dev/", 5); }
// Terminales: en macOS son /dev/ttysNNN; en Linux, /dev/pts/N. ttyname() de macOS solo busca en /dev, así que se presentan
// con el nombre de macOS (y getdirentries de /dev las añade).
const char* tahoe_dev_alias(const char* p, char* buf, size_t cap) {
	unsigned n; char extra;
	if (sscanf(p, "/dev/ttys%u%c", &n, &extra) == 1) { snprintf(buf, cap, "/dev/pts/%u", n); return buf; }
	return p;
}
// Discos emulados: los nodos /dev/diskN[sM] (bloque) y /dev/rdiskN[sM] (caracteres) de los medios que anuncia IOKit
// (emu_iokit.inc, io_disks: BSD Major 1, Minor = posición en esta lista). /dev viene del anfitrión y no los tiene; aquí se
// presentan en stat/lstat y al listar /dev. No hay datos detrás (el volumen es una carpeta de Linux): abrirlos falla.
static const char* const emu_disks[] = { "disk0", "disk0s1", "disk1", "disk1s1" };
#define EMU_DISK_MAJOR 1
static int emu_disk_node(const char* p, int* raw) {
	if (strncmp(p, "/dev/", 5)) return -1;
	const char* n = p + 5;
	*raw = n[0] == 'r';
	if (*raw) n++;
	for (int i = 0; i < (int)(sizeof emu_disks / sizeof emu_disks[0]); i++) if (!strcmp(n, emu_disks[i])) return i;
	return -1;
}
static void emu_disk_stat(int minor_, int raw, struct stat* s) {
	struct stat d;
	memset(s, 0, sizeof *s);
	if (stat("/dev", &d) == 0) { s->st_dev = d.st_dev; s->st_atim = s->st_mtim = s->st_ctim = d.st_ctim; }
	s->st_mode = (raw ? S_IFCHR : S_IFBLK) | 0640;                // root:operator, como en macOS
	s->st_nlink = 1; s->st_uid = 0; s->st_gid = 5;
	s->st_rdev = makedev(EMU_DISK_MAJOR, minor_);
	s->st_ino = 0x7ffe0000 + minor_ * 2 + raw;
	s->st_blksize = 4096;
}

// dev_t de macOS para el st_dev de un archivo: lo que está en la raíz de macOS (TAHOE_ROOT, en un sistema de archivos del
// anfitrión) es del volumen /dev/disk1s1, como en un Mac (stat("/").st_dev = dev_t de disk1s1). Así se traduce una ruta a
// su disco (diskutil info /, devname). El resto (/dev, terminales...) conserva su dispositivo.
#define ROOT_VOLUME_MINOR 3                                           // disk1s1 en emu_disks
static uint32_t guest_dev(dev_t d) {
	static dev_t root; static int known;
	if (!known) { struct stat rs; known = 1; root = (tahoe_root && stat(tahoe_root, &rs) == 0) ? rs.st_dev : (dev_t)-1; }
	if (tahoe_root && d == root) return darwin_dev(EMU_DISK_MAJOR, ROOT_VOLUME_MINOR);
	return darwin_dev(major(d), minor(d));
}

// Resuelve una ruta absoluta del programa bajo TAHOE_ROOT (salvo /dev, del anfitrión).
static const char* resolve(const char* guest, char* buf, size_t cap) {
	if (tahoe_root && guest[0] == '/' && !tahoe_host_passthrough(guest)) { snprintf(buf, cap, "%s%s", tahoe_root, guest); return buf; }
	return tahoe_dev_alias(guest, buf, cap);                                                // /dev: siempre el del anfitrión (el de la raíz está vacío)
}

// ---------------------------------------------------------------- memoria
static long bsd_munmap(struct ctx* c) { RET(munmap((void*)c->a[0], c->a[1])); }
static long bsd_mprotect(struct ctx* c) { RET(mprotect((void*)c->a[0], c->a[1], (int)c->a[2] & 7)); }
static long bsd_madvise(struct ctx* c) { (void)c; return 0; }   // solo es una pista

static long bsd_mmap(struct ctx* c) {
	uint64_t addr = c->a[0], len = c->a[1], flags = c->a[3], off = c->a[5];
	int prot = (int)c->a[2] & 7, fd = (int)c->a[4], lf = 0;
	if (flags & 0x0001) lf |= MAP_SHARED;
	if (flags & 0x0002) lf |= MAP_PRIVATE;
	if (flags & 0x0010) lf |= MAP_FIXED;
	if (flags & 0x0040) lf |= MAP_NORESERVE;
	if (flags & 0x1000) { lf |= MAP_ANONYMOUS; fd = -1; }          // MAP_ANON; el "fd" puede llevar una etiqueta de VM
	void* p = mmap((void*)addr, len, prot, lf, fd, off);
	if (p == MAP_FAILED) return err();
	return (long)p;
}

// ---------------------------------------------------------------- ficheros
static long bsd_lseek(struct ctx* c) {
	int wh = (int)c->a[2];
	if (wh == 3) wh = SEEK_HOLE; else if (wh == 4) wh = SEEK_DATA;   // Darwin: HOLE=3, DATA=4 (en Linux al revés)
	RET(lseek((int)c->a[0], (off_t)c->a[1], wh));
}
static long bsd_pread(struct ctx* c) { RET(syscall(SYS_pread64, c->a[0], c->a[1], c->a[2], c->a[3])); }
static long bsd_pwrite(struct ctx* c) { RET(syscall(SYS_pwrite64, c->a[0], c->a[1], c->a[2], c->a[3])); }

static long bsd_access(struct ctx* c) {
	char p[4096], b[4200];
	if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT;
	int raw; if (emu_disk_node(p, &raw) >= 0) return ((int)c->a[1] & 1) ? -13 /* EACCES */ : 0;   // discos emulados: rw para root, no x
	RET(access(resolve(p, b, sizeof b), (int)c->a[1] & 7));          // R_OK/W_OK/X_OK/F_OK coinciden
}
// Destino de un enlace tal como lo ve macOS: los absolutos se guardan dentro de la raíz (el kernel de Linux los sigue), así
// que se les quita ese prefijo. Devuelve los bytes que caben en cap (readlink no termina en NUL).
static ssize_t guest_link_target(const char* host, char* out, size_t cap) {
	char t[4400];
	ssize_t n = readlink(host, t, sizeof t - 1);
	if (n < 0) return -1;
	t[n] = 0;
	const char* g = t;
	size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
	if (rl && !strncmp(t, tahoe_root, rl) && t[rl] == '/') g = t + rl;
	size_t gl = strlen(g); if (gl > cap) gl = cap;
	memcpy(out, g, gl);
	return (ssize_t)gl;
}
static long bsd_readlink(struct ctx* c) {
	char p[4096], b[4200], out[4096];
	if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT;
	ssize_t n = guest_link_target(resolve(p, b, sizeof b), out, c->a[2] < sizeof out ? c->a[2] : sizeof out);
	if (n < 0) return err();
	return safe_write(c->a[1], out, n) == n ? n : -D_EFAULT;
}
// readlinkat(dirfd, ruta, búfer, tamaño)
static long bsd_readlinkat(struct ctx* c) {
	char p[4096], b[4200], out[4096], h[4400];
	if (safe_string(c->a[1], p, sizeof p) != 0) return -D_EFAULT;
	const char* host;
	int dfd = (int)c->a[0] == -2 ? AT_FDCWD : (int)c->a[0];
	if (p[0] == '/' || dfd == AT_FDCWD) host = resolve(p, b, sizeof b);
	else { char lp[64]; snprintf(lp, sizeof lp, "/proc/self/fd/%d", dfd); char d[4096]; ssize_t dl = readlink(lp, d, sizeof d - 1);
		if (dl < 0) return err(); d[dl] = 0; snprintf(h, sizeof h, "%s/%s", d, p); host = h; }
	ssize_t n = guest_link_target(host, out, c->a[3] < sizeof out ? c->a[3] : sizeof out);
	if (n < 0) return err();
	return safe_write(c->a[2], out, n) == n ? n : -D_EFAULT;
}

// Banderas BSD (st_flags: SF_RESTRICTED, UF_HIDDEN...): Linux no las tiene; se guardan como atributo extendido del archivo.
// Hacen falta porque libsystem comprueba que rootless_mkdir_protected() dejó el directorio protegido.
#include <sys/xattr.h>
uint32_t tahoe_flags_fd(int fd) { uint32_t v = 0; if (fgetxattr(fd, "user.tahoe.flags", &v, 4) != 4) v = 0; return v; }
uint32_t tahoe_flags_path(const char* p) { uint32_t v = 0; if (lgetxattr(p, "user.tahoe.flags", &v, 4) != 4) v = 0; return v; }

// struct stat de Darwin con inodos de 64 bits (144 bytes)
static void put_stat(uint64_t out, const struct stat* s, uint32_t flags) {
	uint8_t d[144];
	memset(d, 0, sizeof d);
#define W(type, off, v) do { type t_ = (type)(v); memcpy(d + (off), &t_, sizeof t_); } while (0)
	W(int32_t, 0, guest_dev(s->st_dev));
	W(uint16_t, 4, s->st_mode);
	W(uint16_t, 6, s->st_nlink);
	W(uint64_t, 8, s->st_ino);
	W(uint32_t, 16, guest_uid(s->st_uid));
	W(uint32_t, 20, guest_gid(s->st_gid));
	W(int32_t, 24, darwin_dev(major(s->st_rdev), minor(s->st_rdev)));
	W(int64_t, 32, s->st_atim.tv_sec);  W(int64_t, 40, s->st_atim.tv_nsec);
	W(int64_t, 48, s->st_mtim.tv_sec);  W(int64_t, 56, s->st_mtim.tv_nsec);
	W(int64_t, 64, s->st_ctim.tv_sec);  W(int64_t, 72, s->st_ctim.tv_nsec);
	W(int64_t, 80, s->st_ctim.tv_sec);  W(int64_t, 88, s->st_ctim.tv_nsec);   // birthtime: se usa ctime
	W(int64_t, 96, s->st_size);
	W(int64_t, 104, s->st_blocks);
	W(int32_t, 112, s->st_blksize);
	W(uint32_t, 116, flags);
#undef W
	safe_write(out, d, sizeof d);
}
static long do_stat(struct ctx* c, int follow) {
	char p[4096], b[4200];
	struct stat s;
	if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT;
	int raw, dm = emu_disk_node(p, &raw);
	if (dm >= 0) { emu_disk_stat(dm, raw, &s); put_stat(c->a[1], &s, 0); return 0; }
	const char* path = resolve(p, b, sizeof b);
	if ((follow ? stat(path, &s) : lstat(path, &s)) != 0) {
		if (trace_all) logf_("    stat(\"%s\") -> %s\n", p, strerror(errno));
		return err();
	}
	put_stat(c->a[1], &s, follow ? tahoe_flags_path(path) : tahoe_flags_path(path));
	return 0;
}
static long bsd_stat64(struct ctx* c) { return do_stat(c, 1); }
static long bsd_lstat64(struct ctx* c) { return do_stat(c, 0); }
// stat64_extended / lstat64_extended / fstat64_extended: como stat, más la ACL (no hay: tamaño 0)
static long ext_tail(struct ctx* c, long r) {
	if (r == 0 && c->a[3]) { uint64_t z = 0; safe_write(c->a[3], &z, 8); }
	return r;
}
static long bsd_stat64_ext(struct ctx* c) { return ext_tail(c, do_stat(c, 1)); }
static long bsd_lstat64_ext(struct ctx* c) { return ext_tail(c, do_stat(c, 0)); }
static long bsd_fstat64(struct ctx* c);
static long bsd_fstat64_ext(struct ctx* c) { return ext_tail(c, bsd_fstat64(c)); }

// ---- atributos extendidos: los de macOS ("com.apple.quarantine"...) se guardan como user.<nombre> en Linux; los internos
// de tahoe-run (user.tahoe.*) no se muestran. options: XATTR_NOFOLLOW 0x1, XATTR_CREATE 0x2, XATTR_REPLACE 0x4.
#include <sys/xattr.h>
static int xname(uint64_t a, char* out, size_t cap) {
	char n[256]; if (safe_string(a, n, sizeof n) != 0) return -D_EFAULT;
	snprintf(out, cap, "user.%s", n); return 0;
}
static long xerr(void) { return errno == ENODATA ? -93 /* ENOATTR */ : errno == ENOTSUP ? -45 : err(); }
// getxattr(ruta, nombre, valor, tamaño, posición, opciones) / fgetxattr(fd, ...)
static long xget(struct ctx* c, int byfd) {
	char p[4096], b[4200], nm[300]; long e;
	if ((e = xname(c->a[1], nm, sizeof nm))) return e;
	size_t sz = c->a[3]; uint8_t* v = sz ? malloc(sz) : NULL;
	ssize_t r;
	if (byfd) r = fgetxattr((int)c->a[0], nm, v, sz);
	else { if (safe_string(c->a[0], p, sizeof p) != 0) { free(v); return -D_EFAULT; } const char* hp = resolve(p, b, sizeof b); r = (c->a[5] & 1) ? lgetxattr(hp, nm, v, sz) : getxattr(hp, nm, v, sz); }
	if (r < 0) { e = errno == ERANGE ? -34 : xerr(); free(v); return e; }
	if (sz && r > 0 && safe_write(c->a[2], v, (size_t)r) != r) r = -D_EFAULT;
	free(v); return r;
}
static long xset(struct ctx* c, int byfd) {
	char p[4096], b[4200], nm[300]; long e;
	if ((e = xname(c->a[1], nm, sizeof nm))) return e;
	size_t sz = c->a[3]; uint8_t* v = malloc(sz ? sz : 1);
	if (sz && safe_read(c->a[2], v, sz) != (ssize_t)sz) { free(v); return -D_EFAULT; }
	int fl = (c->a[5] & 2) ? XATTR_CREATE : (c->a[5] & 4) ? XATTR_REPLACE : 0;
	int r;
	if (byfd) r = fsetxattr((int)c->a[0], nm, v, sz, fl);
	else { if (safe_string(c->a[0], p, sizeof p) != 0) { free(v); return -D_EFAULT; } const char* hp = resolve(p, b, sizeof b); r = (c->a[5] & 1) ? lsetxattr(hp, nm, v, sz, fl) : setxattr(hp, nm, v, sz, fl); }
	free(v);
	return r < 0 ? xerr() : 0;
}
// removexattr(ruta, nombre, opciones)
static long xremove(struct ctx* c, int byfd) {
	char p[4096], b[4200], nm[300]; long e;
	if ((e = xname(c->a[1], nm, sizeof nm))) return e;
	int r;
	if (byfd) r = fremovexattr((int)c->a[0], nm);
	else { if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT; const char* hp = resolve(p, b, sizeof b); r = (c->a[2] & 1) ? lremovexattr(hp, nm) : removexattr(hp, nm); }
	return r < 0 ? xerr() : 0;
}
// listxattr(ruta, lista, tamaño, opciones): nombres separados por NUL, sin "user." ni los internos
static long xlist(struct ctx* c, int byfd) {
	char p[4096], b[4200], raw[8192], out[8192];
	ssize_t r;
	if (byfd) r = flistxattr((int)c->a[0], raw, sizeof raw);
	else { if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT; const char* hp = resolve(p, b, sizeof b); r = (c->a[3] & 1) ? llistxattr(hp, raw, sizeof raw) : listxattr(hp, raw, sizeof raw); }
	if (r < 0) return errno == ENOTSUP ? 0 : err();
	size_t o = 0;
	for (ssize_t i = 0; i < r; i += (ssize_t)strlen(raw + i) + 1) {
		const char* n = raw + i;
		if (strncmp(n, "user.", 5) || !strncmp(n, "user.tahoe.", 11)) continue;
		size_t l = strlen(n + 5) + 1;
		memcpy(out + o, n + 5, l); o += l;
	}
	if (!c->a[1] || !c->a[2]) return (long)o;
	if (c->a[2] < o) return -34;                                  // ERANGE
	return safe_write(c->a[1], out, o) == (ssize_t)o ? (long)o : -D_EFAULT;
}
static long bsd_getxattr(struct ctx* c) { return xget(c, 0); }   static long bsd_fgetxattr(struct ctx* c) { return xget(c, 1); }
static long bsd_setxattr(struct ctx* c) { return xset(c, 0); }   static long bsd_fsetxattr(struct ctx* c) { return xset(c, 1); }
static long bsd_removexattr(struct ctx* c) { return xremove(c, 0); }   static long bsd_fremovexattr(struct ctx* c) { return xremove(c, 1); }
static long bsd_listxattr(struct ctx* c) { return xlist(c, 0); }   static long bsd_flistxattr(struct ctx* c) { return xlist(c, 1); }

static long bsd_fstat64(struct ctx* c) {
	struct stat s;
	if (fstat((int)c->a[0], &s) != 0) return err();
	put_stat(c->a[1], &s, tahoe_flags_fd((int)c->a[0]));
	return 0;
}


static long bsd_dup(struct ctx* c)  { RET(dup((int)c->a[0])); }
static long bsd_dup2(struct ctx* c) { RET(dup2((int)c->a[0], (int)c->a[1])); }

// fstatat64(fd, ruta, búfer, flags): AT_FDCWD = -2 y AT_SYMLINK_NOFOLLOW = 0x20 en Darwin. AT_SYMLINK_NOFOLLOW_ANY (0x800,
// macOS 14) tampoco sigue los enlaces intermedios; aquí cuenta como el primero (el último componente no se sigue).
static long bsd_fstatat64(struct ctx* c) {
	char p[4096], b[4200];
	struct stat s;
	if (safe_string(c->a[1], p, sizeof p) != 0) return -D_EFAULT;
	int dfd = (int)c->a[0];
	if (dfd == -2) dfd = AT_FDCWD;
	int flags = ((int)c->a[3] & (0x20 | 0x800)) ? AT_SYMLINK_NOFOLLOW : 0;
	if (p[0] == '\0') flags |= AT_EMPTY_PATH;
	{                                                                // discos emulados: por ruta absoluta o relativa a /dev
		char full[4200]; int raw, dm = -1;
		if (p[0] == '/') dm = emu_disk_node(p, &raw);
		else if (p[0] && !strchr(p, '/')) {
			struct stat ds, dd;
			int in_dev = dfd == AT_FDCWD ? (getcwd(full, sizeof full) && !strcmp(full, "/dev"))
				: (fstat(dfd, &ds) == 0 && stat("/dev", &dd) == 0 && ds.st_ino == dd.st_ino && ds.st_dev == dd.st_dev);
			snprintf(full, sizeof full, "/dev/%s", p);
			if (in_dev) dm = emu_disk_node(full, &raw);
		}
		if (dm >= 0) { emu_disk_stat(dm, raw, &s); put_stat(c->a[2], &s, 0); return 0; }
	}
	const char* path = (dfd == AT_FDCWD || p[0] == '/') ? resolve(p, b, sizeof b) : p;
	if (fstatat(dfd, path, &s, flags) != 0) {
		if (trace_all) logf_("    fstatat(%d, \"%s\") -> %s\n", (int)c->a[0], p, strerror(errno));
		return err();
	}
	uint32_t fl = 0;
	if (dfd == AT_FDCWD || p[0] == '/') fl = tahoe_flags_path(path);
	else if (p[0] == '\0') fl = tahoe_flags_fd(dfd);
	else { int tf = openat(dfd, path, O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC); if (tf >= 0) { fl = tahoe_flags_fd(tf); close(tf); } }
	put_stat(c->a[2], &s, fl);
	return 0;
}

// fcntl de Darwin: solo los comandos que usa el arranque
static long bsd_fcntl(struct ctx* c) {
	int fd = (int)c->a[0], cmd = (int)c->a[1];
	switch (cmd) {
	case 0:  RET(fcntl(fd, F_DUPFD, (int)c->a[2]));
	case 1:  RET(fcntl(fd, F_GETFD));
	case 2:  RET(fcntl(fd, F_SETFD, (int)c->a[2]));
	case 67: RET(fcntl(fd, F_DUPFD_CLOEXEC, (int)c->a[2]));
	case 3: { int f = fcntl(fd, F_GETFL); if (f < 0) return err(); return (f & 3) | ((f & O_NONBLOCK) ? 4 : 0) | ((f & O_APPEND) ? 8 : 0); }
	case 4: { int f = (int)c->a[2], lf = 0; if (f & 4) lf |= O_NONBLOCK; if (f & 8) lf |= O_APPEND; RET(fcntl(fd, F_SETFL, lf)); }
	case 45: case 48: case 51: return 0;                  // F_RDAHEAD, F_NOCACHE, F_FULLFSYNC: sin efecto
	case 50: {                                            // F_GETPATH: ruta del descriptor
		char link[64], path[4096];
		snprintf(link, sizeof link, "/proc/thread-self/fd/%d", fd);
		ssize_t n = readlink(link, path, sizeof path - 1);
		if (n < 0) { if (trace_all) logf_("    F_GETPATH(fd=%d) fallo: %s\n", fd, strerror(errno)); return err(); }
		path[n] = 0;
		const char* p = path;
		size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
		if (rl && !strncmp(path, tahoe_root, rl) && (path[rl] == '/' || path[rl] == '\0')) p = path[rl] ? path + rl : "/";    // quitar la raíz de macOS
		if (trace_all) logf_("    F_GETPATH(fd=%d) = \"%s\"\n", fd, p);
		return safe_write(c->a[2], p, strlen(p) + 1) > 0 ? 0 : -D_EFAULT;
	}
	case 102: { struct ctx c2 = *c; c2.a[1] = 50; return bsd_fcntl(&c2); }   // F_GETPATH_NOFIRMLINK: sin firmlinks aquí
	case 7: case 8: case 9: case 90: case 91: case 92: case 93: {   // F_GETLK/SETLK/SETLKW y sus versiones OFD (por descripción abierta)
		// struct flock de Darwin: l_start, l_len (off_t), l_pid, l_type, l_whence (short). La de Linux tiene otro orden.
		uint8_t df[24];
		if (safe_read(c->a[2], df, 24) != 24) return -D_EFAULT;
		struct flock lf = { 0 };
		int16_t dt, dw; memcpy(&lf.l_start, df, 8); memcpy(&lf.l_len, df + 8, 8); memcpy(&dt, df + 20, 2); memcpy(&dw, df + 22, 2);
		lf.l_type = dt == 1 ? F_RDLCK : dt == 3 ? F_WRLCK : F_UNLCK;   // Darwin: F_RDLCK 1, F_UNLCK 2, F_WRLCK 3
		lf.l_whence = dw;
		int lc = cmd == 7 ? F_GETLK : cmd == 8 ? F_SETLK : cmd == 9 ? F_SETLKW : cmd == 92 ? F_OFD_GETLK : cmd == 90 ? F_OFD_SETLK : F_OFD_SETLKW;
		if (fcntl(fd, lc, &lf) != 0) return err();
		if (cmd == 7 || cmd == 92) {                       // devolver el cerrojo que estorba (o F_UNLCK)
			int16_t ot = lf.l_type == F_RDLCK ? 1 : lf.l_type == F_WRLCK ? 3 : 2, ow = (int16_t)lf.l_whence;
			int32_t opid = lf.l_pid;
			memcpy(df, &lf.l_start, 8); memcpy(df + 8, &lf.l_len, 8); memcpy(df + 16, &opid, 4); memcpy(df + 20, &ot, 2); memcpy(df + 22, &ow, 2);
			if (safe_write(c->a[2], df, 24) != 24) return -D_EFAULT;
		}
		return 0;
	}
	case 61: case 83: case 103: case 104: case 113: return 0;   // F_ADDFILESIGS*: no hay validación de firmas que hacer
	case 97: {                                              // F_ADDFILESIGS_RETURN: la firma "cubre" todo el archivo
		struct stat st;
		if (fstat(fd, &st) != 0) return err();
		int64_t end = st.st_size;
		return safe_write(c->a[2], &end, 8) == 8 ? 0 : -D_EFAULT;   // fs_file_start de salida: fin cubierto
	}
	case 98: case 62: case 76: case 100: case 101: return 0;   // CHECK_LV, NODIRECT, SINGLE_WRITER, TRIM_ACTIVE_FILE, SPECULATIVE_READ
	case 73: return 0;                                      // F_SETNOSIGPIPE: el emulador ya evita SIGPIPE en los envíos
	case 74: case 63: case 77: return 0;                    // F_GETNOSIGPIPE, F_GETPROTECTIONCLASS, F_GETPROTECTIONLEVEL: 0
	case 64: return 0;                                      // F_SETPROTECTIONCLASS: sin clases de protección
	case 85: RET(fdatasync(fd));                            // F_BARRIERFSYNC
	case 99: {                                              // F_PUNCHHOLE(fpunchhole_t: flags, reservado, offset, length)
		uint64_t ph[3]; uint8_t raw[24];
		if (safe_read(c->a[2], raw, 24) != 24) return -D_EFAULT;
		memcpy(&ph[1], raw + 8, 8); memcpy(&ph[2], raw + 16, 8);
		RET(fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, (off_t)ph[1], (off_t)ph[2]));
	}
	case 106: RET(fcntl(fd, F_SETLEASE, (int)c->a[2] == 1 ? F_RDLCK : (int)c->a[2] == 3 ? F_WRLCK : F_UNLCK));
	case 107: { int l = fcntl(fd, F_GETLEASE); if (l < 0) return err(); return l == F_RDLCK ? 1 : l == F_WRLCK ? 3 : 2; }
	default:
		logf_("    fcntl: comando %d (fd=%d) sin implementar\n", cmd, fd);
		return -D_EINVAL;
	}
}



// struct statfs de Darwin (64 bits de inodo, <sys/mount.h>): 2168 bytes
//   0 f_bsize  4 f_iosize  8 f_blocks  16 f_bfree  24 f_bavail  32 f_files  40 f_ffree  48 f_fsid[2]  56 f_owner
//   60 f_type  64 f_flags  68 f_fssubtype  72 f_fstypename[16]  88 f_mntonname[1024]  1112 f_mntfromname[1024]
//   2136 f_flags_ext  2140 f_reserved[7]
#include <sys/statfs.h>
// Banderas de montaje de XNU (sys/mount.h). MNT_UPDATE (0x10000) es una orden para mount(2), no un estado del volumen.
#define MNT_RDONLY      0x00000001
#define MNT_LOCAL       0x00001000
#define MNT_ROOTFS      0x00004000
#define MNT_DOVOLFS     0x00008000
#define MNT_UPDATE      0x00010000
#define MNT_DONTBROWSE  0x00100000
#define MNT_JOURNALED   0x00800000
#define MNT_MULTILABEL  0x04000000
// Sistemas de archivos que ve macOS: la raíz (APFS, el volumen del sistema) y devfs en /dev, como en un Mac. /dev es el
// del anfitrión (devtmpfs/devpts), así que lo que esté ahí se presenta como devfs.
struct gmount { const char* on; const char* from; const char* type; uint32_t ftype, flags, iosize; };
static const struct gmount gmounts[] = {
	{ "/",    "/dev/disk1s1", "apfs",  0x18, MNT_LOCAL | MNT_ROOTFS | MNT_DOVOLFS | MNT_JOURNALED | MNT_MULTILABEL, 1048576 },
	{ "/dev", "devfs",        "devfs", 0x13, MNT_LOCAL | MNT_DONTBROWSE | MNT_MULTILABEL, 512 },
};
#define NGMOUNT (sizeof gmounts / sizeof gmounts[0])
static int is_devfs_host(const struct statfs* s) { return s->f_type == 0x1373 /* devtmpfs */ || s->f_type == 0x1cd1 /* devpts */; }
static void put_statfs(uint64_t out, const struct statfs* s, const struct gmount* m) {
	uint8_t d[2168];
	memset(d, 0, sizeof d);
#define W(type, off, v) do { type t_ = (type)(v); memcpy(d + (off), &t_, sizeof t_); } while (0)
	W(uint32_t, 0, s->f_bsize);
	W(int32_t, 4, m->iosize);
	W(uint64_t, 8, s->f_blocks);
	W(uint64_t, 16, s->f_bfree);
	W(uint64_t, 24, s->f_bavail);
	W(uint64_t, 32, s->f_files);
	W(uint64_t, 40, s->f_ffree);
	{                                                                // f_fsid: {dev_t del dispositivo, tipo de vfs}, como en XNU
		int raw, dm = emu_disk_node(m->from, &raw);
		if (dm >= 0) { W(uint32_t, 48, darwin_dev(EMU_DISK_MAJOR, dm)); W(uint32_t, 52, m->ftype); }
		else { W(uint32_t, 48, s->f_fsid.__val[0]); W(uint32_t, 52, s->f_fsid.__val[1]); }
	}
	W(uint32_t, 60, m->ftype);
	W(uint32_t, 64, m->flags);
	strncpy((char*)d + 72, m->type, 15);
	strncpy((char*)d + 88, m->on, 1023);
	strncpy((char*)d + 1112, m->from, 1023);
#undef W
	safe_write(out, d, sizeof d);
}
// tmpfs montados por macOS (mount_tmpfs: la Recovery pone uno en /System/Volumes/Data, su /private, que empieza vacío en cada
// arranque). Son tmpfs de Linux de verdad en el espacio de montajes del arranque (unshare --mount), así que todos sus procesos
// los ven; se leen de /proc/self/mountinfo (los que estén dentro de la raíz). Número de vfs: el que XNU asigna al cargar
// tmpfs.kext no es fijo; se usa el siguiente libre tras los del núcleo.
#define LINUX_TMPFS_MAGIC 0x01021994
#define MNT_NOSUID 0x00000008
#define MNT_NODEV  0x00000010
struct dmount { char on[1024], host[1100]; struct gmount m; };
static int tmpfs_mounts(struct dmount* v, int max) {
	size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
	if (!rl) return 0;
	FILE* f = fopen("/proc/self/mountinfo", "r");
	if (!f) return 0;
	char ln[4096]; int n = 0;
	while (n < max && fgets(ln, sizeof ln, f)) {
		char mp[2048], fs[64]; const char* sep = strstr(ln, " - ");
		if (!sep || sscanf(sep + 3, "%63s", fs) != 1 || strcmp(fs, "tmpfs")) continue;
		if (sscanf(ln, "%*s %*s %*s %*s %2047s", mp) != 1) continue;
		char h[1100]; size_t k = 0;                                      // \040 y demás escapes octales de mountinfo
		for (const char* q = mp; *q && k < sizeof h - 1; q++) {
			if (q[0] == '\\' && q[1] >= '0' && q[1] <= '7' && q[2] && q[3]) { h[k++] = (char)((q[1] - '0') * 64 + (q[2] - '0') * 8 + (q[3] - '0')); q += 3; }
			else h[k++] = *q;
		}
		h[k] = 0;
		if (strncmp(h, tahoe_root, rl) || h[rl] != '/') continue;
		snprintf(v[n].host, sizeof v[n].host, "%s", h);
		snprintf(v[n].on, sizeof v[n].on, "%s", h + rl);
		v[n].m = (struct gmount){ v[n].on, "tmpfs", "tmpfs", 0x1d, MNT_LOCAL | MNT_NOSUID | MNT_NODEV, 4096 };
		n++;
	}
	fclose(f);
	return n;
}
// Montaje de macOS al que pertenece un archivo (por su statfs y su dev_t): los tmpfs de la lista, /dev o la raíz
static const struct gmount* gmount_of_dev(const struct statfs* s, dev_t dev) {
	if (s->f_type == LINUX_TMPFS_MAGIC) {
		static __thread struct dmount dm[16];
		int n = tmpfs_mounts(dm, 16);
		for (int i = 0; i < n; i++) { struct stat st; if (stat(dm[i].host, &st) == 0 && st.st_dev == dev) return &dm[i].m; }
	}
	return is_devfs_host(s) ? &gmounts[1] : &gmounts[0];
}
// getfsstat64(buf, bufsize, flags): sin búfer devuelve el número de montajes; si no, copia los que quepan.
static long getfsstat_common(uint64_t buf, uint64_t bufsize) {
	static __thread struct dmount dm[16];
	int nd = tmpfs_mounts(dm, 16);
	if (!buf) return (long)NGMOUNT + nd;
	long n = 0;
	for (size_t i = 0; i < NGMOUNT + (size_t)nd && (n + 1) * 2168 <= (long)bufsize; i++) {
		struct statfs s;
		const struct gmount* m = i < NGMOUNT ? &gmounts[i] : &dm[i - NGMOUNT].m;
		const char* hp = i >= NGMOUNT ? dm[i - NGMOUNT].host : !strcmp(m->on, "/") ? (tahoe_root ? tahoe_root : "/") : m->on;
		if (statfs(hp, &s) != 0) memset(&s, 0, sizeof s);
		put_statfs(buf + (uint64_t)n * 2168, &s, m);
		n++;
	}
	return n;
}
static long bsd_getfsstat64(struct ctx* c) { return getfsstat_common(c->a[0], c->a[1]); }
// __mac_getfsstat(buf, bufsize, mac, macsize, flags): igual, sin etiquetas MAC
static long bsd_mac_getfsstat(struct ctx* c) { return getfsstat_common(c->a[0], c->a[1]); }
static long bsd_statfs64(struct ctx* c) {
	char p[4096], b[4200];
	struct statfs s;
	if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT;
	const char* hp = resolve(p, b, sizeof b); struct stat st;
	if (statfs(hp, &s) != 0 || stat(hp, &st) != 0) return err();
	put_statfs(c->a[1], &s, gmount_of_dev(&s, st.st_dev));
	return 0;
}
static long bsd_fstatfs64(struct ctx* c) {
	struct statfs s; struct stat st;
	if (fstatfs((int)c->a[0], &s) != 0 || fstat((int)c->a[0], &st) != 0) return err();
	put_statfs(c->a[1], &s, gmount_of_dev(&s, st.st_dev));
	return 0;
}

// mount(type, path, flags, data) / __mac_mount(..., mac) / fmount(type, fd, flags, data): no hay volúmenes que montar
// (la raíz de macOS es una carpeta de Linux), pero se respeta la semántica de XNU para lo que sí existe:
//   MNT_UPDATE sobre un punto de montaje existente (p. ej. `mount -uw /` del arranque) se acepta;
//   montar sobre una ruta que no existe da ENOENT; cualquier otro montaje nuevo, ENOTSUP (tipo de sistema de archivos
//   que este núcleo no tiene), que es lo que XNU devuelve cuando no conoce el tipo.
// unmount/funmount: la raíz y /dev están ocupados (EBUSY); otra ruta no es un punto de montaje (EINVAL).
static const struct gmount* gmount_at(const char* p) {
	for (size_t i = 0; i < NGMOUNT; i++) if (!strcmp(gmounts[i].on, p)) return &gmounts[i];
	return NULL;
}
static long do_mount(const char* type, const char* path, uint32_t flags) {
	logf_("    mount(\"%s\", \"%s\", 0x%x)\n", type, path, flags);
	const struct gmount* m = gmount_at(path);
	if (!strcmp(type, "tmpfs") && !(flags & MNT_UPDATE)) {   // tmpfs de verdad en el espacio de montajes del arranque
		char b[4200]; const char* hp = resolve(path, b, sizeof b);
		unsigned long lf = ((flags & MNT_RDONLY) ? 1 : 0) | ((flags & MNT_NOSUID) ? 2 : 0) | ((flags & MNT_NODEV) ? 4 : 0);
		if (syscall(SYS_mount, "tmpfs", hp, "tmpfs", lf, "mode=0755") != 0) return err();
		if (!strcmp(path, "/System/Volumes/Data")) data_log_storage(hp);   // el /private nuevo, con el almacén de registros
		vfs_event_signal(0x8);                          // VQ_MOUNT
		return 0;
	}
	if (flags & MNT_UPDATE) {
		if (!m) return -D_EINVAL;
		vfs_event_signal(0x100);                        // VQ_UPDATE, como vfs_event_signal tras una actualización en XNU
		return 0;
	}
	char b[4200]; struct stat st;
	if (stat(resolve(path, b, sizeof b), &st) != 0) return -D_ENOENT;
	if (m) return -16;                                  // EBUSY: ya hay algo montado ahí
	return -45;                                         // ENOTSUP
}
static long bsd_mount(struct ctx* c) {
	char t[64] = "", p[4096];
	if (c->a[0] && safe_string(c->a[0], t, sizeof t) != 0) return -D_EFAULT;
	if (safe_string(c->a[1], p, sizeof p) != 0) return -D_EFAULT;
	return do_mount(t, p, (uint32_t)c->a[2]);
}
static long bsd_fmount(struct ctx* c) {
	char t[64] = "";
	if (c->a[0] && safe_string(c->a[0], t, sizeof t) != 0) return -D_EFAULT;
	logf_("    fmount(\"%s\", fd=%d, 0x%lx) -> ENOTSUP\n", t, (int)c->a[1], c->a[2]);
	return (c->a[2] & MNT_UPDATE) ? 0 : -45;
}
static long bsd_unmount(struct ctx* c) {
	char p[4096];
	if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT;
	logf_("    unmount(\"%s\", 0x%lx)\n", p, c->a[1]);
	struct dmount dm[16]; int nd = tmpfs_mounts(dm, 16);
	for (int i = 0; i < nd; i++) if (!strcmp(dm[i].on, p)) {
		if (syscall(SYS_umount2, dm[i].host, (c->a[1] & 0x80000) ? 1 /* MNT_FORCE de Linux */ : 0) != 0) return err();
		vfs_event_signal(0x10);                         // VQ_UNMOUNT
		return 0;
	}
	return gmount_at(p) ? -16 : -D_EINVAL;
}
static long bsd_funmount(struct ctx* c) { (void)c; return -16; }


// getattrlist(ruta, attrlist*, búfer, tamaño, opciones): atributos del volumen y del objeto.
// Solo se soportan los que usa el arranque: comunes DEVID (0x2) y FSID (0x4), y de volumen CAPABILITIES
// (0x20000) y UUID (0x40000). El búfer lleva primero su longitud total y luego los atributos en orden de bit.
#define ATTR_CMN_DEVID 0x00000002u
#define ATTR_CMN_FSID  0x00000004u
#define ATTR_CMN_FULLPATH 0x08000000u
#define ATTR_VOL_INFO  0x80000000u
#define ATTR_VOL_CAPABILITIES 0x00020000u
#define ATTR_VOL_UUID  0x00040000u
static size_t pack_entry(uint8_t* o, uint32_t common, uint32_t dir, uint32_t file, uint32_t cmnext, const char* name, const char* fullpath, const struct stat* s, int pack_inval);
// Copia el resultado de getattrlist como XNU: si no cabe se copia lo que quepa (truncado, sin error), y el primer campo
// lleva lo copiado, o lo necesario con FSOPT_REPORT_FULLSIZE (0x4).
static long attr_copyout(uint64_t dst, uint64_t size, uint64_t opts, uint8_t* buf, size_t n) {
	size_t m = n < size ? n : size;
	uint32_t len = (opts & 0x4) ? (uint32_t)n : (uint32_t)m;
	memcpy(buf, &len, 4);
	return safe_write(dst, buf, m) == (ssize_t)m ? 0 : -D_EFAULT;
}

static long bsd_getattrlist(struct ctx* c) {
	char p[4096], b[4200];
	uint32_t al[6] = { 0 };
	struct stat s;
	if (safe_string(c->a[0], p, sizeof p) != 0 || safe_read(c->a[1], al, sizeof al) != sizeof al) return -D_EFAULT;
	uint32_t common = al[1], vol = al[2];
	if (trace_all || (diag_target() && diag_all())) logf_("    getattrlist(\"%s\") common=0x%x vol=0x%x dir=0x%x file=0x%x opts=0x%lx\n", p, common, vol, al[3], al[4], (long)c->a[4]);
	if (!vol) {                                           // atributos del objeto: empaquetado general
		int nofollow = (c->a[4] & (1 | 0x800)) != 0;          // FSOPT_NOFOLLOW / FSOPT_NOFOLLOW_ANY
		struct stat st;
		const char* rpth = resolve(p, b, sizeof b);
		if ((nofollow ? lstat(rpth, &st) : stat(rpth, &st)) != 0) return err();
		char real[4096], gp2[4096];
		const char* rp = realpath(rpth, real) ? real : p;
		size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
		if (rl && !strncmp(rp, tahoe_root, rl) && (rp[rl] == '/' || rp[rl] == '\0')) rp += rl;
		snprintf(gp2, sizeof gp2, "%s", rp[0] ? rp : "/");
		const char* base = strrchr(gp2, '/');
		base = base && base[1] ? base + 1 : gp2;
		static uint8_t buf[8192];
		size_t n = pack_entry(buf, common, al[3], al[4], (c->a[4] & 0x20) ? al[5] : 0, base, gp2, &st, (c->a[4] & 8) != 0);
		return attr_copyout(c->a[2], c->a[3], c->a[4], buf, n);
	}
	// Atributos de volumen (con algunos comunes). Orden de sys/attr.h: longitud, returned_attrs (si se pidió), comunes y de
	// volumen por orden de bit. Las cadenas van al final y se señalan con attrreference_t {desplazamiento desde la propia
	// referencia, longitud con el NUL}. El volumen se presenta como el APFS de arranque montado en "/".
	uint32_t known_c = ATTR_CMN_DEVID | ATTR_CMN_FSID | ATTR_CMN_FULLPATH | 0x80000000u /*RETURNED_ATTRS*/;
	uint32_t known_v = 0xF0FFFFFFu;                               // ATTR_VOL_VALIDMASK
	if ((common & ~known_c) || (vol & ~known_v) || al[3] || al[4] || al[5]) {
		logf_("    getattrlist(\"%s\"): atributos sin implementar common=0x%x vol=0x%x dir=0x%x file=0x%x fork=0x%x\n", p, common, vol, al[3], al[4], al[5]);
		return -45;                                       // ENOTSUP
	}
	const char* rpv = resolve(p, b, sizeof b);
	if (stat(rpv, &s) != 0) return err();
	struct statfs sf; if (statfs(rpv, &sf) != 0) memset(&sf, 0, sizeof sf);
	uint8_t out[1024];
	size_t n = 4;                                         // el primer campo es la longitud total
	struct { size_t pos; const char* str; } refs[8]; int nrefs = 0;
#define PUT(v, len) do { memcpy(out + n, (v), (len)); n += (len); } while (0)
#define PUTU32(x) do { uint32_t _v = (uint32_t)(x); PUT(&_v, 4); } while (0)
#define PUTU64(x) do { uint64_t _v = (uint64_t)(x); PUT(&_v, 8); } while (0)
#define PUTREF(sv) do { refs[nrefs].pos = n; refs[nrefs++].str = (sv); n += 8; } while (0)
	if (common & 0x80000000u) {                           // attribute_set_t: lo que se devuelve (todo lo pedido)
		uint32_t rs[5] = { common, vol, 0, 0, 0 };
		PUT(rs, 20);
	}
	char gpath[4096] = "";
	if (common & ATTR_CMN_DEVID) { uint32_t d = guest_dev(s.st_dev); PUT(&d, 4); }
	if (common & ATTR_CMN_FSID) { uint32_t f2[2] = { guest_dev(s.st_dev), 0x18 }; PUT(f2, 8); }
	if (common & ATTR_CMN_FULLPATH) {                     // ruta absoluta tal como la ve macOS (sin la raíz del host)
		char real[4096];
		const char* rp = realpath(rpv, real) ? real : p;
		size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
		if (rl && !strncmp(rp, tahoe_root, rl) && (rp[rl] == '/' || rp[rl] == '\0')) rp += rl;
		snprintf(gpath, sizeof gpath, "%s", rp[0] ? rp : "/");
		PUTREF(gpath);
	}
	uint64_t bs = sf.f_bsize ? (uint64_t)sf.f_bsize : 4096;
	if (vol & 0x1) PUTU32(0x1c);                           // FSTYPE (número de vfs; apfs)
	if (vol & 0x2) PUTU32(0x4244);                         // SIGNATURE
	if (vol & 0x4) PUTU64(sf.f_blocks * bs);               // SIZE
	if (vol & 0x8) PUTU64(sf.f_bfree * bs);                // SPACEFREE
	if (vol & 0x10) PUTU64(sf.f_bavail * bs);              // SPACEAVAIL
	if (vol & 0x20) PUTU64(bs);                            // MINALLOCATION
	if (vol & 0x40) PUTU64(bs);                            // ALLOCATIONCLUMP
	if (vol & 0x80) PUTU32(bs);                            // IOBLOCKSIZE
	if (vol & 0x100) PUTU32(sf.f_files - sf.f_ffree);      // OBJCOUNT
	if (vol & 0x200) PUTU32(sf.f_files - sf.f_ffree);      // FILECOUNT
	if (vol & 0x400) PUTU32(0);                            // DIRCOUNT
	if (vol & 0x800) PUTU32(sf.f_files);                   // MAXOBJCOUNT
	if (vol & 0x1000) PUTREF("/");                         // MOUNTPOINT
	if (vol & 0x2000) PUTREF("macOS Base System");         // NAME: el del volumen de la Recovery (AppleAPFSVolume en IOKit)
	if (vol & 0x4000) PUTU32(0x1000 | 0x4000);             // MOUNTFLAGS: MNT_LOCAL | MNT_ROOTFS
	if (vol & 0x8000) PUTREF("/dev/disk1s1");              // MOUNTEDDEVICE
	if (vol & 0x10000) PUTU64(0);                          // ENCODINGSUSED
	if (vol & ATTR_VOL_CAPABILITIES) {
		// vol_capabilities_attr_t: capabilities[4] y valid[4] (formato, interfaces, reservados)
		uint32_t caps[8] = {
			0x1 | 0x2 | 0x4 | 0x40 | 0x80 | 0x200 | 0x400 | 0x800 | 0x2000 | 0x4000 | 0x20000,   // formato (como APFS)
			0x2 | 0x10 | 0x20 | 0x40 | 0x100 | 0x200 | 0x400 | 0x800 | 0x4000 | 0x10000,         // interfaces
			0, 0,
			0x03ffffff, 0x000fffff, 0, 0,                                                          // bits válidos
		};
		PUT(caps, 32);
	}
	if (vol & ATTR_VOL_UUID) {
		// el UUID del volumen de arranque (kern.bootuuid y AppleAPFSVolume en IOKit): diskarbitrationd solo toma el nombre del
		// volumen montado si coincide con el del disco
		static const uint8_t uuid[16] = { 0xB0, 0x07, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01 };
		PUT(uuid, 16);
	}
	if (vol & 0x80000) PUTU32(0);                          // MOUNTEXTFLAGS
	if (vol & 0x100000) PUTREF("apfs");                    // FSTYPENAME
	if (vol & 0x200000) PUTU32(0);                         // FSSUBTYPE
	if (vol & 0x400000) PUTU32(0);                         // OWNER (root)
	if (vol & 0x800000) PUTU64((sf.f_blocks - sf.f_bfree) * bs);   // SPACEUSED
	if (vol & 0x10000000) PUTU64(0);                       // QUOTA_SIZE
	if (vol & 0x20000000) PUTU64(0);                       // RESERVED_SIZE
	if (vol & 0x40000000) {                                // ATTRIBUTES: vol_attributes_attr_t (válidos y nativos)
		uint32_t va[10] = { 0xffffffff, 0xF0FFFFFF, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xF0FFFFFF, 0xffffffff, 0xffffffff, 0xffffffff };
		PUT(va, 40);
	}
	for (int i = 0; i < nrefs; i++) {                       // cadenas al final
		uint32_t len = (uint32_t)strlen(refs[i].str) + 1;
		if (n + len + 4 > sizeof out) return -D_ENOMEM;
		int32_t off = (int32_t)(n - refs[i].pos);
		memcpy(out + refs[i].pos, &off, 4);
		memcpy(out + refs[i].pos + 4, &len, 4);
		memcpy(out + n, refs[i].str, len);
		n += (len + 3) & ~3u;
	}
#undef PUT
#undef PUTU32
#undef PUTU64
#undef PUTREF
	return attr_copyout(c->a[2], c->a[3], c->a[4], out, n);
}


// chdir / fchdir: el directorio de trabajo es real (dentro de TAHOE_ROOT), así las rutas relativas funcionan solas.
static long bsd_chdir(struct ctx* c) {
	char p[4096], b[4200];
	if (safe_string(c->a[0], p, sizeof p) != 0) return -D_EFAULT;
	RET(chdir(resolve(p, b, sizeof b)));
}
static long bsd_fchdir(struct ctx* c) { RET(fchdir((int)c->a[0])); }

// getdirentries64(fd, búfer, tamaño, base*): entradas de directorio. struct dirent de Darwin (64 bits de inodo):
// {u64 d_ino, u64 d_seekoff, u16 d_reclen, u16 d_namlen, u8 d_type, char d_name[]}, registros alineados a 8.
static long bsd_getdirentries64(struct ctx* c) {
	int fd = (int)c->a[0];
	uint64_t gbuf = c->a[1], gsize = c->a[2];
	uint8_t lin[16384];
	off_t start = lseek(fd, 0, SEEK_CUR);
	size_t want = gsize * 2 / 3 < sizeof lin ? gsize * 2 / 3 : sizeof lin;
	if (want < 512) want = 512 < sizeof lin ? 512 : sizeof lin;
	long n = syscall(SYS_getdents64, fd, lin, want);
	if (n < 0) return err();
	// /dev del anfitrión: al acabar sus entradas se añaden las terminales con nombre de macOS (ttysNNN = /dev/pts/N)
	static uint8_t dev_synth[4096];
	struct stat ds, dd;
	int is_dev = fd >= 0 && fd < 4096 && fstat(fd, &ds) == 0 && stat("/dev", &dd) == 0 && ds.st_ino == dd.st_ino && ds.st_dev == dd.st_dev;
	if (is_dev && n > 0 && start == 0) dev_synth[fd] = 0;
	if (is_dev && n == 0 && !dev_synth[fd]) {
		dev_synth[fd] = 1;
		DIR* pd = opendir("/dev/pts");
		struct dirent* pe;
		long k = 0;
		while (pd && (pe = readdir(pd)) && k + 64 < (long)sizeof lin) {
			unsigned num; char extra;
			if (sscanf(pe->d_name, "%u%c", &num, &extra) != 1) continue;
			char nm[16]; snprintf(nm, sizeof nm, "ttys%03u", num);
			uint8_t* r = lin + k; size_t nl = strlen(nm); uint16_t rl = (uint16_t)((19 + nl + 1 + 7) & ~7UL);
			memset(r, 0, rl);
			*(uint64_t*)r = pe->d_ino; *(int64_t*)(r + 8) = (int64_t)(0x7fff0000 + num); *(uint16_t*)(r + 16) = rl; r[18] = DT_CHR;
			memcpy(r + 19, nm, nl + 1);
			k += rl;
		}
		if (pd) closedir(pd);
		for (int i = 0; i < (int)(sizeof emu_disks / sizeof emu_disks[0]) * 2 && k + 64 < (long)sizeof lin; i++) {   // y los discos
			char nm[16]; snprintf(nm, sizeof nm, "%s%s", (i & 1) ? "r" : "", emu_disks[i / 2]);
			uint8_t* r = lin + k; size_t nl = strlen(nm); uint16_t rl = (uint16_t)((19 + nl + 1 + 7) & ~7UL);
			memset(r, 0, rl);
			*(uint64_t*)r = 0x7ffe0000 + i; *(int64_t*)(r + 8) = (int64_t)(0x7ffe0000 + i); *(uint16_t*)(r + 16) = rl; r[18] = (i & 1) ? DT_CHR : DT_BLK;
			memcpy(r + 19, nm, nl + 1);
			k += rl;
		}
		n = k;
	}
	uint8_t* out = malloc(gsize);
	size_t used = 0;
	off_t prev = start;
	for (long pos = 0; pos < n;) {
		uint64_t ino = *(uint64_t*)(lin + pos);
		int64_t off = *(int64_t*)(lin + pos + 8);
		uint16_t reclen = *(uint16_t*)(lin + pos + 16);
		uint8_t type = lin[pos + 18];
		const char* name = (const char*)lin + pos + 19;
		size_t nl = strlen(name);
		size_t dr = (21 + nl + 1 + 7) & ~7UL;
		if (used + dr > gsize) { lseek(fd, prev, SEEK_SET); break; }     // no cabe: devolver la entrada al directorio
		uint8_t* e = out + used;
		memset(e, 0, dr);
		memcpy(e, &ino, 8);
		memcpy(e + 8, &off, 8);
		uint16_t drl = (uint16_t)dr, nlen = (uint16_t)nl;
		memcpy(e + 16, &drl, 2);
		memcpy(e + 18, &nlen, 2);
		e[20] = type;
		memcpy(e + 21, name, nl + 1);
		used += dr;
		prev = off;
		pos += reclen;
	}
	long rc = (long)used;
	if (used && safe_write(gbuf, out, used) != (ssize_t)used) rc = -D_EFAULT;
	free(out);
	if (rc >= 0 && c->a[3]) { int64_t base = lseek(fd, 0, SEEK_CUR); safe_write(c->a[3], &base, 8); }
	return rc;
}


// getattrlistbulk: lectura de directorios con atributos. Sin implementar: ENOTSUP (45) hace que fts() use readdir.
#define A_CMN_NAME 0x00000001u
#define A_CMN_DEVID 0x00000002u
#define A_CMN_FSID 0x00000004u
#define A_CMN_OBJTYPE 0x00000008u
#define A_CMN_CRTIME 0x00000200u
#define A_CMN_MODTIME 0x00000400u
#define A_CMN_CHGTIME 0x00000800u
#define A_CMN_ACCTIME 0x00001000u
#define A_CMN_OWNERID 0x00008000u
#define A_CMN_GRPID 0x00010000u
#define A_CMN_ACCESSMASK 0x00020000u
#define A_CMN_FLAGS 0x00040000u
#define A_CMN_FILEID 0x02000000u
#define A_CMN_RETURNED 0x80000000u
#define A_FILE_LINKCOUNT 0x1u
#define A_FILE_TOTALSIZE 0x2u
#define A_FILE_ALLOCSIZE 0x4u
#define A_FILE_IOBLOCKSIZE 0x8u
#define A_FILE_DEVTYPE 0x20u
#define A_FILE_DATALENGTH 0x200u

static uint32_t vtype_of(mode_t m) {                       // fsobj_type_t
	switch (m & S_IFMT) {
	case S_IFREG: return 1; case S_IFDIR: return 2; case S_IFBLK: return 3; case S_IFCHR: return 4;
	case S_IFLNK: return 5; case S_IFSOCK: return 6; case S_IFIFO: return 7; default: return 0;
	}
}

// Empaqueta los atributos de un objeto (getattrlist y getattrlistbulk). El orden es el de sys/attr.h: primero
// returned_attrs si se pidió, luego comunes, de directorio y de archivo, cada grupo por orden de bit. Los
// atributos que no valen para el objeto (de archivo en un directorio y viceversa) se omiten, o quedan a cero sin
// marcar en returned_attrs si se pidió FSOPT_PACK_INVAL_ATTRS.
#define A_CMN_OBJTAG 0x10u
#define A_CMN_OBJID 0x20u
#define A_CMN_PAROBJID 0x80u
#define A_CMN_FNDRINFO 0x4000u
#define A_CMN_GENCOUNT 0x80000u
#define A_CMN_DOCID 0x100000u
#define A_CMN_USERACCESS 0x200000u
#define A_CMN_PARENTID 0x4000000u
#define A_CMN_FULLPATH2 0x08000000u
#define A_CMN_ADDEDTIME 0x10000000u
#define A_CMN_DPFLAGS 0x40000000u
#define A_DIR_LINKCOUNT 0x1u
#define A_DIR_ENTRYCOUNT 0x2u
#define A_DIR_MOUNTSTATUS 0x4u
// cmnext: atributos comunes extendidos (FSOPT_ATTR_CMN_EXTENDED: viajan en el campo forkattr), después de los de archivo.
static size_t pack_entry(uint8_t* o, uint32_t common, uint32_t dir, uint32_t file, uint32_t cmnext, const char* name, const char* fullpath, const struct stat* s, int pack_inval) {
	size_t n = 4;                                            // longitud total, se rellena al final
	size_t ret_at = n;
	if (common & A_CMN_RETURNED) n += 20;                    // attribute_set_t returned_attrs
	uint32_t rc = 0, rd = 0, rf = 0;
	int isdir = S_ISDIR(s->st_mode);
	size_t name_ref = 0, path_ref = 0;
#define P32(v) do { uint32_t t_ = (uint32_t)(v); memcpy(o + n, &t_, 4); n += 4; } while (0)
#define P64(v) do { uint64_t t_ = (uint64_t)(v); memcpy(o + n, &t_, 8); n += 8; } while (0)
#define PTS(ts) do { P64((ts).tv_sec); P64((ts).tv_nsec); } while (0)
	if (common & A_CMN_NAME) { name_ref = n; n += 8; rc |= A_CMN_NAME; }
	if (common & A_CMN_DEVID) { P32(guest_dev(s->st_dev)); rc |= A_CMN_DEVID; }
	if (common & A_CMN_FSID) { P32(guest_dev(s->st_dev)); P32(0x18); rc |= A_CMN_FSID; }
	if (common & A_CMN_OBJTYPE) { P32(vtype_of(s->st_mode)); rc |= A_CMN_OBJTYPE; }
	if (common & A_CMN_OBJTAG) { P32(0x17); rc |= A_CMN_OBJTAG; }                         // VT_APFS
	if (common & A_CMN_OBJID) { P32((uint32_t)s->st_ino); P32((uint32_t)(s->st_ino >> 32)); rc |= A_CMN_OBJID; }
	if (common & A_CMN_PAROBJID) { P32(0); P32(0); rc |= A_CMN_PAROBJID; }
	if (common & A_CMN_CRTIME) { PTS(s->st_ctim); rc |= A_CMN_CRTIME; }
	if (common & A_CMN_MODTIME) { PTS(s->st_mtim); rc |= A_CMN_MODTIME; }
	if (common & A_CMN_CHGTIME) { PTS(s->st_ctim); rc |= A_CMN_CHGTIME; }
	if (common & A_CMN_ACCTIME) { PTS(s->st_atim); rc |= A_CMN_ACCTIME; }
	if (common & A_CMN_FNDRINFO) { memset(o + n, 0, 32); n += 32; rc |= A_CMN_FNDRINFO; }
	if (common & A_CMN_OWNERID) { P32(guest_uid(s->st_uid)); rc |= A_CMN_OWNERID; }
	if (common & A_CMN_GRPID) { P32(guest_gid(s->st_gid)); rc |= A_CMN_GRPID; }
	if (common & A_CMN_ACCESSMASK) { P32(s->st_mode & 07777); rc |= A_CMN_ACCESSMASK; }
	if (common & A_CMN_FLAGS) { P32(0); rc |= A_CMN_FLAGS; }
	if (common & A_CMN_GENCOUNT) { P32(0); rc |= A_CMN_GENCOUNT; }
	if (common & A_CMN_DOCID) { P32(0); rc |= A_CMN_DOCID; }
	if (common & A_CMN_USERACCESS) {                                                      // R_OK 4, W_OK 2, X_OK 1 de Darwin = bits 4,2,1... se calcula por permisos del propietario/otros
		uint32_t ua = 0; uid_t u = geteuid();
		mode_t m = s->st_mode;
		int sh = (u == s->st_uid) ? 6 : 0;
		if (u == 0 || (m >> sh) & 4) ua |= 4;
		if (u == 0 || (m >> sh) & 2) ua |= 2;
		if (u == 0 || (m >> sh) & 1) ua |= 1;
		P32(ua); rc |= A_CMN_USERACCESS;
	}
	if (common & A_CMN_FILEID) { P64(s->st_ino); rc |= A_CMN_FILEID; }
	if (common & A_CMN_PARENTID) { P64(0); rc |= A_CMN_PARENTID; }
	if (common & A_CMN_FULLPATH2) { path_ref = n; n += 8; rc |= A_CMN_FULLPATH2; }
	if (common & A_CMN_ADDEDTIME) { PTS(s->st_ctim); rc |= A_CMN_ADDEDTIME; }
	if (common & 0x20000000u) { P32(0); rc |= 0x20000000u; }                              // ATTR_CMN_ERROR: sin error
	if (common & A_CMN_DPFLAGS) { P32(0); rc |= A_CMN_DPFLAGS; }
	// directorio: solo valen para directorios
	if (dir) {
		if (dir & A_DIR_LINKCOUNT) { P32(isdir ? s->st_nlink : 0); if (isdir) rd |= A_DIR_LINKCOUNT; }
		if (dir & A_DIR_ENTRYCOUNT) { P32(0); if (isdir) rd |= A_DIR_ENTRYCOUNT; }
		if (dir & A_DIR_MOUNTSTATUS) {                                                    // DIR_MNTSTATUS_MNTPOINT si es raíz de un montaje
			uint32_t ms = 0;
			for (size_t i = 0; isdir && fullpath && i < NGMOUNT; i++) if (!strcmp(fullpath, gmounts[i].on)) ms = 0x1;
			P32(ms); if (isdir) rd |= A_DIR_MOUNTSTATUS;
		}
	}
	// archivo: solo valen para no directorios
	if (file & A_FILE_LINKCOUNT) { P32(isdir ? 0 : s->st_nlink); if (!isdir) rf |= A_FILE_LINKCOUNT; }
	if (file & A_FILE_TOTALSIZE) { P64(isdir ? 0 : s->st_size); if (!isdir) rf |= A_FILE_TOTALSIZE; }
	if (file & A_FILE_ALLOCSIZE) { P64(isdir ? 0 : (uint64_t)s->st_blocks * 512); if (!isdir) rf |= A_FILE_ALLOCSIZE; }
	if (file & A_FILE_IOBLOCKSIZE) { P32(isdir ? 0 : s->st_blksize); if (!isdir) rf |= A_FILE_IOBLOCKSIZE; }
	if (file & A_FILE_DEVTYPE) { P32(isdir ? 0 : (uint32_t)s->st_rdev); if (!isdir) rf |= A_FILE_DEVTYPE; }
	if (file & A_FILE_DATALENGTH) { P64(isdir ? 0 : s->st_size); if (!isdir) rf |= A_FILE_DATALENGTH; }
	if (file & 0x400u) { P64(isdir ? 0 : (uint64_t)s->st_blocks * 512); if (!isdir) rf |= 0x400u; }   // DATAALLOCSIZE
	if (file & 0x1000u) { P64(0); if (!isdir) rf |= 0x1000u; }                                         // RSRCLENGTH
	if (file & 0x2000u) { P64(0); if (!isdir) rf |= 0x2000u; }                                         // RSRCALLOCSIZE
	uint32_t rx = 0;
	size_t relpath_ref = 0, nofirm_ref = 0;
	if (cmnext & 0x4) { relpath_ref = n; n += 8; rx |= 0x4; }                          // RELPATH
	if (cmnext & 0x8) { P64(0); rx |= 0x8; }                                             // PRIVATESIZE
	if (cmnext & 0x10) { P64(s->st_ino); rx |= 0x10; }                                   // LINKID
	if (cmnext & 0x20) { nofirm_ref = n; n += 8; rx |= 0x20; }                         // NOFIRMLINKPATH
	if (cmnext & 0x40) { P32(guest_dev(s->st_dev)); rx |= 0x40; }   // REALDEVID
	if (cmnext & 0x80) { P32(guest_dev(s->st_dev)); P32(0x18); rx |= 0x80; }             // REALFSID
	if (cmnext & 0x100) { P64(s->st_ino); rx |= 0x100; }                                 // CLONEID
	if (cmnext & 0x200) { P64(0); rx |= 0x200; }                                         // EXT_FLAGS
	if (cmnext & 0x400) { P64(0); rx |= 0x400; }                                         // RECURSIVE_GENCOUNT
	if (cmnext & 0x800) { P64(0); rx |= 0x800; }                                         // ATTRIBUTION_TAG
	if (cmnext & 0x1000) { P32(1); rx |= 0x1000; }                                       // CLONE_REFCNT
	(void)pack_inval;
	// cadenas de longitud variable al final, en el orden de sus referencias
	if (common & A_CMN_NAME) {
		size_t nl = strlen(name) + 1;
		int32_t off = (int32_t)(n - name_ref);
		uint32_t len = (uint32_t)nl;
		memcpy(o + name_ref, &off, 4);
		memcpy(o + name_ref + 4, &len, 4);
		memcpy(o + n, name, nl);
		n += (nl + 3) & ~3UL;
	}
	if (common & A_CMN_FULLPATH2) {
		size_t pl = strlen(fullpath) + 1;
		int32_t off = (int32_t)(n - path_ref);
		uint32_t len = (uint32_t)pl;
		memcpy(o + path_ref, &off, 4);
		memcpy(o + path_ref + 4, &len, 4);
		memcpy(o + n, fullpath, pl);
		n += (pl + 3) & ~3UL;
	}
	const char* rel_or_path[2] = { fullpath, fullpath };
	size_t refs2[2] = { relpath_ref, nofirm_ref };
	for (int i = 0; i < 2; i++) {
		if (!refs2[i]) continue;
		size_t pl = strlen(rel_or_path[i]) + 1;
		int32_t off = (int32_t)(n - refs2[i]);
		uint32_t len = (uint32_t)pl;
		memcpy(o + refs2[i], &off, 4);
		memcpy(o + refs2[i] + 4, &len, 4);
		memcpy(o + n, rel_or_path[i], pl);
		n += (pl + 3) & ~3UL;
	}
	if (common & A_CMN_RETURNED) {
		uint32_t ret[5] = { rc | A_CMN_RETURNED, 0, rd, rf, rx };    // commonattr, volattr, dirattr, fileattr, forkattr (cmnext)
		memcpy(o + ret_at, ret, 20);
	}
	uint32_t total = (uint32_t)n;
	memcpy(o, &total, 4);
	return n;
#undef P32
#undef P64
#undef PTS
}
static uint32_t dir_attrs_g;                              // dirattr de la petición en curso (getattrlistbulk)
static size_t pack_bulk_entry(uint8_t* o, uint32_t common, uint32_t file, const char* name, const struct stat* s) {
	return pack_entry(o, common, dir_attrs_g, file, 0, name, "", s, 1);
}

// getattrlistbulk(fd, attrlist*, búfer, tamaño, opciones): devuelve cuántas entradas empaquetó (0 = fin).
static long bsd_getattrlistbulk(struct ctx* c) {
	int fd = (int)c->a[0];
	uint32_t al[6] = { 0 };
	if (safe_read(c->a[1], al, sizeof al) != sizeof al) return -D_EFAULT;
	uint32_t common = al[1], dir = al[3], file = al[4];
	uint32_t known_c = A_CMN_NAME | A_CMN_DEVID | A_CMN_FSID | A_CMN_OBJTYPE | A_CMN_OBJTAG | A_CMN_OBJID | A_CMN_PAROBJID | A_CMN_CRTIME | A_CMN_MODTIME |
		A_CMN_CHGTIME | A_CMN_ACCTIME | A_CMN_FNDRINFO | A_CMN_OWNERID | A_CMN_GRPID | A_CMN_ACCESSMASK | A_CMN_FLAGS | A_CMN_GENCOUNT | A_CMN_DOCID |
		A_CMN_USERACCESS | A_CMN_FILEID | A_CMN_PARENTID | A_CMN_ADDEDTIME | 0x20000000u /*ERROR*/ | A_CMN_DPFLAGS | A_CMN_RETURNED;
	uint32_t known_f = A_FILE_LINKCOUNT | A_FILE_TOTALSIZE | A_FILE_ALLOCSIZE | A_FILE_IOBLOCKSIZE | A_FILE_DEVTYPE | A_FILE_DATALENGTH | 0x400u | 0x1000u | 0x2000u;
	if ((common & ~known_c) || (file & ~known_f) || (dir & ~7u) || al[2] || al[5]) {
		logf_("    getattrlistbulk: atributos sin implementar common=0x%x dir=0x%x file=0x%x\n", common, dir, file);
		return -45;
	}
	dir_attrs_g = dir;
	uint64_t gbuf = c->a[2], gsize = c->a[3];
	uint8_t lin[16384];
	uint8_t* out = malloc(gsize);
	if (!out) return -D_ENOMEM;
	size_t used = 0;
	long count = 0, rc = 0;
	for (;;) {
		off_t start = lseek(fd, 0, SEEK_CUR);
		long n = syscall(SYS_getdents64, fd, lin, 4096);
		if (n < 0) { rc = err(); break; }
		if (n == 0) break;
		int full = 0;
		off_t prev = start;
		for (long pos = 0; pos < n;) {
			int64_t off = *(int64_t*)(lin + pos + 8);
			uint16_t reclen = *(uint16_t*)(lin + pos + 16);
			const char* name = (const char*)lin + pos + 19;
			if (!strcmp(name, ".") || !strcmp(name, "..")) { prev = off; pos += reclen; continue; }   // el kernel no las devuelve
			struct stat s;
			if (fstatat(fd, name, &s, AT_SYMLINK_NOFOLLOW) != 0) { prev = off; pos += reclen; continue; }
			uint8_t e[2048];
			size_t en = pack_bulk_entry(e, common, file, name, &s);
			if (used + en > gsize) { lseek(fd, prev, SEEK_SET); full = 1; break; }
			memcpy(out + used, e, en);
			used += en;
			count++;
			prev = off;
			pos += reclen;
		}
		if (full || count > 0) break;                         // con una pasada con resultados basta
	}
	if (rc < 0) { free(out); return rc; }
	if (used && safe_write(gbuf, out, used) != (ssize_t)used) { free(out); return -D_EFAULT; }
	free(out);
	return count;
}


// fsctl(ruta, comando, datos, opciones): ioctls de sistema de archivos. 0x40084a6a ('J', 106) es una consulta privada de
// APFS del estado del dispositivo raíz que lanza launchd; devuelve 8 bytes. Se responde sin banderas.
static long bsd_fsctl(struct ctx* c) {
	if (c->a[1] == 0x40084a6aUL) { uint64_t z = 0; return safe_write(c->a[2], &z, 8) == 8 ? 0 : -D_EFAULT; }
	logf_("    fsctl: comando 0x%lx sin implementar\n", c->a[1]);
	return -45;
}

// openat: AT_FDCWD vale -2 en Darwin
static long bsd_openat(struct ctx* c) {
	char path[4096], full[4200];
	if (safe_string(c->a[1], path, sizeof path) != 0) return -D_EFAULT;
	int dfd = (int)c->a[0];
	if (dfd == -2) dfd = AT_FDCWD;
	int flags = darwin_open_flags((int)c->a[2]);
	long fd = -1;
	if (!strcmp(path, "/dev/fsevents")) { int f = fsevents_open(); return f < 0 ? -2 : f; }
	if (tahoe_root && path[0] == '/' && !tahoe_host_passthrough(path)) {
		snprintf(full, sizeof full, "%s%s", tahoe_root, path);
		fd = open_symlink(dfd, full, (int)c->a[2], flags, syscall(SYS_openat, dfd, full, flags, (int)c->a[3]));
	}
	if (fd < 0 && (!tahoe_root || path[0] != '/' || tahoe_host_passthrough(path))) { char al[64]; const char* q = tahoe_dev_alias(path, al, sizeof al); fd = open_symlink(dfd, q, (int)c->a[2], flags, syscall(SYS_openat, dfd, q, flags, (int)c->a[3])); }
	if (trace_all || fd < 0) logf_("    openat(%d, \"%s\") -> %ld%s%s\n", (int)c->a[0], path, fd, fd < 0 ? " " : "", fd < 0 ? strerror(errno) : "");
	return fd < 0 ? err() : fd;
}

// fsgetpath(búfer, tamaño, fsid*, objid): ruta a partir de un identificador de archivo. Solo se conoce la
// caché de dyld, cuyo identificador se publicó en la región dinámica (inodo en objid).
static long bsd_fsgetpath(struct ctx* c) {
	if (cache_guest_path[0] && (uint32_t)c->a[3] == (uint32_t)cache_ino) {
		size_t n = strlen(cache_guest_path) + 1;
		if (n > c->a[1]) return -D_ENOMEM;
		return safe_write(c->a[0], cache_guest_path, n) == (ssize_t)n ? (long)n : -D_EFAULT;
	}
	logf_("    fsgetpath: identificador desconocido (objid=0x%lx)\n", c->a[3]);
	return -D_ENOENT;
}

// ---------------------------------------------------------------- identidad y políticas
static long bsd_getuid(struct ctx* c)  { (void)c; return getuid(); }
static long bsd_geteuid(struct ctx* c) { (void)c; return geteuid(); }
static long bsd_getgid(struct ctx* c)  { (void)c; return getgid(); }
static long bsd_getegid(struct ctx* c) { (void)c; return getegid(); }

// __mac_syscall(política, llamada, argumento): sandbox, AMFI... Se registra y se acepta.
static long bsd_mac_syscall(struct ctx* c) {
	char pol[64];
	if (safe_string(c->a[0], pol, sizeof pol) != 0) strcpy(pol, "?");
	logf_("    __mac_syscall(\"%s\", llamada=%d) -> aceptada\n", pol, (int)c->a[1]);
	return 0;
}


// ---- volumen de datos: macOS crea el volumen Data a partir de /System/Library/Templates/Data (directorios de /private,
// /Library, /Users... con sus permisos, y archivos como la base de usuarios dslocal). Aquí la raíz es un único árbol: al
// arrancar el primer proceso se copia de la plantilla todo lo que falte, sin tocar lo que ya existe.
#include <ftw.h>
static char tmpl_root[4300];
static size_t tmpl_len;
static int tmpl_copy(const char* src, const struct stat* st, int type, struct FTW* fw) {
	(void)fw;
	char dst[8600];
	snprintf(dst, sizeof dst, "%s%s", tahoe_root, src + tmpl_len);
	if (!src[tmpl_len]) return 0;                                 // la propia plantilla
	struct stat ds;
	if (lstat(dst, &ds) == 0) return 0;                           // ya existe: no se sobrescribe
	if (type == FTW_D) { if (mkdir(dst, st->st_mode & 07777) == 0) chmod(dst, st->st_mode & 07777); }
	else if (type == FTW_SL) { char t[4096]; ssize_t n = readlink(src, t, sizeof t - 1); if (n > 0) { t[n] = 0; if (symlink(t, dst) != 0) {} } }
	else if (type == FTW_F) {
		int in = open(src, O_RDONLY | O_CLOEXEC), out = in < 0 ? -1 : open(dst, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, st->st_mode & 07777);
		char b[65536]; ssize_t n;
		while (in >= 0 && out >= 0 && (n = read(in, b, sizeof b)) > 0) if (write(out, b, (size_t)n) != n) break;
		if (in >= 0) close(in);
		if (out >= 0) close(out);
	}
	return 0;
}
// /private/var/run es por arranque (sockets, .pid, marcas como systemkeychaincheck.done o WindowServer.didRunThisBoot): en un
// Mac empieza vacío. Aquí la raíz persiste entre arranques, así que lo vacía el arranque (launchd como PID 1); la shell
// suelta no es un arranque y no lo toca.
static int run_rm(const char* p, const struct stat* st, int type, struct FTW* fw) {
	(void)st;
	if (fw->level == 0) return 0;                                 // el propio /private/var/run se conserva
	if (type == FTW_DP) rmdir(p); else unlink(p);
	return 0;
}
static void clear_var_run(void) {
	char p[4400];
	snprintf(p, sizeof p, "%s/private/var/run", tahoe_root);
	nftw(p, run_rm, 32, FTW_PHYS | FTW_DEPTH);
}
void populate_data_volume(void) {
	if (!tahoe_root) return;
	if (getpid() == 1) clear_var_run();
	snprintf(tmpl_root, sizeof tmpl_root, "%s/System/Library/Templates/Data", tahoe_root);
	tmpl_len = strlen(tmpl_root);
	struct stat st;
	if (stat(tmpl_root, &st) != 0) return;
	nftw(tmpl_root, tmpl_copy, 32, FTW_PHYS);
	data_log_storage(tahoe_root);
}
// Almacén de registros de logd. En un BaseSystem, libsystem_trace (_os_trace_basesystem_storage_available) solo da por
	// disponible el almacenamiento si /private/var/db/diagnostics es un enlace simbólico (el arranque lo apunta a un volumen
	// escribible) o si los boot-args lo indican; si no, launchd decide que no hay registro completo (HasFullLogging = NO) y
	// no carga logd. Aquí la raíz es una carpeta escribible de Linux: se presenta como lo haría un arranque con almacenamiento.
// Se hace sobre el volumen de datos: al arrancar launchd (populate_data_volume) y otra vez cuando la Recovery monta su
// tmpfs en /System/Volumes/Data (do_mount), que tapa lo anterior; si no, logd queda cargado sin almacén y `log` no conecta.
void data_log_storage(const char* data_root) {
	char p[4400], q[4400]; struct stat st;
	snprintf(p, sizeof p, "%s/private/var/db", data_root);
	if (stat(p, &st) != 0) { char a[4400]; snprintf(a, sizeof a, "%s/private", data_root); mkdir(a, 0755); snprintf(a, sizeof a, "%s/private/var", data_root); mkdir(a, 0755); mkdir(p, 0755); }
	snprintf(p, sizeof p, "%s/private/var/db/diagnostics", data_root);
	if (lstat(p, &st) != 0) {
		snprintf(q, sizeof q, "%s/private/var/db/diagnostics.storage", data_root);
		mkdir(q, 0755);
		if (symlink("diagnostics.storage", p) != 0) {}
	}
	snprintf(p, sizeof p, "%s/private/var/db/uuidtext", data_root);
	if (lstat(p, &st) != 0) mkdir(p, 0755);
}

// ---- map_with_linking_np(regiones[], n, info, tamaño): como vm_dyld_pager de XNU. dyld pide al núcleo que proyecte las
// regiones de datos de una imagen desde su archivo y que aplique al hacerlo los fixups encadenados (rebases con el
// deslizamiento y binds con la tabla de punteros ya resueltos que manda en `info`). Sin esto, esas páginas quedaban con los
// punteros codificados (p. ej. la rutina de personalidad de las excepciones de C++).
struct mwl_region_ { int32_t fd; int32_t prot; uint64_t off, addr, size; };
struct mwl_hdr_ { uint32_t version; uint16_t page_size, ptr_format; uint32_t binds_off, binds_count, chains_off, chains_size; uint64_t slide, image_address; };
static int mwl_fixup_page(uint8_t* page, uint64_t pagesz, uint16_t start, int fmt, const uint64_t* binds, uint32_t nb, const struct mwl_hdr_* h) {
	uint64_t off = start;
	for (;;) {
		if (off + 8 > pagesz) return -1;
		uint64_t v; memcpy(&v, page + off, 8);
		uint64_t next = (v >> 51) & 0xfff, nv;
		if (v >> 63) {                                            // bind: ordinal (24) + addend (8)
			uint32_t ord = (uint32_t)(v & 0xffffff); uint64_t add = (v >> 24) & 0xff;
			if (ord >= nb) return -1;
			nv = binds[ord] + add;
		} else {                                                  // rebase: objetivo (36) + high8
			uint64_t target = v & 0xfffffffffULL, high8 = (v >> 36) & 0xff;
			nv = (fmt == 6 ? h->image_address + target : target + h->slide) | (high8 << 56);
		}
		memcpy(page + off, &nv, 8);
		if (!next) return 0;
		off += next * 4;
	}
}
static long bsd_map_with_linking(struct ctx* c) {
	if (access("/dev/shm/tahoe-mwl-off", F_OK) == 0) return -78;   // diagnóstico: que enlace dyld (comportamiento antiguo)
	uint32_t nreg = (uint32_t)c->a[1], bsz = (uint32_t)c->a[3];
	if (nreg == 0 || nreg > 5 || bsz < sizeof(struct mwl_hdr_) || bsz > (1u << 24)) return -D_EINVAL;
	struct mwl_region_ r[5];
	if (safe_read(c->a[0], r, nreg * sizeof r[0]) != (ssize_t)(nreg * sizeof r[0])) return -D_EFAULT;
	uint8_t* blob = malloc(bsz);
	if (safe_read(c->a[2], blob, bsz) != (ssize_t)bsz) { free(blob); return -D_EFAULT; }
	struct mwl_hdr_ h; memcpy(&h, blob, sizeof h);
	if (diag_target()) { logf_("    MWL <%d> v=%u pág=0x%x fmt=%u binds=%u@0x%x cadenas=0x%x@0x%x slide=0x%lx imagen=0x%lx regiones=%u\n", (int)getpid(), h.version, h.page_size, h.ptr_format, h.binds_count, h.binds_off, h.chains_size, h.chains_off, h.slide, h.image_address, nreg);
		for (uint32_t i = 0; i < nreg; i++) logf_("      región fd=%d prot=%d off=0x%lx dir=0x%lx tam=0x%lx\n", r[i].fd, r[i].prot, r[i].off, r[i].addr, r[i].size); }
	long rc = 0;
	if (h.binds_off + (uint64_t)h.binds_count * 8 > bsz || h.chains_off + (uint64_t)h.chains_size > bsz || (h.ptr_format != 2 && h.ptr_format != 6)) { rc = -D_EINVAL; goto out; }
	// Las regiones ya están en memoria con el contenido del archivo (las proyectó el cargador, con la cola de bss a cero, y
	// dyld aún no las tocó): se enlazan en el sitio. Volver a proyectarlas desde el archivo metería en la última página los
	// bytes que siguen al segmento en vez de ceros.
	for (uint32_t i = 0; i < nreg; i++)
		if (mprotect((void*)r[i].addr, r[i].size, PROT_READ | PROT_WRITE) != 0) { rc = err(); goto out; }
	{
		const uint64_t* binds = (const uint64_t*)(blob + h.binds_off);
		const uint8_t* ch = blob + h.chains_off;
		uint32_t segc; memcpy(&segc, ch, 4);
		for (uint32_t sgi = 0; sgi < segc; sgi++) {
			uint32_t so; memcpy(&so, ch + 4 + sgi * 4, 4);
			if (!so || so + 22 > h.chains_size) continue;
			const uint8_t* sg = ch + so;
			uint16_t psz, fmt, pcount; uint64_t segoff;
			memcpy(&psz, sg + 4, 2); memcpy(&fmt, sg + 6, 2); memcpy(&segoff, sg + 8, 8); memcpy(&pcount, sg + 20, 2);
			for (uint16_t pi = 0; pi < pcount; pi++) {
				uint16_t st; memcpy(&st, sg + 22 + pi * 2, 2);
				if (st == 0xffff) continue;
				uint64_t pa = h.image_address + segoff + (uint64_t)pi * psz;
				int inside = 0;
				for (uint32_t i = 0; i < nreg; i++) if (pa >= r[i].addr && pa + psz <= r[i].addr + r[i].size) inside = 1;
				if (!inside) continue;                           // páginas de otras regiones: ya las enlazó dyld
				if (mwl_fixup_page((uint8_t*)pa, psz, st, fmt, binds, h.binds_count, &h) != 0) { rc = -D_EINVAL; goto out; }
			}
		}
	}
	for (uint32_t i = 0; i < nreg; i++) mprotect((void*)r[i].addr, r[i].size, r[i].prot & 7);
out:
	free(blob);
	if (rc) logf_("    map_with_linking_np: fallo %ld\n", rc);
	return rc;
}

void emu_fs_xattr_init(void) {
	reg_bsd(550, bsd_map_with_linking);                                  // tras emu_fs2_init (sustituye su getxattr provisional)
	reg_bsd(234, bsd_getxattr); reg_bsd(235, bsd_fgetxattr); reg_bsd(236, bsd_setxattr); reg_bsd(237, bsd_fsetxattr);
	reg_bsd(238, bsd_removexattr); reg_bsd(239, bsd_fremovexattr); reg_bsd(240, bsd_listxattr); reg_bsd(241, bsd_flistxattr);
	reg_bsd(341, bsd_stat64_ext); reg_bsd(342, bsd_lstat64_ext); reg_bsd(343, bsd_fstat64_ext);
}
void emu_fs_init(void) {
	reg_bsd(73, bsd_munmap);   reg_bsd(74, bsd_mprotect); reg_bsd(75, bsd_madvise); reg_bsd(197, bsd_mmap);
	reg_bsd(199, bsd_lseek);   reg_bsd(153, bsd_pread);   reg_bsd(154, bsd_pwrite);
	reg_bsd(33, bsd_access);   reg_bsd(58, bsd_readlink); reg_bsd(473, bsd_readlinkat); reg_bsd(92, bsd_fcntl);
	reg_bsd(338, bsd_stat64);  reg_bsd(339, bsd_fstat64); reg_bsd(340, bsd_lstat64);
	reg_bsd(24, bsd_getuid);   reg_bsd(25, bsd_geteuid);  reg_bsd(47, bsd_getgid); reg_bsd(43, bsd_getegid);
	reg_bsd(381, bsd_mac_syscall);
	reg_bsd(347, bsd_getfsstat64); reg_bsd(345, bsd_statfs64); reg_bsd(346, bsd_fstatfs64); reg_bsd(426, bsd_mac_getfsstat);
	reg_bsd(167, bsd_mount); reg_bsd(424, bsd_mount); reg_bsd(526, bsd_fmount); reg_bsd(159, bsd_unmount); reg_bsd(164, bsd_funmount);
	reg_bsd(242, bsd_fsctl); reg_bsd(220, bsd_getattrlist); reg_bsd(12, bsd_chdir); reg_bsd(13, bsd_fchdir); reg_bsd(344, bsd_getdirentries64); reg_bsd(461, bsd_getattrlistbulk);
	reg_bsd(41, bsd_dup);      reg_bsd(90, bsd_dup2);     reg_bsd(470, bsd_fstatat64);
	reg_bsd(463, bsd_openat); reg_bsd(464, bsd_openat);   /* openat_nocancel */     reg_bsd(427, bsd_fsgetpath);
}
