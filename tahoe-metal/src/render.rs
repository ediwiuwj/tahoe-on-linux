//! Texturas, samplers y render: MTLTexture (también sobre IOSurface), MTLSamplerState, MTLRenderPipelineState y
//! MTLRenderCommandEncoder, más las function constants (newFunctionWithName:constantValues:error:).
//!
//! Las function constants se guardan en un objeto de Metal (MTLFunctionConstantValuesInternal) que no deja leerlas:
//! sus tres métodos de dar valores se interceptan y los valores se apuntan aquí, por objeto, antes de seguir.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use crate::canal::{self, protocolo};
use crate::clases::{self, crear, estado, nueva_clase, nuevo_id, poner_error, retener, soltar, Clase, CommandBuffer, ConEtiqueta, Encoder, Funcion, Metodo};
use crate::objc::{self, Bool, Class, Id, Imp, Sel};
use crate::registro;
use protocolo::{clase, orden, tipo, Escritor};

extern "C" {
    fn dlopen(p: *const c_char, modo: c_int) -> *mut c_void;
    fn dlsym(h: *mut c_void, s: *const c_char) -> *mut c_void;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MTLRegion {
    pub x: u64,
    pub y: u64,
    pub z: u64,
    pub w: u64,
    pub h: u64,
    pub d: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MTLClearColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MTLViewport {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub zn: f64,
    pub zf: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MTLScissorRect {
    pub x: u64,
    pub y: u64,
    pub w: u64,
    pub h: u64,
}

pub struct ClasesRender {
    pub heap: Clase,
    pub fence: Clase,
    pub residencia: Clase,
    pub textura: Clase,
    pub sampler: Clase,
    pub pipeline: Clase,
    pub encoder: Clase,
    pub profundidad: Clase,
}

static CLASES: OnceLock<ClasesRender> = OnceLock::new();

fn clases_render() -> &'static ClasesRender {
    CLASES.get().expect("clases de render sin registrar")
}

macro_rules! con_etiqueta {
    ($t:ty) => {
        impl ConEtiqueta for $t {
            fn etiqueta(&mut self) -> &mut Id {
                &mut self.etiqueta
            }
        }
    };
}

fn pedir(t: u32, cuerpo: &[u8]) -> Result<Vec<u8>, String> {
    let g = canal::gpu_ya().ok_or("sin servidor de GPU")?;
    let mut c = g.canal.lock().map_err(|_| "canal envenenado")?;
    c.pedir(t, cuerpo).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ texturas

pub struct Textura {
    pub id: u32,
    tipo: u64,
    formato: u64,
    ancho: u64,
    alto: u64,
    prof: u64,
    niveles: u64,
    capas: u64,
    muestras: u64,
    uso: u64,
    almacen: u64,
    superficie: Id,
    plano: u64,
    etiqueta: Id,
}
con_etiqueta!(Textura);

impl Drop for Textura {
    fn drop(&mut self) {
        clases::liberar_en_servidor(clase::TEXTURA, self.id);
        if !self.superficie.is_null() {
            // SAFETY: IOSurface retenida al crear la textura (CFRetain/CFRelease son retain/release).
            unsafe { objc::m0::<()>(self.superficie, "release") };
        }
        soltar(self.etiqueta);
    }
}

/// Funciones de IOSurface.framework.
fn iosurface(nombre: &std::ffi::CStr) -> *mut c_void {
    // SAFETY: dlopen/dlsym de un framework del sistema.
    unsafe { dlsym(dlopen(c"/System/Library/Frameworks/IOSurface.framework/IOSurface".as_ptr(), 2), nombre.as_ptr()) }
}

fn nueva_textura(desc: Id, superficie: Id, plano: u64) -> Id {
    // SAFETY: desc es un MTLTextureDescriptor (propiedades del protocolo).
    let (tipo, formato, mut ancho, mut alto, prof, niveles, capas, muestras, uso, almacen): (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64) = unsafe {
        (
            objc::m0(desc, "textureType"),
            objc::m0(desc, "pixelFormat"),
            objc::m0(desc, "width"),
            objc::m0(desc, "height"),
            objc::m0(desc, "depth"),
            objc::m0(desc, "mipmapLevelCount"),
            objc::m0(desc, "arrayLength"),
            objc::m0(desc, "sampleCount"),
            objc::m0(desc, "usage"),
            objc::m0(desc, "storageMode"),
        )
    };
    let mut ruta = String::new();
    let mut bpr = 0u64;
    if !superficie.is_null() {
        // SAFETY: funciones de IOSurface con sus firmas.
        unsafe {
            let id: extern "C" fn(Id) -> u32 = std::mem::transmute(iosurface(c"IOSurfaceGetID"));
            let fila: extern "C" fn(Id) -> u64 = std::mem::transmute(iosurface(c"IOSurfaceGetBytesPerRow"));
            let w: extern "C" fn(Id) -> u64 = std::mem::transmute(iosurface(c"IOSurfaceGetWidth"));
            let h: extern "C" fn(Id) -> u64 = std::mem::transmute(iosurface(c"IOSurfaceGetHeight"));
            let plantilla = canal::gpu_ya().map(|g| g.ruta_superficies.clone()).unwrap_or_default();
            ruta = plantilla.replace("%u", &id(superficie).to_string());
            bpr = fila(superficie);
            if ancho == 0 {
                ancho = w(superficie);
            }
            if alto == 0 {
                alto = h(superficie);
            }
        }
    }
    let id = nuevo_id();
    let mut e = Escritor::default();
    e.u32(id).u32(tipo as u32).u32(formato as u32).u32(ancho as u32).u32(alto as u32).u32(prof.max(1) as u32).u32(niveles.max(1) as u32).u32(capas.max(1) as u32).u32(uso as u32).texto(&ruta).u64(bpr);
    if let Err(er) = pedir(tipo::TEXTURA, &e.0) {
        registro(&format!("textura {ancho}x{alto} formato {formato}: {er}"));
        return std::ptr::null_mut();
    }
    let superficie = retener(superficie);
    crear(&clases_render().textura, Textura { id, tipo, formato, ancho, alto, prof: prof.max(1), niveles: niveles.max(1), capas: capas.max(1), muestras: muestras.max(1), uso, almacen, superficie, plano, etiqueta: std::ptr::null_mut() })
}

extern "C-unwind" fn dev_textura(_this: Id, _cmd: Sel, desc: Id) -> Id {
    nueva_textura(desc, std::ptr::null_mut(), 0)
}

/// Una textura a partir de su descriptor (la usan el dispositivo y los heaps).
pub fn textura_de_descriptor(desc: Id) -> Id {
    nueva_textura(desc, std::ptr::null_mut(), 0)
}
extern "C-unwind" fn dev_textura_superficie(_this: Id, _cmd: Sel, desc: Id, sup: Id, plano: u64) -> Id {
    nueva_textura(desc, sup, plano)
}

macro_rules! getter {
    ($n:ident, $campo:ident) => {
        extern "C-unwind" fn $n(this: Id, _cmd: Sel) -> u64 {
            estado::<Textura>(this).map(|t| t.$campo).unwrap_or(0)
        }
    };
}
getter!(tex_tipo, tipo);
getter!(tex_formato, formato);
getter!(tex_ancho, ancho);
getter!(tex_alto, alto);
getter!(tex_prof, prof);
getter!(tex_niveles, niveles);
getter!(tex_capas, capas);
getter!(tex_muestras, muestras);
getter!(tex_uso, uso);
getter!(tex_almacen, almacen);
getter!(tex_plano, plano);

extern "C-unwind" fn tex_superficie(this: Id, _cmd: Sel) -> Id {
    estado::<Textura>(this).map(|t| t.superficie).unwrap_or(std::ptr::null_mut())
}
extern "C-unwind" fn tex_nil(_this: Id, _cmd: Sel) -> Id {
    std::ptr::null_mut()
}
extern "C-unwind" fn tex_no(_this: Id, _cmd: Sel) -> Bool {
    0
}

/// Bytes por píxel de los formatos de píxel más usados (MTLPixelFormat). Los que no están, 4.
fn bytes_por_pixel(formato: u64) -> u64 {
    match formato {
        1 => 1,      // A8Unorm
        10 => 1,     // R8Unorm
        12 => 1,     // R8Snorm
        13 => 1,     // R8Uint
        14 => 1,     // R8Sint
        20 => 2,     // R16Unorm
        25 => 2,     // R16Float
        30 => 2,     // RG8Unorm
        40 => 2,     // B5G6R5Unorm
        44 => 2,     // BGRA4Unorm
        53 => 4,     // R32Uint
        55 => 4,     // R32Float
        60 => 4,     // RG16Unorm
        70 => 4,     // RGBA8Unorm
        71 => 4,     // RGBA8Unorm_sRGB
        72 => 4,     // RGBA8Snorm
        80 => 4,     // BGRA8Unorm
        81 => 4,     // BGRA8Unorm_sRGB
        90 => 4,     // RGB10A2Unorm
        110 => 8,    // RGBA16Unorm
        125 => 16,   // RGBA32Float
        _ => 4,
    }
}

/// Los bytes que ocupa una textura, con todos sus niveles.
fn tamano_de_textura(t: &Textura) -> u64 {
    let bpp = bytes_por_pixel(t.formato);
    let mut total = 0u64;
    for nivel in 0..t.niveles.max(1) {
        let w = (t.ancho >> nivel).max(1);
        let h = (t.alto >> nivel).max(1);
        let d = (t.prof >> nivel).max(1);
        total += w * h * d * bpp;
    }
    total * t.capas.max(1)
}

/// allocatedSize: los bytes que ocupa la textura, con todos sus niveles.
fn tex_allocado(this: Id, _cmd: Sel) -> u64 {
    let Some(t) = estado::<Textura>(this) else { return 0 };
    tamano_de_textura(t)
}

/// Los bytes que ocupa un recurso (buffer o textura) de un conjunto de residencia.
fn tamano_de_recurso(o: Id) -> u64 {
    if let Some(b) = estado::<clases::Buffer>(o) {
        return b.len as u64;
    }
    if let Some(t) = estado::<Textura>(o) {
        return tamano_de_textura(t);
    }
    0
}

/// setPurgeableState: (MTLPurgeableState): la memoria la tiene el servidor de GPU; se acepta y sigue ahí.
extern "C-unwind" fn tex_purgeable(_this: Id, _cmd: Sel, _estado: u64) -> Bool {
    1
}

fn subir(this: Id, r: MTLRegion, nivel: u64, capa: u64, p: *const u8, bpr: u64, bpi: u64) {
    let Some(t) = estado::<Textura>(this) else { return };
    let tam = if r.d > 1 { bpi * r.d } else { bpr * r.h.max(1) } as usize;
    // SAFETY: Metal garantiza los bytes de la región en p.
    let datos = unsafe { std::slice::from_raw_parts(p, tam) };
    let mut e = Escritor::default();
    e.u32(t.id).u32(nivel as u32).u32(capa as u32).u32(r.x as u32).u32(r.y as u32).u32(r.z as u32).u32(r.w as u32).u32(r.h as u32).u32(r.d.max(1) as u32).u32(bpr as u32).bytes(datos);
    if let Err(er) = pedir(tipo::TEXTURA_SUBIR, &e.0) {
        registro(&format!("replaceRegion: {er}"));
    }
}
extern "C-unwind" fn tex_reemplazar(this: Id, _cmd: Sel, r: MTLRegion, nivel: u64, p: *const u8, bpr: u64) {
    subir(this, r, nivel, 0, p, bpr, bpr * r.h);
}
extern "C-unwind" fn tex_reemplazar_capa(this: Id, _cmd: Sel, r: MTLRegion, nivel: u64, capa: u64, p: *const u8, bpr: u64, bpi: u64) {
    subir(this, r, nivel, capa, p, bpr, bpi);
}
fn bajar(this: Id, p: *mut u8, bpr: u64, r: MTLRegion, nivel: u64, capa: u64) {
    let Some(t) = estado::<Textura>(this) else { return };
    let mut e = Escritor::default();
    e.u32(t.id).u32(nivel as u32).u32(capa as u32).u32(r.x as u32).u32(r.y as u32).u32(r.w as u32).u32(r.h as u32).u32(bpr as u32);
    match pedir(tipo::TEXTURA_BAJAR, &e.0) {
        Ok(v) => {
            let mut l = protocolo::Lector::new(&v);
            if let Ok(b) = l.bytes() {
                // SAFETY: el destino tiene bpr × alto bytes.
                unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len().min((bpr * r.h) as usize)) };
            }
        }
        Err(er) => registro(&format!("getBytes: {er}")),
    }
}
extern "C-unwind" fn tex_obtener(this: Id, _cmd: Sel, p: *mut u8, bpr: u64, r: MTLRegion, nivel: u64) {
    bajar(this, p, bpr, r, nivel, 0);
}
extern "C-unwind" fn tex_obtener_capa(this: Id, _cmd: Sel, p: *mut u8, bpr: u64, _bpi: u64, r: MTLRegion, nivel: u64, capa: u64) {
    bajar(this, p, bpr, r, nivel, capa);
}

// ------------------------------------------------------------------ estado de profundidad y esténcil

/// Lee una propiedad de un descriptor solo si el objeto la implementa.
///
/// En macOS 26 los descriptores no son los de iOS: `MTLDepthStencilDescriptor` expone `isDepthWriteEnabled`
/// (no `depthWriteEnabled`) y lleva el estencil en `frontFaceStencil`/`backFaceStencil` (objetos
/// `MTLStencilDescriptor`). Preguntar por una propiedad inexistente lanza `NSException` y desde Rust eso aborta
/// el proceso («Rust cannot catch foreign exceptions»): no se puede capturar. Por eso toda lectura de descriptor
/// pasa por aquí y usa el valor por defecto cuando el selector no existe.
pub fn campo<T: Copy>(o: Id, nombre: &str, por_defecto: T) -> T {
    if o.is_null() || !objc::responde_instancia(o, nombre) {
        return por_defecto;
    }
    // SAFETY: el objeto implementa el selector y el tipo es el que declara el descriptor.
    unsafe { objc::m0::<T>(o, nombre) }
}

/// El estado de profundidad y esténcil de un pipeline (MTLDepthStencilState). Lo crea el compositor para sus pipelines.
pub struct Profundidad {
    comparacion: u64,
    escritura: Bool,
    formato_profundidad: u64,
    comparacion_estencil: u64,
    mascara_estencil: u64,
    mascara_estencil_delante: u64,
    mascara_estencil_detras: u64,
    referencia_estencil_delante: u64,
    referencia_estencil_detras: u64,
    formato_estencil: u64,
    etiqueta: Id,
}
con_etiqueta!(Profundidad);

extern "C-unwind" fn prof_comparacion(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.comparacion).unwrap_or(0)
}
extern "C-unwind" fn prof_escritura(this: Id, _cmd: Sel) -> Bool {
    estado::<Profundidad>(this).map(|p| p.escritura).unwrap_or(0)
}
extern "C-unwind" fn prof_formato_profundidad(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.formato_profundidad).unwrap_or(0)
}
extern "C-unwind" fn prof_comparacion_estencil(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.comparacion_estencil).unwrap_or(0)
}
extern "C-unwind" fn prof_mascara_estencil(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.mascara_estencil).unwrap_or(0)
}
extern "C-unwind" fn prof_mascara_delante(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.mascara_estencil_delante).unwrap_or(0)
}
extern "C-unwind" fn prof_mascara_detras(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.mascara_estencil_detras).unwrap_or(0)
}
extern "C-unwind" fn prof_referencia_delante(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.referencia_estencil_delante).unwrap_or(0)
}
extern "C-unwind" fn prof_referencia_detras(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.referencia_estencil_detras).unwrap_or(0)
}
extern "C-unwind" fn prof_formato_estencil(this: Id, _cmd: Sel) -> u64 {
    estado::<Profundidad>(this).map(|p| p.formato_estencil).unwrap_or(0)
}

