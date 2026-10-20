// psynch: las esperas de libpthread para mutex "fairshare" y variables de condición que no usan ulock.
//
// En XNU el núcleo guarda, por dirección de objeto, quién espera y "entrega" el mutex al siguiente (handoff). libpthread
// lleva los contadores de secuencia en memoria del proceso; al núcleo solo le pide dormir (mutexwait / cvwait) y
// despertar (mutexdrop / cvsignal / cvbroad). Aquí basta con lo mismo: por cada dirección, un contador de entregas
// pendientes y otro de señales, con una variable de condición del anfitrión. Una entrega que llega antes que la espera
// se conserva, así que no se pierden despertares.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <time.h>

#include "tahoe.h"
#include "nucleo.h"

#define PS_SLOTS 4096
struct ps_obj { uint64_t addr; int waiters, grants; uint64_t gen; int signals; pthread_cond_t cv; };
static struct ps_obj ps_tab[PS_SLOTS];
static pthread_mutex_t ps_lock = PTHREAD_MUTEX_INITIALIZER;

// Busca (o crea) el objeto de una dirección. Se llama con ps_lock tomado.
static struct ps_obj* ps_get(uint64_t addr) {
	uint32_t h = (uint32_t)((addr >> 3) * 2654435761u) % PS_SLOTS;
	for (uint32_t i = 0; i < PS_SLOTS; i++) {
		struct ps_obj* o = &ps_tab[(h + i) % PS_SLOTS];
		if (o->addr == addr) return o;
		if (!o->addr) {
			o->addr = addr;
			pthread_condattr_t ca; pthread_condattr_init(&ca); pthread_condattr_setclock(&ca, CLOCK_MONOTONIC);
			pthread_cond_init(&o->cv, &ca);
			return o;
		}
	}
	return NULL;
}

static void ps_grant_locked(uint64_t mutex) {
	struct ps_obj* o = ps_get(mutex);
	if (!o) return;
	o->grants++;
	pthread_cond_broadcast(&o->cv);
}

// Espera hasta que haya una entrega (mutex) o una señal/generación nueva (cv). plazo_ns = 0: sin plazo.
static int ps_wait_locked(struct ps_obj* o, int (*ready)(struct ps_obj*, uint64_t), uint64_t gen0, uint64_t plazo_ns) {
	struct timespec dl;
	if (plazo_ns) { clock_gettime(CLOCK_MONOTONIC, &dl); dl.tv_sec += (time_t)(plazo_ns / 1000000000ULL); dl.tv_nsec += (long)(plazo_ns % 1000000000ULL); if (dl.tv_nsec >= 1000000000L) { dl.tv_sec++; dl.tv_nsec -= 1000000000L; } }
	while (!ready(o, gen0)) {
		int r = plazo_ns ? pthread_cond_timedwait(&o->cv, &ps_lock, &dl) : pthread_cond_wait(&o->cv, &ps_lock);
		if (r == ETIMEDOUT) return ETIMEDOUT;
	}
	return 0;
}
static int mutex_ready(struct ps_obj* o, uint64_t gen0) { (void)gen0; return o->grants > 0; }
static int cv_ready(struct ps_obj* o, uint64_t gen0) { return o->signals > 0 || o->gen != gen0; }

