// Procesos, hilos y señales: bsdthread_register, sigaction, etc.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>

#include <pthread.h>
#include <spawn.h>
#include <poll.h>
#include <stdatomic.h>
#include <setjmp.h>
#include <sys/mman.h>
#include <stdlib.h>
#include <dirent.h>
#include <stddef.h>
#include "tahoe.h"
__thread struct ctx* volatile t_cur;   // llamada en curso del hilo (la pone el manejador de SIGSYS)

// bsdthread_register(thread_start, wqthread, pthread_size, ...): libpthread registra aquí las funciones con las
// que el kernel arranca hilos nuevos. Se guardan y se devuelve la máscara de capacidades.
uint64_t thread_start_fn, wqthread_fn, pthread_size;
static uint32_t tsd_offset, mach_thread_self_offset;
uint64_t dispatch_queue_offset;   // desplazamiento del TSD dentro de pthread_t (struct _pthread_registration_data, +24)
// work_interval_ctl(operación, id, arg, longitud): intervalos de trabajo del planificador (audio/gráficos en tiempo real).
// No hay planificador que informar: se crean intervalos con un puerto propio y el resto de operaciones se aceptan.
static long bsd_work_interval_ctl(struct ctx* c) {
	static uint64_t next_id = 1;
	switch ((uint32_t)c->a[0]) {
	case 4: {                                                 // CREATE2: {id (entrada/salida), puerto, banderas}
		uint8_t p[16] = { 0 };
		if (safe_read(c->a[2], p, 16) != 16) return -D_EFAULT;
		uint64_t id = next_id++; uint32_t port = alloc_port();
		memcpy(p, &id, 8); memcpy(p + 8, &port, 4);
		return safe_write(c->a[2], p, 16) == 16 ? 0 : -D_EFAULT;
	}
	case 9: { if (c->a[2] && c->a[3] >= 4) { uint32_t z = 0; safe_write(c->a[2], &z, 4); } return 0; }   // GET_FLAGS
	default: return 0;                                        // DESTROY, NOTIFY, JOIN, SET_NAME, SET_WORKLOAD_ID
	}
}
static long bsd_bsdthread_register(struct ctx* c) {
	thread_start_fn = c->a[0];
	wqthread_fn = c->a[1];
	pthread_size = c->a[2];
	if (c->a[3]) {
		safe_read(c->a[3] + 24, &tsd_offset, 4); safe_read(c->a[3] + 32, &mach_thread_self_offset, 4);
		dispatch_queue_offset = c->a[5];             // TSD de la cola de dispatch actual (thread_info IDENTIFIER: dispatch_qaddr)
		// mutex_default_policy (+44): mutex y variables de condición basados en ulock (futex) en vez de psynch, que no se emula.
		// 0x100 = _PTHREAD_REG_DEFAULT_USE_ULOCK; los 8 bits bajos son la política (3 = primer ajuste).
		uint32_t pol = 0x100 | 3;
		safe_write(c->a[3] + 44, &pol, 4);
	}
	if (trace_all) logf_("    bsdthread_register: tsd_offset=0x%x mts_offset=0x%x\n", tsd_offset, mach_thread_self_offset);
	if (trace_all) logf_("    bsdthread_register: start=0x%lx wq=0x%lx pthread_size=0x%lx\n", thread_start_fn, wqthread_fn, pthread_size);
	// Máscara de capacidades del kernel: libpthread exige FINEPRIO|BSDTHREADCTL|SETSELF|QOS_MAINTENANCE y
	// QOS_DEFAULT; sin ella nunca marca el kernel como compatible y aborta con "has not been initialized".
	return 0x400000df;   // DISPATCHFUNC|FINEPRIO|BSDTHREADCTL|SETSELF|QOS_MAINTENANCE|KEVENT|WORKLOOP|QOS_DEFAULT
}


#include <errno.h>
#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <sys/time.h>
#include <time.h>

// gettimeofday(timeval*, timezone*, mach_absolute_time*): struct timeval de Darwin = {int64 seg, int32 microseg, pad}
static long bsd_gettimeofday(struct ctx* c) {
	struct timespec ts;
	if (c->a[0]) {
		clock_gettime(CLOCK_REALTIME, &ts);
		int64_t tv[2] = { ts.tv_sec, ts.tv_nsec / 1000 };
		if (safe_write(c->a[0], tv, 16) != 16) return -D_EFAULT;
	}
	if (c->a[1]) { int32_t tz[2] = { 0, 0 }; safe_write(c->a[1], tz, 8); }
	if (c->a[2]) {                                       // tercer argumento: mach_absolute_time (ya en ns)
		clock_gettime(CLOCK_MONOTONIC, &ts);
		uint64_t t = (uint64_t)ts.tv_sec * 1000000000UL + ts.tv_nsec;
		safe_write(c->a[2], &t, 8);
	}
	return 0;
}
// shm_open: sin memoria compartida con nombre; los clientes (libnotify...) tienen ruta alternativa.
// shm_open/shm_unlink: objetos de memoria compartida POSIX como archivos de /dev/shm con prefijo propio y nombre saneado.
static int shm_host_name(uint64_t addr, char* out, size_t cap) {
	char g[256];
	(void)port_exists(0);                       // fuerza la creación/apertura de la región compartida (fija TAHOE_PORTS_OWNER)
	if (safe_string(addr, g, sizeof g) != 0) return -1;
	char* o = out + snprintf(out, cap, "/dev/shm/tahoe.%s.", getenv("TAHOE_PORTS_OWNER") ? getenv("TAHOE_PORTS_OWNER") : "x");
	for (char* s = g; *s && o < out + cap - 1; s++) *o++ = (*s == '/') ? '_' : *s;
	*o = 0;
	return 0;
}
static long bsd_shm_open(struct ctx* c) {
	char h[400];
	if (shm_host_name(c->a[0], h, sizeof h)) return -D_EFAULT;
	int fd = open(h, darwin_open_flags((int)c->a[1]) | O_CLOEXEC, (mode_t)c->a[2]);
	if (trace_all) logf_("    shm_open -> %s = %d\n", h, fd);
	return fd < 0 ? -darwin_errno(errno) : fd;
}
static long bsd_shm_unlink(struct ctx* c) {
	char h[400];
	if (shm_host_name(c->a[0], h, sizeof h)) return -D_EFAULT;
	return unlink(h) ? -darwin_errno(errno) : 0;
}
// ioctl: no se traduce ninguna petición todavía; 25 = ENOTTY ("no es un terminal"), lo correcto para isatty().
static long bsd_ioctl(struct ctx* c) {
	if (trace_all) logf_("    ioctl(fd=%d, 0x%lx)\n", (int)c->a[0], c->a[1]);
	return -25;
}

#include <sys/socket.h>
// socket(dominio, tipo, protocolo): AF_INET6 vale 30 en Darwin y 10 en Linux; el resto coincide.
static long bsd_socket(struct ctx* c) {
	int dom = (int)c->a[0];
	if (dom == 30) dom = AF_INET6;
	long r = syscall(SYS_socket, dom, (int)c->a[1], (int)c->a[2]);
	return r < 0 ? -darwin_errno(errno) : r;
}

// connect: solo sockets de dominio Unix. sockaddr_un de Darwin = {len u8, familia u8, ruta[104]}; en Linux {familia u16, ruta[108]}.
static long bsd_connect(struct ctx* c) {
	uint8_t sa[128];
	uint32_t len = (uint32_t)c->a[2];
	if (len < 2 || len > sizeof sa || safe_read(c->a[1], sa, len) != (ssize_t)len) return -D_EFAULT;
	if (sa[1] != 1) return -47;                          // solo AF_UNIX; el resto: EAFNOSUPPORT
	struct sockaddr_un_ { uint16_t fam; char path[108]; } un = { 1, { 0 } };
	size_t pl = len - 2 < sizeof un.path - 1 ? len - 2 : sizeof un.path - 1;
	memcpy(un.path, sa + 2, pl);
	long r = syscall(SYS_connect, (int)c->a[0], &un, 2 + pl + 1);
	if (trace_all || r < 0) logf_("    connect(\"%s\") -> %s\n", un.path, r < 0 ? strerror(errno) : "ok");
	return r < 0 ? -darwin_errno(errno) : 0;
}
#include <sys/resource.h>
#include <sys/stat.h>
#include <signal.h>

// ---------------------------------------------------------------- identidad de proceso
static long bsd_getppid(struct ctx* c) { (void)c; return getppid(); }
static long bsd_getpgrp(struct ctx* c) { (void)c; return getpgrp(); }
static long bsd_getpgid(struct ctx* c) { long r = syscall(SYS_getpgid, (int)c->a[0]); return r < 0 ? -darwin_errno(errno) : r; }
static long bsd_setpgid(struct ctx* c) { long r = syscall(SYS_setpgid, (int)c->a[0], (int)c->a[1]); return r < 0 ? -darwin_errno(errno) : 0; }
static long bsd_getsid(struct ctx* c)  { long r = syscall(SYS_getsid, (int)c->a[0]); return r < 0 ? -darwin_errno(errno) : r; }
static long bsd_setsid(struct ctx* c)  { (void)c; long r = syscall(SYS_setsid); return r < 0 ? -darwin_errno(errno) : r; }
static long bsd_umask(struct ctx* c)   { return (long)umask((mode_t)c->a[0]); }

// ---------------------------------------------------------------- límites de recursos
// Darwin: NOFILE=8, NPROC=7, MEMLOCK=6, RSS/AS=5 (más la marca POSIX 0x1000); Linux los numera distinto.
static int linux_rlimit(int darwin) {
	switch (darwin & 0xfff) {
	case 0: return RLIMIT_CPU;     case 1: return RLIMIT_FSIZE;  case 2: return RLIMIT_DATA;  case 3: return RLIMIT_STACK;
	case 4: return RLIMIT_CORE;    case 5: return RLIMIT_AS;     case 6: return RLIMIT_MEMLOCK;
	case 7: return RLIMIT_NPROC;   case 8: return RLIMIT_NOFILE; default: return -1;
	}
}
#define D_RLIM_INFINITY 0x7fffffffffffffffULL
static long bsd_getrlimit(struct ctx* c) {
	int r = linux_rlimit((int)c->a[0]);
	if (r < 0) return -D_EINVAL;
	struct rlimit rl;
	if (getrlimit(r, &rl) != 0) return -darwin_errno(errno);
	uint64_t out[2] = { rl.rlim_cur == RLIM_INFINITY ? D_RLIM_INFINITY : rl.rlim_cur, rl.rlim_max == RLIM_INFINITY ? D_RLIM_INFINITY : rl.rlim_max };
	if (((int)c->a[0] & 0xfff) == 8 && out[0] > 10240 && !(c->a[0] & 0x1000)) out[0] = 10240;   // sin la marca POSIX, macOS limita NOFILE
	return safe_write(c->a[1], out, 16) == 16 ? 0 : -D_EFAULT;
}
// getrusage(who, struct rusage*): RUSAGE_SELF (0) y RUSAGE_CHILDREN (-1) valen lo mismo en Linux, y la struct rusage de
// Darwin x86_64 ocupa y ordena igual que la de Linux (dos timeval de 16 bytes: en Darwin tv_usec es int32 con relleno y,
// como es menor que 10^6, sus bytes coinciden; después 14 long). La usan los snmp* (net-snmp) al arrancar.
// setitimer / getitimer (83 / 86): los temporizadores ITIMER_REAL/VIRTUAL/PROF (mismos números en Darwin y Linux) son los
// de Linux; SIGALRM llega como cualquier otra señal. struct itimerval de Darwin: dos timeval {long tv_sec; int tv_usec}
// de 16 bytes (los 4 bytes de relleno se ignoran). Sin ellos, alarm() (p. ej. perl -e 'alarm 10') no avisaba nunca.
static void itv_in(const uint8_t* b, struct itimerval* v) {
	int64_t s0, s1; int32_t u0, u1;
	memcpy(&s0, b, 8); memcpy(&u0, b + 8, 4); memcpy(&s1, b + 16, 8); memcpy(&u1, b + 24, 4);
	v->it_interval.tv_sec = s0; v->it_interval.tv_usec = u0; v->it_value.tv_sec = s1; v->it_value.tv_usec = u1;
}
static void itv_out(uint8_t* b, const struct itimerval* v) {
	memset(b, 0, 32);
	int64_t s0 = v->it_interval.tv_sec, s1 = v->it_value.tv_sec; int32_t u0 = (int32_t)v->it_interval.tv_usec, u1 = (int32_t)v->it_value.tv_usec;
	memcpy(b, &s0, 8); memcpy(b + 8, &u0, 4); memcpy(b + 16, &s1, 8); memcpy(b + 24, &u1, 4);
}
static long bsd_setitimer(struct ctx* c) {
	int which = (int)c->a[0];
	if (which < 0 || which > 2) return -D_EINVAL;
	struct itimerval nv = { 0 }, ov;
	if (c->a[1]) { uint8_t b[32]; if (safe_read(c->a[1], b, 32) != 32) return -D_EFAULT; itv_in(b, &nv); }
	if (setitimer(which, &nv, &ov) != 0) return -D_EINVAL;
	if (c->a[2]) { uint8_t b[32]; itv_out(b, &ov); if (safe_write(c->a[2], b, 32) != 32) return -D_EFAULT; }
	return 0;
}
static long bsd_getitimer(struct ctx* c) {
	int which = (int)c->a[0];
	if (which < 0 || which > 2) return -D_EINVAL;
	struct itimerval v; if (getitimer(which, &v) != 0) return -D_EINVAL;
	uint8_t b[32]; itv_out(b, &v);
	return safe_write(c->a[1], b, 32) == 32 ? 0 : -D_EFAULT;
}
static long bsd_getrusage(struct ctx* c) {
	int who = (int)c->a[0];
	if (who != 0 && who != -1) return -D_EINVAL;
	struct rusage ru;
	if (getrusage(who == 0 ? RUSAGE_SELF : RUSAGE_CHILDREN, &ru) != 0) return -darwin_errno(errno);
	_Static_assert(sizeof ru == 144, "struct rusage de Linux x86_64");
	return safe_write(c->a[1], &ru, sizeof ru) == (ssize_t)sizeof ru ? 0 : -D_EFAULT;
}
// memorystatus_get_level(int* nivel): porcentaje de memoria disponible (0-100), como el memorystatus_level de XNU. Aquí,
// MemAvailable / MemTotal de Linux. Lo usa memory_pressure.
static long bsd_memorystatus_get_level(struct ctx* c) {
	unsigned long long tot = 0, av = 0; char l[128];
	FILE* f = fopen("/proc/meminfo", "r");
	if (f) {
		while (fgets(l, sizeof l, f)) { sscanf(l, "MemTotal: %llu", &tot); sscanf(l, "MemAvailable: %llu", &av); }
		fclose(f);
	}
	int32_t lvl = tot ? (int32_t)(av * 100 / tot) : 0;
	return safe_write(c->a[0], &lvl, 4) == 4 ? 0 : -D_EFAULT;
}
static long bsd_setrlimit(struct ctx* c) {
	int r = linux_rlimit((int)c->a[0]);
	uint64_t in[2];
	if (r < 0 || safe_read(c->a[1], in, 16) != 16) return -D_EINVAL;
	struct rlimit rl = { in[0] == D_RLIM_INFINITY ? RLIM_INFINITY : in[0], in[1] == D_RLIM_INFINITY ? RLIM_INFINITY : in[1] };
	// Los límites que fija launchd para cada servicio (NumberOfFiles, AS, DATA...) no deben bajar los del anfitrión: tahoe-run
	// necesita descriptores altos y mucho espacio de direcciones para relanzarse, y un servicio sin ellos moría con exit(111).
	if (r == RLIMIT_NOFILE) {
		// por encima de FD_INTERNAL_BASE: ahí viven los descriptores del emulador (fd_hi); el programa ve 10240 (getrlimit)
		if (rl.rlim_cur != RLIM_INFINITY && rl.rlim_cur < FD_INTERNAL_LIMIT) rl.rlim_cur = FD_INTERNAL_LIMIT;
		if (rl.rlim_max != RLIM_INFINITY && rl.rlim_max < FD_INTERNAL_LIMIT) rl.rlim_max = FD_INTERNAL_LIMIT;
	} else if (r != RLIMIT_CORE) return 0;
	return setrlimit(r, &rl) == 0 ? 0 : -darwin_errno(errno);
}

