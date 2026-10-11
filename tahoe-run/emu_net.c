// Sockets de macOS sobre los de Linux: traducción de sockaddr, opciones, banderas y mensajes con descriptores.
// AF_UNIX, AF_INET y AF_INET6 directos; PF_SYSTEM/SYSPROTO_EVENT (eventos del núcleo) con un socket de datagramas
// propio; el resto de dominios (AF_ROUTE, SYSPROTO_CONTROL...) responde EAFNOSUPPORT.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/uio.h>
#include <netinet/in.h>

#include "tahoe.h"

#define D_EAFNOSUPPORT 47
#define D_ENOPROTOOPT 42

static long err(void) { return -darwin_errno(errno); }
static int dom_to_linux(int d) { return d == 30 ? AF_INET6 : d; }

// sockaddr de Darwin (len, familia u8) -> Linux. Los sockets Unix se ubican bajo TAHOE_ROOT.
static int sa_in(const uint8_t* d, uint32_t len, uint8_t* out, socklen_t* outlen) {
	if (len < 2) return -D_EINVAL;
	uint8_t fam = d[1];
	if (fam == 1) {                                   // AF_UNIX: {len, familia, ruta[104]}
		struct sockaddr_un un = { .sun_family = AF_UNIX };
		size_t pl = len - 2 < sizeof un.sun_path - 1 ? len - 2 : sizeof un.sun_path - 1;
		char g[128] = { 0 };
		memcpy(g, d + 2, pl);
		if (g[0] == '/' && tahoe_root) snprintf(un.sun_path, sizeof un.sun_path, "%s%s", tahoe_root, g);
		else memcpy(un.sun_path, g, pl);
		memcpy(out, &un, sizeof un);
		*outlen = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + strlen(un.sun_path) + 1);
		return 0;
	}
	if (fam == 2 && len >= 8) {                       // AF_INET: {len, fam, port, addr, zero[8]}
		struct sockaddr_in in = { .sin_family = AF_INET };
		memcpy(&in.sin_port, d + 2, 2); memcpy(&in.sin_addr, d + 4, 4);
		memcpy(out, &in, sizeof in); *outlen = sizeof in;
		return 0;
	}
	if (fam == 30 && len >= 24) {                     // AF_INET6: {len, fam, port, flowinfo, addr[16], scope}
		struct sockaddr_in6 in6 = { .sin6_family = AF_INET6 };
		memcpy(&in6.sin6_port, d + 2, 2); memcpy(&in6.sin6_flowinfo, d + 4, 4); memcpy(&in6.sin6_addr, d + 8, 16);
		if (len >= 28) memcpy(&in6.sin6_scope_id, d + 24, 4);
		memcpy(out, &in6, sizeof in6); *outlen = sizeof in6;
		return 0;
	}
	return -D_EAFNOSUPPORT;
}
// Linux -> Darwin
static uint32_t sa_out(const uint8_t* l, socklen_t llen, uint8_t* d) {
	uint16_t fam; memcpy(&fam, l, 2);
	memset(d, 0, 128);
	if (fam == AF_UNIX) {
		const char* p = (const char*)l + 2;
		size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
		if (rl && !strncmp(p, tahoe_root, rl)) p += rl;
		size_t pl = llen > 2 ? strnlen(p, llen - 2) : 0;
		d[1] = 1; memcpy(d + 2, p, pl > 103 ? 103 : pl);
		d[0] = (uint8_t)(2 + pl + 1);
		return d[0];
	}
	if (fam == AF_INET && llen >= 8) { d[0] = 16; d[1] = 2; memcpy(d + 2, l + 2, 2); memcpy(d + 4, l + 4, 4); return 16; }
	if (fam == AF_INET6 && llen >= 24) { d[0] = 28; d[1] = 30; memcpy(d + 2, l + 2, 2); memcpy(d + 4, l + 4, 4); memcpy(d + 8, l + 8, 16); if (llen >= 28) memcpy(d + 24, l + 24, 4); return 28; }
	return 0;
}

static int msg_flags(int f) {                         // MSG_* de Darwin -> Linux
	int l = MSG_NOSIGNAL;
	if (f & 0x1) l |= MSG_OOB;
	if (f & 0x2) l |= MSG_PEEK;
	if (f & 0x4) l |= MSG_DONTROUTE;
	if (f & 0x8) l |= MSG_EOR;
	if (f & 0x20) l |= MSG_TRUNC;
	if (f & 0x40) l |= MSG_WAITALL;
	if (f & 0x80) l |= MSG_DONTWAIT;
	return l;
}

