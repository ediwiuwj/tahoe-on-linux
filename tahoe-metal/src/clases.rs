//! Las clases de Metal del driver (MTLCommandQueue, MTLBuffer, MTLLibrary, MTLFunction, MTLComputePipelineState,
//! MTLCommandBuffer y los encoders), creadas en tiempo de ejecución con el runtime de Objective-C. Cada objeto guarda su
//! estado de Rust en una variable de instancia (`_tahoe`, un puntero a una caja) que se suelta en dealloc.
//!
//! El trabajo de verdad lo hace el servidor de GPU de Linux (canal.rs): los buffers son memoria compartida en /dev/shm
//! que él importa sin copias, las bibliotecas y pipelines se traducen allí (AIR → SPIR-V) y cada command buffer se manda
//! entero en commit.

use std::any::Any;
use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::canal::{self, protocolo};
use crate::objc::{self, Class, Id, Imp, Sel};
use crate::registro;
use protocolo::{clase, orden, tipo, Escritor, Lector};

#[link(name = "objc")]
extern "C" {
    fn class_addIvar(cls: Class, name: *const c_char, size: usize, alignment: u8, types: *const c_char) -> objc::Bool;
    fn class_getInstanceVariable(cls: Class, name: *const c_char) -> *mut c_void;
    fn ivar_getOffset(ivar: *mut c_void) -> isize;
}

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn _Block_copy(b: *const c_void) -> *mut c_void;
    fn _Block_release(b: *const c_void);
    fn dispatch_data_create_map(data: Id, buffer: *mut *const c_void, size: *mut usize) -> Id;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MTLSize {
    pub w: u64,
    pub h: u64,
    pub d: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NSRange {
    pub loc: u64,
    pub len: u64,
}

/// Una clase creada aquí y dónde guarda su estado.
pub struct Clase {
    pub cls: Class,
    off: isize,
}

// SAFETY: las clases del runtime de Objective-C son globales e inmutables una vez registradas.
unsafe impl Send for Clase {}
unsafe impl Sync for Clase {}

pub struct Clases {
    pub cola: Clase,
    pub buffer: Clase,
    pub biblioteca: Clase,
    pub funcion: Clase,
    pub pipeline: Clase,
    pub cb: Clase,
    pub computo: Clase,
    pub blit: Clase,
}

static CLASES: OnceLock<Clases> = OnceLock::new();
/// El MTLDevice del proceso (sin retener: vive lo que el proceso).
pub static DISPOSITIVO: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static IDS: AtomicU32 = AtomicU32::new(1);

fn nuevo_id() -> u32 {
    IDS.fetch_add(1, Ordering::Relaxed)
}

pub fn clases() -> &'static Clases {
    CLASES.get().expect("clases sin registrar")
}

type Metodo = (&'static str, Imp, &'static str);

/// Crea y registra una clase hija de NSObject con esos protocolos y métodos (más dealloc y el registro de lo que no
/// tiene).
fn nueva_clase(nombre: &str, protocolos: &[&str], metodos: &[Metodo]) -> Clase {
    let c = std::ffi::CString::new(nombre).unwrap();
    // SAFETY: clase nueva con NSObject de madre; los métodos llevan sus firmas.
    unsafe {
        let cls = objc::objc_allocateClassPair(objc::clase("NSObject"), c.as_ptr(), 0);
        class_addIvar(cls, c"_tahoe".as_ptr(), 8, 3, c"^v".as_ptr());
        for p in protocolos {
            let pc = std::ffi::CString::new(*p).unwrap();
            let pr = objc::objc_getProtocol(pc.as_ptr());
            if !pr.is_null() {
                objc::class_addProtocol(cls, pr);
            }
        }
        for (s, imp, t) in metodos {
            objc::metodo(cls, s, *imp, t);
        }
        objc::metodo(cls, "dealloc", dealloc as *const c_void, "v@:");
        objc::metodo(cls, "methodSignatureForSelector:", firma as *const c_void, "@@::");
        objc::metodo(cls, "forwardInvocation:", reenviar as *const c_void, "v@:@");
        objc::objc_registerClassPair(cls);
        let off = ivar_getOffset(class_getInstanceVariable(cls, c"_tahoe".as_ptr()));
        Clase { cls, off }
    }
}