// ---------------------------------------------------------------- señales (virtuales por ahora)
// Los manejadores se registran, pero no se entregan señales asíncronas: hace falta construir marcos de señal de macOS.
static int darwin_to_linux_sig(int s) {
	static const int m[32] = { 0, SIGHUP, SIGINT, SIGQUIT, SIGILL, SIGTRAP, SIGABRT, 0, SIGFPE, SIGKILL, SIGBUS, SIGSEGV, SIGSYS, SIGPIPE,
		SIGALRM, SIGTERM, SIGURG, SIGSTOP, SIGTSTP, SIGCONT, SIGCHLD, SIGTTIN, SIGTTOU, SIGIO, SIGXCPU, SIGXFSZ, SIGVTALRM, SIGPROF,
		SIGWINCH, 0, SIGUSR1, SIGUSR2 };
	return s > 0 && s < 32 ? m[s] : -1;
}
int darwin_sig_to_linux(int s) { return darwin_to_linux_sig(s); }   // para tahoe_run.c (proc_info)
static struct { uint64_t handler, tramp; uint32_t mask, flags; } sigtab[32];
static long bsd_sigaction(struct ctx* c) {
	int s = (int)c->a[0];
	if (s <= 0 || s >= 32 || s == 9 || s == 17) return -D_EINVAL;           // KILL y STOP no se pueden capturar
	if (c->a[2]) {                                                           // struct sigaction anterior: {handler, mask(4), flags(4)}
		uint8_t old[16] = { 0 };
		memcpy(old, &sigtab[s].handler, 8);
		memcpy(old + 8, &sigtab[s].mask, 4);
		memcpy(old + 12, &sigtab[s].flags, 4);
		if (safe_write(c->a[2], old, 16) != 16) return -D_EFAULT;
	}
	if (c->a[1]) {                                                           // struct __sigaction: {handler, tramp, mask(4), flags(4)}
		uint8_t nw[24];
		if (safe_read(c->a[1], nw, 24) != 24) return -D_EFAULT;
		memcpy(&sigtab[s].handler, nw, 8);
		memcpy(&sigtab[s].tramp, nw + 8, 8);
		memcpy(&sigtab[s].mask, nw + 16, 4);
		memcpy(&sigtab[s].flags, nw + 20, 4);
		// Señales de control de la terminal (SIGTSTP 18, SIGTTIN 21, SIGTTOU 22): SIG_IGN / SIG_DFL también en Linux. Un bash
		// interactivo ignora SIGTTOU antes de ponerse en primer plano (tcsetpgrp); si Linux no lo sabía, lo detenía (estado T)
		// y la shell de tools/macos-shell.sh se quedaba congelada sin prompt. Con un manejador propio se deja la acción
		// por omisión: aún no se entregan señales al invitado.
		if ((s == 18 || s == 21 || s == 22) && sigtab[s].handler <= 1) signal(darwin_to_linux_sig(s), sigtab[s].handler == 1 ? SIG_IGN : SIG_DFL);
	}
	return 0;
}
static uint64_t alt_sp, alt_size;
static int alt_flags = 4;                                                    // SS_DISABLE
static long bsd_sigaltstack(struct ctx* c) {
	if (c->a[1]) {
		uint8_t old[24] = { 0 };
		memcpy(old, &alt_sp, 8); memcpy(old + 8, &alt_size, 8); memcpy(old + 16, &alt_flags, 4);
		if (safe_write(c->a[1], old, 24) != 24) return -D_EFAULT;
	}
	if (c->a[0]) {
		uint8_t nw[24];
		if (safe_read(c->a[0], nw, 24) != 24) return -D_EFAULT;
		memcpy(&alt_sp, nw, 8); memcpy(&alt_size, nw + 8, 8); memcpy(&alt_flags, nw + 16, 4);
	}
	return 0;
}
// sigreturn(uctx, infostyle, token). Con uctx NULL es la forma con que siglongjmp/longjmp de libplatform avisan al núcleo
// de que salen (UC_RESET_ALT_STACK) o entran (UC_SET_ALT_STACK) en la pila alternativa: como XNU (unix_signal.c), solo
// se cambia SS_ONSTACK en ss_flags. La vuelta de un manejador de verdad no puede darse aún (no se construyen marcos de
// señal): se anota y EINVAL.
#define UC_SET_ALT_STACK 0x40000000u
#define UC_RESET_ALT_STACK 0x80000000u
static long bsd_sigreturn(struct ctx* c) {
	uint32_t style = (uint32_t)c->a[1];
	if (style == UC_SET_ALT_STACK) { alt_flags |= 1; return 0; }        // SS_ONSTACK
	if (style == UC_RESET_ALT_STACK) { alt_flags &= ~1; return 0; }
	static int dicho;
	if (!dicho++) logf_("    sigreturn <%d>: vuelta de un marco de señal (uctx=0x%lx estilo=0x%x) sin implementar\n", (int)getpid(), c->a[0], style);
	return -D_EINVAL;
}
static uint32_t pthread_mask;
static long bsd_pthread_sigmask(struct ctx* c) {                             // (how, set*, oset*) por valor en Darwin: how, set, oset
	uint32_t old = pthread_mask;
	if (c->a[1]) {
		uint32_t set;
		if (safe_read(c->a[1], &set, 4) != 4) return -D_EFAULT;
		if (c->a[0] == 1) pthread_mask |= set; else if (c->a[0] == 2) pthread_mask &= ~set; else if (c->a[0] == 3) pthread_mask = set; else return -D_EINVAL;
	}
	if (c->a[2] && safe_write(c->a[2], &old, 4) != 4) return -D_EFAULT;
	return 0;
}
static long bsd_sigpending(struct ctx* c) { uint32_t z = 0; return safe_write(c->a[0], &z, 4) == 4 ? 0 : -D_EFAULT; }

// kill(pid, señal) y __pthread_kill(hilo, señal): señales dirigidas al propio proceso se envían de verdad.
static long bsd_kill(struct ctx* c) {
	int s = (int)c->a[1];
	if (s == 0) { long r = syscall(SYS_kill, (int)c->a[0], 0); return r < 0 ? -darwin_errno(errno) : 0; }
	int ls = darwin_to_linux_sig(s);
	if (ls <= 0) return -D_EINVAL;
	long r = syscall(SYS_kill, (int)c->a[0], ls);
	// Señales que terminan a otro proceso: al registro (si no, el proceso desaparece sin dejar rastro: SIGKILL no se
	// puede interceptar en el destino).
	if ((int)c->a[0] != getpid() && (s == 9 || s == 15 || s == 6 || s == 3 || s == 2 || s == 1))
		logf_("    kill: <%d> envía la señal %d a %d%s\n", (int)getpid(), s, (int)c->a[0], r < 0 ? " (falló)" : "");
	return r < 0 ? -darwin_errno(errno) : 0;
}
// Diagnóstico dirigido: 1 si este proceso es uno de los indicados en /dev/shm/tahoe-stacks (nombres separados por espacios;
// cada uno, subcadena de su línea de órdenes: así también vale el xpcproxy que va a lanzar ese servicio).
// 1 si alguno de los nombres del archivo (separados por espacios) está en la línea de órdenes de este proceso.
static int names_match(const char* file) {
	char nm[256] = "", cl[1024] = "";
	FILE* f = fopen(file, "r"); if (!f) return 0;
	if (fgets(nm, sizeof nm, f)) nm[strcspn(nm, "\n")] = 0;
	fclose(f);
	f = fopen("/proc/self/cmdline", "r"); if (f) { size_t n = fread(cl, 1, sizeof cl - 1, f); for (size_t i = 0; i < n; i++) if (!cl[i]) cl[i] = ' '; fclose(f); }
	char* sv = NULL;
	for (char* t = strtok_r(nm, " \t", &sv); t; t = strtok_r(NULL, " \t", &sv)) if (strstr(cl, t)) return 1;
	return 0;
}
// Traza activada por un archivo de /dev/shm: si está vacío vale para todos los procesos; si tiene nombres, solo para esos.
int trace_file_on(const char* file) {
	struct stat st;
	if (stat(file, &st) != 0) return 0;
	return st.st_size == 0 ? 1 : names_match(file);
}
int diag_target(void) { static int want = -1; if (want < 0) want = names_match("/dev/shm/tahoe-stacks"); return want; }
// Traza de mensajes Mach (líneas «MSG»: envíos y recepciones) solo de los procesos nombrados en /dev/shm/tahoe-msg-trace;
// tools/diag/mensajes.py la convierte en una conversación. Mucho más ligera que la traza MT de todos los procesos.
int msg_trace(void) { static int want = -1; if (want < 0) want = names_match("/dev/shm/tahoe-msg-trace"); return want; }
// Con /dev/shm/tahoe-stacks-all, el proceso indicado registra todas sus syscalls BSD (no solo las que fallan).
int diag_all(void) { static int v = -1; if (v < 0) v = access("/dev/shm/tahoe-stacks-all", F_OK) == 0; return v; }
// Lee los mensajes de aborto del ejecutable principal (cargado en 0x100000000) y de todas las imágenes de la caché
// compartida (mapeada en 0x7ff800000000, sin slide).
static void crash_messages(void) {
	const uint64_t cache = 0x7ff800000000UL;
	uint32_t io = 0, ic = 0;
	if (safe_read(cache + 0x1c0, &io, 4) != 4 || safe_read(cache + 0x1c4, &ic, 4) != 4 || !ic || ic > 10000) ic = 0;
	for (uint32_t i = 0; i <= ic; i++) {
		uint64_t mh = 0; uint32_t pathoff = 0;
		if (i == ic) mh = 0x100000000UL;                            // el propio ejecutable
		else if (safe_read(cache + io + i * 32ULL, &mh, 8) != 8 || safe_read(cache + io + i * 32ULL + 24, &pathoff, 4) != 4) continue;
		uint32_t hdr[8];
		if (safe_read(mh, hdr, 32) != 32 || hdr[0] != 0xfeedfacf) continue;
		uint64_t lc = mh + 32;
		for (uint32_t c = 0; c < hdr[4] && c < 128; c++) {
			uint32_t cmd[2];
			if (safe_read(lc, cmd, 8) != 8 || cmd[1] < 8) break;
			if (cmd[0] == 0x19) {                                        // LC_SEGMENT_64
				uint32_t nsect = 0; safe_read(lc + 64, &nsect, 4);
				for (uint32_t s = 0; s < nsect && s < 64; s++) {
					char sname[17] = { 0 }; uint64_t addr = 0;
					uint64_t so = lc + 72 + s * 80ULL;
					safe_read(so, sname, 16); safe_read(so + 32, &addr, 8);
					if (strcmp(sname, "__crash_info") != 0) continue;
					uint64_t m1 = 0, m2 = 0; char t1[300] = "", t2[300] = "", path[160] = "";
					safe_read(addr + 8, &m1, 8); safe_read(addr + 32, &m2, 8);
					if (m1) safe_string(m1, t1, sizeof t1);
					if (m2) safe_string(m2, t2, sizeof t2);
					if (t1[0] || t2[0]) {
						if (pathoff) safe_string(cache + pathoff, path, sizeof path); else snprintf(path, sizeof path, "ejecutable");
						const char* b = strrchr(path, '/');
						logf_("    abort <%d>: [%s] \"%s\"%s%s\n", (int)getpid(), b ? b + 1 : path, t1, t2[0] ? " / " : "", t2);
					}
				}
			}
			lc += cmd[1];
		}
	}
}
// Pila del hilo actual en el registro. "abort" para caídas (con el motivo que dejó la librería); "espera" para los
// diagnósticos de bloqueo (STACKS), que no son caídas.
void diag_stack(ucontext_t* uc, const char* what) {
		// Motivo del aborto: cada librería que llama a abort/CRASH deja su texto en gCRAnnotations, en su sección
		// __DATA*,__crash_info (message en +8, message2 en +32). Se recorren las imágenes de la caché compartida.
		if (!strcmp(what, "abort")) crash_messages();
		{
			// Retornos plausibles: valores de la pila dentro de la caché cuya instrucción anterior es un "call".
			static __thread uint64_t sk[2048]; memset(sk, 0, sizeof sk);
			ssize_t got = safe_read((uint64_t)uc->uc_mcontext.gregs[REG_RSP], sk, sizeof sk);
			logf_("    %s <%d>: rip=0x%llx rsp=0x%llx rbp=0x%llx pila:", what, (int)getpid(), (unsigned long long)uc->uc_mcontext.gregs[REG_RIP], (unsigned long long)uc->uc_mcontext.gregs[REG_RSP], (unsigned long long)uc->uc_mcontext.gregs[REG_RBP]);
			for (int k = 0; k < 14 && k < (int)(got / 8); k++) logf_(" %lx", sk[k]);
			greg_t* g = uc->uc_mcontext.gregs;
			logf_("\n    %s <%d>: rax=%llx rbx=%llx rcx=%llx rdx=%llx rsi=%llx rdi=%llx r8=%llx r12=%llx r13=%llx r14=%llx r15=%llx", what, (int)getpid(),
				(unsigned long long)g[REG_RAX], (unsigned long long)g[REG_RBX], (unsigned long long)g[REG_RCX], (unsigned long long)g[REG_RDX], (unsigned long long)g[REG_RSI],
				(unsigned long long)g[REG_RDI], (unsigned long long)g[REG_R8], (unsigned long long)g[REG_R12], (unsigned long long)g[REG_R13], (unsigned long long)g[REG_R14], (unsigned long long)g[REG_R15]);
			logf_("\n    %s <%d>: marcos (rbp):", what, (int)getpid());          // pila exacta: macOS conserva el puntero de marco
			uint64_t fp = (uint64_t)g[REG_RBP];
			for (int k = 0; k < 32 && fp; k++) {
				uint64_t fr[2];
				if (safe_read(fp, fr, 16) != 16 || !fr[1]) break;
				logf_(" 0x%lx", fr[1]);
				if (fr[0] <= fp) break;
				fp = fr[0];
			}
			logf_("\n    %s <%d>: llamadas plausibles:", what, (int)getpid());
			int shown = 0;
			for (int k = 0; k < (int)(got / 8) && shown < 24; k++) {
				uint64_t a = sk[k];
				if (!((a >= 0x7ff800000000UL && a < 0x7ffffe600000UL) || (a >= 0x100000000UL && a < 0x101000000UL))) continue;   // caché o ejecutable
				uint8_t pre[6];
				if (safe_read(a - 6, pre, 6) != 6) continue;
				if (pre[1] == 0xe8 || pre[3] == 0xff || pre[4] == 0xff || pre[2] == 0xff) { logf_(" 0x%lx", a); shown++; }
			}
			logf_("\n");
		}
	}