// ---- PF_SYSTEM / SYSPROTO_EVENT: socket de eventos del núcleo (kern_event_msg: red, energía, sistema de archivos...).
// configd, mDNSResponder y otros lo abren al arrancar y filtran con SIOCSKEVFILT. Es un extremo de un par de sockets de
// datagramas: la otra punta queda abierta para publicar eventos (hoy ninguno), así que leer espera como en macOS cuando
// nada cambia. Se reconoce por su número de inodo (el descriptor puede reutilizarse tras close).
#include <sys/stat.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#define KEV_MAX_FD 4096
static unsigned long kev_ino[KEV_MAX_FD];
static int kev_peer[KEV_MAX_FD];
static int is_kev(int fd) {
	struct stat st;
	return fd >= 0 && fd < KEV_MAX_FD && kev_ino[fd] && fstat(fd, &st) == 0 && st.st_ino == kev_ino[fd];
}
static long kev_socket(void) {
	int sv[2];
	if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) < 0) return err();
	if (sv[0] >= KEV_MAX_FD) { close(sv[0]); close(sv[1]); return -24; /* EMFILE */ }
	fcntl(sv[1], F_SETFD, FD_CLOEXEC);
	struct stat st; fstat(sv[0], &st);
	if (kev_ino[sv[0]] && kev_peer[sv[0]] > 0) close(kev_peer[sv[0]]);   // un socket de eventos anterior ya cerrado
	kev_ino[sv[0]] = st.st_ino; kev_peer[sv[0]] = sv[1];
	return sv[0];
}
// Códigos de fabricante (SIOCGKEVVENDOR): "com.apple" es KEV_VENDOR_APPLE (1); los demás se numeran desde 1000.
static uint32_t kev_vendor(const char* name) {
	static char names[32][200]; static int n;
	if (!strcmp(name, "com.apple")) return 1;
	for (int i = 0; i < n; i++) if (!strcmp(names[i], name)) return 1000 + (uint32_t)i;
	if (n < 32) { snprintf(names[n], sizeof names[n], "%s", name); return 1000 + (uint32_t)n++; }
	return 0;
}

// ---- Terminales: struct termios de Darwin (flags de 8 bytes, c_cc[20], velocidades) <-> la de Linux. Los bits y los
// índices de c_cc son distintos en cada sistema; se traducen uno a uno.
#include <termios.h>
struct dtermios { uint64_t iflag, oflag, cflag, lflag; uint8_t cc[20]; uint8_t pad[4]; uint64_t ispeed, ospeed; };
struct bitmap { uint64_t d; tcflag_t l; };
static const struct bitmap IMAP[] = { {0x1,IGNBRK},{0x2,BRKINT},{0x4,IGNPAR},{0x8,PARMRK},{0x10,INPCK},{0x20,ISTRIP},{0x40,INLCR},
	{0x80,IGNCR},{0x100,ICRNL},{0x200,IXON},{0x400,IXOFF},{0x800,IXANY},{0x2000,IMAXBEL},{0x4000,IUTF8} };
static const struct bitmap OMAP[] = { {0x1,OPOST},{0x2,ONLCR},{0x4,XTABS},{0x10,OCRNL},{0x20,ONOCR},{0x40,ONLRET} };
static const struct bitmap CMAP[] = { {0x400,CSTOPB},{0x800,CREAD},{0x1000,PARENB},{0x2000,PARODD},{0x4000,HUPCL},{0x8000,CLOCAL} };
static const struct bitmap LMAP[] = { {0x1,ECHOKE},{0x2,ECHOE},{0x4,ECHOK},{0x8,ECHO},{0x10,ECHONL},{0x20,ECHOPRT},{0x40,ECHOCTL},
	{0x80,ISIG},{0x100,ICANON},{0x400,IEXTEN},{0x800,EXTPROC},{0x400000,TOSTOP},{0x800000,FLUSHO},{0x20000000,PENDIN},{0x80000000,NOFLSH} };
// índice de c_cc de Darwin -> de Linux (-1: no existe)
static const int CCMAP[20] = { VEOF, VEOL, VEOL2, VERASE, VWERASE, VKILL, VREPRINT, -1, VINTR, VQUIT, VSUSP, -1, VSTART, VSTOP,
	VLNEXT, VDISCARD, VMIN, VTIME, -1, -1 };
