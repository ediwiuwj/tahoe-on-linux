//! Métodos del protocolo MTLDevice que el driver no implementa de verdad: un stub con la firma correcta devuelve nil/0
//! y lo apunta. Sin esto, un cliente que llame a uno de ellos ejecuta la implementación abstracta de `_MTLDevice`, que
//! lanza una excepción de Objective-C y mata el proceso (Rust no puede capturarla).
//!
//! La lista la genera `metalsuite` (enumera el protocolo MTLDevice del runtime de macOS y compara la implementación del
//! driver con la de su clase madre).

#![allow(non_snake_case)]

use crate::objc::{self, Class, Imp, Sel};
use crate::Id;
use std::ffi::c_void;

/// (selector, codificación de tipos) de los métodos del protocolo sin implementación real.
pub const STUBS: &[(&str, &str)] = &include!("stubs_tabla.rs");

/// Tipo de retorno de una codificación de Objective-C (lo de delante de la primera `@` de nivel superior, sin el
/// desplazamiento de pila que le sigue).
fn tipo_ret(t: &str) -> &str {
    if t.starts_with('@') {
        return "@";
    }
    let b = t.as_bytes();
    let (mut i, mut prof) = (0usize, 0i32);
    while i < b.len() {
        let c = b[i] as char;
        if c == '{' || c == '(' {
            prof += 1;
        } else if c == '}' || c == ')' {
            prof -= 1;
        } else if c == '@' && prof == 0 {
            break;
        }
        i += 1;
    }
    let mut r = &t[..i];
    while r.ends_with(|c: char| c.is_ascii_digit()) {
        r = &r[..r.len() - 1];
    }
    r
}

extern "C" fn nulo(this: Id, cmd: Sel) -> Id {
    crate::registro(&format!("sin implementar (protocolo): -[{} {}] -> nil", crate::objc::nombre_clase(objc::clase_de(this)), objc::nombre_sel(cmd)));
    std::ptr::null_mut()
}

extern "C" fn cero(_this: Id, _cmd: Sel) -> u64 {
    0
}

extern "C" fn cero32(_this: Id, _cmd: Sel) -> u32 {
    0
}

extern "C" fn falso(_this: Id, _cmd: Sel) -> i8 {
    0
}

extern "C" fn nada(_this: Id, _cmd: Sel) {}

/// Estructura de dos enteros de 64 bits (MTLSize, MTLDrawableSize...): vuelve en rax:rdx.
extern "C" fn ceros2(_this: Id, _cmd: Sel) -> [u64; 2] {
    [0, 0]
}

/// Estructura de tres enteros de 64 bits: vuelve por puntero oculto (`objc_msgSend_stret`).
extern "C" fn ceros3(salida: *mut [u64; 3], _this: Id, _cmd: Sel) {
    // SAFETY: el llamador reserva el hueco de la estructura de retorno.
    unsafe { *salida = [0; 3] }
}

/// Registra los stubs que aún no tenga la clase (los reales se registran antes y ganan). Deja en
/// /private/var/tmp/tahoe-metal-stubs.txt la lista de los que quedaron como stub: es la lista de trabajo de la API.
pub fn registrar(cls: Class) {
    let mut puestos = Vec::new();
    for (selector, tipos) in STUBS {
        let imp: Imp = match tipo_ret(tipos) {
            "@" => nulo as *const c_void,
            "v" => nada as *const c_void,
            "Q" => cero as *const c_void,
            "I" => cero32 as *const c_void,
            "c" | "B" => falso as *const c_void,
            "{?=QQ}" => ceros2 as *const c_void,
            "{?=QQQ}" => ceros3 as *const c_void,
            otro => {
                crate::registro(&format!("stub sin tipo conocido: -{selector} [{tipos}] ({otro})"));
                continue;
            }
        };
        // class_addMethod falla (y no pasa nada) si el método ya existe: lo implementado de verdad tiene prioridad.
        if objc::metodo(cls, selector, imp, tipos) != 0 {
            puestos.push(*selector);
        }
    }
    let texto = puestos.join("\n");
    let _ = std::fs::write("/private/var/tmp/tahoe-metal-stubs.txt", format!("{texto}\n"));
    crate::registro(&format!("stubs del protocolo MTLDevice: {} (lista en tahoe-metal-stubs.txt)", puestos.len()));
}