/// Objeto nuevo (+1, como los new*) con su estado.
fn crear<T: Any>(c: &Clase, estado: T) -> Id {
    let caja: Box<Box<dyn Any>> = Box::new(Box::new(estado));
    // SAFETY: alloc/init de NSObject; el ivar _tahoe es de 8 bytes en `off`.
    unsafe {
        let o: Id = objc::m0(objc::m0::<Id>(c.cls, "alloc"), "init");
        *(o.cast::<u8>().offset(c.off) as *mut *mut Box<dyn Any>) = Box::into_raw(caja);
        o
    }
}

/// El estado de Rust de un objeto de estas clases (None si es de otra clase o ya se soltó).
pub fn estado<T: Any>(o: Id) -> Option<&'static mut T> {
    if o.is_null() {
        return None;
    }
    let cs = clases();
    // SAFETY: si la clase es una de las nuestras, el ivar está en su desplazamiento.
    unsafe {
        let cls = objc::object_getClass(o);
        let c = [&cs.cola, &cs.buffer, &cs.biblioteca, &cs.funcion, &cs.pipeline, &cs.cb, &cs.computo, &cs.blit].into_iter().find(|c| c.cls == cls)?;
        let p = *(o.cast::<u8>().offset(c.off) as *mut *mut Box<dyn Any>);
        if p.is_null() {
            return None;
        }
        (*p).downcast_mut::<T>()
    }
}

extern "C-unwind" fn dealloc(this: Id, cmd: Sel) {
    let cs = clases();
    // SAFETY: objeto de una de nuestras clases; super dealloc es el de NSObject.
    unsafe {
        let cls = objc::object_getClass(this);
        if let Some(c) = [&cs.cola, &cs.buffer, &cs.biblioteca, &cs.funcion, &cs.pipeline, &cs.cb, &cs.computo, &cs.blit].into_iter().find(|c| c.cls == cls) {
            let pp = this.cast::<u8>().offset(c.off) as *mut *mut Box<dyn Any>;
            if !(*pp).is_null() {
                drop(Box::from_raw(*pp));
                *pp = std::ptr::null_mut();
            }
        }
        objc::super0::<()>(this, objc::clase("NSObject"), cmd);
    }
}

extern "C-unwind" fn firma(this: Id, cmd: Sel, s: Sel) -> Id {
    // SAFETY: NSObject implementa methodSignatureForSelector:.
    unsafe {
        let r: Id = objc::super1(this, objc::clase("NSObject"), cmd, s);
        if !r.is_null() {
            return r;
        }
        objc::m1(objc::clase("NSMethodSignature"), "signatureWithObjCTypes:", c"@@:".as_ptr())
    }
}

extern "C-unwind" fn reenviar(this: Id, _cmd: Sel, inv: Id) {
    // SAFETY: inv es un NSInvocation.
    let s: Sel = unsafe { objc::m0(inv, "selector") };
    registro(&format!("sin implementar: -[{} {}]", objc::nombre_clase(objc::clase_de(this)), objc::nombre_sel(s)));
}

fn retener(o: Id) -> Id {
    if o.is_null() {
        return o;
    }
    // SAFETY: objeto de Objective-C.
    unsafe { objc::m0(o, "retain") }
}

fn soltar(o: Id) {
    if !o.is_null() {
        // SAFETY: objeto retenido antes.
        unsafe { objc::m0::<()>(o, "release") }
    }
}

/// NSError con un mensaje, en un parámetro de salida `NSError**` (si lo hay).
fn poner_error(salida: *mut Id, msg: &str) {
    registro(&format!("error: {msg}"));
    if salida.is_null() {
        return;
    }
    // SAFETY: NSError/NSDictionary de Foundation; salida apunta a un Id.
    unsafe {
        let info: Id = objc::m2(objc::clase("NSDictionary"), "dictionaryWithObject:forKey:", objc::nsstring(msg), objc::nsstring("NSLocalizedDescription"));
        let e: Id = objc::m3(objc::clase("NSError"), "errorWithDomain:code:userInfo:", objc::nsstring("TahoeMetal"), 1i64, info);
        *salida = e;
    }
}

fn pedir(t: u32, cuerpo: &[u8]) -> Result<Vec<u8>, String> {
    let g = canal::gpu_ya().ok_or("sin servidor de GPU")?;
    let mut c = g.canal.lock().map_err(|_| "canal envenenado")?;
    c.pedir(t, cuerpo).map_err(|e| e.to_string())
}

fn liberar_en_servidor(cl: u32, id: u32) {
    let mut e = Escritor::default();
    e.u32(cl).u32(id);
    let _ = pedir(tipo::LIBERAR, &e.0);
}

