//! tahoe-metal: el driver Metal de tahoe-run. Es el ejecutable de TahoeMetal.bundle, que Metal carga porque el
//! IOAccelerator emulado (tahoe-run con TAHOE_METAL) lo nombra en MetalPluginName; su clase principal, TahoeMTLDevice,
//! es el MTLDevice. Por debajo, los comandos irán a tahoe-gpu (Vulkan en Linux).
//!
//! Fase actual: escucha. La clase hereda de _MTLDevice (la base de Metal.framework) y apunta en el registro qué le pide
//! Metal (selectores que no tiene, respondsToSelector:...), para ir implementando justo lo que hace falta.

pub mod canal;
pub mod clases;
pub mod objc;
pub mod render;

use std::ffi::c_void;
use std::io::Write;
use std::sync::OnceLock;

use objc::{Bool, Class, Id, Sel};

/// Registro: /private/var/tmp/tahoe-metal.log dentro de macOS (y la salida de errores).
pub fn registro(s: &str) {
    let linea = format!("tahoe-metal[{}]: {s}\n", std::process::id());
    let _ = std::io::stderr().write_all(linea.as_bytes());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/private/var/tmp/tahoe-metal.log") {
        let _ = f.write_all(linea.as_bytes());
    }
}

static MADRE: OnceLock<usize> = OnceLock::new();

fn madre() -> Class {
    *MADRE.get().unwrap_or(&0) as Class
}

/// -[_MTLDevice initWithAcceleratorPort:] es abstracta (lanza una excepción y devuelve nil): la base se inicia con init
/// y el puerto del acelerador se guarda aquí.
extern "C-unwind" fn init_con_puerto(this: Id, _cmd: Sel, puerto: u32) -> Id {
    registro(&format!("-[TahoeMTLDevice initWithAcceleratorPort:{puerto:#x}]"));
    // SAFETY: init de NSObject (vía _MTLDevice).
    let r: Id = unsafe { objc::super0(this, madre(), objc::sel("init")) };
    PUERTO.store(puerto, std::sync::atomic::Ordering::Relaxed);
    clases::DISPOSITIVO.store(r, std::sync::atomic::Ordering::Relaxed);
    if canal::gpu(puerto).is_none() {
        // sin servidor de GPU no hay dispositivo: Metal sigue sin él
        registro("  sin GPU: initWithAcceleratorPort: devuelve nil");
        return std::ptr::null_mut();
    }
    registro(&format!("  -> {r:?}"));
    r
}

fn info() -> canal::protocolo::Info {
    canal::gpu_ya().map(|g| g.info.clone()).unwrap_or_default()
}

extern "C-unwind" fn registry_id(_this: Id, _cmd: Sel) -> u64 {
    info().registry_id
}

extern "C-unwind" fn max_buffer(_this: Id, _cmd: Sel) -> u64 {
    info().max_buffer
}

// ------------------------------------------------------------------ creación de objetos (MTLDevice)

extern "C-unwind" fn nueva_cola(_this: Id, _cmd: Sel) -> Id {
    clases::nueva_cola()
}
extern "C-unwind" fn nueva_cola_max(_this: Id, _cmd: Sel, _n: u64) -> Id {
    clases::nueva_cola()
}
extern "C-unwind" fn nuevo_buffer(_this: Id, _cmd: Sel, len: u64, opciones: u64) -> Id {
    clases::nuevo_buffer(len as usize, opciones, None).unwrap_or_else(|e| {
        registro(&format!("newBufferWithLength:{len}: {e}"));
        std::ptr::null_mut()
    })
}
extern "C-unwind" fn nuevo_buffer_datos(_this: Id, _cmd: Sel, p: *const u8, len: u64, opciones: u64) -> Id {
    // SAFETY: Metal garantiza len bytes en p.
    let d = if p.is_null() { None } else { Some(unsafe { std::slice::from_raw_parts(p, len as usize) }) };
    clases::nuevo_buffer(len as usize, opciones, d).unwrap_or_else(|e| {
        registro(&format!("newBufferWithBytes:length:{len}: {e}"));
        std::ptr::null_mut()
    })
}
extern "C-unwind" fn biblioteca_datos(_this: Id, _cmd: Sel, datos: Id, error: *mut Id) -> Id {
    // SAFETY: datos es un dispatch_data_t.
    let v = unsafe { clases::bytes_de_dispatch_data(datos) };
    clases::nueva_biblioteca(&v, error)
}
fn biblioteca_ruta(ruta: &str, error: *mut Id) -> Id {
    match std::fs::read(ruta) {
        Ok(v) => clases::nueva_biblioteca(&v, error),
        Err(e) => {
            registro(&format!("biblioteca {ruta}: {e}"));
            std::ptr::null_mut()
        }
    }
}
extern "C-unwind" fn biblioteca_archivo(_this: Id, _cmd: Sel, ruta: Id, error: *mut Id) -> Id {
    biblioteca_ruta(&objc::texto(ruta), error)
}
extern "C-unwind" fn biblioteca_url(_this: Id, _cmd: Sel, url: Id, error: *mut Id) -> Id {
    // SAFETY: url es un NSURL.
    let ruta = objc::texto(unsafe { objc::m0(url, "path") });
    biblioteca_ruta(&ruta, error)
}
extern "C-unwind" fn pipeline_funcion(_this: Id, _cmd: Sel, f: Id, error: *mut Id) -> Id {
    clases::nuevo_pipeline(f, error)
}

