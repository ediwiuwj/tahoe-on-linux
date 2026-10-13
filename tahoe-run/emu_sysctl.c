// sysctl de macOS 26 sobre Linux.
//
// libSystem usa sysctlbyname(): primero traduce el nombre a un identificador con sysctl({0, 3}) (name2oid)
// y luego consulta ese identificador. Aquí hay una tabla de valores conocidos; lo que no esté se registra
// y devuelve ENOENT, de modo que cada ejecución indica qué falta.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/sysinfo.h>

#include "tahoe.h"

enum kind { K_INT, K_QUAD, K_STR, K_BOOT, K_LOADAVG, K_SWAP };
struct entry {
	const char* name;
	int mib[2];
	enum kind kind;
	long long ival;
	const char* sval;
};

#define CTL_KERN 1
#define CTL_HW 6
static char boot_session[40] = "B0070000-0000-4000-8000-000000000003";
// Centinela del sandbox (security.mac.sandbox.sentinel): una cadena por arranque, ".sb-" y 8 cifras hex. Security la usa para
// nombrar los temporales de sus escrituras atómicas (p. ej. al crear un llavero); sin ella, deshace la creación.
static char sb_sentinel[16] = ".sb-00000000";
// Identificadores reales de Darwin cuando existen; el resto (nombres dinámicos) usan 0x7000 + n.
static struct entry table[] = {
	{ "kern.ostype", { CTL_KERN, 1 }, K_STR, 0, "Darwin" },
	{ "kern.osrelease", { CTL_KERN, 2 }, K_STR, 0, "25.6.0" },
	{ "kern.version", { CTL_KERN, 4 }, K_STR, 0, "Darwin Kernel Version 25.6.0: tahoe-run; root:xnu-12377.121.6~1/RELEASE_X86_64" },
	{ "kern.maxproc", { CTL_KERN, 6 }, K_INT, 4000, NULL },
	{ "kern.maxfiles", { CTL_KERN, 7 }, K_INT, 245760, NULL },
	{ "kern.argmax", { CTL_KERN, 8 }, K_INT, 1048576, NULL },
	{ "kern.hostname", { CTL_KERN, 10 }, K_STR, 0, "localhost" },
	{ "kern.maxfilesperproc", { CTL_KERN, 29 }, K_INT, 122880, NULL },
	{ "kern.usrstack64", { CTL_KERN, 59 }, K_QUAD, (long long)STACK_TOP, NULL },
	{ "kern.osversion", { CTL_KERN, 65 }, K_STR, 0, "25G83" },
	{ "kern.boottime", { CTL_KERN, 21 }, K_BOOT, 0, NULL },
	{ "vm.loadavg", { 2, 2 }, K_LOADAVG, 0, NULL },                  // CTL_VM, VM_LOADAVG: getloadavg (iostat, uptime, top)
	{ "vm.swapusage", { 2, 5 }, K_SWAP, 0, NULL },                   // CTL_VM, VM_SWAPUSAGE: struct xsw_usage (top, sysctl)
	{ "sysctl.proc_cputype", { 0, 0x7300 }, K_INT, 0x01000007, NULL },  // con el pid detrás: tipo de CPU de ese proceso (top)
	{ "kern.bootargs", { CTL_KERN, 0x7000 }, K_STR, 0, "" },
	{ "kern.osproductversion", { CTL_KERN, 0x7001 }, K_STR, 0, "26.6.2" },
	{ "kern.osproductversionextra", { CTL_KERN, 0x7002 }, K_STR, 0, "" },
	{ "kern.secure_kernel", { CTL_KERN, 0x7003 }, K_INT, 0, NULL },
	{ "kern.safeboot", { CTL_KERN, 0x7004 }, K_INT, 0, NULL },
	{ "kern.osvariant_status", { CTL_KERN, 0x7005 }, K_QUAD, 0, NULL },
	{ "kern.wq_limit_cooperative_threads", { CTL_KERN, 0x7006 }, K_INT, 4, NULL },
	{ "kern.wq_max_threads", { CTL_KERN, 0x7007 }, K_INT, 512, NULL },
	{ "kern.wq_stalled_window_usecs", { CTL_KERN, 0x7008 }, K_INT, 200, NULL },
	{ "kern.wq_reduce_pool_window_usecs", { CTL_KERN, 0x7009 }, K_INT, 5000000, NULL },
	{ "kern.wq_max_timer_interval_usecs", { CTL_KERN, 0x700a }, K_INT, 50000, NULL },
	{ "kern.netboot", { CTL_KERN, 0x700b }, K_INT, 0, NULL },
	{ "kern.dyld_flags", { CTL_KERN, 0x700c }, K_INT, 0, NULL },
	{ "kern.bootuuid", { 201, 0x7200 }, K_STR, 0, "B0070000-0000-4000-8000-000000000001" },
	{ "kern.apfsprebootuuid", { 201, 0x7201 }, K_STR, 0, "B0070000-0000-4000-8000-000000000002" },
	{ "kern.bootobjectspath", { 201, 0x7202 }, K_STR, 0, "" },
	{ "kern.minimalboot", { 201, 0x7203 }, K_INT, 0, NULL },
	{ "kern.serverperfmode", { 201, 0x7204 }, K_INT, 0, NULL },
	{ "kern.osreleasetype", { 201, 0x7205 }, K_STR, 0, "User" },
	{ "kern.iossupportversion", { 201, 0x7206 }, K_STR, 0, "26.6" },
	{ "kern.osproductversioncompat", { 201, 0x7207 }, K_STR, 0, "26.6.2" },
	{ "kern.initproc_spawned", { 201, 0x7208 }, K_INT, 0, NULL },
	{ "kern.netboot", { 201, 0x7209 }, K_INT, 0, NULL },
	{ "kern.roots_installed", { 201, 0x720a }, K_INT, 0, NULL },
	{ "kern.willshutdown", { 201, 0x720b }, K_INT, 0, NULL },
	{ "kern.willuserspacereboot", { 201, 0x720c }, K_INT, 0, NULL },
	{ "kern.userspacerebootpurpose", { 201, 0x720d }, K_STR, 0, "" },
	{ "kern.maxprocperuid", { 201, 0x720e }, K_INT, 2666, NULL },
	{ "hw.ephemeral_storage", { 201, 0x720f }, K_INT, 0, NULL },
	{ "security.mac.sandbox.sandcastle.enabled", { 201, 0x7210 }, K_INT, 0, NULL },
	{ "security.mac.asp.willshutdown", { 201, 0x7211 }, K_INT, 0, NULL },
	{ "security.mac.amfi.developer_mode_status", { 201, 0x7212 }, K_INT, 1, NULL },
	{ "security.mac.amfi.allow_only_platform_code", { 201, 0x7213 }, K_INT, 0, NULL },
	{ "vm.shared_region_pivot", { 201, 0x7214 }, K_INT, 0, NULL },
	{ "hw.mempath", { 201, 0x7300 }, K_STR, 0, "" },
	{ "kern.bootsessionuuid", { 201, 0x7215 }, K_STR, 0, boot_session },   // UUID de esta sesión de arranque (el boot_id de Linux)
	{ "kern.hv_support", { 201, 0x7216 }, K_INT, 0, NULL },                  // sin Hypervisor.framework
	{ "kern.hv_vmm_present", { 201, 0x7217 }, K_INT, 0, NULL },
	{ "vm.darkwake_mode", { 201, 0x7218 }, K_INT, 0, NULL },
	{ "net.inet6.send.opmode", { 201, 0x7219 }, K_INT, 0, NULL },
	{ "security.mac.sandbox.sentinel", { 201, 0x721e }, K_STR, 0, sb_sentinel },
	{ "kern.hostid", { CTL_KERN, 11 }, K_INT, 0, NULL },
	{ "kern.safeboot", { CTL_KERN, 66 }, K_INT, 0, NULL },
	{ "hw.target", { 201, 0x721a }, K_STR, 0, "J152fAP" },              // MobileGestalt (HWModelStr) lo usa: el del MacBookPro16,1
	{ "hw.osenvironment", { 201, 0x721b }, K_STR, 0, "" },
	{ "hw.use_kernelmanagerd", { 201, 0x721c }, K_INT, 1, NULL },
	{ "hw.willshutdown", { 201, 0x721d }, K_INT, 0, NULL },
	{ "kern.netboot", { CTL_KERN, 40 }, K_INT, 0, NULL },
	{ "security.mac.lockdown_mode_state", { 200, 1 }, K_INT, 0, NULL },
	{ "hw.machine", { CTL_HW, 1 }, K_STR, 0, "x86_64" },
	{ "hw.model", { CTL_HW, 2 }, K_STR, 0, "MacBookPro16,1" },
	{ "hw.ncpu", { CTL_HW, 3 }, K_INT, -1, NULL },            // -1: se rellena al iniciar
	{ "hw.byteorder", { CTL_HW, 4 }, K_INT, 1234, NULL },
	{ "hw.physmem", { CTL_HW, 5 }, K_INT, -2, NULL },
	{ "hw.pagesize", { CTL_HW, 7 }, K_INT, 4096, NULL },
	{ "hw.cachelinesize", { CTL_HW, 16 }, K_INT, 64, NULL },
	{ "hw.memsize", { CTL_HW, 24 }, K_QUAD, -2, NULL },
	{ "hw.availcpu", { CTL_HW, 25 }, K_INT, -1, NULL },
	{ "hw.activecpu", { CTL_HW, 0x7100 }, K_INT, -1, NULL },
	{ "hw.logicalcpu", { CTL_HW, 0x7101 }, K_INT, -1, NULL },
	{ "hw.logicalcpu_max", { CTL_HW, 0x7102 }, K_INT, -1, NULL },
	{ "hw.physicalcpu", { CTL_HW, 0x7103 }, K_INT, -3, NULL },
	{ "hw.physicalcpu_max", { CTL_HW, 0x7104 }, K_INT, -3, NULL },
	{ "hw.cpu64bit_capable", { CTL_HW, 0x7105 }, K_INT, 1, NULL },
	{ "hw.cputype", { CTL_HW, 0x7106 }, K_INT, 7, NULL },       // CPU_TYPE_X86
	{ "hw.pagesize32", { CTL_HW, 0x7107 }, K_INT, 4096, NULL },
	{ "hw.optional.x86_64", { CTL_HW, 0x7108 }, K_INT, 1, NULL },
	{ "hw.optional.avx1_0", { CTL_HW, 0x7109 }, K_INT, 1, NULL },
	{ "hw.optional.avx2_0", { CTL_HW, 0x710a }, K_INT, 1, NULL },
	{ "hw.optional.sse4_2", { CTL_HW, 0x710b }, K_INT, 1, NULL },
};
#define NENT (sizeof table / sizeof table[0])