extern "C-unwind" fn dispositivo(_this: Id, _cmd: Sel) -> Id {
    DISPOSITIVO.load(Ordering::Relaxed)
}

/// label y setLabel: comunes (el estado lleva `etiqueta`).
trait ConEtiqueta {
    fn etiqueta(&mut self) -> &mut Id;
}
macro_rules! etiqueta_de {
    ($t:ty) => {
        impl ConEtiqueta for $t {
            fn etiqueta(&mut self) -> &mut Id {
                &mut self.etiqueta
            }
        }
    };
}

extern "C-unwind" fn etiqueta<T: ConEtiqueta + Any>(this: Id, _cmd: Sel) -> Id {
    estado::<T>(this).map(|e| *e.etiqueta()).unwrap_or(std::ptr::null_mut())
}

extern "C-unwind" fn poner_etiqueta<T: ConEtiqueta + Any>(this: Id, _cmd: Sel, l: Id) {
    if let Some(e) = estado::<T>(this) {
        let viejo = *e.etiqueta();
        // SAFETY: copia de un NSString.
        *e.etiqueta() = if l.is_null() { l } else { unsafe { objc::m0(l, "copy") } };
        soltar(viejo);
    }
}

extern "C-unwind" fn nada(_this: Id, _cmd: Sel) {}
extern "C-unwind" fn nada1(_this: Id, _cmd: Sel, _a: Id) {}

// ------------------------------------------------------------------ cola

pub struct Cola {
    etiqueta: Id,
}
etiqueta_de!(Cola);

impl Drop for Cola {
    fn drop(&mut self) {
        soltar(self.etiqueta);
    }
}

pub fn nueva_cola() -> Id {
    crear(&clases().cola, Cola { etiqueta: std::ptr::null_mut() })
}

extern "C-unwind" fn cola_command_buffer(this: Id, _cmd: Sel) -> Id {
    let cb = crear(&clases().cb, CommandBuffer { cola: retener(this), ordenes: Escritor::default(), al_acabar: Vec::new(), al_programar: Vec::new(), estado: 0, error: std::ptr::null_mut(), etiqueta: std::ptr::null_mut() });
    // command buffer autoliberado, como en Metal
    // SAFETY: objeto recién creado.
    unsafe { objc::m0(cb, "autorelease") }
}

// ------------------------------------------------------------------ buffer

pub struct Buffer {
    pub id: u32,
    pub ptr: *mut u8,
    pub len: usize,
    proyectado: usize,
    opciones: u64,
    etiqueta: Id,
}
etiqueta_de!(Buffer);

impl Drop for Buffer {
    fn drop(&mut self) {
        liberar_en_servidor(clase::BUFFER, self.id);
        // SAFETY: proyección hecha en nuevo_buffer.
        unsafe { munmap(self.ptr.cast(), self.proyectado) };
        soltar(self.etiqueta);
    }
}

/// MTLBuffer: memoria compartida con el servidor de GPU (archivo en /dev/shm, que se borra en cuanto él lo abre).
pub fn nuevo_buffer(len: usize, opciones: u64, datos: Option<&[u8]>) -> Result<Id, String> {
    let id = nuevo_id();
    let proyectado = len.next_multiple_of(4096).max(4096);
    let ruta = format!("/dev/shm/tahoe-mtl-{}-{id}", std::process::id());
    let f = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&ruta).map_err(|e| format!("{ruta}: {e}"))?;
    f.set_len(proyectado as u64).map_err(|e| e.to_string())?;
    use std::os::fd::AsRawFd;
    // SAFETY: proyección compartida de un archivo propio (macOS: PROT_READ|PROT_WRITE = 3, MAP_SHARED = 1).
    let p = unsafe { mmap(std::ptr::null_mut(), proyectado, 3, 1, f.as_raw_fd(), 0) };
    drop(f);
    if p.is_null() || p as isize == -1 {
        let _ = std::fs::remove_file(&ruta);
        return Err("mmap del buffer".into());
    }
    let ptr = p.cast::<u8>();
    if let Some(d) = datos {
        // SAFETY: proyección de al menos len bytes.
        unsafe { std::ptr::copy_nonoverlapping(d.as_ptr(), ptr, d.len().min(len)) };
    }
    let mut e = Escritor::default();
    e.u32(id).u64(proyectado as u64).texto(&ruta);
    let r = pedir(tipo::BUFFER, &e.0);
    let _ = std::fs::remove_file(&ruta);
    if let Err(er) = r {
        // SAFETY: proyección de arriba.
        unsafe { munmap(p, proyectado) };
        return Err(er);
    }
    Ok(crear(&clases().buffer, Buffer { id, ptr, len, proyectado, opciones, etiqueta: std::ptr::null_mut() }))
}

