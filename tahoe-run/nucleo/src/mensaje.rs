//! Formato de los mensajes Mach (cabecera y descriptores), como `mach/message.h` de XNU para x86_64.
//!
//! Un mensaje empieza con `mach_msg_header_t` (24 bytes). Si el bit «complejo» (0x80000000 en `msgh_bits`) está
//! puesto, sigue un `mach_msg_body_t` con el número de descriptores y luego los descriptores, de 12 o 16 bytes:
//!
//! | tipo | descriptor                 | tamaño | campos (desplazamiento)                                         |
//! |------|----------------------------|--------|-----------------------------------------------------------------|
//! | 0    | puerto                     | 12     | nombre (+0), disposición (+10), tipo (+11)                       |
//! | 1    | memoria fuera de línea     | 16     | dirección (+0), dealloc/copia (+8, +9), tipo (+11), tamaño (+12) |
//! | 2    | puertos fuera de línea     | 16     | dirección (+0), disposición (+10), tipo (+11), número (+12)      |
//! | 3    | memoria volátil            | 16     | como el 1                                                       |
//! | 4    | puerto con guarda          | 16     | contexto (+0), flags (+8), disposición (+10), tipo (+11), nombre (+12) |
//!
//! El tipo está siempre en el byte +11, así que se puede recorrer la lista sin conocer los demás campos.

/// Tamaño de `mach_msg_header_t`.
pub const CABECERA: usize = 24;
const COMPLEJO: u32 = 0x8000_0000;

/// Clase de descriptor (byte +11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Clase {
    Puerto = 0,
    Memoria = 1,
    Puertos = 2,
    MemoriaVolatil = 3,
    PuertoConGuarda = 4,
}

impl Clase {
    fn de(b: u8) -> Option<Clase> {
        Some(match b {
            0 => Clase::Puerto,
            1 => Clase::Memoria,
            2 => Clase::Puertos,
            3 => Clase::MemoriaVolatil,
            4 => Clase::PuertoConGuarda,
            _ => return None,
        })
    }
    /// Bytes que ocupa el descriptor en el mensaje.
    pub fn tamano(self) -> usize {
        if self == Clase::Puerto { 12 } else { 16 }
    }
    /// ¿Lleva datos fuera de línea (memoria o una lista de nombres de puerto) que el núcleo copia?
    pub fn fuera_de_linea(self) -> bool {
        matches!(self, Clase::Memoria | Clase::Puertos | Clase::MemoriaVolatil)
    }
}

/// Un descriptor ya leído. Interfaz C: `struct nucleo_desc` en `nucleo.h` (mismo orden y tamaños).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Desc {
    /// Desplazamiento del descriptor en el mensaje.
    pub off: u32,
    /// Clase (0-4, ver `Clase`).
    pub clase: u8,
    /// Disposición del derecho (puertos y puertos fuera de línea).
    pub disp: u8,
    pub _r: u16,
    /// Nombre del puerto (descriptores de puerto y de puerto con guarda); 0 en los demás.
    pub nombre: u32,
    /// Bytes de datos fuera de línea (para los de puertos, número × 4); 0 en los demás.
    pub bytes: u32,
    /// Dirección de los datos fuera de línea (en el emisor), o el contexto de un puerto con guarda.
    pub dir: u64,
}

fn u32_en(m: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(m.get(off..off + 4)?.try_into().ok()?))
}
fn u64_en(m: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(m.get(off..off + 8)?.try_into().ok()?))
}

/// Error al leer un mensaje.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Más corto que la cabecera.
    Corto,
    /// Un descriptor se sale del mensaje o tiene un tipo desconocido.
    Descriptor,
}