void diag_crash(ucontext_t* uc) { diag_stack(uc, "abort"); }

// ---- Volcado de todos los hilos (diagnóstico de cuelgues): con /dev/shm/tahoe-stacks, cuando el proceso indicado cae,
// cada hilo dice qué llamada al núcleo emulada tiene en curso (t_cur, puesto por el manejador de SIGSYS) o, si estaba
// ejecutando código del invitado, dónde; y los retornos plausibles de su pila del invitado.
extern char tahoe_selector_get(void);
extern void tahoe_selector_set(char v);
static void plausible_returns(uint64_t rsp, int max) {
	static __thread uint64_t sk[1024];
	ssize_t got = safe_read(rsp, sk, sizeof sk);
	int shown = 0;
	for (int k = 0; k < (int)(got / 8) && shown < max; k++) {
		uint64_t a = sk[k];
		if (!((a >= 0x7ff800000000UL && a < 0x7ffffe600000UL) || (a >= 0x100000000UL && a < 0x101000000UL))) continue;
		uint8_t pre[6];
		if (safe_read(a - 6, pre, 6) != 6) continue;
		if (pre[1] == 0xe8 || pre[3] == 0xff || pre[4] == 0xff || pre[2] == 0xff) { logf_(" 0x%lx", a); shown++; }
	}
}
static void on_dump(int sig, siginfo_t* si, void* v) {
	(void)sig; (void)si;
	char sel = tahoe_selector_get();
	tahoe_selector_set(0);                                      // SYSCALL_DISPATCH_FILTER_ALLOW: las syscalls del anfitrión van directas
	struct ctx* c = t_cur;
	uint64_t nr = 0, uca = 0, rip = 0, rsp = 0, fp = 0;
	char ln[1024]; int k = 0;                                   // cada línea de una vez: los hilos escriben a la vez
	if (c && safe_read((uint64_t)&c->nr, &nr, 8) == 8 && safe_read((uint64_t)&c->uc, &uca, 8) == 8 && uca) {
		safe_read(uca + offsetof(ucontext_t, uc_mcontext.gregs) + REG_RIP * 8, &rip, 8);
		safe_read(uca + offsetof(ucontext_t, uc_mcontext.gregs) + REG_RSP * 8, &rsp, 8);
		safe_read(uca + offsetof(ucontext_t, uc_mcontext.gregs) + REG_RBP * 8, &fp, 8);
		k += snprintf(ln + k, sizeof ln - k, "    HILO <%d:%ld> en llamada clase=%lu num=%lu rip=0x%lx", (int)getpid(), (long)syscall(SYS_gettid), nr >> 24, nr & 0xffffff, rip);
		uint64_t a[3] = { 0 }; safe_read((uint64_t)&c->a, a, sizeof a);
		uint32_t num = (uint32_t)(nr & 0xffffff);
		if ((nr >> 24) == 2 && (num == 5 || num == 398 || num == 463 || num == 464)) {   // open / openat: la ruta
			char path[256] = ""; safe_read(num >= 463 ? a[1] : a[0], path, sizeof path - 1); path[255] = 0;
			k += snprintf(ln + k, sizeof ln - k, " \"%s\"", path);
		} else k += snprintf(ln + k, sizeof ln - k, " args=%lx,%lx,%lx", a[0], a[1], a[2]);
		logf_("%s\n", ln);
	} else {
		// fuera de una llamada: ejecutando código de macOS (o de tahoe-run, si la señal llegó ahí)
		ucontext_t* uc = v;
		rip = (uint64_t)uc->uc_mcontext.gregs[REG_RIP]; rsp = (uint64_t)uc->uc_mcontext.gregs[REG_RSP];
		if (rip >= 0x100000000ULL) fp = (uint64_t)uc->uc_mcontext.gregs[REG_RBP];   // código de macOS (tahoe-run vive por debajo)
		logf_("    HILO <%d:%ld> ejecutando rip=0x%lx\n", (int)getpid(), (long)syscall(SYS_gettid), rip);
	}
	if (fp) {                                                   // marcos exactos (macOS conserva el puntero de marco)
		k = snprintf(ln, sizeof ln, "    HILO <%d:%ld> marcos (rbp):", (int)getpid(), (long)syscall(SYS_gettid));
		for (int i = 0; i < 24 && fp && k < (int)sizeof ln - 24; i++) {
			uint64_t fr[2];
			if (safe_read(fp, fr, 16) != 16 || !fr[1]) break;
			k += snprintf(ln + k, sizeof ln - k, " 0x%lx", fr[1]);
			if (fr[0] <= fp) break;
			fp = fr[0];
		}
		logf_("%s\n", ln);
	} else if (rsp && uca) { plausible_returns(rsp, 16); logf_("\n"); }
	tahoe_selector_set(sel);
}
// SIGRTMIN+5 desde fuera (tools/diag/hang.sh): volcar todos los hilos del proceso de STACKS, sin terminarlo
static void on_dump_all(int sig, siginfo_t* si, void* v) {
	(void)sig;
	char sel = tahoe_selector_get();
	tahoe_selector_set(0);
	long me = syscall(SYS_gettid);
	DIR* d = opendir("/proc/self/task");
	struct dirent* e;
	while (d && (e = readdir(d))) {
		long t = atol(e->d_name);
		if (t > 0 && t != me) syscall(SYS_tgkill, getpid(), t, SIGRTMIN + 4);
	}
	if (d) closedir(d);
	tahoe_selector_set(sel);
	on_dump(SIGRTMIN + 4, si, v);
}
// ---- Puntos de interrupción para diagnóstico: /dev/shm/tahoe-break con direcciones en hex (una por línea). En el proceso de
// /dev/shm/tahoe-stacks se pone un int3 en cada una; al pasar se registran los registros, se restaura el byte original, se
// ejecuta esa instrucción paso a paso (TF) y se vuelve a poner el int3.
#define MAX_BRK 32
static uint64_t brk_addr[MAX_BRK]; static uint8_t brk_orig[MAX_BRK]; static int nbrk;
static __thread int brk_step = -1;
static void brk_poke(uint64_t a, uint8_t v) {
	uint64_t pg = a & ~4095ULL;
	mprotect((void*)pg, 8192, PROT_READ | PROT_WRITE | PROT_EXEC);
	*(volatile uint8_t*)a = v;
	mprotect((void*)pg, 8192, PROT_READ | PROT_EXEC);
}
static void on_sigtrap(int sig, siginfo_t* si, void* v) {
	(void)sig; (void)si;
	char sel = tahoe_selector_get();
	tahoe_selector_set(0);
	ucontext_t* uc = v; greg_t* g = uc->uc_mcontext.gregs;
	if (brk_step >= 0) {                                         // vuelta del paso a paso: rearmar
		brk_poke(brk_addr[brk_step], 0xcc);
		brk_step = -1;
		g[REG_EFL] &= ~0x100L;
	} else {
		uint64_t at = (uint64_t)g[REG_RIP] - 1;
		for (int i = 0; i < nbrk; i++) if (brk_addr[i] == at) {
			logf_("    BRK <%d:%ld> 0x%lx rax=%llx rbx=%llx rcx=%llx rdx=%llx rsi=%llx rdi=%llx r8=%llx r12=%llx r13=%llx r14=%llx r15=%llx\n",
			      (int)getpid(), (long)syscall(SYS_gettid), at, (unsigned long long)g[REG_RAX], (unsigned long long)g[REG_RBX],
			      (unsigned long long)g[REG_RCX], (unsigned long long)g[REG_RDX], (unsigned long long)g[REG_RSI], (unsigned long long)g[REG_RDI],
			      (unsigned long long)g[REG_R8], (unsigned long long)g[REG_R12], (unsigned long long)g[REG_R13], (unsigned long long)g[REG_R14], (unsigned long long)g[REG_R15]);
			FILE* dr = fopen("/dev/shm/tahoe-dumpreg", "r");      // volcar regiones de memoria a archivos
			if (dr) { uint64_t a, sz; while (fscanf(dr, "%lx %lx", &a, &sz) == 2) { char fn[128]; snprintf(fn, sizeof fn, "/tmp/tahoe-dump-%d-%lx.bin", (int)getpid(), a); FILE* o = fopen(fn, "w"); if (o) { uint8_t* bb = malloc(sz); if (safe_read(a, bb, sz) == (ssize_t)sz) fwrite(bb, 1, sz, o); free(bb); fclose(o); } } fclose(dr); }
			FILE* pk = fopen("/dev/shm/tahoe-peek", "r");       // además: valor de 8 bytes en esas direcciones
			if (pk) { char ln[64]; while (fgets(ln, sizeof ln, pk)) {
				uint64_t a, v = 0;
				if (!strncmp(ln, "bt", 2)) {                        // bt: cadena de rbp (direcciones de vuelta, 24 marcos)
					uint64_t fp = (uint64_t)g[REG_RBP], fr[2];
					for (int q = 0; q < 24 && fp && safe_read(fp, fr, 16) == 16; q++, fp = fr[0]) logf_("    BT #%d 0x%lx\n", q, fr[1]);
					continue;
				}
				char* e = ln; int deref = *e == '*';                // *expr: además, el texto al que apunta el valor leído
				if (deref) e++;
				int strip = *e == '~';                              // ~reg: sin los 4 bits altos (objetos puenteados de Swift)
				if (strip) e++;
				static const struct { const char* n; int r; } rn[] = { {"rax",REG_RAX},{"rsi",REG_RSI},{"rdi",REG_RDI},{"rbx",REG_RBX},{"rsp",REG_RSP},{"rbp",REG_RBP},{"rcx",REG_RCX},{"rdx",REG_RDX},{"r8",REG_R8},{"r9",REG_R9},{"r12",REG_R12},{"r13",REG_R13},{"r14",REG_R14},{"r15",REG_R15} };
				a = 0; for (size_t q = 0; q < sizeof rn / sizeof rn[0]; q++) { size_t nl = strlen(rn[q].n);
					if (!strncmp(e, rn[q].n, nl) && (e[nl] == '+' || e[nl] == '@' || e[nl] == '\n' || e[nl] == 0)) { a = ((uint64_t)g[rn[q].r] & (strip ? 0x0fffffffffffffffULL : ~0ULL)) + (e[nl] == '+' ? strtoull(e + nl + 1, NULL, 16) : 0); break; } }
				if (!a) a = strtoull(e, NULL, 16);
				for (char* at = strchr(e, '@'); a && at; at = strchr(at + 1, '@')) {   // reg+n@m: seguir el puntero en a y sumar m
					uint64_t nx; if (safe_read(a, &nx, 8) != 8) { a = 0; break; }
					a = (nx & (strip ? 0x0fffffffffffffffULL : ~0ULL)) + strtoull(at + 1, NULL, 16);
				}
				if (a && strip && !deref) {                        // ~reg+n: el texto que hay en esa dirección
					char t[129] = { 0 };
					if (safe_read(a, t, 128) > 0) { for (int q = 0; q < 128 && t[q]; q++) if (t[q] < 32 || t[q] > 126) t[q] = '.'; logf_("    PEEK %.*s: \"%s\"\n", (int)strcspn(ln, "\n"), ln, t); }
				} else if (a && safe_read(a, &v, 8) == 8) {
					logf_("    PEEK %.*s[0x%lx] = 0x%lx\n", (int)strcspn(ln, "\n"), ln, a, v);
					char t[161] = { 0 };                             // 160 bytes, ceros incluidos (objetos: cabecera y luego el texto)
					if (deref && v && safe_read(v, t, 160) > 0) { for (int q = 0; q < 160; q++) if (t[q] < 32 || t[q] > 126) t[q] = '.'; logf_("    PEEK *: \"%s\"\n", t); }
				} } fclose(pk); }
			brk_poke(at, brk_orig[i]);
			g[REG_RIP] = (greg_t)at;
			g[REG_EFL] |= 0x100L;                                // TF: una instrucción y vuelve aquí
			brk_step = i;
			break;
		}
	}
	tahoe_selector_set(sel);
}
void diag_breakpoints(void) {
	if (!diag_target()) return;
	tahoe_sigaction(SIGRTMIN + 4, on_dump);                     // volcados de hilos a petición (SIGRTMIN+5, ver on_dump_all)
	tahoe_sigaction(SIGRTMIN + 5, on_dump_all);
	FILE* f = fopen("/dev/shm/tahoe-break", "r");
	if (!f) return;
	char line[64];
	int mf = open("/proc/self/mem", O_RDONLY | O_CLOEXEC);          // aún no hay memfd: se llama al principio de main
	while (nbrk < MAX_BRK && fgets(line, sizeof line, f)) {
		uint64_t a = strtoull(line, NULL, 16);
		if (!a) continue;
		uint8_t o; if (mf < 0 || pread(mf, &o, 1, (off_t)a) != 1) continue;
		brk_addr[nbrk] = a; brk_orig[nbrk] = o; nbrk++;
	}
	if (mf >= 0) close(mf);
	fclose(f);
	if (!nbrk) return;
	tahoe_sigaction(SIGTRAP, on_sigtrap);
	for (int i = 0; i < nbrk; i++) brk_poke(brk_addr[i], 0xcc);
	logf_("    BRK <%d>: %d puntos puestos\n", (int)getpid(), nbrk);
}

void diag_dump_threads(void) {
	static int done;
	if (done || !diag_target()) return;
	done = 1;
	tahoe_sigaction(SIGRTMIN + 4, on_dump);
	DIR* d = opendir("/proc/self/task");
	if (!d) return;
	long me = syscall(SYS_gettid);
	struct dirent* e;
	while ((e = readdir(d))) {
		long t = atol(e->d_name);
		if (t > 0 && t != me) syscall(SYS_tgkill, getpid(), t, SIGRTMIN + 4);
	}
	closedir(d);
	struct timespec ts = { 0, 200 * 1000000L }; nanosleep(&ts, NULL);   // que escriban antes de salir
}

static long bsd_pthread_kill(struct ctx* c) {
	int s = (int)c->a[1];
	if (s == 0) return 0;
	if (s == 6) {
		diag_crash(c->uc);
		// abort() de un programa de macOS: el proceso muere por SIGABRT de verdad (launchd lo ve como en un Mac), pero sin
		// volcado de memoria de Linux: systemd-coredump guardaría ~10 MB y avisaría en el escritorio en cada caída.
		syscall(SYS_prctl, 4 /* PR_SET_DUMPABLE */, 0, 0, 0, 0);
	}
	int ls = darwin_to_linux_sig(s);
	if (ls <= 0) return -D_EINVAL;
	long r = syscall(SYS_tgkill, getpid(), syscall(SYS_gettid), ls);
	return r < 0 ? -darwin_errno(errno) : 0;
}