extern "C-unwind" fn buffer_contents(this: Id, _cmd: Sel) -> *mut u8 {
    estado::<Buffer>(this).map(|b| b.ptr).unwrap_or(std::ptr::null_mut())
}
extern "C-unwind" fn buffer_length(this: Id, _cmd: Sel) -> u64 {
    estado::<Buffer>(this).map(|b| b.len as u64).unwrap_or(0)
}
extern "C-unwind" fn buffer_allocated(this: Id, _cmd: Sel) -> u64 {
    estado::<Buffer>(this).map(|b| b.proyectado as u64).unwrap_or(0)
}
extern "C-unwind" fn buffer_gpu_address(this: Id, _cmd: Sel) -> u64 {
    estado::<Buffer>(this).map(|b| b.ptr as u64).unwrap_or(0)
}
extern "C-unwind" fn buffer_opciones(this: Id, _cmd: Sel) -> u64 {
    estado::<Buffer>(this).map(|b| b.opciones).unwrap_or(0)
}
extern "C-unwind" fn buffer_storage(this: Id, _cmd: Sel) -> u64 {
    estado::<Buffer>(this).map(|b| (b.opciones >> 4) & 0xf).unwrap_or(0)
}
extern "C-unwind" fn buffer_cache(this: Id, _cmd: Sel) -> u64 {
    estado::<Buffer>(this).map(|b| b.opciones & 0xf).unwrap_or(0)
}
extern "C-unwind" fn buffer_hazard(this: Id, _cmd: Sel) -> u64 {
    estado::<Buffer>(this).map(|b| (b.opciones >> 8) & 0x3).unwrap_or(0)
}
extern "C-unwind" fn buffer_modificado(_this: Id, _cmd: Sel, _r: NSRange) {}

// ------------------------------------------------------------------ biblioteca y funciones

pub struct Biblioteca {
    id: u32,
    nombres: Vec<String>,
    etiqueta: Id,
}
etiqueta_de!(Biblioteca);

impl Drop for Biblioteca {
    fn drop(&mut self) {
        liberar_en_servidor(clase::BIBLIOTECA, self.id);
        soltar(self.etiqueta);
    }
}

pub fn nueva_biblioteca(datos: &[u8], error: *mut Id) -> Id {
    let id = nuevo_id();
    let mut e = Escritor::default();
    e.u32(id).bytes(datos);
    match pedir(tipo::BIBLIOTECA, &e.0) {
        Ok(r) => {
            let mut l = Lector::new(&r);
            let n = l.u32().unwrap_or(0);
            let nombres = (0..n).filter_map(|_| l.texto().ok()).collect();
            crear(&clases().biblioteca, Biblioteca { id, nombres, etiqueta: std::ptr::null_mut() })
        }
        Err(er) => {
            poner_error(error, &format!("biblioteca: {er}"));
            std::ptr::null_mut()
        }
    }
}

/// Bytes de un dispatch_data_t.
///
/// # Safety
/// `d` debe ser un dispatch_data_t.
pub unsafe fn bytes_de_dispatch_data(d: Id) -> Vec<u8> {
    let mut p: *const c_void = std::ptr::null();
    let mut n = 0usize;
    let m = dispatch_data_create_map(d, &mut p, &mut n);
    let v = if p.is_null() { Vec::new() } else { std::slice::from_raw_parts(p.cast::<u8>(), n).to_vec() };
    soltar(m);
    v
}

pub struct Funcion {
    biblioteca: Id,
    nombre: String,
    etiqueta: Id,
}
etiqueta_de!(Funcion);

impl Drop for Funcion {
    fn drop(&mut self) {
        soltar(self.biblioteca);
        soltar(self.etiqueta);
    }
}

extern "C-unwind" fn biblioteca_funcion(this: Id, _cmd: Sel, nombre: Id) -> Id {
    let n = objc::texto(nombre);
    let Some(b) = estado::<Biblioteca>(this) else { return std::ptr::null_mut() };
    if !b.nombres.iter().any(|x| *x == n) {
        registro(&format!("newFunctionWithName:{n}: no está en la biblioteca"));
        return std::ptr::null_mut();
    }
    crear(&clases().funcion, Funcion { biblioteca: retener(this), nombre: n, etiqueta: std::ptr::null_mut() })
}