void emu_sysctl_init(void) {
	FILE* bf = fopen("/proc/sys/kernel/random/boot_id", "r");
	if (bf) {
		char b[64] = "";
		if (fgets(b, sizeof b, bf) && strlen(b) >= 36) { for (int i = 0; i < 36; i++) boot_session[i] = (b[i] >= 'a' && b[i] <= 'f') ? (char)(b[i] - 32) : b[i]; boot_session[36] = 0; snprintf(sb_sentinel, sizeof sb_sentinel, ".sb-%.8s", b); }
		fclose(bf);
	}
	long ncpu = sysconf(_SC_NPROCESSORS_ONLN);
	long long mem = (long long)sysconf(_SC_PHYS_PAGES) * sysconf(_SC_PAGESIZE);
	for (size_t i = 0; i < NENT; i++) {
		if (table[i].ival == -1) table[i].ival = ncpu;
		else if (table[i].ival == -2) table[i].ival = mem;
		else if (table[i].ival == -3) table[i].ival = ncpu > 1 ? ncpu / 2 : 1;
	}
	extern long bsd_sysctl(struct ctx*);
	reg_bsd(202, bsd_sysctl);
}

// Escribe el valor en el búfer del programa como lo hace sysctl(2): oldlenp entra con la capacidad y sale
// con el tamaño real; si el búfer no basta se devuelve ENOMEM.
static long sc_out(struct ctx* c, const void* data, size_t len) {
	uint64_t oldp = c->a[2], oldlenp = c->a[3], cap = 0;
	if (oldlenp && safe_read(oldlenp, &cap, 8) != 8) return -D_EFAULT;
	if (!oldp) {
		if (oldlenp && safe_write(oldlenp, &len, 8) != 8) return -D_EFAULT;
		return 0;
	}
	size_t n = len < cap ? len : cap;
	if (n && safe_write(oldp, data, n) != (ssize_t)n) return -D_EFAULT;
	if (oldlenp) { uint64_t l = len; if (safe_write(oldlenp, &l, 8) != 8) return -D_EFAULT; }
	return n < len ? -D_ENOMEM : 0;
}