#define NB(a) (sizeof a / sizeof a[0])
static tcflag_t to_l(uint64_t d, const struct bitmap* m, size_t n) { tcflag_t r = 0; for (size_t i = 0; i < n; i++) if (d & m[i].d) r |= m[i].l; return r; }
static uint64_t to_d(tcflag_t l, const struct bitmap* m, size_t n) { uint64_t r = 0; for (size_t i = 0; i < n; i++) if ((l & m[i].l) == m[i].l) r |= m[i].d; return r; }
static void term_to_darwin(const struct termios* t, struct dtermios* d) {
	memset(d, 0, sizeof *d);
	d->iflag = to_d(t->c_iflag, IMAP, NB(IMAP));
	d->oflag = to_d(t->c_oflag, OMAP, NB(OMAP));
	d->cflag = to_d(t->c_cflag, CMAP, NB(CMAP)) | (uint64_t)(((t->c_cflag & CSIZE) >> 4) << 8);   // CS5..CS8
	d->lflag = to_d(t->c_lflag, LMAP, NB(LMAP));
	for (int i = 0; i < 20; i++) d->cc[i] = CCMAP[i] >= 0 ? t->c_cc[CCMAP[i]] : 0xff;              // _POSIX_VDISABLE
	d->ispeed = d->ospeed = 38400;
}
static void term_to_linux(const struct dtermios* d, struct termios* t) {
	speed_t sp = cfgetospeed(t);
	t->c_iflag = to_l(d->iflag, IMAP, NB(IMAP));
	t->c_oflag = to_l(d->oflag, OMAP, NB(OMAP));
	t->c_cflag = (t->c_cflag & CBAUD) | to_l(d->cflag, CMAP, NB(CMAP)) | (tcflag_t)(((d->cflag >> 8) & 3) << 4);
	t->c_lflag = to_l(d->lflag, LMAP, NB(LMAP));
	for (int i = 0; i < 20; i++) if (CCMAP[i] >= 0) t->c_cc[CCMAP[i]] = d->cc[i] == 0xff ? _POSIX_VDISABLE : d->cc[i];
	cfsetospeed(t, sp); cfsetispeed(t, sp);
}
static long tty_ioctl(int fd, uint32_t req, uint64_t arg, int* handled) {
	*handled = 1;
	switch (req) {
	case 0x40487413: {                                        // TIOCGETA
		struct termios t; if (tcgetattr(fd, &t) < 0) return err();
		struct dtermios d; term_to_darwin(&t, &d);
		return safe_write(arg, &d, sizeof d) == (ssize_t)sizeof d ? 0 : -D_EFAULT;
	}
	case 0x80487414: case 0x80487415: case 0x80487416: {      // TIOCSETA / TIOCSETAW / TIOCSETAF
		struct dtermios d; if (safe_read(arg, &d, sizeof d) != (ssize_t)sizeof d) return -D_EFAULT;
		struct termios t; if (tcgetattr(fd, &t) < 0) return err();
		term_to_linux(&d, &t);
		int when = req == 0x80487414 ? TCSANOW : req == 0x80487415 ? TCSADRAIN : TCSAFLUSH;
		return tcsetattr(fd, when, &t) < 0 ? err() : 0;
	}
	case 0x40087468: { struct winsize w; if (ioctl(fd, TIOCGWINSZ, &w) < 0) return err(); return safe_write(arg, &w, 8) == 8 ? 0 : -D_EFAULT; }   // TIOCGWINSZ
	case 0x80087467: { struct winsize w; if (safe_read(arg, &w, 8) != 8) return -D_EFAULT; return ioctl(fd, TIOCSWINSZ, &w) < 0 ? err() : 0; }  // TIOCSWINSZ
	case 0x40047477: { int g = tcgetpgrp(fd); if (g < 0) return err(); return safe_write(arg, &g, 4) == 4 ? 0 : -D_EFAULT; }                    // TIOCGPGRP
	case 0x80047476: { int g; if (safe_read(arg, &g, 4) != 4) return -D_EFAULT; return tcsetpgrp(fd, g) < 0 ? err() : 0; }                    // TIOCSPGRP
	case 0x20007461: return ioctl(fd, TIOCSCTTY, 0) < 0 ? err() : 0;                                                                         // TIOCSCTTY
	case 0x20007471: return ioctl(fd, TIOCNOTTY) < 0 ? err() : 0;                                                                            // TIOCNOTTY
	case 0x40047473: { int n; if (ioctl(fd, TIOCOUTQ, &n) < 0) return err(); return safe_write(arg, &n, 4) == 4 ? 0 : -D_EFAULT; }           // TIOCOUTQ
	case 0x80017472: { char ch; if (safe_read(arg, &ch, 1) != 1) return -D_EFAULT; return ioctl(fd, TIOCSTI, &ch) < 0 ? err() : 0; }        // TIOCSTI
	case 0x4004667a: {                                        // FIODTYPE: tipo de dispositivo (D_TTY = 3)
		if (!isatty(fd)) return -25;
		int t = 3; return safe_write(arg, &t, 4) == 4 ? 0 : -D_EFAULT;
	}
	}
	*handled = 0;
	return 0;
}

