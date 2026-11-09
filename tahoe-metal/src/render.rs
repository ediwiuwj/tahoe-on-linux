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
    pub textura: Clase,
    pub sampler: Clase,
    pub pipeline: Clase,
    pub encoder: Clase,
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
        let u = |s: &str| -> u32 { objc::m0::<u64>(d, s) as u32 };
        e.u32(id).u32(u("minFilter")).u32(u("magFilter")).u32(u("mipFilter")).u32(u("sAddressMode")).u32(u("tAddressMode")).u32(u("rAddressMode"));
        e.u32(objc::m0::<Bool>(d, "normalizedCoordinates") as u32).u32(u("maxAnisotropy"));
        e.u32(objc::m0::<f32>(d, "lodMinClamp").to_bits()).u32(objc::m0::<f32>(d, "lodMaxClamp").to_bits());
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
static ORIGINALES: OnceLock<[usize; 3]> = OnceLock::new();

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
        let _ = ORIGINALES.set([objc::method_getImplementation(a) as usize, objc::method_getImplementation(b) as usize, objc::method_getImplementation(c) as usize]);
        objc::method_setImplementation(a, fc_indice as Imp);
        objc::method_setImplementation(b, fc_nombre as Imp);
        objc::method_setImplementation(c, fc_rango as Imp);
    }
}

/// Las constantes de un MTLFunctionConstantValues, serializadas como las pide el protocolo.
fn serializar_constantes(valores: Id) -> Vec<u8> {
    let v = CONSTANTES.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&(valores as usize)).cloned())).unwrap_or_default();
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

fn nuevo_pipeline_render(d: Id, error: *mut Id) -> Id {
    let id = nuevo_id();
    let mut e = Escritor::default();
    e.u32(id);
    // SAFETY: d es un MTLRenderPipelineDescriptor; sus subobjetos, los del protocolo.
    unsafe {
        funcion(objc::m0(d, "vertexFunction"), &mut e);
        funcion(objc::m0(d, "fragmentFunction"), &mut e);
        // atributos y layouts del vertex descriptor
        let vd: Id = objc::m0(d, "vertexDescriptor");
        let mut atributos = Vec::new();
        let mut layouts = Vec::new();
        if !vd.is_null() {
            let attrs: Id = objc::m0(vd, "attributes");
            for i in 0..31u64 {
                let a: Id = objc::m1(attrs, "objectAtIndexedSubscript:", i);
                let f: u64 = objc::m0(a, "format");
                if f != 0 {
                    atributos.push((i as u32, f as u32, objc::m0::<u64>(a, "offset") as u32, objc::m0::<u64>(a, "bufferIndex") as u32));
                }
            }
            let ls: Id = objc::m0(vd, "layouts");
            let mut usados: Vec<u32> = atributos.iter().map(|a| a.3).collect();
            usados.sort();
            usados.dedup();
            for b in usados {
                let lo: Id = objc::m1(ls, "objectAtIndexedSubscript:", b as u64);
                layouts.push((b, objc::m0::<u64>(lo, "stride") as u32, objc::m0::<u64>(lo, "stepFunction") as u32));
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
        let cas: Id = objc::m0(d, "colorAttachments");
        let mut colores = Vec::new();
        for i in 0..8u64 {
            let c: Id = objc::m1(cas, "objectAtIndexedSubscript:", i);
            let f: u64 = objc::m0(c, "pixelFormat");
            let u = |s: &str| objc::m0::<u64>(c, s) as u32;
            colores.push((
                f as u32,
                objc::m0::<Bool>(c, "isBlendingEnabled") as u32,
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
        e.u32(objc::m0::<u64>(d, "depthAttachmentPixelFormat") as u32).u32(objc::m0::<u64>(d, "inputPrimitiveTopology") as u32);
    }
    match pedir(tipo::PIPELINE_RENDER, &e.0) {
        Ok(_) => crear(&clases_render().pipeline, PipelineRender { id, etiqueta: std::ptr::null_mut() }),
        Err(er) => {
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
    let mut pipeline: Vec<Metodo> = vec![];
    etq(&mut pipeline, clases::etiqueta::<PipelineRender> as Imp, clases::poner_etiqueta::<PipelineRender> as Imp);
    let mut encoder: Vec<Metodo> = vec![
        ("setRenderPipelineState:", ren_pipeline as Imp, "v@:@"),
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
    let c = ClasesRender {
        textura: nueva_clase("TahoeMTLTexture", &["MTLTexture"], &textura),
        sampler: nueva_clase("TahoeMTLSamplerState", &["MTLSamplerState"], &sampler),
        pipeline: nueva_clase("TahoeMTLRenderPipelineState", &["MTLRenderPipelineState"], &pipeline),
        encoder: nueva_clase("TahoeMTLRenderCommandEncoder", &["MTLRenderCommandEncoder"], &encoder),
    };
    let _ = CLASES.set(c);
    // métodos nuevos en clases ya registradas: el dispositivo, la biblioteca y el command buffer
    for (n, imp, t) in [
        ("newTextureWithDescriptor:", dev_textura as Imp, "@@:@"),
        ("newTextureWithDescriptor:iosurface:plane:", dev_textura_superficie as Imp, "@@:@^vQ"),
        ("newSamplerStateWithDescriptor:", dev_sampler as Imp, "@@:@"),
        ("newRenderPipelineStateWithDescriptor:error:", dev_pipeline_render as Imp, "@@:@^@"),
        ("newRenderPipelineStateWithDescriptor:options:reflection:error:", dev_pipeline_render_opciones as Imp, "@@:@Q^@^@"),
    ] {
        objc::metodo(dispositivo, n, imp, t);
    }
    objc::metodo(clases::clases().biblioteca.cls, "newFunctionWithName:constantValues:error:", bib_funcion_constantes as Imp, "@@:@@^@");
    objc::metodo(clases::clases().cb.cls, "renderCommandEncoderWithDescriptor:", cb_render as Imp, "@@:@");
}