// kinfo_proc de Darwin (648 bytes): solo se rellenan pid, estado, nombre, ppid y propietario, desde /proc.
#include <dirent.h>
#include <stdlib.h>
// Procesos de macOS = procesos de tahoe-run (mismo ejecutable que este). Su cmdline es: tahoe-run <caché> <dyld> <programa>
// [args...]; el programa y sus argumentos son los que ve macOS (la ruta, sin la raíz de macOS).
static int read_file(const char* path, char* buf, size_t cap) {
	FILE* f = fopen(path, "r"); if (!f) return -1;
	size_t n = fread(buf, 1, cap - 1, f); fclose(f); buf[n] = 0; return (int)n;
}
static int is_tahoe_proc(int pid) {
	static char self[512]; static ssize_t sl = -1;
	if (sl < 0) { sl = readlink("/proc/self/exe", self, sizeof self - 1); if (sl > 0) self[sl] = 0; }
	char d[128], p[160], e[512];
	if (proc_dir(pid, d, sizeof d) != 0) return 0;
	snprintf(p, sizeof p, "%s/exe", d);
	ssize_t n = readlink(p, e, sizeof e - 1);
	if (n <= 0 || sl <= 0) return 0;
	e[n] = 0;
	return !strcmp(e, self);
}
// argv de macOS del proceso: devuelve argc y deja los argumentos (separados por NUL) en out
static int guest_argv(int pid, char* out, size_t cap, size_t* len) {
	char d[128], p[160], buf[16384];
	if (proc_dir(pid, d, sizeof d) != 0) return -1;
	snprintf(p, sizeof p, "%s/cmdline", d);
	int n = read_file(p, buf, sizeof buf); if (n <= 0) return -1;
	int idx = 0, argc = 0; size_t o = 0;
	for (int i = 0; i < n; ) {
		const char* a = buf + i; size_t l = strlen(a);
		if (idx >= 3) {
			const char* g = a;
			size_t rl = tahoe_root ? strlen(tahoe_root) : 0;
			if (idx == 3 && rl && !strncmp(a, tahoe_root, rl) && a[rl] == '/') g = a + rl;
			size_t gl = strlen(g);
			if (o + gl + 1 > cap) break;
			memcpy(out + o, g, gl + 1); o += gl + 1; argc++;
		}
		i += (int)l + 1; idx++;
	}
	*len = o;
	return argc;
}
static uint32_t proc_euid(int pid) {
	char d[128], p[160], env[32768];
	if (proc_dir(pid, d, sizeof d) != 0) return 0;
	snprintf(p, sizeof p, "%s/environ", d);
	FILE* f = fopen(p, "r"); if (!f) return 0;
	size_t n = fread(env, 1, sizeof env - 1, f); fclose(f); env[n] = 0;
	for (size_t i = 0; i < n; i += strlen(env + i) + 1)
		if (!strncmp(env + i, "TAHOE_CRED=", 11)) { unsigned a, b; if (sscanf(env + i + 11, "%u,%u", &a, &b) == 2) return b; }
	return 0;
}
static int fill_kinfo(int pid, uint8_t* o) {
	if (!is_tahoe_proc(pid)) return -1;
	char dir[128], path[160], buf[512];
	if (proc_dir(pid, dir, sizeof dir) != 0) return -1;
	snprintf(path, sizeof path, "%s/stat", dir);   // el de un hilo vivo si el líder es zombi: su estado es el del proceso
	FILE* f = fopen(path, "r");
	if (!f) return -1;
	size_t n = fread(buf, 1, sizeof buf - 1, f);
	fclose(f);
	buf[n] = 0;
	char* lp = strchr(buf, '(');
	char* rp = strrchr(buf, ')');
	if (!lp || !rp) return -1;
	char comm[17] = { 0 };
	{   // nombre: el del programa de macOS (argv[0] de macOS, sin ruta)
		char av[4096]; size_t al = 0;
		if (guest_argv(pid, av, sizeof av, &al) > 0) { const char* b = strrchr(av, '/'); snprintf(comm, sizeof comm, "%s", b ? b + 1 : av); }
		else { size_t cl = (size_t)(rp - lp - 1) < 16 ? (size_t)(rp - lp - 1) : 16; memcpy(comm, lp + 1, cl); }
	}
	char state; int ppid = 0, pgrp = 0, sess = 0, tty = 0, tpgid = -1;
	sscanf(rp + 2, "%c %d %d %d %d %d", &state, &ppid, &pgrp, &sess, &tty, &tpgid);
	memset(o, 0, 648);
	int32_t p_flag = 0x4 /*P_LP64*/ | 0x2 /*P_CONTROLT*/; uint8_t stat = (state == 'Z') ? 5 : (state == 'T') ? 4 : (state == 'S' || state == 'D') ? 3 : 2;
	memcpy(o + 32, &p_flag, 4); o[36] = stat; memcpy(o + 40, &pid, 4);
	memcpy(o + 243, comm, 16);
	uint32_t uid = proc_euid(pid), gid = 0;
	memcpy(o + 312 + 80, &uid, 4); memcpy(o + 312 + 84, &uid, 4); memcpy(o + 312 + 88, &gid, 4); memcpy(o + 312 + 92, &gid, 4);   // e_pcred: ruid, svuid, rgid, svgid
	memcpy(o + 416 + 4, &uid, 4);                                                                                              // e_ucred.cr_uid
	memcpy(o + 560, &ppid, 4); memcpy(o + 564, &pgrp, 4);
	// terminal de control: tty_nr de Linux -> dev_t de Darwin (mayor << 24 | menor), como st_rdev en stat; NODEV si no tiene
	int32_t tdev = -1;
	if (tty) { unsigned ma = ((unsigned)tty >> 8) & 0xfff, mi = ((unsigned)tty & 0xff) | (((unsigned)tty >> 12) & 0xfff00); tdev = (int32_t)darwin_dev(ma, mi); }
	memcpy(o + 572, &tdev, 4); memcpy(o + 576, &tpgid, 4);
	if (!tty) { p_flag &= ~0x2; memcpy(o + 32, &p_flag, 4); }
	if (sess == pid) { int32_t ef = 0x2; memcpy(o + 612, &ef, 4); }   // e_flag: EPROC_SLEADER (la «s» de STAT en ps)
	{   // p_starttime (timeval al principio de extern_proc): campo 22 de stat, en ticks desde el arranque del sistema
		unsigned long long start = 0; int fi = 3;
		for (char* q = rp + 2; *q && fi < 22; q++) if (*q == ' ') { fi++; if (fi == 22) start = strtoull(q + 1, NULL, 10); }
		long hz = sysconf(_SC_CLK_TCK); if (hz <= 0) hz = 100;
		struct timespec now, up; clock_gettime(CLOCK_REALTIME, &now); clock_gettime(CLOCK_BOOTTIME, &up);
		double s = (double)now.tv_sec + now.tv_nsec / 1e9 - (up.tv_sec + up.tv_nsec / 1e9) + (double)start / (double)hz;
		int64_t sec = (int64_t)s; int32_t usec = (int32_t)((s - (double)sec) * 1e6);
		memcpy(o, &sec, 8); memcpy(o + 8, &usec, 4);
	}
	return 0;
}