extern "C-unwind" fn biblioteca_nombres(this: Id, _cmd: Sel) -> Id {
    let Some(b) = estado::<Biblioteca>(this) else { return std::ptr::null_mut() };
    let v: Vec<Id> = b.nombres.iter().map(|n| objc::nsstring(n)).collect();
    // SAFETY: NSArray de Foundation.
    unsafe { objc::m2(objc::clase("NSArray"), "arrayWithObjects:count:", v.as_ptr(), v.len() as u64) }
}

extern "C-unwind" fn funcion_nombre(this: Id, _cmd: Sel) -> Id {
    estado::<Funcion>(this).map(|f| objc::nsstring(&f.nombre)).unwrap_or(std::ptr::null_mut())
}

/// MTLFunctionTypeKernel (por ahora solo cómputo)
extern "C-unwind" fn funcion_tipo(_this: Id, _cmd: Sel) -> u64 {
    3
}

// ------------------------------------------------------------------ pipeline de cómputo

pub struct Pipeline {
    id: u32,
    ancho: u64,
    max_hilos: u64,
    etiqueta: Id,
}
etiqueta_de!(Pipeline);

impl Drop for Pipeline {
    fn drop(&mut self) {
        liberar_en_servidor(clase::PIPELINE, self.id);
        soltar(self.etiqueta);
    }
}

pub fn nuevo_pipeline(funcion: Id, error: *mut Id) -> Id {
    let Some(f) = estado::<Funcion>(funcion) else {
        poner_error(error, "la función no es de este dispositivo");
        return std::ptr::null_mut();
    };
    let Some(b) = estado::<Biblioteca>(f.biblioteca) else { return std::ptr::null_mut() };
    let id = nuevo_id();
    let mut e = Escritor::default();
    e.u32(id).u32(b.id).texto(&f.nombre);
    match pedir(tipo::PIPELINE_COMPUTO, &e.0) {
        Ok(r) => {
            let mut l = Lector::new(&r);
            let ancho = l.u32().unwrap_or(32) as u64;
            let max_hilos = l.u32().unwrap_or(256) as u64;
            crear(&clases().pipeline, Pipeline { id, ancho, max_hilos, etiqueta: std::ptr::null_mut() })
        }
        Err(er) => {
            poner_error(error, &format!("pipeline de {}: {er}", f.nombre));
            std::ptr::null_mut()
        }
    }
}

extern "C-unwind" fn pipeline_ancho(this: Id, _cmd: Sel) -> u64 {
    estado::<Pipeline>(this).map(|p| p.ancho).unwrap_or(32)
}
extern "C-unwind" fn pipeline_max(this: Id, _cmd: Sel) -> u64 {
    estado::<Pipeline>(this).map(|p| p.max_hilos).unwrap_or(256)
}
extern "C-unwind" fn cero(_this: Id, _cmd: Sel) -> u64 {
    0
}

// ------------------------------------------------------------------ command buffer

pub struct CommandBuffer {
    cola: Id,
    ordenes: Escritor,
    al_acabar: Vec<*mut c_void>,
    al_programar: Vec<*mut c_void>,
    /// MTLCommandBufferStatus: 0 sin encolar, 1 encolado, 2 enviado, 3 programado, 4 terminado, 5 error
    estado: u64,
    error: Id,
    etiqueta: Id,
}
etiqueta_de!(CommandBuffer);

impl Drop for CommandBuffer {
    fn drop(&mut self) {
        for b in self.al_acabar.drain(..).chain(self.al_programar.drain(..)) {
            // SAFETY: bloques copiados con _Block_copy.
            unsafe { _Block_release(b) };
        }
        soltar(self.cola);
        soltar(self.error);
        soltar(self.etiqueta);
    }
}

/// Llama a un bloque `void (^)(id<MTLCommandBuffer>)`.
///
/// # Safety
/// `b` debe ser un bloque con esa firma.
unsafe fn llamar_bloque(b: *mut c_void, arg: Id) {
    let invoke: extern "C-unwind" fn(*mut c_void, Id) = std::mem::transmute(*(b.cast::<u8>().add(16) as *const *const c_void));
    invoke(b, arg);
}

