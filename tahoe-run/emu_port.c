// Puertos Mach compartidos entre los procesos de tahoe-run: derechos de recepción con cola de mensajes, conjuntos
// de puertos y envío/recepción con mach_msg2 (ver la región compartida más abajo).
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <glob.h>
#include <sys/eventfd.h>
#include <sys/timerfd.h>

#include <sched.h>
#include <signal.h>
#include <sys/syscall.h>
#include <time.h>
#include "tahoe.h"
#include "nucleo.h"

#include <fcntl.h>
#include <stdatomic.h>
#include <stddef.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>

// ---------------------------------------------------------------- región compartida entre procesos
// Todos los procesos de tahoe-run de un mismo arranque comparten una región (archivo en /dev/shm indicado por
// TAHOE_PORTS) con la tabla de puertos y las colas de mensajes. Un nombre de puerto es global: el mismo número
// designa el mismo puerto en cualquier proceso. El dueño del derecho de recepción es quien lo creó; para despertar
// a su epoll (EVFILT_MACHPORT) cada puerto tiene un socket de datagramas con nombre abstracto.
#define MAXPORTS 8192                // para todo el sistema (unos 170 procesos en el arranque)
#define NBUCKET 16384                // índice por nombre: cubeta = (nombre >> 8) % NBUCKET, cadenas por hnext
#define MAXMSG   4096                    // ranuras de mensaje (archivo disperso: solo ocupan RAM las usadas)
#define MSGSZ    (64 * 1024)
// Un puerto puede estar en varios conjuntos a la vez (XNU lo permite y CFRunLoop lo usa: mete el puerto con el que se
// despierta un runloop en el conjunto de cada uno de sus modos). Con uno solo, el último insert_member se lo quitaba al
// conjunto en que esperaba el hilo y el aviso no llegaba (NSTask -waitUntilExit no volvía: system_profiler con csrutil).
#define NSETS 8
struct sport { uint32_t name, sets[NSETS], head, tail, count; int32_t owner, rpid; uint8_t used, is_set, guarded, strict_guard; uint32_t cv; uint64_t ctx; uint32_t pdreq; uint8_t svc; uint64_t svc_ctx; uint32_t qlimit; uint32_t nsreq, nsmscount; int32_t hnext; uint32_t spn; uint8_t me; };   // me: entrada de memoria (mach_make_memory_entry), ver mer_add_locked   // spn: peticiones MACH_NOTIFY_SEND_POSSIBLE pendientes en shm->sp (para no recorrer la tabla si no hay)   // hnext: siguiente (índice+1) en la cadena de su cubeta del índice por nombre   // nsreq: puerto que pidió MACH_NOTIFY_NO_SENDERS   // qlimit: 0 = MACH_PORT_QLIMIT_DEFAULT (5)   // svc/svc_ctx: puerto de servicio de launchd y su guarda (vuelve con PORT_DESTROYED)   // pdreq: puerto que pidió MACH_NOTIFY_PORT_DESTROYED   // guarded/strict_guard: el contexto es una guarda (mach_port_guard / MPO_CONTEXT_AS_GUARD)   // ctx: contexto del puerto (mach_port_construct / set_context), va en el trailer   // cv: despierta solo a quien espera este puerto/conjunto   // rpid: proceso que recibe ahora (cambia al mover el derecho de recepción)
static int in_set(const struct sport* p, uint32_t set) { for (int j = 0; j < NSETS; j++) if (p->sets[j] == set) return 1; return 0; }
static void set_remove(struct sport* p, uint32_t set) { for (int j = 0; j < NSETS; j++) if (p->sets[j] == set) p->sets[j] = 0; }
struct mslot { uint32_t next, size, total, spid, suid, sgid, sauid, sasid; uint64_t spill; uint8_t data[MSGSZ - 40]; };   // sauid/sasid: sesión de auditoría del remitente   // spill: datos fuera de línea en un archivo aparte (nucleo_desborde_*), 0 = en la ranura
// Derechos que el núcleo tiene que vigilar para avisar cuando muere su dueño (ver "notificaciones" más abajo):
//  sonce: un proceso tiene un derecho send-once (normalmente, la respuesta que debe a quien le pidió algo);
//  dnreq: un proceso pidió MACH_NOTIFY_DEAD_NAME sobre un puerto (aviso al puerto `notify` cuando muera);
//  sproc: hora de arranque de cada proceso, para no confundir un pid reutilizado con el que murió.
#define MAXSO 4096
#define MAXDN 4096
#define MAXSP 1024                     // peticiones SEND_POSSIBLE: una por proceso y puerto, como los derechos de XNU
#define MAXPROC 1024
struct sonce { uint32_t port; int32_t holder; };
struct dnreq { uint32_t port, notify; int32_t pid; uint32_t armado; };   // armado: solo SEND_POSSIBLE (ver port_request_notification)
struct sproc { int32_t pid; uint64_t start; };
// shold: un proceso que no es el dueño tiene un derecho de envío a un puerto (para MACH_NOTIFY_NO_SENDERS, ver más abajo).
#define MAXSH 8192
struct shold { uint32_t port; int32_t holder; };
// Objetos del núcleo con nombre de puerto (tareas de otros procesos, tokens de identidad...): el nombre lleva a su tipo y pid.
#define MAXKOBJ 1024
struct skobj { uint32_t name; uint16_t type; int32_t pid; };
// Mapeos hechos por otro proceso en esta tarea (mach_vm_map con la tarea de otro como destino): Linux no puede mapear en otro
// proceso, así que se reserva un hueco en una zona común (RMAP_BASE) y el destino lo mapea él mismo al primer acceso.
#define MAXRMAP 512
#define MAXEXECW 1024                  // launchd vigila cada servicio vivo (~190 en el macOS completo): 256 se quedaba corto
struct sexecw { int32_t target, watcher; uint32_t port; };
// Derechos de envío a las entradas de memoria, por proceso (pid 0 = en tránsito dentro de mensajes): cuando no queda
// ninguno, la entrada se destruye y se borra su archivo de /dev/shm (las proyecciones ya hechas siguen valiendo).
#define MAXMER 4096
struct meref { uint32_t port; int32_t pid, refs; };   // NOTE_EXEC: quién vigila los exec de qué proceso, y su puerto
#define RMAP_BASE 0x500000000000ULL
#define RMAP_SIZE 0x100000000ULL
struct srmap { int32_t pid; int32_t prot; uint64_t addr, len, off; char path[96]; };
struct shm {
	uint32_t lk, cv;                      // candado y espera compartidos (nucleo/src/candado.rs)
	uint32_t next_name, free_head, magic, special[8];
	struct sport ports[MAXPORTS];
	int32_t bucket[NBUCKET];              // índice+1 del primer puerto de cada cubeta (0 = vacía)
	struct mslot msgs[MAXMSG];
	struct sonce so[MAXSO];
	struct dnreq dn[MAXDN];
	struct dnreq sp[MAXSP];
	struct sproc procs[MAXPROC];
	struct shold sh[MAXSH];
	struct skobj kobjs[MAXKOBJ];
	struct srmap rmaps[MAXRMAP];
	uint64_t rmap_next;
	uint64_t osvariant_status;            // kern.osvariant_status: lo fija launchd (PID 1) una vez y lo leen todos
	uint64_t spill_next;                  // número del siguiente archivo de desbordamiento (mensajes grandes)
	struct sexecw execw[MAXEXECW];
	struct meref mer[MAXMER];
};
static struct shm* shm;
static char shm_path[128], tag[32];
static int local_sock[MAXPORTS];           // socket de despertar de los puertos propios (índice = posición en la tabla)

static int is_creator;
void start_syslog_listener(void);
void populate_data_volume(void);
void ports_cleanup(void);
void ports_cleanup(void) {                       // solo el proceso que creó la región la borra (y los shm_open del arranque)
	if (!is_creator) return;
	unlink(shm_path);
	glob_t g;
	char pat[128];
	snprintf(pat, sizeof pat, "/dev/shm/tahoe.%s.*", tag);
	if (glob(pat, 0, NULL, &g) == 0) { for (size_t i = 0; i < g.gl_pathc; i++) unlink(g.gl_pathv[i]); globfree(&g); }
}

