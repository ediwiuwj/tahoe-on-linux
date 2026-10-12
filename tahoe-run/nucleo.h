// Interfaz con el núcleo en Rust (nucleo/, biblioteca estática). Los tipos son espejo de los de Rust (#[repr(C)]).
#pragma once
#include <stddef.h>
#include <stdint.h>

// Un descriptor de mensaje Mach ya leído (nucleo/src/mensaje.rs, struct Desc).
struct nucleo_desc {
	uint32_t off;      // desplazamiento en el mensaje
	uint8_t clase;     // 0 puerto, 1 memoria, 2 puertos, 3 memoria volátil, 4 puerto con guarda
	uint8_t disp;      // disposición del derecho
	uint16_t r;
	uint32_t nombre;   // puerto (clases 0 y 4)
	uint32_t bytes;    // datos fuera de línea (clases 1-3; puertos: número × 4)
	uint64_t dir;      // dirección de los datos fuera de línea, o la guarda (clase 4)
};
_Static_assert(sizeof(struct nucleo_desc) == 24 && offsetof(struct nucleo_desc, nombre) == 8 && offsetof(struct nucleo_desc, dir) == 16, "nucleo_desc");
enum { NUCLEO_PUERTO = 0, NUCLEO_MEMORIA = 1, NUCLEO_PUERTOS = 2, NUCLEO_MEMORIA_VOLATIL = 3, NUCLEO_PUERTO_GUARDA = 4 };
static inline int nucleo_fuera_de_linea(uint8_t clase) { return clase >= NUCLEO_MEMORIA && clase <= NUCLEO_MEMORIA_VOLATIL; }

// Descriptores del mensaje m (tam bytes): rellena out (hasta max) y devuelve cuántos hay en total, o -1 si el mensaje está
// mal formado (descriptor fuera del mensaje o de tipo desconocido).
int nucleo_descriptores(const uint8_t* m, uint32_t tam, struct nucleo_desc* out, uint32_t max);

// Desbordamiento de mensajes grandes (nucleo/src/desborde.rs): los datos fuera de línea que no caben en una ranura van a
// /dev/shm/tahoe.<etiqueta>.ool.<id>. guardar/leer devuelven 0 si todo bien; leer borra el archivo.
int nucleo_desborde_guardar(const char* etiqueta, uint64_t id, const uint8_t* datos, uint64_t len);
int nucleo_desborde_leer(const char* etiqueta, uint64_t id, uint8_t* dst, uint64_t len);
void nucleo_desborde_borrar(const char* etiqueta, uint64_t id);

// pthread_rwlock en el núcleo (nucleo/src/rwlock.rs, traducción de kern_synch.c). Se llama con el candado de
// emu_psynch.c tomado. despertar(ctx, espera, valor) despierta a un hilo dormido (devuelve 1 si esperaba).
enum { NUCLEO_RW_LECTURA = 0x01, NUCLEO_RW_ESCRITURA = 0x04 };
typedef int (*nucleo_rw_despertar)(void* ctx, uint64_t espera, uint32_t valor);
// 0: concedido (*valor); 1: a dormir hasta que despertar llame con «espera», y luego nucleo_rw_fin_espera(*ranura);
// otro: errno.
int nucleo_rw_lock(uint32_t tipo, uint64_t addr, uint32_t l, uint32_t u, uint32_t s, uint32_t flags, uint64_t espera,
	uint64_t ahora, nucleo_rw_despertar despertar, void* ctx, uint32_t* valor, uint32_t* ranura);
void nucleo_rw_fin_espera(uint32_t ranura, uint64_t ahora);
// 0 y *valor, o errno.
int nucleo_rw_unlock(uint64_t addr, uint32_t l, uint32_t u, uint32_t s, uint32_t flags, uint64_t ahora,
	nucleo_rw_despertar despertar, void* ctx, uint32_t* valor);

// Candado y esperas de la región de puertos, entre procesos (nucleo/src/candado.rs). El candado es una palabra a cero
// (libre); tomar devuelve 0 o EOWNERDEAD (el dueño murió con él tomado). esperar: con el candado tomado, lo suelta,
// duerme hasta un aviso en *e (o el plazo absoluto hasta, CLOCK_MONOTONIC; NULL = sin plazo) y lo vuelve a tomar;
// devuelve 0 (también en despertares espurios: hay que volver a mirar), ETIMEDOUT o EOWNERDEAD.
struct timespec;
int nucleo_candado_tomar(uint32_t* c);
void nucleo_candado_soltar(uint32_t* c);
int nucleo_esperar(uint32_t* e, uint32_t* c, const struct timespec* hasta);
void nucleo_avisar(uint32_t* e);