// ---- Interfaces de red (getifaddrs de macOS): sysctl {CTL_NET, PF_ROUTE, 0, af, NET_RT_IFLIST(2), 0} devuelve mensajes
// de rutas: un if_msghdr(2) + sockaddr_dl por interfaz y un ifa_msghdr + sockaddrs por dirección. Se generan del getifaddrs
// de Linux. Nombres al estilo de macOS: lo -> lo0, las demás en0, en1... (en orden de índice).
#include <ifaddrs.h>
#include <net/if.h>
#include <netinet/in.h>
#include <netpacket/packet.h>
#define SA_RLEN(l) (((l) + 3) & ~3)
struct nif { char lname[IFNAMSIZ], dname[16]; unsigned idx; unsigned flags; uint8_t mac[6]; int has_mac; };
static int nif_list(struct nif* v, int max) {
	struct ifaddrs* ifa; if (getifaddrs(&ifa) != 0) return 0;
	int n = 0;
	for (struct ifaddrs* a = ifa; a; a = a->ifa_next) {
		int k; for (k = 0; k < n && strcmp(v[k].lname, a->ifa_name); k++) {}
		if (k == n) { if (n == max) continue; memset(&v[n], 0, sizeof v[n]); snprintf(v[n].lname, IFNAMSIZ, "%s", a->ifa_name); v[n].idx = if_nametoindex(a->ifa_name); v[n].flags = a->ifa_flags; n++; }
		if (a->ifa_addr && a->ifa_addr->sa_family == AF_PACKET) { struct sockaddr_ll* l = (void*)a->ifa_addr; if (l->sll_halen == 6) { memcpy(v[k].mac, l->sll_addr, 6); v[k].has_mac = 1; } }
	}
	freeifaddrs(ifa);
	for (int i = 0; i < n; i++) for (int j = i + 1; j < n; j++) if (v[j].idx < v[i].idx) { struct nif t = v[i]; v[i] = v[j]; v[j] = t; }
	int en = 0;
	for (int i = 0; i < n; i++) { if (v[i].flags & IFF_LOOPBACK) snprintf(v[i].dname, 16, "lo0"); else snprintf(v[i].dname, 16, "en%d", en++); }
	return n;
}
// nombre de macOS -> de Linux (para las ioctl por nombre)
int net_name_to_linux(const char* d, char* out, size_t cap) {
	struct nif v[32]; int n = nif_list(v, 32);
	for (int i = 0; i < n; i++) if (!strcmp(v[i].dname, d)) { snprintf(out, cap, "%s", v[i].lname); return 0; }
	return -1;
}
static uint32_t dflags(unsigned l) { uint32_t d = l & 0x3ff; if (l & IFF_MULTICAST) d |= 0x8000; d |= 0x800; /* SIMPLEX */ return d; }
static size_t put_sa(uint8_t* o, const struct sockaddr* sa) {
	if (!sa) return 0;
	if (sa->sa_family == AF_INET) { const struct sockaddr_in* s4 = (const void*)sa; o[0] = 16; o[1] = 2; memcpy(o + 2, &s4->sin_port, 2); memcpy(o + 4, &s4->sin_addr, 4); return 16; }
	if (sa->sa_family == AF_INET6) { const struct sockaddr_in6* s6 = (const void*)sa; o[0] = 28; o[1] = 30; memcpy(o + 2, &s6->sin6_port, 2); memcpy(o + 4, &s6->sin6_flowinfo, 4); memcpy(o + 8, &s6->sin6_addr, 16); memcpy(o + 24, &s6->sin6_scope_id, 4); return 28; }
	return 0;
}
static long net_iflist(struct ctx* c, int af, int v2, int only) {
	struct nif v[32]; int n = nif_list(v, 32);
	struct ifaddrs* ifa; if (getifaddrs(&ifa) != 0) return -D_ENOENT;
	size_t cap = 65536, k = 0; uint8_t* b = calloc(1, cap);
	for (int i = 0; i < n; i++) {
		if (only && (int)v[i].idx != only) continue;
		size_t h = v2 ? 160 : 112, start = k;          // if_msghdr2 (con if_data64) / if_msghdr (con if_data)
		uint8_t* m = b + k; memset(m, 0, h);
		m[2] = 5; m[3] = v2 ? 0x12 : 0xe;              // RTM_VERSION, RTM_IFINFO2 / RTM_IFINFO
		int32_t addrs = 0x10, fl = (int32_t)dflags(v[i].flags); uint16_t idx = (uint16_t)v[i].idx;
		memcpy(m + 4, &addrs, 4); memcpy(m + 8, &fl, 4); memcpy(m + 12, &idx, 2);
		uint8_t* d = m + (v2 ? 32 : 16);               // if_data: tipo, longitud de dirección, MTU
		d[0] = (v[i].flags & IFF_LOOPBACK) ? 24 : 6; d[3] = v[i].has_mac ? 6 : 0; uint32_t mtu = (v[i].flags & IFF_LOOPBACK) ? 16384 : 1500; memcpy(d + 8, &mtu, 4);
		k += h;
		uint8_t* dl = b + k; size_t nl = strlen(v[i].dname), al = v[i].has_mac ? 6 : 0, dlen = 8 + nl + al; if (dlen < 20) dlen = 20;
		dl[0] = (uint8_t)dlen; dl[1] = 18; memcpy(dl + 2, &idx, 2); dl[4] = d[0]; dl[5] = (uint8_t)nl; dl[6] = (uint8_t)al;
		memcpy(dl + 8, v[i].dname, nl); if (al) memcpy(dl + 8 + nl, v[i].mac, 6);
		k += SA_RLEN(dlen);
		uint16_t ml = (uint16_t)(k - start); memcpy(b + start, &ml, 2);
		for (struct ifaddrs* a = ifa; a; a = a->ifa_next) {
			if (strcmp(a->ifa_name, v[i].lname) || !a->ifa_addr) continue;
			int fam = a->ifa_addr->sa_family;
			if (fam != AF_INET && fam != AF_INET6) continue;
			if (af && !((af == 2 && fam == AF_INET) || (af == 30 && fam == AF_INET6))) continue;
			size_t as = k; uint8_t* am = b + k; memset(am, 0, 20);
			am[2] = 5; am[3] = 0xc;                    // RTM_NEWADDR
			int32_t aa = 0; k += 20;
			uint8_t tmp[32]; size_t l;
			if ((l = put_sa(tmp, a->ifa_netmask))) { tmp[1] = 0; if (a->ifa_netmask->sa_family == AF_INET) tmp[1] = 2; else tmp[1] = 30; memcpy(b + k, tmp, l); k += SA_RLEN(l); aa |= 0x4; }
			if ((l = put_sa(b + k, a->ifa_addr))) { k += SA_RLEN(l); aa |= 0x20; }
			if (fam == AF_INET) {                              // RTA_BRD siempre en IPv4: broadcast/destino, o la propia dirección (lo0)
				const struct sockaddr* bd = ((a->ifa_flags & (IFF_BROADCAST | IFF_POINTOPOINT)) && a->ifa_broadaddr) ? a->ifa_broadaddr : a->ifa_addr;
				if ((l = put_sa(b + k, bd))) { k += SA_RLEN(l); aa |= 0x80; }
			}
			memcpy(am + 4, &aa, 4); memcpy(am + 8, &fl, 4); memcpy(am + 12, &idx, 2);
			uint16_t al2 = (uint16_t)(k - as); memcpy(am, &al2, 2);
		}
	}
	freeifaddrs(ifa);
	long r = sc_out(c, b, k); free(b); return r;
}