// ---------------------------------------------------------------- procesos: fork, execve, wait4, pipe
#include <fcntl.h>
#include <stdlib.h>
#include <sys/wait.h>

static int linux_to_darwin_sig(int s) {
	for (int d = 1; d < 32; d++) if (darwin_to_linux_sig(d) == s) return d;
	return s;
}
// Estado de wait4: salida normal = código << 8; terminado por señal = nº de señal de Darwin (+0x80 si volcó core).
static int darwin_wait_status(int st) {
	if (WIFEXITED(st)) return WEXITSTATUS(st) << 8;
	if (WIFSIGNALED(st)) return linux_to_darwin_sig(WTERMSIG(st)) | (WCOREDUMP(st) ? 0x80 : 0);
	if (WIFSTOPPED(st)) return 0x7f | (linux_to_darwin_sig(WSTOPSIG(st)) << 8);
	return st;
}

// fork: se usa el de Linux. La interceptación y la memoria se heredan. Convenio de Darwin: padre rax=pid hijo, rdx=0;
// hijo rax=pid del padre, rdx=1 (libsyscall devuelve 0 al hijo al ver rdx != 0).
static long bsd_fork(struct ctx* c) {
	long p = syscall(SYS_fork);
	if (p < 0) return -darwin_errno(errno);
	if (p == 0) { memfd_reopen(); ports_forked_child(); fileport_forked_child(); reenable_dispatch(); }  // el hijo: su propia memoria, no es el creador
	// Convenio de Darwin: hijo rax=pid del padre, rdx=1; padre rax=pid del hijo, rdx=0. libsyscall devuelve 0 al hijo
	// al ver rdx != 0, y libc ejecuta entonces los manejadores de hijo (reinicio de puertos Mach, hilo principal...).
	c->has_ret2 = 1;
	c->ret2 = (p == 0);
	return p == 0 ? getppid() : p;
}
static long bsd_wait4(struct ctx* c) {
	int st = 0;
	long r = syscall(SYS_wait4, (int)c->a[0], &st, (int)c->a[2], NULL);
	if (r < 0) return -darwin_errno(errno);
	if (r > 0 && WIFSIGNALED(st)) logf_("    fin <%ld> por la señal %d de Linux (lo recoge %d)\n", r, WTERMSIG(st), (int)getpid());
	if (c->a[1] && r > 0) { int ds = darwin_wait_status(st); if (safe_write(c->a[1], &ds, 4) != 4) return -D_EFAULT; }
	return r;
}
static long bsd_pipe(struct ctx* c) {
	int fds[2];
	if (pipe(fds) != 0) return -darwin_errno(errno);
	c->has_ret2 = 1;
	c->ret2 = (uint64_t)fds[1];
	return fds[0];
}
static long bsd_getpeername(struct ctx* c) { (void)c; return -38; }                // ENOTSOCK

// Lee un array de punteros a cadena del programa (argv o envp).
static int read_strv(uint64_t addr, char*** out, int max) {
	char** v = calloc(max + 1, sizeof(char*));
	int n = 0;
	for (; addr && n < max; n++) {
		uint64_t ptr;
		if (safe_read(addr + 8 * n, &ptr, 8) != 8 || !ptr) break;
		char tmp[131072];
		if (safe_string(ptr, tmp, sizeof tmp) != 0) break;
		v[n] = strdup(tmp);
	}
	*out = v;
	return n;
}

// Ruta del programa (vista de macOS) -> ruta real en el host.
static void host_path(const char* guest, char* out, size_t cap) {
	guest += tahoe_resolve_skip(guest);
	if (tahoe_root && guest[0] == '/' && !tahoe_host_passthrough(guest)) { snprintf(out, cap, "%s%s", tahoe_root, guest); return; }
	snprintf(out, cap, "%s", guest);
}

// Acciones de archivo de posix_spawn (struct _psfa_action de XNU, 1040 bytes: tipo, fd y una unión de 1032 bytes; en la de
// open van oflag (int), mode (mode_t de 16 bits) y la ruta[PATH_MAX] desde el byte 14). Con POSIX_SPAWN_CLOEXEC_DEFAULT
// (0x4000, el que usa launchd) el hijo solo recibe los descriptores que nombran las acciones. Sin esto la salida de un
// programa no llegaba a la tubería de quien lo lanza (diskarbitrationd no leía lo que imprime apfs.util -p) y los hijos
// heredaban todos los descriptores.
enum { PSFA_OPEN = 0, PSFA_CLOSE = 1, PSFA_DUP2 = 2, PSFA_INHERIT = 3, PSFA_FILEPORT_DUP2 = 4, PSFA_CHDIR = 5, PSFA_FCHDIR = 6 };
#define PSFA_SIZE 1040
struct psfa { int type, fd, arg; int oflag; uint16_t mode; char path[1024]; };
static struct psfa spawn_fa[32];
static int spawn_fa_n, spawn_cloexec_default;
static int spawn_setpgroup, spawn_pgroup;                          // POSIX_SPAWN_SETPGROUP: grupo de procesos del hijo (0: el suyo)
static void read_file_actions(uint64_t addr, uint64_t size) {
	spawn_fa_n = 0;
	if (!addr || size < 8) return;
	int hdr[2] = { 0, 0 };
	if (safe_read(addr, hdr, 8) != 8) return;
	if (trace_all) logf_("    posix_spawn: acciones de archivo: espacio=%d número=%d tamaño=%lu\n", hdr[0], hdr[1], size);
	for (int i = 0; i < hdr[1] && i < 32 && 8 + (uint64_t)(i + 1) * PSFA_SIZE <= size; i++) {
		uint8_t a[PSFA_SIZE];
		if (safe_read(addr + 8 + (uint64_t)i * PSFA_SIZE, a, sizeof a) != (ssize_t)sizeof a) break;
		struct psfa* f = &spawn_fa[spawn_fa_n++];
		memset(f, 0, sizeof *f);
		memcpy(&f->type, a, 4); memcpy(&f->fd, a + 4, 4); memcpy(&f->arg, a + 8, 4);
		if (f->type == PSFA_OPEN) { memcpy(&f->oflag, a + 8, 4); memcpy(&f->mode, a + 12, 2); memcpy(f->path, a + 14, 1023); }
		else if (f->type == PSFA_CHDIR) memcpy(f->path, a + 8, 1023);
		if (trace_all) logf_("    posix_spawn: acción %d fd=%d arg=%d ruta=\"%s\"\n", f->type, f->fd, f->arg, f->path);
	}
}
static void psfa_host(const char* guest, char* out, size_t cap) {
	char tmp[1100];
	if (!strcmp(guest, "/dev/console")) {                         // como bsd_open: no hay consola real (o la de diagnóstico)
		snprintf(out, cap, "%s", access("/dev/shm/tahoe-console", W_OK) == 0 ? "/dev/shm/tahoe-console" : "/dev/null");
		return;
	}
	host_path(guest, tmp, sizeof tmp);
	char al[64]; snprintf(out, cap, "%s", tahoe_dev_alias(tmp, al, sizeof al));
}
// Para posix_spawn del anfitrión: las mismas acciones con sus equivalentes de glibc. Devuelve 0 o un errno de Darwin.
static int psfa_build(posix_spawn_file_actions_t* fa) {
	posix_spawn_file_actions_init(fa);
	uint8_t keep[1024] = { 0 };
	int maxk = 2;
	for (int i = 0; i < spawn_fa_n; i++) {
		struct psfa* f = &spawn_fa[i];
		char hp[1100];
		switch (f->type) {
		case PSFA_OPEN: psfa_host(f->path, hp, sizeof hp); posix_spawn_file_actions_addopen(fa, f->fd, hp, darwin_open_flags(f->oflag), f->mode); break;
		case PSFA_CLOSE: posix_spawn_file_actions_addclose(fa, f->fd); break;
		case PSFA_DUP2: posix_spawn_file_actions_adddup2(fa, f->fd, f->arg); break;
		case PSFA_INHERIT: posix_spawn_file_actions_adddup2(fa, f->fd, f->fd); break;   // dup2 consigo mismo: quita FD_CLOEXEC
		case PSFA_CHDIR: psfa_host(f->path, hp, sizeof hp); posix_spawn_file_actions_addchdir_np(fa, hp); break;
		case PSFA_FCHDIR: posix_spawn_file_actions_addfchdir_np(fa, f->fd); break;
		default: logf_("    posix_spawn: acción de archivo %d sin implementar\n", f->type); break;
		}
		int t = f->type == PSFA_DUP2 ? f->arg : (f->type == PSFA_OPEN || f->type == PSFA_INHERIT) ? f->fd : -1;
		if (t >= 0 && t < 1024) { keep[t] = 1; if (t > maxk) maxk = t; }
	}
	if (spawn_cloexec_default) {                                   // el resto se cierra (después: pueden ser origen de un dup2)
		for (int fd = 0; fd <= maxk; fd++) if (!keep[fd]) posix_spawn_file_actions_addclose(fa, fd);
		posix_spawn_file_actions_addclosefrom_np(fa, maxk + 1);
	}
	return 0;
}
// POSIX_SPAWN_SETEXEC: las acciones se hacen en este proceso antes de exec.
static void psfa_apply_here(void) {
	uint8_t keep[1024] = { 0 };
	for (int i = 0; i < spawn_fa_n; i++) {
		struct psfa* f = &spawn_fa[i];
		char hp[1100];
		switch (f->type) {
		case PSFA_OPEN: {
			psfa_host(f->path, hp, sizeof hp);
			int n = (int)syscall(SYS_openat, AT_FDCWD, hp, darwin_open_flags(f->oflag) & ~O_CLOEXEC, f->mode);
			if (n >= 0 && n != f->fd) { dup2(n, f->fd); close(n); }
			break;
		}
		case PSFA_CLOSE: close(f->fd); break;
		case PSFA_DUP2: dup2(f->fd, f->arg); break;
		case PSFA_INHERIT: { int fl = fcntl(f->fd, F_GETFD); if (fl >= 0) fcntl(f->fd, F_SETFD, fl & ~FD_CLOEXEC); break; }
		case PSFA_CHDIR: psfa_host(f->path, hp, sizeof hp); if (chdir(hp) != 0) {} break;
		case PSFA_FCHDIR: if (fchdir(f->fd) != 0) {} break;
		default: logf_("    posix_spawn: acción de archivo %d sin implementar\n", f->type); break;
		}
		int t = f->type == PSFA_DUP2 ? f->arg : (f->type == PSFA_OPEN || f->type == PSFA_INHERIT) ? f->fd : -1;
		if (t >= 0 && t < 1024) keep[t] = 1;
	}
	if (spawn_cloexec_default) {
		DIR* d = opendir("/proc/self/fd"); struct dirent* e;
		int self = d ? dirfd(d) : -1;
		while (d && (e = readdir(d))) {
			int fd = atoi(e->d_name);
			if (e->d_name[0] == '.' || fd == self || (fd < 1024 && keep[fd])) continue;
			int fl = fcntl(fd, F_GETFD); if (fl >= 0) fcntl(fd, F_SETFD, fl | FD_CLOEXEC);
		}
		if (d) closedir(d);
	}
}