// ---- /dev/fsevents: el dispositivo de eventos del sistema de archivos. Se abre un descriptor marcado; FSEVENTS_CLONE
// devuelve el extremo de lectura de una tubería (no llegan eventos: la lectura espera, como en un sistema quieto). Las demás
// ioctl del grupo 's' (filtro de dispositivos, eventos compactos/extendidos, id actual) se aceptan.
#define FSEV_MAX_FD 4096
static uint8_t fsev_fd[FSEV_MAX_FD];
static int fsev_keep[FSEV_MAX_FD];                       // extremo de escritura de cada clon (para que la lectura no vea EOF)
int fsevents_open(void) {
	int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
	if (fd >= 0 && fd < FSEV_MAX_FD) fsev_fd[fd] = 1;
	return fd;
}
static long fsev_ioctl(int fd, uint32_t req, uint64_t arg, int* handled) {
	*handled = 0;
	if (fd < 0 || fd >= FSEV_MAX_FD || !fsev_fd[fd] || ((req >> 8) & 0xff) != 's') return 0;
	*handled = 1;
	if ((req & 0xff) == 1) {                                  // FSEVENTS_CLONE(event_list*, num_events, queue_depth, int* fd)
		uint64_t a[3]; if (safe_read(arg, a, 24) != 24) return -D_EFAULT;
		int p[2]; if (pipe(p) < 0) return err();
		if (p[0] < FSEV_MAX_FD) { fsev_fd[p[0]] = 2; fsev_keep[p[0]] = p[1]; }
		fcntl(p[1], F_SETFD, FD_CLOEXEC);
		int32_t nfd = p[0];
		return safe_write(a[2], &nfd, 4) == 4 ? 0 : -D_EFAULT;
	}
	if ((req & 0xff) == 103) { uint64_t z = 0; return safe_write(arg, &z, 8) == 8 ? 0 : -D_EFAULT; }   // FSEVENTS_GET_CURRENT_ID
	return 0;
}

// ioctl de Darwin: las de FIO* genéricas y las de los sockets de eventos; el resto, ENOTTY (lo correcto para isatty()).
static long net_ioctl(struct ctx* c) {
	int fd = (int)c->a[0];
	uint32_t req = (uint32_t)c->a[1];
	uint64_t arg = c->a[2];
	switch (req) {
	case 0x8004667e: {                                        // FIONBIO
		int on = 0; if (safe_read(arg, &on, 4) != 4) return -D_EFAULT;
		int fl = fcntl(fd, F_GETFL); if (fl < 0) return err();
		return fcntl(fd, F_SETFL, on ? fl | O_NONBLOCK : fl & ~O_NONBLOCK) < 0 ? err() : 0;
	}
	case 0x4004667f: {                                        // FIONREAD
		int v = 0; if (ioctl(fd, FIONREAD, &v) < 0) return err();
		return safe_write(arg, &v, 4) == 4 ? 0 : -D_EFAULT;
	}
	case 0x20006601: return fcntl(fd, F_SETFD, FD_CLOEXEC) < 0 ? err() : 0;   // FIOCLEX
	case 0x20006602: return fcntl(fd, F_SETFD, 0) < 0 ? err() : 0;            // FIONCLEX
	}
	{ int handled; long r = fsev_ioctl(fd, req, arg, &handled); if (handled) return r; }
	{ int handled; long r = tty_ioctl(fd, req, arg, &handled); if (handled) return r; }
	if (is_kev(fd)) {
		switch (req) {
		case 0x40046501: { uint32_t id = 0; return safe_write(arg, &id, 4) == 4 ? 0 : -D_EFAULT; }   // SIOCGKEVID: último id de evento
		case 0x800c6502: return 0;                                                                  // SIOCSKEVFILT: filtro aceptado
		case 0x400c6503: { uint32_t f[3] = { 0, 0, 0 }; return safe_write(arg, f, 12) == 12 ? 0 : -D_EFAULT; }   // SIOCGKEVFILT
		case 0xc0cc6504: {                                                                          // SIOCGKEVVENDOR
			char v[204] = { 0 };
			if (safe_read(arg, v, sizeof v) != (ssize_t)sizeof v) return -D_EFAULT;
			v[203] = 0;
			uint32_t code = kev_vendor(v + 4);
			if (!code) return -2;                                                                   // ENOENT
			memcpy(v, &code, 4);
			return safe_write(arg, v, 4) == 4 ? 0 : -D_EFAULT;
		}
		}
	}
	if (((req >> 8) & 0xff) == 'i') {                         // ioctl de interfaces de red sobre un socket (struct ifreq / in6_ifreq)
		struct stat st;
		if (fstat(fd, &st) == 0 && S_ISSOCK(st.st_mode)) {
			unsigned nr = req & 0xff, len = (req >> 16) & 0x1fff;
			if ((nr == 73 || nr == 76 || nr == 81) && len > 16 && len <= 512) {   // SIOCGIFAFLAG_IN6 / SIOCGIFINFO_IN6 / SIOCGIFALIFETIME_IN6: vacíos
				uint8_t z[512] = { 0 };
				return safe_write(arg + 16, z, len - 16) == (ssize_t)(len - 16) ? 0 : -D_EFAULT;
			}
			return -2;                                        // ENOENT: sin esa configuración (CLAT46, NAT64...)
		}
	}
	if (trace_all) logf_("    ioctl(fd=%d, 0x%x) sin traducir\n", fd, req);
	return -25;
}

