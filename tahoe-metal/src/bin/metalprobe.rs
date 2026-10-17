//! metalprobe: prueba de Metal dentro del macOS de tahoe-run. Lista los MTLDevice (MTLCopyAllDevices) y crea el de
//! por omisión (MTLCreateSystemDefaultDevice), con su clase y su nombre.

use std::ffi::{c_char, c_int, c_void, CStr};

#[path = "../objc.rs"]
#[allow(dead_code)]
mod objc;

use objc::Id;

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
        std::process::exit(0);
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