// psynch_mutexwait(mutex, mgen, ugen, tid, flags) -> secuencia actualizada
static long bsd_psynch_mutexwait(struct ctx* c) {
	pthread_mutex_lock(&ps_lock);
	struct ps_obj* o = ps_get(c->a[0]);
	if (!o) { pthread_mutex_unlock(&ps_lock); return -D_ENOMEM; }
	o->waiters++;
	ps_wait_locked(o, mutex_ready, 0, 0);
	o->grants--; o->waiters--;
	pthread_mutex_unlock(&ps_lock);
	return (long)(uint32_t)c->a[1];
}
// psynch_mutexdrop(mutex, mgen, ugen, tid, flags): el mutex pasa al siguiente que espere (o al próximo que llegue)
static long bsd_psynch_mutexdrop(struct ctx* c) {
	pthread_mutex_lock(&ps_lock);
	ps_grant_locked(c->a[0]);
	pthread_mutex_unlock(&ps_lock);
	return 0;
}
// psynch_cvwait(cv, cvlsgen, cvugen, mutex, mugen, flags, sec, nsec): suelta el mutex (si se indica) y espera una señal
static long bsd_psynch_cvwait(struct ctx* c) {
	uint64_t cv = c->a[0], mutex = c->a[3];
	int64_t sec = (int64_t)ctx_arg(c, 6); uint32_t nsec = (uint32_t)ctx_arg(c, 7);
	uint64_t plazo = (sec > 0 || nsec) ? (uint64_t)(sec > 0 ? sec : 0) * 1000000000ULL + nsec : 0;
	pthread_mutex_lock(&ps_lock);
	if (mutex) ps_grant_locked(mutex);
	struct ps_obj* o = ps_get(cv);
	if (!o) { pthread_mutex_unlock(&ps_lock); return -D_ENOMEM; }
	o->waiters++;
	int r = ps_wait_locked(o, cv_ready, o->gen, plazo);
	if (r == 0 && o->signals > 0) o->signals--;
	o->waiters--;
	if (!o->waiters) o->signals = 0;                  // señales sin nadie esperando no se acumulan (como en el núcleo)
	pthread_mutex_unlock(&ps_lock);
	return r == ETIMEDOUT ? -60 /* ETIMEDOUT de Darwin */ : 0;
}
// psynch_cvsignal(cv, cvlsgen, cvugen, thread_port, mutex, mugen, tid, flags): despierta a uno
static long bsd_psynch_cvsignal(struct ctx* c) {
	pthread_mutex_lock(&ps_lock);
	struct ps_obj* o = ps_get(c->a[0]);
	if (o && o->waiters > o->signals) { o->signals++; pthread_cond_broadcast(&o->cv); }
	pthread_mutex_unlock(&ps_lock);
	return 0;
}
// psynch_cvbroad(cv, cvlsgen, cvudgen, flags, mutex, mugen, tid): despierta a todos los que esperan ahora
static long bsd_psynch_cvbroad(struct ctx* c) {
	pthread_mutex_lock(&ps_lock);
	struct ps_obj* o = ps_get(c->a[0]);
	if (o) { o->gen++; pthread_cond_broadcast(&o->cv); }
	pthread_mutex_unlock(&ps_lock);
	return 0;
}
// ---- pthread_rwlock (306-308): las decisiones las toma nucleo/src/rwlock.rs; aquí solo se duerme y se despierta ----
struct rw_espera { int listo; uint32_t valor; pthread_cond_t cv; };
static uint64_t rw_ahora(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return (uint64_t)t.tv_sec; }
static int rw_despertar(void* ctx, uint64_t espera, uint32_t valor) {
	(void)ctx;
	struct rw_espera* e = (struct rw_espera*)(uintptr_t)espera;
	e->valor = valor; e->listo = 1;
	pthread_cond_signal(&e->cv);
	return 1;
}
static void rw_aviso(const char* que, uint64_t addr, uint32_t l, uint32_t u, uint32_t s, uint32_t flags, int r) {
	logf_("    psynch: <%d> %s 0x%lx L=0x%x U=0x%x S=0x%x flags=0x%x -> error %d\n", (int)getpid(), que, (unsigned long)addr, l, u, s, flags, r);
}
// Una línea en el registro la primera vez que un proceso entra aquí (para saber que los rwlock se usan). Con ps_lock.
static void rw_primera_vez(void) {
	static int visto;
	if (!visto) { visto = 1; logf_("    psynch: <%d> primer rwlock con contención (psynch_rw_*)\n", (int)getpid()); }
}
// psynch_rw_rdlock / psynch_rw_wrlock(rwlock, lgenval, ugenval, rw_wc, flags) -> bits para libpthread
static long rw_lock(struct ctx* c, uint32_t tipo) {
	uint64_t addr = c->a[0];
	uint32_t l = (uint32_t)c->a[1], u = (uint32_t)c->a[2], s = (uint32_t)c->a[3], flags = (uint32_t)c->a[4];
	if (flags & 0x10) rw_aviso("rwlock compartido entre procesos (se trata como privado)", addr, l, u, s, flags, 0);
	struct rw_espera e = { 0 };
	pthread_cond_init(&e.cv, NULL);
	uint32_t valor = 0, ranura = 0;
	pthread_mutex_lock(&ps_lock);
	rw_primera_vez();
	int r = nucleo_rw_lock(tipo, addr, l, u, s, flags, (uint64_t)(uintptr_t)&e, rw_ahora(), rw_despertar, NULL, &valor, &ranura);
	if (r == 1) {
		while (!e.listo) pthread_cond_wait(&e.cv, &ps_lock);
		valor = e.valor;
		nucleo_rw_fin_espera(ranura, rw_ahora());
		r = 0;
	}
	pthread_mutex_unlock(&ps_lock);
	pthread_cond_destroy(&e.cv);
	if (r) { rw_aviso(tipo == NUCLEO_RW_LECTURA ? "rw_rdlock" : "rw_wrlock", addr, l, u, s, flags, r); return -r; }
	return (long)valor;
}
static long bsd_psynch_rw_rdlock(struct ctx* c) { return rw_lock(c, NUCLEO_RW_LECTURA); }
static long bsd_psynch_rw_wrlock(struct ctx* c) { return rw_lock(c, NUCLEO_RW_ESCRITURA); }
// psynch_rw_unlock(rwlock, lgenval, ugenval, rw_wc, flags): reparte el cerrojo a los que esperan
static long bsd_psynch_rw_unlock(struct ctx* c) {
	uint64_t addr = c->a[0];
	uint32_t l = (uint32_t)c->a[1], u = (uint32_t)c->a[2], s = (uint32_t)c->a[3], flags = (uint32_t)c->a[4];
	uint32_t valor = 0;
	pthread_mutex_lock(&ps_lock);
	rw_primera_vez();
	int r = nucleo_rw_unlock(addr, l, u, s, flags, rw_ahora(), rw_despertar, NULL, &valor);
	pthread_mutex_unlock(&ps_lock);
	if (r) { rw_aviso("rw_unlock", addr, l, u, s, flags, r); return -r; }
	return (long)valor;
}
// psynch_rw_longrdlock / psynch_rw_yieldwrlock: XNU tampoco los tiene (ESRCH)
static long bsd_psynch_esrch(struct ctx* c) { (void)c; return -D_ESRCH; }