/// Recorre los descriptores de un mensaje. `f` recibe cada uno; devuelve cuántos hay (0 si el mensaje no es complejo).
/// Un mensaje mal formado (descriptor fuera de límites o de tipo desconocido) da error y no se mira más allá.
pub fn descriptores(m: &[u8], mut f: impl FnMut(Desc)) -> Result<usize, Error> {
    let bits = u32_en(m, 0).ok_or(Error::Corto)?;
    if m.len() < CABECERA {
        return Err(Error::Corto);
    }
    if bits & COMPLEJO == 0 {
        return Ok(0);
    }
    let cuenta = u32_en(m, CABECERA).ok_or(Error::Descriptor)? as usize;
    let mut off = CABECERA + 4;
    for _ in 0..cuenta {
        let clase = Clase::de(*m.get(off + 11).ok_or(Error::Descriptor)?).ok_or(Error::Descriptor)?;
        if off + clase.tamano() > m.len() {
            return Err(Error::Descriptor);
        }
        let disp = m[off + 10];
        let d = match clase {
            Clase::Puerto => Desc { nombre: u32_en(m, off).unwrap_or(0), ..Default::default() },
            Clase::PuertoConGuarda => {
                Desc { nombre: u32_en(m, off + 12).unwrap_or(0), dir: u64_en(m, off).unwrap_or(0), ..Default::default() }
            }
            Clase::Memoria | Clase::MemoriaVolatil => {
                Desc { bytes: u32_en(m, off + 12).unwrap_or(0), dir: u64_en(m, off).unwrap_or(0), ..Default::default() }
            }
            Clase::Puertos => {
                // número de puertos × 4 bytes por nombre; un número absurdo no puede desbordar
                let n = u32_en(m, off + 12).unwrap_or(0);
                Desc { bytes: n.checked_mul(4).ok_or(Error::Descriptor)?, dir: u64_en(m, off).unwrap_or(0), ..Default::default() }
            }
        };
        f(Desc { off: off as u32, clase: clase as u8, disp, ..d });
        off += clase.tamano();
    }
    Ok(cuenta)
}