// execve: se relanza tahoe-run con el programa nuevo. Soporta "#!" (hasta 4 niveles).
static char spawn_special_env[8][32];   // TAHOE_SP<n>=<puerto>: puertos especiales para el hijo (acciones de puerto de posix_spawn)
static int spawn_special_n;
static long exec_common(uint64_t pathp, uint64_t argvp, uint64_t envp, int spawn, pid_t* pidout) {
	char gpath[4096], hpath[4200];
	if (safe_string(pathp, gpath, sizeof gpath) != 0) return -D_EFAULT;
	char **av, **ev;
	int ac = read_strv(argvp, &av, 4096), ec = read_strv(envp, &ev, 4096);
	if (ac == 0) { av[0] = strdup(gpath); ac = 1; }
	for (int depth = 0; depth < 4; depth++) {
		host_path(gpath, hpath, sizeof hpath);
		if (access(hpath, X_OK) != 0) return -darwin_errno(errno);
		int fd = open(hpath, O_RDONLY);
		if (fd < 0) return -darwin_errno(errno);
		char head[256];
		ssize_t hn = read(fd, head, sizeof head - 1);
		close(fd);
		if (hn >= 2 && head[0] == '#' && head[1] == '!') {                   // script: ejecutar el intérprete
			head[hn] = 0;
			char* nl = strchr(head, '\n');
			if (nl) *nl = 0;
			char* interp = head + 2;
			while (*interp == ' ' || *interp == '\t') interp++;
			char* arg = interp;
			while (*arg && *arg != ' ' && *arg != '\t') arg++;
			if (*arg) { *arg++ = 0; while (*arg == ' ' || *arg == '\t') arg++; }
			char** nv = calloc(ac + 4, sizeof(char*));
			int k = 0;
			nv[k++] = strdup(interp);
			if (*arg) nv[k++] = strdup(arg);
			nv[k++] = strdup(gpath);
			for (int i = 1; i < ac; i++) nv[k++] = av[i];
			av = nv; ac = k;
			snprintf(gpath, sizeof gpath, "%s", interp);
			continue;
		}
		// Como XNU: un Mach-O sin parte x86_64 no se ejecuta y la llamada falla con EBADARCH (86) sin crear el proceso. El
		// macOS completo es universal pero trae programas solo de Apple Silicon (p. ej. seputil, la tarea de arranque
		// data-protection), y launchd en un Mac Intel cuenta con ese error para saltarlos.
		if (hn >= 8) {
			uint32_t m; memcpy(&m, head, 4);
			int fat = m == 0xbebafecaU || m == 0xbfbafecaU, ok = 1;     // FAT_MAGIC / FAT_MAGIC_64 (big endian)
			if (m == 0xfeedfacfU) ok = *(uint32_t*)(head + 4) == 0x01000007;
			else if (fat) {
				uint32_t n = __builtin_bswap32(*(uint32_t*)(head + 4)), esz = m == 0xbebafecaU ? 20 : 32;
				ok = 0;
				for (uint32_t i = 0; i < n && 8 + (i + 1) * esz <= (uint32_t)hn; i++)
					if (__builtin_bswap32(*(uint32_t*)(head + 8 + i * esz)) == 0x01000007) ok = 1;
			}
			if (!ok) { logf_("    exec \"%s\": sin parte x86_64 -> EBADARCH\n", gpath); return -86; }
		}
		break;
	}
	// Programas que se sustituyen por un proceso que sale con éxito: rutas separadas por comas en TAHOE_STUB_EXEC (para
	// diagnóstico). Las fases de montaje del arranque (`mount -P <fase>` -> apfs_boot_util) se ejecutan de verdad: el
	// registro IOKit ya tiene el volumen APFS del UUID de arranque (io_disks, emu_iokit.inc); los volúmenes Preboot y Update
	// no existen y apfs_boot_util lo anota y sigue, como en un contenedor sin ellos.
	{
		const char* stubs = getenv("TAHOE_STUB_EXEC");
		if (!stubs) stubs = "";
		size_t gl = strlen(gpath);
		for (const char* p = stubs; *p;) {
			const char* q = strchr(p, ',');
			size_t l = q ? (size_t)(q - p) : strlen(p);
			if (l == gl && !strncmp(p, gpath, l)) {
				logf_("    exec \"%s\" sustituido por un proceso que sale con 0\n", gpath);
				char* targv[] = { (char*)"true", NULL };
				if (spawn) { int pe = posix_spawnp(pidout, "true", NULL, NULL, targv, environ); return pe ? -darwin_errno(pe) : 0; }
				_exit(0);
			}
			p += l + (q ? 1 : 0);
		}
	}
	// argv de tahoe-run: [self, caché, dyld, programa(host), argumentos del programa...]
	char** nargv = calloc(ac + 5, sizeof(char*));
	int k = 0;
	nargv[k++] = (char*)g_self_exe;
	nargv[k++] = (char*)g_cache_path;
	nargv[k++] = (char*)g_dyld_path;
	nargv[k++] = hpath;
	for (int i = 1; i < ac; i++) nargv[k++] = av[i];
	char** nenv = calloc(ec + 15 + 8 + 9, sizeof(char*));
	int e = 0;
	for (int i = 0; i < ec; i++) nenv[e++] = ev[i];
	char b1[4300], b2[4300];
	if (tahoe_root) { snprintf(b1, sizeof b1, "TAHOE_ROOT=%s", tahoe_root); nenv[e++] = b1; }
	snprintf(b2, sizeof b2, "TAHOE_ARGV0=%s", av[0]);
	nenv[e++] = b2;
	static char b3[4300];
	static char b4[200], b5[40];
	if (getenv("TAHOE_PORTS")) { snprintf(b4, sizeof b4, "TAHOE_PORTS=%s", getenv("TAHOE_PORTS")); nenv[e++] = b4; }
	if (getenv("TAHOE_PORTS_OWNER")) { snprintf(b5, sizeof b5, "TAHOE_PORTS_OWNER=%s", getenv("TAHOE_PORTS_OWNER")); nenv[e++] = b5; }
	if (getenv("TAHOE_LOGFILE")) { snprintf(b3, sizeof b3, "TAHOE_LOGFILE=%s", getenv("TAHOE_LOGFILE")); nenv[e++] = b3; }
	static char b6[600];
	if (getenv("TAHOE_CACHE_DIR")) { snprintf(b6, sizeof b6, "TAHOE_CACHE_DIR=%s", getenv("TAHOE_CACHE_DIR")); nenv[e++] = b6; }
	static char b8[600];
	if (getenv("TAHOE_OSK_FILE")) { snprintf(b8, sizeof b8, "TAHOE_OSK_FILE=%s", getenv("TAHOE_OSK_FILE")); nenv[e++] = b8; }
	static char b9[600];
	if (getenv("TAHOE_SMBIOS_FILE")) { snprintf(b9, sizeof b9, "TAHOE_SMBIOS_FILE=%s", getenv("TAHOE_SMBIOS_FILE")); nenv[e++] = b9; }
	static char b10[600];                                   // argumentos de arranque (boot-args): para todos los procesos
	if (getenv("TAHOE_BOOTARGS")) { snprintf(b10, sizeof b10, "TAHOE_BOOTARGS=%s", getenv("TAHOE_BOOTARGS")); nenv[e++] = b10; }
	static char b11[64];                                    // identidad PCI de la GPU emulada (emu_iokit.inc)
	if (getenv("TAHOE_GPU")) { snprintf(b11, sizeof b11, "TAHOE_GPU=%s", getenv("TAHOE_GPU")); nenv[e++] = b11; }
	if (getenv("TAHOE_FD2LOG")) nenv[e++] = (char*)"TAHOE_FD2LOG=1";
	if (getenv("TAHOE_DEBUG_NOREBOOT")) nenv[e++] = (char*)"TAHOE_DEBUG_NOREBOOT=1";   // modo depuración (ver emu_iokit.inc)
	if (trace_all) nenv[e++] = (char*)"TAHOE_TRACE=1";
	static char b7[300]; cred_env(b7, sizeof b7); nenv[e++] = b7;   // credenciales emuladas (uid/gid) para el hijo
	for (int i = 0; i < spawn_special_n; i++) nenv[e++] = spawn_special_env[i];
	static char inh[9][40];                                          // lo que hereda el hijo (ver ports_inherit_env)
	int ni = ports_inherit_env(inh, 9);
	for (int i = 0; i < ni; i++) nenv[e++] = inh[i];
	if (trace_all) logf_("    execve(\"%s\") -> relanzando tahoe-run\n", gpath);
	if (spawn) {
		posix_spawn_file_actions_t fa;
		psfa_build(&fa);
		posix_spawnattr_t at; posix_spawnattr_init(&at);
		if (spawn_setpgroup) { posix_spawnattr_setflags(&at, POSIX_SPAWN_SETPGROUP); posix_spawnattr_setpgroup(&at, spawn_pgroup); }
		int pe = posix_spawn(pidout, g_self_exe, &fa, &at, nargv, nenv);
		posix_spawnattr_destroy(&at);
		posix_spawn_file_actions_destroy(&fa);
		spawn_fa_n = 0; spawn_cloexec_default = 0; spawn_setpgroup = 0;
		return pe ? -darwin_errno(pe) : 0;
	}
	if (spawn_fa_n || spawn_cloexec_default) { psfa_apply_here(); spawn_fa_n = 0; spawn_cloexec_default = 0; }   // POSIX_SPAWN_SETEXEC
	if (spawn_setpgroup) { setpgid(0, spawn_pgroup); spawn_setpgroup = 0; }
	execwatch_notify();                                              // NOTE_EXEC para quien vigila este proceso (kqueue)
	execve(g_self_exe, nargv, nenv);
	return -darwin_errno(errno);
}

static long bsd_execve(struct ctx* c) { return exec_common(c->a[0], c->a[1], c->a[2], 0, NULL); }

// posix_spawn(pid*, ruta, adesc*, argv, envp): crea un proceso nuevo de tahoe-run. De momento se ignoran los
// atributos y las acciones sobre archivos (se avisa en el registro); POSIX_SPAWN_SETEXEC se trata como execve.
static long bsd_posix_spawn(struct ctx* c) {
	uint64_t ad[6] = { 0 };
	uint16_t psa_flags = 0;
	int32_t psa_pgroup = 0;
	if (c->a[2]) {
		safe_read(c->a[2], ad, sizeof ad);                          // attr_size, attrp, file_actions_size, file_actions
		if (ad[1]) { safe_read(ad[1], &psa_flags, 2); safe_read(ad[1] + 12, &psa_pgroup, 4); }   // psa_flags, ..., psa_pgroup (+12)
	}
	char gp[256] = "?";
	safe_string(c->a[1], gp, sizeof gp);
	spawn_special_n = 0;
	if (ad[5] && ad[4] >= 8) {                                      // acciones de puerto: PSPA_SPECIAL (1) fija un puerto especial del hijo
		int hdr[2] = { 0, 0 };
		safe_read(ad[5], hdr, 8);
		for (int i = 0; i < hdr[1] && i < 8; i++) {
			uint32_t act[6] = { 0 };                                // port_type, mask, new_port, behavior, flavor, which
			if (safe_read(ad[5] + 8 + i * 24, act, 24) != 24) break;
			if (act[0] == 1 && spawn_special_n < 8) snprintf(spawn_special_env[spawn_special_n++], 32, "TAHOE_SP%u=%u", act[5], act[2]);
			if (act[0] == 2) {                                      // PSPA_AU_SESSION: el hijo nace en la sesión de ese puerto
				int kt; int32_t asid;
				if (kobj_lookup(act[2], &kt, &asid) && kt == KO_AUDIT_SESSION) cred_spawn_session((uint32_t)asid);
			}
			logf_("    posix_spawn: acción de puerto tipo=%u which=%u puerto=0x%x\n", act[0], act[5], act[2]);
		}
	}
	logf_("    posix_spawn <%d>(\"%s\") attr_flags=0x%x ad=%lx,%lx,%lx,%lx,%lx,%lx\n", (int)getpid(), gp, psa_flags, ad[0], ad[1], ad[2], ad[3], ad[4], ad[5]);
	read_file_actions(ad[3], ad[2]);
	spawn_cloexec_default = (psa_flags & 0x4000) != 0;              // POSIX_SPAWN_CLOEXEC_DEFAULT
	// POSIX_SPAWN_SETPGROUP (0x2): el hijo en el grupo psa_pgroup, o en uno nuevo con su pid si es 0. Quien lo lanza puede
	// luego matar al grupo entero (system_profiler: kill(-pid, SIGTERM) a un informe que no acaba); sin esto fallaba y
	// system_profiler se quedaba esperando para siempre.
	spawn_setpgroup = (psa_flags & 0x2) != 0; spawn_pgroup = psa_pgroup;
	if (psa_flags & 0x40) { long r0 = exec_common(c->a[1], c->a[3], c->a[4], 0, NULL); cred_spawn_session(0); return r0; }
	pid_t pid = 0;
	long r = exec_common(c->a[1], c->a[3], c->a[4], 1, &pid);
	cred_spawn_session(0);
	if (r == 0 && c->a[0]) { uint32_t p = (uint32_t)pid; safe_write(c->a[0], &p, 4); }
	return r;
}

// setpriority(which, who, prio) / getpriority(which, who): PRIO_PROCESS/PGRP/USER (0-2) son el nice de POSIX y van al de Linux
// (los pid son los del espacio de PID del arranque). Bajar el nice exige CAP_SYS_NICE en el anfitrión: siendo root en macOS
// se acepta igual, como en XNU. Las clases propias de Darwin (PRIO_DARWIN_THREAD 3, _PROCESS 4, _GPU 5, _ROLE 6, _GAME_MODE
// 7...: modo de fondo, rol de la app) solo cambian la planificación de XNU: se aceptan sin efecto y getpriority da 0.
static long bsd_setpriority(struct ctx* c) {
	int which = (int)c->a[0], prio = (int)c->a[2];
	if (which > 2) return 0;
	if (which < 0) return -D_EINVAL;
	if (prio < -20) prio = -20;
	if (prio > 20) prio = 20;
	if (setpriority(which, (id_t)c->a[1], prio) == 0) return 0;
	if ((errno == EACCES || errno == EPERM) && cred_euid() == 0) return 0;
	return -darwin_errno(errno);
}
static long bsd_getpriority(struct ctx* c) {
	int which = (int)c->a[0];
	if (which > 2) return 0;
	if (which < 0) return -D_EINVAL;
	errno = 0;
	int v = getpriority(which, (id_t)c->a[1]);
	if (v == -1 && errno) return -darwin_errno(errno);
	c->raw_ret = 1;                                                  // el nice puede ser negativo: sin CF
	return v;
}

// bsdthread_ctl(comando, arg1, arg2, arg3): QoS, sobrescrituras de prioridad y permisos de las colas de trabajo.
// libdispatch llama a BSDTHREAD_CTL_WORKQ_ALLOW_KILL (0x1000) tras un fork y ABORTA si falla; sin colas de trabajo
// reales todos los comandos pueden aceptarse sin efecto.
static long bsd_bsdthread_ctl(struct ctx* c) {
	if (trace_all) logf_("    bsdthread_ctl(cmd=0x%lx, 0x%lx) -> sin efecto\n", c->a[0], c->a[1]);
	return 0;
}

// bsdthread_create(func, arg, stack, pthread, flags): crea un hilo de Linux que arranca en thread_start (el
// punto de entrada que libpthread registró en bsdthread_register) con la pila que eligió libpthread.
// Salidas de los hilos del invitado: al terminar (bsdthread_terminate) o aparcarse (workq_kernreturn) se vuelve con
// siglongjmp a la función del anfitrión que los lanzó, para que el pthread de Linux termine normalmente y libere su pila.
static __thread sigjmp_buf thr_exit, wq_park;
static __thread int thr_exit_ok, wq_park_ok, wq_terminated;
extern void guest_thread_done(void);
struct new_thread { uint64_t pthread, func, arg, stack, flags; };
extern void enter_guest_thread(uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t) __attribute__((noreturn));
static void* thread_main(void* p) {
	struct new_thread t = *(struct new_thread*)p;
	free(p);
	// Como XNU: base de GS en el TSD del hilo, se avisa con TSD_BASE_SET y se retira SUSPENDED (libpthread lo exige).
	{ FILE* f = fopen("/dev/shm/tahoe-watch-func", "r"); unsigned long fn = 0; if (f) { if (fscanf(f, "%lx", &fn) != 1) fn = 0; fclose(f); } g_watch = fn && fn == t.func; }
	uint64_t flags = t.flags & ~0x20000000u;
	uint32_t kport = alloc_port();
	if (mach_trace()) logf_("    THR <%d:%ld> puerto de hilo 0x%x func=0x%lx\n", (int)getpid(), (long)syscall(SYS_gettid), kport, t.func);
	// cada hilo tiene su propio nombre de puerto (os_unfair_lock lo usa como dueño)
	if (mach_thread_self_offset) *(uint32_t*)(t.pthread + tsd_offset + mach_thread_self_offset) = kport;   // el kernel deja aquí el puerto del hilo (relativo al TSD)
	if (tsd_offset) { syscall(SYS_arch_prctl, 0x1001 /*ARCH_SET_GS*/, t.pthread + tsd_offset); flags |= 0x10000000u; }
	if (sigsetjmp(thr_exit, 1) == 0) {
		thr_exit_ok = 1;
		enter_guest_thread(thread_start_fn, t.stack, t.pthread, kport, t.func, t.arg, t.stack, flags);
	}
	guest_thread_done();                                             // pthread de Linux termina y libera su pila
	return NULL;
}
static long bsd_bsdthread_create(struct ctx* c) {
	struct new_thread* t = malloc(sizeof *t);
	if (!t) return -D_ENOMEM;
	*t = (struct new_thread){ .pthread = c->a[3], .func = c->a[0], .arg = c->a[1], .stack = c->a[2], .flags = c->a[4] };
	pthread_attr_t at; pthread_attr_init(&at);
	pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
	pthread_attr_setstacksize(&at, 1 << 20);
	pthread_t th;
	int e = pthread_create(&th, &at, thread_main, t);
	if (e) { free(t); return -35 /* EAGAIN de Darwin */; }
	if (mach_trace()) { uint64_t w[8] = { 0 }; safe_read(c->a[3] + 0, w, 0); safe_read(c->a[1], w, 64);   // arg de thread_fun: estructura con la rutina real
		logf_("    THR <%d> crea hilo func=0x%lx arg=0x%lx [%lx %lx %lx %lx]\n", (int)getpid(), c->a[0], c->a[1], w[0], w[1], w[2], w[3]); }
	if (trace_all) logf_("    bsdthread_create: pthread=0x%lx stack=0x%lx flags=0x%lx\n", c->a[3], c->a[2], c->a[4]);
	return (long)c->a[3];
}
// bsdthread_terminate(stackaddr, freesize, port, sem): libera la pila del hilo y termina solo este hilo.
static long bsd_bsdthread_terminate(struct ctx* c) {
	if (mach_trace()) logf_("    THR <%d> termina hilo\n", (int)getpid());
	// Hilo del grupo: su pila la reservó este emulador (wq_main la libera). libpthread calcula el rango a liberar con su
	// propia idea de la pila del núcleo y podría alcanzar memoria vecina (la pila del anfitrión), así que se ignora.
	if (wq_park_ok) { wq_terminated = 1; siglongjmp(wq_park, 1); }
	if (c->a[0] && c->a[1]) munmap((void*)c->a[0], c->a[1]);
	if (thr_exit_ok) siglongjmp(thr_exit, 1);                        // hilo de bsdthread_create: termina su función normalmente
	syscall(SYS_exit, 0);
	return 0;
}

