//! Máquina Mach pura. El llamador serializa cada transición con el mutex del objeto.
//! `all` libera sólo esperas registradas en la generación anterior, nunca crea permisos.
#![forbid(unsafe_code)]
pub const OK: i32 = 0;
pub const BAD_ARG: i32 = 4;
pub const ABORTED: i32 = 14;
pub const TIMED_OUT: i32 = 49;
pub const WAIT: i32 = -1;
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Estado { pub count: i64, pub generation: u64, pub destroyed: u32, pub reserved: u32 }
#[repr(C)]
pub struct Transicion { pub state: Estado, pub result: i32 }
impl Estado {
    pub fn new(value: i32) -> Self { Self { count: i64::from(value), generation: 0, destroyed: 0, reserved: 0 } }
    pub fn signal(mut self, all: bool) -> Transicion {
        let result = if self.destroyed != 0 { BAD_ARG } else if all {
            match self.generation.checked_add(1) { Some(g) => { self.generation = g; OK }, None => ABORTED }
        } else {
            match self.count.checked_add(1) { Some(n) => { self.count = n; OK }, None => ABORTED }
        };
        Transicion { state: self, result }
    }
    pub fn destroy(mut self) -> Transicion {
        let result = if self.destroyed != 0 { BAD_ARG } else { self.destroyed = 1; OK };
        Transicion { state: self, result }
    }
    // Comprobar disponibilidad antes de timeout: una señal que ganó el mutex gana la carrera.
    pub fn poll(mut self, generation: u64, error: i32) -> Transicion {
        let result = if self.destroyed != 0 { ABORTED }
        else if generation != self.generation { OK }
        else if self.count > 0 { self.count -= 1; OK }
        else if error != 0 { error } else { WAIT };
        Transicion { state: self, result }
    }
}
pub fn relative_ns(sec: u32, nsec: u32) -> i64 {
    if nsec >= 1_000_000_000 { -1 } else { i64::from(sec) * 1_000_000_000 + i64::from(nsec) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn wait_single() {
        let s = Estado::new(0);
        assert_eq!(s.poll(0, 0).result, WAIT);
        let s = s.signal(false).state;
        let t = s.poll(0, 0); assert_eq!(t.result, OK);
        assert_eq!(t.state.poll(0, 0).result, WAIT);
    }
    #[test] fn all_only_existing_waiters() {
        let s = Estado::new(0).signal(true).state;
        for _ in 0..2001 { assert_eq!(s.poll(0, 0).result, OK); }
        assert_eq!(s.poll(s.generation, 0).result, WAIT);
    }
    #[test] fn timeout_and_errors() {
        assert_eq!(Estado::new(0).poll(0, TIMED_OUT).result, TIMED_OUT);
        assert_eq!(Estado::new(0).poll(0, ABORTED).result, ABORTED);
        assert_eq!(Estado::new(1).poll(0, TIMED_OUT).result, OK);
        assert_eq!(relative_ns(0, 1_000_000_000), -1);
        assert_eq!(relative_ns(u32::MAX, 999_999_999), 4_294_967_295_999_999_999);
    }
    #[test] fn destroy_wakes_and_invalidates() {
        let t = Estado::new(1).destroy(); assert_eq!(t.result, OK);
        assert_eq!(t.state.poll(0, 0).result, ABORTED);
        assert_eq!(t.state.signal(false).result, BAD_ARG);
        assert_eq!(t.state.destroy().result, BAD_ARG);
    }
}
