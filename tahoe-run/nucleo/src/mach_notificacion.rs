//! Planes puros de notificación. Sin punteros, asignación ni unsafe; C conserva el lock.
#![forbid(unsafe_code)]
use crate::mach_puerto::Sport;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Entry { pub port: u32, pub notify: u32, pub pid: i32, pub armado: u32 }
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Note { pub dest: u32, pub arg: u32, pub id: i32 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Plan {
    pub port: Sport, pub entry: Entry, pub note: Note,
    pub previous: u32, pub remove: u32, pub store: u32, pub full: u32, pub error: i32,
}

/// 0 ignorar, 1 acumular, 2 informar pérdida. No se incrementa fuera del límite.
pub fn enqueue(dest: u32, len: u32, cap: u32) -> u32 {
    if dest == 0 { 0 } else if len < cap { 1 } else { 2 }
}
fn note(dest: u32, id: i32, arg: u32) -> Note { Note { dest, id, arg } }

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Message { pub bytes: [u8; 40], pub len: u32 }

/// Mensaje de notificación Darwin, con destino MOVE_SEND_ONCE.
/// La propiedad del derecho y los efectos del envío se gestionan fuera del codec.
pub fn message(n: Note) -> Message {
    let mut r = Message { bytes: [0; 40], len: 24 };
    let mut bits = 18u32;
    match n.id {
        66 | 70 | 72 => {
            r.len = 36;
            r.bytes[28] = 1; // NDR little-endian
            r.bytes[32..36].copy_from_slice(&n.arg.to_le_bytes());
        }
        69 => {
            bits |= 0x8000_0000;
            r.len = 40;
            r.bytes[24..28].copy_from_slice(&1u32.to_le_bytes());
            r.bytes[28..32].copy_from_slice(&n.arg.to_le_bytes());
            r.bytes[38] = 16; // MOVE_RECEIVE, descriptor de puerto
        }
        _ => {}
    }
    r.bytes[0..4].copy_from_slice(&bits.to_le_bytes());
    r.bytes[4..8].copy_from_slice(&r.len.to_le_bytes());
    r.bytes[8..12].copy_from_slice(&n.dest.to_le_bytes());
    r.bytes[20..24].copy_from_slice(&n.id.to_le_bytes());
    r
}

/// `old` es la primera coincidencia (nombre,pid); `space` incluye su ranura.
/// Se preserva la conducta histórica: tabla llena y variante desconocida devuelven éxito.
pub fn request(p: Sport, old: Entry, name: u32, variant: i32, sync: u32,
               notify: u32, pid: i32, next: u32, kernel: u32, space: u32) -> Plan {
    let mut r = Plan { port: p, ..Plan::default() };
    match variant {
        69 | 70 => {
            if p.used == 0 { r.error = 17; }
            else if variant == 69 { r.previous = p.pdreq; r.port.pdreq = notify; }
            else { r.previous = p.nsreq; r.port.nsreq = notify; r.port.nsmscount = sync; }
        }
        66 | 72 => {
            // SEND_POSSIBLE sobre objeto kernel no reemplaza entradas; DEAD_NAME sí.
            if variant == 66 && p.used == 0 && kernel != 0 {
                if notify != 0 && sync != 0 { r.note = note(notify, 66, name); }
                return r;
            }
            if variant == 72 || p.used != 0 {
                if old.port == name && old.pid == pid {
                    r.previous = old.notify; r.remove = 1;
                    if variant == 66 { r.port.spn = r.port.spn.saturating_sub(1); }
                }
            }
            if p.used == 0 {
                if variant == 66 || notify != 0 {
                    if notify != 0 && name != 0 && name < next { r.note = note(notify, 72, name); }
                    else { r.error = 15; }
                }
            } else if notify != 0 {
                let limit = if p.qlimit == 0 { 5 } else { p.qlimit };
                if variant == 66 && sync != 0 && p.count < limit {
                    r.note = note(notify, 66, name);
                } else if space != 0 {
                    r.store = 1;
                    r.entry = Entry { port: name, notify, pid, armado: u32::from(variant == 66 && sync != 0) };
                    if variant == 66 { r.port.spn = r.port.spn.saturating_add(1); }
                } else { r.full = 1; }
            }
        }
        _ => {}
    }
    r
}

/// Eventos: 0 no-senders, 1 devolución de recepción, 2 muerte de entrada,
/// 3 hueco en cola, 4 armar tras MACH_SEND_NOTIFY. La selección de tablas queda en C.
pub fn event(p: Sport, e: Entry, name: u32, pid: i32, kind: u32, held: u32) -> Plan {
    let mut r = Plan { port: p, entry: e, ..Plan::default() };
    match kind {
        0 if p.used != 0 && p.nsreq != 0 && held == 0 => {
            r.note = note(p.nsreq, 70, p.nsmscount); r.port.nsreq = 0;
        }
        1 if p.pdreq != 0 && p.is_set == 0 => {
            r.note = note(p.pdreq, 69, p.name); r.port.pdreq = 0; r.port.owner = 0;
            if p.svc != 0 && p.svc_ctx != 0 {
                r.port.ctx = p.svc_ctx; r.port.guarded = 1; r.port.strict_guard = 1;
            } else { r.port.ctx = 0; r.port.guarded = 0; r.port.strict_guard = 0; }
        }
        2 if e.port != 0 && e.port == name => {
            r.note = note(e.notify, 72, name); r.entry.port = 0;
        }
        3 if e.port != 0 && e.port == name && e.armado != 0 => {
            r.note = note(e.notify, 66, name); r.entry.port = 0;
            r.port.spn = r.port.spn.saturating_sub(1);
        }
        4 if e.port != 0 && e.port == name && e.pid == pid => { r.entry.armado = 1; }
        _ => {}
    }
    r
}

const _: () = {
    use core::mem::{size_of, offset_of};
    assert!(size_of::<Entry>() == 16 && offset_of!(Entry, armado) == 12);
    assert!(size_of::<Note>() == 12 && offset_of!(Note, id) == 8);
    assert!(size_of::<Plan>() == 256 && offset_of!(Plan, entry) == 208);
    assert!(offset_of!(Plan, note) == 224 && offset_of!(Plan, error) == 252);
};

#[cfg(test)]
mod tests {
    use super::*;
    fn port() -> Sport { Sport { used: 1, name: 10, count: 5, ..Sport::default() } }
    fn req(p: Sport, e: Entry, sync: u32, notify: u32, space: u32) -> Plan {
        request(p,e,10,66,sync,notify,7,20,0,space)
    }
    #[test] fn notification_wire_layouts() {
        for id in [66, 70, 72] {
            let m = message(note(0x1234, id, 0x5678));
            assert_eq!(m.len, 36);
            assert_eq!(&m.bytes[..4], &18u32.to_le_bytes());
            assert_eq!(&m.bytes[8..12], &0x1234u32.to_le_bytes());
            assert_eq!(&m.bytes[24..32], &[0,0,0,0,1,0,0,0]);
            assert_eq!(&m.bytes[32..36], &0x5678u32.to_le_bytes());
        }
        let m = message(note(1, 69, 2));
        assert_eq!(m.len, 40);
        assert_eq!(&m.bytes[..4], &0x8000_0012u32.to_le_bytes());
        assert_eq!(&m.bytes[24..28], &1u32.to_le_bytes());
        assert_eq!(&m.bytes[28..32], &2u32.to_le_bytes());
        assert_eq!(&m.bytes[32..40], &[0,0,0,0,0,0,16,0]);
        let m = message(note(1, 71, 0));
        assert_eq!(m.len, 24);
        assert_eq!(&m.bytes[24..], &[0;16]);
        assert_eq!(core::mem::size_of::<Message>(), 44);
    }
    #[test] fn arm_rearm_sync_and_replace() {
        let p = port();
        let r = req(p,Entry::default(),0,30,1);
        assert_eq!(r.entry.armado,0);
        let unarmed = event(r.port,r.entry,10,7,3,0);
        assert_eq!(unarmed.note.dest,0);
        let armed = event(r.port,r.entry,10,7,4,0);
        assert_eq!(armed.entry.armado,1);
        let sent = event(armed.port,armed.entry,10,7,3,0);
        assert_eq!(sent.note,note(30,66,10)); assert_eq!(sent.port.spn,0);
        assert_eq!(event(sent.port,sent.entry,10,7,3,0).note.dest,0);
        let replacement = req(r.port,r.entry,1,31,1);
        assert_eq!(replacement.previous,30); assert_eq!(replacement.port.spn,1);
        assert_eq!(replacement.entry.armado,1);
        let mut empty = p; empty.count = 4;
        assert_eq!(req(empty,Entry::default(),1,32,0).note,note(32,66,10));
        let cancel = req(r.port,r.entry,0,0,1);
        assert_eq!(cancel.remove,1); assert_eq!(cancel.port.spn,0); assert_eq!(cancel.store,0);
    }
    #[test] fn no_senders_consumes_only_without_holders() {
        let r = request(port(),Entry::default(),10,70,99,30,7,20,0,0);
        assert_eq!(r.note.dest,0); // registro no dispara de inmediato, conducta actual
        assert_eq!(event(r.port,Entry::default(),10,0,0,1).port.nsreq,30);
        let sent = event(r.port,Entry::default(),10,0,0,0);
        assert_eq!(sent.note,note(30,70,99)); assert_eq!(sent.port.nsreq,0);
        assert_eq!(event(sent.port,Entry::default(),10,0,0,0).note.dest,0);
    }
    #[test] fn errors_limits_and_dead_names() {
        assert_eq!(enqueue(0,512,512),0); assert_eq!(enqueue(1,511,512),1);
        assert_eq!(enqueue(1,512,512),2); assert_eq!(enqueue(1,u32::MAX,512),2);
        assert_eq!(req(port(),Entry::default(),1,30,0).full,1);
        assert_eq!(request(Sport::default(),Entry::default(),0,69,0,1,7,20,0,0).error,17);
        for name in [0,20,u32::MAX] {
            assert_eq!(request(Sport::default(),Entry::default(),name,72,0,1,7,20,0,0).error,15);
        }
        let dead = request(Sport::default(),Entry::default(),10,72,0,30,7,20,0,0);
        assert_eq!(dead.note,note(30,72,10));
        assert_eq!(request(Sport::default(),Entry::default(),10,66,1,30,7,20,1,0).note,note(30,66,10));
        assert_eq!(request(port(),Entry::default(),10,999,0,30,7,20,0,0).error,0);
    }
    #[test] fn ffi_roundtrip_and_explicit_limit() {
        let mut p = port(); p.qlimit = 2; p.count = 1;
        let r = crate::mach_notificacion_ffi::nucleo_notificacion_request(
            p, Entry::default(), 10, 66, 1, 30, 7, 20, 0, 0);
        assert_eq!(r.note, note(30,66,10));
        p.count = 2;
        let full = req(p,Entry::default(),1,30,1);
        assert_eq!(full.entry.armado,1); assert_eq!(full.note.dest,0);
        let sent = event(full.port,full.entry,10,7,3,0);
        let rearmed = req(sent.port,sent.entry,1,31,1);
        assert_eq!(rearmed.port.spn,1); assert_eq!(rearmed.entry.armado,1);
        let other = event(full.port,full.entry,10,8,4,0);
        assert_eq!(other.entry,full.entry);
        let old = Entry { port:10, notify:40, pid:7, armado:0 };
        let dead = request(p,old,10,72,0,41,7,20,0,1);
        assert_eq!(dead.previous,40); assert_eq!(dead.remove,1); assert_eq!(dead.store,1);
        assert_eq!(dead.entry.notify,41); assert_eq!(dead.port.spn,p.spn);
    }
    #[test] fn destroyed_restores_service_guard_and_dead_entry() {
        let mut p = port(); p.pdreq = 40; p.svc = 1; p.svc_ctx = 55;
        let r = event(p,Entry::default(),10,0,1,0);
        assert_eq!(r.note,note(40,69,10)); assert_eq!(r.port.owner,0);
        assert_eq!(r.port.ctx,55); assert_eq!(r.port.strict_guard,1);
        let e = Entry { port:10, notify:30, pid:7, armado:0 };
        assert_eq!(event(p,e,10,0,2,0).note,note(30,72,10));
        assert_eq!(event(p,e,11,0,2,0).entry,e);
    }
}