// Tras fork: el hijo no es el creador de la región (si sale, no debe borrarla: la usan los demás).
void ports_forked_child(void) { is_creator = 0; }
static void shm_init(void) {
	const char* env = getenv("TAHOE_PORTS");
	int creator = 0;
	if (env) snprintf(shm_path, sizeof shm_path, "%s", env);
	else { struct timespec tsn; clock_gettime(CLOCK_REALTIME, &tsn); snprintf(tag, sizeof tag, "%lx%lx", (long)tsn.tv_sec, (long)tsn.tv_nsec); snprintf(shm_path, sizeof shm_path, "/dev/shm/tahoe-ports-%s", tag); creator = 1; }
	int fd = open(shm_path, O_RDWR | (creator ? O_CREAT : 0) | O_CLOEXEC, 0600);   // solo el creador la crea
	if (fd < 0) { logf_("    puertos: no puedo abrir %s: %s\n", shm_path, strerror(errno)); exit(70); }
	if (creator && ftruncate(fd, sizeof(struct shm)) != 0) { logf_("    puertos: ftruncate: %s\n", strerror(errno)); exit(70); }
	shm = mmap(NULL, sizeof(struct shm), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	close(fd);
	if (shm == MAP_FAILED) { logf_("    puertos: mmap: %s\n", strerror(errno)); exit(70); }
	if (creator) {
		is_creator = 1;
		populate_data_volume();                       // plantilla del volumen de datos (dslocal, /private/var/...)
		start_syslog_listener();
		// El candado (shm->lk) y las esperas por puerto (ports[i].cv) empiezan a cero: libres (ftruncate rellena con ceros).
		shm->next_name = 0x2503;
		for (int i = 0; i < MAXMSG; i++) shm->msgs[i].next = i + 1 < MAXMSG ? (uint32_t)(i + 2) : 0;   // índices desde 1; 0 = fin
		shm->free_head = 1;
		shm->magic = 0x7a110002;
		setenv("TAHOE_PORTS", shm_path, 1);
		setenv("TAHOE_PORTS_OWNER", tag, 1);
		atexit(ports_cleanup);
	} else {
		snprintf(tag, sizeof tag, "%s", getenv("TAHOE_PORTS_OWNER") ? getenv("TAHOE_PORTS_OWNER") : "x");
	}
	for (int i = 0; i < MAXPORTS; i++) local_sock[i] = -1;
}
static pthread_once_t once = PTHREAD_ONCE_INIT;
static uint64_t proc_start(int pid);
static void start_reaper(void);
static void init_all(void) {
	shm_init();
	int me = (int)getpid();
	nucleo_candado_tomar(&shm->lk);
	shm->procs[me % MAXPROC] = (struct sproc){ me, proc_start(me) };
	nucleo_candado_soltar(&shm->lk);
	if (is_creator) start_reaper();
}
// Candado de toda la región, entre procesos. Es de nucleo/src/candado.rs y no un mutex robusto de glibc: glibc identifica
// al dueño por el TID que guarda en su descriptor de hilo, y nuestro fork (llamada directa, emu_proc.c) no lo actualiza en
// el hijo; un hijo se creía dueño del candado del padre, entraba sin esperar y se perdían despertares (cuelgues).
static void lock(void) {
	pthread_once(&once, init_all);
	if (nucleo_candado_tomar(&shm->lk) == EOWNERDEAD) logf_("    puertos: <%d> el dueño del candado murió con él tomado; se sigue\n", (int)getpid());
}
static void unlock(void) { nucleo_candado_soltar(&shm->lk); }

enum { KERN_OK = 0, KERN_INVALID_NAME_ = 15, KERN_INVALID_RIGHT_ = 17, KERN_INVALID_VALUE_ = 18 };
#define MACH_RCV_TIMED_OUT 0x10004003
#define MACH_RCV_TOO_LARGE_ 0x10004004

// Los nombres se piden siempre a la región compartida (también los de semáforos y puertos del kernel falsos).
uint32_t alloc_port(void) {
	lock();
	uint32_t n = shm->next_name;
	shm->next_name += 0x100;
	unlock();
	return n;
}

static uint32_t bucket_of(uint32_t name) { return (name >> 8) & (NBUCKET - 1); }
static struct sport* find_locked(uint32_t name) {
	for (int32_t i = shm->bucket[bucket_of(name)]; i > 0 && i <= MAXPORTS; i = shm->ports[i - 1].hnext)
		if (shm->ports[i - 1].used && shm->ports[i - 1].name == name) return &shm->ports[i - 1];
	return NULL;
}
static void bucket_add_locked(struct sport* p) {
	int32_t* b = &shm->bucket[bucket_of(p->name)];
	p->hnext = *b; *b = (int32_t)(p - shm->ports) + 1;
}
static void bucket_del_locked(struct sport* p) {
	int32_t me = (int32_t)(p - shm->ports) + 1;
	for (int32_t* q = &shm->bucket[bucket_of(p->name)]; *q > 0 && *q <= MAXPORTS; q = &shm->ports[*q - 1].hnext)
		if (*q == me) { *q = p->hnext; p->hnext = 0; return; }
}
static int port_index(struct sport* p) { return (int)(p - shm->ports); }

// Despierta a quien espere por un puerto con epoll (el dueño puede ser otro proceso).
static void poke(uint32_t name, int32_t rpid) {
	int s = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
	if (s < 0) return;
	struct sockaddr_un a = { .sun_family = AF_UNIX };
	int n = snprintf(a.sun_path + 1, sizeof a.sun_path - 1, "tahoe.%s.%u.%d", getenv("TAHOE_PORTS_OWNER") ? getenv("TAHOE_PORTS_OWNER") : "x", name, (int)rpid);
	char b = 1;
	if (sendto(s, &b, 1, 0, (struct sockaddr*)&a, (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + n)) < 0 && mach_trace()) logf_("    poke 0x%x -> pid %d FALLÓ: %s\n", name, (int)rpid, strerror(errno));
	close(s);
}
// Vacía todos los avisos pendientes del socket: el aviso es un nivel ("hay mensajes"), no un contador; la cola de datagramas
// de Unix es corta y puede perder avisos, así que tras recibir se vuelve a avisar si aún quedan mensajes.
static void drain_all(int fd) { char b[64]; if (fd < 0) return; while (recv(fd, b, sizeof b, MSG_DONTWAIT) > 0) {} }
// Vacía el socket de aviso de un puerto y vuelve a mirar su cola con el cerrojo: si entre tanto llegó un mensaje, su aviso
// pudo irse con el vaciado (el emisor avisa después de soltar el cerrojo) y el kqueue no despertaría nunca por él (se vio en
// configd: un mensaje a su propio puerto quedó en cola sin aviso y con él se colgaron powerd y storagekitd). Se avisa de nuevo.
static void drain_recheck(uint32_t name, int fd) {
	if (fd < 0) return;
	drain_all(fd);
	lock();
	struct sport* p = find_locked(name);
	uint32_t cnt = p ? p->count : 0; int32_t rp = p ? p->rpid : 0;
	unlock();
	if (cnt) poke(name, rp);
}

static int make_wake_socket(uint32_t name, int32_t rpid) {
	int s = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
	if (s < 0) return -1;
	struct sockaddr_un a = { .sun_family = AF_UNIX };
	int n = snprintf(a.sun_path + 1, sizeof a.sun_path - 1, "tahoe.%s.%u.%d", getenv("TAHOE_PORTS_OWNER") ? getenv("TAHOE_PORTS_OWNER") : "x", name, (int)rpid);
	if (bind(s, (struct sockaddr*)&a, (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + n)) < 0) { close(s); return -1; }
	return fd_hi(s);
}

// Diagnóstico: qué clase de puertos recibe un proceso. «solo muertos»: todos sus usuarios anotados (derechos de envío) son
// procesos que ya no existen, así que el aviso no-senders se debió dar y no se dio. «con mensajes»: tienen cola sin leer.
static void puertos_de(int32_t pid) {
	int n = 0, ns = 0, held = 0, sets = 0, queued = 0, pd = 0, insets = 0, solo_muertos = 0, ns_sin = 0, msgs = 0, sin_ns_ni_usuarios = 0;
	char ejemplos[120] = "";
	uint32_t recientes[6] = { 0 };
	lock();
	for (int i = 0; i < MAXPORTS; i++) {
		struct sport* q = &shm->ports[i];
		if (!q->used || q->rpid != pid) continue;
		n++;
		ns += q->nsreq != 0; sets += q->is_set; queued += q->count > 0; msgs += (int)q->count; pd += q->pdreq != 0; insets += q->sets[0] != 0;
		int h = 0, vivos = 0;
		for (int j = 0; j < MAXSH; j++) if (shm->sh[j].port == q->name) { h++; if (kill(shm->sh[j].holder, 0) == 0 || errno != ESRCH) vivos++; }
		held += h > 0; solo_muertos += h > 0 && !vivos; ns_sin += q->nsreq && !h;
		if (!q->nsreq && !h && !q->is_set) {                        // ejemplos: los 6 de nombre más alto (los más recientes)
			sin_ns_ni_usuarios++;
			for (int k = 0; k < 6; k++) if (q->name > recientes[k]) { memmove(recientes + k + 1, recientes + k, (5 - k) * sizeof *recientes); recientes[k] = q->name; break; }
		}
	}
	unlock();
	for (int k = 0; k < 6 && recientes[k]; k++) { char e[16]; snprintf(e, sizeof e, " 0x%x", recientes[k]); strncat(ejemplos, e, sizeof ejemplos - strlen(ejemplos) - 1); }
	logf_("    puertos de %d (%d): no-senders pedido=%d (sin usuarios anotados %d) con usuarios anotados=%d (solo muertos %d) sin aviso ni usuarios=%d conjuntos=%d en conjunto=%d con mensajes=%d (%d mensajes) port-destroyed=%d; sin aviso ni usuarios:%s\n",
		(int)pid, n, ns, ns_sin, held, solo_muertos, sin_ns_ni_usuarios, sets, insets, queued, msgs, pd, ejemplos);
}
uint32_t port_create(int is_set) {
	uint32_t name = alloc_port();
	lock();
	struct sport* p = NULL;
	for (int i = 0; i < MAXPORTS; i++) if (!shm->ports[i].used) { p = &shm->ports[i]; break; }
	if (!p) {
		// Diagnóstico (una vez por proceso): quién tiene los puertos. rpid = dueño del derecho de recepción.
		static int dumped;
		if (!dumped) {
			dumped = 1;
			static int pid[1024], cnt[1024]; int np = 0; memset(cnt, 0, sizeof cnt);
			for (int i = 0; i < MAXPORTS; i++) {
				int r = shm->ports[i].rpid, k;
				for (k = 0; k < np && pid[k] != r; k++) {}
				if (k == np && np < 1024) { pid[np] = r; np++; }
				if (k < 1024) cnt[k]++;
			}
			unlock();
			logf_("    puertos: tabla llena; por dueño:");
			int resto = 0, nresto = 0;
			for (int k = 0; k < np; k++) if (cnt[k] >= 20) logf_(" %d:%d%s", pid[k], cnt[k], kill(pid[k], 0) == 0 ? "" : "(muerto)"); else { resto += cnt[k]; nresto++; }
			logf_(" | otros %d dueños con %d\n", nresto, resto);
			// Los 5 dueños con más puertos: qué clase de puertos son (puertos_de).
			for (int r5 = 0; r5 < 5 && r5 < np; r5++) {
				int top = -1; for (int k = 0; k < np; k++) if (cnt[k] > 0 && (top < 0 || cnt[k] > cnt[top])) top = k;
				if (top < 0) break;
				puertos_de(pid[top]);
				cnt[top] = -cnt[top];
			}
		} else unlock();
		logf_("    puertos: tabla llena\n");
		return 0;
	}
	{ uint32_t keep = p->cv; memset(p, 0, sizeof *p); p->cv = keep; }   // la secuencia de espera sigue (quien esperase vuelve a mirar)
	p->used = 1; p->is_set = (uint8_t)is_set; p->name = name; p->owner = (int32_t)getpid(); p->rpid = (int32_t)getpid();
	bucket_add_locked(p);
	local_sock[port_index(p)] = make_wake_socket(name, p->rpid);
	unlock();
	if (trace_all || mach_trace() || msg_trace())
		logf_("    puerto 0x%x creado por pid %d.%ld (conjunto=%d) desde %p\n", name, (int)getpid(), (long)syscall(SYS_gettid), is_set, __builtin_return_address(0));
	return name;
}

int port_exists(uint32_t name) { lock(); int r = find_locked(name) != NULL; unlock(); return r; }

// Socket legible mientras haya mensajes pendientes (solo para puertos que creó este proceso). -1 si no.
int port_eventfd(uint32_t name) {
	lock();
	struct sport* p = find_locked(name);
	int r = -1;
	if (p) {
		int i = port_index(p);
		if (p->rpid != (int32_t)getpid() || local_sock[i] < 0) {             // el derecho de recepción se movió a este proceso
			if (local_sock[i] >= 0) close(local_sock[i]);
			p->rpid = (int32_t)getpid();
			local_sock[i] = make_wake_socket(name, p->rpid);
			if (mach_trace()) logf_("    puerto 0x%x: socket de aviso para pid %d fd=%d\n", name, (int)p->rpid, local_sock[i]);
			if (p->count) poke(name, p->rpid);                              // ya hay mensajes: despertar de inmediato
		}
		r = local_sock[i];
	}
	unlock();
	return r;
}


// ---------------------------------------------------------------- notificaciones
// Como en XNU, el "núcleo" avisa por mensajes cuando un derecho desaparece:
//  - MACH_NOTIFY_SEND_ONCE (71): se destruye un derecho send-once sin usarlo (el servidor murió sin responder, o el
//    mensaje con el derecho de respuesta se descartó). El cliente MIG lo traduce a MIG_SERVER_DIED y XPC a un error,
//    en vez de esperar para siempre.
//  - MACH_NOTIFY_DEAD_NAME (72): muere el puerto al que se tenía un derecho de envío y se pidió el aviso.
//  - MACH_NOTIFY_NO_SENDERS (70): ya nadie tiene derechos de envío al puerto. libxpc lo usa en los servidores para cerrar la
//    conexión de un cliente y destruir su puerto; sin el aviso, cada cliente que termina deja un puerto huérfano en el servidor
//    (logd, launchd...) hasta llenar la tabla. Aquí se anota qué procesos (distintos del dueño) recibieron un derecho de envío
//    a cada puerto, y el aviso sale cuando muere el último de ellos. Es conservador: si un cliente suelta su derecho sin
//    terminar, el aviso llega cuando termine, nunca antes de tiempo.
//  - MACH_NOTIFY_PORT_DESTROYED (69): se iba a destruir un derecho de recepción con esa petición; en vez de eso el derecho
//    viaja (con sus mensajes) al que la pidió. launchd lo usa para recuperar los puertos de los servicios que mueren.
// Los avisos se acumulan con el cerrojo tomado y se envían después (enviar toma el cerrojo).
struct note { uint32_t dest, arg; int id; };
static __thread struct note notes[512];
static __thread int nnotes;
static void note_locked(uint32_t dest, int id, uint32_t arg) {
	if (!dest) return;
	if (nnotes < (int)(sizeof notes / sizeof notes[0])) notes[nnotes++] = (struct note){ dest, arg, id };
	else logf_("    notificaciones: cola llena, se pierde %d para 0x%x\n", id, dest);
}
static int port_send_k(uint32_t dest, const uint8_t* msg, uint32_t size, int kernel);
static void destroy_locked(struct sport* p);
static void flush_notes(void) {
	while (nnotes > 0) {
		struct note n = notes[--nnotes];
		uint8_t m[40] = { 0 };
		uint32_t bits = 18, size = 24;                       // remoto: MOVE_SEND_ONCE (el aviso usa el send-once de la petición)
		if (n.id == 72 || n.id == 70 || n.id == 66) { size = 36; m[24 + 4] = 1; memcpy(m + 32, &n.arg, 4); }      // NDR (little endian) + nombre muerto
		else if (n.id == 69) {                               // complejo: un descriptor de puerto con el derecho de recepción
			bits |= 0x80000000u; size = 40;
			uint32_t one = 1; memcpy(m + 24, &one, 4); memcpy(m + 28, &n.arg, 4); m[38] = 16; m[39] = 0;
		}
		memcpy(m, &bits, 4); memcpy(m + 4, &size, 4); memcpy(m + 8, &n.dest, 4); memcpy(m + 20, &n.id, 4);
		int r = port_send_k(n.dest, m, size, 1);
		if (mach_trace() || n.id != 71) logf_("    aviso %d (0x%x) -> 0x%x%s\n", n.id, n.arg, n.dest, r ? " (sin destino)" : "");
		if (r && n.id == 69) {
			// PORT_DESTROYED sin destino (quien pidió el aviso ya no existe): como en XNU, el mensaje no entregado se destruye y
			// con él el derecho de recepción que llevaba, así que el puerto muere de verdad. Antes se quedaba «en tránsito»
			// para siempre: cada conexión XPC de un cliente que terminaba dejaba un puerto en cfprefsd, opendirectoryd...
			lock();
			struct sport* q = find_locked(n.arg);
			if (q && q->owner == 0) destroy_locked(q);       // puede añadir avisos (DEAD_NAME...): el bucle los envía
			unlock();
		}
	}
}

static void sh_put_locked(uint32_t port, int32_t holder) {
	int fr = -1;
	for (int i = 0; i < MAXSH; i++) { if (shm->sh[i].port == port && shm->sh[i].holder == holder) return; if (fr < 0 && !shm->sh[i].port) fr = i; }
	if (fr >= 0) shm->sh[fr] = (struct shold){ port, holder };
	else logf_("    no-senders: tabla llena\n");
}
static void sh_add_locked(uint32_t port, int32_t holder) {
	struct sport* p = find_locked(port);
	if (!p || p->rpid == holder) return;                      // solo derechos a puertos de otro proceso
	sh_put_locked(port, holder);
}
// Entradas de memoria: en XNU son objetos del núcleo que viven mientras haya derechos de envío; aquí cada una es un archivo
// de /dev/shm. Sin contar los derechos, los archivos no se borraban nunca: lsd crea una de 1,5 MB por cada cliente que pide
// su base de datos y en un par de minutos llenaba /dev/shm (SIGBUS al tocar la región compartida: launchd moría).
static void mer_add_locked(uint32_t port, int32_t pid, int delta) {
	if (!port || !delta) return;
	struct sport* p = find_locked(port);
	if (!p || !p->me) return;
	int fr = -1, mine = -1; long total = 0;
	for (int i = 0; i < MAXMER; i++) {
		if (shm->mer[i].port == port) { total += shm->mer[i].refs; if (shm->mer[i].pid == pid) mine = i; }
		else if (fr < 0 && !shm->mer[i].port) fr = i;
	}
	if (mine < 0) {
		if (delta < 0) return;                                // no constaba: no se puede soltar lo que no se tiene
		if (fr < 0) { logf_("    entradas de memoria: tabla llena\n"); return; }
		shm->mer[mine = fr] = (struct meref){ port, pid, 0 };
	}
	int32_t old = shm->mer[mine].refs, nw = old + delta < 0 ? 0 : old + delta;
	shm->mer[mine].refs = nw; total += nw - old;
	if (!nw) shm->mer[mine].port = 0;
	if (total <= 0) destroy_locked(p);
}
void port_me_created(uint32_t port) {
	lock();
	struct sport* p = find_locked(port);
	if (p) { p->me = 1; mer_add_locked(port, (int32_t)getpid(), 1); }
	unlock();
}
void port_me_hold(uint32_t port, int32_t pid) { lock(); mer_add_locked(port, pid, 1); unlock(); }   // hasta que muera pid
static void port_me_release(uint32_t port, int delta) { lock(); mer_add_locked(port, (int32_t)getpid(), delta); unlock(); flush_notes(); }

// Derechos send-once ya gastados hace poco (puerto, quien lo tenía): un segundo envío con el mismo derecho es una respuesta
// duplicada, que en XNU no puede ocurrir (el derecho se consume al enviar).
static struct sonce so_used[256];
static unsigned so_used_next;
static int so_was_used_locked(uint32_t port, int32_t holder) {
	for (int i = 0; i < 256; i++) if (so_used[i].port == port && so_used[i].holder == holder) return 1;
	return 0;
}
static void so_add_locked(uint32_t port, int32_t holder) {
	for (int i = 0; i < 256; i++) if (so_used[i].port == port && so_used[i].holder == holder) so_used[i].port = 0;
	for (int i = 0; i < MAXSO; i++) if (!shm->so[i].port) { shm->so[i] = (struct sonce){ port, holder }; return; }
	logf_("    send-once: tabla llena\n");
}
static int so_take_locked(uint32_t port, int32_t holder) {
	for (int i = 0; i < MAXSO; i++) if (shm->so[i].port == port && shm->so[i].holder == holder) { shm->so[i].port = 0; return 1; }
	return 0;
}

// Derechos que lleva un mensaje (cabecera y descriptores de puerto), según el momento:
//  RIGHTS_SEND: el emisor los entrega (un send-once que usa para responder o reenviar deja de ser suyo; un derecho de
//               recepción queda "en tránsito" y nadie puede destruirlo hasta que se reciba);
//  RIGHTS_RECV: el receptor pasa a tener los send-once;
//  RIGHTS_DESTROY: el mensaje se descarta sin entregarse: cada send-once genera MACH_NOTIFY_SEND_ONCE.
enum { RIGHTS_SEND, RIGHTS_RECV, RIGHTS_DESTROY };
static void msg_rights_locked(const uint8_t* m, uint32_t size, int mode, uint32_t dest) {
	if (size < 24) return;
	uint32_t bits, remote, local; memcpy(&bits, m, 4); memcpy(&remote, m + 8, 4); memcpy(&local, m + 12, 4);
	uint32_t rd = bits & 0x1f, ld = (bits >> 8) & 0x1f;
	int32_t me = (int32_t)getpid();
	if (mode == RIGHTS_SEND && rd == 18) {                                  // responde con su send-once
		if (so_take_locked(dest, me)) so_used[so_used_next++ & 255] = (struct sonce){ dest, me };
		else if (so_was_used_locked(dest, me)) {
			uint32_t id; memcpy(&id, m + 20, 4);
			logf_("    send-once: pid %d responde otra vez por 0x%x (id=%u): derecho ya gastado\n", (int)me, dest, id);
		}
	}
	if ((ld == 18 || ld == 21) && local) {                                  // derecho de respuesta send-once
		if (mode == RIGHTS_SEND && ld == 18) so_take_locked(local, me);
		else if (mode == RIGHTS_RECV) so_add_locked(local, me);
		else if (mode == RIGHTS_DESTROY) note_locked(local, 71, 0);
	}
	if (mode == RIGHTS_RECV && local && (ld == 17 || ld == 19 || ld == 20)) sh_add_locked(local, me);   // MOVE/COPY/MAKE_SEND
	struct nucleo_desc d[64];
	int nd = nucleo_descriptores(m, size, d, 64);
	if (nd > 64) { logf_("    puertos: mensaje con %d descriptores, solo se siguen 64\n", nd); nd = 64; }
	for (int i = 0; i < nd; i++) {
		uint32_t nm = d[i].nombre; uint8_t disp = d[i].disp;
		if (!nm || (d[i].clase != NUCLEO_PUERTO && d[i].clase != NUCLEO_PUERTO_GUARDA)) continue;
		if (disp == 17 || disp == 19 || disp == 20) {                     // entradas de memoria: el derecho viaja en el mensaje
			if (mode == RIGHTS_SEND) { mer_add_locked(nm, 0, 1); if (disp == 17) mer_add_locked(nm, me, -1); }
			else if (mode == RIGHTS_RECV) { mer_add_locked(nm, me, 1); mer_add_locked(nm, 0, -1); }
			else mer_add_locked(nm, 0, -1);
		}
		if (disp == 18 || disp == 21) {
			if (mode == RIGHTS_SEND && disp == 18) so_take_locked(nm, me);
			else if (mode == RIGHTS_RECV) so_add_locked(nm, me);
			else if (mode == RIGHTS_DESTROY) note_locked(nm, 71, 0);
		} else if (mode == RIGHTS_RECV && (disp == 17 || disp == 19 || disp == 20)) {
			sh_add_locked(nm, me);
		} else if (disp == 16) {                                       // MOVE_RECEIVE
			struct sport* q = find_locked(nm);
			if (q && mode == RIGHTS_SEND) {
				q->owner = 0; q->ctx = 0; q->guarded = q->strict_guard = 0;   // en tránsito (ipc_port_clear_receive)
				// Quien traspasa el derecho de recepción suele quedarse con derechos de envío hechos por él (así crea
				// libxpc las conexiones: el cliente hace el puerto y entrega la recepción al servidor). Se anota como
				// usuario; si no los tenía, el aviso NO_SENDERS solo llega más tarde (al terminar), nunca antes.
				sh_put_locked(nm, me);
			}
			else if (q && mode == RIGHTS_DESTROY) destroy_locked(q);      // nadie lo recibirá
		}
	}
}

// ipc_port_destroy: con petición PORT_DESTROYED el derecho de recepción se entrega (con sus mensajes) al que la pidió;
// si no, se descartan los mensajes y se avisa a quien pidió DEAD_NAME.
static void destroy_locked(struct sport* p) {
	uint32_t name = p->name;
	for (int i = 0; i < MAXSH; i++) if (shm->sh[i].port == name) shm->sh[i].port = 0;   // ya no hay a quién avisar
	if (p->pdreq && !p->is_set) {
		uint32_t to = p->pdreq; p->pdreq = 0;
		p->owner = 0;                                         // en tránsito hacia quien pidió el aviso
		if (p->svc && p->svc_ctx) { p->ctx = p->svc_ctx; p->guarded = p->strict_guard = 1; }   // ipc_right_copyout_recv: launchd recupera su guarda
		else { p->guarded = p->strict_guard = 0; p->ctx = 0; }
		note_locked(to, 69, name);
		logf_("    puerto 0x%x: derecho de recepción devuelto a 0x%x (PORT_DESTROYED, cola=%u)\n", name, to, p->count);
		return;
	}
	logf_("    puerto 0x%x destruido por pid %d (cola=%u)\n", name, (int)getpid(), p->count);
	if (p->me) {
		char path[128]; snprintf(path, sizeof path, "/dev/shm/tahoe-me-%s-%u", tag, name); unlink(path);
		for (int k = 0; k < MAXMER; k++) if (shm->mer[k].port == name) shm->mer[k].port = 0;
		p->me = 0;
	}
	int i = port_index(p);
	for (int k = 0; k < MAXPORTS; k++) if (shm->ports[k].used) set_remove(&shm->ports[k], name);
	for (int j = 0; j < NSETS; j++) if (p->sets[j]) { struct sport* st = find_locked(p->sets[j]); if (st && st->count >= p->count) st->count -= p->count; }
	bucket_del_locked(p);
	p->used = 0;                                              // antes de recorrer: un descriptor no puede volver a destruirlo
	while (p->head) {                                         // mensajes encolados: sus derechos se destruyen con ellos
		struct mslot* q = &shm->msgs[p->head - 1]; uint32_t nx = q->next;
		msg_rights_locked(q->data, q->size, RIGHTS_DESTROY, name);
		if (q->spill) { nucleo_desborde_borrar(tag, q->spill); q->spill = 0; }
		q->next = shm->free_head; shm->free_head = p->head; p->head = nx;
	}
	p->tail = 0; p->count = 0;
	for (int k = 0; k < MAXDN; k++) if (shm->dn[k].port == name) { note_locked(shm->dn[k].notify, 72, name); shm->dn[k].port = 0; }
	if (p->spn) for (int k = 0; k < MAXSP; k++) if (shm->sp[k].port == name) { note_locked(shm->sp[k].notify, 72, name); shm->sp[k].port = 0; }   // SEND_POSSIBLE pendientes: el puerto murió
	p->spn = 0;
	for (int k = 0; k < MAXSO; k++) if (shm->so[k].port == name) shm->so[k].port = 0;     // ya no hay a quién responder
	if (local_sock[i] >= 0) { close(local_sock[i]); local_sock[i] = -1; }
	nucleo_avisar(&p->cv);   // quien espere en este puerto debe enterarse de que murió
}
static void destroy(uint32_t name) {
	lock();
	struct sport* p = find_locked(name);
	if (p) destroy_locked(p);
	unlock();
	flush_notes();
}

// ---------------------------------------------------------------- recolector de procesos muertos
// Al terminar una tarea, XNU destruye todos sus derechos. Aquí un hilo del primer proceso mira cada poco qué procesos
// murieron (o son zombis) y hace lo mismo: destruye sus derechos de recepción y sus send-once pendientes.
static uint64_t proc_start(int pid) {
	char p[64], b[1024]; snprintf(p, sizeof p, "/proc/%d/stat", pid);
	int fd = open(p, O_RDONLY | O_CLOEXEC); if (fd < 0) return 0;
	ssize_t n = read(fd, b, sizeof b - 1); close(fd);
	if (n <= 0) return 0;
	b[n] = 0;
	char* e = strrchr(b, ')'); if (!e) return 0;
	char st = e[2];
	if (st == 'Z' || st == 'X') {                            // líder zombi: el proceso vive si le quedan hilos (dispatch_main
		long nth = 0; int f2 = 3;                              // termina el hilo principal); campo 20 = número de hilos
		for (char* q = e + 2; *q && f2 < 20; q++) if (*q == ' ') { f2++; if (f2 == 20) nth = strtol(q + 1, NULL, 10); }
		if (nth < 2) return 0;
	}
	unsigned long long v = 0; int f = 3;                     // campo 22: hora de arranque
	for (char* q = e + 2; *q && f < 22; q++) if (*q == ' ') { f++; if (f == 22) v = strtoull(q + 1, NULL, 10); }
	return v ? v : 1;
}
static int proc_alive_unlocked(int32_t pid, uint64_t want) {
	if (pid <= 0) return 1;
	uint64_t st = proc_start(pid);
	return st && (!want || st == want);
}
// Ocupación de las tablas compartidas, para ver fugas: puertos (y de cuántos dueños; «dueño muerto» = su proceso ya no
// existe), derechos de envío anotados (sh) y cuántos son de procesos muertos, objetos del núcleo, vigilancias NOTE_EXEC (y
// cuántas de procesos vigilados que ya terminaron), avisos dead-name y respuestas send-once pendientes.
static int muerto(int32_t pid) { return pid > 0 && kill(pid, 0) != 0 && errno == ESRCH; }
static void ocupacion(void) {
	int np = 0, npm = 0, nsh = 0, nshm = 0, nk = 0, nkm = 0, nx = 0, nxm = 0, ndn = 0, ndnm = 0, nso = 0, nsom = 0;
	lock();
	for (int i = 0; i < MAXPORTS; i++) if (shm->ports[i].used) { np++; npm += muerto(shm->ports[i].owner); }
	for (int i = 0; i < MAXSH; i++) if (shm->sh[i].port) { nsh++; nshm += muerto(shm->sh[i].holder); }
	for (int i = 0; i < MAXKOBJ; i++) if (shm->kobjs[i].name) { nk++; nkm += muerto(shm->kobjs[i].pid); }
	for (int i = 0; i < MAXEXECW; i++) if (shm->execw[i].port) { nx++; nxm += muerto(shm->execw[i].target); }
	for (int i = 0; i < MAXDN; i++) if (shm->dn[i].port) { ndn++; ndnm += muerto(shm->dn[i].pid); }
	for (int i = 0; i < MAXSO; i++) if (shm->so[i].port) { nso++; nsom += muerto(shm->so[i].holder); }
	// los 6 procesos que más puertos reciben y los 6 que más derechos de envío anotados tienen
	static int32_t pp[1024], ph[1024]; static int cp[1024], ch[1024]; int npp = 0, nph = 0;
	for (int i = 0; i < MAXPORTS; i++) if (shm->ports[i].used) {
		int k; for (k = 0; k < npp && pp[k] != shm->ports[i].rpid; k++) {}
		if (k == npp) { if (npp == 1024) continue; pp[npp] = shm->ports[i].rpid; cp[npp++] = 0; }
		cp[k]++;
	}
	for (int i = 0; i < MAXSH; i++) if (shm->sh[i].port) {
		int k; for (k = 0; k < nph && ph[k] != shm->sh[i].holder; k++) {}
		if (k == nph) { if (nph == 1024) continue; ph[nph] = shm->sh[i].holder; ch[nph++] = 0; }
		ch[k]++;
	}
	unlock();
	char top[400]; size_t tl = 0;
	for (int r = 0; r < 12; r++) {
		int32_t* pv = r < 6 ? pp : ph; int* cv = r < 6 ? cp : ch; int n = r < 6 ? npp : nph;
		int b = -1; for (int k = 0; k < n; k++) if (cv[k] > 0 && (b < 0 || cv[k] > cv[b])) b = k;
		if (r == 6) tl += (size_t)snprintf(top + tl, sizeof top - tl, " | envío:");
		if (b < 0 || tl >= sizeof top - 20) continue;
		tl += (size_t)snprintf(top + tl, sizeof top - tl, " %d:%d", (int)pv[b], cv[b]);
		cv[b] = -cv[b];
	}
	logf_("    ocupación, más puertos:%s\n", top);
	for (int r = 0, hechos = 0; np > MAXPORTS * 3 / 4 && r < npp && hechos < 6; r++) {   // casi llena: detalle de los 6 primeros
		int b = -1; for (int k = 0; k < npp; k++) if (cp[k] < 0 && (b < 0 || cp[k] < cp[b])) b = k;
		if (b < 0) break;
		cp[b] = 0;
		puertos_de(pp[b]); hechos++;
	}
	logf_("    ocupación (usados/de muertos): puertos %d/%d de %d, envío %d/%d de %d, kobj %d/%d de %d, NOTE_EXEC %d/%d de %d, dead-name %d/%d de %d, send-once %d/%d de %d\n",
		np, npm, MAXPORTS, nsh, nshm, MAXSH, nk, nkm, MAXKOBJ, nx, nxm, MAXEXECW, ndn, ndnm, MAXDN, nso, nsom, MAXSO);
}
static void* reaper_thread(void* arg) {
	(void)arg;
	int32_t pids[256]; uint64_t starts[256];
	for (unsigned vuelta = 1;; vuelta++) {
		struct timespec ts = { 0, 300 * 1000000L }; nanosleep(&ts, NULL);
		if (vuelta % 100 == 0) ocupacion();                   // cada 30 s: cuánto se usa cada tabla (fugas)
		int np = 0;
		lock();
		// Se recoge hasta 256 pids distintos por vuelta, empezando donde se quedó la anterior: con más de 256 dueños en la
		// tabla, los del final nunca se revisaban y sus puertos y derechos no se limpiaban al morir.
		static int cursor;
		int j0 = cursor;
		for (int j = 0; j < MAXPORTS + MAXSO + MAXMER && np < 256; j++) {
			int i = (j0 + j) % (MAXPORTS + MAXSO + MAXMER);
			cursor = (i + 1) % (MAXPORTS + MAXSO + MAXMER);
			int32_t pid = i < MAXPORTS ? (shm->ports[i].used ? shm->ports[i].owner : 0)
				: i < MAXPORTS + MAXSO ? (shm->so[i - MAXPORTS].port ? shm->so[i - MAXPORTS].holder : 0)
				: (shm->mer[i - MAXPORTS - MAXSO].port ? shm->mer[i - MAXPORTS - MAXSO].pid : 0);
			if (pid <= 1) continue;                           // launchd (1) no muere sin llevarse todo
			int seen = 0; for (int k = 0; k < np; k++) if (pids[k] == pid) { seen = 1; break; }
			if (seen) continue;
			struct sproc* sp = &shm->procs[pid % MAXPROC];
			pids[np] = pid; starts[np] = sp->pid == pid ? sp->start : 0; np++;
		}
		unlock();
		int ndead = 0; int32_t dead[256];
		for (int k = 0; k < np; k++) if (!proc_alive_unlocked(pids[k], starts[k])) dead[ndead++] = pids[k];
		if (!ndead) continue;
		lock();
		for (int k = 0; k < ndead; k++) {
			int32_t pid = dead[k];
			int nso = 0, nrecv = 0;
			for (int i = 0; i < MAXSO; i++) if (shm->so[i].port && shm->so[i].holder == pid) { note_locked(shm->so[i].port, 71, 0); shm->so[i].port = 0; nso++; }
			for (int i = 0; i < MAXDN; i++) if (shm->dn[i].port && shm->dn[i].pid == pid) shm->dn[i].port = 0;
			for (int i = 0; i < MAXMER; i++) if (shm->mer[i].port && shm->mer[i].pid == pid) mer_add_locked(shm->mer[i].port, pid, -shm->mer[i].refs);
			for (int i = 0; i < MAXSP; i++) if (shm->sp[i].port && shm->sp[i].pid == pid) { struct sport* q = find_locked(shm->sp[i].port); if (q && q->spn) q->spn--; shm->sp[i].port = 0; }
			for (int i = 0; i < MAXSH; i++) {                     // sus derechos de envío desaparecen: ¿queda alguien más?
				if (!shm->sh[i].port || shm->sh[i].holder != pid) continue;
				uint32_t port = shm->sh[i].port; shm->sh[i].port = 0;
				int others = 0;
				for (int j = 0; j < MAXSH && !others; j++) if (shm->sh[j].port == port) others = 1;
				struct sport* q = others ? NULL : find_locked(port);
				if (q && q->nsreq) { note_locked(q->nsreq, 70, q->nsmscount); q->nsreq = 0; }   // el aviso consume la petición
			}
			if (mach_trace()) for (int i = 0; i < MAXPORTS; i++) if (shm->ports[i].used && shm->ports[i].owner == pid) logf_("    recolector: 0x%x era de %d (rpid %d)\n", shm->ports[i].name, (int)pid, (int)shm->ports[i].rpid);
			for (int i = 0; i < MAXPORTS; i++) if (shm->ports[i].used && shm->ports[i].owner == pid) { destroy_locked(&shm->ports[i]); nrecv++; }
			logf_("    recolector: pid %d murió (%d derechos de recepción, %d respuestas pendientes)\n", (int)pid, nrecv, nso);
		}
		unlock();
		flush_notes();
	}
	return NULL;
}
static void start_reaper(void) {
	pthread_attr_t a; pthread_attr_init(&a); pthread_attr_setstacksize(&a, 2 << 20);   // TLS estático grande: pila de 2 MB
	pthread_t th;
	if (pthread_create(&th, &a, reaper_thread, NULL) == 0) pthread_detach(th);
	else logf_("    recolector: no pude crear el hilo\n");
	pthread_attr_destroy(&a);
}

// Descriptores fuera de línea (memoria, memoria volátil y listas de puertos): el contenido se copia dentro del mensaje
// porque la dirección del emisor no vale en el proceso receptor. El recorrido lo hace nucleo_descriptores (Rust): los
// descriptores de puerto con guarda (tipo 4, 16 bytes) son puertos, no datos; la versión anterior en C los copiaba como
// memoria (de la «dirección» que es la guarda, con el nombre del puerto como tamaño).
static int ool_scan(const uint8_t* m, uint32_t size, uint32_t* out_off, uint32_t* out_len, int max) {
	struct nucleo_desc d[64];
	int nd = nucleo_descriptores(m, size, d, 64);
	if (nd < 0) { logf_("    puertos: mensaje mal formado (id=%d)\n", size >= 24 ? (int)(m[20] | m[21] << 8 | m[22] << 16 | (uint32_t)m[23] << 24) : -1); return 0; }
	if (nd > 64) nd = 64;
	int n = 0;
	for (int i = 0; i < nd && n < max; i++) {
		if (nucleo_fuera_de_linea(d[i].clase)) { out_off[n] = d[i].off; out_len[n] = d[i].bytes; n++; }
		// COMPATIBILIDAD (temporal): la versión en C trataba también el puerto con guarda como datos fuera de línea (la
		// guarda como dirección y el nombre como tamaño). Es un fallo, pero al corregirlo diskutil deja de funcionar
		// (storagekitd no responde): se mantiene hasta entender qué depende de él. La fase 1 del núcleo en Rust es una
		// traducción fiel; las correcciones de comportamiento van aparte, de una en una.
		else if (d[i].clase == NUCLEO_PUERTO_GUARDA) { out_off[n] = d[i].off; out_len[n] = d[i].nombre; n++; }
	}
	return n;
}

// Encola un mensaje completo (cabecera con los campos reales). 0 si el puerto existe.
int port_send(uint32_t dest, const uint8_t* msg, uint32_t size) { return port_send_k(dest, msg, size, 0); }
static int port_send_t(uint32_t dest, const uint8_t* msg, uint32_t size, int kernel, int timeout_ms, int notify);
static int port_send_k(uint32_t dest, const uint8_t* msg, uint32_t size, int kernel) { return port_send_t(dest, msg, size, kernel, -1, 0); }
// Envío desde mach_msg con su plazo (MACH_SEND_TIMEOUT): 0 si se encola, -1 si no se puede, o un código mach_msg
// (MACH_SEND_TIMED_OUT, MACH_SEND_INVALID_DEST).
int port_send_timeout(uint32_t dest, const uint8_t* msg, uint32_t size, int timeout_ms, int notify) { return port_send_t(dest, msg, size, 0, timeout_ms, notify); }
// kernel = 1: mensaje del "núcleo" (notificaciones): remitente pid 0 en el trailer de auditoría y sin derechos que ceder.
// Límite de cola (como ipc_mqueue_send de XNU): con la cola del puerto llena (qlimit, 5 por omisión), el remitente espera a
// que el receptor saque mensajes, o hasta su plazo. No cuentan los mensajes del núcleo ni los que van a un derecho
// send-once (respuestas): XNU siempre los encola. Sin esto, un puerto que nadie vacía agota la reserva común de ranuras y
// todos los envíos fallan (libxpc aborta: "MACH_SEND_NO_BUFFER returned for reliable message size").
static int port_send_t(uint32_t dest, const uint8_t* msg, uint32_t size, int kernel, int timeout_ms, int notify) {
	uint32_t ooff[32], olen[32];
	uint8_t desc_fix[32 * 16];
	int nool = ool_scan(msg, size, ooff, olen, 32);
	uint64_t total = size;
	for (int i = 0; i < nool; i++) total += olen[i];
	(void)desc_fix;
	// Mensaje grande: si con sus datos fuera de línea no cabe en una ranura, esos datos van a un archivo de desbordamiento
	// (el mensaje en sí siempre debe caber). Se escribe antes de tomar el cerrojo: puede ser lento.
	uint64_t spill = 0;
	if (total > sizeof(((struct mslot*)0)->data)) {
		if (size > sizeof(((struct mslot*)0)->data)) { logf_("    puertos: mensaje de %u bytes demasiado grande\n", size); return -1; }
		uint8_t* buf = malloc(total - size ? total - size : 1);
		if (!buf) return -1;
		uint64_t pos = 0;
		for (int i = 0; i < nool; i++) {
			uint64_t addr; memcpy(&addr, msg + ooff[i], 8);
			if (olen[i] && safe_read(addr, buf + pos, olen[i]) != (ssize_t)olen[i]) memset(buf + pos, 0, olen[i]);
			pos += olen[i];
		}
		pthread_once(&once, init_all);
		spill = __atomic_add_fetch(&shm->spill_next, 1, __ATOMIC_SEQ_CST);
		int w = nucleo_desborde_guardar(tag, spill, buf, total - size);
		free(buf);
		if (w != 0) { logf_("    puertos: no se pudo desbordar un mensaje de %lu bytes\n", (unsigned long)total); return -1; }
	}
	lock();
	struct sport* p = find_locked(dest);
	uint32_t rbits = 0; memcpy(&rbits, msg, 4); rbits &= 0x1f;      // disposición del destino
	if (p && !p->is_set && !kernel && rbits != 18 && rbits != 21) {   // MOVE_SEND_ONCE / MAKE_SEND_ONCE: sin límite
		struct timespec dl;
		// MACH_SEND_NOTIFY con la cola llena: se arma la petición SEND_POSSIBLE de este proceso sobre el destino, si la hay
		// (ipc_kmsg_copyin_header -> ipc_port_request_sparm). Solo las armadas avisan cuando se saca un mensaje.
		if (notify && p->spn && p->count >= (p->qlimit ? p->qlimit : 5)) {
			int32_t me = (int32_t)getpid();
			for (int k = 0; k < MAXSP; k++) if (shm->sp[k].port == dest && shm->sp[k].pid == me) shm->sp[k].armado = 1;
		}
		if (timeout_ms > 0) { clock_gettime(CLOCK_MONOTONIC, &dl); dl.tv_sec += timeout_ms / 1000; dl.tv_nsec += (timeout_ms % 1000) * 1000000L; if (dl.tv_nsec >= 1000000000L) { dl.tv_sec++; dl.tv_nsec -= 1000000000L; } }
		while (p && p->count >= (p->qlimit ? p->qlimit : 5)) {
			if (timeout_ms == 0) { unlock(); if (spill) nucleo_desborde_borrar(tag, spill); return 0x10000004; }   // MACH_SEND_TIMED_OUT
			int r = nucleo_esperar(&p->cv, &shm->lk, timeout_ms > 0 ? &dl : NULL);
			p = find_locked(dest);                                    // el puerto pudo morir mientras se esperaba
			if (r == ETIMEDOUT && p && p->count >= (p->qlimit ? p->qlimit : 5)) { unlock(); if (spill) nucleo_desborde_borrar(tag, spill); return 0x10000004; }
		}
		if (!p) { unlock(); if (spill) nucleo_desborde_borrar(tag, spill); return 0x10000003; }   // MACH_SEND_INVALID_DEST
	}
	if (!p || p->is_set || !shm->free_head) {
		if (p && !shm->free_head) logf_("    puertos: reserva de mensajes agotada (envío a 0x%x)\n", dest);
		unlock(); if (spill) nucleo_desborde_borrar(tag, spill); return -1;
	}
	uint32_t idx = shm->free_head;
	struct mslot* q = &shm->msgs[idx - 1];
	shm->free_head = q->next;
	q->next = 0; q->size = size; q->total = (uint32_t)total; q->spill = spill;
	q->spid = kernel ? 0 : (uint32_t)getpid(); q->suid = kernel ? 0 : cred_euid(); q->sgid = kernel ? 0 : cred_egid();
	q->sauid = kernel ? (uint32_t)-1 : cred_auid(); q->sasid = kernel ? 0 : cred_asid();   // para el trailer de auditoría del receptor
	if (!kernel) msg_rights_locked(msg, size, RIGHTS_SEND, dest);
	memcpy(q->data, msg, size);
	uint32_t pos = size;
	for (int i = 0; i < nool && !spill; i++) {             // copiar los bytes de cada descriptor OOL tras el mensaje
		uint64_t addr; memcpy(&addr, msg + ooff[i], 8);
		if (olen[i] && safe_read(addr, q->data + pos, olen[i]) != (ssize_t)olen[i]) memset(q->data + pos, 0, olen[i]);
		pos += olen[i];
	}
	if (p->tail) shm->msgs[p->tail - 1].next = idx; else p->head = idx;
	p->tail = idx; p->count++;
	uint32_t setnames[NSETS]; int32_t rp_sets[NSETS] = { 0 };
	int32_t rp_dest = p->rpid;
	memcpy(setnames, p->sets, sizeof setnames);
	for (int j = 0; j < NSETS; j++) if (setnames[j]) { struct sport* st = find_locked(setnames[j]); if (st) { st->count++; rp_sets[j] = st->rpid; } }
	nucleo_avisar(&p->cv);
	for (int j = 0; j < NSETS; j++) if (setnames[j]) { struct sport* st2 = find_locked(setnames[j]); if (st2) nucleo_avisar(&st2->cv); }
	unlock();
	poke(dest, rp_dest);
	for (int j = 0; j < NSETS; j++) if (setnames[j]) poke(setnames[j], rp_sets[j]);
	return 0;
}

// Saca un mensaje de un puerto o de cualquier miembro de un conjunto. timeout_ms < 0: sin plazo.
// Devuelve 0 con *out/*size (a liberar con free), o un código mach_msg de error.
int port_receive(uint32_t name, int timeout_ms, uint8_t** out, uint32_t* size, uint32_t* sender /* pid, uid, gid, auid, asid */, uint32_t limit, int large) {
	struct timespec dl;
	if (timeout_ms >= 0) { clock_gettime(CLOCK_MONOTONIC, &dl); dl.tv_sec += timeout_ms / 1000; dl.tv_nsec += (timeout_ms % 1000) * 1000000L; if (dl.tv_nsec >= 1000000000L) { dl.tv_sec++; dl.tv_nsec -= 1000000000L; } }
	lock();
	int waited = 0;
	for (;;) {
		struct sport* p = find_locked(name);
		if (!p) { unlock(); return waited ? MACH_RCV_PORT_DIED_ : MACH_RCV_INVALID_NAME_; }
		struct sport* src = NULL;
		if (!p->is_set) src = p->count ? p : NULL;
		else for (int i = 0; i < MAXPORTS; i++) if (shm->ports[i].used && shm->ports[i].count && in_set(&shm->ports[i], name)) { src = &shm->ports[i]; break; }
		if (src) {
			uint32_t idx = src->head;
			struct mslot* q = &shm->msgs[idx - 1];
			if (large && limit && q->total > limit) {                       // MACH_RCV_LARGE: no se consume; se informa del tamaño necesario
				uint8_t* h = malloc(q->size ? q->size : 1);
				memcpy(h, q->data, q->size);
				*size = q->size; *out = h;
				unlock();
				return 0x10004004;
			}
			src->head = q->next; if (!src->head) src->tail = 0;
			src->count--;
			nucleo_avisar(&src->cv);                     // hay sitio: remitentes esperando por el límite de cola
			if (src->spn && src->count < (src->qlimit ? src->qlimit : 5)) {   // y las peticiones SEND_POSSIBLE armadas (ipc_port_spnotify);
				for (int k = 0; k < MAXSP; k++)                                   // las no armadas siguen esperando
					if (shm->sp[k].port == src->name && shm->sp[k].armado) { note_locked(shm->sp[k].notify, 66, src->name); shm->sp[k].port = 0; src->spn--; }
			}
			msg_rights_locked(q->data, q->size, RIGHTS_RECV, src->name);
			for (int j = 0; j < NSETS; j++) if (src->sets[j]) { struct sport* s = find_locked(src->sets[j]); if (s && s->count) s->count--; }
			uint32_t sz = q->size, tot = q->total; uint64_t spill = q->spill;
			if (sender) { sender[0] = q->spid; sender[1] = q->suid; sender[2] = q->sgid; sender[3] = q->sauid; sender[4] = q->sasid; }
			uint8_t* m = malloc(tot ? tot : 1);
			memcpy(m, q->data, spill ? sz : tot);
			q->spill = 0;
			q->next = shm->free_head; shm->free_head = idx;
			int si = port_index(src);
			int fd1 = local_sock[si], fd2[NSETS];
			uint32_t rname1 = src->name, rname2[NSETS] = { 0 };
			for (int j = 0; j < NSETS; j++) { fd2[j] = -1; if (src->sets[j]) { struct sport* s = find_locked(src->sets[j]); if (s) { fd2[j] = local_sock[port_index(s)]; rname2[j] = s->name; } } }
			unlock();
			flush_notes();
			if (spill && nucleo_desborde_leer(tag, spill, m + sz, tot - sz) != 0) logf_("    puertos: datos desbordados %lu perdidos\n", (unsigned long)spill);
			drain_recheck(rname1, fd1);                         // si aún quedan mensajes (o llegó uno), se renueva el aviso
			for (int j = 0; j < NSETS; j++) if (rname2[j]) drain_recheck(rname2[j], fd2[j]);
			// materializar los descriptores OOL en memoria nueva de este proceso
			uint32_t ooff[32], olen[32];
			int nool = ool_scan(m, sz, ooff, olen, 32);
			uint32_t pos = sz;
			for (int i = 0; i < nool; i++) {
				uint64_t addr = 0;
				if (olen[i]) {
					void* mem = mmap(NULL, (olen[i] + 4095u) & ~4095u, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
					if (mem != MAP_FAILED) { memcpy(mem, m + pos, olen[i]); addr = (uint64_t)mem; }
				}
				memcpy(m + ooff[i], &addr, 8);
				pos += olen[i];
			}
			*out = m; *size = sz;
			return 0;
		}
		waited = 1;
		if (trace_all) logf_("    mach_msg: <%d> espera mensaje en 0x%x (plazo %d ms)\n", (int)getpid(), name, timeout_ms);
		int r = nucleo_esperar(&p->cv, &shm->lk, timeout_ms >= 0 ? &dl : NULL);
		if (r == ETIMEDOUT) { unlock(); return MACH_RCV_TIMED_OUT; }
	}
}

// Puertos especiales de la tarea (bootstrap = 4): compartidos, para que los hijos hereden el de launchd.
// /var/run/syslog: libsystem usa asl/syslog como último recurso para mensajes de error (p. ej. launchd cuando rechaza una
// petición). Un hilo del proceso creador escucha en ese socket de datagramas y vuelca el texto al registro.
static void* syslog_thread(void* arg) {
	int s = (int)(long)arg;
	char buf[4096];
	for (;;) {
		ssize_t n = recv(s, buf, sizeof buf - 1, 0);
		if (n <= 0) { if (errno == EINTR) continue; break; }
		char out[4096]; size_t o = 0, run = 0;                 // texto imprimible del mensaje (ASL es binario)
		for (ssize_t i = 0; i < n && o < sizeof out - 2; i++) {
			unsigned char ch = (unsigned char)buf[i];
			if (ch >= 32 && ch < 127) { out[o++] = (char)ch; run++; }
			else { if (run >= 3 && o < sizeof out - 2) out[o++] = '|'; else o -= run; run = 0; }
		}
		out[o] = 0;
		logf_("    [syslog] %s\n", out);
	}
	return NULL;
}
void start_syslog_listener(void) {
	if (!tahoe_root) return;
	char path[4300];
	snprintf(path, sizeof path, "%s/private/var/run/syslog", tahoe_root);
	unlink(path);
	int s = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	struct sockaddr_un a = { .sun_family = AF_UNIX };
	snprintf(a.sun_path, sizeof a.sun_path, "%s", path);
	if (s < 0 || bind(s, (struct sockaddr*)&a, sizeof a) < 0) { if (s >= 0) close(s); return; }
	chmod(path, 0666);
	pthread_t th;
	pthread_create(&th, NULL, syslog_thread, (void*)(long)s);
	pthread_detach(th);
}

// Puertos especiales de ESTA tarea. Los hijos reciben los suyos por el entorno (TAHOE_SP<n>) desde las acciones de
// puerto de posix_spawn, como el bootstrap que launchd da a cada servicio.
static uint32_t special[8];
static pthread_once_t sp_once = PTHREAD_ONCE_INIT;
static void sp_init(void) {
	for (int i = 0; i < 8; i++) { char k[16]; snprintf(k, sizeof k, "TAHOE_SP%d", i); const char* v = getenv(k); if (v) special[i] = (uint32_t)strtoul(v, NULL, 10); }
}
// mach_ports_register(ports[3]): se guardan en la región compartida; los hijos los heredan (en XNU, task_create copia
// itk_registered) y, mientras no tengan puerto bootstrap propio, usan el primero: así encuentran a launchd.
// kern.osvariant_status (kern_sysctl.c de XNU): un valor por arranque, compartido por todos los procesos. Solo el PID 1 lo
// fija, y una sola vez; si no, EPERM (devuelve 0 si se fijó).
uint64_t osvariant_status_get(void) { lock(); uint64_t v = shm->osvariant_status; unlock(); return v; }
int osvariant_status_set(uint64_t v) {
	if (getpid() != 1) return -1;
	lock(); int ok = shm->osvariant_status == 0; if (ok) shm->osvariant_status = v; unlock();
	return ok ? 0 : -1;
}
// Puertos registrados (mach_ports_register): como en XNU son de cada tarea y los hereda el hijo (task_create copia
// itk_registered); aquí viajan en TAHOE_REG al hacer exec/posix_spawn. Solo los de launchd (PID 1) se guardan además en la
// región compartida: son el respaldo de un proceso que no desciende de launchd (la shell que se une con nsenter). Antes
// eran globales: el último proceso que registraba (p. ej. al crear una sesión) cambiaba el bootstrap de todos los demás.
static uint32_t reg_local[3]; static int reg_known;
static pthread_once_t reg_once = PTHREAD_ONCE_INIT;
static void reg_init(void) {
	const char* v = getenv("TAHOE_REG");
	if (v && sscanf(v, "%u,%u,%u", &reg_local[0], &reg_local[1], &reg_local[2]) == 3) reg_known = 1;
}
void registered_ports_set(const uint32_t* p, int n) {
	pthread_once(&reg_once, reg_init);
	for (int i = 0; i < 3; i++) reg_local[i] = i < n ? p[i] : 0;
	reg_known = 1;
	if (getpid() == 1) { lock(); for (int i = 0; i < 3; i++) shm->special[i] = reg_local[i]; unlock(); logf_("    bootstrap: launchd registra 0x%x 0x%x 0x%x\n", reg_local[0], reg_local[1], reg_local[2]); }
}
// Un puerto registrado nulo no cuenta como heredado: la shell se une a launchd con nsenter y, si lo hace antes de que launchd
// registre su bootstrap, toma ceros y los pasaba a todos sus hijos (TAHOE_REG=0,0,0), que se quedaban sin launchd todo el
// arranque: launchctl, defaults, diskutil... fallaban con «141: Reentrancy avoided» en la mitad de los arranques. En XNU no
// puede pasar (todo proceso desciende de launchd); aquí, si no hay uno propio, se mira el que registró launchd.
static uint32_t registered_port(int i) {
	pthread_once(&reg_once, reg_init);
	if (reg_known && reg_local[i]) return reg_local[i];
	lock(); uint32_t v = shm->special[i]; unlock(); return v;
}
// Entorno que hereda un hijo en exec/posix_spawn: sus puertos especiales (el bootstrap, sobre todo) y los registrados, como
// task_create de XNU. Las acciones de puerto de posix_spawn van antes en el entorno y tienen prioridad (getenv da la primera).
int ports_inherit_env(char out[][40], int max) {
	pthread_once(&sp_once, sp_init);
	pthread_once(&reg_once, reg_init);
	int k = 0;
	for (int i = 0; i < 8 && k < max; i++) if (special[i]) snprintf(out[k++], 40, "TAHOE_SP%d=%u", i, special[i]);
	if (reg_known && (reg_local[0] || reg_local[1] || reg_local[2]) && k < max)   // ceros no se heredan (ver registered_port)
		snprintf(out[k++], 40, "TAHOE_REG=%u,%u,%u", reg_local[0], reg_local[1], reg_local[2]);
	return k;
}
uint32_t special_port_get(int which) {
	pthread_once(&sp_once, sp_init);
	if (which < 0 || which >= 8) return 0;
	uint32_t r = (which == 4 && !special[4]) ? registered_port(0) : special[which];
	if (which == 4 && getpid() != 1) {                             // diagnóstico: un proceso que se queda sin launchd
		static int dicho;
		if ((!r || !port_exists(r)) && !dicho++)
			logf_("    bootstrap <%d>: %s 0x%x (propio=0x%x registrado=%s, launchd registró 0x%x)\n", (int)getpid(), r ? "puerto muerto" : "nulo", r,
				special[4], reg_known ? "heredado" : "de la región", shm ? shm->special[0] : 0);
	}
	return r;
}
void special_port_set(int which, uint32_t name) { pthread_once(&sp_once, sp_init); if (which >= 0 && which < 8) special[which] = name; }

// ---------------------------------------------------------------- trampas Mach
// mach_port_allocate_trap(task, right, *name): 1 = derecho de recepción, 3 = conjunto de puertos
static long t_allocate(struct ctx* c) {
	if (c->a[1] != 1 && c->a[1] != 3) return KERN_INVALID_VALUE_;
	uint32_t n = port_create(c->a[1] == 3);
	if (!n) return 6;                                // KERN_RESOURCE_SHORTAGE: tabla llena (no un nombre 0 con éxito)
	return safe_write(c->a[2], &n, 4) == 4 ? KERN_OK : KERN_INVALID_VALUE_;
}
// mach_port_construct_trap(task, options*, context, *name)
static long t_construct(struct ctx* c) {
	uint32_t flags = 0;
	if (c->a[1]) safe_read(c->a[1], &flags, 4);
	uint32_t n = port_create(0);                     // construct nunca crea conjuntos (el bit 8 es MPO_IMPORTANCE_RECEIVER)
	if (!n) return 6;                                // KERN_RESOURCE_SHORTAGE: tabla llena (libxpc aborta si recibe éxito con nombre 0)
	{                         // el contexto, o la guarda con MPO_CONTEXT_AS_GUARD (0x1; MPO_STRICT 0x20)
		if (flags & 0x1) port_guard(n, c->a[2], (flags & 0x20) != 0);
		else port_set_context(n, c->a[2]);
		if (flags & 0x400) { lock(); struct sport* p = find_locked(n); if (p) { p->svc = 1; p->svc_ctx = (flags & 0x1) ? c->a[2] : 0; } unlock(); }   // MPO_SERVICE_PORT
		if (flags & 0x2) {                                   // MPO_QLIMIT: mach_port_options_t.mpl.mpl_qlimit (+4)
			uint32_t ql = 0; safe_read(c->a[1] + 4, &ql, 4);
			lock(); struct sport* p = find_locked(n); if (p) p->qlimit = ql > 2048 ? 2048 : ql; unlock();
		}
	}
	if (mach_trace()) { uint32_t o[6] = { 0 }; if (c->a[1]) safe_read(c->a[1], o, 24); logf_("    MT pid=%d.%ld construct 0x%x contexto=0x%lx flags=0x%x opt=%x %x %x %x\n", (int)getpid(), (long)syscall(SYS_gettid), n, c->a[2], o[0], o[2], o[3], o[4], o[5]); }
	if (trace_all) { uint32_t opt[6] = { 0 }; if (c->a[1]) safe_read(c->a[1], opt, 24); logf_("    construct 0x%x: flags=0x%x qlimit=%u opt2=0x%x opt3=0x%x contexto=0x%lx\n", n, opt[0], opt[1], opt[2], opt[3], c->a[2]); }
	return safe_write(c->a[3], &n, 4) == 4 ? KERN_OK : KERN_INVALID_VALUE_;
}
// Los nombres de puerto son globales, pero en Mach cada tarea tiene los suyos: quien suelta su derecho de ENVÍO no debe destruir el
// puerto del receptor. Solo el dueño del derecho de recepción (el creador, o quien lo recibió con MOVE_RECEIVE) puede destruirlo.
// Vacía el aviso de un puerto sin mensajes (o inexistente): evita que un aviso viejo mantenga legible el descriptor y
// el kqueue vuelva a despertar sin fin. Devuelve 1 si el puerto ya no existe.
int port_drain_idle(uint32_t name) {
	lock(); struct sport* p = find_locked(name);
	int gone = p == NULL, fd = p ? local_sock[port_index(p)] : -1, cnt = p ? p->count : 0;
	unlock();
	if (!cnt) drain_recheck(name, fd);
	return gone;
}
uint64_t port_get_context(uint32_t name) { lock(); struct sport* p = find_locked(name); uint64_t r = p ? p->ctx : 0; unlock(); return r; }
// mach_port_set_context: un puerto con guarda estricta no deja cambiar su contexto. Devuelve un kern_return_t.
int port_set_context(uint32_t name, uint64_t v) {
	lock(); struct sport* p = find_locked(name);
	int r = !p ? 15 /* KERN_INVALID_NAME */ : p->strict_guard ? 4 /* KERN_INVALID_ARGUMENT */ : 0;
	if (!r) p->ctx = v;
	unlock();
	if (r == 4) logf_("    guarda: <%d> mach_port_set_context(0x%x) sobre guarda estricta\n", (int)getpid(), name);
	return r;
}
// mach_port_assert_attributes(MACH_PORT_GUARD_INFO): el puerto tiene guarda estricta con ese valor
int port_assert_guard(uint32_t name, uint64_t guard) {
	lock(); struct sport* p = find_locked(name);
	int r = !p ? 15 : (p->guarded && p->strict_guard && p->ctx == guard) ? 0 : 4;
	unlock(); return r;
}
// mach_port_get_refs(name, right): como los nombres son globales, se responde según lo que este proceso tiene de ese puerto.
// right: 0 SEND, 1 RECEIVE, 2 SEND_ONCE, 3 PORT_SET, 4 DEAD_NAME. Un nombre que fue puerto y ya no existe es un nombre
// muerto (1 ref de DEAD_NAME, 0 del resto); uno que nunca existió, KERN_INVALID_NAME.
int port_get_refs(uint32_t name, uint32_t right, uint32_t* refs) {
	*refs = 0;
	lock();
	struct sport* p = find_locked(name);
	int r = 0;
	int32_t me = (int32_t)getpid();
	if (!p) {
		if (name && name < shm->next_name && (name & 0xff) == 0x03) { if (right == 4) *refs = 1; }
		else r = KERN_INVALID_NAME_;
	} else switch (right) {
	case 0: *refs = p->is_set ? 0 : 1; break;
	case 1: *refs = (!p->is_set && p->owner == me) ? 1 : 0; break;
	case 2:
		for (int i = 0; i < MAXSO; i++) if (shm->so[i].port == name && shm->so[i].holder == me) (*refs)++;
		break;
	case 3: *refs = (p->is_set && p->owner == me) ? 1 : 0; break;
	case 4: *refs = 0; break;
	default: r = 4;                                               // KERN_INVALID_VALUE (MACH_PORT_RIGHT desconocido)
	}
	unlock();
	return r;
}
// mach_port_swap_guard: cambia la guarda de un puerto guardado (no estricto) si la vieja coincide
int port_swap_guard(uint32_t name, uint64_t old, uint64_t nw) {
	lock(); struct sport* p = find_locked(name);
	int r = !p ? 15 : (!p->guarded || p->strict_guard || p->ctx != old) ? 4 : 0;
	if (!r) p->ctx = nw;
	unlock(); return r;
}
// Guardas de puerto como en XNU (mach_port_guard_locked / unguard_locked): la guarda se guarda en el contexto, que el
// receptor ve en el trailer (msgh_context). libxpc guarda así sus puertos de respuesta con el objeto de la petición.
int port_guard(uint32_t name, uint64_t guard, int strict) {
	lock(); struct sport* p = find_locked(name);
	int r = !p ? 15 : p->ctx ? 4 : 0;                    // ya tiene contexto o guarda: KERN_INVALID_ARGUMENT
	if (!r) { p->ctx = guard; p->guarded = 1; p->strict_guard = strict != 0; }
	uint64_t old = p ? p->ctx : 0;
	unlock();
	if (r) logf_("    guarda: <%d> mach_port_guard(0x%x, 0x%lx) falla %d (contexto 0x%lx)\n", (int)getpid(), name, guard, r, old);
	return r;
}
int port_unguard(uint32_t name, uint64_t guard) {
	lock(); struct sport* p = find_locked(name);
	int r = !p ? 15 : (!p->guarded || p->ctx != guard) ? 4 : 0;
	uint64_t old = p ? p->ctx : 0; int gd = p ? p->guarded : 0;
	if (!r) { p->ctx = 0; p->guarded = p->strict_guard = 0; }
	unlock();
	if (r) logf_("    guarda: <%d> mach_port_unguard(0x%x, 0x%lx) falla %d (contexto 0x%lx, guardado %d)\n", (int)getpid(), name, guard, r, old, gd);
	return r;
}
// Puertos con derecho de recepción de este proceso (mach_port_names): nombre y tipo (RECEIVE|SEND o PORT_SET).
int port_list_owned(uint32_t* names, uint32_t* types, int max) {
	int n = 0;
	lock();
	for (int i = 0; i < MAXPORTS && n < max; i++) {
		struct sport* s = &shm->ports[i];
		if (!s->used || s->rpid != (int32_t)getpid()) continue;
		names[n] = s->name; types[n] = s->is_set ? (1u << 19) : ((1u << 17) | (1u << 16));
		n++;
	}
	unlock();
	return n;
}
int port_rpid(uint32_t name) { lock(); struct sport* p = find_locked(name); int r = p ? p->rpid : -1; unlock(); return r; }
static int owns(uint32_t name) { lock(); struct sport* p = find_locked(name); int r = p && p->owner == (int32_t)getpid(); unlock(); return r; }
// Diagnóstico: una destrucción que se ignora porque el puerto consta de otro dueño (las primeras 20 por proceso).
static void no_es_mio(const char* que, uint32_t name) {
	static int n;
	if (n >= 20) return;
	lock(); struct sport* p = find_locked(name); int32_t o = p ? p->owner : -1, r = p ? p->rpid : -1; unlock();
	if (o == (int32_t)getpid()) return;
	n++;
	logf_("    %s 0x%x ignorado: <%d> no es su dueño (dueño %d, recibe %d%s)\n", que, name, (int)getpid(), (int)o, (int)r, p ? "" : ", no existe");
}
void port_move_receive(uint32_t name) { lock(); struct sport* p = find_locked(name); if (p) { p->owner = (int32_t)getpid(); p->rpid = (int32_t)getpid(); } unlock(); }
extern void diag_crash(ucontext_t* uc);
static long t_destruct(struct ctx* c) {
	if (diag_target()) { logf_("    destruct 0x%x (hilo %ld):\n", (uint32_t)c->a[1], (long)syscall(SYS_gettid)); diag_stack(c->uc, "traza"); }
	if (owns((uint32_t)c->a[1])) destroy((uint32_t)c->a[1]);
	else no_es_mio("destruct", (uint32_t)c->a[1]);
	return KERN_OK;
}
// mach_port_mod_refs_trap(task, name, right, delta): quitar la última referencia de recepción destruye el puerto
// Soltar un derecho send-once sin usarlo genera MACH_NOTIFY_SEND_ONCE (mach_port_deallocate / mod_refs / mach_msg_destroy).
// Los nombres son globales: solo cuenta si este proceso tiene apuntado un send-once de ese puerto.
int port_release_send_once(uint32_t name) {
	lock();
	int had = so_take_locked(name, (int32_t)getpid());
	if (had) note_locked(name, 71, 0);
	unlock();
	flush_notes();
	return had;
}
// mach_port_deallocate_trap(task, nombre)
static long t_deallocate(struct ctx* c) { port_release_send_once((uint32_t)c->a[1]); kobj_release_thread((uint32_t)c->a[1]); port_me_release((uint32_t)c->a[1], -1); return KERN_OK; }
static long t_mod_refs(struct ctx* c) {
	int32_t delta = (int32_t)c->a[3];
	if (c->a[2] == 2 /* SEND_ONCE */ && delta < 0) port_release_send_once((uint32_t)c->a[1]);
	if (c->a[2] == 0 /* SEND */ && delta) port_me_release((uint32_t)c->a[1], delta);
	if (c->a[2] == 1 /* RECEIVE */ && delta < 0 && !owns((uint32_t)c->a[1])) no_es_mio("mod_refs RECEIVE", (uint32_t)c->a[1]);
	else if (c->a[2] == 1 /* RECEIVE */ && delta < 0) {
		if (diag_target()) { logf_("    mod_refs RECEIVE -1 0x%x (hilo %ld):\n", (uint32_t)c->a[1], (long)syscall(SYS_gettid)); diag_stack(c->uc, "traza"); }
		destroy((uint32_t)c->a[1]);
	}
	return KERN_OK;
}
static long t_nop(struct ctx* c) { (void)c; return KERN_OK; }
// mach_port_insert_member_trap(task, name, pset): además de los conjuntos en que ya esté (hasta NSETS).
// Los mensajes ya encolados en el puerto cuentan también para el conjunto nuevo.
static long t_insert_member(struct ctx* c) {
	lock();
	struct sport *p = find_locked((uint32_t)c->a[1]), *st = find_locked((uint32_t)c->a[2]);
	long r = KERN_INVALID_NAME_;
	if (p && st && st->is_set) {
		if (in_set(p, st->name)) r = 17;                                 // KERN_ALREADY_IN_SET
		else { r = 3; for (int j = 0; j < NSETS; j++) if (!p->sets[j]) { p->sets[j] = st->name; st->count += p->count; r = KERN_OK; break; } }   // 3: KERN_NO_SPACE
	}
	unlock();
	return r;
}
// mach_port_move_member_trap(task, member, after): sale de todos sus conjuntos y entra en «after» (MACH_PORT_NULL: en ninguno)
static long t_move_member(struct ctx* c) {
	lock();
	struct sport *p = find_locked((uint32_t)c->a[1]), *st = c->a[2] ? find_locked((uint32_t)c->a[2]) : NULL;
	long r = KERN_INVALID_NAME_;
	if (p && (!c->a[2] || (st && st->is_set))) {
		for (int j = 0; j < NSETS; j++) if (p->sets[j]) { struct sport* o = find_locked(p->sets[j]); if (o && o->count >= p->count) o->count -= p->count; p->sets[j] = 0; }
		if (st) { p->sets[0] = st->name; st->count += p->count; }
		r = KERN_OK;
	}
	unlock();
	return r;
}
// mach_port_extract_member_trap(task, name, pset): sale de ese conjunto
static long t_extract_member(struct ctx* c) {
	lock();
	struct sport *p = find_locked((uint32_t)c->a[1]), *st = find_locked((uint32_t)c->a[2]);
	long r = KERN_INVALID_NAME_;
	if (p && st && st->is_set) {
		if (!in_set(p, st->name)) r = 18;                                // KERN_NOT_IN_SET
		else { set_remove(p, st->name); if (st->count >= p->count) st->count -= p->count; r = KERN_OK; }
	}
	unlock();
	return r;
}
// mach_port_type_trap(task, name, *tipo): bits de MACH_PORT_TYPE_* del nombre
static long t_type(struct ctx* c) {
	uint32_t name = (uint32_t)c->a[1], t;
	lock();
	struct sport* p = find_locked(name);
	t = p ? (p->is_set ? 1u << 19 : (1u << 17) | (1u << 16)) : (name >> 8) < 0x25 ? (1u << 16) : 0;   // nombres bajos = objetos del kernel
	unlock();
	int ktype; int32_t kpid;
	if (!t && kobj_lookup(name, &ktype, &kpid)) {
		// objeto del núcleo (puerto de tarea de otro proceso, fileport...): derecho de envío. El de una tarea que ya terminó
		// es un nombre muerto (MACH_PORT_TYPE_DEAD_NAME), como en XNU al destruirse la tarea: launchd lo mira así para saber
		// si el proceso de un dominio sigue vivo.
		int dead = (ktype == KO_TASK || ktype == KO_TASK_NAME) && kill(kpid, 0) != 0 && errno == ESRCH;
		t = dead ? 1u << 20 : 1u << 16;
	}
	if (!t) return KERN_INVALID_NAME_;
	return safe_write(c->a[2], &t, 4) == 4 ? KERN_OK : KERN_INVALID_VALUE_;
}
// thread_switch(hilo, opción, ms): ceder la CPU. Con OSLOCK_WAIT/WAIT (4/2) se espera el tiempo indicado; si no, solo se cede.
static long t_thread_switch(struct ctx* c) {
	uint32_t opt = (uint32_t)c->a[1], ms = (uint32_t)c->a[2];
	if ((opt == 2 || opt == 4) && ms) { struct timespec ts = { ms / 1000, (long)(ms % 1000) * 1000000L }; nanosleep(&ts, NULL); }
	else sched_yield();
	return 0;
}
static long t_reply_port(struct ctx* c) { (void)c; return port_create(0); }
// thread_get_special_reply_port: cada hilo tiene UN puerto de respuesta especial que se reutiliza mientras exista
static __thread uint32_t special_reply;
static long t_special_reply_port(struct ctx* c) {
	(void)c;
	if (!special_reply || !port_exists(special_reply)) special_reply = port_create(0);
	return special_reply;
}

// mach_generate_activity_id(task, cantidad, *id): identificadores de actividad (os_activity) únicos
static long t_activity_id(struct ctx* c) {
	static _Atomic uint64_t next = 1;
	uint64_t n = c->a[1] ? c->a[1] : 1;
	uint64_t id = atomic_fetch_add(&next, n) | ((uint64_t)getpid() << 40);
	return safe_write(c->a[2], &id, 8) == 8 ? KERN_OK : KERN_INVALID_VALUE_;
}

// ---------------------------------------------------------------- mk_timer: puertos que reciben un mensaje al vencer
// mach_absolute_time de este entorno son nanosegundos de CLOCK_MONOTONIC, así que el plazo se usa tal cual.
struct mktimer { uint32_t name; int tfd; pthread_t th; int live; };
static struct mktimer timers[64];
static pthread_mutex_t timers_lock = PTHREAD_MUTEX_INITIALIZER;
static void* mk_timer_thread(void* p) {
	struct mktimer* t = p;
	for (;;) {
		uint64_t n;
		ssize_t r = read(t->tfd, &n, 8);
		if (r != 8) { if (errno == EINTR) continue; break; }
		uint8_t msg[48] = { 0 };                                   // mk_timer_expire_msg: cabecera + 3 palabras sin uso
		uint32_t bits = 0x11, size = 48, id = 0;
		memcpy(msg, &bits, 4); memcpy(msg + 4, &size, 4); memcpy(msg + 8, &t->name, 4); memcpy(msg + 20, &id, 4);
		port_send(t->name, msg, 48);
	}
	return NULL;
}
static struct mktimer* timer_find(uint32_t name) { for (int i = 0; i < 64; i++) if (timers[i].live && timers[i].name == name) return &timers[i]; return NULL; }
static long t_mk_create(struct ctx* c) {
	(void)c;
	pthread_mutex_lock(&timers_lock);
	struct mktimer* t = NULL;
	for (int i = 0; i < 64; i++) if (!timers[i].live) { t = &timers[i]; break; }
	if (!t) { pthread_mutex_unlock(&timers_lock); return 0; }
	t->name = port_create(0);
	t->tfd = fd_hi(timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC));
	t->live = 1;
	pthread_create(&t->th, NULL, mk_timer_thread, t);
	pthread_detach(t->th);
	pthread_mutex_unlock(&timers_lock);
	return t->name;
}
static long t_mk_arm(struct ctx* c) {              // (name, expire) o (name, flags, expire, leeway)
	struct mktimer* t = timer_find((uint32_t)c->a[0]);
	if (!t) return KERN_INVALID_NAME_;
	uint64_t exp = c->a[1];
	struct itimerspec it = { .it_value = { exp / 1000000000ULL, exp % 1000000000ULL } };
	if (!it.it_value.tv_sec && !it.it_value.tv_nsec) it.it_value.tv_nsec = 1;
	timerfd_settime(t->tfd, TFD_TIMER_ABSTIME, &it, NULL);
	return KERN_OK;
}
static long t_mk_arm_leeway(struct ctx* c) { struct ctx c2 = *c; c2.a[1] = c->a[2]; return t_mk_arm(&c2); }
static long t_mk_cancel(struct ctx* c) {
	struct mktimer* t = timer_find((uint32_t)c->a[0]);
	if (!t) return KERN_INVALID_NAME_;
	struct itimerspec cur, off = { 0 };
	timerfd_settime(t->tfd, 0, &off, &cur);
	uint64_t res = cur.it_value.tv_sec ? (uint64_t)cur.it_value.tv_sec * 1000000000ULL + cur.it_value.tv_nsec : cur.it_value.tv_nsec;
	if (c->a[1]) safe_write(c->a[1], &res, 8);
	return KERN_OK;
}
static long t_mk_destroy(struct ctx* c) {
	struct mktimer* t = timer_find((uint32_t)c->a[0]);
	if (!t) return KERN_INVALID_NAME_;
	t->live = 0;
	close(t->tfd);
	destroy(t->name);
	return KERN_OK;
}

// host_create_mach_voucher_trap(host, recetas*, tamaño, *puerto): los vouchers son nombres de puerto sin contenido
static long t_voucher_create(struct ctx* c) {
	uint32_t n = alloc_port();
	return safe_write(c->a[3], &n, 4) == 4 ? KERN_OK : KERN_INVALID_VALUE_;
}

// mach_port_guard_trap(task, nombre, guarda, estricto) / mach_port_unguard_trap(task, nombre, guarda)
static long t_guard(struct ctx* c) { return port_guard((uint32_t)c->a[1], c->a[2], (int)c->a[3]); }
static long t_unguard(struct ctx* c) { return port_unguard((uint32_t)c->a[1], c->a[2]); }
// mach_port_get_attributes (como mach_port.c de XNU). Sabores: 1 LIMITS_INFO {qlimit}, 2 RECEIVE_STATUS
// (mach_port_status_t), 3 DNREQUESTS_SIZE, 7 INFO_EXT (estado + boost + reservado), 8 GUARD_INFO {guarda u64}.
// *cnt entra con la capacidad (en enteros de 32 bits) y sale con lo escrito; si no cabe, KERN_FAILURE (5).
int port_get_attributes(uint32_t name, int flavor, uint32_t* out, uint32_t* cnt) {
	static const uint32_t need[9] = { 0, 1, 10, 1, 0, 0, 0, 17, 2 };
	if (flavor < 1 || flavor > 8 || !need[flavor]) return 4;                 // KERN_INVALID_ARGUMENT
	if (*cnt < need[flavor]) return 5;
	lock();
	struct sport* p = find_locked(name);
	if (!p) { unlock(); return KERN_INVALID_RIGHT_; }
	memset(out, 0, need[flavor] * 4);
	uint32_t ql = p->qlimit ? p->qlimit : 5;
	if (flavor == 1) out[0] = ql;
	else if (flavor == 3) out[0] = 0;
	else if (flavor == 8) memcpy(out, &p->ctx, 8);
	else {                                                                   // mach_port_status_t
		uint32_t so = 0;
		for (int i = 0; i < MAXSO; i++) if (shm->so[i].port == name) so++;
		out[0] = p->sets[0]; out[1] = 0; out[2] = 0; out[3] = ql; out[4] = p->count; out[5] = so; out[6] = 1;
		out[7] = p->pdreq != 0; out[8] = 0;
		out[9] = (p->guarded ? 0x02 : 0) | (p->strict_guard ? 0x04 : 0);    // MACH_PORT_STATUS_FLAG_GUARDED / STRICT_GUARD
	}
	unlock();
	*cnt = need[flavor];
	return KERN_OK;
}
// mach_port_set_attributes: solo LIMITS_INFO (el límite de cola) tiene efecto que guardar.
int port_set_attributes(uint32_t name, int flavor, const uint32_t* in, uint32_t cnt) {
	lock();
	struct sport* p = find_locked(name);
	if (p && flavor == 1 && cnt >= 1) p->qlimit = in[0] > 2048 ? 2048 : in[0];
	unlock();
	return p ? KERN_OK : KERN_INVALID_RIGHT_;
}
// mach_port_get_attributes_trap(task, nombre, sabor, info*, cuenta*)
static long t_get_attributes(struct ctx* c) {
	uint32_t cnt = 0, out[17];
	if (safe_read(c->a[4], &cnt, 4) != 4) return KERN_INVALID_VALUE_;
	if (cnt > 17) cnt = 17;
	int r = port_get_attributes((uint32_t)c->a[1], (int)c->a[2], out, &cnt);
	if (r) return r;
	if (safe_write(c->a[3], out, cnt * 4) != (ssize_t)(cnt * 4) || safe_write(c->a[4], &cnt, 4) != 4) return KERN_INVALID_VALUE_;
	return KERN_OK;
}
// task_name_for_pid(host, pid, *puerto): puerto de nombre de tarea ficticio por proceso
// Reserva en la zona común un hueco para `len` bytes del archivo `path` (desde `off`) en la tarea `pid`. Devuelve la dirección.
uint64_t rmap_add(int32_t pid, const char* path, uint64_t off, uint64_t len, int prot, uint64_t align) {
	if (align < 4096) align = 4096;
	lock();
	uint64_t a = (RMAP_BASE + shm->rmap_next + align - 1) & ~(align - 1);
	uint64_t r = 0;
	if (a + len <= RMAP_BASE + RMAP_SIZE) {
		for (int i = 0; i < MAXRMAP; i++) if (!shm->rmaps[i].pid) {
			struct srmap* e = &shm->rmaps[i];
			e->pid = pid; e->prot = prot; e->addr = a; e->len = len; e->off = off;
			snprintf(e->path, sizeof e->path, "%s", path);
			shm->rmap_next = a + len - RMAP_BASE;
			r = a;
			break;
		}
	}
	unlock();
	if (!r) logf_("    rmap: sin hueco para 0x%lx bytes\n", len);
	return r;
}
// Fallo de página en la zona común: si otro proceso mapeó ahí algo para esta tarea, se mapea ahora. 1 = resuelto.
int rmap_fault(uint64_t addr) {
	if (addr < RMAP_BASE || addr >= RMAP_BASE + RMAP_SIZE || !shm) return 0;
	struct srmap e = { 0 };
	lock();
	for (int i = 0; i < MAXRMAP; i++) {
		struct srmap* q = &shm->rmaps[i];
		if (q->pid == (int32_t)getpid() && addr >= q->addr && addr < q->addr + q->len) { e = *q; break; }
	}
	unlock();
	if (!e.pid) return 0;
	int fd = open(e.path, O_RDWR | O_CLOEXEC);
	if (fd < 0) return 0;
	void* p = mmap((void*)e.addr, e.len, e.prot & 7, MAP_SHARED | MAP_FIXED, fd, (off_t)e.off);
	close(fd);
	return p != MAP_FAILED;
}
// Nombre (estable por tipo y pid) de un objeto del núcleo; se reutiliza si ya existe.
// Con la tabla llena se reciclan los puertos de tareas e hilos que ya terminaron (el pid o tid ya no existe) y los fileports
// cuyo dueño terminó: el duplicado del descriptor vivía en ese proceso, así que fileport_makefd ya no podía dar nada (EINVAL,
// igual que con el nombre reciclado). Sin esto, los fileports de procesos terminados llenaban la tabla en el macOS completo.
// Los nombres no se reutilizan nunca (next_name solo crece), así que un nombre viejo no puede acabar señalando a otro objeto.
static int kobj_reclaim_locked(void) {
	int freed = 0;
	for (int i = 0; i < MAXKOBJ; i++) {
		struct skobj* k = &shm->kobjs[i];
		if (!k->name || (k->type != KO_TASK && k->type != KO_TASK_NAME && k->type != KO_THREAD && k->type != KO_FILEPORT)) continue;
		char p[48]; snprintf(p, sizeof p, "/proc/%d", (int)k->pid);
		if (access(p, F_OK) != 0) { k->name = 0; freed++; }
	}
	return freed;
}
// Diagnóstico (una vez por proceso) de la tabla llena: cuántos objetos hay de cada tipo y cuántos son de procesos que ya no existen.
static void kobj_dump_locked(void) {
	static int dumped;
	if (dumped) return;
	dumped = 1;
	int n[8] = { 0 }, muertos[8] = { 0 };
	for (int i = 0; i < MAXKOBJ; i++) {
		struct skobj* k = &shm->kobjs[i];
		if (!k->name) continue;
		int t = k->type < 8 ? k->type : 0;
		n[t]++;
		if (kill(k->pid, 0) != 0 && errno == ESRCH) muertos[t]++;
	}
	logf_("    kobj: tabla llena; por tipo (total/de procesos muertos):");
	for (int t = 0; t < 8; t++) if (n[t]) logf_(" %d:%d/%d", t, n[t], muertos[t]);
	logf_("\n");
}
uint32_t kobj_make(int type, int32_t pid) {
	lock();
	uint32_t r = 0;
	for (int i = 0; i < MAXKOBJ && !r; i++) if (shm->kobjs[i].name && shm->kobjs[i].type == type && shm->kobjs[i].pid == pid) r = shm->kobjs[i].name;
	if (!r) {
		r = shm->next_name; shm->next_name += 0x100;
		int put = 0;
		for (int pass = 0; pass < 2 && !put; pass++) {
			if (pass == 1 && !kobj_reclaim_locked()) break;
			for (int i = 0; i < MAXKOBJ; i++) if (!shm->kobjs[i].name) { shm->kobjs[i] = (struct skobj){ r, (uint16_t)type, pid }; put = 1; break; }
		}
		if (!put) { logf_("    kobj: tabla llena\n"); kobj_dump_locked(); }
	}
	unlock();
	return r;
}
// Objeto del núcleo con nombre propio cada vez (p. ej. un fileport por cada fileport_makeport). 0 si la tabla está llena.
uint32_t kobj_new(int type, int32_t pid) {
	lock();
	uint32_t r = 0;
	for (int pass = 0; pass < 2 && !r; pass++) {
		if (pass == 1 && !kobj_reclaim_locked()) break;                     // llena: reciclar los de procesos terminados
		for (int i = 0; i < MAXKOBJ; i++) if (!shm->kobjs[i].name) { r = shm->next_name; shm->next_name += 0x100; shm->kobjs[i] = (struct skobj){ r, (uint16_t)type, pid }; break; }
	}
	if (!r) kobj_dump_locked();
	unlock();
	if (!r) logf_("    kobj: tabla llena\n");
	return r;
}
// Soltar (mach_port_deallocate) el puerto de un hilo de otro proceso libera su objeto. Esos puertos solo los da
// task_threads, a ps y top, que los sueltan tras thread_info, como en XNU, donde el último derecho de envío se lleva el
// puerto. Sin esto se quedaban todos: un top recién arrancado el sistema llenaba la tabla (unos 700 de 1024 eran hilos) y
// launchctl, defaults, dscl o diskutil fallaban después al no poder obtener los suyos.
void kobj_release_thread(uint32_t name) {
	int type; int32_t tid;
	if (name && name != 0x103 && kobj_lookup(name, &type, &tid) && type == KO_THREAD) kobj_free(name);
}
void kobj_free(uint32_t name) {
	lock();
	for (int i = 0; i < MAXKOBJ; i++) if (shm->kobjs[i].name == name) { shm->kobjs[i].name = 0; break; }
	unlock();
}
int kobj_lookup(uint32_t name, int* type, int32_t* pid) {
	if (name == 0x103) { *type = KO_TASK; *pid = (int32_t)getpid(); return 1; }   // mach_task_self
	lock();
	int ok = 0;
	for (int i = 0; i < MAXKOBJ; i++) if (shm->kobjs[i].name == name) { *type = shm->kobjs[i].type; *pid = shm->kobjs[i].pid; ok = 1; break; }
	unlock();
	return ok;
}
// ---------------------------------------------------------------- NOTE_EXEC (EVFILT_PROC)
// Linux no avisa de los exec de otro proceso, pero aquí todo exec de un programa de macOS pasa por exec_common (emu_proc.c):
// el que vigila crea un puerto propio y lo anota con el pid vigilado; el proceso vigilado, justo antes de hacer exec, manda
// un mensaje a cada puerto anotado con su pid. El knote de kqueue espera ese puerto además del pidfd (salida).
uint32_t execwatch_add(int32_t target) {
	uint32_t port = port_create(0);
	if (!port) return 0;
	lock();
	int slot = -1;
	for (int i = 0; i < MAXEXECW && slot < 0; i++) if (!shm->execw[i].port) slot = i;
	for (int i = 0; i < MAXEXECW && slot < 0; i++)                              // reciclar los de vigilantes que ya terminaron
		if (kill(shm->execw[i].watcher, 0) != 0 && errno == ESRCH) slot = i;
	if (slot >= 0) shm->execw[slot] = (struct sexecw){ target, (int32_t)getpid(), port };
	unlock();
	if (slot < 0) {                                                               // el knote sigue valiendo para la salida
		logf_("    NOTE_EXEC: tabla llena\n");
		static int dumped;
		if (!dumped) {                                                            // diagnóstico: quién vigila y si el vigilado vive
			dumped = 1;
			int w[16] = { 0 }, cnt[16] = { 0 }, vivos[16] = { 0 }, nw = 0;
			lock();
			for (int i = 0; i < MAXEXECW; i++) {
				if (!shm->execw[i].port) continue;
				int k; for (k = 0; k < nw && w[k] != shm->execw[i].watcher; k++) {}
				if (k == nw && nw < 16) w[nw++] = shm->execw[i].watcher;
				if (k == 16) continue;
				cnt[k]++;
				int32_t t = shm->execw[i].target;
				if (t < 0 || kill(t, 0) == 0 || errno != ESRCH) vivos[k]++;
			}
			unlock();
			logf_("    NOTE_EXEC: por vigilante (entradas/vigilados vivos):");
			for (int k = 0; k < nw; k++) logf_(" %d:%d/%d", w[k], cnt[k], vivos[k]);
			logf_("\n");
		}
		return port;
	}
	return port;
}
void execwatch_del(uint32_t port) {
	lock();
	for (int i = 0; i < MAXEXECW; i++) if (shm->execw[i].port == port) { shm->execw[i].port = 0; break; }
	struct sport* p = find_locked(port);
	if (p) destroy_locked(p);
	unlock();
	flush_notes();
}
// EVFILT_FS: quien vigila los eventos de VFS (KernelEventAgent) anota un puerto con destino -2; vfs_event_signal manda a cada
// uno un mensaje cuyo id son los bits VQ_* (VQ_MOUNT 0x8, VQ_UNMOUNT 0x10, VQ_UPDATE 0x100), como vfs_event_signal de XNU.
#define VFSWATCH_TARGET (-2)
uint32_t vfswatch_add(void) { return execwatch_add(VFSWATCH_TARGET); }
void vfs_event_signal(uint32_t vq) {
	uint32_t ports[MAXEXECW]; int n = 0;
	lock();
	for (int i = 0; i < MAXEXECW; i++) if (shm->execw[i].port && shm->execw[i].target == VFSWATCH_TARGET) ports[n++] = shm->execw[i].port;
	unlock();
	for (int i = 0; i < n; i++) {
		uint32_t m[6] = { 0x13 /* MACH_MSG_TYPE_COPY_SEND */, 24, ports[i], 0, 0, vq };
		port_send_k(ports[i], (const uint8_t*)m, 24, 1);
	}
}
void execwatch_notify(void) {
	uint32_t ports[MAXEXECW]; int n = 0;
	lock();
	for (int i = 0; i < MAXEXECW; i++) if (shm->execw[i].port && shm->execw[i].target == (int32_t)getpid()) ports[n++] = shm->execw[i].port;
	unlock();
	for (int i = 0; i < n; i++) {
		uint32_t m[6] = { 0x13 /* MACH_MSG_TYPE_COPY_SEND */, 24, ports[i], 0, 0, 0 };
		port_send_k(ports[i], (const uint8_t*)m, 24, 1);
	}
}

// task_name_for_pid(task, pid, *nombre)
static long t_task_name_for_pid(struct ctx* c) {
	int32_t pid = (int32_t)c->a[1];
	if (pid <= 0 || kill(pid, 0) != 0) return 5 /* KERN_FAILURE */;
	uint32_t n = pid == (int32_t)getpid() ? 0x103 : kobj_make(KO_TASK_NAME, pid);
	return safe_write(c->a[2], &n, 4) == 4 ? KERN_OK : KERN_INVALID_VALUE_;
}
// task_for_pid (trampa 45) y task_read_for_pid (syscall 539) (tarea, pid, *puerto): el puerto de la tarea de otro proceso.
// Como en XNU, solo root o el mismo usuario; el propio proceso recibe mach_task_self.
static int task_port_for_pid(int32_t pid, uint64_t out) {
	if (pid <= 0 || kill(pid, 0) != 0) return 5 /* KERN_FAILURE */;
	if (geteuid() != 0) {
		char d[128], p[160], ln[128]; unsigned ru = 0, eu = (unsigned)-1;
		FILE* f = NULL;
		if (proc_dir(pid, d, sizeof d) == 0) { snprintf(p, sizeof p, "%s/status", d); f = fopen(p, "r"); }
		if (f) { while (fgets(ln, sizeof ln, f)) if (!strncmp(ln, "Uid:", 4)) sscanf(ln + 4, "%u %u", &ru, &eu); fclose(f); }
		if (eu != geteuid()) return 5;
	}
	uint32_t n = pid == (int32_t)getpid() ? 0x103 : kobj_make(KO_TASK, pid);
	return safe_write(out, &n, 4) == 4 ? KERN_OK : KERN_INVALID_VALUE_;
}
static long t_task_for_pid(struct ctx* c) { return task_port_for_pid((int32_t)c->a[1], c->a[2]); }
static long bsd_task_read_for_pid(struct ctx* c) { return task_port_for_pid((int32_t)c->a[1], c->a[2]); }
// pid_for_task(tarea, *pid)
static long t_pid_for_task(struct ctx* c) {
	int type; int32_t pid;
	if (!kobj_lookup((uint32_t)c->a[0], &type, &pid) || (type != KO_TASK && type != KO_TASK_NAME)) return 5;
	return safe_write(c->a[1], &pid, 4) == 4 ? KERN_OK : KERN_INVALID_VALUE_;
}

// mach_port_request_notification_trap(task, nombre, variante, sync, aviso, polí, *anterior)
// Se guardan PORT_DESTROYED (69, sobre un derecho de recepción) y DEAD_NAME (72, sobre un derecho de envío). Si el puerto
// ya murió, DEAD_NAME avisa en el acto (como un nombre muerto en XNU). NO_SENDERS (70) se guarda en el puerto y avisa al
// morir el último proceso con derecho de envío (ver «notificaciones»). SEND_POSSIBLE (66): libdispatch la pide cuando un
// envío con plazo 0 da MACH_SEND_TIMED_OUT (cola llena) y reintenta al recibir el aviso; se envía en cuanto se saca un
// mensaje y queda sitio, o en el acto si ya lo hay y la petición es síncrona (sync != 0), como en XNU. Como en XNU, solo
// avisan las peticiones armadas: las síncronas que encontraron la cola llena y las que arma después un envío con
// MACH_SEND_NOTIFY que la encuentra llena. Una petición sin sync y sin armar no avisa nunca (antes avisaba en cada
// mensaje que se sacaba: WindowServer y powerd entraban en un bucle de miles de mensajes por segundo).
int port_request_notification(uint32_t name, int variant, uint32_t sync, uint32_t notify, uint32_t* previous) {
	*previous = 0;
	int r = KERN_OK;
	lock();
	struct sport* p = find_locked(name);
	if (variant == 69) {
		if (!p) r = KERN_INVALID_RIGHT_;
		else { *previous = p->pdreq; p->pdreq = notify; }
	} else if (variant == 70) {                                // NO_SENDERS: sobre un derecho de recepción; sync = make-send count
		if (!p) r = KERN_INVALID_RIGHT_;
		else {
			*previous = p->nsreq; p->nsreq = notify; p->nsmscount = sync;
			if (msg_trace()) {
				int h = 0; for (int j = 0; j < MAXSH; j++) h += shm->sh[j].port == name;
				logf_("    no-senders <%d>: pedido para 0x%x sync=%u aviso=0x%x usuarios anotados=%d dueño=%d\n", (int)getpid(), name, sync, notify, h, (int)p->owner);
			}
		}
	} else if (variant == 66) {
		int ko = name == 0x103;                                   // objetos del núcleo (tarea, hilo, IOKit...): fuera de la
		for (int i = 0; i < MAXKOBJ && !ko; i++) ko = shm->kobjs[i].name == name;   // tabla de puertos pero vivos, y su cola nunca se llena
		if (!p && ko) { if (notify && sync) note_locked(notify, 66, name); }
		else if (!p) {
			logf_("    send-possible <%d>: 0x%x no existe -> DEAD_NAME\n", (int)getpid(), name);
			if (notify && name && name < shm->next_name) note_locked(notify, 72, name); else r = KERN_INVALID_NAME_;
		}
		else {                                                     // una petición por proceso: la nueva sustituye a la anterior
			int32_t me = (int32_t)getpid();
			for (int k = 0; k < MAXSP; k++) if (shm->sp[k].port == name && shm->sp[k].pid == me) { *previous = shm->sp[k].notify; shm->sp[k].port = 0; if (p->spn) p->spn--; break; }
			if (notify && sync && p->count < (p->qlimit ? p->qlimit : 5)) note_locked(notify, 66, name);
			else if (notify) {
				int ok = 0;
				for (int k = 0; k < MAXSP; k++) if (!shm->sp[k].port) { shm->sp[k] = (struct dnreq){ name, notify, me, sync != 0 }; p->spn++; ok = 1; break; }
				if (!ok) logf_("    send-possible: tabla llena\n");
			}
		}
	} else if (variant == 72) {
		int32_t me = (int32_t)getpid();
		for (int i = 0; i < MAXDN; i++) if (shm->dn[i].port == name && shm->dn[i].pid == me) { *previous = shm->dn[i].notify; shm->dn[i].port = 0; break; }
		if (notify) {
			if (!p) { if (name && name < shm->next_name) note_locked(notify, 72, name); else r = KERN_INVALID_NAME_; }
			else {
				int ok = 0;
				for (int i = 0; i < MAXDN; i++) if (!shm->dn[i].port) { shm->dn[i] = (struct dnreq){ name, notify, me, 0 }; ok = 1; break; }
				if (!ok) logf_("    dead-name: tabla llena\n");
			}
		}
	}
	unlock();
	flush_notes();
	return r;
}
static long t_request_notification(struct ctx* c) {
	uint32_t prev = 0;
	long r = port_request_notification((uint32_t)c->a[1], (int)c->a[2], (uint32_t)c->a[3], (uint32_t)c->a[4], &prev);
	uint64_t pa = ctx_arg(c, 6);
	if (pa) safe_write(pa, &prev, 4);
	return r;
}

void emu_port_init(void) {
	reg_mach(42, t_unguard); reg_mach(77, t_request_notification);
	reg_mach(41, t_guard); reg_mach(40, t_get_attributes); reg_mach(44, t_task_name_for_pid); reg_mach(45, t_task_for_pid); reg_mach(46, t_pid_for_task);
	reg_bsd(539, bsd_task_read_for_pid);
	reg_mach(70, t_voucher_create);
	reg_mach(91, t_mk_create); reg_mach(92, t_mk_destroy); reg_mach(93, t_mk_arm); reg_mach(94, t_mk_cancel); reg_mach(95, t_mk_arm_leeway);
	reg_mach(43, t_activity_id);
	reg_mach(16, t_allocate); reg_mach(23, t_extract_member); reg_mach(24, t_construct); reg_mach(25, t_destruct); reg_mach(19, t_mod_refs);
	reg_mach(18, t_deallocate); reg_mach(21, t_nop); reg_mach(22, t_insert_member); reg_mach(20, t_move_member);
	reg_mach(26, t_reply_port); reg_mach(61, t_thread_switch); reg_mach(50, t_special_reply_port); reg_mach(76, t_type);
}