/// newDepthStencilStateWithDescriptor: (MTLDepthStencilDescriptor): el estado de profundidad y esténcil del pipeline.
extern "C-unwind" fn dev_profundidad(_this: Id, _cmd: Sel, d: Id) -> Id {
    // macOS 26: la escritura de profundidad es `isDepthWriteEnabled` y el estencil vive en
    // `frontFaceStencil`/`backFaceStencil`; `depthWriteEnabled`, `stencilWriteMask*`, `stencilReferenceValue*` y
    // `stencilPixelFormat` no existen en este descriptor. Se pregunta antes de leer (ver `campo`).
    let comparacion = campo(d, "depthCompareFunction", 0u64);
    let escritura = if objc::responde_instancia(d, "isDepthWriteEnabled") {
        campo(d, "isDepthWriteEnabled", 0 as Bool)
    } else {
        campo(d, "depthWriteEnabled", 0 as Bool)
    };
    let formato_profundidad = campo(d, "depthAttachmentPixelFormat", 0u64);
    let comparacion_estencil = campo(d, "stencilCompareFunction", 0u64);
    let frente: Id = campo(d, "frontFaceStencil", std::ptr::null_mut());
    let detras: Id = campo(d, "backFaceStencil", std::ptr::null_mut());
    let mascara_estencil = campo(d, "stencilWriteMask", 0u64);
    let mascara_delante = if frente.is_null() {
        campo(d, "stencilWriteMaskFront", 0u64)
    } else {
        campo(frente, "stencilWriteMask", 0u64)
    };
    let mascara_detras = if detras.is_null() {
        campo(d, "stencilWriteMaskBack", 0u64)
    } else {
        campo(detras, "stencilWriteMask", 0u64)
    };
    let referencia_delante = if frente.is_null() {
        campo(d, "stencilReferenceValueFront", 0i64) as u64
    } else {
        campo(frente, "stencilReferenceValue", 0i64) as u64
    };
    let referencia_detras = if detras.is_null() {
        campo(d, "stencilReferenceValueBack", 0i64) as u64
    } else {
        campo(detras, "stencilReferenceValue", 0i64) as u64
    };
    let formato_estencil = campo(d, "stencilPixelFormat", 0u64);
    crear(
        &clases_render().profundidad,
        Profundidad {
            comparacion,
            escritura,
            formato_profundidad,
            comparacion_estencil,
            mascara_estencil,
            mascara_estencil_delante: mascara_delante,
            mascara_estencil_detras: mascara_detras,
            referencia_estencil_delante: referencia_delante,
            referencia_estencil_detras: referencia_detras,
            formato_estencil,
            etiqueta: std::ptr::null_mut(),
        },
    )
}

// ------------------------------------------------------------------ samplers

pub struct Sampler {
    pub id: u32,
    etiqueta: Id,
}
con_etiqueta!(Sampler);

impl Drop for Sampler {
    fn drop(&mut self) {
        clases::liberar_en_servidor(clase::SAMPLER, self.id);
        soltar(self.etiqueta);
    }
}

extern "C-unwind" fn dev_sampler(_this: Id, _cmd: Sel, d: Id) -> Id {
    let id = nuevo_id();
    let mut e = Escritor::default();
    // SAFETY: d es un MTLSamplerDescriptor.
    unsafe {
        let u = |s: &str| -> u32 { campo(d, s, 0u64) as u32 };
        e.u32(id).u32(u("minFilter")).u32(u("magFilter")).u32(u("mipFilter")).u32(u("sAddressMode")).u32(u("tAddressMode")).u32(u("rAddressMode"));
        let norm = if objc::responde_instancia(d, "normalizedCoordinates") {
            campo(d, "normalizedCoordinates", 0 as Bool) as u32
        } else {
            campo(d, "isNormalizedCoordinates", 0 as Bool) as u32
        };
        e.u32(norm).u32(u("maxAnisotropy"));
        let lmin = if objc::responde_instancia(d, "lodMinClamp") { campo(d, "lodMinClamp", 0f32) } else { campo(d, "minLod", 0f32) };
        let lmax = if objc::responde_instancia(d, "lodMaxClamp") { campo(d, "lodMaxClamp", 0f32) } else { campo(d, "maxLod", 0f32) };
        e.u32(lmin.to_bits()).u32(lmax.to_bits());
        e.u32(u("compareFunction")).u32(u("borderColor"));
    }
    if let Err(er) = pedir(tipo::SAMPLER, &e.0) {
        registro(&format!("sampler: {er}"));
        return std::ptr::null_mut();
    }
    crear(&clases_render().sampler, Sampler { id, etiqueta: std::ptr::null_mut() })
}