// Tabla de rutas (NET_RT_DUMP): un rt_msghdr (92 bytes) + destino, puerta de enlace y máscara por ruta IPv4 de
// /proc/net/route. Las rutas directas llevan como puerta de enlace un sockaddr_dl de la interfaz, como en macOS.
static size_t sin_put(uint8_t* o, uint32_t a) { memset(o, 0, 16); o[0] = 16; o[1] = 2; memcpy(o + 4, &a, 4); return 16; }
static long net_rtdump(struct ctx* c, int af) {
	if (af && af != 2) return sc_out(c, "", 0);
	FILE* f = fopen("/proc/net/route", "r"); if (!f) return -D_ENOENT;
	struct nif v[32]; int nn = nif_list(v, 32);
	size_t cap = 65536, k = 0; uint8_t* b = calloc(1, cap);
	char line[512]; if (!fgets(line, sizeof line, f)) line[0] = 0;
	while (fgets(line, sizeof line, f) && k + 256 < cap) {
		char ifn[32]; unsigned dst, gw, fl, refc, use, met, mask, mtu, win, irtt;
		if (sscanf(line, "%31s %x %x %x %u %u %u %x %u %u %u", ifn, &dst, &gw, &fl, &refc, &use, &met, &mask, &mtu, &win, &irtt) < 8) continue;
		uint16_t idx = (uint16_t)if_nametoindex(ifn);
		const char* dn = ifn; for (int i = 0; i < nn; i++) if (!strcmp(v[i].lname, ifn)) dn = v[i].dname;
		uint8_t* m = b + k; size_t st = k; memset(m, 0, 92);
		m[2] = 5; m[3] = 4;                                        // RTM_VERSION, RTM_GET
		memcpy(m + 4, &idx, 2);
		int32_t rf = (int32_t)(fl & 0x7) | 0x800 /* RTF_STATIC */, addrs = 0x1 | 0x2 | 0x4;
		if (mask == 0xffffffffu) rf |= 0x4;
		memcpy(m + 8, &rf, 4); memcpy(m + 12, &addrs, 4);
		k += 92;
		k += sin_put(b + k, dst);
		if (fl & 2) k += sin_put(b + k, gw);
		else {                                                     // puerta de enlace = la interfaz (AF_LINK)
			uint8_t* dl = b + k; size_t nl = strlen(dn); memset(dl, 0, 20); dl[0] = 20; dl[1] = 18; memcpy(dl + 2, &idx, 2); dl[5] = (uint8_t)nl; memcpy(dl + 8, dn, nl < 12 ? nl : 12); k += 20;
		}
		k += sin_put(b + k, mask);
		uint16_t ml = (uint16_t)(k - st); memcpy(b + st, &ml, 2);
	}
	fclose(f);
	long r = sc_out(c, b, k); free(b); return r;
}

