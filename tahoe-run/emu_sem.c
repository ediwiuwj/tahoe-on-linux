// Semáforos Mach (semaphore_create y las trampas semaphore_*) y __semwait_signal, sobre pthread.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <time.h>

#include "tahoe.h"

#define KERN_OK 0
#define KERN_BAD_ARG 4
#define KERN_TIMED_OUT 49
#define KERN_ABORTED_ 14
#define KERN_TERMINATED_ 37        // el semáforo se destruyó mientras el hilo esperaba (XNU: THREAD_RESTART)

struct sem { uint32_t name; int count; int destruido; pthread_mutex_t m; pthread_cond_t cv; struct sem* next; };
static struct sem* sems;
static pthread_mutex_t table_lock = PTHREAD_MUTEX_INITIALIZER;

uint32_t sem_create(int value) {
	struct sem* s = calloc(1, sizeof *s);
	pthread_condattr_t ca; pthread_condattr_init(&ca); pthread_condattr_setclock(&ca, CLOCK_MONOTONIC);
	pthread_mutex_init(&s->m, NULL); pthread_cond_init(&s->cv, &ca);
	s->count = value;
	s->name = alloc_port();
	pthread_mutex_lock(&table_lock);
	s->next = sems; sems = s;
	pthread_mutex_unlock(&table_lock);
	return s->name;
}

static struct sem* find(uint32_t name) {
	pthread_mutex_lock(&table_lock);
	struct sem* s = sems;
	while (s && s->name != name) s = s->next;
	pthread_mutex_unlock(&table_lock);
	return s;
}

// semaphore_destroy: el semáforo deja de existir (los que esperaban despiertan). La memoria se conserva por si un hilo
// sigue dentro de wait_sem con el puntero; son pocos bytes por hilo terminado.
// En XNU, semaphore_destroy_internal (osfmk/kern/sync_sema.c:152-196) pone el contador a 0 y despierta a los que
// esperan con THREAD_RESTART, que en semaphore_block (sync_sema.c:617-618) se traduce en KERN_TERMINATED. Aquí se
// marca el semáforo como destruido y los que esperan despiertan con ese mismo error: si no, el cliente creería que
// obtuvo el semáforo y seguiría como si nada.
int machsem_destroy(uint32_t name) {
	pthread_mutex_lock(&table_lock);
	struct sem** pp = &sems;
	while (*pp && (*pp)->name != name) pp = &(*pp)->next;
	struct sem* s = *pp;
	if (s) *pp = s->next;
	pthread_mutex_unlock(&table_lock);
	if (!s) return KERN_BAD_ARG;
	pthread_mutex_lock(&s->m); s->destruido = 1; s->count = 0; pthread_cond_broadcast(&s->cv); pthread_mutex_unlock(&s->m);
	return KERN_OK;
}

static void signal_one(struct sem* s) { pthread_mutex_lock(&s->m); s->count++; pthread_cond_signal(&s->cv); pthread_mutex_unlock(&s->m); }

// Espera con plazo opcional (ns relativos; <0 = sin plazo). 0 = obtenido, 1 = plazo vencido, 2 = el semáforo se
// destruyó mientras se esperaba (KERN_TERMINATED, como en XNU).
static int wait_sem(struct sem* s, long long rel_ns) {
	struct timespec dl;
	if (rel_ns >= 0) { clock_gettime(CLOCK_MONOTONIC, &dl); dl.tv_sec += rel_ns / 1000000000LL; dl.tv_nsec += rel_ns % 1000000000LL; if (dl.tv_nsec >= 1000000000L) { dl.tv_sec++; dl.tv_nsec -= 1000000000L; } }
	int r = 0;
	pthread_mutex_lock(&s->m);
	while (s->count <= 0 && !r && !s->destruido) r = rel_ns >= 0 ? pthread_cond_timedwait(&s->cv, &s->m, &dl) : pthread_cond_wait(&s->cv, &s->m);
	if (s->destruido) r = 2;                              // el semáforo ya no existe: KERN_TERMINATED
	else if (s->count > 0) { s->count--; r = 0; }
	pthread_mutex_unlock(&s->m);
	return r;
}

