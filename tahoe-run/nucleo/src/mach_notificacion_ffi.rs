//! Frontera exclusivamente por valor: sin bloques unsafe ni referencias a shm.
#![deny(unsafe_code)]
// SAFETY de enlace: símbolos únicos, ABI por valor reflejado en nucleo.h.
// La excepción se limita al atributo de exportación.
use crate::mach_notificacion::{self as n, Entry, Plan, Note, Message};
// SAFETY de enlace: símbolo único y retorno por valor con layout verificado en C.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_notificacion_message(note: Note) -> Message { n::message(note) }
use crate::mach_puerto::Sport;
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_notificacion_enqueue(dest: u32, len: u32, cap: u32) -> u32 {
    n::enqueue(dest, len, cap)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_notificacion_request(p: Sport, e: Entry, name: u32, variant: i32,
    sync: u32, notify: u32, pid: i32, next: u32, kernel: u32, space: u32) -> Plan {
    n::request(p, e, name, variant, sync, notify, pid, next, kernel, space)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_notificacion_event(p: Sport, e: Entry, name: u32, pid: i32,
    kind: u32, held: u32) -> Plan { n::event(p, e, name, pid, kind, held) }