// ------------------------------------------------------------------ function constants

/// Valores dados a cada MTLFunctionConstantValues (por la dirección del objeto): índice (u32::MAX: por nombre), nombre,
/// MTLDataType y bytes.
type Constantes = Vec<(u32, String, u32, Vec<u8>)>;
static CONSTANTES: Mutex<Option<HashMap<usize, Constantes>>> = Mutex::new(None);
static ORIGINALES: OnceLock<[usize; 4]> = OnceLock::new();

/// Bytes de un valor de MTLDataType.
fn tam_tipo(t: u64) -> usize {
    match t {
        3..=6 => 4 * (t - 2) as usize,
        16..=19 => 2 * (t - 15) as usize,
        29..=36 => 4 * ((t - 29) % 4 + 1) as usize,
        37..=44 => 2 * ((t - 37) % 4 + 1) as usize,
        45..=56 => ((t - 45) % 4 + 1) as usize,
        81..=88 => 8,
        _ => 4,
    }
}

fn apuntar(obj: Id, indice: u32, nombre: String, tipo_d: u64, p: *const u8) {
    // SAFETY: Metal pasa un valor de ese tipo en p.
    let b = unsafe { std::slice::from_raw_parts(p, tam_tipo(tipo_d)) }.to_vec();
    registro(&format!("constante: índice {indice} nombre «{nombre}» tipo {tipo_d} bytes {}", b.len()));
    if let Ok(mut g) = CONSTANTES.lock() {
        let v = g.get_or_insert_with(HashMap::new).entry(obj as usize).or_default();
        v.retain(|x| !(x.0 == indice && x.1 == nombre));
        v.push((indice, nombre, tipo_d as u32, b));
    }
}

extern "C-unwind" fn fc_indice(this: Id, cmd: Sel, p: *const u8, t: u64, i: u64) {
    apuntar(this, i as u32, String::new(), t, p);
    // SAFETY: la implementación original de Metal con la misma firma.
    unsafe {
        let f: extern "C-unwind" fn(Id, Sel, *const u8, u64, u64) = std::mem::transmute(ORIGINALES.get().unwrap()[0]);
        f(this, cmd, p, t, i)
    }
}
extern "C-unwind" fn fc_nombre(this: Id, cmd: Sel, p: *const u8, t: u64, n: Id) {
    apuntar(this, u32::MAX, objc::texto(n), t, p);
    // SAFETY: ver fc_indice.
    unsafe {
        let f: extern "C-unwind" fn(Id, Sel, *const u8, u64, Id) = std::mem::transmute(ORIGINALES.get().unwrap()[1]);
        f(this, cmd, p, t, n)
    }
}
/// setConstantValues: (MTLFunctionDescriptor): Metal copia los valores a un objeto interno; los nuestros se copian
/// al objeto copiado, que es el que luego lee el driver al crear la función.
extern "C-unwind" fn fd_constantes(this: Id, cmd: Sel, valores: Id) {
    let orig = ORIGINALES.get().map(|o| o[3]).unwrap_or(0);
    if orig == 0 {
        return;
    }
    // SAFETY: el método original de Metal con la misma firma.
    unsafe {
        let f: extern "C-unwind" fn(Id, Sel, Id) = std::mem::transmute(orig);
        f(this, cmd, valores);
    }
    // SAFETY: el descriptor es un MTLFunctionDescriptor.
    let destino: Id = unsafe { objc::m0(this, "constantValues") };
    if destino.is_null() || valores.is_null() {
        return;
    }
    let v = CONSTANTES.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&(valores as usize)).cloned()));
    if let Some(v) = v {
        let n = v.len();
        if let Ok(mut g) = CONSTANTES.lock() {
            g.get_or_insert_with(HashMap::new).insert(destino as usize, v);
        }
        registro(&format!("constantes: {n} valores copiados al objeto del descriptor"));
    }
}

extern "C-unwind" fn fc_rango(this: Id, cmd: Sel, p: *const u8, t: u64, r: clases::NSRange) {
    let paso = tam_tipo(t);
    for k in 0..r.len {
        // SAFETY: r.len valores consecutivos.
        apuntar(this, (r.loc + k) as u32, String::new(), t, unsafe { p.add(k as usize * paso) });
    }
    // SAFETY: ver fc_indice.
    unsafe {
        let f: extern "C-unwind" fn(Id, Sel, *const u8, u64, clases::NSRange) = std::mem::transmute(ORIGINALES.get().unwrap()[2]);
        f(this, cmd, p, t, r)
    }
}

fn interceptar_constantes() {
    let cls = objc::clase("MTLFunctionConstantValuesInternal");
    if cls.is_null() {
        registro("no está MTLFunctionConstantValuesInternal: sin function constants");
        return;
    }
    // SAFETY: métodos existentes de esa clase; las nuevas implementaciones llaman a las originales.
    unsafe {
        let m = |s: &str| objc::class_getInstanceMethod(cls, objc::sel(s));
        let (a, b, c) = (m("setConstantValue:type:atIndex:"), m("setConstantValue:type:withName:"), m("setConstantValues:type:withRange:"));
        if a.is_null() || b.is_null() || c.is_null() {
            registro("MTLFunctionConstantValuesInternal sin los métodos esperados");
            return;
        }
        // También el descriptor: al poner los valores, Metal los copia a un objeto interno suyo, y esa copia no
        // pasa por los métodos interceptados. Se copia lo nuestro al objeto copiado.
        let d = objc::clase("MTLFunctionDescriptor");
        let md = if d.is_null() { std::ptr::null_mut() } else { objc::class_getInstanceMethod(d, objc::sel("setConstantValues:")) };
        // El OnceLock solo se puede fijar una vez: las cuatro implementaciones originales seGuardan juntas.
        let _ = ORIGINALES.set([
            objc::method_getImplementation(a) as usize,
            objc::method_getImplementation(b) as usize,
            objc::method_getImplementation(c) as usize,
            if md.is_null() { 0 } else { objc::method_getImplementation(md) as usize },
        ]);
        objc::method_setImplementation(a, fc_indice as Imp);
        objc::method_setImplementation(b, fc_nombre as Imp);
        objc::method_setImplementation(c, fc_rango as Imp);
        if !md.is_null() {
            objc::method_setImplementation(md, fd_constantes as Imp);
        }
    }
}

/// Las constantes de un MTLFunctionConstantValues, serializadas como las pide el protocolo.
fn serializar_constantes(valores: Id) -> Vec<u8> {
    let v = CONSTANTES.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&(valores as usize)).cloned())).unwrap_or_default();
    registro(&format!("constantes: {} valores para {:p}", v.len(), valores));
    let mut e = Escritor::default();
    e.u32(v.len() as u32);
    for (i, n, t, b) in v {
        e.u32(i);
        if i == u32::MAX {
            e.texto(&n);
        }
        e.u32(t).bytes(&b);
    }
    e.0
}

extern "C-unwind" fn bib_funcion_constantes(this: Id, cmd: Sel, nombre: Id, valores: Id, error: *mut Id) -> Id {
    // SAFETY: newFunctionWithName: de nuestra biblioteca.
    let f: Id = unsafe { objc::m1(this, "newFunctionWithName:", nombre) };
    let _ = cmd;
    if f.is_null() {
        poner_error(error, &format!("la biblioteca no tiene «{}»", objc::texto(nombre)));
        return f;
    }
    if let Some(fs) = estado::<Funcion>(f) {
        fs.constantes = serializar_constantes(valores);
    }
    f
}

/// newFunctionWithDescriptor:error: (MTLFunctionDescriptor): la función por su descriptor, que lleva el nombre y las
/// function constants. Es la vía que usa el compositor (CoreAnimation) para cargar sus shaders.
extern "C-unwind" fn bib_funcion_descriptor(this: Id, _cmd: Sel, desc: Id, error: *mut Id) -> Id {
    // SAFETY: desc es un MTLFunctionDescriptor del protocolo: tiene «functionName» y «constantValues».
    let (nombre, valores): (Id, Id) = unsafe {
        let tiene = |s: &str| objc::m1::<Sel, Bool>(desc, "respondsToSelector:", objc::sel(s)) != 0;
        // En macOS 26 el descriptor lleva el nombre en «name» (en las versiones anteriores, «functionName»).
        let n: Id = if tiene("name") {
            objc::m0(desc, "name")
        } else if tiene("functionName") {
            objc::m0(desc, "functionName")
        } else {
            std::ptr::null_mut()
        };
        let v: Id = if tiene("constantValues") { objc::m0(desc, "constantValues") } else { std::ptr::null_mut() };
        registro(&format!(
            "newFunctionWithDescriptor: clase={} name={} constantValues={} nombre={}",
            objc::nombre_clase(objc::object_getClass(desc)),
            tiene("name"),
            tiene("constantValues"),
            if n.is_null() { "(nil)".to_string() } else { objc::texto(n) }
        ));
        (if n.is_null() { objc::nsstring("") } else { n }, v)
    };
    // SAFETY: newFunctionWithName: de nuestra biblioteca.
    let f: Id = unsafe { objc::m1(this, "newFunctionWithName:", nombre) };
    if f.is_null() {
        poner_error(error, &format!("la biblioteca no tiene «{}»", objc::texto(nombre)));
        return f;
    }
    if !valores.is_null() {
        if let Some(fs) = estado::<Funcion>(f) {
            fs.constantes = serializar_constantes(valores);
        }
    }
    f
}

