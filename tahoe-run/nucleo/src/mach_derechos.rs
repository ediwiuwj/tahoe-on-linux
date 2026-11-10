//! Contabilidad por entrada; C conserva lookup, lock, commit y efectos externos.
//! No recibe tablas ni referencias a shm. Mensajes/notificaciones y limpieza global
//! siguen pendientes en C: este bloque no pretende completar todo Mach.
#![forbid(unsafe_code)]
use crate::mach_puerto::Sport;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Entry { pub port: u32, pub holder: i32, pub refs: i32 }
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Result { pub entry: Entry, pub changed: u32, pub released: u32, pub destroy: u32 }
// kind: 0 envío, 1 memoria, 2 llegada envío (holder != 0), 3 conservar
// envío al mover recepción (idempotente). total incluye el valor anterior.
pub fn reference(e: Entry, p: Sport, port: u32, holder: i32, delta: i32, base: i32, kind: u32, total: i64) -> Result {
    let mut r = Result { entry: e, changed: 0, released: 0, destroy: 0 };
    if port == 0 || delta == 0 || kind > 3 || (kind == 2 && holder == 0) { return r; }
    let present = e.port == port && e.holder == holder;
    if e.port != 0 && !present { return r; }
    if kind == 1 && (p.used == 0 || p.name != port || p.me == 0) { return r; }
    if kind == 3 && present { return r; }
    if !present && (delta < 0 || p.used == 0 || p.name != port) { return r; }
    let old = if present { e.refs } else if kind == 1 { 0 } else { base.max(0) };
    // Suma ensanchada: nunca overflow firmado; saturación explícita de urefs.
    let new = (i64::from(old) + i64::from(delta)).clamp(0, i64::from(i32::MAX)) as i32;
    r.entry = Entry { port: if new == 0 { 0 } else { port }, holder, refs: new };
    r.changed = 1;
    r.released = u32::from(new == 0);
    r.destroy = u32::from(kind == 1 && total.saturating_add(i64::from(new) - if present { i64::from(old) } else { 0 }) <= 0);
    r
}
// op 0: coincide, 1: insertar en hueco, 2: consumir/invalida coincidencia.
// send-once usa el mismo snapshot compacto; refs no pertenece a su formato shm.
pub fn once(mut e: Entry, port: u32, holder: i32, op: u32) -> Result {
    let matched = e.port == port && e.holder == holder;
    let changed = match op { 0 => matched, 1 => e.port == 0, 2 => matched, _ => false };
    if changed {
        if op == 1 { e = Entry { port, holder, refs: 0 }; }
        if op == 2 { e.port = 0; }
    }
    Result { entry: e, changed: u32::from(changed), released: u32::from(changed && op == 2), destroy: 0 }
}
const _: () = {
    assert!(core::mem::size_of::<Entry>() == 12);
    assert!(core::mem::size_of::<Result>() == 24);
    assert!(core::mem::offset_of!(Result, changed) == 12);
};
#[cfg(test)]
mod tests {
    use super::*;
    fn p() -> Sport { Sport { name: 7, used: 1, me: 1, ..Sport::default() } }
    fn run(e: Entry, holder: i32, delta: i32, kind: u32, total: i64) -> Result {
        reference(e, p(), 7, holder, delta, 0, kind, total)
    }
    #[test]
    fn references_edges_and_ownership() {
        let e = run(Entry::default(), 42, 1, 0, 0).entry;
        assert_eq!(e.refs, 1);
        assert_eq!(run(e, 43, -1, 0, 0).changed, 0);
        assert_eq!(run(e, 42, 1, 3, 0).changed, 0);
        assert_eq!(run(e, 42, i32::MIN, 0, 0).released, 1);
        assert_eq!(run(e, 42, i32::MAX, 0, 0).entry.refs, i32::MAX);
        assert_eq!(run(Entry::default(), 42, -1, 0, 0).changed, 0);
        assert_eq!(run(Entry::default(), 0, 1, 2, 0).changed, 0);
        assert_eq!(run(Entry::default(), 0, 1, 0, 0).entry.refs, 1);
        assert_eq!(reference(Entry::default(), Sport::default(), 7, 42, 1, 0, 0, 0).changed, 0);
        assert_eq!(reference(Entry::default(), p(), 7, 42, i32::MAX, 1, 0, 0).entry.refs, i32::MAX);
    }
    #[test]
    fn saturation_missing_slots_and_noops() {
        let full = Entry { port: 7, holder: 42, refs: i32::MAX };
        assert_eq!(run(full, 42, 1, 0, 0).entry, full);
        assert_eq!(run(full, 42, 0, 0, 0).changed, 0);
        assert_eq!(run(full, 42, 1, 99, 0).changed, 0);
        // Un candidato ocupado por otro derecho nunca se sobrescribe (tabla llena).
        assert_eq!(run(full, 43, 1, 0, 0).entry, full);
        assert_eq!(once(full, 8, 43, 1).changed, 0);
        assert_eq!(once(full, 7, 42, 99).changed, 0);
        let port = p();
        let r = crate::mach_derechos_ffi::nucleo_derechos_reference(
            Entry::default(), port, 7, 42, 1, 1, 0, 0);
        assert_eq!(r.entry.refs, 2);
        assert_eq!(port, p()); // snapshot: el estado Sport no se modifica
        let mut missing = port; missing.used = 0;
        // Envío ya contabilizado puede soltarse aunque haya muerto el puerto.
        assert_eq!(reference(full, missing, 7, 42, -1, 0, 0, 0).entry.refs, i32::MAX - 1);
        assert_eq!(reference(full, missing, 7, 42, -1, 0, 1, 0).changed, 0);
    }
    #[test]
    fn memory_lifetime_and_type() {
        let e = run(Entry::default(), 42, 2, 1, 0).entry;
        assert_eq!(run(e, 42, -2, 1, 3).destroy, 0);
        assert_eq!(run(e, 42, i32::MIN, 1, 2).destroy, 1);
        let mut port = p(); port.me = 0;
        assert_eq!(reference(e, port, 7, 42, -2, 0, 1, 2).changed, 0);
        assert_eq!(run(Entry::default(), 0, 1, 1, 0).entry.refs, 1);
    }
    #[test]
    fn once_consumption_regrant_and_ffi() {
        let e = once(Entry::default(), 7, 42, 1).entry;
        assert_eq!(once(e, 7, 43, 2).changed, 0);
        assert_eq!(once(e, 7, 42, 1).changed, 0);
        let r = crate::mach_derechos_ffi::nucleo_derechos_once(e, 7, 42, 2);
        assert_eq!(r.released, 1);
        assert_eq!(once(r.entry, 7, 42, 2).changed, 0);
        assert_eq!(once(r.entry, 7, 42, 1).entry, e);
    }
}
