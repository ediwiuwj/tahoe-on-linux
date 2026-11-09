//! Exportaciones únicas del parser de spawn; argumentos propios por valor.
#![deny(unsafe_code)]
use crate::spawn::{self, BytesAccion, Resultado};
// SAFETY: símbolos únicos, declarados y comprobados por tamaño en nucleo.h.
// No hay punteros ni acceso a memoria C en estas entradas.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_spawn_cabecera(cap: i32, n: i32, tam: u64, limite: u32) -> u64 {
    spawn::cabecera(cap,n,tam,limite)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_spawn_accion(bytes: BytesAccion) -> Resultado { spawn::leer(bytes) }
