//! Desbordamiento de mensajes grandes. Las ranuras de la cola de mensajes (región compartida de `emu_port.c`) tienen un
//! tamaño fijo; los datos fuera de línea de un mensaje pueden ser mucho mayores (en XNU, megas). Cuando un mensaje no cabe,
//! esos datos van a un archivo de `/dev/shm` (`tahoe.<arranque>.ool.<id>`), la ranura guarda solo el mensaje y el
//! receptor lee el archivo y lo borra. Si el mensaje se descarta sin recibirse, se borra igual. La limpieza del arranque
//! (`ports_cleanup`, que borra `/dev/shm/tahoe.<arranque>.*`) se lleva los que queden.
//!
//! Sin biblioteca estándar: las llamadas al sistema van por las funciones de la libc con la que se enlaza tahoe-run.

use core::ffi::{c_char, c_int};

extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn read(fd: c_int, buf: *mut u8, n: usize) -> isize;
    fn write(fd: c_int, buf: *const u8, n: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn unlink(path: *const c_char) -> c_int;
}
const O_RDONLY: c_int = 0;
const O_WRONLY: c_int = 1;
const O_CREAT: c_int = 0o100;
const O_EXCL: c_int = 0o200;
const O_CLOEXEC: c_int = 0o2000000;

/// Ruta `/dev/shm/tahoe.<etiqueta>.ool.<id>` terminada en NUL, en un búfer fijo.
struct Ruta([u8; 128]);