// ------------------------------------------------------------------ pipeline de render

pub struct PipelineRender {
    pub id: u32,
    etiqueta: Id,
}
con_etiqueta!(PipelineRender);

impl Drop for PipelineRender {
    fn drop(&mut self) {
        clases::liberar_en_servidor(clase::PIPELINE_RENDER, self.id);
        soltar(self.etiqueta);
    }
}

/// Biblioteca, nombre y constantes de una función (o ceros si es nil).
fn funcion(f: Id, e: &mut Escritor) {
    match estado::<Funcion>(f) {
        Some(fs) => {
            let bib = estado::<clases::Biblioteca>(fs.biblioteca).map(|b| b.id).unwrap_or(0);
            e.u32(bib).texto(&fs.nombre);
            if fs.constantes.is_empty() {
                e.u32(0);
            } else {
                e.0.extend_from_slice(&fs.constantes);
            }
        }
        None => {
            e.u32(0).texto("").u32(0);
        }
    }
}

pub fn nuevo_pipeline_render(d: Id, error: *mut Id) -> Id {
    let id = nuevo_id();
    let mut e = Escritor::default();
    e.u32(id);
    // SAFETY: d es un MTLRenderPipelineDescriptor; sus subobjetos, los del protocolo.
    unsafe {
        funcion(campo(d, "vertexFunction", std::ptr::null_mut()), &mut e);
        funcion(campo(d, "fragmentFunction", std::ptr::null_mut()), &mut e);
        // atributos y layouts del vertex descriptor
        let vd: Id = campo(d, "vertexDescriptor", std::ptr::null_mut());
        let mut atributos = Vec::new();
        let mut layouts = Vec::new();
        if !vd.is_null() {
            let attrs: Id = campo(vd, "attributes", std::ptr::null_mut());
            for i in 0..31u64 {
                let a: Id = objc::m1(attrs, "objectAtIndexedSubscript:", i);
                let f: u64 = campo(a, "format", 0u64);
                if f != 0 {
                    atributos.push((i as u32, f as u32, campo(a, "offset", 0u64) as u32, campo(a, "bufferIndex", 0u64) as u32));
                }
            }
            let ls: Id = campo(vd, "layouts", std::ptr::null_mut());
            let mut usados: Vec<u32> = atributos.iter().map(|a| a.3).collect();
            usados.sort();
            usados.dedup();
            for b in usados {
                let lo: Id = objc::m1(ls, "objectAtIndexedSubscript:", b as u64);
                layouts.push((b, campo(lo, "stride", 0u64) as u32, campo(lo, "stepFunction", 0u64) as u32));
            }
        }
        e.u32(atributos.len() as u32);
        for (a, f, o, b) in atributos {
            e.u32(a).u32(f).u32(o).u32(b);
        }
        e.u32(layouts.len() as u32);
        for (b, s, f) in layouts {
            e.u32(b).u32(s).u32(f);
        }
        // colores: hasta el último con formato
        let cas: Id = campo(d, "colorAttachments", std::ptr::null_mut());
        let mut colores = Vec::new();
        for i in 0..8u64 {
            let c: Id = objc::m1(cas, "objectAtIndexedSubscript:", i);
            let f: u64 = campo(c, "pixelFormat", 0u64);
            let u = |s: &str| campo(c, s, 0u64) as u32;
            colores.push((
                f as u32,
                campo(c, "isBlendingEnabled", 0 as Bool) as u32,
                [u("sourceRGBBlendFactor"), u("destinationRGBBlendFactor"), u("rgbBlendOperation"), u("sourceAlphaBlendFactor"), u("destinationAlphaBlendFactor"), u("alphaBlendOperation")],
                u("writeMask"),
            ));
        }
        while colores.last().is_some_and(|c| c.0 == 0) {
            colores.pop();
        }
        e.u32(colores.len() as u32);
        for (f, act, fx, m) in colores {
            e.u32(f).u32(act);
            for x in fx {
                e.u32(x);
            }
            e.u32(m);
        }
        e.u32(campo(d, "depthAttachmentPixelFormat", 0u64) as u32).u32(campo(d, "inputPrimitiveTopology", 0u64) as u32);
    }
    match pedir(tipo::PIPELINE_RENDER, &e.0) {
        Ok(_) => {
            registro("pipeline de render: creado");
            crear(&clases_render().pipeline, PipelineRender { id, etiqueta: std::ptr::null_mut() })
        }
        Err(er) => {
            registro(&format!("pipeline de render: {er}"));
            poner_error(error, &format!("pipeline de render: {er}"));
            std::ptr::null_mut()
        }
    }
}

extern "C-unwind" fn dev_pipeline_render(_this: Id, _cmd: Sel, d: Id, error: *mut Id) -> Id {
    nuevo_pipeline_render(d, error)
}
extern "C-unwind" fn dev_pipeline_render_opciones(_this: Id, _cmd: Sel, d: Id, _opciones: u64, reflexion: *mut Id, error: *mut Id) -> Id {
    if !reflexion.is_null() {
        // SAFETY: parámetro de salida.
        unsafe { *reflexion = std::ptr::null_mut() };
    }
    nuevo_pipeline_render(d, error)
}

// ------------------------------------------------------------------ encoder de render

fn ordenes(enc: Id) -> Option<&'static mut Escritor> {
    clases::ordenes(enc)
}

extern "C-unwind" fn cb_render(this: Id, _cmd: Sel, d: Id) -> Id {
    let Some(c) = estado::<CommandBuffer>(this) else { return std::ptr::null_mut() };
    let _ = c;
    let mut colores = Vec::new();
    // SAFETY: d es un MTLRenderPassDescriptor.
    unsafe {
        let cas: Id = objc::m0(d, "colorAttachments");
        for i in 0..8u64 {
            let a: Id = objc::m1(cas, "objectAtIndexedSubscript:", i);
            let t: Id = objc::m0(a, "texture");
            let Some(tx) = estado::<Textura>(t) else { continue };
            let cc: MTLClearColor = objc::m0_stret(a, "clearColor");
            colores.push((tx.id, objc::m0::<u64>(a, "level") as u32, objc::m0::<u64>(a, "slice") as u32, objc::m0::<u64>(a, "loadAction") as u32, objc::m0::<u64>(a, "storeAction") as u32, [cc.r as f32, cc.g as f32, cc.b as f32, cc.a as f32]));
        }
    }
    let enc = crear(&clases_render().encoder, Encoder { cb: retener(this), etiqueta: std::ptr::null_mut() });
    if let Some(o) = ordenes(enc) {
        o.u32(orden::PASE).u32(colores.len() as u32);
        for (t, nivel, capa, carga, guardar, col) in colores {
            o.u32(t).u32(nivel).u32(capa).u32(carga).u32((guardar != 0) as u32);
            for x in col {
                o.u32(x.to_bits());
            }
        }
    }
    // SAFETY: objeto recién creado.
    unsafe { objc::m0(enc, "autorelease") }
}

/// updateFence: / waitForFence: (MTLCommandEncoder): aquí no hay planificador de citas y el commit es síncrono, así
/// que el fence no sincroniza nada.
extern "C-unwind" fn ren_fence(_this: Id, _cmd: Sel, _fence: Id) {}

