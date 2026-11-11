//! Símbolos de enlace aislados. ABI por valor, sin punteros ni acceso a shm.
#![deny(unsafe_code)]
// SAFETY de enlace: símbolos únicos, ABI y offsets reflejados en nucleo.h.
// La excepción se limita a publicar nombres C; no hay bloques unsafe.
use crate::mach_puerto::{self as puerto, Batch, GuardResult, Sport};

// SAFETY de enlace: símbolo único; Sport se copia por valor con ABI verificado.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_puerto_create(previous: Sport, name: u32, owner: i32, is_set: i32) -> Sport {
    puerto::create(previous, name, owner, is_set)
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_puerto_destroy_allowed(p: Sport, caller: i32) -> i32 {
    puerto::destroy_allowed(p, caller)
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_puerto_in_set(p: Sport, set: u32) -> u32 {
    u32::from(puerto::in_set(p, set))
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_puerto_remove(p: Sport, set: u32) -> Sport {
    puerto::set_remove(p, set)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_puerto_membership(b: Batch, op: u32) -> Batch {
    puerto::membership(b, op)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_puerto_guard(p: Sport, op: u32, old: u64, new: u64, strict: i32) -> GuardResult {
    puerto::guard(p, op, old, new, strict)
}