extern "C-unwind" fn cb_handler_acabar(this: Id, _cmd: Sel, b: *mut c_void) {
    if let Some(c) = estado::<CommandBuffer>(this) {
        // SAFETY: b es un bloque.
        c.al_acabar.push(unsafe { _Block_copy(b) });
    }
}
extern "C-unwind" fn cb_handler_programar(this: Id, _cmd: Sel, b: *mut c_void) {
    if let Some(c) = estado::<CommandBuffer>(this) {
        // SAFETY: b es un bloque.
        c.al_programar.push(unsafe { _Block_copy(b) });
    }
}

extern "C-unwind" fn cb_commit(this: Id, _cmd: Sel) {
    let Some(c) = estado::<CommandBuffer>(this) else { return };
    c.estado = 2;
    let programar: Vec<*mut c_void> = std::mem::take(&mut c.al_programar);
    for b in programar {
        // SAFETY: bloques de addScheduledHandler:.
        unsafe {
            llamar_bloque(b, this);
            _Block_release(b);
        }
    }
    let mut e = Escritor::default();
    e.bytes(&c.ordenes.0);
    let r = pedir(tipo::COMANDOS, &e.0).and_then(|r| {
        let mut l = Lector::new(&r);
        match l.u32() {
            Ok(0) => Ok(()),
            _ => Err(l.texto().unwrap_or_default()),
        }
    });
    let Some(c) = estado::<CommandBuffer>(this) else { return };
    match r {
        Ok(()) => c.estado = 4,
        Err(er) => {
            registro(&format!("command buffer: {er}"));
            c.estado = 5;
            let mut err: Id = std::ptr::null_mut();
            poner_error(&mut err, &er);
            c.error = retener(err);
        }
    }
    let acabar: Vec<*mut c_void> = std::mem::take(&mut c.al_acabar);
    for b in acabar {
        // SAFETY: bloques de addCompletedHandler:.
        unsafe {
            llamar_bloque(b, this);
            _Block_release(b);
        }
    }
}

extern "C-unwind" fn cb_estado(this: Id, _cmd: Sel) -> u64 {
    estado::<CommandBuffer>(this).map(|c| c.estado).unwrap_or(0)
}
extern "C-unwind" fn cb_error(this: Id, _cmd: Sel) -> Id {
    estado::<CommandBuffer>(this).map(|c| c.error).unwrap_or(std::ptr::null_mut())
}
extern "C-unwind" fn cb_cola(this: Id, _cmd: Sel) -> Id {
    estado::<CommandBuffer>(this).map(|c| c.cola).unwrap_or(std::ptr::null_mut())
}
extern "C-unwind" fn cb_enqueue(this: Id, _cmd: Sel) {
    if let Some(c) = estado::<CommandBuffer>(this) {
        if c.estado == 0 {
            c.estado = 1;
        }
    }
}
extern "C-unwind" fn cb_tiempo(_this: Id, _cmd: Sel) -> f64 {
    0.0
}

pub struct Encoder {
    cb: Id,
    etiqueta: Id,
}
etiqueta_de!(Encoder);

impl Drop for Encoder {
    fn drop(&mut self) {
        soltar(self.cb);
        soltar(self.etiqueta);
    }
}

fn ordenes(enc: Id) -> Option<&'static mut Escritor> {
    let e = estado::<Encoder>(enc)?;
    Some(&mut estado::<CommandBuffer>(e.cb)?.ordenes)
}

extern "C-unwind" fn cb_computo(this: Id, _cmd: Sel) -> Id {
    let e = crear(&clases().computo, Encoder { cb: retener(this), etiqueta: std::ptr::null_mut() });
    // SAFETY: objeto recién creado.
    unsafe { objc::m0(e, "autorelease") }
}
extern "C-unwind" fn cb_computo_tipo(this: Id, cmd: Sel, _tipo: u64) -> Id {
    cb_computo(this, cmd)
}
extern "C-unwind" fn cb_blit(this: Id, _cmd: Sel) -> Id {
    let e = crear(&clases().blit, Encoder { cb: retener(this), etiqueta: std::ptr::null_mut() });
    // SAFETY: objeto recién creado.
    unsafe { objc::m0(e, "autorelease") }
}

extern "C-unwind" fn enc_fin(this: Id, _cmd: Sel) {
    if let Some(o) = ordenes(this) {
        o.u32(orden::FIN);
    }
}