// ---- ICMP sin privilegios (ping): en Linux SOCK_RAW exige CAP_NET_RAW; se usa el socket "ping" (SOCK_DGRAM,
// IPPROTO_ICMP). Diferencias que se ocultan: macOS entrega la cabecera IP delante del ICMP recibido, y Linux reescribe el
// identificador del eco con su puerto. Al recibir se restaura el identificador enviado y se antepone una cabecera IPv4.
#define ICMP_MAX_FD 4096
static int icmp_emul[ICMP_MAX_FD];
static uint16_t icmp_id[ICMP_MAX_FD];
static int is_icmp(int fd) { return fd >= 0 && fd < ICMP_MAX_FD && icmp_emul[fd]; }
static uint16_t csum(const uint8_t* p, size_t n) { uint32_t s = 0; for (size_t i = 0; i + 1 < n; i += 2) s += (uint32_t)(p[i] << 8 | p[i + 1]); if (n & 1) s += (uint32_t)p[n - 1] << 8; while (s >> 16) s = (s & 0xffff) + (s >> 16); return (uint16_t)~s; }
static void icmp_note_send(int fd, const uint8_t* buf, size_t len) { if (is_icmp(fd) && len >= 8) memcpy(&icmp_id[fd], buf + 4, 2); }
// paquete ICMP recibido (in, n) de src -> cabecera IPv4 + ICMP con el id original; devuelve la longitud
static size_t icmp_fix(int fd, const uint8_t* in, size_t n, const struct sockaddr_in* src, uint8_t* out) {
	uint8_t* ip = out; memset(ip, 0, 20);
	ip[0] = 0x45; uint16_t tl = (uint16_t)(20 + n); ip[2] = tl >> 8; ip[3] = tl & 0xff; ip[8] = 64; ip[9] = 1;
	if (src) memcpy(ip + 12, &src->sin_addr, 4);
	uint16_t hc = csum(ip, 20); ip[10] = hc >> 8; ip[11] = hc & 0xff;
	memcpy(out + 20, in, n);
	uint8_t* ic = out + 20;
	if (n >= 8) { memcpy(ic + 4, &icmp_id[fd], 2); ic[2] = ic[3] = 0; uint16_t cc = csum(ic, n); ic[2] = cc >> 8; ic[3] = cc & 0xff; }
	return 20 + n;
}

static long net_socket(struct ctx* c) {
	if ((int)c->a[0] == 32 && (int)c->a[2] == 1) return kev_socket();   // PF_SYSTEM, SYSPROTO_EVENT
	int dom = dom_to_linux((int)c->a[0]);
	if (dom != AF_UNIX && dom != AF_INET && dom != AF_INET6) return -D_EAFNOSUPPORT;
	int type = (int)c->a[1] & 0xff;
	long r = syscall(SYS_socket, dom, type | SOCK_CLOEXEC * 0, (int)c->a[2]);
	if (r < 0 && errno == EPERM && dom == AF_INET && type == SOCK_RAW && (int)c->a[2] == IPPROTO_ICMP) {
		r = syscall(SYS_socket, AF_INET, SOCK_DGRAM, IPPROTO_ICMP);   // ping sin privilegios
		if (r >= 0 && r < ICMP_MAX_FD) { icmp_emul[r] = 1; icmp_id[r] = 0; }
	} else if (r >= 0 && r < ICMP_MAX_FD) icmp_emul[r] = 0;
	return r < 0 ? err() : r;
}
static long net_socketpair(struct ctx* c) {
	int fds[2];
	if (socketpair(dom_to_linux((int)c->a[0]), (int)c->a[1], (int)c->a[2], fds) < 0) return err();
	return safe_write(c->a[3], fds, 8) == 8 ? 0 : -D_EFAULT;
}
static long net_bind(struct ctx* c) {
	uint8_t sa[128], ls[128]; socklen_t ll;
	uint32_t len = (uint32_t)c->a[2];
	if (len > sizeof sa || safe_read(c->a[1], sa, len) != (ssize_t)len) return -D_EFAULT;
	int e = sa_in(sa, len, ls, &ll);
	if (e) return e;
	long r = bind((int)c->a[0], (struct sockaddr*)ls, ll);
	if (trace_all || r < 0) logf_("    bind(%s) -> %s\n", ((struct sockaddr_un*)ls)->sun_family == AF_UNIX ? ((struct sockaddr_un*)ls)->sun_path : "inet", r < 0 ? strerror(errno) : "ok");
	return r < 0 ? err() : 0;
}
static long net_connect(struct ctx* c) {
	uint8_t sa[128], ls[128]; socklen_t ll;
	uint32_t len = (uint32_t)c->a[2];
	if (len > sizeof sa || safe_read(c->a[1], sa, len) != (ssize_t)len) return -D_EFAULT;
	int e = sa_in(sa, len, ls, &ll);
	if (e) return e;
	long r = connect((int)c->a[0], (struct sockaddr*)ls, ll);
	if (trace_all || (r < 0 && errno != EINPROGRESS)) logf_("    connect(%s) -> %s\n", ((struct sockaddr_un*)ls)->sun_family == AF_UNIX ? ((struct sockaddr_un*)ls)->sun_path : "inet", r < 0 ? strerror(errno) : "ok");
	return r < 0 ? err() : 0;
}
static long net_listen(struct ctx* c) { long r = listen((int)c->a[0], (int)c->a[1]); return r < 0 ? err() : 0; }
static long write_name(uint64_t nameaddr, uint64_t lenaddr, const uint8_t* l, socklen_t ll) {
	if (!nameaddr || !lenaddr) return 0;
	uint8_t d[128]; uint32_t dl = sa_out(l, ll, d), cap = 0;
	safe_read(lenaddr, &cap, 4);
	if (dl && safe_write(nameaddr, d, dl < cap ? dl : cap) < 0) return -D_EFAULT;
	safe_write(lenaddr, &dl, 4);
	return 0;
}
static long net_accept(struct ctx* c) {
	uint8_t l[128]; socklen_t ll = sizeof l;
	long r = accept((int)c->a[0], (struct sockaddr*)l, &ll);
	if (r < 0) return err();
	write_name(c->a[1], c->a[2], l, ll);
	return r;
}
static long net_getsockname(struct ctx* c) {
	uint8_t l[128]; socklen_t ll = sizeof l;
	if (getsockname((int)c->a[0], (struct sockaddr*)l, &ll) < 0) return err();
	return write_name(c->a[1], c->a[2], l, ll);
}
static long net_getpeername(struct ctx* c) {
	uint8_t l[128]; socklen_t ll = sizeof l;
	if (getpeername((int)c->a[0], (struct sockaddr*)l, &ll) < 0) return err();
	return write_name(c->a[1], c->a[2], l, ll);
}
static long net_shutdown(struct ctx* c) { long r = shutdown((int)c->a[0], (int)c->a[1]); return r < 0 ? err() : 0; }

