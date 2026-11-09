//! Disposición en memoria de los tipos de AIR (datalayout de `air64`): punteros de 64 bits; vectores alineados por su
//! tamaño en bits (v96 → 128, v48 → 64: un float3 ocupa 16 bytes y un half3 8); estructuras con relleno natural salvo
//! las empaquetadas.

use super::ir::{Module, Type, TypeId};

/// Alineación (en bytes) de un tipo.
pub fn align(m: &Module, t: TypeId) -> u64 {
    match &m.types[t] {
        Type::Int(w) => ((*w as u64).div_ceil(8)).next_power_of_two().max(1),
        Type::Half | Type::BFloat => 2,
        Type::Float => 4,
        Type::Double | Type::Ptr(_) => 8,
        Type::Vector(n, e) => {
            let bits = *n as u64 * bits_elem(m, *e);
            bits.div_ceil(8).next_power_of_two().max(1)
        }
        Type::Array(_, e) => align(m, *e),
        Type::Struct { packed: true, .. } => 1,
        Type::Struct { elems, .. } => elems.iter().map(|&e| align(m, e)).max().unwrap_or(1),
        _ => 1,
    }
}

fn bits_elem(m: &Module, t: TypeId) -> u64 {
    match &m.types[t] {
        Type::Int(w) => *w as u64,
        Type::Half | Type::BFloat => 16,
        Type::Float => 32,
        Type::Double | Type::Ptr(_) => 64,
        _ => 8 * size(m, t),
    }
}

/// Tamaño de almacenamiento (alloc size: con el relleno hasta la alineación), en bytes.
pub fn size(m: &Module, t: TypeId) -> u64 {
    let raw = match &m.types[t] {
        Type::Int(w) => (*w as u64).div_ceil(8),
        Type::Half | Type::BFloat => 2,
        Type::Float => 4,
        Type::Double | Type::Ptr(_) => 8,
        Type::Vector(n, e) => (*n as u64 * bits_elem(m, *e)).div_ceil(8),
        Type::Array(n, e) => n * size(m, *e),
        Type::Struct { packed, elems, .. } => {
            let mut off: u64 = 0;
            for &e in elems {
                if !packed {
                    off = off.next_multiple_of(align(m, e));
                }
                off += size(m, e);
            }
            off
        }
        _ => 0,
    };
    raw.next_multiple_of(align(m, t))
}

/// Desplazamiento de cada campo de una estructura.
pub fn offsets(m: &Module, t: TypeId) -> Vec<u64> {
    let Type::Struct { packed, elems, .. } = &m.types[t] else { return Vec::new() };
    let mut v = Vec::with_capacity(elems.len());
    let mut off: u64 = 0;
    for &e in elems {
        if !packed {
            off = off.next_multiple_of(align(m, e));
        }
        v.push(off);
        off += size(m, e);
    }
    v
}

/// Desplazamiento de un elemento de vector (los elementos van seguidos, sin relleno).
pub fn elem_size(m: &Module, t: TypeId) -> u64 {
    match &m.types[t] {
        Type::Vector(_, e) => bits_elem(m, *e).div_ceil(8),
        Type::Array(_, e) => size(m, *e),
        _ => size(m, t),
    }
}