extern "C-unwind" fn comp_pipeline(this: Id, _cmd: Sel, p: Id) {
    if let (Some(o), Some(pl)) = (ordenes(this), estado::<Pipeline>(p)) {
        o.u32(orden::PIPELINE).u32(pl.id);
    }
}
extern "C-unwind" fn comp_buffer(this: Id, _cmd: Sel, b: Id, off: u64, idx: u64) {
    if let (Some(o), Some(bu)) = (ordenes(this), estado::<Buffer>(b)) {
        o.u32(orden::BUFFER).u32(idx as u32).u32(bu.id).u64(off);
    }
}
extern "C-unwind" fn comp_bytes(this: Id, _cmd: Sel, p: *const u8, len: u64, idx: u64) {
    if let Some(o) = ordenes(this) {
        // SAFETY: Metal garantiza len bytes en p.
        let d = unsafe { std::slice::from_raw_parts(p, len as usize) };
        o.u32(orden::BYTES).u32(idx as u32).bytes(d);
    }
}
extern "C-unwind" fn comp_memoria(this: Id, _cmd: Sel, len: u64, idx: u64) {
    if let Some(o) = ordenes(this) {
        o.u32(orden::MEMORIA_GRUPO).u32(idx as u32).u32(len as u32);
    }
}
fn despacho(this: Id, op: u32, a: MTLSize, b: MTLSize) {
    if let Some(o) = ordenes(this) {
        o.u32(op);
        for x in [a.w, a.h, a.d, b.w, b.h, b.d] {
            o.u32(x as u32);
        }
    }
}
extern "C-unwind" fn comp_grupos(this: Id, _cmd: Sel, g: MTLSize, t: MTLSize) {
    despacho(this, orden::GRUPOS, g, t);
}
extern "C-unwind" fn comp_hilos(this: Id, _cmd: Sel, g: MTLSize, t: MTLSize) {
    despacho(this, orden::HILOS, g, t);
}
extern "C-unwind" fn blit_copiar(this: Id, _cmd: Sel, a: Id, ao: u64, b: Id, bo: u64, n: u64) {
    if let (Some(o), Some(ba), Some(bb)) = (ordenes(this), estado::<Buffer>(a), estado::<Buffer>(b)) {
        o.u32(orden::COPIA_BUFFER).u32(ba.id).u64(ao).u32(bb.id).u64(bo).u64(n);
    }
}

// ------------------------------------------------------------------ registro de clases