extern "C-unwind" fn ren_pipeline(this: Id, _cmd: Sel, p: Id) {
    if let (Some(o), Some(pl)) = (ordenes(this), estado::<PipelineRender>(p)) {
        o.u32(orden::PIPELINE_RENDER).u32(pl.id);
    }
}
fn buffer_etapa(this: Id, etapa: u32, b: Id, off: u64, idx: u64) {
    if let (Some(o), Some(bu)) = (ordenes(this), estado::<clases::Buffer>(b)) {
        o.u32(orden::BUFFER_ETAPA).u32(etapa).u32(idx as u32).u32(bu.id).u64(off);
    }
}
fn bytes_etapa(this: Id, etapa: u32, p: *const u8, len: u64, idx: u64) {
    if let Some(o) = ordenes(this) {
        // SAFETY: Metal garantiza len bytes en p.
        let d = unsafe { std::slice::from_raw_parts(p, len as usize) };
        o.u32(orden::BYTES_ETAPA).u32(etapa).u32(idx as u32).bytes(d);
    }
}
fn textura_etapa(this: Id, etapa: u32, t: Id, idx: u64) {
    if let (Some(o), Some(tx)) = (ordenes(this), estado::<Textura>(t)) {
        o.u32(orden::TEXTURA_ETAPA).u32(etapa).u32(idx as u32).u32(tx.id);
    }
}
fn sampler_etapa(this: Id, etapa: u32, s: Id, idx: u64) {
    if let (Some(o), Some(sm)) = (ordenes(this), estado::<Sampler>(s)) {
        o.u32(orden::SAMPLER_ETAPA).u32(etapa).u32(idx as u32).u32(sm.id);
    }
}
extern "C-unwind" fn ren_vbuffer(this: Id, _cmd: Sel, b: Id, off: u64, i: u64) {
    buffer_etapa(this, 0, b, off, i);
}
extern "C-unwind" fn ren_fbuffer(this: Id, _cmd: Sel, b: Id, off: u64, i: u64) {
    buffer_etapa(this, 1, b, off, i);
}
extern "C-unwind" fn ren_vbytes(this: Id, _cmd: Sel, p: *const u8, n: u64, i: u64) {
    bytes_etapa(this, 0, p, n, i);
}
extern "C-unwind" fn ren_fbytes(this: Id, _cmd: Sel, p: *const u8, n: u64, i: u64) {
    bytes_etapa(this, 1, p, n, i);
}
extern "C-unwind" fn ren_vtextura(this: Id, _cmd: Sel, t: Id, i: u64) {
    textura_etapa(this, 0, t, i);
}
extern "C-unwind" fn ren_ftextura(this: Id, _cmd: Sel, t: Id, i: u64) {
    textura_etapa(this, 1, t, i);
}
extern "C-unwind" fn ren_vsampler(this: Id, _cmd: Sel, s: Id, i: u64) {
    sampler_etapa(this, 0, s, i);
}
extern "C-unwind" fn ren_fsampler(this: Id, _cmd: Sel, s: Id, i: u64) {
    sampler_etapa(this, 1, s, i);
}
extern "C-unwind" fn ren_viewport(this: Id, _cmd: Sel, v: MTLViewport) {
    if let Some(o) = ordenes(this) {
        o.u32(orden::VIEWPORT);
        for x in [v.x, v.y, v.w, v.h, v.zn, v.zf] {
            o.u32((x as f32).to_bits());
        }
    }
}
extern "C-unwind" fn ren_tijera(this: Id, _cmd: Sel, r: MTLScissorRect) {
    if let Some(o) = ordenes(this) {
        o.u32(orden::TIJERA).u32(r.x as u32).u32(r.y as u32).u32(r.w as u32).u32(r.h as u32);
    }
}
static CULL: Mutex<(u32, u32)> = Mutex::new((0, 0));
extern "C-unwind" fn ren_cull(this: Id, _cmd: Sel, c: u64) {
    let w = CULL.lock().map(|g| g.1).unwrap_or(0);
    if let Some(o) = ordenes(this) {
        o.u32(orden::CULL).u32(c as u32).u32(w);
    }
}
extern "C-unwind" fn ren_sentido(_this: Id, _cmd: Sel, w: u64) {
    if let Ok(mut g) = CULL.lock() {
        g.1 = w as u32;
    }
}
fn dibujar(this: Id, prim: u64, primero: u64, cuenta: u64, inst: u64, pinst: u64) {
    // Se anota el tipo de primitiva: es lo que decide la topología en el servidor, y sirve para ver qué usa
    // el compositor de verdad.
    registro(&format!("dibujo: primitiva {prim}, desde {primero}, {cuenta} vértices"));
    if let Some(o) = ordenes(this) {
        o.u32(orden::DIBUJAR).u32(prim as u32).u32(primero as u32).u32(cuenta as u32).u32(inst as u32).u32(pinst as u32);
    }
}
extern "C-unwind" fn ren_dibujar(this: Id, _cmd: Sel, p: u64, primero: u64, cuenta: u64) {
    dibujar(this, p, primero, cuenta, 1, 0);
}
extern "C-unwind" fn ren_dibujar_inst(this: Id, _cmd: Sel, p: u64, primero: u64, cuenta: u64, inst: u64) {
    dibujar(this, p, primero, cuenta, inst, 0);
}
extern "C-unwind" fn ren_dibujar_inst_base(this: Id, _cmd: Sel, p: u64, primero: u64, cuenta: u64, inst: u64, base: u64) {
    dibujar(this, p, primero, cuenta, inst, base);
}
#[allow(clippy::too_many_arguments)]
fn dibujar_indices(this: Id, prim: u64, cuenta: u64, tipo_i: u64, b: Id, off: u64, inst: u64, base: i64, pinst: u64) {
    if let (Some(o), Some(bu)) = (ordenes(this), estado::<clases::Buffer>(b)) {
        o.u32(orden::DIBUJAR_INDICES).u32(prim as u32).u32(cuenta as u32).u32(tipo_i as u32).u32(bu.id).u64(off).u32(inst as u32).u32(base as i32 as u32).u32(pinst as u32);
    }
}
extern "C-unwind" fn ren_indices(this: Id, _cmd: Sel, p: u64, n: u64, t: u64, b: Id, off: u64) {
    dibujar_indices(this, p, n, t, b, off, 1, 0, 0);
}
extern "C-unwind" fn ren_indices_inst(this: Id, _cmd: Sel, p: u64, n: u64, t: u64, b: Id, off: u64, inst: u64) {
    dibujar_indices(this, p, n, t, b, off, inst, 0, 0);
}
#[allow(clippy::too_many_arguments)]
extern "C-unwind" fn ren_indices_inst_base(this: Id, _cmd: Sel, p: u64, n: u64, t: u64, b: Id, off: u64, inst: u64, base: i64, pinst: u64) {
    dibujar_indices(this, p, n, t, b, off, inst, base, pinst);
}
extern "C-unwind" fn ren_fin(this: Id, _cmd: Sel) {
    if let Some(o) = ordenes(this) {
        o.u32(orden::FIN_PASE);
    }
}
extern "C-unwind" fn ren_nada_u(_this: Id, _cmd: Sel, _x: u64) {}
extern "C-unwind" fn ren_nada_o(_this: Id, _cmd: Sel, _x: Id) {}
extern "C-unwind" fn ren_color(_this: Id, _cmd: Sel, _r: f32, _g: f32, _b: f32, _a: f32) {}

// ------------------------------------------------------------------ registro

/// Tamaño y alineación de un recurso dentro de un heap (MTLSizeAndAlign): vuelve en rax:rdx.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TamanoAlineacion {
    pub tamano: u64,
    pub alineacion: u64,
}

/// La alineación que se supone a los recursos de un heap: 256 bytes, como en los heaps de Apple.
const ALINEACION_HEAP: u64 = 256;

/// heapBufferSizeAndAlignWithLength:options: (MTLDevice): MPS lo llama 7 veces (MPSAutoCache, MPSAutoBuffer,
/// GetResourceSize) y usa el valor para trocear memoria. No hace falta tocar el servidor de GPU: se devuelve el
/// tamaño redondeado a la alineación y la alineación.
extern "C-unwind" fn tamano_buffer_heap(_this: Id, _cmd: Sel, len: u64, _opciones: u64) -> TamanoAlineacion {
    TamanoAlineacion { tamano: len.div_ceil(ALINEACION_HEAP) * ALINEACION_HEAP, alineacion: ALINEACION_HEAP }
}

/// heapTextureSizeAndAlignWithDescriptor: (MTLDevice): lo usa MPSDevice_MacOS::SupportsPlugin. El tamaño es el
/// de la textura entera (todos los niveles) y la alineación, 256.
extern "C-unwind" fn tamano_textura_heap(_this: Id, _cmd: Sel, d: Id) -> TamanoAlineacion {
    let (ancho, alto, prof, capas, niveles, formato) = (
        campo(d, "width", 0u64),
        campo(d, "height", 0u64),
        campo(d, "depth", 0u64),
        campo(d, "arrayLength", 0u64),
        campo(d, "mipmapLevelCount", 0u64),
        campo(d, "pixelFormat", 0u64),
    );
    let muestras = campo(d, "sampleCount", 1u64).max(1);
    let bpp = bytes_por_pixel(formato);
    let mut total = 0u64;
    for nivel in 0..niveles.max(1) {
        let w = (ancho >> nivel).max(1);
        let h = (alto >> nivel).max(1);
        let p = (prof >> nivel).max(1);
        // La fila va alineada, como en el hardware.
        let fila = (w * bpp).next_multiple_of(ALINEACION_HEAP);
        total += fila * h * p;
    }
    total *= capas.max(1) * muestras;
    TamanoAlineacion { tamano: total.div_ceil(ALINEACION_HEAP) * ALINEACION_HEAP, alineacion: ALINEACION_HEAP }
}

// ------------------------------------------------------------------ heap, fence y conjunto de residencia

/// Un heap (MTLHeap): la memoria reservada de la que salen buffers y texturas. Aquí solo se lleva la cuenta de lo
/// ocupado: cada recurso se crea en el servidor de GPU como los del dispositivo.
pub struct Heap {
    tamano: u64,
    /// Las opciones del heap (MTLResourceOptions): la caché en los bits 0-3, el modo de almacenamiento en los 4-7 y el
    /// modo de hazard en los 8-9. Se guardan empaquetadas para poder contestar resourceOptions tal cual.
    opciones: u64,
    ocupado: u64,
    etiqueta: Id,
}
con_etiqueta!(Heap);

impl Drop for Heap {
    fn drop(&mut self) {
        soltar(self.etiqueta);
    }
}

/// Un fence (MTLFence): aquí no hay planificador de citas, así que no sincroniza nada; existe para que el cliente
/// tenga el objeto que espera.
pub struct Fence {
    etiqueta: Id,
}
con_etiqueta!(Fence);

impl Drop for Fence {
    fn drop(&mut self) {
        soltar(self.etiqueta);
    }
}

/// Un conjunto de residencia (MTLResidencySet): los recursos que se mantienen vivos mientras esté puesto.
pub struct Residencia {
    objetos: Vec<Id>,
    etiqueta: Id,
}
con_etiqueta!(Residencia);

impl Drop for Residencia {
    fn drop(&mut self) {
        for o in self.objetos.drain(..) {
            soltar(o);
        }
        soltar(self.etiqueta);
    }
}