// ---- Identificadores especiales {0, x, oid...} de XNU (kern_newsysctl.c), que usa la orden sysctl(8):
//   {0,1,oid} nombre del oid; {0,2,oid} el oid siguiente en orden (para recorrer el árbol con sysctl -a);
//   {0,4,oid} formato: uint32 kind (CTLTYPE_* | CTLFLAG_*) seguido de la cadena de formato ("I", "Q", "A", "S,timeval").
// Solo se conocen los oid hoja de la tabla (dos niveles); un prefijo cuenta como nodo para "siguiente".
#define CTLTYPE_INT 2
#define CTLTYPE_STRING 3
#define CTLTYPE_QUAD 4
#define CTLTYPE_STRUCT 5
#define CTLFLAG_RD 0x80000000u
static int mib_cmp(const int* a, const int* b) { return a[0] != b[0] ? (a[0] < b[0] ? -1 : 1) : a[1] != b[1] ? (a[1] < b[1] ? -1 : 1) : 0; }
static const struct entry* entry_by_mib(const int* m) {
	for (size_t i = 0; i < NENT; i++) if (!mib_cmp(table[i].mib, m)) return &table[i];
	return NULL;
}
static long sysctl_meta(struct ctx* c, int op, const int* oid, uint32_t len) {
	if (op == 2) {                                       // siguiente: el menor oid de la tabla mayor que el dado
		int from[2] = { len >= 1 ? oid[0] : -1, len >= 2 ? oid[1] : -1 };
		if (len == 1) from[1] = -1;                      // un nodo: su primer hijo es el siguiente
		const struct entry* best = NULL;
		for (size_t i = 0; i < NENT; i++) {
			if (len && mib_cmp(table[i].mib, from) <= 0) continue;
			if (!best || mib_cmp(table[i].mib, best->mib) < 0) best = &table[i];
		}
		if (!best) return -D_ENOENT;
		return sc_out(c, best->mib, sizeof best->mib);
	}
	if (len != 2) return -D_ENOENT;
	const struct entry* e = entry_by_mib(oid);
	if (!e) return -D_ENOENT;
	if (op == 1) return sc_out(c, e->name, strlen(e->name) + 1);
	uint8_t b[32] = { 0 };                              // op 4: formato
	uint32_t kind = CTLFLAG_RD;
	const char* fmt;
	switch (e->kind) {
	case K_INT:  kind |= CTLTYPE_INT; fmt = "I"; break;
	case K_QUAD: kind |= CTLTYPE_QUAD; fmt = "Q"; break;
	case K_STR:  kind |= CTLTYPE_STRING; fmt = "A"; break;
	case K_LOADAVG: kind |= CTLTYPE_STRUCT; fmt = "S,loadavg"; break;
	case K_SWAP: kind |= CTLTYPE_STRUCT; fmt = "S,xsw_usage"; break;
	default:     kind |= CTLTYPE_STRUCT; fmt = "S,timeval"; break;
	}
	memcpy(b, &kind, 4);
	size_t fl = strlen(fmt) + 1;
	memcpy(b + 4, fmt, fl);
	return sc_out(c, b, 4 + fl);
}