// setsockopt/getsockopt: SOL_SOCKET de Darwin es 0xffff; las opciones usan otros números.
static int opt_to_linux(int o) {
	switch (o) {
	case 0x4: return SO_REUSEADDR; case 0x8: return SO_KEEPALIVE; case 0x10: return SO_DONTROUTE; case 0x20: return SO_BROADCAST;
	case 0x80: return SO_LINGER; case 0x100: return SO_OOBINLINE; case 0x200: return SO_REUSEPORT;
	case 0x1001: return SO_SNDBUF; case 0x1002: return SO_RCVBUF; case 0x1005: return SO_SNDTIMEO; case 0x1006: return SO_RCVTIMEO;
	case 0x1007: return SO_ERROR; case 0x1008: return SO_TYPE; case 0x1009: return SO_PEERCRED * 0 - 1;
	default: return -1;
	}
}
static long net_setsockopt(struct ctx* c) {
	int fd = (int)c->a[0], level = (int)c->a[1], opt = (int)c->a[2];
	uint8_t v[64]; uint32_t len = (uint32_t)c->a[4];
	if (len > sizeof v || (len && safe_read(c->a[3], v, len) != (ssize_t)len)) return -D_EFAULT;
	if (level == 0xffff) {
		if (opt == 0x1022) return 0;                                  // SO_NOSIGPIPE: ya se usa MSG_NOSIGNAL
		int lo = opt_to_linux(opt);
		if (lo < 0) return 0;                                         // opción sin equivalente: se acepta
		if ((opt == 0x1005 || opt == 0x1006) && len >= 16) {          // timeval de Darwin {i64, i32, pad}
			struct timeval tv; int64_t s; int32_t us; memcpy(&s, v, 8); memcpy(&us, v + 8, 4); tv.tv_sec = s; tv.tv_usec = us;
			return setsockopt(fd, SOL_SOCKET, lo, &tv, sizeof tv) < 0 ? err() : 0;
		}
		return setsockopt(fd, SOL_SOCKET, lo, v, len) < 0 ? err() : 0;
	}
	if (level == 0 /*IPPROTO_IP*/ || level == 6 /*TCP*/ || level == 41) return 0;   // ajustes de red: se aceptan sin efecto
	return -D_ENOPROTOOPT;
}
static long net_getsockopt(struct ctx* c) {
	int fd = (int)c->a[0], level = (int)c->a[1], opt = (int)c->a[2];
	uint32_t cap = 0;
	if (safe_read(c->a[4], &cap, 4) != 4) return -D_EFAULT;
	uint8_t v[64] = { 0 };
	socklen_t l = sizeof v;
	if (level == 0xffff) {
		int lo = opt_to_linux(opt);
		if (opt == 0x1007) {                                          // SO_ERROR: errno traducido a Darwin
			int e = 0; socklen_t el = sizeof e;
			if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &e, &el) < 0) return err();
			e = e ? darwin_errno(e) : 0;
			memcpy(v, &e, 4); l = 4;
		} else if (lo < 0 || getsockopt(fd, SOL_SOCKET, lo, v, &l) < 0) { if (lo >= 0) return err(); memset(v, 0, 4); l = 4; }
	} else { memset(v, 0, 4); l = 4; }
	uint32_t n = l < cap ? l : cap;
	if (n && safe_write(c->a[3], v, n) < 0) return -D_EFAULT;
	safe_write(c->a[4], &n, 4);
	return 0;
}