static long t_signal(struct ctx* c)     { struct sem* s = find((uint32_t)c->a[0]); if (!s) return KERN_BAD_ARG; signal_one(s); return KERN_OK; }
// semaphore_signal_all: en XNU (osfmk/kern/sync_sema.c:370-380) despierta a todos los que esperan y deja el contador
// a CERO ("always reset"). Aquí se sumaban 1000 créditos de más: un semaphore_wait posterior no bloqueaba, cuando en
// macOS sí (y el semáforo de un dispatch se quedaba con créditos de sobra).
static long t_signal_all(struct ctx* c) { struct sem* s = find((uint32_t)c->a[0]); if (!s) return KERN_BAD_ARG; pthread_mutex_lock(&s->m); s->count = 0; pthread_cond_broadcast(&s->cv); pthread_mutex_unlock(&s->m); return KERN_OK; }
// El resultado de wait_sem: 0 = obtenido, 1 = plazo vencido, 2 = el semáforo se destruyó (KERN_TERMINATED).
static long sem_espera(struct sem* s, long long rel_ns) {
	int r = wait_sem(s, rel_ns);
	return r == 2 ? KERN_TERMINATED_ : (r ? KERN_TIMED_OUT : KERN_OK);
}
static long t_wait(struct ctx* c)       { struct sem* s = find((uint32_t)c->a[0]); if (!s) return KERN_BAD_ARG; return sem_espera(s, -1); }
static long t_wait_signal(struct ctx* c) {
	struct sem *w = find((uint32_t)c->a[0]), *g = find((uint32_t)c->a[1]);
	if (!w || !g) return KERN_BAD_ARG;
	signal_one(g); return sem_espera(w, -1);
}
static long t_timedwait(struct ctx* c) {
	struct sem* s = find((uint32_t)c->a[0]); if (!s) return KERN_BAD_ARG;
	return sem_espera(s, (long long)(uint32_t)c->a[1] * 1000000000LL + (int32_t)c->a[2]);
}
static long t_timedwait_signal(struct ctx* c) {
	struct sem *w = find((uint32_t)c->a[0]), *g = find((uint32_t)c->a[1]);
	if (!w || !g) return KERN_BAD_ARG;
	signal_one(g);
	return sem_espera(w, (long long)(uint32_t)c->a[2] * 1000000000LL + (int32_t)c->a[3]);
}

// __semwait_signal(cond_sem, mutex_sem, timeout, relative, tv_sec, tv_nsec): usado por pthread_cond/join.
static long bsd_semwait_signal(struct ctx* c) {
	struct sem* w = find((uint32_t)c->a[0]);
	if (!w) return -D_EINVAL;
	if (c->a[1]) { struct sem* g = find((uint32_t)c->a[1]); if (g) signal_one(g); }
	long long ns = -1;
	if (c->a[2]) {
		long long sec = (long long)c->a[4], nsec = (long long)(int)ctx_arg(c, 5);
		if (c->a[3]) ns = sec * 1000000000LL + nsec;      // relativo
		else { struct timespec now; clock_gettime(CLOCK_REALTIME, &now); ns = (sec - now.tv_sec) * 1000000000LL + (nsec - now.tv_nsec); if (ns < 0) ns = 0; }
	}
	// wait_sem: 0 = obtenido, 1 = plazo vencido, 2 = el semáforo se destruyó. Aquí la API es POSIX (errno), no
	// kern_return: un semáforo destruido no es un timeout, así que se distingue (ECANCELED).
	int r = wait_sem(w, ns);
	return r == 2 ? -89 /* ECANCELED (Darwin) */ : (r ? -60 /* ETIMEDOUT */ : 0);
}