extern "C-unwind" fn heap_tamano(this: Id, _cmd: Sel) -> u64 {
    estado::<Heap>(this).map(|h| h.tamano).unwrap_or(0)
}
/// storageMode (MTLHeap): el modo de almacenamiento son los bits 4-7 de las opciones.
extern "C-unwind" fn heap_modo(this: Id, _cmd: Sel) -> u64 {
    estado::<Heap>(this).map(|h| (h.opciones >> 4) & 0xf).unwrap_or(0)
}
/// cpuCacheMode (MTLHeap): la caché son los bits 0-3 de las opciones.
extern "C-unwind" fn heap_cache(this: Id, _cmd: Sel) -> u64 {
    estado::<Heap>(this).map(|h| h.opciones & 0xf).unwrap_or(0)
}
/// hazardTrackingMode (MTLHeap): el modo de hazard son los bits 8-9 de las opciones.
extern "C-unwind" fn heap_hazard(this: Id, _cmd: Sel) -> u64 {
    estado::<Heap>(this).map(|h| (h.opciones >> 8) & 0x3).unwrap_or(0)
}
/// resourceOptions (MTLHeap): las opciones tal cual (modo de almacenamiento, caché y hazard empaquetados). Antes se
/// contestaba 0 y el cliente deducía de ahí un almacenamiento *Shared* que no era el del heap.
extern "C-unwind" fn heap_opciones(this: Id, _cmd: Sel) -> u64 {
    estado::<Heap>(this).map(|h| h.opciones).unwrap_or(0)
}
/// unfilteredResourceOptions (MTLHeap): las opciones sin el modo de hazard tracking.
extern "C-unwind" fn heap_opciones_sin_hazard(this: Id, _cmd: Sel) -> u64 {
    estado::<Heap>(this).map(|h| h.opciones & !(3u64 << 8)).unwrap_or(0)
}
/// usedSize y currentAllocatedSize (MTLHeap): los bytes que ocupa lo que se ha sacado del heap.
extern "C-unwind" fn heap_ocupado(this: Id, _cmd: Sel) -> u64 {
    estado::<Heap>(this).map(|h| h.ocupado).unwrap_or(0)
}
/// maxAvailableSizeWithAlignment: (MTLHeap): lo que queda libre del heap, redondeado hacia abajo a la alineación
/// pedida (si no se pide ninguna, se devuelve tal cual).
extern "C-unwind" fn heap_max_disponible(this: Id, _cmd: Sel, alineacion: u64) -> u64 {
    let Some(h) = estado::<Heap>(this) else { return 0 };
    let libre = h.tamano.saturating_sub(h.ocupado);
    if alineacion > 1 {
        libre - libre % alineacion
    } else {
        libre
    }
}
/// setPurgeableState: (MTLHeap): la memoria la tiene el servidor de GPU y no se puede purgar; se acepta el cambio de
/// estado y se dice que la memoria sigue ahí (como en el buffer).
extern "C-unwind" fn heap_purgeable(_this: Id, _cmd: Sel, _estado: u64) -> Bool {
    1
}
/// type y gpuAddress (MTLHeap): el heap no tiene memoria propia (los recursos viven en el servidor de GPU), así que se
/// contesta el tipo automático y la dirección 0.
extern "C-unwind" fn heap_cero(_this: Id, _cmd: Sel) -> u64 {
    0
}
/// memoryPoolId (MTLHeap): este heap no sale de ningún conjunto de memoria.
extern "C-unwind" fn heap_nil(_this: Id, _cmd: Sel) -> Id {
    std::ptr::null_mut()
}
extern "C-unwind" fn heap_nuevo_buffer(this: Id, _cmd: Sel, len: u64, opciones: u64) -> Id {
    let b = match clases::nuevo_buffer(len as usize, opciones, None) {
        Ok(b) => b,
        Err(e) => {
            registro(&format!("heap: newBufferWithLength:{len}: {e}"));
            return std::ptr::null_mut();
        }
    };
    if let Some(h) = estado::<Heap>(this) {
        h.ocupado += len.next_multiple_of(256);
    }
    b
}
/// newBufferWithLength:options:offset: (MTLHeap): el desplazamiento dentro del heap no se puede honorar (aquí los
/// recursos no se colocan en el heap, sino en el servidor de GPU), así que se crea el buffer como el otro y se anota.
extern "C-unwind" fn heap_nuevo_buffer_offset(this: Id, cmd: Sel, len: u64, opciones: u64, offset: u64) -> Id {
    registro(&format!("heap: newBufferWithLength:options:offset: {len} bytes en {offset}: el desplazamiento se ignora"));
    heap_nuevo_buffer(this, cmd, len, opciones)
}
/// newBufferWithLength:options:atOffset: (MTLHeap): la otra forma de pedir lo mismo que con offset:.
extern "C-unwind" fn heap_nuevo_buffer_at_offset(this: Id, cmd: Sel, len: u64, opciones: u64, offset: u64) -> Id {
    registro(&format!("heap: newBufferWithLength:options:atOffset: {len} bytes en {offset}: el desplazamiento se ignora"));
    heap_nuevo_buffer(this, cmd, len, opciones)
}
/// newBufferWithDescriptor: (MTLHeap): el buffer sale del descriptor (MTLBufferDescriptor: «length» y «options»).
extern "C-unwind" fn heap_nuevo_buffer_desc(this: Id, cmd: Sel, desc: Id) -> Id {
    let len = campo(desc, "length", 0u64);
    let opciones = campo(desc, "options", 0u64);
    heap_nuevo_buffer(this, cmd, len, opciones)
}
/// newBufferWithDescriptor:offset: (MTLHeap): como el otro, el desplazamiento dentro del heap se ignora.
extern "C-unwind" fn heap_nuevo_buffer_desc_offset(this: Id, cmd: Sel, desc: Id, offset: u64) -> Id {
    registro(&format!("heap: newBufferWithDescriptor:offset: en {offset}: el desplazamiento se ignora"));
    heap_nuevo_buffer_desc(this, cmd, desc)
}
extern "C-unwind" fn heap_nueva_textura(this: Id, _cmd: Sel, desc: Id) -> Id {
    let t = textura_de_descriptor(desc);
    if t.is_null() {
        return t;
    }
    let bytes = estado::<Textura>(t).map(|x| x.ancho * x.alto * x.prof.max(1) * bytes_por_pixel(x.formato)).unwrap_or(0);
    if let Some(h) = estado::<Heap>(this) {
        h.ocupado += bytes.next_multiple_of(256);
    }
    t
}
/// newTextureWithDescriptor:offset: (MTLHeap): como el otro, el desplazamiento dentro del heap se ignora.
extern "C-unwind" fn heap_nueva_textura_offset(this: Id, cmd: Sel, desc: Id, offset: u64) -> Id {
    registro(&format!("heap: newTextureWithDescriptor:offset: en {offset}: el desplazamiento se ignora"));
    heap_nueva_textura(this, cmd, desc)
}
/// newTextureWithDescriptor:atOffset: (MTLHeap): la otra forma de pedir lo mismo que con offset:.
extern "C-unwind" fn heap_nueva_textura_at_offset(this: Id, cmd: Sel, desc: Id, offset: u64) -> Id {
    registro(&format!("heap: newTextureWithDescriptor:atOffset: en {offset}: el desplazamiento se ignora"));
    heap_nueva_textura(this, cmd, desc)
}
/// Las estructuras de aceleración no existen en este driver: se devuelve nil (como hacen los stubs del protocolo del
/// dispositivo) para que el cliente no se deadly con un selector desconocido.
extern "C-unwind" fn heap_acel_tamano(_this: Id, _cmd: Sel, _tamano: u64) -> Id {
    std::ptr::null_mut()
}
extern "C-unwind" fn heap_acel_desc(_this: Id, _cmd: Sel, _desc: Id) -> Id {
    std::ptr::null_mut()
}
extern "C-unwind" fn heap_acel_tamano_offset(_this: Id, _cmd: Sel, _tamano: u64, _offset: u64) -> Id {
    std::ptr::null_mut()
}
extern "C-unwind" fn heap_acel_desc_offset(_this: Id, _cmd: Sel, _desc: Id, _offset: u64) -> Id {
    std::ptr::null_mut()
}
extern "C-unwind" fn heap_acel_tamano_indice(_this: Id, _cmd: Sel, _tamano: u64, _indice: u64) -> Id {
    std::ptr::null_mut()
}
extern "C-unwind" fn heap_acel_tamano_offset_indice(_this: Id, _cmd: Sel, _tamano: u64, _offset: u64, _indice: u64) -> Id {
    std::ptr::null_mut()
}
/// addAllocation: (MTLResidencySet): añade un recurso (buffer o textura) al conjunto.
extern "C-unwind" fn residencia_anadir(this: Id, _cmd: Sel, recurso: Id) {
    let Some(r) = estado::<Residencia>(this) else { return };
    if recurso.is_null() {
        return;
    }
    r.objetos.push(retener(recurso));
}
/// removeAllocation: (MTLResidencySet): quita un recurso del conjunto.
extern "C-unwind" fn residencia_quitar(this: Id, _cmd: Sel, recurso: Id) {
    let Some(r) = estado::<Residencia>(this) else { return };
    if let Some(p) = r.objetos.iter().position(|o| *o == recurso) {
        let o = r.objetos.remove(p);
        soltar(o);
    }
}
/// allAllocations (MTLResidencySet): los recursos que tiene el conjunto ahora mismo.
extern "C-unwind" fn residencia_todos(this: Id, _cmd: Sel) -> Id {
    let v: Vec<Id> = estado::<Residencia>(this).map(|r| r.objetos.clone()).unwrap_or_default();
    // SAFETY: NSArray de Foundation.
    unsafe { objc::m2(objc::clase("NSArray"), "arrayWithObjects:count:", v.as_ptr(), v.len() as u64) }
}
/// allocatedSize (MTLResidencySet): los bytes que ocupan los recursos del conjunto.
extern "C-unwind" fn residencia_tamano(this: Id, _cmd: Sel) -> u64 {
    let Some(r) = estado::<Residencia>(this) else { return 0 };
    r.objetos.iter().map(|o| tamano_de_recurso(*o)).sum()
}
/// allocationCount (MTLResidencySet): cuántos recursos tiene el conjunto.
extern "C-unwind" fn residencia_cuenta(this: Id, _cmd: Sel) -> u64 {
    estado::<Residencia>(this).map(|r| r.objetos.len() as u64).unwrap_or(0)
}
/// addAllObjects: (no es del protocolo MTLResidencySet, pero algunos clientes lo usan): añade todos los objetos de
/// un NSArray al conjunto.
extern "C-unwind" fn residencia_anadir_todos(this: Id, _cmd: Sel, objetos: Id) {
    if objetos.is_null() {
        return;
    }
    // SAFETY: objetos es un NSArray.
    let n = unsafe { objc::m0::<u64>(objetos, "count") };
    for i in 0..n {
        // SAFETY: el NSArray tiene n objetos.
        let o = unsafe { objc::m1::<u64, Id>(objetos, "objectAtIndexedSubscript:", i) };
        if !o.is_null() {
            residencia_anadir(this, _cmd, o);
        }
    }
}