static long net_sendto(struct ctx* c) {              // (fd, buf, len, flags, to*, tolen)
	uint8_t sa[128], ls[128]; socklen_t ll = 0;
	void* tp = NULL;
	if (c->a[4] && c->a[5]) {
		uint32_t len = (uint32_t)c->a[5];
		if (len > sizeof sa || safe_read(c->a[4], sa, len) != (ssize_t)len) return -D_EFAULT;
		int e = sa_in(sa, len, ls, &ll);
		if (e) return e;
		tp = ls;
	}
	icmp_note_send((int)c->a[0], (const uint8_t*)c->a[1], c->a[2]);
	long r = sendto((int)c->a[0], (const void*)c->a[1], c->a[2], msg_flags((int)c->a[3]), tp, ll);
	return r < 0 ? err() : r;
}
static long net_recvfrom(struct ctx* c) {            // (fd, buf, len, flags, from*, fromlen*)
	uint8_t l[128]; socklen_t ll = sizeof l;
	if (is_icmp((int)c->a[0])) {
		uint8_t in[65536], out[65536 + 20];
		long r = recvfrom((int)c->a[0], in, sizeof in, msg_flags((int)c->a[3]) & ~MSG_NOSIGNAL, (struct sockaddr*)l, &ll);
		if (r < 0) return err();
		size_t n = icmp_fix((int)c->a[0], in, (size_t)r, (struct sockaddr_in*)l, out);
		if (n > c->a[2]) n = c->a[2];
		if (safe_write(c->a[1], out, n) != (ssize_t)n) return -D_EFAULT;
		if (c->a[4]) write_name(c->a[4], c->a[5], l, ll);
		return (long)n;
	}
	long r = recvfrom((int)c->a[0], (void*)c->a[1], c->a[2], msg_flags((int)c->a[3]) & ~MSG_NOSIGNAL, c->a[4] ? (struct sockaddr*)l : NULL, c->a[4] ? &ll : NULL);
	if (r < 0) return err();
	if (c->a[4]) write_name(c->a[4], c->a[5], l, ll);
	return r;
}

// sendmsg/recvmsg: msghdr de Darwin = {name*, namelen u32, iov*, iovlen i32, control*, controllen u32, flags i32}; los
// mensajes de control (cmsghdr {len u32, level i32, type i32}) solo se traducen para SCM_RIGHTS.
struct dmsghdr { uint64_t name; uint32_t namelen; uint32_t pad0; uint64_t iov; int32_t iovlen; int32_t pad1; uint64_t control; uint32_t controllen; int32_t flags; };
static long net_sendmsg(struct ctx* c) {
	struct dmsghdr d;
	if (safe_read(c->a[1], &d, sizeof d) != sizeof d) return -D_EFAULT;
	struct msghdr m = { 0 };
	uint8_t ls[128], sa[128]; socklen_t ll = 0;
	if (d.name && d.namelen) {
		if (d.namelen > sizeof sa || safe_read(d.name, sa, d.namelen) != (ssize_t)d.namelen) return -D_EFAULT;
		int e = sa_in(sa, d.namelen, ls, &ll);
		if (e) return e;
		m.msg_name = ls; m.msg_namelen = ll;
	}
	m.msg_iov = (struct iovec*)d.iov; m.msg_iovlen = (size_t)d.iovlen;
	uint8_t cbuf[256] = { 0 };
	if (d.control && d.controllen >= 12) {
		uint8_t dc[256];
		uint32_t cl = d.controllen < sizeof dc ? d.controllen : sizeof dc;
		if (safe_read(d.control, dc, cl) != (ssize_t)cl) return -D_EFAULT;
		uint32_t clen; int32_t lvl, typ; memcpy(&clen, dc, 4); memcpy(&lvl, dc + 4, 4); memcpy(&typ, dc + 8, 4);
		if (lvl == 0xffff && typ == 1 && clen >= 12) {                           // SCM_RIGHTS
			size_t dl = clen - 12;
			struct cmsghdr* ch = (struct cmsghdr*)cbuf;
			ch->cmsg_len = CMSG_LEN(dl); ch->cmsg_level = SOL_SOCKET; ch->cmsg_type = SCM_RIGHTS;
			memcpy(CMSG_DATA(ch), dc + 12, dl);
			m.msg_control = cbuf; m.msg_controllen = CMSG_SPACE(dl);
		}
	}
	if (is_icmp((int)c->a[0]) && d.iovlen >= 1) { uint64_t io[2]; if (safe_read(d.iov, io, 16) == 16) { uint8_t h[8]; if (io[1] >= 8 && safe_read(io[0], h, 8) == 8) icmp_note_send((int)c->a[0], h, 8); } }
	long r = sendmsg((int)c->a[0], &m, msg_flags((int)c->a[2]));
	return r < 0 ? err() : r;
}
static long net_recvmsg(struct ctx* c) {
	struct dmsghdr d;
	if (safe_read(c->a[1], &d, sizeof d) != sizeof d) return -D_EFAULT;
	struct msghdr m = { 0 };
	uint8_t l[128], cbuf[256];
	m.msg_name = d.name ? l : NULL; m.msg_namelen = d.name ? sizeof l : 0;
	m.msg_iov = (struct iovec*)d.iov; m.msg_iovlen = (size_t)d.iovlen;
	m.msg_control = d.control ? cbuf : NULL; m.msg_controllen = d.control ? sizeof cbuf : 0;
	static __thread uint8_t icin[65536], icout[65536 + 20];
	struct iovec iv1 = { icin, sizeof icin };
	int icmp = is_icmp((int)c->a[0]);
	if (icmp) { m.msg_iov = &iv1; m.msg_iovlen = 1; }
	long r = recvmsg((int)c->a[0], &m, msg_flags((int)c->a[2]) & ~MSG_NOSIGNAL);
	if (r < 0) return err();
	if (icmp) {                                                  // cabecera IP delante y el id original; se reparte en los iov del programa
		size_t n = icmp_fix((int)c->a[0], icin, (size_t)r, (struct sockaddr_in*)l, icout), done = 0;
		for (int i = 0; i < d.iovlen && done < n; i++) {
			uint64_t io[2]; if (safe_read(d.iov + (uint64_t)i * 16, io, 16) != 16) break;
			size_t k = n - done < io[1] ? n - done : io[1];
			if (safe_write(io[0], icout + done, k) != (ssize_t)k) break;
			done += k;
		}
		r = (long)done;
	}
	if (d.name) { uint8_t dd[128]; uint32_t dl = sa_out(l, m.msg_namelen, dd); if (dl) safe_write(d.name, dd, dl < d.namelen ? dl : d.namelen); d.namelen = dl; }
	uint32_t used = 0;
	if (d.control) {
		for (struct cmsghdr* ch = CMSG_FIRSTHDR(&m); ch; ch = CMSG_NXTHDR(&m, ch)) {
			if (ch->cmsg_level == SOL_SOCKET && ch->cmsg_type == SCM_RIGHTS) {
				size_t dl = ch->cmsg_len - CMSG_LEN(0);
				uint8_t o[256]; uint32_t clen = (uint32_t)(12 + dl); int32_t lvl = 0xffff, typ = 1;
				if (used + ((clen + 3) & ~3u) > d.controllen) break;
				memcpy(o, &clen, 4); memcpy(o + 4, &lvl, 4); memcpy(o + 8, &typ, 4); memcpy(o + 12, CMSG_DATA(ch), dl);
				safe_write(d.control + used, o, clen);
				used += (clen + 3) & ~3u;
			}
		}
	}
	d.controllen = used;
	int fl = 0;
	if (m.msg_flags & MSG_TRUNC) fl |= 0x10; if (m.msg_flags & MSG_CTRUNC) fl |= 0x20; if (m.msg_flags & MSG_EOR) fl |= 0x8;
	d.flags = fl;
	safe_write(c->a[1], &d, sizeof d);
	return r;
}

