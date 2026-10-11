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

enum kind { K_INT, K_QUAD, K_STR, K_BOOT };
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
		if (fgets(b, sizeof b, bf) && strlen(b) >= 36) { for (int i = 0; i < 36; i++) boot_session[i] = (b[i] >= 'a' && b[i] <= 'f') ? (char)(b[i] - 32) : b[i]; boot_session[36] = 0; }
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
	char p[64], e[512]; snprintf(p, sizeof p, "/proc/%d/exe", pid);
	ssize_t n = readlink(p, e, sizeof e - 1);
	if (n <= 0 || sl <= 0) return 0;
	e[n] = 0;
	return !strcmp(e, self);
}
// argv de macOS del proceso: devuelve argc y deja los argumentos (separados por NUL) en out
static int guest_argv(int pid, char* out, size_t cap, size_t* len) {
	char p[64], buf[16384]; snprintf(p, sizeof p, "/proc/%d/cmdline", pid);
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
	char p[64], env[32768]; snprintf(p, sizeof p, "/proc/%d/environ", pid);
	FILE* f = fopen(p, "r"); if (!f) return 0;
	size_t n = fread(env, 1, sizeof env - 1, f); fclose(f); env[n] = 0;
	for (size_t i = 0; i < n; i += strlen(env + i) + 1)
		if (!strncmp(env + i, "TAHOE_CRED=", 11)) { unsigned a, b; if (sscanf(env + i + 11, "%u,%u", &a, &b) == 2) return b; }
	return 0;
}
static int fill_kinfo(int pid, uint8_t* o) {
	if (!is_tahoe_proc(pid)) return -1;
	char path[64], buf[512];
	snprintf(path, sizeof path, "/proc/%d/stat", pid);
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
	return 0;
}

long bsd_sysctl(struct ctx* c) {
	int mib[16];
	uint32_t n = (uint32_t)c->a[1];
	if (n == 0 || n > 16 || safe_read(c->a[0], mib, n * 4) != (ssize_t)(n * 4)) return -D_EINVAL;

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
		char ep[64], env[8192]; snprintf(ep, sizeof ep, "/proc/%d/environ", pid);
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
	if (n == 2) {
		for (size_t i = 0; i < NENT; i++) {
			if (table[i].mib[0] != mib[0] || table[i].mib[1] != mib[1]) continue;
			const struct entry* e = &table[i];
			if (trace_all) logf_("    sysctl %s\n", e->name);
			switch (e->kind) {
			case K_INT:  { int v = (int)e->ival; return sc_out(c, &v, sizeof v); }
			case K_QUAD: { long long v = e->ival; return sc_out(c, &v, sizeof v); }
			case K_STR:  return sc_out(c, e->sval, strlen(e->sval) + 1);
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
