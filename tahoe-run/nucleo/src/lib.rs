//! Núcleo de tahoe-run en Rust: las partes del «kernel» emulado que más fallaban en C se reescriben aquí, una a una
//! (ver README, «criterio de lenguaje»). Se enlaza en tahoe-run como biblioteca estática y se llama desde C con
//! funciones `extern "C"` cuyo prototipo está en `nucleo.h`.
//!
//! Todo esto se ejecuta dentro del manejador de SIGSYS (y en procesos de macOS), así que la biblioteca no usa la
//! biblioteca estándar ni memoria dinámica: trabaja sobre los búferes que le pasa el C y comprueba siempre los límites.
#![cfg_attr(not(test), no_std)]

pub mod candado;
pub mod desborde;
pub mod mensaje;
pub mod rwlock;

/// Un pánico en el núcleo es un error de programación: se anota y se aborta el proceso, como haría un fallo en C, pero
/// sin corromper memoria antes.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    extern "C" {
        fn abort() -> !;
    }
    // SAFETY: abort de la libc no vuelve y no necesita nada.
    unsafe { abort() }
}

/// La biblioteca `core` precompilada hace referencia a la rutina de desenrollado de pila aunque aquí un pánico aborta
/// (panic = "abort"): nunca se llama, pero el enlazador necesita el símbolo.
#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn rust_eh_personality() {}