// pathconf / fpathconf: valores típicos de APFS
static long pc_value(int name) {
	switch (name) {
	case 1: return 32767; case 2: return 1024; case 3: return 1024; case 4: return 255; case 5: return 1024; case 6: return 512;
	case 7: return 1; case 8: return 1; case 9: return 255; case 18: return 64; case 13: return 1; case 14: return 1; case 15: return 1;
	default: return -1;
	}
}
static long net_pathconf(struct ctx* c) { long v = pc_value((int)c->a[1]); return v < 0 ? -D_EINVAL : v; }

// coalition(op, id*, tipo, flags): solo se emula la creación (id sintético) y la consulta
static long net_coalition(struct ctx* c) {
	static uint64_t next = 100;
	if ((int)c->a[0] == 1 /* CREATE */) { uint64_t id = ++next; if (c->a[1]) safe_write(c->a[1], &id, 8); return 0; }
	return 0;
}

void emu_net_init(void) {
	reg_bsd(97, net_socket); reg_bsd(54, net_ioctl); reg_bsd(98, net_connect); reg_bsd(409, net_connect);
	reg_bsd(104, net_bind); reg_bsd(106, net_listen); reg_bsd(30, net_accept); reg_bsd(404, net_accept);
	reg_bsd(32, net_getsockname); reg_bsd(31, net_getpeername); reg_bsd(134, net_shutdown); reg_bsd(135, net_socketpair);
	reg_bsd(105, net_setsockopt); reg_bsd(118, net_getsockopt);
	reg_bsd(133, net_sendto); reg_bsd(413, net_sendto); reg_bsd(29, net_recvfrom); reg_bsd(403, net_recvfrom);
	reg_bsd(28, net_sendmsg); reg_bsd(402, net_sendmsg); reg_bsd(27, net_recvmsg); reg_bsd(401, net_recvmsg);
	reg_bsd(191, net_pathconf); reg_bsd(192, net_pathconf);
	reg_bsd(458, net_coalition); reg_bsd(459, net_coalition); reg_bsd(532, net_coalition);   // coalition, coalition_info, coalition_ledger
}
