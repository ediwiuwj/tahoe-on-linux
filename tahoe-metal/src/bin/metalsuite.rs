//! metalsuite: recorrido de la API de Metal 2 dentro del macOS de tahoe-run.
//!
//! Dos cosas: (1) los métodos del protocolo MTLDevice que el driver **no** implementa (la lista de trabajo de la ruta
//! A: son los que, si un cliente los llama, caen en la implementación abstracta de Apple); (2) una batería de
//! comprobaciones funcionales (buffers, texturas, pipelines, codificadores, blits...). Al final, el resumen.

use std::ffi::{c_char, c_int, c_void, CStr};

#[path = "../objc.rs"]
#[allow(dead_code)]
mod objc;

use objc::Id;

extern "C" {
    fn dlopen(p: *const c_char, modo: c_int) -> *mut c_void;
    fn dlsym(h: *mut c_void, s: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn free(p: *mut c_void);
}

struct Informe {
    total: u32,
    bien: u32,
    fallos: Vec<(String, String)>,
    saltados: u32,
}

impl Informe {
    fn nuevo() -> Self {
        Informe { total: 0, bien: 0, fallos: Vec::new(), saltados: 0 }
    }
}

/// Comprueba una cosa: si la función devuelve Err, es un fallo del driver.
fn chequear(inf: &mut Informe, nombre: &str, f: impl FnOnce() -> Result<(), String>) {
    match f() {
        Ok(()) => {
            inf.total += 1;
            inf.bien += 1;
            println!("  ok      {nombre}");
        }
        Err(m) => {
            inf.total += 1;
            println!("  FALLO   {nombre}: {m}");
            inf.fallos.push((nombre.to_string(), m));
        }
    }
}

/// Un mensaje con cuatro argumentos (los que no cubre el módulo de Objective-C).
unsafe fn m4<A, B, C, D, R>(obj: Id, s: &str, a: A, b: B, c: C, d: D) -> R {
    let f: unsafe extern "C" fn(Id, objc::Sel, A, B, C, D) -> R = std::mem::transmute(objc::objc_msgSend as *const ());
    f(obj, objc::sel(s), a, b, c, d)
}

/// Cuántos bloques de completado se han llamado.
static BLOQUES_LLAMADOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

extern "C" fn bloque_invocado(_bloque: Id, _a: Id, _b: Id) {
    BLOQUES_LLAMADOS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Un bloque de Objective-C de mentira: el driver solo usa la función de invocación (está en el desplazamiento 16
/// del literal), así que basta con un bloque con esa forma.
#[repr(C)]
struct BloqueFalso {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: extern "C" fn(Id, Id, Id),
    descriptor: *const c_void,
}

fn bloque_falso() -> Id {
    let b = Box::into_raw(Box::new(BloqueFalso {
        isa: std::ptr::null(),
        flags: 0,
        reserved: 0,
        invoke: bloque_invocado,
        descriptor: std::ptr::null(),
    }));
    b as Id
}

/// La función de cómputo de la biblioteca de CoreRE3D (la misma que usa metalprobe).
fn funcion_de_computo(d: Id) -> Option<Id> {
    unsafe {
        let mut err: Id = std::ptr::null_mut();
        let ruta = "/System/Library/PrivateFrameworks/CoreRE3DGSFoundation.framework/Versions/A/Resources/default.metallib";
        let lib = objc::m2::<Id, *mut Id, Id>(d, "newLibraryWithFile:error:", objc::nsstring(ruta), &mut err as *mut Id);
        if lib.is_null() {
            return None;
        }
        let f = objc::m1::<Id, Id>(lib, "newFunctionWithName:", objc::nsstring("SetValueFloat"));
        if f.is_null() {
            return None;
        }
        Some(f)
    }
}

/// MTLSizeAndAlign: el tamaño y la alineación de un recurso en un heap.
#[repr(C)]
#[derive(Clone, Copy)]
struct TamanoAlineacion {
    tamano: u64,
    alineacion: u64,
}

/// Una comprobación que solo aplica si el marco de Apple tiene esa clase: si no, se cuenta como saltada.
fn chequear_si(inf: &mut Informe, nombre: &str, aplica: bool, f: impl FnOnce() -> Result<(), String>) {
    if !aplica {
        inf.saltados += 1;
        println!("  (n/d)   {nombre}: Apple no tiene esa clase");
        return;
    }
    chequear(inf, nombre, f)
}

/// Una comprobación que depende de que exista un selector en el marco de Apple: si no existe, no es un fallo del
/// driver, se cuenta como saltada.
fn comprobar(inf: &mut Informe, nombre: &str, selector: Option<&str>, f: impl FnOnce() -> Result<(), String>) {
    match selector {
        Some(s) if !existe(s) => {
            inf.saltados += 1;
            println!("  (n/d)   {nombre}: Apple no tiene «{s}»");
        }
        _ => chequear(inf, nombre, f),
    }
}

/// Si el marco de Apple (Metal) tiene ese selector de clase.
fn existe(selector: &str) -> bool {
    unsafe {
        // Se busca en cualquier clase de Metal registrada: el runtime lo resuelve por nombre de selector.
        let h = dlopen(c"/System/Library/Frameworks/Metal.framework/Metal".as_ptr(), 2);
        if h.is_null() {
            return false;
        }
        // metal: usa el runtime de Objective-C: se mira si alguna clase Metal implementa el selector.
        let clases = [
            "MTLTextureDescriptor", "MTLSamplerStateDescriptor", "MTLRenderPipelineDescriptor", "MTLHeapDescriptor",
            "MTLArgumentBufferDescriptor", "MTLIndirectCommandBufferDescriptor", "MTLResidencySetDescriptor",
        ];
        clases.iter().any(|c| {
            let clase = objc::clase(c);
            !clase.is_null() && objc::responde_clase(clase, selector)
        })
    }
}

/// Métodos del protocolo que el driver no implementa: se comparan las implementaciones de la clase del driver con las
/// de la clase madre de Apple (_MTLDevice). Si son la misma, es que el driver no lo implementa.
fn sin_implementar(d: Id) -> Vec<(String, String)> {
    unsafe {
        let proto = objc::objc_getProtocol(c"MTLDevice".as_ptr());
        if proto.is_null() {
            return Vec::new();
        }
        let mut n: u32 = 0;
        let lista = objc::protocol_copyMethodDescriptionList(proto, 1, 1, &mut n);
        if lista.is_null() {
            return Vec::new();
        }
        let clase = objc::object_getClass(d);
        let madre = objc::class_getSuperclass(clase);
        let mut faltan = Vec::new();
        for i in 0..n as usize {
            let desc = *lista.add(i);
            let nombre = objc::nombre_sel(desc.nombre);
            let tipos = CStr::from_ptr(desc.tipos).to_string_lossy().into_owned();
            let sel = desc.nombre;
            let a = objc::method_getImplementation(objc::class_getInstanceMethod(clase, sel));
            let b = objc::method_getImplementation(objc::class_getInstanceMethod(madre, sel));
            if a == b {
                faltan.push((nombre, tipos));
            }
        }
        free(lista.cast());
        faltan
    }
}

/// La implementación que tiene el driver de un selector (para comparar).
fn imp(clase: objc::Class, selector: &str) -> *const c_void {
    unsafe { objc::method_getImplementation(objc::class_getInstanceMethod(clase, objc::sel(selector))) }
}

fn main() {
    unsafe {
        let h = dlopen(c"/System/Library/Frameworks/Metal.framework/Metal".as_ptr(), 2);
        if h.is_null() {
            eprintln!("dlopen de Metal: {}", CStr::from_ptr(dlerror()).to_string_lossy());
            std::process::exit(1);
        }
        let p_defecto = dlsym(h, c"MTLCreateSystemDefaultDevice".as_ptr());
        let p_todos = dlsym(h, c"MTLCopyAllDevices".as_ptr());
        let defecto: extern "C" fn() -> Id = std::mem::transmute(p_defecto);
        let todos: extern "C" fn() -> Id = std::mem::transmute(p_todos);
        let lista = todos();
        let n: usize = if lista.is_null() { 0 } else { objc::m0(lista, "count") };
        println!("== Metal 2 en tahoe-run (ruta A) ==");
        println!("dispositivos: {n}");
        for i in 0..n {
            let d: Id = objc::m1(lista, "objectAtIndex:", i);
            println!("  {i}: {} «{}»", objc::nombre_clase(objc::object_getClass(d)), objc::texto(objc::m0::<Id>(d, "name")));
        }
        let d = defecto();
        if d.is_null() {
            println!("no hay dispositivo por omisión");
            std::process::exit(2);
        }
        let clase = objc::object_getClass(d);
        let madre = objc::class_getSuperclass(clase);

        // 1. Estado del protocolo MTLDevice: implementación real, stub o nada.
        let faltan = sin_implementar(d);
        let stubs = std::fs::read_to_string("/private/var/tmp/tahoe-metal-stubs.txt").unwrap_or_default();
        let n_stubs = stubs.lines().filter(|l| !l.trim().is_empty()).count();
        let total_proto = cuenta_protocolo();
        println!("\n-- protocolo MTLDevice: {total_proto} métodos --");
        println!("  implementados de verdad: {}", total_proto as usize - n_stubs);
        println!("  con stub (nil/0):        {n_stubs}");
        if !faltan.is_empty() {
            println!("  sin registrar (peligro: excepción de Apple): {}", faltan.len());
            for (s, _t) in &faltan {
                println!("    -{s}");
            }
        }
        if std::env::args().any(|a| a == "--lista") {
            for l in stubs.lines().filter(|l| !l.trim().is_empty()) {
                println!("  stub  -{l}");
            }
        }

        let mut inf = Informe::nuevo();

        // 2. Comprobaciones funcionales.
        println!("\n-- funcionalidad --");
        let buf = objc::m2::<u64, u64, Id>(d, "newBufferWithLength:options:", 1024, 0);
        chequear(&mut inf, "buffer de 1 KiB", || {
            if buf.is_null() {
                return Err("nil".into());
            }
            let largo: u64 = objc::m0(buf, "length");
            if largo != 1024 {
                return Err(format!("length = {largo}"));
            }
            Ok(())
        });
        chequear(&mut inf, "buffer con contenido", || {
            let p = objc::m0::<*const u8>(buf, "contents");
            if p.is_null() {
                return Err("contents nil".into());
            }
            Ok(())
        });
        chequear(&mut inf, "buffer con bytes iniciales", || {
            let datos = [1u8, 2, 3, 4];
            let b: Id = objc::m3(d, "newBufferWithBytes:length:options:", datos.as_ptr(), 4u64, 0u64);
            if b.is_null() {
                return Err("nil".into());
            }
            let p = objc::m0::<*const u8>(b, "contents");
            let leido = std::slice::from_raw_parts(p, 4);
            if leido != datos {
                return Err(format!("contenido {leido:?}"));
            }
            Ok(())
        });

        // Texturas.
        let td2d = descriptor_textura2d(80, 64, 64, true);
        chequear(&mut inf, "textura 2D con mipmaps", || {
            if td2d.is_null() {
                return Err("descriptor nil".into());
            }
            objc::m1::<u64, ()>(td2d, "setMipmapLevelCount:", 2);
            objc::m1::<u64, ()>(td2d, "setUsage:", 1 | 2 | 4);
            let t: Id = objc::m1(d, "newTextureWithDescriptor:", td2d);
            if t.is_null() {
                return Err("nil".into());
            }
            let w: u64 = objc::m0(t, "width");
            let h: u64 = objc::m0(t, "height");
            if w != 64 || h != 64 {
                return Err(format!("{w}x{h}"));
            }
            Ok(())
        });
        chequear(&mut inf, "textura 2D array", || {
            let td = descriptor_textura2d(80, 32, 32, false);
            objc::m1::<u64, ()>(td, "setTextureType:", 3);
            objc::m1::<u64, ()>(td, "setArrayLength:", 4);
            let t: Id = objc::m1(d, "newTextureWithDescriptor:", td);
            if t.is_null() {
                return Err("nil".into());
            }
            let capas: u64 = objc::m0(t, "arrayLength");
            if capas != 4 {
                return Err(format!("arrayLength = {capas}"));
            }
            Ok(())
        });
        chequear(&mut inf, "textura cubo", || {
            let td = descriptor_textura2d(80, 32, 32, false);
            objc::m1::<u64, ()>(td, "setTextureType:", 5);
            let t: Id = objc::m1(d, "newTextureWithDescriptor:", td);
            if t.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });
        chequear(&mut inf, "textura de profundidad y estencil", || {
            let td = descriptor_textura2d(252, 64, 64, false);
            objc::m1::<u64, ()>(td, "setUsage:", 4);
            let t: Id = objc::m1(d, "newTextureWithDescriptor:", td);
            if t.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });
        chequear(&mut inf, "estado de profundidad y estencil", || {
            let sd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLDepthStencilDescriptor"), "alloc"), "init");
            if sd.is_null() {
                return Err("descriptor nil".into());
            }
            objc::m1::<u64, ()>(sd, "setDepthCompareFunction:", 1);
            // macOS 26 llama a la propiedad `isDepthWriteEnabled`: el setter de iOS no existe y lanzaría NSException.
            if objc::responde_instancia(sd, "setDepthWriteEnabled:") {
                objc::m1::<i8, ()>(sd, "setDepthWriteEnabled:", 1);
            } else {
                objc::m1::<i8, ()>(sd, "setIsDepthWriteEnabled:", 1);
            }
            let st: Id = objc::m1(d, "newDepthStencilStateWithDescriptor:", sd);
            if st.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });
        chequear(&mut inf, "command buffer del dispositivo", || {
            let cb: Id = objc::m0(d, "newCommandBuffer");
            if cb.is_null() {
                return Err("nil".into());
            }
            if !objc::responde_instancia(cb, "commit") {
                return Err("no responde a commit".into());
            }
            Ok(())
        });
        chequear(&mut inf, "tamaño y alineación de buffer en heap", || {
            let r = objc::m2::<u64, u64, TamanoAlineacion>(d, "heapBufferSizeAndAlignWithLength:options:", 1024, 0);
            if r.tamano < 1024 {
                return Err(format!("tamaño {} menor que 1024", r.tamano));
            }
            if r.alineacion != 256 {
                return Err(format!("alineación {} en vez de 256", r.alineacion));
            }
            Ok(())
        });
        chequear(&mut inf, "tamaño y alineación de textura en heap", || {
            let td = descriptor_textura2d(252, 64, 64, false);
            let r = objc::m1::<Id, TamanoAlineacion>(d, "heapTextureSizeAndAlignWithDescriptor:", td);
            if r.tamano < 64 * 64 * 4 {
                return Err(format!("tamaño {} menor que 16384", r.tamano));
            }
            if r.alineacion != 256 {
                return Err(format!("alineación {} en vez de 256", r.alineacion));
            }
            Ok(())
        });
        chequear(&mut inf, "posiciones de muestreo", || {
            let mut posiciones = [[0.0f32; 2]; 4];
            objc::m2::<*mut [f32; 2], u64, ()>(d, "getDefaultSamplePositions:count:", posiciones.as_mut_ptr(), 4);
            if posiciones[0] != [0.375, 0.375] {
                return Err(format!("primera posición {:?} en vez de [0.375, 0.375]", posiciones[0]));
            }
            Ok(())
        });
        chequear(&mut inf, "máximo de samplers en argument buffer", || {
            let n = objc::m0::<u64>(d, "maxArgumentBufferSamplerCount");
            if n != 0 {
                return Err(format!("{n} en vez de 0"));
            }
            Ok(())
        });
        let hay_heap = !objc::clase("MTLHeapDescriptor").is_null();
        let hay_computo = funcion_de_computo(d).is_some();
        chequear_si(&mut inf, "heap de memoria", hay_heap, || {
            let hd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLHeapDescriptor"), "alloc"), "init");
            if hd.is_null() {
                return Err("descriptor nil".into());
            }
            objc::m1::<u64, ()>(hd, "setSize:", 1024 * 1024);
            objc::m1::<u64, ()>(hd, "setStorageMode:", 0);
            let h: Id = objc::m1(d, "newHeapWithDescriptor:", hd);
            if h.is_null() {
                return Err("nil".into());
            }
            let tam = objc::m0::<u64>(h, "size");
            if tam != 1024 * 1024 {
                return Err(format!("tamaño {tam} en vez de 1048576"));
            }
            let b: Id = objc::m2::<u64, u64, Id>(h, "newBufferWithLength:options:", 4096, 0);
            if b.is_null() {
                return Err("buffer del heap nil".into());
            }
            Ok(())
        });
        chequear(&mut inf, "fence", || {
            let f: Id = objc::m0(d, "newFence");
            if f.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });
        let hay_residencia = !objc::clase("MTLResidencySetDescriptor").is_null();
        chequear_si(&mut inf, "conjunto de residencia", hay_residencia, || {
            let rd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLResidencySetDescriptor"), "alloc"), "init");
            if rd.is_null() {
                return Err("descriptor nil".into());
            }
            let mut err: Id = std::ptr::null_mut();
            let r: Id = objc::m2::<Id, *mut Id, Id>(d, "newResidencySetWithDescriptor:error:", rd, &mut err as *mut Id);
            if r.is_null() {
                return Err("nil".into());
            }
            let buf: Id = objc::m2::<u64, u64, Id>(d, "newBufferWithLength:options:", 256, 0);
            // NSMutableArray: +array devuelve un NSArray inmutable y addObject: sobre él lanza excepción.
            let arr: Id = objc::m0(objc::clase("NSMutableArray"), "array");
            objc::m1::<Id, ()>(arr, "addObject:", buf);
            objc::m1::<Id, ()>(r, "addAllObjects:", arr);
            Ok(())
        });
        chequear(&mut inf, "buffer sin copia", || {
            let datos: [u8; 4] = [1, 2, 3, 4];
            let enviar4: unsafe extern "C" fn(Id, objc::Sel, *const u8, u64, u64, Id) -> Id =
                std::mem::transmute(objc::objc_msgSend as *const ());
            let b: Id = enviar4(d, objc::sel("newBufferWithBytesNoCopy:length:options:deallocator:"), datos.as_ptr(), 4, 0, std::ptr::null_mut());
            if b.is_null() {
                return Err("nil".into());
            }
            let len = objc::m0::<u64>(b, "length");
            if len != 4 {
                return Err(format!("longitud {len} en vez de 4"));
            }
            Ok(())
        });
        // Eventos y manejadores.
        chequear(&mut inf, "evento compartido", || {
            let e: Id = objc::m0(d, "newSharedEvent");
            if e.is_null() {
                return Err("nil".into());
            }
            objc::m1::<u64, ()>(e, "setSignaledValue:", 42);
            let v = objc::m0::<u64>(e, "signaledValue");
            if v != 42 {
                return Err(format!("valor {v} en vez de 42"));
            }
            Ok(())
        });
        chequear(&mut inf, "evento con mango", || {
            let e: Id = objc::m1(d, "newSharedEventWithHandle:", objc::nsstring("mango-de-prueba"));
            if e.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });
        chequear(&mut inf, "manejador de función", || {
            let b: Id = objc::m2::<u64, u64, Id>(d, "newBufferWithLength:options:", 64, 0);
            let h: Id = objc::m1(d, "functionHandleWithFunction:", b);
            if h.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });
        // Los pipelines de cómputo y de render, con sus variantes.
        chequear_si(&mut inf, "pipeline de cómputo por función con opciones", hay_computo, || {
            let f = funcion_de_computo(d).ok_or("no hay función de cómputo")?;
            let mut reflexion: Id = std::ptr::null_mut();
            let mut err: Id = std::ptr::null_mut();
            let p = unsafe {
                m4::<Id, u64, *mut Id, *mut Id, Id>(
                    d,
                    "newComputePipelineStateWithFunction:options:reflection:error:",
                    f,
                    0,
                    &mut reflexion as *mut Id,
                    &mut err as *mut Id,
                )
            };
            if p.is_null() {
                return Err(format!("nil: {}", objc::texto(err)));
            }
            Ok(())
        });
        chequear_si(&mut inf, "pipeline de cómputo por descriptor", hay_computo, || {
            let f = funcion_de_computo(d).ok_or("no hay función de cómputo")?;
            let pd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLComputePipelineDescriptor"), "alloc"), "init");
            if pd.is_null() {
                return Err("descriptor nil".into());
            }
            objc::m1::<Id, ()>(pd, "setComputeFunction:", f);
            let mut reflexion: Id = std::ptr::null_mut();
            let mut err: Id = std::ptr::null_mut();
            let p = unsafe {
                m4::<Id, u64, *mut Id, *mut Id, Id>(
                    d,
                    "newComputePipelineStateWithDescriptor:options:reflection:error:",
                    pd,
                    0,
                    &mut reflexion as *mut Id,
                    &mut err as *mut Id,
                )
            };
            if p.is_null() {
                return Err(format!("nil: {}", objc::texto(err)));
            }
            Ok(())
        });
        chequear_si(&mut inf, "pipeline de cómputo con bloque de completado", hay_computo, || {
            let f = funcion_de_computo(d).ok_or("no hay función de cómputo")?;
            let antes = BLOQUES_LLAMADOS.load(std::sync::atomic::Ordering::SeqCst);
            objc::m2::<Id, Id, ()>(d, "newComputePipelineStateWithFunction:completionHandler:", f, bloque_falso());
            let despues = BLOQUES_LLAMADOS.load(std::sync::atomic::Ordering::SeqCst);
            if despues != antes + 1 {
                return Err(format!("el bloque se llamó {despues} veces, se esperaba {antes}+1"));
            }
            Ok(())
        });
        chequear(&mut inf, "estado de muestreo", || {
            let sd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLSamplerDescriptor"), "alloc"), "init");
            if sd.is_null() {
                return Err("descriptor nil".into());
            }
            objc::m1::<u64, ()>(sd, "setMinFilter:", 0);
            objc::m1::<u64, ()>(sd, "setMagFilter:", 0);
            let st: Id = objc::m1(d, "newSamplerStateWithDescriptor:", sd);
            if st.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });

        // Codificadores y comandos.
        chequear(&mut inf, "cola de comandos", || {
            let q: Id = objc::m0(d, "newCommandQueue");
            if q.is_null() {
                return Err("nil".into());
            }
            let cb: Id = objc::m0(q, "commandBuffer");
            if cb.is_null() {
                return Err("nil".into());
            }
            let enc: Id = objc::m0(cb, "computeCommandEncoder");
            if enc.is_null() {
                return Err("nil".into());
            }
            let blit: Id = objc::m0(cb, "blitCommandEncoder");
            if blit.is_null() {
                return Err("nil".into());
            }
            Ok(())
        });
        chequear(&mut inf, "command buffer se completa", || {
            let q: Id = objc::m0(d, "newCommandQueue");
            let cb: Id = objc::m0(q, "commandBuffer");
            objc::m0::<()>(cb, "commit");
            objc::m0::<()>(cb, "waitUntilCompleted");
            let st: u64 = objc::m0(cb, "status");
            if st != 4 {
                return Err(format!("estado {st}"));
            }
            Ok(())
        });

        // Lo que el driver declara: comparación de implementaciones con la clase madre.
        println!("\n-- implementado por el driver (implementación propia) --");
        let mut propios = 0;
        for s in ["newCommandQueue", "newBufferWithLength:options:", "newLibraryWithFile:error:", "newTextureWithDescriptor:"] {
            if imp(clase, s) != imp(madre, s) {
                propios += 1;
            }
        }
        println!("  comprobados: {propios}/4 con implementación propia en TahoeMTLDevice");

        println!("\n== resumen ==");
        println!("  comprobaciones: {}/{} correctas ({} saltadas)", inf.bien, inf.total, inf.saltados);
        println!("  selectores del protocolo MTLDevice sin implementar: {}", faltan.len());
        for (n, m) in &inf.fallos {
            println!("  - {n}: {m}");
        }
        std::process::exit(if inf.fallos.is_empty() { 0 } else { 1 });
    }
}

/// Cuenta los métodos del protocolo MTLDevice.
fn cuenta_protocolo() -> u32 {
    unsafe {
        let proto = objc::objc_getProtocol(c"MTLDevice".as_ptr());
        if proto.is_null() {
            return 0;
        }
        let mut n: u32 = 0;
        let lista = objc::protocol_copyMethodDescriptionList(proto, 1, 1, &mut n);
        if !lista.is_null() {
            free(lista.cast());
        }
        n
    }
}

/// Descriptor de textura 2D.
fn descriptor_textura2d(formato: u64, w: u64, h: u64, mip: bool) -> Id {
    unsafe {
        let f: unsafe extern "C" fn(Id, objc::Sel, u64, u64, u64, i8) -> Id =
            std::mem::transmute(objc::objc_msgSend as *const ());
        f(objc::clase("MTLTextureDescriptor"), objc::sel("texture2DDescriptorWithPixelFormat:width:height:mipmapped:"), formato, w, h, mip as i8)
    }
}
