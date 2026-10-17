//! Canal con el servidor de GPU de Linux (tahoe-gpu servir): socket Unix cuyo nombre publica el IOAccelerator emulado
//! (propiedad TahoeGPUSocket). Un canal por proceso.

use std::ffi::{c_char, c_int, c_void};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, OnceLock};

#[path = "../../tahoe-gpu/src/protocolo.rs"]
pub mod protocolo;

use crate::objc::{self, Id};
use crate::registro;
use protocolo::{tipo, Escritor, Info, Lector};

extern "C" {
    fn dlopen(p: *const c_char, modo: c_int) -> *mut c_void;
    fn dlsym(h: *mut c_void, s: *const c_char) -> *mut c_void;
}

pub struct Canal {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl Canal {
    fn conectar(ruta: &str) -> std::io::Result<Canal> {
        let w = UnixStream::connect(ruta)?;
        let r = BufReader::new(w.try_clone()?);
        Ok(Canal { w, r })
    }

    /// Petición y respuesta.
    pub fn pedir(&mut self, t: u32, cuerpo: &[u8]) -> std::io::Result<Vec<u8>> {
        protocolo::enviar(&mut self.w, t, cuerpo)?;
        let (rt, c) = protocolo::recibir(&mut self.r)?;
        if rt == tipo::ERROR {
            let msg = Lector::new(&c).texto().unwrap_or_default();
            return Err(std::io::Error::other(msg));
        }
        Ok(c)
    }
}

pub struct Gpu {
    pub canal: Mutex<Canal>,
    pub info: Info,
    /// ruta de la memoria de una IOSurface con «%u» en lugar de su ID (propiedad TahoeSurfacePath)
    pub ruta_superficies: String,
}

static GPU: OnceLock<Option<Gpu>> = OnceLock::new();

/// La GPU del proceso (conecta la primera vez, con el servicio del IOAccelerator).
pub fn gpu(puerto: u32) -> Option<&'static Gpu> {
    GPU.get_or_init(|| match conectar(puerto) {
        Ok(g) => {
            registro(&format!("conectado al servidor de GPU: {:?}", g.info));
            Some(g)
        }
        Err(e) => {
            registro(&format!("sin servidor de GPU: {e}"));
            None
        }
    })
    .as_ref()
}

pub fn gpu_ya() -> Option<&'static Gpu> {
    GPU.get().and_then(|g| g.as_ref())
}

fn conectar(puerto: u32) -> std::io::Result<Gpu> {
    // el puerto de initWithAcceleratorPort: no siempre es el servicio: se busca el IOAccelerator de tahoe-run
    let servicio = buscar_servicio("TahoeAccelerator");
    let ruta_superficies = servicio.and_then(|s| propiedad_texto(s, "TahoeSurfacePath")).unwrap_or_default();
    let ruta = servicio
        .and_then(|s| propiedad_texto(s, "TahoeGPUSocket"))
        .or_else(|| propiedad_texto(puerto, "TahoeGPUSocket"))
        .ok_or_else(|| std::io::Error::other("no encuentro TahoeGPUSocket en el IOAccelerator"))?;
    let mut c = Canal::conectar(&ruta).map_err(|e| std::io::Error::other(format!("{ruta}: {e}")))?;
    let mut e = Escritor::default();
    e.u32(protocolo::VERSION);
    let r = c.pedir(tipo::HOLA, &e.0)?;
    let info = Info::leer(&mut Lector::new(&r))?;
    Ok(Gpu { canal: Mutex::new(c), info, ruta_superficies })
}

/// El primer servicio de IOKit de esa clase (IOServiceGetMatchingService(IOServiceMatching(clase))).
fn buscar_servicio(clase: &str) -> Option<u32> {
    let c = std::ffi::CString::new(clase).ok()?;
    // SAFETY: funciones de IOKit con sus firmas; IOServiceGetMatchingService consume el diccionario.
    unsafe {
        let iokit = dlopen(c"/System/Library/Frameworks/IOKit.framework/IOKit".as_ptr(), 2);
        let coincide = dlsym(iokit, c"IOServiceMatching".as_ptr());
        let buscar = dlsym(iokit, c"IOServiceGetMatchingService".as_ptr());
        if coincide.is_null() || buscar.is_null() {
            return None;
        }
        let coincide: extern "C" fn(*const c_char) -> Id = std::mem::transmute(coincide);
        let buscar: extern "C" fn(u32, Id) -> u32 = std::mem::transmute(buscar);
        let s = buscar(0, coincide(c.as_ptr()));
        (s != 0).then_some(s)
    }
}

/// Propiedad de texto de una entrada del registro de IOKit (IORegistryEntryCreateCFProperty).
fn propiedad_texto(entrada: u32, clave: &str) -> Option<String> {
    // SAFETY: IOKit y CoreFoundation están en la caché de dyld; las firmas son las de sus cabeceras. CFString y NSString
    // son el mismo objeto (toll-free bridging).
    unsafe {
        let iokit = dlopen(c"/System/Library/Frameworks/IOKit.framework/IOKit".as_ptr(), 2);
        let cf = dlopen(c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation".as_ptr(), 2);
        let crear = dlsym(iokit, c"IORegistryEntryCreateCFProperty".as_ptr());
        let soltar = dlsym(cf, c"CFRelease".as_ptr());
        if crear.is_null() || soltar.is_null() {
            return None;
        }
        let crear: extern "C" fn(u32, Id, *const c_void, u32) -> Id = std::mem::transmute(crear);
        let soltar: extern "C" fn(Id) = std::mem::transmute(soltar);
        let v = crear(entrada, objc::nsstring(clave), std::ptr::null(), 0);
        if v.is_null() {
            return None;
        }
        let s = objc::texto(v);
        soltar(v);
        Some(s)
    }
}
