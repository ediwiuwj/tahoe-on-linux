//! Frontera de símbolos C; toda la lógica vive en módulos de Rust seguro.
#![deny(unsafe_code)]
use crate::entrega_senal::{self as core, Maquina, MarcoBytes, Vuelta, PrefijoFp};
use crate::marco_senal::Ucontext64;

// SAFETY de enlace: estos símbolos tienen una única definición en nucleo.
// Sus declaraciones C están en nucleo.h, con comprobaciones de tamaños ABI.
// Todos los argumentos y resultados son valores propios, sin punteros C.
// La excepción al lint permite exclusivamente publicar estas entradas ABI.
macro_rules! exportar {
    ($nombre:ident($($arg:ident: $tipo:ty),*) -> $ret:ty) => {
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        pub extern "C" fn $nombre($($arg: $tipo),*) -> $ret {
            core::$nombre($($arg),*)
        }
    };
}
exportar!(nucleo_senal_codificar(contexto: Ucontext64, maquina: Maquina) -> MarcoBytes);
exportar!(nucleo_senal_decodificar(bytes: MarcoBytes, guardada: Maquina, mc: u64) -> Vuelta);
exportar!(nucleo_senal_fp_inicial(fp: PrefijoFp, bv: u64) -> PrefijoFp);
exportar!(nucleo_senal_xstate_largo(fp: PrefijoFp) -> u32);
exportar!(nucleo_senal_pila(rsp: u64, tam: u64) -> u64);
exportar!(nucleo_senal_elegible(pendientes: u32, mascara: u32, permitidas: u32) -> u32);
exportar!(nucleo_senal_mascara(old: u32, set: u32, how: u64, has_set: u32) -> u64);