// ---------------------------------------------------------------- colas de trabajo (workqueue)
// Sin kernel, los hilos de las colas de trabajo los lanza este emulador: cada petición (hilos de un nivel de QoS o
// un workloop de libdispatch) crea un hilo de Linux con su propia pila y pthread_t, y entra en wqthread con los
// mismos registros y banderas que usa XNU: rdi=pthread, rsi=puerto, rdx=pila baja, rcx=lista de eventos, r8=banderas,
// r9=nº de eventos. El hilo termina cuando libpthread llama a workq_kernreturn para "aparcarse".
#define WQ_PRIO_QOS   0x00004000u
#define WQ_NEWSPI     0x00040000u
#define WQ_KEVENT     0x00080000u
#define WQ_TSD_SET    0x00200000u
#define WQ_WORKLOOP   0x00400000u
#define WQ_REUSE      0x00020000u   // el hilo ya pasó por libpthread: su pthread_t sigue en la lista y no se reinicia
#define WQ_STACK      (1u << 20)
static __thread uint64_t my_workloop;   // workloop que atiende este hilo
#define WQ_DATA_SIZE 32768
// npre/pre/pdata: eventos que el vigilante ya recogió del kqueue (y el búfer donde quedaron los mensajes Mach recibidos).
struct wq_job { uint64_t kqid; int workloop, has_req, npre; uint32_t prio; uint8_t req[72]; uint8_t pre[16 * 72]; uint8_t* pdata; struct wq_job* next; };
struct wl_state { uint64_t kqid; int running, pending, watching, has_req, owned; unsigned gen; uint8_t req[72]; struct wl_state* next; };   // gen: cuántas veces se armó (para no retirar un vigilante que acaban de volver a necesitar)   // watching: ya tiene vigilante; owned: petición en espera porque la cola tiene propietario
extern int kq_workloop_fd(uint64_t);
extern int kq_workloop_try_free(uint64_t);
extern long kq_drain(uint64_t, uint64_t, int, uint64_t, uint64_t);
static void wl_arm(uint64_t kqid);
// El kernel solo lanza el hilo si el estado de la cola sigue siendo el que esperaba libdispatch:
// (*ext[ADDR] & ext[MASK]) == ext[VALUE]; si cambió, la petición está obsoleta y se descarta.
// Banderas de prioridad del hilo (como workq_setup_and_run de XNU): la QoS de la petición (1..6) en los bits bajos y
// OVERCOMMIT o COOPERATIVE. libdispatch elige con ellas qué cola raíz vacía (_dispatch_worker_thread2): si no coinciden con
// la de la petición, lo encolado en esa cola no se vacía nunca. pp es un pthread_priority_t; sin QoS, LEGACY (4).
#define WQ_OVERCOMMIT 0x00010000u
#define WQ_COOPERATIVE 0x01000000u
#define WQ_EVENT_MANAGER 0x00100000u   // hilo gestor de eventos de libdispatch (WORKQ_THREAD_QOS_MANAGER de XNU)
#define PP_EVENT_MANAGER 0x02000000u   // _PTHREAD_PRIORITY_EVENT_MANAGER_FLAG
static uint32_t prio_from_pp(uint32_t pp, int coop) {
	if (pp & PP_EVENT_MANAGER) return WQ_EVENT_MANAGER;              // knote del gestor (p. ej. su EVFILT_USER de aviso)
	uint32_t qos = 4;                                                  // THREAD_QOS_LEGACY: los hilos de la cola por omisión
	if (!(pp & (0x20000000u | 0x02000000u)) && (pp & 0x3f00u)) qos = (uint32_t)__builtin_ffs((int)((pp & 0x3f00u) >> 8));
	if (coop) qos |= WQ_COOPERATIVE;
	else if (pp & 0x80000000u) qos |= WQ_OVERCOMMIT;                   // _PTHREAD_PRIORITY_OVERCOMMIT_FLAG
	return qos;
}
static uint32_t prio_from_kev(const uint8_t* kev72) { int32_t pp; memcpy(&pp, kev72 + 12, 4); return prio_from_pp((uint32_t)pp, 0); }
static int req_still_valid(const uint8_t* req) {
	uint64_t ext[4];
	memcpy(ext, req + 40, 32);
	if (!ext[1] || !ext[2]) return 1;
	uint64_t cur;
	if (safe_read(ext[1], &cur, 8) != 8) return 1;
	return (cur & ext[2]) == (ext[3] & ext[2]);
}
// Propietario de la cola (NOTE_WL_DISCOVER_OWNER, 0x80): XNU lee el nombre del hilo propietario de los bits bajos de
// dq_state (kdata & ~3). Mientras un workloop tiene propietario (un hilo que vacía la cola por su cuenta, p. ej. con
// dispatch_sync), su petición de hilo no se atiende: queda pendiente hasta que el propietario la suelta. Si se atendía,
// el hilo no podía tomar la cola, volvía a pedirla y terminaba, en bucle (systemstats gastaba así 1,4 núcleos).
static uint32_t req_owner(const uint8_t* req) {
	uint32_t ff; memcpy(&ff, req + 24, 4);
	if (!(ff & 0x80u)) return 0;
	uint64_t ext[4]; memcpy(ext, req + 40, 32);
	uint64_t cur;
	if (!ext[1] || safe_read(ext[1], &cur, 8) != 8) return 0;
	return (uint32_t)cur & ~3u;
}
// diagnóstico (MACH=1): máscara, valor esperado y valor actual de una petición de hilo
static void wl_trace(const char* que, uint64_t kqid, const uint8_t* req) {
	if (!wl_trace_on()) return;
	uint64_t ext[4] = { 0 }, cur = 0; if (req) memcpy(ext, req + 40, 32);
	if (ext[1]) safe_read(ext[1], &cur, 8);
	logf_("    WLQ <%d:%ld> %s kqid=0x%lx máscara=0x%lx valor=0x%lx actual=0x%lx\n", (int)getpid(), (long)syscall(SYS_gettid), que, (unsigned long)kqid, (unsigned long)ext[2], (unsigned long)ext[3], (unsigned long)cur);
}
static struct wl_state* wl_states;
static pthread_mutex_t wl_lock = PTHREAD_MUTEX_INITIALIZER;
// Punto de no retorno de una entrega (como filt_wlprocess de XNU, bajo el candado del kqueue): la petición tiene que seguir
// registrada y válida. libdispatch la retira (EV_DELETE, wq_cancel_workloop) cuando otro hilo toma la cola por traspaso
// síncrono; un hilo lanzado antes con su copia la entregaba igual, y al encontrar la cola con dueño y sin encolar,
// libdispatch abortaba («BUG IN LIBDISPATCH: Invalid wlh state»). Se entrega la versión vigente (libdispatch puede haberla
// actualizado entretanto). 1 si se entrega (req queda con la vigente), 0 si el hilo va sin ella.
static int wl_req_deliver(uint64_t kqid, uint8_t* req) {
	pthread_mutex_lock(&wl_lock);
	struct wl_state* w = wl_states;
	while (w && w->kqid != kqid) w = w->next;
	int ok = w && w->has_req;
	if (ok) memcpy(req, w->req, 72);
	if (ok && !req_still_valid(req)) ok = 0;
	pthread_mutex_unlock(&wl_lock);
	return ok;
}
static void wq_spawn(uint64_t kqid, int workloop, const uint8_t* req);
// Peticiones en espera de que su cola deje de tener propietario: un hilo las revisa cada 10 ms mientras haya alguna (la
// liberación llega también con NOTE_WL_END_OWNERSHIP, pero así no depende de ella). Llamar con wl_lock tomado.
static pthread_cond_t wl_owner_cv = PTHREAD_COND_INITIALIZER;
static pthread_cond_t wl_cv;                                   // (definida más abajo) avisa a los vigilantes
static int wl_owner_started;
static void* wl_owner_poll(void* p) {
	(void)p;
	pthread_mutex_lock(&wl_lock);
	for (;;) {
		int any = 0;
		for (struct wl_state* w = wl_states; w; w = w->next) {
			if (!w->owned) continue;
			if (!w->pending || w->running || !req_still_valid(w->req)) { w->owned = 0; pthread_cond_broadcast(&wl_cv); continue; }
			if (req_owner(w->req)) { any = 1; continue; }
			w->owned = 0; w->pending = 0; w->running = 1;           // el propietario la soltó: ahora sí, un hilo
			uint8_t copy[72]; memcpy(copy, w->req, 72); uint64_t kq = w->kqid;
			pthread_mutex_unlock(&wl_lock);
			wl_trace("propietario libre: lanza", kq, copy);
			wq_spawn(kq, 1, copy);
			pthread_mutex_lock(&wl_lock);
			any = 1;                                                 // la lista pudo cambiar: otra vuelta
			break;
		}
		if (any) { struct timespec dl; clock_gettime(CLOCK_REALTIME, &dl); dl.tv_nsec += 10000000L; if (dl.tv_nsec >= 1000000000L) { dl.tv_sec++; dl.tv_nsec -= 1000000000L; }
			pthread_cond_timedwait(&wl_owner_cv, &wl_lock, &dl); }
		else pthread_cond_wait(&wl_owner_cv, &wl_lock);
	}
	return NULL;
}
static void wl_defer_owned_locked(struct wl_state* w) {
	w->owned = 1; w->pending = 1; w->running = 0;
	if (!wl_owner_started) {
		wl_owner_started = 1;
		pthread_attr_t at; pthread_attr_init(&at); pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
		pthread_t th; if (pthread_create(&th, &at, wl_owner_poll, NULL)) wl_owner_started = 0;
	}
	pthread_cond_signal(&wl_owner_cv);
}

// Hilos de las colas de trabajo: un grupo reutilizable. Cuando libpthread "aparca" un hilo (workq_kernreturn) no se
// termina: se vuelve a la función del anfitrión con siglongjmp y el hilo espera la siguiente petición. Así las pilas
// (la del anfitrión, la del invitado y la alterna de SIGSYS) se crean una vez y no se fugan, y un bucle de peticiones
// no puede crear hilos sin límite: pasado WQ_MAX_HILOS las peticiones esperan en cola.
#define WQ_MAX_HILOS 48
#define WQ_IDLE_MS   4000
static pthread_mutex_t wq_pool_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t wq_pool_cv = PTHREAD_COND_INITIALIZER;
static struct wq_job* wq_queue_head; static struct wq_job** wq_queue_tail = &wq_queue_head;
static int wq_live, wq_idle;

