//! Planes puros de cola; índices shm desde 1. C conserva lock, lookup y bytes.
#![forbid(unsafe_code)]
use crate::mach_puerto::Sport;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub head: u32, pub tail: u32, pub count: u32, pub free: u32,
    pub slot: u32, pub next: u32, pub link: u32, pub link_next: u32,
    pub possible: u32, pub action: i32,
}
pub fn qlimit(value: u32) -> u32 { value.min(2048) }
pub fn limit(p: Sport) -> u32 { if p.qlimit == 0 { 5 } else { p.qlimit } }
// 0 continuar, 1 esperar, códigos Mach o -1 error. waited distingue muerte durante espera.
pub fn send(p: Sport, bits: u32, kernel: u32, timeout: i32, expired: u32, waited: u32) -> i32 {
    if p.used == 0 { return if waited != 0 { 0x10000003 } else { -1 }; }
    if p.is_set != 0 { return -1; }
    if kernel == 0 && bits & 31 != 18 && bits & 31 != 21 && p.count >= limit(p) {
        return if timeout == 0 || expired != 0 { 0x10000004 } else { 1 };
    }
    0
}
// op 0 enqueue, 1 unlink head, 2 recycle detached slot. No payload aliases.
pub fn transition(p: Sport, free: u32, slot: u32, next: u32, op: u32) -> Plan {
    let mut r = Plan { head:p.head, tail:p.tail, count:p.count, free, slot, next, action:-1, ..Plan::default() };
    if free > 4096 || slot == 0 || slot > 4096 || next > 4096 { return r; }
    match op {
        0 if free == slot && p.used != 0 && p.is_set == 0 && p.tail <= 4096 && ((p.head == 0) == (p.tail == 0)) => {
            if p.count == u32::MAX { return r; }
            r.free = next; r.next = 0; r.link = p.tail; r.link_next = slot;
            if p.tail == 0 { r.head = slot; }
            r.tail = slot; r.count += 1;
        }
        1 if p.head == slot && p.count > 0 => {
            r.head = next; if next == 0 { r.tail = 0; }
            r.count -= 1; r.possible = u32::from(p.spn != 0 && r.count < limit(p));
        }
        2 => { r.next = free; r.free = slot; }
        _ => return r,
    }
    r.action = 0; r
}
pub fn set_count(count: u32, enqueue: u32) -> u32 {
    if enqueue != 0 { count.wrapping_add(1) } else { count.saturating_sub(1) }
}
// 0 consume, 1 empty, MACH_RCV_TOO_LARGE sin consumir.
pub fn receive(p: Sport, total: u32, limit: u32, large: u32) -> i32 {
    if p.count == 0 || p.head == 0 { 1 }
    else if large != 0 && limit != 0 && total > limit { 0x10004004 } else { 0 }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn p() -> Sport { Sport { used:1, ..Sport::default() } }
    #[test] fn empty_enqueue_unlink_recycle() {
        assert_eq!(receive(p(),0,0,0),1);
        let a=transition(p(),1,1,2,0); assert_eq!((a.head,a.tail,a.count,a.free,a.next),(1,1,1,2,0));
        let b=Sport { head:1,tail:1,count:1,..p() };
        let r=transition(b,2,1,0,1); assert_eq!((r.head,r.tail,r.count),(0,0,0));
        let r=transition(b,2,1,0,2); assert_eq!((r.free,r.next),(1,2));
    }
    #[test] fn full_timeout_kernel_once_error() {
        let b=Sport {count:5,..p()};
        assert_eq!(send(b,19,0,-1,0,0),1);
        assert_eq!(send(b,19,0,0,0,0),0x10000004);
        assert_eq!(send(b,19,0,20,1,1),0x10000004);
        assert_eq!(send(b,19,1,0,0,0),0);
        for bits in [18,21] { assert_eq!(send(b,bits,0,0,0,0),0); }
        assert_eq!(send(Sport::default(),19,0,20,1,1),0x10000003);
        assert_eq!(send(Sport::default(),19,0,-1,0,0),-1);
        assert_eq!(transition(p(),0,0,0,0).action,-1);
    }
    #[test] fn abi_and_limits() {
        assert_eq!(core::mem::size_of::<Plan>(), 40);
        assert_eq!(qlimit(0), 0); assert_eq!(qlimit(2048), 2048);
        assert_eq!(qlimit(u32::MAX), 2048);
        let b = Sport { count: 7, qlimit: 8, ..p() };
        assert_eq!(crate::mach_cola_ffi::nucleo_cola_send(b, 19, 0, 0, 0, 0), 0);
        assert_eq!(transition(p(), 4097, 1, 0, 0).action, -1);
        assert_eq!(transition(p(), 1, 1, 4097, 0).action, -1);
    }
    #[test] fn limits_and_nonempty_links() {
        let b=Sport {head:4,tail:6,count:2,qlimit:3,spn:1,..p()};
        let r=transition(b,8,8,9,0); assert_eq!((r.link,r.link_next,r.head,r.tail,r.count),(6,8,4,8,3));
        let r=transition(b,8,4,6,1); assert_eq!((r.head,r.tail,r.count,r.possible),(6,6,1,1));
        assert_eq!(receive(b,100,99,1),0x10004004);
        assert_eq!(receive(b,100,100,1),0); assert_eq!(receive(b,100,0,1),0);
        assert_eq!(set_count(0,0),0); assert_eq!(set_count(4,1),5);
    }
}
