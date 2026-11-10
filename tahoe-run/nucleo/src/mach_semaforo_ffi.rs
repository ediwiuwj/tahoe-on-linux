//! ABI por valor: no desreferencia punteros.
#![deny(unsafe_code)]
// SAFETY de enlace: símbolos únicos y ABI por valor declarado en nucleo.h.
// Se permite sólo la exportación; los cuerpos no contienen bloques unsafe.
use crate::mach_semaforo::{Estado, Transicion};
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_sem_new(value: i32) -> Estado { Estado::new(value) }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_sem_signal(state: Estado, all: u32) -> Transicion { state.signal(all != 0) }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_sem_destroy(state: Estado) -> Transicion { state.destroy() }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_sem_poll(state: Estado, generation: u64, error: i32) -> Transicion { state.poll(generation, error) }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_sem_relative_ns(sec: u32, nsec: u32) -> i64 { crate::mach_semaforo::relative_ns(sec, nsec) }