/// Interfaz C: rellena `out` (hasta `max`) con los descriptores del mensaje `m` (`tam` bytes) y devuelve cuántos hay en
/// total, o -1 si el mensaje está mal formado (en ese caso los de `out` son los leídos hasta el error).
///
/// # Safety
/// `m` debe apuntar a `tam` bytes legibles y `out` a `max` elementos escribibles (o ser nulo con `max` = 0).
#[no_mangle]
pub unsafe extern "C" fn nucleo_descriptores(m: *const u8, tam: u32, out: *mut Desc, max: u32) -> i32 {
    if m.is_null() {
        return -1;
    }
    // SAFETY: el llamante garantiza que m apunta a tam bytes.
    let msg = unsafe { core::slice::from_raw_parts(m, tam as usize) };
    let salida: &mut [Desc] = if out.is_null() || max == 0 {
        &mut []
    } else {
        // SAFETY: el llamante garantiza max elementos escribibles en out.
        unsafe { core::slice::from_raw_parts_mut(out, max as usize) }
    };
    let mut n = 0usize;
    let r = descriptores(msg, |d| {
        if let Some(s) = salida.get_mut(n) {
            *s = d;
        }
        n += 1;
    });
    match r {
        Ok(total) => i32::try_from(total).unwrap_or(i32::MAX),
        Err(_) => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn cabecera(complejo: bool, tam: u32) -> Vec<u8> {
        let mut m = Vec::new();
        let bits: u32 = 0x1513 | if complejo { COMPLEJO } else { 0 };
        m.extend_from_slice(&bits.to_le_bytes());
        m.extend_from_slice(&tam.to_le_bytes());
        m.extend_from_slice(&[0u8; 16]); // remoto, local, vale, id
        m
    }
    fn puerto(nombre: u32, disp: u8) -> [u8; 12] {
        let mut d = [0u8; 12];
        d[0..4].copy_from_slice(&nombre.to_le_bytes());
        d[10] = disp;
        d[11] = 0;
        d
    }
    fn de16(clase: u8, a: u64, disp: u8, ultimo: u32) -> [u8; 16] {
        let mut d = [0u8; 16];
        d[0..8].copy_from_slice(&a.to_le_bytes());
        d[10] = disp;
        d[11] = clase;
        d[12..16].copy_from_slice(&ultimo.to_le_bytes());
        d
    }
    fn mensaje(descs: &[&[u8]]) -> Vec<u8> {
        let mut m = cabecera(true, 0);
        m.extend_from_slice(&(descs.len() as u32).to_le_bytes());
        for d in descs {
            m.extend_from_slice(d);
        }
        let t = m.len() as u32;
        m[4..8].copy_from_slice(&t.to_le_bytes());
        m
    }
    fn todos(m: &[u8]) -> Result<Vec<Desc>, Error> {
        let mut v = Vec::new();
        descriptores(m, |d| v.push(d)).map(|_| v)
    }

    #[test]
    fn mensaje_simple_sin_descriptores() {
        assert_eq!(todos(&cabecera(false, 24)), Ok(vec![]));
        assert_eq!(todos(&[0u8; 10]), Err(Error::Corto));
    }

    #[test]
    fn puerto_memoria_y_puertos() {
        let m = mensaje(&[&puerto(0x2e03, 17), &de16(1, 0x7000, 0, 300), &de16(2, 0x8000, 19, 5)]);
        let v = todos(&m).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!((v[0].clase, v[0].nombre, v[0].disp, v[0].off), (0, 0x2e03, 17, 28));
        assert_eq!((v[1].clase, v[1].bytes, v[1].dir, v[1].off), (1, 300, 0x7000, 40));
        assert_eq!((v[2].clase, v[2].bytes, v[2].disp), (2, 20, 19)); // 5 puertos × 4 bytes
    }

    #[test]
    fn puerto_con_guarda_no_es_memoria() {
        // el fallo de la versión en C: el tipo 4 se trataba como datos fuera de línea (dirección = guarda, tamaño =
        // nombre del puerto); aquí es un puerto con su nombre en +12 y su guarda en +0
        let m = mensaje(&[&de16(4, 0xdead_beef, 16, 0x1e103)]);
        let v = todos(&m).unwrap();
        assert_eq!((v[0].clase, v[0].nombre, v[0].dir, v[0].bytes, v[0].disp), (4, 0x1e103, 0xdead_beef, 0, 16));
        assert!(!Clase::PuertoConGuarda.fuera_de_linea());
    }

    #[test]
    fn mensajes_mal_formados() {
        let mut m = mensaje(&[&puerto(1, 17)]);
        m.truncate(m.len() - 1); // descriptor cortado
        assert_eq!(todos(&m), Err(Error::Descriptor));
        let mut m = mensaje(&[&puerto(1, 17)]);
        m[28 + 11] = 9; // tipo desconocido
        assert_eq!(todos(&m), Err(Error::Descriptor));
        let mut m = mensaje(&[]);
        m[24..28].copy_from_slice(&u32::MAX.to_le_bytes()); // dice tener 4 000 millones de descriptores
        assert_eq!(todos(&m), Err(Error::Descriptor));
        let m = mensaje(&[&de16(2, 0, 0, u32::MAX)]); // número de puertos que desbordaría ×4
        assert_eq!(todos(&m), Err(Error::Descriptor));
    }

    #[test]
    fn interfaz_c() {
        let m = mensaje(&[&puerto(7, 18), &de16(1, 1, 0, 8)]);
        let mut out = [Desc::default(); 1];
        // SAFETY: búferes válidos del tamaño indicado.
        let n = unsafe { nucleo_descriptores(m.as_ptr(), m.len() as u32, out.as_mut_ptr(), 1) };
        assert_eq!(n, 2); // hay 2, aunque solo cabía 1
        assert_eq!(out[0].nombre, 7);
        // SAFETY: puntero nulo con tamaño 0.
        assert_eq!(unsafe { nucleo_descriptores(core::ptr::null(), 0, core::ptr::null_mut(), 0) }, -1);
    }
}
