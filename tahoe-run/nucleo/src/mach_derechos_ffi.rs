//! ABI por valor. Única excepción unsafe: exportar símbolos C sin mangling;
//! no hay punteros, bloques unsafe, ni referencias a memoria compartida.
#![deny(unsafe_code)]
use crate::mach_derechos::{self, Entry, Result};
use crate::mach_puerto::Sport;
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_derechos_reference(e: Entry, p: Sport, port: u32, holder: i32, delta: i32, base: i32, kind: u32, total: i64) -> Result {
    mach_derechos::reference(e, p, port, holder, delta, base, kind, total)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_derechos_once(e: Entry, port: u32, holder: i32, op: u32) -> Result {
    mach_derechos::once(e, port, holder, op)
}