long bsd_sysctl(struct ctx* c) {
	int mib[16];
	uint32_t n = (uint32_t)c->a[1];
	if (n == 0 || n > 16 || safe_read(c->a[0], mib, n * 4) != (ssize_t)(n * 4)) return -D_EINVAL;

	// vfs.generic: sistemas de archivos que tiene el núcleo (getvfsbyname). VFS_MAXTYPENUM (1) y VFS_CONF (2, struct vfsconf:
	// reservado, nombre[15], typenum, refcount, flags, 2 reservados = 40 bytes). Son los que tahoe-run sabe presentar o montar,
	// con los mismos números de tipo que statfs (emu_fs.c): mount_tmpfs ve así que tmpfs ya está y no intenta cargar su kext.
	if (n >= 3 && mib[0] == 3 && mib[1] == 0 && (mib[2] == 1 || (mib[2] == 2 && n == 4))) {
		static const struct { const char* name; int typenum; } vfs[] = { { "devfs", 0x13 }, { "apfs", 0x18 }, { "tmpfs", 0x1d } };
		if (mib[2] == 1) { int32_t max = 0x1e; return sc_out(c, &max, 4); }
		for (size_t i = 0; i < sizeof vfs / sizeof vfs[0]; i++) if (vfs[i].typenum == mib[3]) {
			uint8_t vc[40] = { 0 };
			strncpy((char*)vc + 4, vfs[i].name, 14);
			memcpy(vc + 20, &vfs[i].typenum, 4);
			int32_t refs = 1; memcpy(vc + 24, &refs, 4);
			return sc_out(c, vc, sizeof vc);
		}
		return -45;                                        // ENOTSUP: no hay sistema de archivos con ese número (como XNU)
	}
	if (n >= 3 && mib[0] == 1 && mib[1] == 14) {         // kern.proc.*: KERN_PROC_ALL (0), KERN_PROC_PID (1)
		uint8_t one[648];
		if (n == 4 && mib[2] == 1) {
			if (fill_kinfo(mib[3], one) < 0) { uint64_t z = 0; safe_write(c->a[3], &z, 8); return -3; /* ESRCH */ }
			return sc_out(c, one, sizeof one);
		}
		if (mib[2] >= 0 && mib[2] <= 6 && mib[2] != 1) {      // ALL 0, PGRP 2, SESSION 3, TTY 4, UID 5, RUID 6
			if (trace_all || access("/dev/shm/tahoe-mach-trace", F_OK) == 0) logf_("    sysctl kern.proc sub=%d arg=%d\n", mib[2], n >= 4 ? mib[3] : -1);
			DIR* d = opendir("/proc");
			size_t cap = 64 * 648, used = 0;
			uint8_t* all = malloc(cap);
			struct dirent* de;
			while (d && (de = readdir(d))) {
				int pid = atoi(de->d_name);
				if (pid <= 0 || !all) continue;
				if (used + 648 > cap) { cap *= 2; all = realloc(all, cap); }
				if (fill_kinfo(pid, all + used) != 0) continue;
				uint8_t* k = all + used; int32_t pg, ui; memcpy(&pg, k + 564, 4); memcpy(&ui, k + 416 + 4, 4);
				int arg = n >= 4 ? mib[3] : 0, keep = 1;
				if (mib[2] == 2) keep = pg == arg;
				else if (mib[2] == 3) keep = getsid(pid) == arg;
				else if (mib[2] == 5 || mib[2] == 6) keep = ui == arg;
				if (keep) used += 648;
			}
			if (d) closedir(d);
			long r = sc_out(c, all, used);
			free(all);
			return r;
		}
	}
	if (n == 6 && mib[0] == 4 && mib[1] == 17 && (mib[4] == 1 || mib[4] == 7)) return net_rtdump(c, mib[3]);   // NET_RT_DUMP(2)
	if (n == 6 && mib[0] == 4 && mib[1] == 17 && (mib[4] == 3 || mib[4] == 6)) return net_iflist(c, mib[3], mib[4] == 6, mib[5]);   // NET_RT_IFLIST(2)
	if (n == 3 && mib[0] == 1 && mib[1] == 49) {         // KERN_PROCARGS2: argc, ruta del ejecutable, argv y entorno
		int pid = mib[2];
		if (!is_tahoe_proc(pid)) return -D_EINVAL;
		char av[16384]; size_t al = 0;
		int argc = guest_argv(pid, av, sizeof av, &al);
		if (argc <= 0) return -D_EINVAL;
		size_t cap = 4 + al + 8 + al + 8192, k = 0;
		uint8_t* b = calloc(1, cap);
		memcpy(b, &argc, 4); k = 4;
		size_t pl = strlen(av) + 1; memcpy(b + k, av, pl); k += pl;           // ruta del ejecutable
		k = (k + 7) & ~(size_t)7;                                              // relleno de NUL hasta alinear
		memcpy(b + k, av, al); k += al;                                        // argv
		char ed[128], ep[160], env[8192];
		if (proc_dir(pid, ed, sizeof ed) != 0) snprintf(ed, sizeof ed, "/proc/%d", pid);
		snprintf(ep, sizeof ep, "%s/environ", ed);
		FILE* f = fopen(ep, "r");
		size_t en = f ? fread(env, 1, sizeof env - 1, f) : 0; if (f) fclose(f);
		for (size_t i = 0; i < en; i += strlen(env + i) + 1) {                  // entorno, sin las variables internas
			size_t l = strlen(env + i);
			if (!strncmp(env + i, "TAHOE_", 6) || k + l + 1 > cap) continue;
			memcpy(b + k, env + i, l + 1); k += l + 1;
		}
		long r = sc_out(c, b, k);
		free(b);
		return r;
	}
	if (n >= 2 && mib[0] == 0 && (mib[1] == 1 || mib[1] == 2 || mib[1] == 4)) return sysctl_meta(c, mib[1], mib + 2, n - 2);
	if (n == 2 && mib[0] == 0 && mib[1] == 3) {          // name2oid: nombre en newp -> identificador
		char name[256];
		size_t len = c->a[5] < sizeof name - 1 ? c->a[5] : sizeof name - 1;
		if (!c->a[4] || safe_read(c->a[4], name, len) != (ssize_t)len) return -D_EFAULT;
		name[len] = 0;
		for (size_t i = 0; i < NENT; i++)
			if (!strcmp(table[i].name, name)) return sc_out(c, table[i].mib, sizeof table[i].mib);
		if (trace_all || 1) logf_("    sysctl: nombre desconocido \"%s\"\n", name);
		return -D_ENOENT;
	}
	if (n == 3 && mib[0] == 0 && mib[1] == 0x7300) {     // sysctl.proc_cputype.<pid>: todos los procesos son x86_64 aquí
		char pp[32]; snprintf(pp, sizeof pp, "/proc/%d", mib[2]);
		if (mib[2] < 0 || access(pp, F_OK) != 0) return -D_ESRCH;
		int v = 0x01000007;                                  // CPU_TYPE_X86_64
		return sc_out(c, &v, sizeof v);
	}
	if (n == 2 && mib[0] == CTL_KERN && mib[1] == 0x7005) {   // kern.osvariant_status: lectura y escritura como XNU
		if (c->a[4]) {
			uint64_t v = 0;
			if (c->a[5] != 8 || safe_read(c->a[4], &v, 8) != 8) return -D_EINVAL;
			if (osvariant_status_set(v) != 0) return -D_EPERM;
			if (trace_all || mach_trace()) logf_("    sysctl kern.osvariant_status <- 0x%lx\n", (unsigned long)v);
			if (!c->a[2]) return 0;
		}
		uint64_t v = osvariant_status_get();
		return sc_out(c, &v, sizeof v);
	}
	if (n == 2) {
		for (size_t i = 0; i < NENT; i++) {
			if (table[i].mib[0] != mib[0] || table[i].mib[1] != mib[1]) continue;
			const struct entry* e = &table[i];
			if (trace_all) logf_("    sysctl %s\n", e->name);
			switch (e->kind) {
			case K_INT:  { int v = (int)e->ival; return sc_out(c, &v, sizeof v); }
			case K_QUAD: { long long v = e->ival; return sc_out(c, &v, sizeof v); }
			case K_STR:  return sc_out(c, e->sval, strlen(e->sval) + 1);
			case K_LOADAVG: {                                   // struct loadavg { fixpt_t ldavg[3]; long fscale; }, FSCALE = 1 << 11
				double l[3] = { 0, 0, 0 };
				FILE* f = fopen("/proc/loadavg", "r");
				if (f) { if (fscanf(f, "%lf %lf %lf", &l[0], &l[1], &l[2]) != 3) l[0] = l[1] = l[2] = 0; fclose(f); }
				uint8_t la[24] = { 0 };
				for (int i = 0; i < 3; i++) { uint32_t v = (uint32_t)(l[i] * 2048); memcpy(la + i * 4, &v, 4); }
				long long fs = 2048; memcpy(la + 16, &fs, 8);
				return sc_out(c, la, sizeof la);
			}
			case K_SWAP: {                                      // struct xsw_usage { u64 total, avail, used; u32 pagesize; boolean_t encrypted; }
				unsigned long long tot = 0, fr = 0; char l[128];
				FILE* f = fopen("/proc/meminfo", "r");
				if (f) { while (fgets(l, sizeof l, f)) { sscanf(l, "SwapTotal: %llu", &tot); sscanf(l, "SwapFree: %llu", &fr); } fclose(f); }
				uint8_t x[32] = { 0 };
				uint64_t v[3] = { tot * 1024, fr * 1024, (tot - fr) * 1024 }; uint32_t pg = 4096, enc = 1;
				memcpy(x, v, 24); memcpy(x + 24, &pg, 4); memcpy(x + 28, &enc, 4);
				return sc_out(c, x, sizeof x);
			}
			case K_BOOT: {
				struct timespec up, now;
				clock_gettime(CLOCK_BOOTTIME, &up);
				clock_gettime(CLOCK_REALTIME, &now);
				long long tv[2] = { now.tv_sec - up.tv_sec, (now.tv_nsec - up.tv_nsec) / 1000 };
				if (tv[1] < 0) { tv[1] += 1000000; tv[0]--; }
				return sc_out(c, tv, 16);
			}
			}
		}
	}
	logf_("    sysctl: identificador desconocido [");
	for (uint32_t i = 0; i < n; i++) logf_("%s%d", i ? "." : "", mib[i]);
	logf_("]\n");
	return -D_ENOENT;
}