// ---- semáforos POSIX con nombre: los de Linux (glibc, /dev/shm/sem.*), compartidos entre procesos como en macOS.
// sem_open de Darwin devuelve un identificador que libsystem trata como sem_t*; aquí es un índice de esta tabla + 1.
#include <semaphore.h>
#include <fcntl.h>
#include <stdio.h>
#define PSEM_MAX 256
static sem_t* psem_tab[PSEM_MAX];
static pthread_mutex_t psem_lock = PTHREAD_MUTEX_INITIALIZER;
static int psem_name(uint64_t gname, char* out, size_t cap) {
	char n[256];
	if (safe_string(gname, n, sizeof n) != 0) return -1;
	const char* s = n[0] == '/' ? n + 1 : n;
	snprintf(out, cap, "/tahoe-%s", s);
	for (char* q = out + 1; *q; q++) if (*q == '/') *q = '_';
	return 0;
}
static sem_t* psem_get(uint64_t id) { return (id >= 1 && id <= PSEM_MAX) ? psem_tab[id - 1] : NULL; }
static long bsd_sem_open(struct ctx* c) {
	char nm[300];
	if (psem_name(c->a[0], nm, sizeof nm) != 0) return -14;
	int df = (int)c->a[1], lf = 0;
	if (df & 0x200) lf |= O_CREAT;
	if (df & 0x800) lf |= O_EXCL;
	sem_t* s = (lf & O_CREAT) ? sem_open(nm, lf, (mode_t)c->a[2], (unsigned)c->a[3]) : sem_open(nm, lf);
	if (s == SEM_FAILED) return -darwin_errno(errno);
	pthread_mutex_lock(&psem_lock);
	long id = -24;                                       // EMFILE
	for (int i = 0; i < PSEM_MAX; i++) if (!psem_tab[i]) { psem_tab[i] = s; id = i + 1; break; }
	pthread_mutex_unlock(&psem_lock);
	if (id < 0) sem_close(s);
	return id;
}
static long bsd_sem_close(struct ctx* c) {
	pthread_mutex_lock(&psem_lock);
	sem_t* s = psem_get(c->a[0]);
	if (s) psem_tab[c->a[0] - 1] = NULL;
	pthread_mutex_unlock(&psem_lock);
	if (!s) return -22;
	sem_close(s);
	return 0;
}
static long bsd_sem_unlink(struct ctx* c) {
	char nm[300];
	if (psem_name(c->a[0], nm, sizeof nm) != 0) return -14;
	return sem_unlink(nm) == 0 ? 0 : -darwin_errno(errno);
}
static long bsd_sem_wait(struct ctx* c) {
	sem_t* s = psem_get(c->a[0]);
	if (!s) return -22;
	while (sem_wait(s) != 0) { if (errno != EINTR) return -darwin_errno(errno); return -4; }
	return 0;
}
static long bsd_sem_trywait(struct ctx* c) {
	sem_t* s = psem_get(c->a[0]);
	if (!s) return -22;
	return sem_trywait(s) == 0 ? 0 : -darwin_errno(errno);   // EAGAIN -> 35 en Darwin
}
static long bsd_sem_post(struct ctx* c) {
	sem_t* s = psem_get(c->a[0]);
	if (!s) return -22;
	return sem_post(s) == 0 ? 0 : -darwin_errno(errno);
}

void emu_sem_init(void) {
	reg_mach(33, t_signal); reg_mach(34, t_signal_all); reg_mach(36, t_wait); reg_mach(37, t_wait_signal);
	reg_mach(38, t_timedwait); reg_mach(39, t_timedwait_signal);
	reg_bsd(334, bsd_semwait_signal); reg_bsd(423, bsd_semwait_signal);
	reg_bsd(268, bsd_sem_open); reg_bsd(269, bsd_sem_close); reg_bsd(270, bsd_sem_unlink);
	reg_bsd(271, bsd_sem_wait); reg_bsd(272, bsd_sem_trywait); reg_bsd(273, bsd_sem_post);
}
