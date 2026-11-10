//! ABI por valor, sin referencias a shm ni bloques unsafe.
#![deny(unsafe_code)]
// SAFETY de enlace: símbolos únicos y ABI por valor reflejado en nucleo.h.
// Excepción limitada al atributo de exportación, no a lógica ni punteros.
use crate::{mach_cola as q, mach_puerto::Sport};
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_cola_qlimit(value: u32) -> u32 { q::qlimit(value) }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_cola_send(p: Sport, bits: u32, kernel: u32, timeout: i32, expired: u32, waited: u32) -> i32 { q::send(p,bits,kernel,timeout,expired,waited) }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_cola_transition(p: Sport, free: u32, slot: u32, next: u32, op: u32) -> q::Plan { q::transition(p,free,slot,next,op) }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_cola_count(count: u32, enqueue: u32) -> u32 { q::set_count(count,enqueue) }
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_cola_receive(p: Sport, total: u32, limit: u32, large: u32) -> i32 { q::receive(p,total,limit,large) }