#[repr(C)]
struct MTLSize {
    w: u64,
    h: u64,
    d: u64,
}
extern "C-unwind" fn max_hilos(_this: Id, _cmd: Sel) -> MTLSize {
    let m = info().max_hilos_grupo.max(64) as u64;
    MTLSize { w: m, h: m, d: 64 }
}
extern "C-unwind" fn memoria_grupo(_this: Id, _cmd: Sel) -> u64 {
    info().memoria_grupo as u64
}
extern "C-unwind" fn si(_this: Id, _cmd: Sel) -> Bool {
    1
}
extern "C-unwind" fn no(_this: Id, _cmd: Sel) -> Bool {
    0
}
extern "C-unwind" fn trabajo_recomendado(_this: Id, _cmd: Sel) -> u64 {
    1 << 30
}
/// supportsFamily: las de Mac de Metal 2 (MTLGPUFamilyMac1 = 2001, Mac2 = 2002, Common1-2 = 3001-3002).
extern "C-unwind" fn familia(_this: Id, _cmd: Sel, f: i64) -> Bool {
    matches!(f, 2001 | 2002 | 3001 | 3002) as Bool
}
/// supportsFeatureSet: los de macOS GPUFamily1/2 (10000-10005).
extern "C-unwind" fn feature_set(_this: Id, _cmd: Sel, f: u64) -> Bool {
    (10000..=10005).contains(&f) as Bool
}

/// featureProfile (privado): 0 por ahora (el que usa un Mac sin GPU de Apple).
extern "C-unwind" fn perfil(_this: Id, _cmd: Sel) -> u64 {
    0
}

static PUERTO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

extern "C-unwind" fn puerto_acelerador(_this: Id, _cmd: Sel) -> u32 {
    PUERTO.load(std::sync::atomic::Ordering::Relaxed)
}

static SERVICIO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

extern "C-unwind" fn poner_servicio(_this: Id, _cmd: Sel, servicio: u32) {
    registro(&format!("_setAcceleratorService:{servicio:#x}"));
    SERVICIO.store(servicio, std::sync::atomic::Ordering::Relaxed);
}

/// Selectores que la clase no tiene: firma genérica (devuelve un objeto) para que lleguen a forwardInvocation:.
extern "C-unwind" fn firma_metodo(this: Id, cmd: Sel, s: Sel) -> Id {
    // SAFETY: NSObject implementa methodSignatureForSelector:; NSMethodSignature existe con Foundation.
    unsafe {
        let r: Id = objc::super1(this, madre(), cmd, s);
        if !r.is_null() {
            return r;
        }
        objc::m1(objc::clase("NSMethodSignature"), "signatureWithObjCTypes:", c"@@:".as_ptr())
    }
}

/// Fase de escucha: un mensaje que no se implementa se apunta y devuelve 0/nil (en vez de lanzar una excepción).
extern "C-unwind" fn reenviar(_this: Id, _cmd: Sel, inv: Id) {
    // SAFETY: inv es un NSInvocation.
    let s: Sel = unsafe { objc::m0(inv, "selector") };
    registro(&format!("sin implementar: -{}", objc::nombre_sel(s)));
}

extern "C-unwind" fn nombre(_this: Id, _cmd: Sel) -> Id {
    let n = info().nombre;
    objc::nsstring(if n.is_empty() { "Tahoe GPU" } else { &n })
}

extern "C-unwind" fn responde(this: Id, cmd: Sel, s: Sel) -> Bool {
    // SAFETY: NSObject implementa respondsToSelector:.
    let r: Bool = unsafe { objc::super1(this, madre(), cmd, s) };
    registro(&format!("respondsToSelector:{} -> {r}", objc::nombre_sel(s)));
    r
}

