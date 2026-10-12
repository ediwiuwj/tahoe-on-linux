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