/// commit (MTLResidencySet): no hay planificador de citas; el conjunto solo retiene los recursos.
extern "C-unwind" fn residencia_commit(_this: Id, _cmd: Sel) {}
/// requestResidency (MTLResidencySet): no hay planificador de citas; el conjunto ya retiene los recursos.
extern "C-unwind" fn residencia_pedir(_this: Id, _cmd: Sel) {}
/// endResidency (MTLResidencySet): lo mismo que pedir residencia; aquí no hace nada.
extern "C-unwind" fn residencia_terminar(_this: Id, _cmd: Sel) {}
extern "C-unwind" fn residencia_quitar_todo(this: Id, _cmd: Sel) {
    if let Some(r) = estado::<Residencia>(this) {
        for o in r.objetos.drain(..) {
            soltar(o);
        }
    }
}

/// newHeapWithDescriptor: (MTLDevice): QuartzCore lo pide en `update_surface_heap` y MPS en sus cachés.
extern "C-unwind" fn dev_heap(_this: Id, _cmd: Sel, desc: Id) -> Id {
    let tamano = campo(desc, "size", 0u64);
    let modo = campo(desc, "storageMode", 0u64);
    let cache = campo(desc, "cpuCacheMode", 0u64);
    let hazard = campo(desc, "hazardTrackingMode", 0u64);
    // MTLResourceOptions: la caché en los bits 0-3, el modo de almacenamiento en los 4-7 y el hazard en los 8-9.
    let opciones = (cache & 0xf) | ((modo & 0xf) << 4) | ((hazard & 0x3) << 8);
    registro(&format!("heap: {tamano} bytes, modo {modo}, opciones {opciones}"));
    crear(&clases_render().heap, Heap { tamano, opciones, ocupado: 0, etiqueta: std::ptr::null_mut() })
}

/// newFence (MTLDevice): el objeto en sí; no sincroniza nada.
extern "C-unwind" fn dev_fence(_this: Id, _cmd: Sel) -> Id {
    crear(&clases_render().fence, Fence { etiqueta: std::ptr::null_mut() })
}

/// newResidencySetWithDescriptor:error: (MTLDevice): guarda los recursos que le metan.
extern "C-unwind" fn dev_residencia(_this: Id, _cmd: Sel, desc: Id, error: *mut Id) -> Id {
    if !error.is_null() {
        // SAFETY: parámetro de salida.
        unsafe { *error = std::ptr::null_mut() };
    }
    let _ = desc;
    crear(&clases_render().residencia, Residencia { objetos: Vec::new(), etiqueta: std::ptr::null_mut() })
}

/// newBufferWithBytesNoCopy:length:options:deallocator: (MTLDevice): el cliente da memoria ya paginada y espera que
/// Metal no la copie. Aquí sí se copia al buffer del servidor (el servidor no puede importar memoria del cliente),
/// pero el bloque de liberación se guarda y se llama cuando el buffer muere con el puntero que dio el cliente, como
/// manda el contrato (`void (^)(const void *)`).
///
/// Si el cliente no da memoria (`bytes == NULL`) pero sí da liberador, en Metal se crearía un buffer vacío y el bloque
/// se llamaría una vez con NULL: aquí el bloque se copia igualmente y se llama con NULL al morir el buffer (el puntero
/// que se le pasa es `origen`, que es NULL cuando el cliente no dio memoria).
extern "C-unwind" fn dev_buffer_nocopia(_this: Id, _cmd: Sel, p: *const u8, len: u64, opciones: u64, liberador: Id) -> Id {
    // SAFETY: el cliente garantiza que hay len bytes en p (si no hay memoria, p es nulo y no se lee nada).
    let datos = if p.is_null() { None } else { Some(unsafe { std::slice::from_raw_parts(p, len as usize) }) };
    // El bloque se copia siempre que no sea nulo: si no hay memoria del cliente, se llamará con NULL.
    let bloq = clases::copiar_bloque(liberador);
    match clases::nuevo_buffer_con_liberador(len as usize, opciones, datos, None, p) {
        Ok(b) => {
            // Los bytes ya están copiados al fichero del servidor: la memoria del cliente ya no la necesita Metal
            // para nada, así que se avisa AHORA, exactamente una vez y con su puntero y su longitud. Si se esperase
            // a que el buffer muera, el bloque no se llamaría nunca en un cliente que no suelta el objeto (Metal
            // devuelve los objetos autoreleados) y ese cliente se quedaría esperando su aviso: es el caso de
            // CoreImage, cuyo bloque recupera el contexto de la conversión.
            if let Some(bloque) = bloq {
                // SAFETY: bloque copiado con _Block_copy, con la firma void (^)(const void *, NSUInteger).
                unsafe { clases::liberar_bloque_del_cliente(bloque, p, len) };
            }
            b
        }
        Err(e) => {
            // El buffer no llegó a existir: la memoria sigue siendo del cliente, así que el bloque NO se llama;
            // solo se suelta la copia.
            if let Some(bloque) = bloq {
                // SAFETY: bloque copiado con _Block_copy.
                unsafe { clases::soltar_bloque(bloque) };
            }
            registro(&format!("newBufferWithBytesNoCopy:{len}: {e}"));
            std::ptr::null_mut()
        }
    }
}