pub fn registrar() {
    if CLASES.get().is_some() {
        return;
    }
    let comunes = |v: &mut Vec<Metodo>| {
        v.push(("device", dispositivo as Imp, "@@:"));
    };
    macro_rules! etiquetas {
        ($v:expr, $t:ty) => {
            $v.push(("label", etiqueta::<$t> as Imp, "@@:"));
            $v.push(("setLabel:", poner_etiqueta::<$t> as Imp, "v@:@"));
        };
    }
    let mut cola: Vec<Metodo> = vec![
        ("commandBuffer", cola_command_buffer as Imp, "@@:"),
        ("commandBufferWithUnretainedReferences", cola_command_buffer as Imp, "@@:"),
        ("insertDebugCaptureBoundary", nada as Imp, "v@:"),
    ];
    comunes(&mut cola);
    etiquetas!(cola, Cola);
    let mut buffer: Vec<Metodo> = vec![
        ("contents", buffer_contents as Imp, "^v@:"),
        ("length", buffer_length as Imp, "Q@:"),
        ("allocatedSize", buffer_allocated as Imp, "Q@:"),
        ("gpuAddress", buffer_gpu_address as Imp, "Q@:"),
        ("resourceOptions", buffer_opciones as Imp, "Q@:"),
        ("storageMode", buffer_storage as Imp, "Q@:"),
        ("cpuCacheMode", buffer_cache as Imp, "Q@:"),
        ("hazardTrackingMode", buffer_hazard as Imp, "Q@:"),
        ("didModifyRange:", buffer_modificado as Imp, "v@:{_NSRange=QQ}"),
    ];
    comunes(&mut buffer);
    etiquetas!(buffer, Buffer);
    let mut biblioteca: Vec<Metodo> = vec![
        ("newFunctionWithName:", biblioteca_funcion as Imp, "@@:@"),
        ("functionNames", biblioteca_nombres as Imp, "@@:"),
    ];
    comunes(&mut biblioteca);
    etiquetas!(biblioteca, Biblioteca);
    let mut funcion: Vec<Metodo> = vec![("name", funcion_nombre as Imp, "@@:"), ("functionType", funcion_tipo as Imp, "Q@:")];
    comunes(&mut funcion);
    etiquetas!(funcion, Funcion);
    let mut pipeline: Vec<Metodo> = vec![
        ("threadExecutionWidth", pipeline_ancho as Imp, "Q@:"),
        ("maxTotalThreadsPerThreadgroup", pipeline_max as Imp, "Q@:"),
        ("staticThreadgroupMemoryLength", cero as Imp, "Q@:"),
    ];
    comunes(&mut pipeline);
    etiquetas!(pipeline, Pipeline);
    let mut cb: Vec<Metodo> = vec![
        ("computeCommandEncoder", cb_computo as Imp, "@@:"),
        ("computeCommandEncoderWithDispatchType:", cb_computo_tipo as Imp, "@@:Q"),
        ("blitCommandEncoder", cb_blit as Imp, "@@:"),
        ("commit", cb_commit as Imp, "v@:"),
        ("enqueue", cb_enqueue as Imp, "v@:"),
        ("waitUntilCompleted", nada as Imp, "v@:"),
        ("waitUntilScheduled", nada as Imp, "v@:"),
        ("addCompletedHandler:", cb_handler_acabar as Imp, "v@:@?"),
        ("addScheduledHandler:", cb_handler_programar as Imp, "v@:@?"),
        ("status", cb_estado as Imp, "Q@:"),
        ("error", cb_error as Imp, "@@:"),
        ("commandQueue", cb_cola as Imp, "@@:"),
        ("GPUStartTime", cb_tiempo as Imp, "d@:"),
        ("GPUEndTime", cb_tiempo as Imp, "d@:"),
        ("kernelStartTime", cb_tiempo as Imp, "d@:"),
        ("kernelEndTime", cb_tiempo as Imp, "d@:"),
        ("pushDebugGroup:", nada1 as Imp, "v@:@"),
        ("popDebugGroup", nada as Imp, "v@:"),
    ];
    comunes(&mut cb);
    etiquetas!(cb, CommandBuffer);
    let mut computo: Vec<Metodo> = vec![
        ("setComputePipelineState:", comp_pipeline as Imp, "v@:@"),
        ("setBuffer:offset:atIndex:", comp_buffer as Imp, "v@:@QQ"),
        ("setBytes:length:atIndex:", comp_bytes as Imp, "v@:^vQQ"),
        ("setThreadgroupMemoryLength:atIndex:", comp_memoria as Imp, "v@:QQ"),
        ("dispatchThreadgroups:threadsPerThreadgroup:", comp_grupos as Imp, "v@:{MTLSize=QQQ}{MTLSize=QQQ}"),
        ("dispatchThreads:threadsPerThreadgroup:", comp_hilos as Imp, "v@:{MTLSize=QQQ}{MTLSize=QQQ}"),
        ("endEncoding", enc_fin as Imp, "v@:"),
        ("pushDebugGroup:", nada1 as Imp, "v@:@"),
        ("popDebugGroup", nada as Imp, "v@:"),
        ("insertDebugSignpost:", nada1 as Imp, "v@:@"),
    ];
    comunes(&mut computo);
    etiquetas!(computo, Encoder);
    let mut blit: Vec<Metodo> = vec![
        ("copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:", blit_copiar as Imp, "v@:@Q@QQ"),
        ("synchronizeResource:", nada1 as Imp, "v@:@"),
        ("endEncoding", enc_fin as Imp, "v@:"),
        ("pushDebugGroup:", nada1 as Imp, "v@:@"),
        ("popDebugGroup", nada as Imp, "v@:"),
    ];
    comunes(&mut blit);
    etiquetas!(blit, Encoder);
    let c = Clases {
        cola: nueva_clase("TahoeMTLCommandQueue", &["MTLCommandQueue"], &cola),
        buffer: nueva_clase("TahoeMTLBuffer", &["MTLBuffer"], &buffer),
        biblioteca: nueva_clase("TahoeMTLLibrary", &["MTLLibrary"], &biblioteca),
        funcion: nueva_clase("TahoeMTLFunction", &["MTLFunction"], &funcion),
        pipeline: nueva_clase("TahoeMTLComputePipelineState", &["MTLComputePipelineState"], &pipeline),
        cb: nueva_clase("TahoeMTLCommandBuffer", &["MTLCommandBuffer"], &cb),
        computo: nueva_clase("TahoeMTLComputeCommandEncoder", &["MTLComputeCommandEncoder"], &computo),
        blit: nueva_clase("TahoeMTLBlitCommandEncoder", &["MTLBlitCommandEncoder"], &blit),
    };
    let _ = CLASES.set(c);
}