impl Ruta {
    fn nueva(etiqueta: &[u8], id: u64) -> Option<Ruta> {
        let mut b = [0u8; 128];
        let mut n = 0;
        let mut poner = |s: &[u8]| -> Option<()> {
            let fin = n + s.len();
            b.get_mut(n..fin)?.copy_from_slice(s);
            n = fin;
            Some(())
        };
        poner(b"/dev/shm/tahoe.")?;
        // la etiqueta del arranque es hexadecimal; cualquier otro carácter (como '/') se rechaza
        if etiqueta.is_empty() || !etiqueta.iter().all(u8::is_ascii_alphanumeric) {
            return None;
        }
        poner(etiqueta)?;
        poner(b".ool.")?;
        let mut dig = [0u8; 20];
        let mut v = id;
        let mut k = dig.len();
        loop {
            k -= 1;
            dig[k] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        poner(&dig[k..])?;
        // queda al menos un byte a cero detrás (b empezó a cero y n < 128 si cupo todo)
        (n < b.len()).then_some(Ruta(b))
    }
    fn ptr(&self) -> *const c_char {
        self.0.as_ptr().cast()
    }
}

/// Lee la etiqueta (cadena C) como bytes, como mucho 64.
///
/// # Safety
/// `etiqueta` debe ser nula o una cadena C válida.
unsafe fn etiqueta_de<'a>(etiqueta: *const c_char) -> &'a [u8] {
    if etiqueta.is_null() {
        return &[];
    }
    // SAFETY: cadena C terminada en NUL, según el llamante; se para en 64 bytes.
    let p = etiqueta.cast::<u8>();
    let mut n = 0;
    while n < 64 && unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: los n primeros bytes son legibles (recorridos arriba).
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// Guarda `len` bytes de `datos` en el archivo de desbordamiento `id`. 0 si todo se escribió; -1 si no.
///
/// # Safety
/// `etiqueta` es una cadena C válida y `datos` apunta a `len` bytes legibles.
#[no_mangle]
pub unsafe extern "C" fn nucleo_desborde_guardar(etiqueta: *const c_char, id: u64, datos: *const u8, len: u64) -> c_int {
    // SAFETY: según el contrato de esta función.
    let Some(ruta) = Ruta::nueva(unsafe { etiqueta_de(etiqueta) }, id) else { return -1 };
    if datos.is_null() && len > 0 {
        return -1;
    }
    // SAFETY: ruta terminada en NUL; open con modo como tercer argumento.
    let fd = unsafe { open(ruta.ptr(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0o600 as c_int) };
    if fd < 0 {
        return -1;
    }
    let mut hecho: u64 = 0;
    while hecho < len {
        let trozo = core::cmp::min(len - hecho, 1 << 30) as usize;
        // SAFETY: datos + hecho .. + trozo está dentro de los len bytes legibles.
        let n = unsafe { write(fd, datos.add(hecho as usize), trozo) };
        if n <= 0 {
            break;
        }
        hecho += n as u64;
    }
    // SAFETY: fd abierto arriba.
    unsafe { close(fd) };
    if hecho == len {
        0
    } else {
        // SAFETY: ruta válida.
        unsafe { unlink(ruta.ptr()) };
        -1
    }
}

/// Lee el archivo de desbordamiento `id` en `dst` (`len` bytes) y lo borra. 0 si se leyó entero; -1 si no (lo que no se
/// pudo leer queda a cero).
///
/// # Safety
/// `etiqueta` es una cadena C válida y `dst` apunta a `len` bytes escribibles.
#[no_mangle]
pub unsafe extern "C" fn nucleo_desborde_leer(etiqueta: *const c_char, id: u64, dst: *mut u8, len: u64) -> c_int {
    // SAFETY: según el contrato de esta función.
    let Some(ruta) = Ruta::nueva(unsafe { etiqueta_de(etiqueta) }, id) else { return -1 };
    if dst.is_null() && len > 0 {
        return -1;
    }
    // SAFETY: ruta terminada en NUL.
    let fd = unsafe { open(ruta.ptr(), O_RDONLY | O_CLOEXEC) };
    let mut hecho: u64 = 0;
    if fd >= 0 {
        while hecho < len {
            let trozo = core::cmp::min(len - hecho, 1 << 30) as usize;
            // SAFETY: dst + hecho .. + trozo está dentro de los len bytes escribibles.
            let n = unsafe { read(fd, dst.add(hecho as usize), trozo) };
            if n <= 0 {
                break;
            }
            hecho += n as u64;
        }
        // SAFETY: fd abierto arriba.
        unsafe { close(fd) };
    }
    if hecho < len {
        // SAFETY: el resto de dst sigue dentro de los len bytes escribibles.
        unsafe { core::ptr::write_bytes(dst.add(hecho as usize), 0, (len - hecho) as usize) };
    }
    // SAFETY: ruta válida.
    unsafe { unlink(ruta.ptr()) };
    if hecho == len { 0 } else { -1 }
}

/// Borra el archivo de desbordamiento `id` (mensaje descartado sin recibir).
///
/// # Safety
/// `etiqueta` es una cadena C válida.
#[no_mangle]
pub unsafe extern "C" fn nucleo_desborde_borrar(etiqueta: *const c_char, id: u64) {
    // SAFETY: según el contrato de esta función.
    if let Some(ruta) = Ruta::nueva(unsafe { etiqueta_de(etiqueta) }, id) {
        // SAFETY: ruta válida.
        unsafe { unlink(ruta.ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn ruta() {
        let r = Ruta::nueva(b"6acc5f38", 1234).unwrap();
        let s = std::ffi::CStr::from_bytes_until_nul(&r.0).unwrap();
        assert_eq!(s.to_str().unwrap(), "/dev/shm/tahoe.6acc5f38.ool.1234");
        assert!(Ruta::nueva(b"../x", 1).is_none()); // nada de rutas raras
        assert!(Ruta::nueva(b"", 1).is_none());
        assert!(Ruta::nueva(&[b'a'; 120], 1).is_none()); // no cabe
    }

    #[test]
    fn guardar_leer_y_borrar() {
        let tag = CString::new(format!("prueba{}", std::process::id())).unwrap();
        let datos: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect(); // mayor que una ranura
        // SAFETY: búferes válidos del tamaño indicado.
        unsafe {
            assert_eq!(nucleo_desborde_guardar(tag.as_ptr(), 7, datos.as_ptr(), datos.len() as u64), 0);
            // el mismo id no se pisa (O_EXCL)
            assert_eq!(nucleo_desborde_guardar(tag.as_ptr(), 7, datos.as_ptr(), 1), -1);
            let mut fuera = vec![0xffu8; datos.len()];
            assert_eq!(nucleo_desborde_leer(tag.as_ptr(), 7, fuera.as_mut_ptr(), fuera.len() as u64), 0);
            assert_eq!(fuera, datos);
            // ya se borró: leerlo otra vez falla y deja ceros
            let mut otra = vec![0xffu8; 16];
            assert_eq!(nucleo_desborde_leer(tag.as_ptr(), 7, otra.as_mut_ptr(), 16), -1);
            assert_eq!(otra, vec![0u8; 16]);
            nucleo_desborde_borrar(tag.as_ptr(), 7); // no existe: sin efecto
        }
    }
}