pub fn registrar(dispositivo: Class) {
    if CLASES.get().is_some() {
        return;
    }
    interceptar_constantes();
    let disp = clases::dispositivo as Imp;
    let etq = |v: &mut Vec<Metodo>, l: Imp, s: Imp| {
        v.push(("label", l, "@@:"));
        v.push(("setLabel:", s, "v@:@"));
        v.push(("device", disp, "@@:"));
    };
    let mut textura: Vec<Metodo> = vec![
        ("textureType", tex_tipo as Imp, "Q@:"),
        ("pixelFormat", tex_formato as Imp, "Q@:"),
        ("width", tex_ancho as Imp, "Q@:"),
        ("height", tex_alto as Imp, "Q@:"),
        ("depth", tex_prof as Imp, "Q@:"),
        ("mipmapLevelCount", tex_niveles as Imp, "Q@:"),
        ("arrayLength", tex_capas as Imp, "Q@:"),
        ("sampleCount", tex_muestras as Imp, "Q@:"),
        ("usage", tex_uso as Imp, "Q@:"),
        ("storageMode", tex_almacen as Imp, "Q@:"),
        ("allocatedSize", tex_allocado as Imp, "Q@:"),
        ("setPurgeableState:", tex_purgeable as Imp, "c@:Q"),
        ("iosurface", tex_superficie as Imp, "^v@:"),
        ("iosurfacePlane", tex_plano as Imp, "Q@:"),
        ("parentTexture", tex_nil as Imp, "@@:"),
        ("buffer", tex_nil as Imp, "@@:"),
        ("rootResource", tex_nil as Imp, "@@:"),
        ("isFramebufferOnly", tex_no as Imp, "c@:"),
        ("replaceRegion:mipmapLevel:withBytes:bytesPerRow:", tex_reemplazar as Imp, "v@:{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}Q^vQ"),
        ("replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:", tex_reemplazar_capa as Imp, "v@:{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}QQ^vQQ"),
        ("getBytes:bytesPerRow:fromRegion:mipmapLevel:", tex_obtener as Imp, "v@:^vQ{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}Q"),
        ("getBytes:bytesPerRow:bytesPerImage:fromRegion:mipmapLevel:slice:", tex_obtener_capa as Imp, "v@:^vQQ{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}QQ"),
    ];
    etq(&mut textura, clases::etiqueta::<Textura> as Imp, clases::poner_etiqueta::<Textura> as Imp);
    let mut sampler: Vec<Metodo> = vec![];
    etq(&mut sampler, clases::etiqueta::<Sampler> as Imp, clases::poner_etiqueta::<Sampler> as Imp);
    let mut profundidad: Vec<Metodo> = vec![
        ("depthCompareFunction", prof_comparacion as Imp, "Q@:"),
        ("depthWriteEnabled", prof_escritura as Imp, "c@:"),
        ("depthAttachmentPixelFormat", prof_formato_profundidad as Imp, "Q@:"),
        ("stencilCompareFunction", prof_comparacion_estencil as Imp, "Q@:"),
        ("stencilWriteMask", prof_mascara_estencil as Imp, "Q@:"),
        ("stencilWriteMaskFront", prof_mascara_delante as Imp, "Q@:"),
        ("stencilWriteMaskBack", prof_mascara_detras as Imp, "Q@:"),
        ("stencilReferenceValueFront", prof_referencia_delante as Imp, "Q@:"),
        ("stencilReferenceValueBack", prof_referencia_detras as Imp, "Q@:"),
        ("stencilPixelFormat", prof_formato_estencil as Imp, "Q@:"),
    ];
    etq(&mut profundidad, clases::etiqueta::<Profundidad> as Imp, clases::poner_etiqueta::<Profundidad> as Imp);
    let mut pipeline: Vec<Metodo> = vec![];
    etq(&mut pipeline, clases::etiqueta::<PipelineRender> as Imp, clases::poner_etiqueta::<PipelineRender> as Imp);
    let mut encoder: Vec<Metodo> = vec![
        ("setRenderPipelineState:", ren_pipeline as Imp, "v@:@"),
        ("updateFence:", ren_fence as Imp, "v@:@"),
        ("waitForFence:", ren_fence as Imp, "v@:@"),
        ("setVertexBuffer:offset:atIndex:", ren_vbuffer as Imp, "v@:@QQ"),
        ("setFragmentBuffer:offset:atIndex:", ren_fbuffer as Imp, "v@:@QQ"),
        ("setVertexBytes:length:atIndex:", ren_vbytes as Imp, "v@:^vQQ"),
        ("setFragmentBytes:length:atIndex:", ren_fbytes as Imp, "v@:^vQQ"),
        ("setVertexTexture:atIndex:", ren_vtextura as Imp, "v@:@Q"),
        ("setFragmentTexture:atIndex:", ren_ftextura as Imp, "v@:@Q"),
        ("setVertexSamplerState:atIndex:", ren_vsampler as Imp, "v@:@Q"),
        ("setFragmentSamplerState:atIndex:", ren_fsampler as Imp, "v@:@Q"),
        ("setViewport:", ren_viewport as Imp, "v@:{MTLViewport=dddddd}"),
        ("setScissorRect:", ren_tijera as Imp, "v@:{MTLScissorRect=QQQQ}"),
        ("setCullMode:", ren_cull as Imp, "v@:Q"),
        ("setFrontFacingWinding:", ren_sentido as Imp, "v@:Q"),
        ("setTriangleFillMode:", ren_nada_u as Imp, "v@:Q"),
        ("setDepthStencilState:", ren_nada_o as Imp, "v@:@"),
        ("setStencilReferenceValue:", ren_nada_u as Imp, "v@:I"),
        ("setBlendColorRed:green:blue:alpha:", ren_color as Imp, "v@:ffff"),
        ("drawPrimitives:vertexStart:vertexCount:", ren_dibujar as Imp, "v@:QQQ"),
        ("drawPrimitives:vertexStart:vertexCount:instanceCount:", ren_dibujar_inst as Imp, "v@:QQQQ"),
        ("drawPrimitives:vertexStart:vertexCount:instanceCount:baseInstance:", ren_dibujar_inst_base as Imp, "v@:QQQQQ"),
        ("drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:", ren_indices as Imp, "v@:QQQ@Q"),
        ("drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:", ren_indices_inst as Imp, "v@:QQQ@QQ"),
        ("drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:baseVertex:baseInstance:", ren_indices_inst_base as Imp, "v@:QQQ@QQqQ"),
        ("endEncoding", ren_fin as Imp, "v@:"),
        ("pushDebugGroup:", ren_nada_o as Imp, "v@:@"),
        ("popDebugGroup", clases::nada as Imp, "v@:"),
        ("insertDebugSignpost:", ren_nada_o as Imp, "v@:@"),
    ];
    etq(&mut encoder, clases::etiqueta::<Encoder> as Imp, clases::poner_etiqueta::<Encoder> as Imp);
    let mut heap: Vec<Metodo> = vec![
        ("size", heap_tamano as Imp, "Q@:"),
        ("allocatedSize", heap_ocupado as Imp, "Q@:"),
        ("usedSize", heap_ocupado as Imp, "Q@:"),
        ("currentAllocatedSize", heap_ocupado as Imp, "Q@:"),
        ("maxAvailableSizeWithAlignment:", heap_max_disponible as Imp, "Q@:Q"),
        ("setPurgeableState:", heap_purgeable as Imp, "c@:Q"),
        ("storageMode", heap_modo as Imp, "Q@:"),
        ("cpuCacheMode", heap_cache as Imp, "Q@:"),
        ("hazardTrackingMode", heap_hazard as Imp, "Q@:"),
        ("resourceOptions", heap_opciones as Imp, "Q@:"),
        ("unfilteredResourceOptions", heap_opciones_sin_hazard as Imp, "Q@:"),
        ("type", heap_cero as Imp, "Q@:"),
        ("gpuAddress", heap_cero as Imp, "Q@:"),
        ("memoryPoolId", heap_nil as Imp, "@@:"),
        ("newBufferWithLength:options:", heap_nuevo_buffer as Imp, "@@:QQ"),
        ("newBufferWithLength:options:offset:", heap_nuevo_buffer_offset as Imp, "@@:QQQ"),
        ("newBufferWithLength:options:atOffset:", heap_nuevo_buffer_at_offset as Imp, "@@:QQQ"),
        ("newBufferWithDescriptor:", heap_nuevo_buffer_desc as Imp, "@@:@"),
        ("newBufferWithDescriptor:offset:", heap_nuevo_buffer_desc_offset as Imp, "@@:@Q"),
        ("newTextureWithDescriptor:", heap_nueva_textura as Imp, "@@:@"),
        ("newTextureWithDescriptor:offset:", heap_nueva_textura_offset as Imp, "@@:@Q"),
        ("newTextureWithDescriptor:atOffset:", heap_nueva_textura_at_offset as Imp, "@@:@Q"),
        ("newAccelerationStructureWithSize:", heap_acel_tamano as Imp, "@@:Q"),
        ("newAccelerationStructureWithDescriptor:", heap_acel_desc as Imp, "@@:@"),
        ("newAccelerationStructureWithSize:offset:", heap_acel_tamano_offset as Imp, "@@:QQ"),
        ("newAccelerationStructureWithDescriptor:offset:", heap_acel_desc_offset as Imp, "@@:@Q"),
        ("newAccelerationStructureWithSize:resourceIndex:", heap_acel_tamano_indice as Imp, "@@:QQ"),
        ("newAccelerationStructureWithSize:offset:resourceIndex:", heap_acel_tamano_offset_indice as Imp, "@@:QQQ"),
    ];
    etq(&mut heap, clases::etiqueta::<Heap> as Imp, clases::poner_etiqueta::<Heap> as Imp);
    let mut fence: Vec<Metodo> = vec![];
    etq(&mut fence, clases::etiqueta::<Fence> as Imp, clases::poner_etiqueta::<Fence> as Imp);
    let mut residencia: Vec<Metodo> = vec![
        ("addAllocation:", residencia_anadir as Imp, "v@:@"),
        ("removeAllocation:", residencia_quitar as Imp, "v@:@"),
        ("allAllocations", residencia_todos as Imp, "@@:"),
        ("allocatedSize", residencia_tamano as Imp, "Q@:"),
        ("allocationCount", residencia_cuenta as Imp, "Q@:"),
        ("commit", residencia_commit as Imp, "v@:"),
        ("requestResidency", residencia_pedir as Imp, "v@:"),
        ("endResidency", residencia_terminar as Imp, "v@:"),
        ("addAllObjects:", residencia_anadir_todos as Imp, "v@:@"),
        ("removeAllObjects", residencia_quitar_todo as Imp, "v@:"),
    ];
    etq(&mut residencia, clases::etiqueta::<Residencia> as Imp, clases::poner_etiqueta::<Residencia> as Imp);
    let c = ClasesRender {
        heap: nueva_clase("TahoeMTLHeap", &["MTLHeap"], &heap),
        fence: nueva_clase("TahoeMTLFence", &["MTLFence"], &fence),
        residencia: nueva_clase("TahoeMTLResidencySet", &["MTLResidencySet"], &residencia),
        textura: nueva_clase("TahoeMTLTexture", &["MTLTexture"], &textura),
        sampler: nueva_clase("TahoeMTLSamplerState", &["MTLSamplerState"], &sampler),
        pipeline: nueva_clase("TahoeMTLRenderPipelineState", &["MTLRenderPipelineState"], &pipeline),
        encoder: nueva_clase("TahoeMTLRenderCommandEncoder", &["MTLRenderCommandEncoder"], &encoder),
        profundidad: nueva_clase("TahoeMTLDepthStencilState", &["MTLDepthStencilState"], &profundidad),
    };
    let _ = CLASES.set(c);
    // métodos nuevos en clases ya registradas: el dispositivo, la biblioteca y el command buffer
    for (n, imp, t) in [
        ("newTextureWithDescriptor:", dev_textura as Imp, "@@:@"),
        ("newTextureWithDescriptor:iosurface:plane:", dev_textura_superficie as Imp, "@@:@^vQ"),
        ("newSamplerStateWithDescriptor:", dev_sampler as Imp, "@@:@"),
        ("heapBufferSizeAndAlignWithLength:options:", tamano_buffer_heap as Imp, "{?=QQ}32@0:8Q16Q24"),
        ("heapTextureSizeAndAlignWithDescriptor:", tamano_textura_heap as Imp, "{?=QQ}24@0:8@16"),
        ("newHeapWithDescriptor:", dev_heap as Imp, "@@:@"),
        ("newFence", dev_fence as Imp, "@16@0:8"),
        ("newResidencySetWithDescriptor:error:", dev_residencia as Imp, "@@:@^@"),
        ("newBufferWithBytesNoCopy:length:options:deallocator:", dev_buffer_nocopia as Imp, "@48@0:8^v16Q24Q32@?40"),
        ("newDepthStencilStateWithDescriptor:", dev_profundidad as Imp, "@@:@"),
        ("newRenderPipelineStateWithDescriptor:error:", dev_pipeline_render as Imp, "@@:@^@"),
        ("newRenderPipelineStateWithDescriptor:options:reflection:error:", dev_pipeline_render_opciones as Imp, "@@:@Q^@^@"),
        ("newRenderPipelineStateWithDescriptor:completionHandler:", crate::pipeline_render_completado as Imp, "v32@0:8@16@?24"),
        ("newRenderPipelineStateWithDescriptor:options:completionHandler:", crate::pipeline_render_opciones_completado as Imp, "v40@0:8@16Q24@?32"),
        ("newEvent", crate::dev_evento as Imp, "@16@0:8"),
        ("newSharedEvent", crate::dev_evento as Imp, "@16@0:8"),
        ("newSharedEventWithHandle:", crate::dev_evento_manejador as Imp, "@24@0:8@16"),
        ("functionHandleWithFunction:", crate::dev_manejador_funcion as Imp, "@24@0:8@16"),
        ("functionHandleWithBinaryFunction:", crate::dev_manejador_funcion as Imp, "@24@0:8@16"),
    ] {
        objc::metodo(dispositivo, n, imp, t);
    }
    objc::metodo(clases::clases().biblioteca.cls, "newFunctionWithName:constantValues:error:", bib_funcion_constantes as Imp, "@@:@@^@");
    objc::metodo(clases::clases().biblioteca.cls, "newFunctionWithDescriptor:error:", bib_funcion_descriptor as Imp, "@@:@^@");
    objc::metodo(clases::clases().cb.cls, "renderCommandEncoderWithDescriptor:", cb_render as Imp, "@@:@");
}