// psynch_cvclrprepost: sin estado previo que limpiar
static long bsd_psynch_ret0(struct ctx* c) { (void)c; return 0; }

void emu_psynch_init(void) {
	reg_bsd(301, bsd_psynch_mutexwait); reg_bsd(302, bsd_psynch_mutexdrop);
	reg_bsd(303, bsd_psynch_cvbroad); reg_bsd(304, bsd_psynch_cvsignal); reg_bsd(305, bsd_psynch_cvwait);
	reg_bsd(312, bsd_psynch_ret0);
	reg_bsd(297, bsd_psynch_esrch); reg_bsd(298, bsd_psynch_esrch);
	reg_bsd(306, bsd_psynch_rw_rdlock); reg_bsd(307, bsd_psynch_rw_wrlock); reg_bsd(308, bsd_psynch_rw_unlock);
}

// ---- fork: candados de este módulo
// Se cogen antes del fork y se sueltan después, para que el hijo no herede ninguno cogido (ver tahoe.h). El orden es el
// global: si el código normal puede tener uno cogido y pedir otro, el primero tiene que ir antes.
// Este módulo solo tiene ps_lock: es un mutex normal (PTHREAD_MUTEX_INITIALIZER, ni recursive ni robust). Los ps_obj
// no tienen mutex propio (su pthread_cond_t se espera siempre con ps_lock), así que no hay nada dinámico que coger aquí.
void emu_psynch_fork_lock(void) {
	pthread_mutex_lock(&ps_lock);
}
void emu_psynch_fork_unlock(void) {
	pthread_mutex_unlock(&ps_lock);
}
