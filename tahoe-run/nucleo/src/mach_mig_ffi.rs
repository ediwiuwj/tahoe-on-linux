//! Exportación de codecs MIG; la lógica segura está en mach_mig.
#![deny(unsafe_code)]
use crate::mach_mig::{self,Respuesta};
// SAFETY de enlace: nombres únicos y ABI por valor reflejado en nucleo.h.
// No se reciben ni desreferencian punteros de C en estas funciones.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_mig_error(id:i32,codigo:i32)->Respuesta {mach_mig::error(id,codigo)}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_mig_puerto(id:i32,nombre:u32,disposicion:u8)->Respuesta {mach_mig::puerto(id,nombre,disposicion)}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_mig_ool(id:i32,dir:u64,len:u32,cnt:u32,ports:u32,extra:u32,has_extra:u32)->Respuesta {
    mach_mig::ool(id,dir,len,cnt,ports,extra,has_extra)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn nucleo_mig_rx_disp(tx:u32)->u32 {mach_mig::disposicion_recibida(tx)}