extern "C-unwind" fn resolver(_cls: Class, _cmd: Sel, s: Sel) -> Bool {
    registro(&format!("+resolveInstanceMethod:{} (no implementado)", objc::nombre_sel(s)));
    0
}

/// Crea la clase TahoeMTLDevice al cargar el bundle (antes de que NSBundle pida la clase principal).
extern "C" fn registrar() {
    let base = objc::clase("_MTLDevice");
    if base.is_null() {
        for n in ["NSObject", "_MTLDevice", "MTLIOAccelDevice", "MTLIGAccelDevice", "_MTLCommandQueue", "MTLCompiler", "NSBundle"] {
            registro(&format!("objc_getClass({n}) = {:?}", objc::clase(n)));
        }
        registro("no está _MTLDevice (¿Metal.framework sin cargar?)");
        return;
    }
    let _ = MADRE.set(base as usize);
    let nombre_c = c"TahoeMTLDevice";
    // SAFETY: clase nueva con una madre existente; los métodos llevan las firmas de sus tipos.
    unsafe {
        let cls = objc::objc_allocateClassPair(base, nombre_c.as_ptr(), 0);
        if cls.is_null() {
            registro("TahoeMTLDevice ya existe");
            return;
        }
        objc::metodo(cls, "initWithAcceleratorPort:", init_con_puerto as *const c_void, "@@:I");
        objc::metodo(cls, "name", nombre as *const c_void, "@@:");
        objc::metodo(cls, "acceleratorPort", puerto_acelerador as *const c_void, "I@:");
        objc::metodo(cls, "_setAcceleratorService:", poner_servicio as *const c_void, "v@:I");
        objc::metodo(cls, "registryID", registry_id as *const c_void, "Q@:");
        objc::metodo(cls, "maxBufferLength", max_buffer as *const c_void, "Q@:");
        objc::metodo(cls, "featureProfile", perfil as *const c_void, "Q@:");
        for (n, imp, t) in [
            ("newCommandQueue", nueva_cola as *const c_void, "@@:"),
            ("newCommandQueueWithMaxCommandBufferCount:", nueva_cola_max as *const c_void, "@@:Q"),
            ("newBufferWithLength:options:", nuevo_buffer as *const c_void, "@@:QQ"),
            ("newBufferWithBytes:length:options:", nuevo_buffer_datos as *const c_void, "@@:^vQQ"),
            ("newLibraryWithData:error:", biblioteca_datos as *const c_void, "@@:@^@"),
            ("newLibraryWithFile:error:", biblioteca_archivo as *const c_void, "@@:@^@"),
            ("newLibraryWithURL:error:", biblioteca_url as *const c_void, "@@:@^@"),
            ("newComputePipelineStateWithFunction:error:", pipeline_funcion as *const c_void, "@@:@^@"),
            ("maxThreadsPerThreadgroup", max_hilos as *const c_void, "{MTLSize=QQQ}@:"),
            ("maxThreadgroupMemoryLength", memoria_grupo as *const c_void, "Q@:"),
            ("recommendedMaxWorkingSetSize", trabajo_recomendado as *const c_void, "Q@:"),
            ("hasUnifiedMemory", si as *const c_void, "c@:"),
            ("isLowPower", si as *const c_void, "c@:"),
            ("isHeadless", no as *const c_void, "c@:"),
            ("isRemovable", no as *const c_void, "c@:"),
            ("supportsFamily:", familia as *const c_void, "c@:q"),
            ("supportsFeatureSet:", feature_set as *const c_void, "c@:Q"),
        ] {
            objc::metodo(cls, n, imp, t);
        }
        objc::metodo(cls, "methodSignatureForSelector:", firma_metodo as *const c_void, "@@::");
        objc::metodo(cls, "forwardInvocation:", reenviar as *const c_void, "v@:@");
        objc::metodo(cls, "respondsToSelector:", responde as *const c_void, "c@::");
        // MTLAddDevice comprueba conformsToProtocol:@protocol(MTLDevice)
        for p in [c"MTLDevice", c"MTLDeviceSPI"] {
            let pr = objc::objc_getProtocol(p.as_ptr());
            if !pr.is_null() {
                objc::class_addProtocol(cls, pr);
            } else {
                registro(&format!("no está el protocolo {p:?}"));
            }
        }
        let meta = objc::object_getClass(cls);
        objc::metodo(meta, "resolveInstanceMethod:", resolver as *const c_void, "c@::");
        objc::objc_registerClassPair(cls);
        render::registrar(cls);
    }
    clases::registrar();
    registro("TahoeMTLDevice registrada (madre _MTLDevice)");
}

#[used]
#[link_section = "__DATA,__mod_init_func"]
static CONSTRUCTOR: extern "C" fn() = registrar;
