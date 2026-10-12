// Mach IPC: mach_msg2 (trampa 47) hacia objetos del kernel (tarea, host, hilo).
//
// macOS 26 llama a las rutinas MIG del kernel (mach_vm_map, host_info...) con mach_msg2 y la opción
// MACH64_SEND_KOBJECT_CALL, sin vector: el mensaje completo (cabecera incluida) está en `data` y la respuesta
// se escribe en ese mismo búfer (hasta rcv_size bytes), seguida de un trailer de 8 bytes.
//
// Aquí el "kernel" es este proceso: cada identificador de mensaje se atiende en una función. Lo que no se
// conozca recibe la respuesta estándar de MIG "identificador erróneo" (MIG_BAD_ID) y se registra.
#define _GNU_SOURCE
#include <sys/syscall.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/sysinfo.h>
#include <unistd.h>

#include <time.h>
#include <dirent.h>
#include <fcntl.h>
#include <stddef.h>
#include "tahoe.h"
#include "nucleo.h"

#define MACH_MSG_SUCCESS     0
#define MACH_RCV_TOO_LARGE   0x10004004
#define MIG_BAD_ID           (-303)
#define KERN_SUCCESS_        0
#define KERN_INVALID_ADDR    1
#define KERN_NO_SPACE_       3
#define KERN_INVALID_ARG     4

struct hdr { uint32_t bits, size, remote, local, voucher; int32_t id; };

// ---------------------------------------------------------------- volcado para depurar
static void hexdump(const uint8_t* b, size_t len) {
	for (size_t i = 0; i < len; i += 16) {
		char line[100];
		int n = snprintf(line, sizeof line, "      %04zx ", i);
		for (size_t j = i; j < i + 16 && j < len; j++) n += snprintf(line + n, sizeof line - n, " %02x", b[j]);
		logf_("%s\n", line);
	}
}

// ---------------------------------------------------------------- construcción de respuestas
// Respuesta MIG: cabecera (24) + NDR (8) + código de retorno (4) + datos. Todo empaquetado a 4 bytes.
static const uint8_t NDR_LE[8] = { 0, 0, 0, 0, 1, 0, 0, 0 };   // little-endian, ASCII, IEEE

static size_t reply_begin(uint8_t* r, const struct hdr* req, int32_t retcode) {
	struct hdr h = { .bits = 0, .size = 0, .remote = 0, .local = 0, .voucher = 0, .id = req->id + 100 };
	memcpy(r, &h, sizeof h);
	memcpy(r + 24, NDR_LE, 8);
	memcpy(r + 32, &retcode, 4);
	return 36;
}
static void reply_end(uint8_t* r, size_t len) { memcpy(r + 4, &(uint32_t){ (uint32_t)len }, 4); }


#include "emu_iokit.inc"

