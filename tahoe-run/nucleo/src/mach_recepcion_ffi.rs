//! Frontera por valor; no recibe punteros ni accede a memoria de C.
#![deny(unsafe_code)]
use crate::mach_recepcion::{self, Cabecera, Emisor, Trailer};

// SAFETY de enlace: símbolo único, firma por valor reflejada en nucleo.h.
#[allow(unsafe_code)] // Sólo el atributo de exportación; cuerpo seguro.
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_recepcion_tam_trailer(options: u64) -> u32 {
    mach_recepcion::tam_trailer(options)
}

// SAFETY de enlace: símbolo único, Cabecera repr(C) reflejada en nucleo.h.
#[allow(unsafe_code)] // Sólo el atributo de exportación; cuerpo seguro.
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_recepcion_cabecera(h: Cabecera) -> Cabecera {
    mach_recepcion::cabecera(h)
}

// SAFETY de enlace: símbolo único, Cabecera repr(C) reflejada en nucleo.h.
#[allow(unsafe_code)] // Sólo el atributo de exportación; cuerpo seguro.
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_recepcion_grande(h: Cabecera, options: u64) -> Cabecera {
    mach_recepcion::cabecera_grande(h, options)
}

// SAFETY de enlace: símbolo único, Emisor/Trailer repr(C) reflejados en nucleo.h.
#[allow(unsafe_code)] // Sólo el atributo de exportación; cuerpo seguro.
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_recepcion_trailer(options: u64, emisor: Emisor, contexto: u64) -> Trailer {
    mach_recepcion::trailer(options, emisor, contexto)
}