// first: primer servicio de este hilo (libpthread inicializa su pthread_t). exit_thread: en vez de trabajo, la llamada de
// salida de XNU (nkevents = -1, WORKQ_EXIT_THREAD_NKEVENT) para que libpthread retire el hilo de su lista y lo termine.
static void wq_run_job(const struct wq_job* jp, uint8_t* base, size_t psize, int first, int exit_thread) {
	struct { uint64_t kqid; int workloop, has_req; uint8_t req[72]; } j = { jp->kqid, jp->workloop, jp->has_req, { 0 } };
	memcpy(j.req, jp->req, 72);
	uint64_t self = (uint64_t)base + WQ_STACK;
	if (first) memset(base + WQ_STACK - 0x3000, 0, 0x3000 + psize);   // pthread_t limpio solo la primera vez
	uint64_t kqid_slot = self - 0x2000, events = kqid_slot + 8;
	// Banderas como workq_setup_and_run de XNU: el hilo gestor lleva EVENT_MANAGER en lugar de la QoS. Sin ella,
	// libdispatch (_dispatch_wlh_worker_thread_init) no se reconoce como gestor: no vacía su cola ni programa los
	// temporizadores de las colas que no son workloop (dispatch_after en la cola principal o en las globales).
	uint64_t flags = (jp->prio & WQ_EVENT_MANAGER) ? (WQ_NEWSPI | WQ_EVENT_MANAGER) : (WQ_NEWSPI | WQ_PRIO_QOS | (jp->prio ? jp->prio : 4));
	if (tsd_offset) { syscall(SYS_arch_prctl, 0x1001 /*ARCH_SET_GS*/, self + tsd_offset); flags |= WQ_TSD_SET; }
	if (!first) flags |= WQ_REUSE;
	int nev = 0;
	my_workloop = j.workloop ? j.kqid : 0;
	static __thread uint8_t* data;                                   // búfer de datos (32 KB) de los mensajes Mach, uno por hilo
	if (exit_thread) { j.workloop = 0; nev = -1; }
	// La petición va como evento 0; abajo, en el punto de no retorno, se comprueba que siga registrada y válida (si no, el
	// hilo llega sin ella y libdispatch lo trata como «cancelada antes del punto de no retorno»).
	if (j.workloop) { *(uint64_t*)kqid_slot = j.kqid; flags |= (j.kqid == ~0ULL ? 0 : WQ_WORKLOOP) | WQ_KEVENT; if (j.has_req) { memcpy((void*)events, j.req, 72); nev = 1; }
		if (jp->npre) { memcpy((void*)(events + nev * 72), jp->pre, (size_t)jp->npre * 72); nev += jp->npre; }   // ya recogidos por el vigilante
		else {
			if (!data) data = mmap(NULL, WQ_DATA_SIZE + 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
			uint64_t* avail = (uint64_t*)(data + WQ_DATA_SIZE);
			*avail = WQ_DATA_SIZE;
			long d = kq_drain(j.kqid, events + nev * 72, 16 - nev, (uint64_t)data, (uint64_t)avail);
			if (d > 0) nev += (int)d;
		} }
	// Punto de no retorno, lo más tarde posible: la petición (evento 0) sigue registrada y válida, o el hilo va sin ella.
	if (j.workloop && j.has_req && j.kqid != ~0ULL && !wl_req_deliver(j.kqid, (uint8_t*)events)) {
		wl_trace("entrega: retirada o caducada, sin petición", j.kqid, j.req);
		memmove((void*)events, (void*)(events + 72), (size_t)(nev - 1) * 72); nev--;
	}
	if (trace_all) logf_("    wqthread: %s kqid=0x%lx pthread=0x%lx flags=0x%lx\n", j.workloop ? "workloop" : "worker", j.kqid, self, flags);
	if (sigsetjmp(wq_park, 1) == 0) {
		wq_park_ok = 1;
		static __thread uint32_t kp_cache; if (!kp_cache) kp_cache = alloc_port();
		uint32_t kp = kp_cache;
		if (wl_trace_on()) logf_("    THR <%d:%ld> hilo de cola puerto 0x%x %s kqid=0x%lx eventos=%d\n", (int)getpid(), (long)syscall(SYS_gettid), kp, j.workloop ? (j.kqid == ~0ULL ? "kq-cola" : "workloop") : "trabajador", j.kqid, nev);
		enter_guest_thread(wqthread_fn, self - 0x3000, self, kp, (uint64_t)base, j.workloop ? events : 0, flags, (uint64_t)(int64_t)nev);
	}
	wq_park_ok = 0;                                                  // volvió el hilo aparcado
	t_cur = NULL;                                                    // salió de la llamada con siglongjmp
	if (wl_trace_on()) logf_("    THR <%d:%ld> fin del trabajo\n", (int)getpid(), (long)syscall(SYS_gettid));
}

static void* wq_main(void* p) {
	struct wq_job* first = p;
	size_t psize = pthread_size ? pthread_size : 0x2000;
	uint8_t* base = mmap(NULL, WQ_STACK + psize, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	struct wq_job* job = first;
	if (base == MAP_FAILED) { free(first); pthread_mutex_lock(&wq_pool_lock); wq_live--; pthread_mutex_unlock(&wq_pool_lock); return NULL; }
	int first_use = 1;
	for (;;) {
		wq_run_job(job, base, psize, first_use, 0);
		first_use = 0;
		if (job->pdata) munmap(job->pdata, WQ_DATA_SIZE + 4096);
		free(job);
		if (wq_terminated) { pthread_mutex_lock(&wq_pool_lock); wq_live--; pthread_mutex_unlock(&wq_pool_lock); guest_thread_done(); munmap(base, WQ_STACK + psize); return NULL; }   // pthread_exit dentro de un trabajo
		pthread_mutex_lock(&wq_pool_lock);
		job = NULL;
		for (;;) {
			if (wq_queue_head) { job = wq_queue_head; wq_queue_head = job->next; if (!wq_queue_head) wq_queue_tail = &wq_queue_head; break; }
			struct timespec dl; clock_gettime(CLOCK_REALTIME, &dl);
			dl.tv_sec += WQ_IDLE_MS / 1000; dl.tv_nsec += (WQ_IDLE_MS % 1000) * 1000000L; if (dl.tv_nsec >= 1000000000L) { dl.tv_sec++; dl.tv_nsec -= 1000000000L; }
			wq_idle++;
			int rc = pthread_cond_timedwait(&wq_pool_cv, &wq_pool_lock, &dl);
			wq_idle--;
			if (rc && !wq_queue_head) break;                         // sin trabajo tras esperar: el hilo termina
		}
		if (!job) { wq_live--; pthread_mutex_unlock(&wq_pool_lock); break; }
		pthread_mutex_unlock(&wq_pool_lock);
	}
	// Inactivo: como XNU, se devuelve el hilo a libpthread con la llamada de salida; libpthread lo quita de su lista y
	// llama a bsdthread_terminate, que libera la pila del invitado y vuelve aquí.
	struct wq_job bye = { 0 };
	wq_run_job(&bye, base, psize, 0, 1);
	guest_thread_done();
	munmap(base, WQ_STACK + psize);
	return NULL;
}

static void wq_spawn_job(struct wq_job* j) {
	static int max_hilos;
	if (!max_hilos) { const char* e = getenv("TAHOE_WQ_MAX"); max_hilos = e && atoi(e) > 0 ? atoi(e) : WQ_MAX_HILOS; }
	pthread_mutex_lock(&wq_pool_lock);
	if (wq_idle > 0 || wq_live >= max_hilos) {                     // un hilo libre la atiende, o espera turno
		if (!wq_idle) {                                            // sin hilos libres: la petición espera (una línea por segundo)
			static time_t ult; time_t now = time(NULL);
			if (now != ult) { ult = now; logf_("    hilos de cola: %d en uso (máximo), la petición espera\n", wq_live); }
		}
		*wq_queue_tail = j; wq_queue_tail = &j->next;
		pthread_cond_signal(&wq_pool_cv);
		pthread_mutex_unlock(&wq_pool_lock);
		return;
	}
	wq_live++;
	pthread_mutex_unlock(&wq_pool_lock);
	pthread_attr_t at; pthread_attr_init(&at);
	pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
	pthread_attr_setstacksize(&at, 1 << 20);
	pthread_t th;
	if (pthread_create(&th, &at, wq_main, j)) { if (j->pdata) munmap(j->pdata, WQ_DATA_SIZE + 4096); free(j); pthread_mutex_lock(&wq_pool_lock); wq_live--; pthread_mutex_unlock(&wq_pool_lock); }
}
static void wq_spawn(uint64_t kqid, int workloop, const uint8_t* req) {
	struct wq_job* j = calloc(1, sizeof *j);
	if (!j) return;
	j->kqid = kqid; j->workloop = workloop;
	if (req) { j->has_req = 1; memcpy(j->req, req, 72); j->prio = prio_from_kev(req); }
	if (kqid == ~0ULL && !(j->prio & WQ_EVENT_MANAGER)) j->prio = (j->prio & 0xffu) | WQ_OVERCOMMIT;   // la kqueue de la cola: hilos de eventos overcommit (kqwq de XNU)
	wq_spawn_job(j);
}

// Petición de hilo para un workloop (EVFILT_WORKLOOP con NOTE_WL_THREAD_REQUEST). Un solo hilo por workloop a la vez.
void wq_request_workloop(uint64_t kqid, const void* req) {
	pthread_mutex_lock(&wl_lock);
	struct wl_state* w = wl_states;
	while (w && w->kqid != kqid) w = w->next;
	if (!w) { w = calloc(1, sizeof *w); w->kqid = kqid; w->next = wl_states; wl_states = w; }
	// El rebote (ESTALE) ya lo hizo kevent al registrarla: desde ahí la petición está activa, aunque el servidor actual
	// cambie dq_state en paralelo (volver a mirarlo aquí la perdía).
	int start = !w->running;
	memcpy(w->req, req, 72); w->has_req = 1;
	uint8_t copy[72]; memcpy(copy, req, 72);
	if (start && req_owner(req)) { wl_defer_owned_locked(w); pthread_mutex_unlock(&wl_lock); wl_trace("petición: cola con propietario, espera", kqid, copy); return; }
	if (start) w->running = 1; else w->pending = 1;
	pthread_mutex_unlock(&wl_lock);
	wl_trace(start ? "petición: lanza" : "petición: pendiente", kqid, copy);
	if (start) wq_spawn(kqid, 1, copy);
}

// libdispatch retiró la petición de hilo de este workloop: ya no hay que lanzar otro servidor al terminar el actual.
void wq_cancel_workloop(uint64_t kqid) {
	pthread_mutex_lock(&wl_lock);
	struct wl_state* w = wl_states;
	while (w && w->kqid != kqid) w = w->next;
	if (w) { w->pending = 0; w->has_req = 0; if (w->owned) { w->owned = 0; pthread_cond_broadcast(&wl_cv); } }
	pthread_mutex_unlock(&wl_lock);
	wl_trace("petición retirada", kqid, NULL);
}

static pthread_cond_t wl_cv = PTHREAD_COND_INITIALIZER;     // avisa a los vigilantes de que su workloop quedó libre

static void wl_finished(uint64_t kqid) {
	pthread_mutex_lock(&wl_lock);
	struct wl_state* w = wl_states;
	while (w && w->kqid != kqid) w = w->next;
	// Como en XNU: la petición que llegó con el workloop ocupado sigue activa hasta que se entrega o libdispatch la retira
	// (EV_DELETE con NOTE_WL_THREAD_REQUEST, ver wq_cancel_workloop). Al entregarla se comprueba otra vez que la cola siga
	// en el estado que esperaba ((*addr & mask) == value): el servidor que acaba de terminar puede haberla vaciado, y un
	// hilo con una petición caducada hace abortar a libdispatch («BUG IN LIBDISPATCH: Invalid wlh state»). Si vuelve a
	// hacer falta, libdispatch pide otro hilo al encolar.
	int again = w && w->pending && req_still_valid(w->req);
	uint8_t copy[72];
	if (again && req_owner(w->req)) {                                // la cola tiene propietario: esperar a que la suelte
		wl_defer_owned_locked(w); memcpy(copy, w->req, 72);
		pthread_cond_broadcast(&wl_cv);
		pthread_mutex_unlock(&wl_lock);
		wl_trace("fin: cola con propietario, espera", kqid, copy);
		wl_arm(kqid);
		return;
	}
	if (w) { w->pending = 0; w->running = again; memcpy(copy, w->req, 72); }
	pthread_cond_broadcast(&wl_cv);
	pthread_mutex_unlock(&wl_lock);
	if (w) wl_trace(again ? "fin: relanza pendiente" : "fin: libre", kqid, copy);
	if (again) wq_spawn(kqid, 1, copy); else wl_arm(kqid);
}

// Vigilante de un workloop: un hilo fijo por workloop. Mientras el workloop no tiene hilo, espera a que su kqueue esté
// listo, recoge él mismo los eventos y solo entonces lanza un hilo con ellos. poll() sobre un epoll puede avisar en
// falso (el aviso de un puerto ya se consumió por otra vía); en ese caso no se lanza nada y se vuelve a esperar.
static void* wl_watch(void* p) {
	uint64_t kqid = (uint64_t)p;
	int fd = kq_workloop_fd(kqid);
	pthread_mutex_lock(&wl_lock);
	struct wl_state* w = wl_states;
	while (w && w->kqid != kqid) w = w->next;
	pthread_mutex_unlock(&wl_lock);
	if (mach_trace()) logf_("    wl_watch <%d> kqid=0x%lx fd=%d inicia (w=%p)\n", (int)getpid(), (unsigned long)kqid, fd, (void*)w);
	if (!w || fd < 0) return NULL;
	for (;;) {
		pthread_mutex_lock(&wl_lock);
		while (w->running || w->owned) pthread_cond_wait(&wl_cv, &wl_lock);   // con propietario, los eventos son suyos
		pthread_mutex_unlock(&wl_lock);
		struct pollfd pf = { .fd = fd, .events = POLLIN };
		int pr = poll(&pf, 1, kqid == ~0ULL ? -1 : 2000);
		if (pr < 0) { if (errno == EINTR) continue; logf_("    wl_watch <%d>: poll falló: %s\n", (int)getpid(), strerror(errno)); break; }
		if (pr == 0) {                                              // 2 s sin eventos: ¿se puede liberar el workloop?
			pthread_mutex_lock(&wl_lock);
			int idle = !w->running && !w->owned && !w->pending; unsigned g = w->gen;
			pthread_mutex_unlock(&wl_lock);
			if (!idle || !kq_workloop_try_free(kqid)) continue;
			pthread_mutex_lock(&wl_lock);
			if (!w->running && !w->owned && !w->pending && w->gen == g) {   // nadie lo volvió a usar: fuera el vigilante
				for (struct wl_state** pp = &wl_states; *pp; pp = &(*pp)->next) if (*pp == w) { *pp = w->next; break; }
				pthread_mutex_unlock(&wl_lock);
				if (mach_trace()) logf_("    wl_watch <%d> kqid=0x%lx liberado\n", (int)getpid(), (unsigned long)kqid);
				free(w);
				return NULL;
			}
			pthread_mutex_unlock(&wl_lock);
			fd = kq_workloop_fd(kqid);                              // se volvió a usar entretanto: vigilar el nuevo
			if (fd < 0) break;
			continue;
		}
		if (!(pf.revents & POLLIN)) continue;
		pthread_mutex_lock(&wl_lock);
		if (w->running) { pthread_mutex_unlock(&wl_lock); continue; }   // otra petición ya lanzó el hilo
		w->running = 1;
		pthread_mutex_unlock(&wl_lock);
		struct wq_job* j = calloc(1, sizeof *j);
		uint8_t* data = mmap(NULL, WQ_DATA_SIZE + 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		long d = 0;
		if (j && data != MAP_FAILED) {
			uint64_t* avail = (uint64_t*)(data + WQ_DATA_SIZE);
			*avail = WQ_DATA_SIZE;
			d = kq_drain(kqid, (uint64_t)j->pre, 16, (uint64_t)data, (uint64_t)avail);
		}
		if (wl_trace_on()) { logf_("    wl_watch <%d> kqid=0x%lx recogió %ld:", (int)getpid(), (unsigned long)kqid, d);
			for (long q = 0; q < d && j; q++) { uint64_t id; int16_t fl; uint32_t ff; memcpy(&id, j->pre + q * 72, 8); memcpy(&fl, j->pre + q * 72 + 8, 2); memcpy(&ff, j->pre + q * 72 + 24, 4); logf_(" [id=0x%lx filtro=%d fflags=0x%x]", (unsigned long)id, fl, ff); }
			logf_("\n"); }
		if (d > 0) {
			j->kqid = kqid; j->workloop = 1; j->npre = (int)d; j->pdata = data;
			pthread_mutex_lock(&wl_lock);
			j->prio = w->has_req ? prio_from_kev(w->req) : prio_from_kev(j->pre);
			pthread_mutex_unlock(&wl_lock);
			if (kqid == ~0ULL) {
				// La kqueue de la cola (kqworkq de XNU) entrega los eventos de knotes del gestor (QoS con EVENT_MANAGER, como
				// el EVFILT_USER con que libdispatch lo despierta) a un hilo gestor, y el resto a hilos overcommit. Si el
				// lote trae alguno del gestor va entero a él: el gestor procesa todos los eventos que recibe antes de vaciar
				// su cola, y así los mensajes Mach recogidos siguen en un solo búfer.
				int mgr = 0;
				for (int i = 0; i < j->npre; i++) if (prio_from_kev(j->pre + i * 72) & WQ_EVENT_MANAGER) mgr = 1;
				j->prio = mgr ? WQ_EVENT_MANAGER : (j->prio & 0xffu) | WQ_OVERCOMMIT;
			}
			wq_spawn_job(j);
			continue;
		}
		free(j);
		if (data != MAP_FAILED) munmap(data, WQ_DATA_SIZE + 4096);
		static __thread int fails; static __thread int dumped;
		if (++fails == 2000 && !dumped && mach_trace()) {             // diagnóstico: qué descriptor mantiene listo el epoll sin dar eventos
			dumped = 1;
			char pth[64], ln[256]; snprintf(pth, sizeof pth, "/proc/self/fdinfo/%d", fd);
			FILE* f = fopen(pth, "r");
			while (f && fgets(ln, sizeof ln, f)) {
				int tfd = -1; unsigned ev = 0; unsigned long long dat = 0;
				if (sscanf(ln, "tfd: %d events: %x data: %llx", &tfd, &ev, &dat) >= 2) {
					char lk[128] = "", fp[64]; snprintf(fp, sizeof fp, "/proc/self/fd/%d", tfd); ssize_t n = readlink(fp, lk, sizeof lk - 1); if (n > 0) lk[n] = 0;
					struct pollfd q = { .fd = tfd, .events = POLLIN | POLLOUT }; poll(&q, 1, 0);
					logf_("    wl_watch <%d> fd %d en epoll %d: eventos=0x%x listo=0x%x knote=0x%llx %s\n", (int)getpid(), tfd, fd, ev, q.revents, dat, lk);
				}
			}
			if (f) fclose(f);
		}
		pthread_mutex_lock(&wl_lock);                               // aviso falso: liberar, salvo que entretanto llegara una petición
		int again = w->pending && req_still_valid(w->req);
		uint8_t copy[72]; memcpy(copy, w->req, 72);
		if (again && req_owner(w->req)) { wl_defer_owned_locked(w); pthread_mutex_unlock(&wl_lock); continue; }
		w->pending = 0; w->running = again;
		pthread_mutex_unlock(&wl_lock);
		if (again) wq_spawn(kqid, 1, copy);
	}
	return NULL;
}
static void wl_arm(uint64_t kqid) {
	pthread_mutex_lock(&wl_lock);
	struct wl_state* w = wl_states;
	while (w && w->kqid != kqid) w = w->next;
	if (!w) { w = calloc(1, sizeof *w); w->kqid = kqid; w->next = wl_states; wl_states = w; }
	int start = !w->watching;
	if (start) w->watching = 1;
	w->gen++;
	if (mach_trace()) logf_("    wl_arm <%d> kqid=0x%lx start=%d running=%d\n", (int)getpid(), (unsigned long)kqid, start, w->running);
	pthread_cond_broadcast(&wl_cv);
	pthread_mutex_unlock(&wl_lock);
	if (!start) return;
	pthread_attr_t at; pthread_attr_init(&at);
	pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
	pthread_attr_setstacksize(&at, 1 << 21);   // el TLS estático del emulador (búferes de mach_msg) vive en esta pila
	pthread_t th;
	if (pthread_create(&th, &at, wl_watch, (void*)kqid)) { pthread_mutex_lock(&wl_lock); w->watching = 0; pthread_mutex_unlock(&wl_lock); }
}

static long bsd_workq_open(struct ctx* c) { (void)c; return 0; }
void wq_note_workloop(uint64_t kqid) { (void)kqid; }
void wq_arm_workloop(uint64_t kqid) { wl_arm(kqid); }   // vigilar los eventos de este workloop aunque aún no tenga hilo
extern long kq_apply_changes(int layout, uint64_t chg, long n, uint64_t workloop);

static long bsd_workq_kernreturn(struct ctx* c) {
	uint64_t op = c->a[0];
	if (trace_all) logf_("    workq_kernreturn(op=0x%lx, 0x%lx, %ld, 0x%lx)\n", op, c->a[1], (long)c->a[2], c->a[3]);
	switch (op) {
	case 0x400: case 0x10: return 0;                                   // SETUP_DISPATCH, NEWSPISUPP
	case 0x20: case 0x30: {                                            // REQTHREADS(n): hilos de trabajo normales
		long n = (long)(int)c->a[2];                                  // arg2 = cuántos, arg3 = pthread_priority_t; 0x30 = cooperativos
		uint32_t prio = prio_from_pp((uint32_t)c->a[3] & ~PP_EVENT_MANAGER, op == 0x30);   // sin kevent no hay hilo gestor
		for (long i = 0; i < n && i < 8; i++) {
			struct wq_job* j = calloc(1, sizeof *j);
			if (!j) break;
			j->prio = prio;
			wq_spawn_job(j);
		}
		return 0;
	}
	case 0x200: return 0;                                              // SHOULD_NARROW: no
	case 0x80: return 0;                                               // SET_EVENT_MANAGER_PRIORITY
	case 0x100: case 0x40: case 0x04: {                                // retorno de hilo: se aparca = termina
		if (trace_all && op == 0x100) for (long i = 0; i < (long)(int)c->a[2] && i < 4; i++) {
			uint8_t kv[72]; uint64_t id, ud, ext[4]; int16_t f; uint16_t fl; uint32_t ff;
			if (safe_read(c->a[1] + i * 72, kv, 72) != 72) break;
			memcpy(&id, kv, 8); memcpy(&f, kv + 8, 2); memcpy(&fl, kv + 10, 2); memcpy(&ud, kv + 16, 8); memcpy(&ff, kv + 24, 4); memcpy(ext, kv + 40, 32);
			logf_("      retorno: ident=0x%lx filter=%d flags=0x%x fflags=0x%x udata=0x%lx ext=%lx,%lx,%lx,%lx\n", id, f, fl, ff, ud, ext[0], ext[1], ext[2], ext[3]);
		}
		if ((op == 0x100 || op == 0x40) && (long)(int)c->a[2] > 0) kq_apply_changes(2, c->a[1], (long)(int)c->a[2], my_workloop);   // 0x40: THREAD_KEVENT_RETURN
		if ((op == 0x100 || op == 0x40) && my_workloop) wl_finished(my_workloop);
		if (wq_park_ok) siglongjmp(wq_park, 1);                    // aparcar = volver al grupo de hilos
		syscall(SYS_exit, 0);
		return 0;
	}
	}
	return 0;
}


static long bsd_proc_rlimit_control(struct ctx* c) { (void)c; return 0; }   // límites de monitorización: sin efecto


// sigsuspend(mask): aquí no hay señales de Darwin reales todavía; el hilo se duerme hasta que Linux lo interrumpa.
static long bsd_sigsuspend(struct ctx* c) { (void)c; syscall(SYS_pause); return -4; /* EINTR */ }

// map_with_linking_np (dyld): sin soporte; dyld aplica los fixups por su cuenta cuando devuelve error.
static long bsd_map_with_linking(struct ctx* c) { (void)c; return -D_ENOSYS; }

// ulock (os_unfair_lock, dispatch_once, libpthread): ulock_wait/ulock_wake sobre futex de Linux. Con ULF_NO_ERRNO
// (0x1000000) los errores vuelven como valor negativo en lugar de por errno; aquí se devuelven siempre como errno.
#include <linux/futex.h>
// Diagnóstico: tabla de los hilos que esperan en un ulock; un hilo vigilante la vuelca pasados unos segundos.
static struct { volatile long tid; volatile uint32_t* addr; volatile uint32_t expect; volatile uint64_t op; volatile uint64_t spin; } wait_tab[256];
static void* wait_dump_thread(void* a) {
	(void)a;
	sleep(12);
	for (int i = 0; i < 256; i++) if (wait_tab[i].tid) logf_("    WAITTAB tid=%ld addr=%p esperado=0x%x op=0x%lx valor_actual=0x%x (%lx)\n", wait_tab[i].tid, (void*)wait_tab[i].addr, wait_tab[i].expect, (unsigned long)wait_tab[i].op, *wait_tab[i].addr, (unsigned long)((uint64_t*)wait_tab[i].addr)[0]);
	return NULL;
}
// XNU devuelve de ulock_wait cuántos hilos más siguen esperando en esa dirección (también cuando el valor ya no
// coincidía). libplatform lo usa para no marcar un os_unfair_lock "sin esperadores" al adquirirlo tras despertar; y
// libdispatch aborta si una espera de la que es el único dueño devuelve > 0. Por eso la cuenta es exacta por dirección.
struct uwait { uintptr_t addr; int n; struct uwait* next; };
static struct uwait* uwait_tab[1024];
static pthread_mutex_t uwait_lock = PTHREAD_MUTEX_INITIALIZER;
static void uwait_enter(uintptr_t a) {
	pthread_mutex_lock(&uwait_lock);
	struct uwait** b = &uwait_tab[(a >> 2) & 1023], *w = *b;
	while (w && w->addr != a) w = w->next;
	if (!w) { w = calloc(1, sizeof *w); w->addr = a; w->next = *b; *b = w; }
	w->n++;
	pthread_mutex_unlock(&uwait_lock);
}
static int uwait_leave(uintptr_t a) {                // devuelve cuántos quedan esperando
	pthread_mutex_lock(&uwait_lock);
	struct uwait** pp = &uwait_tab[(a >> 2) & 1023];
	while (*pp && (*pp)->addr != a) pp = &(*pp)->next;
	int left = 0;
	if (*pp) { left = --(*pp)->n; if (left <= 0) { struct uwait* d = *pp; *pp = d->next; free(d); left = 0; } }
	pthread_mutex_unlock(&uwait_lock);
	return left;
}
static long ulock_wait_impl(struct ctx* c, uint64_t timeout_ns) {
	uint32_t* addr = (uint32_t*)c->a[1];
	uint32_t expect = (uint32_t)c->a[2];
	struct timespec ts, *tp = NULL;
	if (timeout_ns) { ts.tv_sec = timeout_ns / 1000000000ULL; ts.tv_nsec = timeout_ns % 1000000000ULL; tp = &ts; }
	if (c->a[0] & 0x1000000) c->raw_ret = 1;     // ULF_NO_ERRNO
	if (mach_trace()) {                          // diagnóstico: pila de cada hilo que se bloquea sin plazo en el proceso indicado por /dev/shm/tahoe-stacks
		int want = diag_target();
		if (want && (!timeout_ns || syscall(SYS_gettid) == getpid())) { logf_("    ULW <%d:%ld> %p esperado=0x%x plazo=%lu op=0x%lx\n", (int)getpid(), (long)syscall(SYS_gettid), (void*)addr, expect, (unsigned long)timeout_ns, c->a[0]); { uint64_t m[8] = { 0 }; safe_read((uint64_t)addr - 24, m, 64); logf_("    ULW mem[-24..+40]: %lx %lx %lx %lx %lx %lx %lx %lx\n", m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7]); } diag_stack(c->uc, "espera"); }
	}
	if (trace_all) logf_("    ulock_wait(op=0x%lx, %p, esperado=0x%x, plazo=%luns)\n", c->a[0], (void*)addr, expect, (unsigned long)timeout_ns);
	int wslot = -1;
	if (mach_trace() && access("/dev/shm/tahoe-waittab", F_OK) == 0) {
		static pthread_once_t once = PTHREAD_ONCE_INIT; static int started;
		if (!started) { started = 1; pthread_t th; pthread_create(&th, NULL, wait_dump_thread, NULL); pthread_detach(th); }
		(void)once;
		for (int i = 0; i < 256; i++) { long z = 0; if (__atomic_compare_exchange_n(&wait_tab[i].tid, &z, (long)syscall(SYS_gettid), 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) { wslot = i; wait_tab[i].addr = addr; wait_tab[i].expect = expect; wait_tab[i].op = c->a[0]; break; } }
	}
	uwait_enter((uintptr_t)addr);
	long r = syscall(SYS_futex, addr, FUTEX_WAIT_PRIVATE, expect, tp, NULL, 0);
	int e = errno;
	int others = uwait_leave((uintptr_t)addr);
	if (wslot >= 0) wait_tab[wslot].tid = 0;
	if (r == 0) return others;
	switch (e) {
	case EAGAIN: return others;                  // el valor ya cambió: no hay que esperar
	case ETIMEDOUT: return -60;                  // ETIMEDOUT de Darwin
	case EINTR: return -4;
	default: return -darwin_errno(e);
	}
}
static long bsd_ulock_wait(struct ctx* c)  { return ulock_wait_impl(c, (uint64_t)(uint32_t)c->a[3] * 1000ULL); }   // plazo en microsegundos
static long bsd_ulock_wait2(struct ctx* c) { return ulock_wait_impl(c, c->a[3]); }                                 // plazo en nanosegundos
static long bsd_ulock_wake(struct ctx* c) {
	if (g_watch) { uint64_t m[8] = { 0 }; safe_read(c->a[1] - 24, m, 64); logf_("    WATCH ulock_wake op=0x%lx %p mem[-24..+40]: %lx %lx %lx %lx %lx %lx %lx %lx\n", c->a[0], (void*)c->a[1], m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7]); }
	if (c->a[0] & 0x1000000) c->raw_ret = 1;
	long n = syscall(SYS_futex, (uint32_t*)c->a[1], FUTEX_WAKE_PRIVATE, (c->a[0] & 0x100) ? 0x7fffffff : 1, NULL, NULL, 0);
	return n > 0 ? 0 : -D_ENOENT;
}

// ptrace: no hay depuradores dentro del emulador. PT_DENY_ATTACH (31, «que nadie me depure») y PT_TRACE_ME (0) se aceptan
// sin efecto; SecurityAgent (la ventana de inicio de sesión) sale con «Unable to deny attach» si PT_DENY_ATTACH falla.
// El resto (adjuntarse a otro proceso, leer su memoria...) no se admite: EPERM, como con un proceso protegido.
static long bsd_ptrace(struct ctx* c) {
	int req = (int)c->a[0];
	if (req == 31 || req == 0) return 0;
	return -1;                                              // EPERM
}

void emu_proc_init(void) {
	reg_bsd(26, bsd_ptrace);
	reg_bsd(515, bsd_ulock_wait); reg_bsd(516, bsd_ulock_wake); reg_bsd(544, bsd_ulock_wait2);
	reg_bsd(550, bsd_map_with_linking);
	reg_bsd(96, bsd_setpriority); reg_bsd(100, bsd_getpriority);
	reg_bsd(322, bsd_proc_rlimit_control);   // iopolicysys: sin efecto
	reg_bsd(331, bsd_proc_rlimit_control);   // __disable_threadsignal: sin efecto
	reg_bsd(111, bsd_sigsuspend); reg_bsd(410, bsd_sigsuspend);
	reg_bsd(446, bsd_proc_rlimit_control);
	reg_bsd(243, bsd_proc_rlimit_control);   // initgroups: sin efecto
	reg_bsd(499, bsd_work_interval_ctl);
	reg_bsd(440, bsd_proc_rlimit_control);   // memorystatus_control: sin efecto reg_bsd(444, bsd_proc_rlimit_control);   // change_fdguard_np
	reg_bsd(552, bsd_proc_rlimit_control);   // record_system_event: sin efecto
	reg_bsd(50, bsd_proc_rlimit_control);   // setlogin: sin efecto
	reg_bsd(367, bsd_workq_open); reg_bsd(368, bsd_workq_kernreturn);
	reg_bsd(360, bsd_bsdthread_create); reg_bsd(361, bsd_bsdthread_terminate);
	reg_bsd(478, bsd_bsdthread_ctl);
	reg_bsd(244, bsd_posix_spawn);
	reg_bsd(2, bsd_fork); reg_bsd(66, bsd_fork); reg_bsd(7, bsd_wait4); reg_bsd(42, bsd_pipe);
	reg_bsd(31, bsd_getpeername); reg_bsd(59, bsd_execve);
	reg_bsd(39, bsd_getppid); reg_bsd(81, bsd_getpgrp); reg_bsd(151, bsd_getpgid); reg_bsd(82, bsd_setpgid);
	reg_bsd(310, bsd_getsid); reg_bsd(147, bsd_setsid); reg_bsd(60, bsd_umask);
	reg_bsd(194, bsd_getrlimit); reg_bsd(195, bsd_setrlimit); reg_bsd(117, bsd_getrusage); reg_bsd(83, bsd_setitimer); reg_bsd(86, bsd_getitimer); reg_bsd(453, bsd_memorystatus_get_level); reg_bsd(184, bsd_sigreturn);
	reg_bsd(46, bsd_sigaction); reg_bsd(53, bsd_sigaltstack); reg_bsd(329, bsd_pthread_sigmask); reg_bsd(52, bsd_sigpending);
	reg_bsd(37, bsd_kill); reg_bsd(328, bsd_pthread_kill);
	reg_bsd(98, bsd_connect);
	reg_bsd(97, bsd_socket);
	reg_bsd(267, bsd_shm_unlink);
	reg_bsd(116, bsd_gettimeofday); reg_bsd(266, bsd_shm_open); reg_bsd(54, bsd_ioctl);
	reg_bsd(366, bsd_bsdthread_register);
}

