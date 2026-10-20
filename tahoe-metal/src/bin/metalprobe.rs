//! metalprobe: prueba de Metal dentro del macOS de tahoe-run. Lista los MTLDevice (MTLCopyAllDevices) y crea el de
//! por omisión (MTLCreateSystemDefaultDevice), con su clase y su nombre.

use std::ffi::{c_char, c_int, c_void, CStr};

#[path = "../objc.rs"]
#[allow(dead_code)]
mod objc;

use objc::Id;

/// Anota una línea en un fichero: si el proceso aborta, lo que iba por stdout se pierde.
fn nota(texto: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/private/var/tmp/nota.txt") {
        let _ = writeln!(f, "{texto}");
    }
}

/// Un NSRange (de Foundation): dos enteros de 64 bits.
#[repr(C)]
#[derive(Clone, Copy)]
struct Rango {
    loc: u64,
    len: u64,
}

extern "C" {
    fn dlopen(p: *const c_char, modo: c_int) -> *mut c_void;
    fn dlsym(h: *mut c_void, s: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

fn describir(d: Id) -> String {
    if d.is_null() {
        return "(nil)".into();
    }
    // SAFETY: d es un MTLDevice.
    unsafe { format!("{} «{}»", objc::nombre_clase(objc::object_getClass(d)), objc::texto(objc::m0::<Id>(d, "name"))) }
}

fn main() {
    // SAFETY: Metal.framework está en la caché de dyld; los símbolos tienen esas firmas.
    unsafe {
        let h = dlopen(c"/System/Library/Frameworks/Metal.framework/Metal".as_ptr(), 2);
        if h.is_null() {
            eprintln!("dlopen de Metal: {}", CStr::from_ptr(dlerror()).to_string_lossy());
            std::process::exit(1);
        }
        let todos = dlsym(h, c"MTLCopyAllDevices".as_ptr());
        let defecto = dlsym(h, c"MTLCreateSystemDefaultDevice".as_ptr());
        let todos: extern "C" fn() -> Id = std::mem::transmute(todos);
        let defecto: extern "C" fn() -> Id = std::mem::transmute(defecto);
        let lista = todos();
        let n: usize = if lista.is_null() { 0 } else { objc::m0(lista, "count") };
        println!("MTLCopyAllDevices: {n} dispositivos");
        for i in 0..n {
            let d: Id = objc::m1(lista, "objectAtIndex:", i);
            println!("  {i}: {}", describir(d));
        }
        let d = defecto();
        println!("MTLCreateSystemDefaultDevice: {}", describir(d));
        if d.is_null() {
            std::process::exit(2);
        }
        if std::env::args().nth(1).as_deref() == Some("computo") {
            std::process::exit(computo(d));
        }
        if std::env::args().nth(1).as_deref() == Some("render") {
            std::process::exit(render(d));
        }
        if std::env::args().nth(1).as_deref() == Some("funciones") {
            std::process::exit(funciones(d));
        }
        if std::env::args().nth(1).as_deref() == Some("descriptor") {
            std::process::exit(descriptor(d));
        }
        if std::env::args().nth(1).as_deref() == Some("plano") {
            std::process::exit(plano(d));
        }
        if std::env::args().nth(1).as_deref() == Some("constante") {
            std::process::exit(plano_con(d, true));
        }
        if std::env::args().nth(1).as_deref() == Some("cadena") {
            std::process::exit(cadena(d));
        }
        std::process::exit(0);
    }
}

/// Recorrido completo con cualquier metallib: carga la biblioteca, pide dos funciones por descriptor (como hace
/// CoreAnimation), crea el pipeline de render, ejecuta un pase con un triángulo y comprueba la imagen resultante.
/// Es el test de regresión del driver: uso: metalprobe cadena <metallib> [vertice] [fragmento].
fn cadena(d: Id) -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let ruta = args.get(2).cloned().unwrap_or_default();
    if ruta.is_empty() {
        println!("uso: metalprobe cadena <metallib> [vertice] [fragmento]");
        return 1;
    }
    unsafe {
        let mut err: Id = std::ptr::null_mut();
        let lib: Id = objc::m2(d, "newLibraryWithFile:error:", objc::nsstring(&ruta), &mut err as *mut Id);
        if lib.is_null() {
            println!("biblioteca {ruta}: nil");
            return 3;
        }
        let nombres: Id = objc::m0(lib, "functionNames");
        let n: u64 = objc::m0(nombres, "count");
        let todos: Vec<String> = (0..n).map(|i| objc::texto(objc::m1(nombres, "objectAtIndex:", i))).collect();
        println!("biblioteca {ruta}: {n} funciones");
        // Los shaders pedidos: los da la orden, o los primeros con «vert» y «frag» en el nombre.
        let (vn, fnn) = match (args.get(3), args.get(4)) {
            (Some(v), Some(f)) => (v.clone(), f.clone()),
            _ => (
                todos.iter().find(|s| s.contains("vert")).cloned().unwrap_or_default(),
                todos.iter().find(|s| s.contains("frag")).cloned().unwrap_or_default(),
            ),
        };
        println!("shaders: vertice={vn} fragmento={fnn}");
        if vn.is_empty() || fnn.is_empty() {
            println!("no hay shaders de vértice/fragmento en la biblioteca");
            return 4;
        }
        // La función, por descriptor (como hace CoreAnimation en macOS 26: alloc + setName:).
        let por_descriptor = |lib: Id, nombre: &str| -> Id {
            let desc: Id = objc::m0(objc::clase("MTLFunctionDescriptor"), "alloc");
            objc::m1::<Id, ()>(desc, "setName:", objc::nsstring(nombre));
            nota("creando la función por descriptor");
            let f: Id = objc::m2(lib, "newFunctionWithDescriptor:error:", desc, std::ptr::null_mut::<Id>());
            nota(&format!("función: {}", if f.is_null() { "nil" } else { "ok" }));
            f
        };
        let vf = por_descriptor(lib, &vn);
        let ff = por_descriptor(lib, &fnn);
        println!("funciones: vertex={} fragment={}", if vf.is_null() { "nil" } else { "ok" }, if ff.is_null() { "nil" } else { "ok" });
        if vf.is_null() || ff.is_null() {
            return 5;
        }
        // El pipeline de render.
        let pd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLRenderPipelineDescriptor"), "alloc"), "init");
        objc::m1::<Id, ()>(pd, "setVertexFunction:", vf);
        objc::m1::<Id, ()>(pd, "setFragmentFunction:", ff);
        let ca0: Id = objc::m1(objc::m0::<Id>(pd, "colorAttachments"), "objectAtIndexedSubscript:", 0u64);
        objc::m1::<u64, ()>(ca0, "setPixelFormat:", 80);
        let p: Id = objc::m2(d, "newRenderPipelineStateWithDescriptor:error:", pd, &mut err as *mut Id);
        if p.is_null() {
            println!("pipeline: nil ({})", objc::texto(if err.is_null() { err } else { objc::m0(err, "localizedDescription") }));
            return 6;
        }
        println!("pipeline: ok");
        // Un pase sobre una textura BGRA8 de 64x64, con un triángulo.
        let td: Id = {
            let f: unsafe extern "C" fn(Id, objc::Sel, u64, u64, u64, i8) -> Id = std::mem::transmute(objc::objc_msgSend as *const ());
            f(objc::clase("MTLTextureDescriptor"), objc::sel("texture2DDescriptorWithPixelFormat:width:height:mipmapped:"), 80, 64, 64, 0)
        };
        objc::m1::<u64, ()>(td, "setUsage:", 5);
        let tex: Id = objc::m1(d, "newTextureWithDescriptor:", td);
        if tex.is_null() {
            println!("textura: nil");
            return 7;
        }
        let vs: [f32; 24] = [-1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let vb: Id = objc::m3(d, "newBufferWithBytes:length:options:", vs.as_ptr(), 96u64, 0u64);
        let mut ident = [0f32; 16];
        for i in 0..4 {
            ident[i * 4 + i] = 1.0;
        }
        let rp: Id = objc::m0(objc::clase("MTLRenderPassDescriptor"), "renderPassDescriptor");
        let a0: Id = objc::m1(objc::m0::<Id>(rp, "colorAttachments"), "objectAtIndexedSubscript:", 0u64);
        objc::m1::<Id, ()>(a0, "setTexture:", tex);
        objc::m1::<u64, ()>(a0, "setLoadAction:", 2);
        objc::m1::<u64, ()>(a0, "setStoreAction:", 1);
        objc::m1::<MTLClearColor, ()>(a0, "setClearColor:", MTLClearColor { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        let q: Id = objc::m0(d, "newCommandQueue");
        let cb: Id = objc::m0(q, "commandBuffer");
        let enc: Id = objc::m1(cb, "renderCommandEncoderWithDescriptor:", rp);
        objc::m1::<Id, ()>(enc, "setRenderPipelineState:", p);
        objc::m3::<Id, u64, u64, ()>(enc, "setVertexBuffer:offset:atIndex:", vb, 0, 0);
        objc::m3::<*const f32, u64, u64, ()>(enc, "setVertexBytes:length:atIndex:", ident.as_ptr(), 64, 2);
        objc::m3::<u64, u64, u64, ()>(enc, "drawPrimitives:vertexStart:vertexCount:", 3, 0, 3);
        objc::m0::<()>(enc, "endEncoding");
        objc::m0::<()>(cb, "commit");
        objc::m0::<()>(cb, "waitUntilCompleted");
        let st: u64 = objc::m0(cb, "status");
        let mut px = vec![0u8; 64 * 64 * 4];
        {
            let f: unsafe extern "C" fn(Id, objc::Sel, *mut u8, u64, MTLRegion, u64) = std::mem::transmute(objc::objc_msgSend as *const ());
            f(tex, objc::sel("getBytes:bytesPerRow:fromRegion:mipmapLevel:"), px.as_mut_ptr(), 256, MTLRegion { x: 0, y: 0, z: 0, w: 64, h: 64, d: 1 }, 0);
        }
        let punto = |x: usize, y: usize| -> [u8; 4] { px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].try_into().unwrap() };
        let (ai, ad) = (punto(8, 56), punto(56, 8));
        println!("command buffer: estado {st}; abajo a la izquierda BGRA {ai:?}; arriba a la derecha {ad:?}");
        // El triángulo cubre media pantalla: se dibuja algo y el resto es el color de limpieza.
        let dibuja = px.chunks(4).any(|p| p != [0, 0, 0, 255] && p != [0, 0, 0, 0]);
        println!("cadena: {}", if dibuja { "CORRECTA" } else { "VACÍA" });
        if dibuja { 0 } else { 8 }
    }
}

/// Reproduce lo que hace el compositor: sus shaders de plano (plane__vertex y plane_color_fragment, que en macOS 26
/// están en RenderBox) y los datos que les pasa: un buffer de «globals» de 36 bytes en el índice 0 y cuatro vértices.
///
/// El vertex shader hace pos = x*g0 + y*g1 + g2 (las esquinas del quad salen del vertex_id) y el fragment shader
/// escribe el color que viene en los globals (28 bytes del plano + 8 del color) en el primer attachment y la
/// cobertura en el segundo.
///
/// Con `constante` se pone además la function constant `_shader_state` (un uint4) como pone el compositor: con el
/// bit 0x80000 del componente 2, el shader fuerza el alfa a 1 en vez de usar el del buffer.
fn plano(d: Id) -> i32 {
    plano_con(d, false)
}

fn plano_con(d: Id, constante: bool) -> i32 {
    const RUTA: &str = "/System/Library/PrivateFrameworks/RenderBox.framework/Versions/A/Resources/default.metallib";
    const COLOR: u64 = 80;    // MTLPixelFormatBGRA8Unorm
    const COVERAGE: u64 = 65; // MTLPixelFormatRG16Float (el shader escribe half2 en el segundo attachment)
    unsafe {
        let mut err: Id = std::ptr::null_mut();
        let lib: Id = objc::m2(d, "newLibraryWithFile:error:", objc::nsstring(RUTA), &mut err as *mut Id);
        if lib.is_null() {
            println!("biblioteca: nil");
            return 3;
        }
        // La función, por descriptor (como hace el compositor). Los valores de la constante se ponen en el
        // objeto que da el propio descriptor: en macOS 26, MTLFunctionConstantValues es una fachada y el objeto
        // de verdad es un MTLFunctionConstantValuesInternal, que es el único que acepta los valores.
        let por_descriptor = |lib: Id, nombre: &str, con_constante: bool| -> Id {
            let desc: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLFunctionDescriptor"), "alloc"), "init");
            objc::m1::<Id, ()>(desc, "setName:", objc::nsstring(nombre));
            if con_constante {
                // Los valores: un uint4 con el bit 0x80000 en el componente 2 (el shader fuerza el alfa a 1).
                let valores: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLFunctionConstantValues"), "alloc"), "init");
                nota(&format!("valores: {}", objc::nombre_clase(objc::object_getClass(valores))));
                let mut v = [0u32; 4];
                v[2] = 0x80000;
                // Una constante vectorial se pone con el rango: cuatro escalares (MTLDataType 29 = un entero
                // de 32 bits) en los índices 0..3.
                let r = Rango { loc: 0, len: 4 };
                nota("poniendo la constante por rango");
                objc::m3::<*const u32, u64, Rango, ()>(valores, "setConstantValues:type:withRange:", v.as_ptr(), 29, r);
                nota("constante puesta");
                // Con los valores, la función se pide por nombre (la vía que usa el driver).
                let f: Id = objc::m3::<Id, Id, *mut Id, Id>(lib, "newFunctionWithName:constantValues:error:", objc::nsstring(nombre), valores, std::ptr::null_mut::<Id>());
                nota(&format!("función con constantes: {}", if f.is_null() { "nil" } else { "ok" }));
                return f;
            }
            objc::m2(lib, "newFunctionWithDescriptor:error:", desc, std::ptr::null_mut::<Id>())
        };
        let vf = por_descriptor(lib, "plane__vertex", false);
        let ff = por_descriptor(lib, "plane_color_fragment", constante);
        if vf.is_null() || ff.is_null() {
            println!("shaders: vertex={} fragment={}", if vf.is_null() { "nil" } else { "ok" }, if ff.is_null() { "nil" } else { "ok" });
            return 4;
        }
        // El pipeline: el color y la cobertura, y la topología de tira (el compositor dibuja un quad).
        let pd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLRenderPipelineDescriptor"), "alloc"), "init");
        objc::m1::<Id, ()>(pd, "setVertexFunction:", vf);
        objc::m1::<Id, ()>(pd, "setFragmentFunction:", ff);
        let cas: Id = objc::m0(pd, "colorAttachments");
        let c0: Id = objc::m1(cas, "objectAtIndexedSubscript:", 0u64);
        objc::m1::<u64, ()>(c0, "setPixelFormat:", COLOR);
        let c1: Id = objc::m1(cas, "objectAtIndexedSubscript:", 1u64);
        objc::m1::<u64, ()>(c1, "setPixelFormat:", COVERAGE);
        // El runtime solo admite 0..3 aquí (Metal compara con 4 y falla); el tipo de primitiva va en el dibujo.
        objc::m1::<u64, ()>(pd, "setInputPrimitiveTopology:", 3); // MTLPrimitiveTypeTriangle
        let p: Id = objc::m2(d, "newRenderPipelineStateWithDescriptor:error:", pd, &mut err as *mut Id);
        if p.is_null() {
            println!("pipeline: nil ({})", objc::texto(if err.is_null() { err } else { objc::m0(err, "localizedDescription") }));
            return 5;
        }
        println!("pipeline: ok");
        // Los «globals»: 28 bytes del plano (la matriz de 3 float2 y la profundidad) y 8 del color (4 half).
        let mut g = [0u8; 36];
        for (o, v) in [(0usize, 2.0f32), (4, 0.0), (8, 0.0), (12, 2.0), (16, -1.0), (20, -1.0), (24, 0.0)] {
            g[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        // El color: (1.0, 0.5, 0.25) con alfa 0.5 (así se ve el efecto de la constante).
        for (o, v) in [(28usize, 0x3c00u16), (30, 0x3800), (32, 0x3400), (34, 0x3800)] {
            g[o..o + 2].copy_from_slice(&v.to_le_bytes());
        }
        let gb: Id = objc::m3(d, "newBufferWithBytes:length:options:", g.as_ptr(), 36u64, 0u64);
        // Las texturas: el color (BGRA8) y la cobertura (RG16Float).
        let textura = |formato: u64| -> Id {
            let f: unsafe extern "C" fn(Id, objc::Sel, u64, u64, u64, i8) -> Id = std::mem::transmute(objc::objc_msgSend as *const ());
            let td: Id = f(objc::clase("MTLTextureDescriptor"), objc::sel("texture2DDescriptorWithPixelFormat:width:height:mipmapped:"), formato, 64, 64, 0);
            objc::m1::<u64, ()>(td, "setUsage:", 5);
            objc::m1(d, "newTextureWithDescriptor:", td)
        };
        let tex = textura(COLOR);
        let cob = textura(COVERAGE);
        if tex.is_null() || cob.is_null() {
            println!("texturas: nil");
            return 6;
        }
        // El pase, con los dos attachments.
        let rp: Id = objc::m0(objc::clase("MTLRenderPassDescriptor"), "renderPassDescriptor");
        let ras: Id = objc::m0(rp, "colorAttachments");
        for (i, t) in [tex, cob].iter().enumerate() {
            let a: Id = objc::m1(ras, "objectAtIndexedSubscript:", i as u64);
            objc::m1::<Id, ()>(a, "setTexture:", *t);
            objc::m1::<u64, ()>(a, "setLoadAction:", 2);
            objc::m1::<u64, ()>(a, "setStoreAction:", 1);
        }
        let q: Id = objc::m0(d, "newCommandQueue");
        let cb: Id = objc::m0(q, "commandBuffer");
        let enc: Id = objc::m1(cb, "renderCommandEncoderWithDescriptor:", rp);
        objc::m1::<Id, ()>(enc, "setRenderPipelineState:", p);
        objc::m3::<Id, u64, u64, ()>(enc, "setVertexBuffer:offset:atIndex:", gb, 0, 0);
        objc::m3::<Id, u64, u64, ()>(enc, "setFragmentBuffer:offset:atIndex:", gb, 0, 0);
        // El quad: cuatro vértices; el shader elige la esquina con el vertex_id.
        // El tipo de primitiva va en la llamada: 4 = MTLPrimitiveTypeTriangleStrip.
        objc::m3::<u64, u64, u64, ()>(enc, "drawPrimitives:vertexStart:vertexCount:", 4, 0, 4);
        objc::m0::<()>(enc, "endEncoding");
        objc::m0::<()>(cb, "commit");
        objc::m0::<()>(cb, "waitUntilCompleted");
        let st: u64 = objc::m0(cb, "status");
        let mut px = vec![0u8; 64 * 64 * 4];
        {
            let f: unsafe extern "C" fn(Id, objc::Sel, *mut u8, u64, MTLRegion, u64) = std::mem::transmute(objc::objc_msgSend as *const ());
            f(tex, objc::sel("getBytes:bytesPerRow:fromRegion:mipmapLevel:"), px.as_mut_ptr(), 256, MTLRegion { x: 0, y: 0, z: 0, w: 64, h: 64, d: 1 }, 0);
        }
        let punto = |x: usize, y: usize| -> [u8; 4] { px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].try_into().unwrap() };
        println!("command buffer: estado {st}; centro BGRA {:?}; esquina {:?}", punto(32, 32), punto(0, 0));
        // El color de los globals es (1.0, 0.5, 0.25, 0.5): en BGRA8 son B=64, G=128, R=255, A=128.
        // Con la constante puesta, el shader fuerza el alfa a 1 (A=255).
        let esperado: [u8; 4] = if constante { [64, 128, 255, 255] } else { [64, 128, 255, 128] };
        let bien = punto(32, 32) == esperado && punto(0, 0) == esperado;
        println!("plano{}: {}", if constante { " con constante" } else { "" }, if bien { "CORRECTO" } else { "MAL" });
        if bien { 0 } else { 7 }
    }
}

/// Los métodos de MTLFunctionDescriptor (clase e instancia) y un intento de crear uno: el compositor carga sus shaders
/// por descriptor, así que hay que saber cómo se hace.
fn descriptor(_d: Id) -> i32 {
    unsafe {
        let clase = objc::clase("MTLFunctionDescriptor");
        if clase.is_null() {
            println!("no está MTLFunctionDescriptor");
            return 7;
        }
        println!("MTLFunctionDescriptor: clase={}", objc::nombre_clase(clase));
        // métodos de clase
        let cm = objc::responde_clase(clase, "functionDescriptorWithName:");
        println!("  +functionDescriptorWithName: {}", if cm { "existe" } else { "no existe" });
        let cm2 = objc::responde_clase(clase, "new");
        println!("  +new: {}", if cm2 { "existe" } else { "no existe" });
        // candidatos: de clase y de instancia
        for s in [
            "functionDescriptorWithName:",
            "descriptorWithName:",
            "newFunctionDescriptorWithName:",
        ] {
            println!("  clase +{s}: {}", objc::responde_clase(clase, s));
        }
        let desc: Id = objc::m0(clase, "alloc");
        for s in [
            "functionName",
            "name",
            "setFunctionName:",
            "setName:",
            "constantValues",
            "setConstantValues:",
            "specializationConstants",
            "setSpecializationConstants:",
            "label",
            "setLabel:",
            "initWithFunctionName:",
            "initWithName:",
        ] {
            println!("  instancia -{s}: {}", objc::responde_clase(objc::object_getClass(desc), s) || objc::responde_instancia(desc, s));
        }
        // intentos, uno por orden; si uno lanza una excepción de Objective-C el programa muere, así que se prueban
        // por separado desde la línea de órdenes.
        let modo = std::env::args().nth(2).unwrap_or_default();
        match modo.as_str() {
            "nombre" => {
                // En macOS 26 el descriptor se crea con alloc/init y el nombre se pone con setName:.
                let desc: Id = objc::m0(clase, "alloc");
                objc::m1::<Id, ()>(desc, "setName:", objc::nsstring("std_vert0_lpf"));
                let nombre: Id = objc::m0(desc, "name");
                println!("descriptor: nombre={} clase={}", objc::texto(nombre), objc::nombre_clase(objc::object_getClass(desc)));
                let lib: Id = objc::m2(
                    _d,
                    "newLibraryWithFile:error:",
                    objc::nsstring("/System/Library/Frameworks/QuartzCore.framework/Versions/A/Resources/default.metallib"),
                    std::ptr::null_mut::<Id>(),
                );
                if !lib.is_null() {
                    let f: Id = objc::m2(lib, "newFunctionWithDescriptor:error:", desc, std::ptr::null_mut::<Id>());
                    println!("función por descriptor: {}", if f.is_null() { "nil" } else { "ok" });
                }
            }
            "alloc" => {
                let desc: Id = objc::m0(clase, "alloc");
                println!("alloc: {}", if desc.is_null() { "nil" } else { "ok" });
            }
            _ => {
                println!("uso: metalprobe descriptor [nombre|alloc]");
            }
        }
        0
    }
}

/// Prueba de las funciones de una biblioteca: los nombres que da, y si se pueden crear por nombre y por descriptor
/// (esto es lo que hace el compositor con los shaders de CoreAnimation).
fn funciones(d: Id) -> i32 {
    const RUTA: &str = "/System/Library/Frameworks/QuartzCore.framework/Versions/A/Resources/default.metallib";
    unsafe {
        let mut err: Id = std::ptr::null_mut();
        let lib: Id = objc::m2(d, "newLibraryWithFile:error:", objc::nsstring(RUTA), &mut err as *mut Id);
        if lib.is_null() {
            println!("biblioteca: nil");
            return 3;
        }
        let nombres: Id = objc::m0(lib, "functionNames");
        let n: u64 = objc::m0(nombres, "count");
        println!("funciones en la biblioteca: {n}");
        for i in 0..n.min(24) {
            let nm: Id = objc::m1(nombres, "objectAtIndex:", i);
            let s = objc::texto(nm);
            let f: Id = objc::m2(lib, "newFunctionWithName:constantValues:error:", nm, std::ptr::null_mut::<Id>());
            println!("  «{s}» -> {}", if f.is_null() { "nil" } else { "ok" });
        }
        // Por descriptor: el descriptor lo crea Metal (MTLFunctionDescriptor). En macOS 26 el nombre está en «name».
        let clase = objc::clase("MTLFunctionDescriptor");
        if clase.is_null() {
            println!("no está MTLFunctionDescriptor");
            return 7;
        }
        let desc: Id = objc::m0(clase, "alloc");
        objc::m1::<Id, ()>(desc, "setName:", objc::nsstring("std_vert0_lpf"));
        let nombre: Id = objc::m0(desc, "name");
        println!("descriptor: clase={} nombre={}", objc::nombre_clase(objc::object_getClass(desc)), objc::texto(nombre));
        let f: Id = objc::m2(lib, "newFunctionWithDescriptor:error:", desc, &mut err as *mut Id);
        println!("función por descriptor: {}", if f.is_null() { "nil" } else { "ok" });
        0
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MTLSize {
    w: u64,
    h: u64,
    d: u64,
}

/// Prueba de cómputo de punta a punta: SetValueFloat (de un metallib de macOS) llena un buffer, y una copia (blit) lo
/// pasa a otro.
fn computo(d: Id) -> i32 {
    const RUTA: &str = "/System/Library/PrivateFrameworks/CoreRE3DGSFoundation.framework/Versions/A/Resources/default.metallib";
    const N: usize = 1024;
    const TAM: u32 = 1000;
    // SAFETY: d es un MTLDevice; las firmas son las del protocolo.
    unsafe {
        let mut err: Id = std::ptr::null_mut();
        let lib: Id = objc::m2(d, "newLibraryWithFile:error:", objc::nsstring(RUTA), &mut err as *mut Id);
        if lib.is_null() {
            println!("newLibraryWithFile: nil ({})", objc::texto(if err.is_null() { err } else { objc::m0(err, "localizedDescription") }));
            return 3;
        }
        let f: Id = objc::m1(lib, "newFunctionWithName:", objc::nsstring("SetValueFloat"));
        println!("función: {}", if f.is_null() { "nil".into() } else { objc::texto(objc::m0(f, "name")) });
        let p: Id = objc::m2(d, "newComputePipelineStateWithFunction:error:", f, &mut err as *mut Id);
        if p.is_null() {
            println!("pipeline: nil ({})", objc::texto(if err.is_null() { err } else { objc::m0(err, "localizedDescription") }));
            return 4;
        }
        let w: u64 = objc::m0(p, "threadExecutionWidth");
        println!("pipeline: ancho SIMD {w}");
        let buf: Id = objc::m2(d, "newBufferWithLength:options:", (N * 4) as u64, 0u64);
        let copia: Id = objc::m2(d, "newBufferWithLength:options:", (N * 4) as u64, 0u64);
        let q: Id = objc::m0(d, "newCommandQueue");
        let cb: Id = objc::m0(q, "commandBuffer");
        let enc: Id = objc::m0(cb, "computeCommandEncoder");
        objc::m1::<Id, ()>(enc, "setComputePipelineState:", p);
        objc::m3::<Id, u64, u64, ()>(enc, "setBuffer:offset:atIndex:", buf, 0, 0);
        let valor: f32 = 3.5;
        objc::m3::<*const f32, u64, u64, ()>(enc, "setBytes:length:atIndex:", &valor, 4, 1);
        objc::m3::<*const u32, u64, u64, ()>(enc, "setBytes:length:atIndex:", &TAM, 4, 2);
        objc::m2::<MTLSize, MTLSize, ()>(enc, "dispatchThreads:threadsPerThreadgroup:", MTLSize { w: N as u64, h: 1, d: 1 }, MTLSize { w: 64, h: 1, d: 1 });
        objc::m0::<()>(enc, "endEncoding");
        let blit: Id = objc::m0(cb, "blitCommandEncoder");
        let f5: Id = std::ptr::null_mut();
        let _ = f5;
        let cp: unsafe extern "C" fn(Id, objc::Sel, Id, u64, Id, u64, u64) = std::mem::transmute(objc::objc_msgSend as *const ());
        cp(blit, objc::sel("copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:"), buf, 0, copia, 0, (N * 4) as u64);
        objc::m0::<()>(blit, "endEncoding");
        objc::m0::<()>(cb, "commit");
        objc::m0::<()>(cb, "waitUntilCompleted");
        let st: u64 = objc::m0(cb, "status");
        let v = std::slice::from_raw_parts(objc::m0::<*const f32>(buf, "contents"), N);
        let c = std::slice::from_raw_parts(objc::m0::<*const f32>(copia, "contents"), N);
        let bien = v[..TAM as usize].iter().all(|&x| x == 3.5) && v[TAM as usize..].iter().all(|&x| x == 0.0);
        let bien_copia = v == c;
        println!("command buffer: estado {st}; valores[0]={} valores[999]={} valores[1000]={}", v[0], v[999], v[1000]);
        println!("cómputo: {}; copia (blit): {}", if bien { "CORRECTO" } else { "MAL" }, if bien_copia { "CORRECTA" } else { "MAL" });
        if bien && bien_copia { 0 } else { 5 }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MTLClearColor {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MTLRegion {
    x: u64,
    y: u64,
    z: u64,
    w: u64,
    h: u64,
    d: u64,
}

/// Render de punta a punta con la API de Metal y shaders de QuartzCore: se limpia a rojo un BGRA8 de 64×64 y se dibuja el
/// triángulo (-1,-1) (1,-1) (-1,1) con std_vert0_lpf y attachment_clear_frag_lpf (escribe 0): abajo a la izquierda debe
/// quedar 0 y arriba a la derecha, rojo.
fn render(d: Id) -> i32 {
    const RUTA: &str = "/System/Library/Frameworks/QuartzCore.framework/Versions/A/Resources/default.metallib";
    // SAFETY: d es un MTLDevice; las clases de descriptores son las de Metal; firmas del protocolo.
    unsafe {
        let mut err: Id = std::ptr::null_mut();
        let lib: Id = objc::m2(d, "newLibraryWithFile:error:", objc::nsstring(RUTA), &mut err as *mut Id);
        if lib.is_null() {
            println!("biblioteca: nil");
            return 3;
        }
        let vf: Id = objc::m1(lib, "newFunctionWithName:", objc::nsstring("std_vert0_lpf"));
        let ff: Id = objc::m1(lib, "newFunctionWithName:", objc::nsstring("attachment_clear_frag_lpf"));
        let pd: Id = objc::m0(objc::m0::<Id>(objc::clase("MTLRenderPipelineDescriptor"), "alloc"), "init");
        objc::m1::<Id, ()>(pd, "setVertexFunction:", vf);
        objc::m1::<Id, ()>(pd, "setFragmentFunction:", ff);
        let ca0: Id = objc::m1(objc::m0::<Id>(pd, "colorAttachments"), "objectAtIndexedSubscript:", 0u64);
        objc::m1::<u64, ()>(ca0, "setPixelFormat:", 80);
        let vd: Id = objc::m0(objc::clase("MTLVertexDescriptor"), "vertexDescriptor");
        let attrs: Id = objc::m0(vd, "attributes");
        for (i, f, o) in [(0u64, 31u64, 0u64), (1, 29, 0), (3, 31, 16)] {
            let a: Id = objc::m1(attrs, "objectAtIndexedSubscript:", i);
            objc::m1::<u64, ()>(a, "setFormat:", f);
            objc::m1::<u64, ()>(a, "setOffset:", o);
            objc::m1::<u64, ()>(a, "setBufferIndex:", 0);
        }
        let lo: Id = objc::m1(objc::m0::<Id>(vd, "layouts"), "objectAtIndexedSubscript:", 0u64);
        objc::m1::<u64, ()>(lo, "setStride:", 32);
        objc::m1::<Id, ()>(pd, "setVertexDescriptor:", vd);
        let p: Id = objc::m2(d, "newRenderPipelineStateWithDescriptor:error:", pd, &mut err as *mut Id);
        if p.is_null() {
            println!("pipeline de render: nil ({})", objc::texto(if err.is_null() { err } else { objc::m0(err, "localizedDescription") }));
            return 4;
        }
        println!("pipeline de render: bien");
        let td: Id = {
            let f: unsafe extern "C" fn(Id, objc::Sel, u64, u64, u64, i8) -> Id = std::mem::transmute(objc::objc_msgSend as *const ());
            f(objc::clase("MTLTextureDescriptor"), objc::sel("texture2DDescriptorWithPixelFormat:width:height:mipmapped:"), 80, 64, 64, 0)
        };
        objc::m1::<u64, ()>(td, "setUsage:", 5);
        let tex: Id = objc::m1(d, "newTextureWithDescriptor:", td);
        if tex.is_null() {
            println!("textura: nil");
            return 5;
        }
        let vs: [f32; 24] = [-1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let vb: Id = objc::m3(d, "newBufferWithBytes:length:options:", vs.as_ptr(), 96u64, 0u64);
        let mut ident = [0f32; 16];
        for i in 0..4 {
            ident[i * 4 + i] = 1.0;
        }
        let rp: Id = objc::m0(objc::clase("MTLRenderPassDescriptor"), "renderPassDescriptor");
        let a0: Id = objc::m1(objc::m0::<Id>(rp, "colorAttachments"), "objectAtIndexedSubscript:", 0u64);
        objc::m1::<Id, ()>(a0, "setTexture:", tex);
        objc::m1::<u64, ()>(a0, "setLoadAction:", 2);
        objc::m1::<u64, ()>(a0, "setStoreAction:", 1);
        objc::m1::<MTLClearColor, ()>(a0, "setClearColor:", MTLClearColor { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        let q: Id = objc::m0(d, "newCommandQueue");
        let cb: Id = objc::m0(q, "commandBuffer");
        let enc: Id = objc::m1(cb, "renderCommandEncoderWithDescriptor:", rp);
        objc::m1::<Id, ()>(enc, "setRenderPipelineState:", p);
        objc::m3::<Id, u64, u64, ()>(enc, "setVertexBuffer:offset:atIndex:", vb, 0, 0);
        objc::m3::<*const f32, u64, u64, ()>(enc, "setVertexBytes:length:atIndex:", ident.as_ptr(), 64, 2);
        objc::m3::<u64, u64, u64, ()>(enc, "drawPrimitives:vertexStart:vertexCount:", 3, 0, 3);
        objc::m0::<()>(enc, "endEncoding");
        objc::m0::<()>(cb, "commit");
        objc::m0::<()>(cb, "waitUntilCompleted");
        let st: u64 = objc::m0(cb, "status");
        let mut px = vec![0u8; 64 * 64 * 4];
        {
            let f: unsafe extern "C" fn(Id, objc::Sel, *mut u8, u64, MTLRegion, u64) = std::mem::transmute(objc::objc_msgSend as *const ());
            f(tex, objc::sel("getBytes:bytesPerRow:fromRegion:mipmapLevel:"), px.as_mut_ptr(), 256, MTLRegion { x: 0, y: 0, z: 0, w: 64, h: 64, d: 1 }, 0);
        }
        let p = |x: usize, y: usize| -> [u8; 4] { px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].try_into().unwrap() };
        let (ai, ad) = (p(8, 56), p(56, 8));
        println!("command buffer: estado {st}; abajo a la izquierda BGRA {ai:?}; arriba a la derecha {ad:?}");
        let bien = ai == [0, 0, 0, 0] && ad == [0, 0, 255, 255];
        println!("render: {}", if bien { "CORRECTO" } else { "MAL" });
        if bien { 0 } else { 6 }
    }
}