// ---- entradas de memoria (mach_make_memory_entry): se respaldan con un archivo en /dev/shm, nombrado por el puerto.
// El rango del creador se sustituye por una proyección compartida del archivo para que el receptor vea sus escrituras.
static void me_path(char* b, size_t n, uint32_t port) { snprintf(b, n, "/dev/shm/tahoe-me-%s-%u", getenv("TAHOE_PORTS_OWNER") ? getenv("TAHOE_PORTS_OWNER") : "x", port); }
static size_t mig_make_memory_entry(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	uint64_t size, off;
	memcpy(&size, m + 48, 8); memcpy(&off, m + 56, 8);
	uint64_t len = round_up(size);
	uint32_t port = port_create(0);
	char path[128]; me_path(path, sizeof path, port);
	int fd = open(path, O_RDWR | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
	int ok = fd >= 0 && ftruncate(fd, (off_t)len) == 0 && off != 0;
	if (ok) {
		uint8_t* tmp = malloc(len);
		memcpy(tmp, (void*)off, len);                           // lo que ya hubiera escrito el creador
		if (pwrite(fd, tmp, len, 0) != (ssize_t)len) ok = 0;
		free(tmp);
		if (ok && mmap((void*)off, len, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, fd, 0) == MAP_FAILED) ok = 0;
	}
	if (fd >= 0) close(fd);
	if (mach_trace()) { uint32_t perm = 0; memcpy(&perm, m + 64, 4); logf_("    MT pid=%d make_memory_entry tamaño=0x%lx dir=0x%lx permiso=0x%x -> puerto 0x%x %s\n", (int)getpid(), size, off, perm, port, ok ? "ok" : "FALLO"); }
	if (!ok) return reply_begin(r, req, KERN_INVALID_ARG);
	size_t n = reply_port(r, req, port, 17);
	memcpy(r + n, NDR_LE, 8); memcpy(r + n + 8, &len, 8);
	n += 16;
	return n;
}

// ---------------------------------------------------------------- rutinas MIG
// mach_vm_map (id 4811): cabecera, 1 descriptor de puerto (objeto), NDR, address, size, mask, flags, offset,
// copy, cur_protection, max_protection, inheritance. Solo se soporta memoria anónima (objeto nulo).
static size_t mig_mach_vm_map(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	if (req->size < 100) return reply_begin(r, req, KERN_INVALID_ARG);
	uint32_t objname;
	uint64_t addr, size, mask;
	int32_t flags, cur;
	memcpy(&objname, m + 28, 4);
	memcpy(&addr, m + 48, 8);
	memcpy(&size, m + 56, 8);
	memcpy(&mask, m + 64, 8);
	memcpy(&flags, m + 72, 4);
	memcpy(&cur, m + 88, 4);
	int32_t kr = KERN_SUCCESS_;
	int ttype; int32_t tpid;
	if (objname && kobj_lookup(req->remote, &ttype, &tpid) && ttype == KO_TASK && tpid != (int32_t)getpid()) {
		// la tarea destino es otro proceso: se reserva el hueco en la zona común y él lo mapeará al usarlo
		uint64_t moff; memcpy(&moff, m + 76, 8);
		char path[128]; me_path(path, sizeof path, objname);
		addr = rmap_add(tpid, path, moff, round_up(size), cur & 7, mask + 1);
		if (!addr) kr = KERN_NO_SPACE_;
		goto done;
	}
	if (objname) {                                          // objeto = entrada de memoria compartida
		uint64_t moff; memcpy(&moff, m + 76, 8);
		char path[128]; me_path(path, sizeof path, objname);
		int fd = open(path, O_RDWR | O_CLOEXEC);
		if (fd < 0) { kr = KERN_INVALID_ARG; goto done; }
		uint64_t len = round_up(size);
		void* p = mmap((flags & 1) ? NULL : (void*)addr, len, cur & 7, MAP_SHARED | ((flags & 1) ? 0 : ((flags & 0x4000) ? MAP_FIXED : MAP_FIXED_NOREPLACE)), fd, (off_t)moff);
		close(fd);
		if (p == MAP_FAILED) { kr = KERN_NO_SPACE_; goto done; }
		addr = (uint64_t)p;
	} else {
		int prot = cur & 7;
		uint64_t len = round_up(size);
		void* p;
		if (flags & 1) {                                        // VM_FLAGS_ANYWHERE
			uint64_t align = mask + 1 > PAGE ? mask + 1 : PAGE;
			uint8_t* q = mmap(NULL, len + align, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
			if (q == MAP_FAILED) { kr = KERN_NO_SPACE_; goto done; }
			uint64_t a = ((uint64_t)q + align - 1) & ~(align - 1);
			if (a > (uint64_t)q) munmap(q, a - (uint64_t)q);
			if ((uint64_t)q + len + align > a + len) munmap((void*)(a + len), (uint64_t)q + len + align - (a + len));
			p = (void*)a;
		} else {
			int extra = (flags & 0x4000) ? MAP_FIXED : MAP_FIXED_NOREPLACE;
			p = mmap((void*)addr, len, prot, MAP_PRIVATE | MAP_ANONYMOUS | extra, -1, 0);
			if (p == MAP_FAILED) { kr = KERN_NO_SPACE_; goto done; }
		}
		addr = (uint64_t)p;
	}
done:;
	if (mach_trace() && objname) logf_("    MT pid=%d vm_map objeto=0x%x tamaño=0x%lx flags=0x%x prot=%d -> 0x%lx kr=%d\n", (int)getpid(), objname, size, flags, cur, addr, kr);
	size_t n = reply_begin(r, req, kr);
	memcpy(r + n, &addr, 8);                                    // address (salida)
	return n + 8;
}

// mach_vm_remap (id 4813): cabecera, 1 descriptor de puerto (tarea origen), NDR, target_address, size, mask, flags,
// src_address, copy, inheritance -> target_address, cur_protection, max_protection. Mapea en esta tarea las páginas de otra
// zona suya (libobjc lo usa para sus trampolines de bloques: copia junto a una página de datos la página de código de la
// caché). Con copy=FALSE XNU comparte las páginas: en Linux solo se puede con mapeos compartidos (mremap con tamaño 0); en
// los privados (la caché, memoria anónima) se copia el contenido, que para código de solo lectura es lo mismo.
// La nueva zona lleva la protección del origen; flags: VM_FLAGS_ANYWHERE (1), VM_FLAGS_OVERWRITE (0x4000).
static int prot_of(uint64_t a) {
	FILE* f = fopen("/proc/self/maps", "r");
	if (!f) return -1;
	char ln[512]; int prot = -1;
	while (fgets(ln, sizeof ln, f)) {
		unsigned long lo, hi; char pr[5];
		if (sscanf(ln, "%lx-%lx %4s", &lo, &hi, pr) != 3 || a < lo || a >= hi) continue;
		prot = (pr[0] == 'r' ? 1 : 0) | (pr[1] == 'w' ? 2 : 0) | (pr[2] == 'x' ? 4 : 0);
		break;
	}
	fclose(f);
	return prot;
}
static size_t mig_mach_vm_remap(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	if (req->size < 92) return reply_begin(r, req, KERN_INVALID_ARG);
	uint32_t srctask; uint64_t addr, size, mask, src; int32_t flags, copy;
	memcpy(&srctask, m + 28, 4); memcpy(&addr, m + 48, 8); memcpy(&size, m + 56, 8); memcpy(&mask, m + 64, 8);
	memcpy(&flags, m + 72, 4); memcpy(&src, m + 76, 8); memcpy(&copy, m + 84, 4);
	int ttype; int32_t tpid;
	if ((kobj_lookup(req->remote, &ttype, &tpid) && ttype == KO_TASK && tpid != (int32_t)getpid()) ||
	    (kobj_lookup(srctask, &ttype, &tpid) && ttype == KO_TASK && tpid != (int32_t)getpid())) {
		logf_("    mach_vm_remap entre tareas distintas: sin implementar\n");
		return reply_begin(r, req, KERN_INVALID_ARG);
	}
	uint64_t s0 = src & ~(uint64_t)(PAGE - 1), len = round_up(src + size) - s0;
	int prot = prot_of(s0);
	if (!size || prot < 0) return reply_begin(r, req, 1);       // KERN_INVALID_ADDRESS
	int32_t kr = KERN_SUCCESS_;
	void* p = MAP_FAILED;
	uint64_t want = (flags & 1) ? 0 : (addr & ~(uint64_t)(PAGE - 1));
	if (!copy) {
		p = want ? mremap((void*)s0, 0, len, MREMAP_MAYMOVE | MREMAP_FIXED, (void*)want) : mremap((void*)s0, 0, len, MREMAP_MAYMOVE);
	}
	if (p == MAP_FAILED) {
		uint64_t align = mask + 1 > PAGE ? mask + 1 : PAGE;
		if (want) p = mmap((void*)want, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | ((flags & 0x4000) ? MAP_FIXED : MAP_FIXED_NOREPLACE), -1, 0);
		else {
			uint8_t* q = mmap(NULL, len + align, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
			if (q != MAP_FAILED) {
				uint64_t a = ((uint64_t)q + align - 1) & ~(align - 1);
				if (a > (uint64_t)q) munmap(q, a - (uint64_t)q);
				if ((uint64_t)q + len + align > a + len) munmap((void*)(a + len), (uint64_t)q + len + align - (a + len));
				p = (void*)a;
			}
		}
		if (p == MAP_FAILED) kr = KERN_NO_SPACE_;
		else {
			if (!(prot & 1)) mprotect((void*)s0, len, prot | PROT_READ);      // origen ilegible: leerlo para copiarlo
			memcpy(p, (void*)s0, len);
			if (!(prot & 1)) mprotect((void*)s0, len, prot);
			mprotect(p, len, prot);
		}
	}
	uint64_t out = p == MAP_FAILED ? 0 : (uint64_t)p + (src - s0);
	if (mach_trace()) logf_("    MT pid=%d vm_remap 0x%lx (0x%lx) copy=%d flags=0x%x -> 0x%lx kr=%d\n", (int)getpid(), src, size, copy, flags, out, kr);
	size_t n = reply_begin(r, req, kr);
	int32_t cur = prot, max = 7;
	memcpy(r + n, &out, 8); memcpy(r + n + 8, &cur, 4); memcpy(r + n + 12, &max, 4);
	return n + 16;
}

// host_info (id 200): NDR, flavor, capacidad. Solo HOST_BASIC_INFO (1): 12 enteros.
static size_t mig_host_info(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	int32_t flavor, cap;
	memcpy(&flavor, m + 32, 4);
	memcpy(&cap, m + 36, 4);
	if (flavor == 5 && cap >= 8) {                              // HOST_PRIORITY_INFO (valores de osfmk/kern/sched.h)
		int32_t prio[8] = { 80, 80, 64, 31, 0, 0, 0, 79 };
		size_t n = reply_begin(r, req, KERN_SUCCESS_);
		int32_t cnt = 8;
		memcpy(r + n, &cnt, 4);
		memcpy(r + n + 4, prio, sizeof prio);
		return n + 4 + sizeof prio;
	}
	if (flavor != 1 || cap < 12) {
		logf_("    host_info: sabor %d (capacidad %d) sin implementar\n", flavor, cap);
		return reply_begin(r, req, KERN_INVALID_ARG);
	}
	long ncpu = sysconf(_SC_NPROCESSORS_ONLN);
	uint64_t mem = (uint64_t)sysconf(_SC_PHYS_PAGES) * sysconf(_SC_PAGESIZE);
	int32_t info[12] = {
		(int32_t)ncpu, (int32_t)ncpu,                           // max_cpus, avail_cpus
		(int32_t)(mem > 0x7fffffff ? 0x7fffffff : mem),         // memory_size (32 bits)
		7, 8, 0,                                                // CPU_TYPE_X86, CPU_SUBTYPE_X86_64_H, threadtype
		(int32_t)(ncpu > 1 ? ncpu / 2 : 1), (int32_t)(ncpu > 1 ? ncpu / 2 : 1),   // physical_cpu(_max)
		(int32_t)ncpu, (int32_t)ncpu,                           // logical_cpu(_max)
		0, 0,                                                   // max_mem (64 bits), se rellena abajo
	};
	memcpy(&info[10], &mem, 8);
	size_t n = reply_begin(r, req, KERN_SUCCESS_);
	int32_t outcnt = 12;
	memcpy(r + n, &outcnt, 4);
	memcpy(r + n + 4, info, sizeof info);
	return n + 4 + sizeof info;
}

// host_get_clock_service (id 206): NDR + clock_id. La respuesta es un mensaje complejo con un descriptor de puerto
// (sin NDR ni código de retorno): cabecera (24) + cuerpo con 1 descriptor (4) + descriptor (12) = 40 bytes.
static size_t mig_host_get_clock_service(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	int32_t clock_id;
	memcpy(&clock_id, m + 32, 4);
	struct hdr h = { .bits = 0x80000000u, .size = 40, .remote = 0, .local = 0, .voucher = 0, .id = req->id + 100 };
	memcpy(r, &h, sizeof h);
	uint32_t ndesc = 1;
	memcpy(r + 24, &ndesc, 4);
	uint32_t name = 0x2203 + (uint32_t)clock_id * 0x100;       // nombre de puerto ficticio y estable por reloj
	uint8_t desc[12] = { 0 };
	memcpy(desc, &name, 4);
	desc[10] = 17;                                              // MACH_MSG_TYPE_PORT_SEND
	desc[11] = 0;                                               // MACH_MSG_PORT_DESCRIPTOR
	memcpy(r + 28, desc, 12);
	return 40;
}

// Asignador de nombres de puerto: en Mach un nombre es (índice << 8 | generación); aquí solo importa que sean únicos.

// Respuesta compleja con un único descriptor de puerto: cabecera (24) + cuerpo (4) + descriptor (12) = 40 bytes.
static size_t reply_port(uint8_t* r, const struct hdr* req, uint32_t name, uint8_t disposition) {
	struct hdr h = { .bits = 0x80000000u, .size = 40, .remote = 0, .local = 0, .voucher = 0, .id = req->id + 100 };
	memcpy(r, &h, sizeof h);
	uint32_t ndesc = 1;
	memcpy(r + 24, &ndesc, 4);
	uint8_t desc[12] = { 0 };
	memcpy(desc, &name, 4);
	desc[10] = disposition;                                     // p. ej. 17 = MACH_MSG_TYPE_PORT_SEND
	memcpy(r + 28, desc, 12);
	return 40;
}

// semaphore_create (id 3418): NDR, política, valor inicial. Devuelve un puerto de semáforo.
static size_t mig_semaphore_create(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	int32_t policy, value;
	memcpy(&policy, m + 32, 4);
	memcpy(&value, m + 36, 4);
	if (trace_all) logf_("    semaphore_create(política=%d, valor=%d)\n", policy, value);
	return reply_port(r, req, sem_create(value), 17);
}

// task_restartable_ranges_register (id 8000, subsistema task_restartable): libsystem registra rangos de código
// "reiniciables" para sus colas sin bloqueo. Es solo una optimización del kernel; se acepta sin hacer nada.
static size_t mig_task_restartable_register(const struct hdr* req, uint8_t* r) { return reply_begin(r, req, KERN_SUCCESS_); }

// task_get_special_port (id 3409): NDR + qué puerto. No hay launchd ni bootstrap: se devuelve el puerto nulo.
static size_t mig_task_get_special_port(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	int32_t which;
	memcpy(&which, m + 32, 4);
	if (trace_all) logf_("    task_get_special_port(%d)\n", which);
	return reply_port(r, req, special_port_get(which), 17);
}

// task_set_special_port (id 3410): descriptor de puerto + NDR + qué puerto. Se acepta sin efecto.
static size_t mig_task_set_special_port(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	uint32_t name, which;
	memcpy(&name, m + 28, 4);                                   // descriptor de puerto: nombre en +28
	memcpy(&which, m + 48, 4);                                  // cabecera 24 + cuerpo 4 + descriptor 12 + NDR 8
	special_port_set((int)which, name);
	if (trace_all) logf_("    task_set_special_port(%u) = 0x%x\n", which, name);
	return reply_begin(r, req, KERN_SUCCESS_);
}

// task_info (id 3405): NDR, sabor, capacidad (en enteros). Sabores soportados: 15 (TASK_AUDIT_TOKEN).
// Campos de /proc/<pid>/stat (o de un hilo: /proc/<tid>/stat) a partir del estado: f[0] = estado, f[11] utime, f[12] stime,
// f[15] prioridad, f[16] nice, f[17] hilos, f[20] vsize, f[21] rss (páginas). Devuelve el número de campos leídos.
static int stat_fields(const char* path, long long f[24], char* state) {
	char b[1024]; int fd = open(path, O_RDONLY | O_CLOEXEC); if (fd < 0) return 0;
	ssize_t n = read(fd, b, sizeof b - 1); close(fd);
	if (n <= 0) return 0;
	b[n] = 0;
	char* e = strrchr(b, ')'); if (!e || e[1] != ' ') return 0;
	*state = e[2];
	int k = 1; char* q = e + 3;
	while (k < 24 && *q) { while (*q == ' ') q++; if (!*q) break; f[k++] = strtoll(q, &q, 10); }
	return k;
}
// tiempo en ticks de reloj -> time_value_t {segundos, microsegundos}
static void ticks_tv(long long t, int32_t tv[2]) {
	long hz = sysconf(_SC_CLK_TCK); if (hz <= 0) hz = 100;
	tv[0] = (int32_t)(t / hz); tv[1] = (int32_t)((t % hz) * (1000000 / hz));
}
// prioridad de Mach a partir del nice de Linux: BASEPRI_DEFAULT (31) menos el nice, como hace XNU con setpriority
static int32_t mach_pri(long long nice) { long long p = 31 - nice; return (int32_t)(p < 0 ? 0 : p > 63 ? 63 : p); }
// estado de un hilo de Linux -> TH_STATE_* de Mach (RUNNING 1, STOPPED 2, WAITING 3, UNINTERRUPTIBLE 4, HALTED 5)
static int32_t mach_run_state(char st) { return st == 'R' ? 1 : st == 'T' || st == 't' ? 2 : st == 'D' ? 4 : st == 'Z' || st == 'X' ? 5 : 3; }

static size_t mig_task_info(const struct hdr* req, const uint8_t* m, uint8_t* r) {
	int32_t flavor, cap;
	memcpy(&flavor, m + 32, 4);
	memcpy(&cap, m + 36, 4);
	if (flavor == 15 && cap >= 8) {                             // audit_token_t: 8 enteros
		uint32_t tok[8] = { cred_auid(), cred_euid(), cred_egid(), cred_euid(), cred_egid(), (uint32_t)getpid(), cred_asid(), 1 };   // auid, euid, egid, ruid, rgid, pid, asid
		int ktype; int32_t kpid;
		if (kobj_lookup(req->remote, &ktype, &kpid) && (ktype == KO_TASK || ktype == KO_TASK_NAME) && kpid != (int32_t)getpid()) {
			// la tarea de otro proceso (task_name_for_pid...): su token, no el de quien pregunta. launchd lo usa como destino de
			// las señales que pide launchctl (proc_signal_delegate); con el propio se mandaba la señal a sí mismo.
			char d[128], p[160], ln[128]; unsigned ru = 0, eu = 0, rg = 0, eg = 0;
			FILE* f = NULL;
			if (proc_dir((int)kpid, d, sizeof d) == 0) { snprintf(p, sizeof p, "%s/status", d); f = fopen(p, "r"); }
			if (!f) return reply_begin(r, req, 5 /* KERN_FAILURE: la tarea ya no existe */);
			while (fgets(ln, sizeof ln, f)) {
				if (!strncmp(ln, "Uid:", 4)) sscanf(ln + 4, "%u %u", &ru, &eu);
				else if (!strncmp(ln, "Gid:", 4)) sscanf(ln + 4, "%u %u", &rg, &eg);
			}
			fclose(f);
			uint32_t o[8] = { (uint32_t)-1, eu, eg, ru, rg, (uint32_t)kpid, 100000, 1 };   // sesión del sistema (la propia del otro proceso no se conoce aquí)
			memcpy(tok, o, sizeof tok);
		}
		size_t n = reply_begin(r, req, KERN_SUCCESS_);
		int32_t cnt = 8;
		memcpy(r + n, &cnt, 4);
		memcpy(r + n + 4, tok, sizeof tok);
		return n + 4 + sizeof tok;
	}
	// Consumo de la tarea (la propia o la de otro proceso, p. ej. de task_read_for_pid para ps), desde /proc
	int ktype; int32_t kpid = (int32_t)getpid();
	if (req->remote != 0x103 && !(kobj_lookup(req->remote, &ktype, &kpid) && (ktype == KO_TASK || ktype == KO_TASK_NAME)))
		kpid = (int32_t)getpid();
	int need = flavor == 5 ? 10 : flavor == 20 ? 12 : flavor == 3 ? 4 : flavor == 10 ? 5 : flavor == 2 ? 8 : -1;
	if (need > 0) {
		if (cap < need) return reply_begin(r, req, KERN_INVALID_ARG);
		char d[128], p[160]; long long f[24] = { 0 }; char st = 0;
		if (proc_dir((int)kpid, d, sizeof d) != 0) return reply_begin(r, req, 5 /* KERN_FAILURE */);
		snprintf(p, sizeof p, "/proc/%d/stat", (int)kpid);       // del proceso entero (todos sus hilos), aunque el líder sea zombi
		if (stat_fields(p, f, &st) < 22) return reply_begin(r, req, 5);
		long pg = sysconf(_SC_PAGESIZE);
		long long g[24] = { 0 }; char gs = 0;                   // la memoria, de un hilo vivo (la del líder zombi es 0)
		snprintf(p, sizeof p, "%s/stat", d);
		if (stat_fields(p, g, &gs) < 22) memcpy(g, f, sizeof g);
		uint64_t vsz = (uint64_t)g[20], rss = (uint64_t)g[21] * (uint64_t)pg;
		int32_t ut[2], stm[2]; ticks_tv(f[11], ut); ticks_tv(f[12], stm);
		uint8_t o[64] = { 0 };
		if (flavor == 5) {                                        // TASK_BASIC_INFO_64 (empaquetado a 4)
			memcpy(o + 4, &vsz, 8); memcpy(o + 12, &rss, 8); memcpy(o + 20, ut, 8); memcpy(o + 28, stm, 8);
			int32_t pol = 1; memcpy(o + 36, &pol, 4);             // POLICY_TIMESHARE
		} else if (flavor == 20) {                                // MACH_TASK_BASIC_INFO
			memcpy(o, &vsz, 8); memcpy(o + 8, &rss, 8); memcpy(o + 16, &rss, 8); memcpy(o + 24, ut, 8); memcpy(o + 32, stm, 8);
			int32_t pol = 1; memcpy(o + 40, &pol, 4);
		} else if (flavor == 3) {                                 // TASK_THREAD_TIMES_INFO: de los hilos vivos
			memcpy(o, ut, 8); memcpy(o + 8, stm, 8);
		} else if (flavor == 10) {                                // TASK_SCHED_TIMESHARE_INFO: max, base, cur, depressed, depress
			int32_t pr = mach_pri(f[16]); int32_t v[5] = { 63, pr, pr, 0, 0 }; memcpy(o, v, 20);
		} else if (flavor == 2) {                                 // TASK_EVENTS_INFO: fallos de página; el resto sin contar
			int32_t v[8] = { (int32_t)(f[7] + f[9]), (int32_t)f[9], 0, 0, 0, 0, 0, 0 }; memcpy(o, v, 32);
		}
		size_t n = reply_begin(r, req, KERN_SUCCESS_);
		memcpy(r + n, &need, 4);
		memcpy(r + n + 4, o, (size_t)need * 4);
		return n + 4 + (size_t)need * 4;
	}
	logf_("    task_info: sabor %d (capacidad %d) sin implementar\n", flavor, cap);
	return reply_begin(r, req, KERN_INVALID_ARG);
}

// clock_get_time (id 1000, clock.defs): devuelve mach_timespec_t {u32 segundos, i32 nanosegundos}.
static size_t mig_clock_get_time(const struct hdr* req, uint8_t* r) {
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	size_t n = reply_begin(r, req, KERN_SUCCESS_);
	uint32_t v[2] = { (uint32_t)ts.tv_sec, (uint32_t)ts.tv_nsec };
	memcpy(r + n, v, 8);
	return n + 8;
}

// host_get_exception_ports (id 415): no hay manejadores de excepciones registrados -> 0 entradas. El stub de
// libsystem_kernel exige una respuesta compleja con SIEMPRE 32 descriptores de puerto (los no usados, nulos):
// cabecera(24) + nº de descriptores=32 (4) + 32*12 descriptores + NDR(8) + masksCnt=0 (los tres arrays comparten el contador): 424 bytes.
static size_t mig_host_get_exception_ports(const struct hdr* req, uint8_t* r) {
	size_t n = 24 + 4 + 32 * 12 + 8 + 4;
	struct hdr h = { .bits = 0x80000000u, .size = (uint32_t)n, .id = req->id + 100 };
	memset(r, 0, n);
	memcpy(r, &h, sizeof h);
	uint32_t cnt = 32;
	memcpy(r + 24, &cnt, 4);
	static const uint8_t ndr[8] = { 0, 0, 0, 0, 1, 0, 0, 0 };
	memcpy(r + 28 + 32 * 12, ndr, 8);
	return n;
}

// mach_ports_lookup3 (id 3404): puertos registrados por el padre con mach_ports_register. Respuesta compleja con
// exactamente 3 descriptores de puerto (cabecera 24 + cuenta 4 + 3*12 = 64 bytes), sin NDR ni retcode.
static size_t mig_mach_ports_lookup(const struct hdr* req, uint8_t* r) {
	uint32_t ports[3] = { special_port_get(4), 0, 0 };
	struct hdr h = { .bits = 0x80000000u, .size = 64, .id = req->id + 100 };
	memset(r, 0, 64);
	memcpy(r, &h, sizeof h);
	uint32_t three = 3;
	memcpy(r + 24, &three, 4);
	for (int i = 0; i < 3; i++) {
		memcpy(r + 28 + i * 12, &ports[i], 4);
		r[28 + i * 12 + 10] = 17;                      // disposición: MACH_MSG_TYPE_PORT_SEND; el tipo (byte 11) es 0 = descriptor de puerto
	}
	return 64;
}

// ---------------------------------------------------------------- mach_msg2
// Recepción desde un puerto de usuario: saca el mensaje, ajusta la cabecera como lo hace el kernel (el puerto de
// destino pasa a ser "local" y el de respuesta "remoto", con las disposiciones convertidas) y añade el trailer pedido.
static uint32_t rx_disp(uint32_t tx) {
	switch (tx) {
	case 17: case 19: case 20: return 17;        // MOVE_SEND / COPY_SEND / MAKE_SEND -> PORT_SEND
	case 18: case 21: return 18;                 // MOVE_SEND_ONCE / MAKE_SEND_ONCE -> PORT_SEND_ONCE
	default: return tx;
	}
}
// Recibe un mensaje de `rcv_name` en `buf` (capacidad `cap`) con el trailer que piden las opciones (bits 24-27).
// Devuelve 0 o un código mach_msg; *total = bytes escritos (mensaje + trailer).
// Cadenas legibles de un mensaje (de 6 caracteres o más, separadas por «|»), para las trazas: en XPC llevan las claves
// y valores del diccionario (nombres de rutinas, de servicios...). Devuelve su longitud.
static int msg_texts(const uint8_t* req, uint32_t size, char* s, size_t cap) {
	size_t k = 0, run = 0;
	for (uint32_t q = 24; q < size && k < cap - 2; q++) {
		uint8_t ch = req[q];
		if (ch >= 0x20 && ch < 0x7f) { s[k++] = (char)ch; run++; }
		else { if (run >= 6) s[k++] = 0x7c; else k -= run; run = 0; }
	}
	if (run < 6) k -= run;
	s[k] = 0;
	return (int)k;
}
long mach_rx_message(uint32_t rcv_name, int timeout_ms, uint64_t options, uint64_t buf, uint32_t cap, uint32_t* total) {
	uint32_t elems = (uint32_t)(options >> 24) & 0xf, tsize = elems == 0 ? 8 : elems == 1 ? 12 : elems == 2 ? 20 : elems == 3 ? 52 : 68;
	uint8_t* m; uint32_t size, snd[5] = { 0, 0, 0, (uint32_t)-1, 0 };
	int large = (options & 0x4) != 0;                         // MACH_RCV_LARGE
	int r = port_receive(rcv_name, timeout_ms, &m, &size, snd, cap > tsize ? cap - tsize : 1, large);
	if (r == 0x10004004 && large) {                           // el mensaje sigue en la cola; el receptor lee el tamaño de la cabecera
		struct hdr hh; memcpy(&hh, m, sizeof hh);
		safe_write(buf, m, sizeof hh < size ? sizeof hh : size);
		if (trace_all) logf_("    mach_msg: 0x%x demasiado grande (%u bytes, cabía %u): RCV_LARGE\n", rcv_name, size, cap);
		if (total) *total = size + tsize;                      // tamaño necesario (con trailer), para kevent ext[1]
		free(m);
		return r;
	}
	if (r) { if (r != 0x10004003) logf_("    mach_msg: <%d> recepción en 0x%x falló: 0x%x\n", (int)getpid(), rcv_name, r); return r; }
	struct hdr h;
	memcpy(&h, m, sizeof h);
	uint32_t txr = h.bits & 0xff, txl = (h.bits >> 8) & 0xff;
	uint32_t nb = (h.bits & ~0x1f1fu & ~0xffu) | (txl ? rx_disp(txl) : 0) | ((txr ? 16u : 0) << 8);
	uint32_t remote = h.local, local = h.remote;
	h.bits = nb; h.remote = remote; h.local = local;
	memcpy(m, &h, sizeof h);
	char rxports[96] = "";                                    // para la traza MSG: puertos que trae el mensaje
	{                                                          // complejo: el receptor ve los derechos de los descriptores ya convertidos
		struct nucleo_desc d[64];
		int nd = nucleo_descriptores(m, size, d, 64);
		size_t rk = 0;
		for (int i = 0; i < nd && i < 64; i++) {
			uint8_t cl = d[i].clase;
			if ((cl == NUCLEO_PUERTO || cl == NUCLEO_PUERTO_GUARDA) && rk < sizeof rxports - 24)
				rk += (size_t)snprintf(rxports + rk, sizeof rxports - rk, "%s0x%x%s", rk ? "," : " puertos=", d[i].nombre, port_exists(d[i].nombre) ? "" : "(muerto)");
			if ((trace_all || mach_trace()) && (cl == NUCLEO_PUERTO || cl == NUCLEO_PUERTO_GUARDA)) logf_("    mach_msg: <%d> descriptor de puerto%s 0x%x disposición %u\n", (int)getpid(), cl == NUCLEO_PUERTO_GUARDA ? " con guarda" : "", d[i].nombre, d[i].disp);
			// MOVE_RECEIVE: el receptor pasa a ser dueño (también en un puerto con guarda, que es como launchd entrega los
			// puertos de servicio)
			if ((cl == NUCLEO_PUERTO || cl == NUCLEO_PUERTO_GUARDA) && d[i].disp == 16) port_move_receive(d[i].nombre);
			if (cl == NUCLEO_PUERTO || cl == NUCLEO_PUERTOS) m[d[i].off + 10] = (uint8_t)rx_disp(d[i].disp);   // disposición vista por el receptor
		}
	}
	uint8_t tr[68] = { 0 };
	uint32_t seq = 0, uid = snd[1], gid = snd[2], at[8] = { snd[3], snd[1], snd[2], snd[1], snd[2], snd[0], snd[4], 1 };   // audit_token_t: auid, euid, egid, ruid, rgid, pid, asid, versión
	memcpy(tr + 4, &tsize, 4);
	if (tsize >= 12) memcpy(tr + 8, &seq, 4);
	if (tsize >= 20) { memcpy(tr + 12, &uid, 4); memcpy(tr + 16, &gid, 4); }
	if (tsize >= 52) memcpy(tr + 20, at, 32);
	if (tsize >= 60) { uint64_t ctx = port_get_context(local); memcpy(tr + 52, &ctx, 8); }   // msgh_context: contexto del puerto receptor
	long res = 0;
	if (size + tsize > cap) {                                 // sin RCV_LARGE: se entrega truncado y se descarta el resto
		uint32_t fit = cap > 24 ? cap - 24 : 0;
		safe_write(buf, m, cap < size ? cap : size);
		(void)fit;
		res = 0x10004004;
		if (total) *total = size + tsize;
	}
	else if (safe_write(buf, m, size) != (ssize_t)size || safe_write(buf + size, tr, tsize) != (ssize_t)tsize) res = MACH_RCV_INVALID_DATA_;
	else if (total) *total = size + tsize;
	if (trace_all) logf_("    mach_msg: recibe en 0x%x id=%d (%u bytes) -> 0x%lx\n", rcv_name, h.id, size, res);
	if (msg_trace()) {
		logf_("    MSG <%d.%ld> recibe en=0x%x dest=0x%x resp=0x%x id=%d tam=%u de=%d -> 0x%lx%s\n", (int)getpid(), (long)syscall(SYS_gettid), rcv_name, local, remote, h.id, size, (int)snd[0], res, rxports);
		if (size > 24) {                                        // cuerpo recibido, hasta 400 bytes (respuestas de procesos sin traza)
			static __thread char hx[2100];
			uint32_t end = size < 24 + 400 ? size : 24 + 400;
			for (uint32_t q = 24; q < end; q++) { static const char d[] = "0123456789abcdef"; hx[(q - 24) * 2] = d[m[q] >> 4]; hx[(q - 24) * 2 + 1] = d[m[q] & 15]; }
			hx[(end - 24) * 2] = 0;
			logf_("      MSG recibido <%d.%ld>: %s\n", (int)getpid(), (long)syscall(SYS_gettid), hx);
		}
	}
	if (mach_trace()) { uint64_t cx; memcpy(&cx, tr + 52, 8); logf_("    MT pid=%d.%ld rx destino=0x%x id=%d trailer=%u ctx=0x%lx\n", (int)getpid(), (long)syscall(SYS_gettid), local, h.id, tsize, tsize >= 60 ? cx : 0); }
	free(m);
	return res;
}

static long user_receive(struct ctx* c, uint64_t options, uint64_t buf, uint32_t rcv_name, uint32_t rcvsize) {
	int timeout = (options & 0x100) ? (int)ctx_arg(c, 7) : -1;
	if (trace_all && timeout < 0 && getenv("TAHOE_STACKS")) diag_stack(c->uc, "espera");   // quién se bloquea esperando
	if (g_watch) logf_("    WATCH recv espera puerto 0x%x plazo=%d opciones=0x%lx\n", rcv_name, timeout, options);
	long r = mach_rx_message(rcv_name, timeout, options, buf, rcvsize, NULL);
	// Errores de recepción que los clientes no esperan (libdispatch aborta con "Unexpected error from
	// mach_msg_receive"): siempre al registro, son raros. TIMED_OUT (0x10004003), INTERRUPTED (0x10004005) y
	// TOO_LARGE (0x10004004, se reintenta con un búfer mayor) son normales.
	if (r && r != 0x10004003 && r != 0x10004005 && r != 0x10004004)
		logf_("    mach_msg <%d:%ld>: recepción en 0x%x -> 0x%lx (opciones=0x%lx)\n", (int)getpid(), (long)syscall(SYS_gettid), rcv_name, r, options);
	if (access("/dev/shm/tahoe-mach-trace", F_OK) == 0) {
		uint32_t hh[6] = { 0 }; if (r == 0) safe_read(buf, hh, 24);
		logf_("    MT pid=%d.%ld recv port=0x%x r=0x%lx id=%d size=%u\n", (int)getpid(), (long)syscall(SYS_gettid), rcv_name, r, (int32_t)hh[5], hh[1]);
		if (r == 0 && hh[1] <= 192 && hh[1] >= 24) { uint32_t w[48] = { 0 }; safe_read(buf, w, hh[1]); char line[480]; int k = 0; for (uint32_t q = 0; q < hh[1] / 4; q++) k += snprintf(line + k, sizeof line - (size_t)k, " %08x", w[q]); logf_("      MT bytes <%d>:%s\n", (int)getpid(), line); }
		if (r == 0x10004003 && access("/dev/shm/tahoe-stacks", F_OK) == 0) diag_stack(c->uc, "espera");
	}
	return r;
}

// Con vectores, el núcleo escribe siempre una cabecera auxiliar vacía (mach_msg_aux_header_t: tamaño 8) tras recibir.
static long vec_aux(long r, uint64_t aux_addr, uint32_t aux_size) {
	if (r == 0 && aux_addr && aux_size >= 8) { uint32_t h[2] = { 8, 0 }; safe_write(aux_addr, h, 8); }
	return r;
}

static long mach_msg2(struct ctx* c) {
	uint64_t options = c->a[1];
	uint64_t buf = c->a[0];
	uint32_t ssize = (uint32_t)(c->a[2] >> 32);
	uint32_t rcvsize = (uint32_t)ctx_arg(c, 6);
	uint64_t rbuf = buf, aux_addr = 0; uint32_t aux_size = 0;
	if (options & 0x100000000ULL) {                         // MACH64_MSG_VECTOR: `data` apunta a mach_msg_vector_t[mensaje, auxiliar]
		uint32_t cnt = (uint32_t)(c->a[2] >> 32), rc = (uint32_t)ctx_arg(c, 6);
		if (rc > cnt) cnt = rc;
		if (cnt > 2) cnt = 2;
		if (!cnt) cnt = 1;
		uint64_t v[6] = { 0 };
		if (safe_read(c->a[0], v, cnt * 24) != (ssize_t)(cnt * 24)) return 0x10000002;
		buf = v[0]; rbuf = v[1] ? v[1] : v[0];
		ssize = (uint32_t)v[2]; rcvsize = (uint32_t)(v[2] >> 32);
		if (cnt == 2) { aux_addr = v[4] ? v[4] : v[3]; aux_size = (uint32_t)(v[5] >> 32); }
	}

	static int mtr = -1;
	if (mtr < 0) mtr = access("/dev/shm/tahoe-mach-trace", F_OK) == 0;   // traza ligera: solo envíos
	static __thread uint8_t reqbuf[512 * 1024];     // los mensajes entre procesos (listas de servicios...) pueden ser grandes
	uint8_t* req = reqbuf;
	uint8_t rep[1024];
	uint32_t rcv_name = (uint32_t)(c->a[5] >> 32);
	if (!(options & 1) && (options & 2)) return vec_aux(user_receive(c, options, rbuf, rcv_name, rcvsize), aux_addr, aux_size);
	if (ssize < sizeof(struct hdr) || ssize > sizeof reqbuf || safe_read(buf, req, ssize) != (ssize_t)ssize) {
		logf_("    mach_msg2: mensaje ilegible o fuera de tamaño (%u bytes)\n", ssize);
		return 0x10000002;                                      // MACH_SEND_INVALID_DATA
	}
	struct hdr h;
	memcpy(&h, req, sizeof h);
	// La cabecera de verdad viaja en registros (bits/tamaño, destino/local, voucher/id). Con SEND_KOBJECT_CALL el
	// búfer la repite; en mensajes a puertos normales el búfer puede no contenerla.
	uint64_t bs = c->a[2], rl = c->a[3], vi = c->a[4];
	h.bits = (uint32_t)bs; h.size = ssize;
	h.remote = (uint32_t)rl; h.local = (uint32_t)(rl >> 32);
	h.voucher = (uint32_t)vi; h.id = (int32_t)(vi >> 32);
	int kobject = (options & 0x200000000ULL) != 0;
	if (mtr) logf_("    MT pid=%d.%ld %s dest=0x%x(->%d) resp=0x%x id=%d size=%u rcv=%d\n", (int)getpid(), (long)syscall(SYS_gettid), kobject ? "kobj" : "send", h.remote, kobject ? 0 : port_rpid(h.remote), h.local, h.id, ssize, (options & 2) != 0);
	if (mtr && !kobject) { char s[200]; if (msg_texts(req, ssize, s, sizeof s)) logf_("      MT textos: %s\n", s); }
	if (!kobject && msg_trace()) {
		char s[200]; int t = msg_texts(req, ssize, s, sizeof s);
		logf_("    MSG <%d.%ld> envía dest=0x%x(->%d) resp=0x%x id=%d tam=%u bits=0x%x%s%s%s\n", (int)getpid(), (long)syscall(SYS_gettid), h.remote, port_rpid(h.remote), h.local, h.id, ssize, h.bits, (options & 2) ? " y espera" : "", t ? " | " : "", t ? s : "");
		if (ssize > 24) {                                       // el cuerpo en hexadecimal, hasta 400 bytes (mensajes.py descodifica XPC)
			static __thread char hx[2100];
			uint32_t end = ssize < 24 + 400 ? ssize : 24 + 400;
			for (uint32_t q = 24; q < end; q++) { static const char d[] = "0123456789abcdef"; hx[(q - 24) * 2] = d[req[q] >> 4]; hx[(q - 24) * 2 + 1] = d[req[q] & 15]; }
			hx[(end - 24) * 2] = 0;
			logf_("      MSG cuerpo <%d.%ld>: %s\n", (int)getpid(), (long)syscall(SYS_gettid), hx);
		}
	}
	if (!kobject && h.remote == 0) { if (trace_all) logf_("    mach_msg2: envío al puerto nulo (id=%d) -> MACH_SEND_INVALID_DEST\n", h.id); return 0x10000003; }   // el núcleo rechaza el destino nulo
	if (!kobject && (h.bits & 0x1f) == 18 && !port_exists(h.remote)) {   // respuesta (MOVE_SEND_ONCE) a un puerto que ya murió
		if (mtr) logf_("    MT pid=%d respuesta id=%d a 0x%x, que ya no existe -> MACH_SEND_INVALID_DEST\n", (int)getpid(), h.id, h.remote);
		return MACH_SEND_INVALID_DEST_;                         // XNU consume el derecho send-once y no lo entrega a nadie
	}
	if (!kobject && port_exists(h.remote)) {                    // envío a un puerto de usuario: se encola tal cual
		memcpy(req, &h, sizeof h);
		int sr = port_send_timeout(h.remote, req, ssize, (options & 0x10) ? (int)ctx_arg(c, 7) : -1, (options & 0x80) != 0);   // MACH_SEND_TIMEOUT, MACH_SEND_NOTIFY
		if (sr > 0) {                                             // plazo agotado con la cola llena, o el destino murió
			if (msg_trace()) logf_("    MSG <%d.%ld> envío a 0x%x id=%d falló 0x%x\n", (int)getpid(), (long)syscall(SYS_gettid), h.remote, h.id, sr);
			return sr;
		}
		if (sr != 0) {                                            // el mensaje no se pudo encolar: decirlo, no perderlo en silencio
			logf_("    mach_msg2: <%d> envío a 0x%x (id=%d, %u bytes) no encolado\n", (int)getpid(), h.remote, h.id, ssize);
			return port_exists(h.remote) ? 0x1000000d /* MACH_SEND_NO_BUFFER */ : MACH_SEND_INVALID_DEST_;
		}
		if (trace_all) logf_("    mach_msg2: envía a 0x%x (respuesta 0x%x) id=%d (%u bytes) bits=0x%x opciones=0x%lx\n", h.remote, h.local, h.id, ssize, h.bits, options);
		return (options & 2) ? vec_aux(user_receive(c, options, rbuf, rcv_name, rcvsize), aux_addr, aux_size) : MACH_MSG_SUCCESS;
	}
	size_t n;
	switch (h.id) {
	case 217: case 3616: case 3617: n = reply_begin(rep, &h, KERN_SUCCESS_); break;
	case 5403: n = reply_begin(rep, &h, 46 /* KERN_NOT_SUPPORTED */); break;   // mach_voucher_attr_command: sin vales de atributos
	case 3206: {                                                  // mach_port_deallocate (MIG): soltar un send-once avisa; un derecho de envío no destruye nada
		uint32_t nm; memcpy(&nm, req + 32, 4);
		port_release_send_once(nm);
		n = reply_begin(rep, &h, KERN_SUCCESS_);
		break;
	}
	// Ids de task sacados de los stubs de libsystem_kernel de macOS 26 (la copia de task.defs de XNU no coincide).
	case 3440: n = reply_begin(rep, &h, KERN_SUCCESS_); break;   // task_set_mach_voucher
	case 3442: n = reply_begin(rep, &h, 46 /* KERN_NOT_SUPPORTED */); break;   // task_generate_corpse: sin corpses
	case 3457: {                                                  // task_create_identity_token(task) -> token (puerto)
		n = reply_port(rep, &h, kobj_make(KO_IDENTITY_TOKEN, (int32_t)getpid()), 17);   // el token identifica a este proceso
		break;
	}
	case 3458: {                                                  // task_identity_token_get_task_port(token, flavor) -> tarea
		int type; int32_t pid;
		if (!kobj_lookup(h.remote, &type, &pid) || type != KO_IDENTITY_TOKEN) { n = reply_begin(rep, &h, 4 /* KERN_INVALID_ARGUMENT */); break; }
		if (pid != (int32_t)getpid() && kill(pid, 0) != 0) { n = reply_begin(rep, &h, 5 /* KERN_FAILURE: ya no existe */); break; }
		n = reply_port(rep, &h, pid == (int32_t)getpid() ? 0x103 : kobj_make(KO_TASK, pid), 17);
		break;
	}
	case 3455: {                                                  // _kernelrpc_mach_task_is_self(task) -> boolean
		n = reply_begin(rep, &h, KERN_SUCCESS_);
		uint32_t yes = h.remote == 0x103; memcpy(rep + n, &yes, 4); n += 4;   // PORT_TASK_SELF
		break;
	}
	case 3242: {                                                  // mach_port_assert_attributes(task, name, flavor, info[])
		uint32_t nm, fl, cnt; uint64_t g = 0; memcpy(&nm, req + 32, 4); memcpy(&fl, req + 36, 4); memcpy(&cnt, req + 40, 4);
		if (cnt >= 2 && ssize >= 52) memcpy(&g, req + 44, 8);
		n = reply_begin(rep, &h, fl == 8 ? port_assert_guard(nm, g) : 4 /* KERN_INVALID_ARGUMENT */);
		break;
	}
	case 3419: {                                                  // semaphore_destroy(task, semáforo): descriptor de puerto en el cuerpo
		uint32_t nm = 0; if (ssize >= 40) memcpy(&nm, req + 28, 4);
		n = reply_begin(rep, &h, machsem_destroy(nm));
		break;
	}
	case 3415: n = reply_begin(rep, &h, 5 /* KERN_FAILURE */); break;       // task_swap_exception_ports: sin puertos de excepción
	case 3200: {                                                  // mach_port_names(task) -> names[], types[] (dos OOL)
		static uint32_t nm[2048], ty[2048];
		int cnt = port_list_owned(nm, ty, 2048);
		struct hdr nh = { .bits = 0x80000000u, .size = 0, .remote = 0, .local = 0, .voucher = 0, .id = h.id + 100 };
		memcpy(rep, &nh, sizeof nh);
		uint32_t two = 2; memcpy(rep + 24, &two, 4);
		size_t off = 28;
		const uint32_t* arrs[2] = { nm, ty };
		for (int k = 0; k < 2; k++) {
			uint64_t addr = 0; size_t len = (size_t)cnt * 4;
			if (len) { void* mm = mmap(NULL, (len + 4095) & ~4095ul, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0); if (mm != MAP_FAILED) { memcpy(mm, arrs[k], len); addr = (uint64_t)mm; } else len = 0; }
			uint8_t dsc[16] = { 0 }; memcpy(dsc, &addr, 8); dsc[8] = 1; dsc[9] = 1; dsc[11] = 1; uint32_t l32 = (uint32_t)len; memcpy(dsc + 12, &l32, 4);
			memcpy(rep + off, dsc, 16); off += 16;
		}
		memcpy(rep + off, NDR_LE, 8); off += 8;
		uint32_t c32 = (uint32_t)cnt; memcpy(rep + off, &c32, 4); off += 4; memcpy(rep + off, &c32, 4); off += 4;
		n = off;
		break;
	}
	case 3612: {                                                  // thread_info(hilo, flavor, cuenta) -> info[cuenta]
		uint32_t flavor, cnt; memcpy(&flavor, req + 32, 4); memcpy(&cnt, req + 36, 4);
		uint32_t info[32] = { 0 }; uint32_t outc = 0;
		extern __thread uint32_t g_thread_port;
		extern uint64_t dispatch_queue_offset;
		int self = h.remote == g_thread_port;
		int kt; int32_t tid = 0;
		int other = !self && kobj_lookup(h.remote, &kt, &tid) && kt == KO_THREAD;   // un hilo de otro proceso (task_threads)
		if (other && (flavor == 3 || flavor == 10)) {
			char p[48]; long long f[24] = { 0 }; char st = 0;
			snprintf(p, sizeof p, "/proc/%d/stat", (int)tid);
			if (stat_fields(p, f, &st) < 17) { n = reply_begin(rep, &h, 5 /* KERN_FAILURE: el hilo terminó */); break; }
			if (flavor == 3) {                                    // THREAD_BASIC_INFO
				int32_t tv[2]; ticks_tv(f[11], tv); memcpy(info, tv, 8); ticks_tv(f[12], tv); memcpy(info + 2, tv, 8);
				info[5] = 1; info[6] = (uint32_t)mach_run_state(st); outc = 10;   // POLICY_TIMESHARE, estado
			} else {                                              // THREAD_SCHED_TIMESHARE_INFO
				int32_t pr = mach_pri(f[16]); info[0] = 63; info[1] = (uint32_t)pr; info[2] = (uint32_t)pr; outc = 5;
			}
		} else if (flavor == 3) {                                 // THREAD_BASIC_INFO: tiempos 0, ejecutándose (run_state 1)
			info[6] = 1; outc = 10;
		} else if (flavor == 4) {                                 // THREAD_IDENTIFIER_INFO: thread_id, thread_handle (TSD), dispatch_qaddr
			uint64_t tid = self ? (uint64_t)syscall(SYS_gettid) : h.remote, gs = 0;
			if (self) syscall(SYS_arch_prctl, 0x1004 /*ARCH_GET_GS*/, &gs);
			uint64_t dq = (self && gs && dispatch_queue_offset) ? gs + dispatch_queue_offset : 0;
			memcpy(info, &tid, 8); memcpy(info + 2, &gs, 8); memcpy(info + 4, &dq, 8); outc = 6;
		} else outc = 0;
		if (cnt < outc) outc = cnt;
		if (!outc && flavor != 3 && flavor != 4) { n = reply_begin(rep, &h, 4 /* KERN_INVALID_ARGUMENT */); break; }
		n = reply_begin(rep, &h, KERN_SUCCESS_);
		memcpy(rep + n, &outc, 4); n += 4;
		memcpy(rep + n, info, outc * 4); n += outc * 4;
		break;
	}   // host_request_notification / thread_policy(_set): sin efecto
	case 3402: {                                                  // task_threads(tarea) -> puertos de sus hilos (un OOL de puertos)
		// Solo para otros procesos (ps, top): un puerto por hilo de Linux vivo, que thread_info entiende. Para la propia tarea
		// no se ofrece: los puertos de hilo propios tienen otro esquema y nadie lo ha necesitado.
		int kt; int32_t kpid;
		if (h.remote == 0x103 || !kobj_lookup(h.remote, &kt, &kpid) || kt != KO_TASK || kpid == (int32_t)getpid()) {
			n = reply_begin(rep, &h, 46 /* KERN_NOT_SUPPORTED */); break;
		}
		static uint32_t th[1024]; int cnt = 0;
		char tp[48]; snprintf(tp, sizeof tp, "/proc/%d/task", (int)kpid);
		DIR* dd = opendir(tp); struct dirent* de;
		while (dd && cnt < 1024 && (de = readdir(dd))) {
			int tid = atoi(de->d_name); if (tid <= 0) continue;
			char sp[96]; long long f[24]; char st = 0; snprintf(sp, sizeof sp, "%s/%d/stat", tp, tid);
			if (stat_fields(sp, f, &st) && st != 'Z' && st != 'X') th[cnt++] = kobj_make(KO_THREAD, tid);
		}
		if (dd) closedir(dd);
		if (!cnt) { n = reply_begin(rep, &h, 5 /* KERN_FAILURE: la tarea terminó */); break; }
		struct hdr nh = { .bits = 0x80000000u, .size = 0, .remote = 0, .local = 0, .voucher = 0, .id = h.id + 100 };
		memcpy(rep, &nh, sizeof nh);
		uint32_t one = 1; memcpy(rep + 24, &one, 4);
		uint64_t addr = 0; size_t len = (size_t)cnt * 4;
		void* mm = mmap(NULL, (len + 4095) & ~4095ul, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (mm == MAP_FAILED) { n = reply_begin(rep, &h, 6 /* KERN_RESOURCE_SHORTAGE */); break; }
		memcpy(mm, th, len); addr = (uint64_t)mm;
		uint8_t dsc[16] = { 0 }; memcpy(dsc, &addr, 8); dsc[8] = 1; dsc[10] = 17; dsc[11] = 2;   // dealloc, PORT_SEND, OOL_PORTS
		uint32_t c32 = (uint32_t)cnt; memcpy(dsc + 12, &c32, 4);
		memcpy(rep + 28, dsc, 16);
		memcpy(rep + 44, NDR_LE, 8); memcpy(rep + 52, &c32, 4);
		n = 56;
		break;
	}
	case 3240: {                                                  // mach_port_is_connection_for_service(conexión, servicio) -> id de política de filtro
		n = reply_begin(rep, &h, KERN_SUCCESS_);
		uint64_t pol = 0; memcpy(rep + n, &pol, 8); n += 8;       // sin filtrado de mensajes: política 0
		break;
	}
	case 3420: n = reply_begin(rep, &h, KERN_SUCCESS_); break;
	case 3217: {                                                  // mach_port_get_attributes(task, name, flavor, cuenta) -> info[]
		uint32_t nm, fl, cnt, out[17]; memcpy(&nm, req + 32, 4); memcpy(&fl, req + 36, 4); memcpy(&cnt, req + 40, 4);
		if (cnt > 17) cnt = 17;
		int kr = port_get_attributes(nm, (int)fl, out, &cnt);
		n = reply_begin(rep, &h, kr);
		if (!kr) { memcpy(rep + n, &cnt, 4); n += 4; memcpy(rep + n, out, cnt * 4); n += cnt * 4; }
		break;
	}
	case 3218: {                                                  // mach_port_set_attributes(task, name, flavor, info[])
		uint32_t nm, fl, cnt, in[17] = { 0 }; memcpy(&nm, req + 32, 4); memcpy(&fl, req + 36, 4); memcpy(&cnt, req + 40, 4);
		if (cnt > 17) cnt = 17;
		if (44 + cnt * 4 <= ssize) memcpy(in, req + 44, cnt * 4);
		n = reply_begin(rep, &h, port_set_attributes(nm, (int)fl, in, cnt));
		break;
	}
	case 3228: {                                                  // mach_port_get_context(task, name) -> context
		uint32_t nm; memcpy(&nm, req + 32, 4);
		n = reply_begin(rep, &h, port_exists(nm) ? KERN_SUCCESS_ : 15 /* KERN_INVALID_NAME */);
		uint64_t ctx = port_get_context(nm); memcpy(rep + n, &ctx, 8); n += 8;
		break;
	}
	case 3233: {                                                  // mach_port_guard(task, name, guard, strict): la guarda es el contexto
		uint32_t nm; uint64_t g; memcpy(&nm, req + 32, 4); memcpy(&g, req + 36, 8);
		uint32_t strict = 0; memcpy(&strict, req + 44, 4);
		n = reply_begin(rep, &h, port_guard(nm, g, strict));
		break;
	}
	case 3234: {                                                  // mach_port_unguard(task, name, guard)
		uint32_t nm; uint64_t g; memcpy(&nm, req + 32, 4); memcpy(&g, req + 36, 8);
		n = reply_begin(rep, &h, port_unguard(nm, g));
		break;
	}
	case 3207: {                                                  // mach_port_get_refs(task, name, right) -> refs
		uint32_t nm, right, refs = 0; memcpy(&nm, req + 32, 4); memcpy(&right, req + 36, 4);
		n = reply_begin(rep, &h, port_get_refs(nm, right, &refs));
		memcpy(rep + n, &refs, 4); n += 4;
		break;
	}
	case 3237: {                                                  // mach_port_guard_with_flags(task, name, guard, flags): MPG_STRICT = 1
		uint32_t nm; uint64_t g, fl; memcpy(&nm, req + 32, 4); memcpy(&g, req + 36, 8); memcpy(&fl, req + 44, 8);
		n = reply_begin(rep, &h, port_guard(nm, g, (int)(fl & 1)));
		break;
	}
	case 3238: {                                                  // mach_port_swap_guard(task, name, viejo, nuevo): solo sin guarda estricta
		uint32_t nm; uint64_t o, nw; memcpy(&nm, req + 32, 4); memcpy(&o, req + 36, 8); memcpy(&nw, req + 44, 8);
		n = reply_begin(rep, &h, port_swap_guard(nm, o, nw));
		break;
	}
	case 3229: {                                                  // mach_port_set_context(task, name, context)
		uint32_t nm; uint64_t ctx; memcpy(&nm, req + 32, 4); memcpy(&ctx, req + 36, 8);
		n = reply_begin(rep, &h, port_set_context(nm, ctx));
		break;
	}   // task_policy_set / mach_port_set_attributes: sin efecto
	case 4811: n = mig_mach_vm_map(&h, req, rep); break;
	case 4817: n = mig_make_memory_entry(&h, req, rep); break;
	case 4813: n = mig_mach_vm_remap(&h, req, rep); break;
	case 4807: {                                                  // mach_vm_copy(task, src, size, dst): el destino ya está asignado
		uint64_t src, size, dst; memcpy(&src, req + 32, 8); memcpy(&size, req + 40, 8); memcpy(&dst, req + 48, 8);
		memmove((void*)dst, (void*)src, size);
		n = reply_begin(rep, &h, KERN_SUCCESS_);
		break;
	}
	case 4808: {                                                  // mach_vm_read_overwrite(task, addr, size, data) -> outsize
		uint64_t src, size, dst; memcpy(&src, req + 32, 8); memcpy(&size, req + 40, 8); memcpy(&dst, req + 48, 8);
		memmove((void*)dst, (void*)src, size);
		n = reply_begin(rep, &h, KERN_SUCCESS_); memcpy(rep + n, &size, 8); n += 8;
		break;
	}
	case 4804: {                                                  // mach_vm_read(task, addr, size) -> data (OOL), dataCnt
		uint64_t src, size; memcpy(&src, req + 32, 8); memcpy(&size, req + 40, 8);
		if (size > (1ull << 30)) { n = reply_begin(rep, &h, 4 /* KERN_INVALID_ARGUMENT */); break; }
		void* mm = size ? mmap(NULL, (size + 4095) & ~4095ul, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) : NULL;
		if (mm == MAP_FAILED) { n = reply_begin(rep, &h, 6 /* KERN_RESOURCE_SHORTAGE */); break; }
		if (size && safe_read(src, mm, size) != (ssize_t)size) { munmap(mm, (size + 4095) & ~4095ul); n = reply_begin(rep, &h, 1 /* KERN_INVALID_ADDRESS */); break; }
		struct hdr nh = { .bits = 0x80000000u, .size = 0, .remote = 0, .local = 0, .voucher = 0, .id = h.id + 100 };
		memcpy(rep, &nh, sizeof nh);
		uint32_t one = 1; memcpy(rep + 24, &one, 4);
		uint64_t addr = (uint64_t)mm; uint32_t l32 = (uint32_t)size;
		uint8_t dsc[16] = { 0 }; memcpy(dsc, &addr, 8); dsc[8] = 1; dsc[9] = 1; dsc[11] = 1; memcpy(dsc + 12, &l32, 4);
		memcpy(rep + 28, dsc, 16);
		memcpy(rep + 44, NDR_LE, 8); memcpy(rep + 52, &l32, 4);
		n = 56;
		break;
	}
	case 4803: case 4809: case 4810: n = reply_begin(rep, &h, KERN_SUCCESS_); break;   // inherit / msync / behavior_set: sin efecto
	case 200:  n = mig_host_info(&h, req, rep); break;
	case 206:  n = mig_host_get_clock_service(&h, req, rep); break;
	case 222: n = reply_port(rep, &h, alloc_port(), 17); break;   // host_create_mach_voucher: puerto de voucher ficticio
	case 225: n = reply_begin(rep, &h, KERN_SUCCESS_); break;     // (host, un entero): se acepta sin efecto
	case 413: n = reply_begin(rep, &h, KERN_SUCCESS_); break;   // host_set_special_port: sin efecto
	case 205: n = reply_port(rep, &h, alloc_port(), 17); break;  // host_get_io_main
	case 412: n = reply_begin(rep, &h, KERN_INVALID_ARG); break;   // host_get_special_port: sin puertos privilegiados (libdispatch usa mach_host_self)
	case 415: n = mig_host_get_exception_ports(&h, rep); break;
	case 414: case 416: n = reply_begin(rep, &h, KERN_SUCCESS_); break;   // set / swap exception ports: sin efecto
	case 1000: n = mig_clock_get_time(&h, rep); break;
	case 3418: n = mig_semaphore_create(&h, req, rep); break;
	case 3404: n = mig_mach_ports_lookup(&h, rep); break;
	case 3403: {                                                  // mach_ports_register: 3 descriptores de puerto
		uint32_t cnt, ports[3] = { 0, 0, 0 };
		memcpy(&cnt, req + 24, 4);
		for (uint32_t i = 0; i < cnt && i < 3; i++) memcpy(&ports[i], req + 28 + i * 12, 4);
		registered_ports_set(ports, (int)(cnt < 3 ? cnt : 3));
		if (trace_all) logf_("    mach_ports_register: 0x%x 0x%x 0x%x\n", ports[0], ports[1], ports[2]);
		n = reply_begin(rep, &h, KERN_SUCCESS_);
		break;
	}     // mach_ports_register: sin efecto
	case 8000: case 8001: n = mig_task_restartable_register(&h, rep); break;   // register y synchronize
	case 3409: n = mig_task_get_special_port(&h, req, rep); break;
	case 3410: n = mig_task_set_special_port(&h, req, rep); break;
	case 3405: n = mig_task_info(&h, req, rep); break;
	default:
		if (h.id >= 2800 && h.id < 2900) {                          // IOKit (device.defs): sin registro de E/S todavía -> kIOReturnNotFound
			{ int hd; size_t r2 = mig_iokit(&h, req, rep, &hd); if (hd) { n = r2; break; } }
			{ char s[96] = ""; size_t k = 0; for (uint32_t i = 32; i < ssize && k < sizeof s - 1; i++) { uint8_t ch = req[i]; if (ch >= 32 && ch < 127) s[k++] = (char)ch; else if (k && s[k - 1] != 0x7c) s[k++] = 0x7c; } s[k] = 0; logf_("    iokit <%d> id=%d (%u bytes) \"%s\" -> kIOReturnNotFound\n", (int)getpid(), h.id, ssize, s); }
			n = reply_begin(rep, &h, (int32_t)0xe00002f0);
			break;
		}
		logf_("    mach_msg2: opciones=0x%lx %s id=%d destino=0x%x local=0x%x (%u bytes, bits=0x%x) sin implementar -> MIG_BAD_ID\n",
		      options, kobject ? "kobject" : "puerto", h.id, h.remote, h.local, ssize, h.bits);
		hexdump(req, ssize < 128 ? ssize : 128);
		n = reply_begin(rep, &h, MIG_BAD_ID);
		break;
	}
	{   // MIG: una respuesta con error es siempre mig_reply_error_t (cabecera + NDR + RetCode = 36 bytes), sin los datos de
		// salida; si trae más, el stub cliente la rechaza con MIG_TYPE_ERROR (-300).
		uint32_t bits; int32_t rc; memcpy(&bits, rep, 4); memcpy(&rc, rep + 32, 4);
		if (n > 36 && !(bits & 0x80000000u) && rc != 0) n = 36;
	}
	reply_end(rep, n);
	if (trace_all) logf_("    mach_msg2: id=%d -> respuesta id=%d (%zu bytes)\n", h.id, h.id + 100, n);

	if (!(options & 2)) return MACH_MSG_SUCCESS;               // solo envío: no hay respuesta que entregar
	memset(rep + n, 0, 8);                                      // trailer: tipo 0, tamaño 8
	uint32_t tsize = 8;
	memcpy(rep + n + 4, &tsize, 4);
	if (kobject && rcvsize < 16) {
		// En algunas ramas (reinicialización tras fork) el tamaño de recepción de la pila no es fiable. Un cliente MIG
		// reserva un búfer del tamaño de la respuesta + trailer, y nuestras respuestas tienen el tamaño real.
		if (trace_all) logf_("    mach_msg2: rcv_size=%u no fiable; se usa el tamaño de la respuesta (%zu+8)\n", rcvsize, n);
		rcvsize = (uint32_t)(n + 8);
	}
	if (n + 8 > rcvsize) {
		logf_("    mach_msg2: la respuesta (%zu+8) no cabe en rcv_size=%u (id=%d destino=0x%x)\n", n, rcvsize, h.id, h.remote);
		return MACH_RCV_TOO_LARGE;
	}
	if (safe_write(rbuf, rep, n + 8) != (ssize_t)(n + 8)) return MACH_RCV_INVALID_DATA_;
	return MACH_MSG_SUCCESS;
}

static long t_iokit_user_client_trap(struct ctx* c) {
	uint64_t p[6]; for (int i = 0; i < 6; i++) p[i] = ctx_arg(c, 2 + i);
	return io_user_client_trap((uint32_t)c->a[0], (uint32_t)c->a[1], p);
}
void emu_mach_init(void) { reg_mach(47, mach_msg2); reg_mach(100, t_iokit_user_client_trap); }
