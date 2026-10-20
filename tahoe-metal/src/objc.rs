//! Lo justo del runtime de Objective-C de macOS (libobjc) para crear clases y mandar mensajes desde Rust, sin crates.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void, CStr, CString};

pub type Id = *mut c_void;
pub type Class = *mut c_void;
pub type Sel = *const c_void;
pub type Imp = *const c_void;
pub type Bool = i8;

#[repr(C)]
pub struct ObjcSuper {
    pub receiver: Id,
    pub super_class: Class,
}

#[link(name = "objc")]
extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Class;
    pub fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra: usize) -> Class;
    pub fn objc_registerClassPair(cls: Class);
    pub fn class_addMethod(cls: Class, name: Sel, imp: Imp, types: *const c_char) -> Bool;
    pub fn class_getName(cls: Class) -> *const c_char;
    pub fn class_getSuperclass(cls: Class) -> Class;
    pub fn object_getClass(obj: Id) -> Class;
    pub fn sel_registerName(name: *const c_char) -> Sel;
    pub fn sel_getName(sel: Sel) -> *const c_char;
    pub fn objc_getProtocol(name: *const c_char) -> *mut c_void;
    pub fn class_addProtocol(cls: Class, proto: *mut c_void) -> Bool;
    pub fn objc_msgSend();
    pub fn objc_msgSend_stret();
    pub fn objc_msgSendSuper();
    pub fn class_getInstanceMethod(cls: Class, s: Sel) -> *mut c_void;
    pub fn method_getImplementation(m: *mut c_void) -> Imp;
    pub fn method_setImplementation(m: *mut c_void, imp: Imp) -> Imp;
    pub fn protocol_copyMethodDescriptionList(
        proto: *mut c_void,
        requerido: Bool,
        instancia: Bool,
        cuenta: *mut u32,
    ) -> *mut DescripcionMetodo;
}

/// Descripción de un método de un protocolo (como la devuelve `protocol_copyMethodDescriptionList`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DescripcionMetodo {
    pub nombre: Sel,
    pub tipos: *const c_char,
}

/// Selector por nombre.
pub fn sel(nombre: &str) -> Sel {
    let c = CString::new(nombre).unwrap();
    // SAFETY: cadena C válida; el runtime la copia.
    unsafe { sel_registerName(c.as_ptr()) }
}

pub fn clase(nombre: &str) -> Class {
    let c = CString::new(nombre).unwrap();
    // SAFETY: cadena C válida.
    unsafe { objc_getClass(c.as_ptr()) }
}

pub fn nombre_sel(s: Sel) -> String {
    // SAFETY: un selector del runtime tiene nombre.
    unsafe { CStr::from_ptr(sel_getName(s)).to_string_lossy().into_owned() }
}

pub fn nombre_clase(c: Class) -> String {
    if c.is_null() {
        return "(nil)".into();
    }
    // SAFETY: clase del runtime.
    unsafe { CStr::from_ptr(class_getName(c)).to_string_lossy().into_owned() }
}

/// objc_msgSend con la firma de cada llamada (0, 1, 2 o 3 argumentos).
///
/// # Safety
/// `obj` debe responder a `s` con esa firma.
pub unsafe fn m0<R>(obj: Id, s: &str) -> R {
    let f: unsafe extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s))
}
/// # Safety
/// Ver `m0`.
pub unsafe fn m1<A, R>(obj: Id, s: &str, a: A) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A) -> R = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), a)
}
/// # Safety
/// Ver `m0`.
pub unsafe fn m2<A, B, R>(obj: Id, s: &str, a: A, b: B) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B) -> R = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), a, b)
}
/// # Safety
/// Ver `m0`.
pub unsafe fn m3<A, B, C, R>(obj: Id, s: &str, a: A, b: B, c: C) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B, C) -> R = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), a, b, c)
}
/// Mensaje que devuelve una estructura de más de 16 bytes (en x86_64 va por objc_msgSend_stret).
///
/// # Safety
/// Ver `m0`.
pub unsafe fn m0_stret<R>(obj: Id, s: &str) -> R {
    let f: unsafe extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc_msgSend_stret as *const ());
    f(obj, sel(s))
}

/// Mensaje a la superclase `sup` (la implementación de la clase madre): sin argumentos.
///
/// # Safety
/// Ver `m0`.
pub unsafe fn super0<R>(obj: Id, sup: Class, s: Sel) -> R {
    let mut os = ObjcSuper { receiver: obj, super_class: sup };
    let f: unsafe extern "C" fn(*mut ObjcSuper, Sel) -> R = std::mem::transmute(objc_msgSendSuper as *const ());
    f(&mut os, s)
}
/// Mensaje a la superclase `sup` (la implementación de la clase madre): 1 argumento.
///
/// # Safety
/// Ver `m0`.
pub unsafe fn super1<A, R>(obj: Id, sup: Class, s: Sel, a: A) -> R {
    let mut os = ObjcSuper { receiver: obj, super_class: sup };
    let f: unsafe extern "C" fn(*mut ObjcSuper, Sel, A) -> R = std::mem::transmute(objc_msgSendSuper as *const ());
    f(&mut os, s, a)
}

/// NSString de una cadena de Rust (autoliberada).
pub fn nsstring(s: &str) -> Id {
    let c = CString::new(s).unwrap_or_default();
    // SAFETY: NSString existe en todo proceso con Foundation (Metal la carga).
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id = std::mem::transmute(objc_msgSend as *const ());
        f(clase("NSString"), sel("stringWithUTF8String:"), c.as_ptr())
    }
}

/// Texto de un NSString (o "(nil)").
pub fn texto(s: Id) -> String {
    if s.is_null() {
        return "(nil)".into();
    }
    // SAFETY: objeto que responde a UTF8String.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> *const c_char = std::mem::transmute(objc_msgSend as *const ());
        let p = f(s, sel("UTF8String"));
        if p.is_null() {
            "(nil)".into()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Añade un método a la clase (o a la metaclase, para los de clase).
pub fn metodo(cls: Class, nombre: &str, imp: Imp, tipos: &str) -> Bool {
    let t = CString::new(tipos).unwrap();
    // SAFETY: clase en construcción; el runtime copia los tipos.
    unsafe { class_addMethod(cls, sel(nombre), imp, t.as_ptr()) }
}

/// Clase de un objeto (nil si el objeto es nil).
pub fn clase_de(o: Id) -> Class {
    if o.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: objeto de Objective-C.
    unsafe { object_getClass(o) }
}

pub fn es_nulo(p: *const c_void) -> bool {
    p.is_null()
}

/// Si el objeto tiene ese método.
pub fn responde_instancia(o: Id, nombre: &str) -> bool {
    if o.is_null() {
        return false;
    }
    // SAFETY: objeto de Objective-C.
    unsafe { m1::<Sel, Bool>(o, "respondsToSelector:", sel(nombre)) != 0 }
}

/// Si la clase tiene ese método de clase (+nombre).
pub fn responde_clase(cls: Class, nombre: &str) -> bool {
    if cls.is_null() {
        return false;
    }
    // SAFETY: la clase es un objeto; respondsToSelector: sobre ella mira los métodos de clase.
    unsafe { m1::<Sel, Bool>(cls.cast(), "respondsToSelector:", sel(nombre)) != 0 }
}
