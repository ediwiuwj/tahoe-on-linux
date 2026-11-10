//! Estado Mach puro. Snapshots propios por valor; nunca referencias a shm.
//! Conserva la semántica histórica (incluso guardas cero y conjuntos miembros).
#![forbid(unsafe_code)]

pub const NSETS: usize = 32;
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Sport {
    pub name: u32, pub sets: [u32; NSETS],
    pub head: u32, pub tail: u32, pub count: u32,
    pub owner: i32, pub rpid: i32,
    pub used: u8, pub is_set: u8, pub guarded: u8, pub strict_guard: u8,
    pub cv: u32, pub ctx: u64, pub pdreq: u32, pub svc: u8,
    pub svc_ctx: u64, pub qlimit: u32, pub nsreq: u32, pub nsmscount: u32,
    pub hnext: i32, pub spn: u32, pub me: u8,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Batch { pub ports: [Sport; 34], pub len: u32, pub target: u32, pub error: i32 }
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GuardResult { pub port: Sport, pub error: i32 }

pub fn in_set(p: Sport, set: u32) -> bool {
    p.sets.iter().take_while(|&&n| n != 0).any(|&n| n == set)
}
pub fn set_remove(mut p: Sport, set: u32) -> Sport {
    let mut n = 0;
    for j in 0..NSETS {
        if p.sets[j] == 0 { break; }
        if p.sets[j] != set { p.sets[n] = p.sets[j]; n += 1; }
    }
    // Igual que C: no toca datos después del primer cero original.
    for j in n..NSETS { if p.sets[j] == 0 { break; } p.sets[j] = 0; }
    p
}
// op: 0 insertar, 1 mover, 2 extraer. ports[0] es el miembro;
// target == len significa MACH_PORT_NULL. used=0 representa find fallido.
// Cada nombre aparece una sola vez: también conserva el caso miembro == conjunto.
pub fn membership(mut b: Batch, op: u32) -> Batch {
    b.error = 15;
    let len = b.len as usize;
    let t = b.target as usize;
    if len == 0 || len > 34 || t > len || b.ports[0].used == 0 { return b; }
    if op > 2 || (t == len && op != 1) { return b; }
    if t < len && (b.ports[t].used == 0 || b.ports[t].is_set == 0) { return b; }
    if op == 0 {
        let name = b.ports[t].name;
        if in_set(b.ports[0], name) { b.error = 17; return b; }
        let Some(j) = b.ports[0].sets.iter().position(|&n| n == 0) else { b.error = 3; return b; };
        b.ports[0].sets[j] = name;
        b.ports[t].count = b.ports[t].count.wrapping_add(b.ports[0].count);
    } else if op == 2 {
        let name = b.ports[t].name;
        if !in_set(b.ports[0], name) { b.error = 18; return b; }
        b.ports[0] = set_remove(b.ports[0], name);
        subtract(&mut b, t);
    } else {
        for j in 0..NSETS {
            let name = b.ports[0].sets[j];
            if name != 0 {
                if let Some(i) = (0..len).find(|&i| b.ports[i].used != 0 && b.ports[i].name == name) { subtract(&mut b, i); }
                b.ports[0].sets[j] = 0;
            }
        }
        if t < len {
            b.ports[0].sets[0] = b.ports[t].name;
            b.ports[t].count = b.ports[t].count.wrapping_add(b.ports[0].count);
        }
    }
    b.error = 0;
    b
}
fn subtract(b: &mut Batch, i: usize) {
    let count = b.ports[0].count;
    if b.ports[i].count >= count { b.ports[i].count -= count; }
}
// op: assert, swap, guard, unguard, set_context.
pub fn guard(mut p: Sport, op: u32, old: u64, new: u64, strict: i32) -> GuardResult {
    let error = if p.used == 0 { 15 } else {
        let valid = match op {
            0 => p.guarded != 0 && p.strict_guard != 0 && p.ctx == old,
            1 => p.guarded != 0 && p.strict_guard == 0 && p.ctx == old,
            2 => p.ctx == 0,
            3 => p.guarded != 0 && p.ctx == old,
            4 => p.strict_guard == 0,
            _ => false,
        };
        if !valid { 4 } else {
            match op {
                1 | 4 => p.ctx = new,
                2 => { p.ctx = old; p.guarded = 1; p.strict_guard = u8::from(strict != 0); }
                3 => { p.ctx = 0; p.guarded = 0; p.strict_guard = 0; }
                _ => {},
            }
            0
        }
    };
    GuardResult { port: p, error }
}

const _: () = {
    use core::mem::{size_of, align_of, offset_of};
    assert!(size_of::<Sport>() == 208 && align_of::<Sport>() == 8);
    assert!(offset_of!(Sport, name) == 0);
    assert!(offset_of!(Sport, sets) == 4);
    assert!(offset_of!(Sport, head) == 132);
    assert!(offset_of!(Sport, tail) == 136);
    assert!(offset_of!(Sport, count) == 140);
    assert!(offset_of!(Sport, owner) == 144);
    assert!(offset_of!(Sport, rpid) == 148);
    assert!(offset_of!(Sport, used) == 152);
    assert!(offset_of!(Sport, is_set) == 153);
    assert!(offset_of!(Sport, guarded) == 154);
    assert!(offset_of!(Sport, strict_guard) == 155);
    assert!(offset_of!(Sport, cv) == 156);
    assert!(offset_of!(Sport, ctx) == 160);
    assert!(offset_of!(Sport, pdreq) == 168);
    assert!(offset_of!(Sport, svc) == 172);
    assert!(offset_of!(Sport, svc_ctx) == 176);
    assert!(offset_of!(Sport, qlimit) == 184);
    assert!(offset_of!(Sport, nsreq) == 188);
    assert!(offset_of!(Sport, nsmscount) == 192);
    assert!(offset_of!(Sport, hnext) == 196);
    assert!(offset_of!(Sport, spn) == 200);
    assert!(offset_of!(Sport, me) == 204);
    assert!(size_of::<Batch>() == 7088);
    assert!(offset_of!(Batch, len) == 7072);
    assert!(offset_of!(Batch, target) == 7076);
    assert!(offset_of!(Batch, error) == 7080);
    assert!(size_of::<GuardResult>() == 216);
    assert!(offset_of!(GuardResult, error) == 208);
};

#[cfg(test)]
mod tests {
    use super::*;
    fn port(name: u32, is_set: u8, count: u32) -> Sport {
        Sport { name, used: 1, is_set, count, cv: 0x12345678, head: 9, tail: 10, ..Sport::default() }
    }
    fn batch() -> Batch {
        let mut b = Batch { ports: [Sport::default(); 34], len: 3, target: 1, error: -1 };
        b.ports[0] = port(10, 0, 3);
        b.ports[1] = port(20, 1, 5);
        b.ports[2] = port(30, 1, 7);
        b
    }
    fn unchanged_error(b: Batch, op: u32, error: i32) {
        let r = membership(b, op);
        assert_eq!(r.error, error);
        assert_eq!(r.ports, b.ports);
    }
    #[test]
    fn insert_duplicate_capacity_and_wrap() {
        let b = membership(batch(), 0);
        assert_eq!(b.error, 0);
        assert_eq!(b.ports[0].sets[0], 20);
        assert_eq!(b.ports[1].count, 8);
        assert_eq!(b.ports[0].cv, 0x12345678);
        assert_eq!(b.ports[0].head, 9);
        unchanged_error(b, 0, 17);
        let mut full = batch();
        full.ports[0].sets = core::array::from_fn(|i| 100 + i as u32);
        unchanged_error(full, 0, 3);
        full.ports[0].sets[31] = 0;
        full.ports[1].count = u32::MAX;
        let r = membership(full, 0);
        assert_eq!(r.error, 0);
        assert_eq!(r.ports[0].sets[31], 20);
        assert_eq!(r.ports[1].count, 2);
    }
    #[test]
    fn remove_compacts_first_last_duplicates_and_zero_boundary() {
        let mut p = port(10, 0, 0);
        p.sets[..5].copy_from_slice(&[20, 30, 20, 40, 50]);
        assert!(in_set(p, 50));
        assert!(!in_set(p, 0));
        let r = set_remove(p, 20);
        assert_eq!(&r.sets[..6], &[30, 40, 50, 0, 0, 0]);
        assert_eq!(set_remove(r, 50).sets[2], 0);
        assert_eq!(set_remove(p, 99), p);
        p.sets = [20; NSETS];
        assert_eq!(set_remove(p, 20).sets, [0; NSETS]);
        p.sets[0] = 0;
        assert!(!in_set(p, 20));
        assert_eq!(set_remove(p, 20), p);
    }
    #[test]
    fn extract_counts_underflow_and_absence() {
        unchanged_error(batch(), 2, 18);
        let mut b = membership(batch(), 0);
        b.ports[0].sets[1] = 30;
        let r = membership(b, 2);
        assert_eq!(r.error, 0);
        assert_eq!(r.ports[0].sets[0], 30);
        assert_eq!(r.ports[1].count, 5);
        b.ports[1].count = 2;
        assert_eq!(membership(b, 2).ports[1].count, 2);
    }
    #[test]
    fn move_all_to_target_null_stale_and_same_target() {
        let mut b = batch();
        b.ports[0].sets[..3].copy_from_slice(&[20, 30, 999]);
        b.ports[1].count = 8;
        b.ports[2].count = 2; // conserva la protección contra underflow de C
        let r = membership(b, 1);
        assert_eq!(r.error, 0);
        assert_eq!(r.ports[0].sets[0], 20);
        assert!(r.ports[0].sets[1..].iter().all(|&n| n == 0));
        assert_eq!(r.ports[1].count, 8);
        assert_eq!(r.ports[2].count, 2);
        b.target = b.len;
        let r = membership(b, 1);
        assert_eq!(r.error, 0);
        assert_eq!(r.ports[0].sets, [0; NSETS]);
        assert_eq!(r.ports[1].count, 5);
        let mut empty = batch();
        empty.target = empty.len;
        assert_eq!(membership(empty, 1).error, 0);
    }
    #[test]
    fn missing_wrong_type_and_batch_bounds_are_atomic() {
        for op in 0..3 {
            let mut b = batch(); b.ports[0].used = 0;
            unchanged_error(b, op, 15);
            b = batch(); b.ports[1].used = 0;
            unchanged_error(b, op, 15);
            b = batch(); b.ports[1].is_set = 0;
            unchanged_error(b, op, 15);
            for len in [0, 35, u32::MAX] {
                b = batch(); b.len = len;
                unchanged_error(b, op, 15);
            }
            b = batch(); b.target = 4;
            unchanged_error(b, op, 15);
        }
        let mut b = batch(); b.target = b.len;
        unchanged_error(b, 0, 15);
        unchanged_error(b, 2, 15);
        unchanged_error(batch(), 99, 15);
    }
    #[test]
    fn largest_snapshot_moves_all_32_memberships_and_ffi_roundtrip() {
        let mut b = batch();
        b.len = 34; b.target = 33;
        for i in 1..34 {
            b.ports[i] = port(100 + i as u32, 1, 3);
            if i <= 32 { b.ports[0].sets[i - 1] = b.ports[i].name; }
        }
        let r = crate::mach_puerto_ffi::nucleo_puerto_membership(b, 1);
        assert_eq!(r.error, 0);
        assert_eq!(r.ports[0].sets[0], 133);
        assert!(r.ports[0].sets[1..].iter().all(|&n| n == 0));
        for i in 1..33 { assert_eq!(r.ports[i].count, 0); }
        assert_eq!(r.ports[33].count, 6);
        assert_eq!(crate::mach_puerto_ffi::nucleo_puerto_in_set(r.ports[0], 133), 1);
        let p = crate::mach_puerto_ffi::nucleo_puerto_remove(r.ports[0], 133);
        assert_eq!(p.sets, [0; NSETS]);
        let g = crate::mach_puerto_ffi::nucleo_puerto_guard(p, 2, 9, 0, 1);
        assert_eq!(g.error, 0); assert_eq!(g.port.ctx, 9);
    }
    #[test]
    fn self_membership_uses_one_object_and_sequential_counts() {
        let mut b = batch(); b.target = 0; b.ports[0].is_set = 1;
        b = membership(b, 0);
        assert_eq!(b.error, 0);
        assert_eq!(b.ports[0].sets[0], 10);
        assert_eq!(b.ports[0].count, 6);
        let r = membership(b, 1);
        assert_eq!(r.ports[0].count, 0); // resta primero, suma el valor ya modificado
        assert_eq!(r.ports[0].sets[0], 10);
        assert_eq!(membership(b, 2).ports[0].count, 0);
    }
    #[test]
    fn guards_every_transition_and_rejection_preserves_port() {
        let p = port(10, 0, 3);
        for strict in [0, 1, -1] {
            let g = guard(p, 2, u64::MAX, 0, strict);
            assert_eq!(g.error, 0);
            assert_eq!(g.port.ctx, u64::MAX);
            assert_eq!(g.port.strict_guard, u8::from(strict != 0));
            assert_eq!(guard(g.port, 0, u64::MAX, 0, 0).error, if strict == 0 { 4 } else { 0 });
            assert_eq!(guard(g.port, 0, 7, 0, 0).error, 4);
            for (op, old, new) in [(1, 7, 9), (2, 7, 0), (3, 7, 0)] {
                let r = guard(g.port, op, old, new, 0);
                assert_eq!(r.error, 4); assert_eq!(r.port, g.port);
            }
            let swap = guard(g.port, 1, u64::MAX, 42, 0);
            let ctx = guard(g.port, 4, 0, 42, 0);
            assert_eq!(swap.error, if strict == 0 { 0 } else { 4 });
            assert_eq!(ctx.error, swap.error);
            assert_eq!(swap.port.ctx, if strict == 0 { 42 } else { u64::MAX });
            let u = guard(g.port, 3, u64::MAX, 0, 0);
            assert_eq!(u.error, 0);
            assert_eq!(u.port, p);
        }
        for op in 0..5 {
            let absent = Sport::default();
            let r = guard(absent, op, 0, 0, 0);
            assert_eq!(r.error, 15); assert_eq!(r.port, absent);
        }
        for op in [0, 1, 3, 99] {
            let r = guard(p, op, 0, 0, 0);
            assert_eq!(r.error, 4); assert_eq!(r.port, p);
        }
        let ctx = guard(p, 4, 0, 123, 0);
        assert_eq!(ctx.error, 0); assert_eq!(ctx.port.ctx, 123);
        assert_eq!(guard(ctx.port, 2, 1, 0, 0).error, 4);
    }
    #[test]
    fn zero_guard_can_be_guarded_again_as_in_original_c() {
        let p = port(1, 0, 0);
        let g = guard(p, 2, 0, 0, 1);
        assert_eq!(g.error, 0);
        assert_eq!(guard(g.port, 0, 0, 0, 0).error, 0);
        assert_eq!(guard(g.port, 3, 0, 0, 0).port, p);
        let g = guard(g.port, 2, 42, 0, 0);
        assert_eq!(g.error, 0); assert_eq!(g.port.ctx, 42);
        assert_eq!(g.port.strict_guard, 0);
    }
}
