//! Intérprete del IR de AIR en la CPU. Sirve para tres cosas:
//! - ejecutar el constructor global que deriva los predicados de las function constants (`__metal_implicit_fc_pred_N`)
//!   a partir de los valores que da la app al crear el pipeline;
//! - plegar constantes al traducir (`plegar`: las instrucciones sin memoria con operandos conocidos);
//! - de referencia en las pruebas: el mismo kernel en la CPU y en la GPU deben dar lo mismo.

use super::ir::{BinOp, CastOp, Const, Function, Inst, Module, Type, TypeId, ValueId, ValueKind};
use super::layout;

pub type Res<T> = Result<T, String>;

/// Un valor concreto.
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    /// Entero de `bits` bits, guardado con extensión de ceros.
    Int(u32, u64),
    /// Real; `bits` es 16, 32 o 64 (el valor ya está redondeado a esa precisión).
    Real(u32, f64),
    Vec(Vec<V>),
    Agg(Vec<V>),
    Ptr(Mem, i64),
    Undef,
}

/// Región de memoria a la que apunta un puntero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mem {
    Null,
    Global(usize),
    Alloca(usize),
    Function(usize),
    /// Región externa (buffers de un kernel en las pruebas): número que da quien llama.
    Externa(u32),
}

fn mask(bits: u32) -> u64 {
    if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 }
}

fn sext(bits: u32, v: u64) -> i64 {
    if bits == 0 || bits >= 64 {
        return v as i64;
    }
    let s = 64 - bits;
    ((v << s) as i64) >> s
}

/// bfloat16 (los 16 bits altos de un f32, redondeo al par más cercano). En el intérprete se marca con «17 bits».
pub const BF16: u32 = 17;

pub fn f32_a_bf16(x: f32) -> u16 {
    let b = x.to_bits();
    if x.is_nan() {
        return ((b >> 16) | 0x40) as u16;
    }
    ((b + 0x7fff + ((b >> 16) & 1)) >> 16) as u16
}

pub fn bf16_a_f32(h: u16) -> f32 {
    f32::from_bits((h as u32) << 16)
}

fn redondear(bits: u32, x: f64) -> f64 {
    match bits {
        16 => f16_a_f32(f32_a_f16(x as f32)) as f64,
        BF16 => bf16_a_f32(f32_a_bf16(x as f32)) as f64,
        32 => x as f32 as f64,
        _ => x,
    }
}

/// float → half (redondeo al más cercano).
pub fn f32_a_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let s = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32;
    let m = b & 0x7f_ffff;
    if e == 0xff {
        return s | 0x7c00 | if m != 0 { 0x200 } else { 0 };
    }
    let e2 = e - 127 + 15;
    if e2 >= 0x1f {
        return s | 0x7c00;
    }
    if e2 <= 0 {
        if e2 < -10 {
            return s;
        }
        let m = m | 0x80_0000;
        let shift = (14 - e2) as u32;
        let half = 1 << (shift - 1);
        let r = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let r = if rem > half || (rem == half && r & 1 != 0) { r + 1 } else { r };
        return s | r as u16;
    }
    let r = ((e2 as u32) << 10) | (m >> 13);
    let rem = m & 0x1fff;
    let r = if rem > 0x1000 || (rem == 0x1000 && r & 1 != 0) { r + 1 } else { r };
    s | r as u16
}

pub fn f16_a_f32(h: u16) -> f32 {
    let s = ((h & 0x8000) as u32) << 16;
    let e = ((h >> 10) & 0x1f) as u32;
    let m = (h & 0x3ff) as u32;
    let b = if e == 0 {
        if m == 0 {
            s
        } else {
            // subnormal
            let mut e = 127 - 15 + 1;
            let mut m = m;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            s | (e << 23) | ((m & 0x3ff) << 13)
        }
    } else if e == 0x1f {
        s | 0x7f80_0000 | (m << 13)
    } else {
        s | ((e + 127 - 15) << 23) | (m << 13)
    };
    f32::from_bits(b)
}

impl V {
    pub fn int(&self) -> Option<u64> {
        match self {
            V::Int(_, v) => Some(*v),
            _ => None,
        }
    }
    pub fn real(&self) -> Option<f64> {
        match self {
            V::Real(_, v) => Some(*v),
            _ => None,
        }
    }
    fn elems(&self) -> Option<&[V]> {
        match self {
            V::Vec(v) | V::Agg(v) => Some(v),
            _ => None,
        }
    }
}

/// El valor de una constante del IR.
pub fn constante(m: &Module, values: &[super::ir::Value], ty: TypeId, c: &Const) -> Option<V> {
    Some(match c {
        Const::Null => cero(m, ty),
        Const::Undef | Const::Poison => V::Undef,
        Const::Int(n) => match &m.types[ty] {
            Type::Int(w) => V::Int(*w, (*n as u64) & mask(*w)),
            _ => return None,
        },
        Const::Float(b) => match &m.types[ty] {
            Type::Half => V::Real(16, f16_a_f32(*b as u16) as f64),
            Type::BFloat => V::Real(BF16, bf16_a_f32(*b as u16) as f64),
            Type::Float => V::Real(32, f32::from_bits(*b as u32) as f64),
            Type::Double => V::Real(64, f64::from_bits(*b)),
            _ => return None,
        },
        Const::Aggregate(ids) => {
            let mut v = Vec::with_capacity(ids.len());
            for &i in ids {
                let x = values.get(i)?;
                v.push(match &x.kind {
                    ValueKind::Const(c) => constante(m, values, x.ty, c)?,
                    ValueKind::Global(g) => V::Ptr(Mem::Global(*g), 0),
                    ValueKind::Function(f) => V::Ptr(Mem::Function(*f), 0),
                    _ => return None,
                });
            }
            if matches!(m.types[ty], Type::Vector(..)) { V::Vec(v) } else { V::Agg(v) }
        }
        Const::Data(d) => {
            let e = match &m.types[ty] {
                Type::Vector(_, e) | Type::Array(_, e) => *e,
                _ => return None,
            };
            let v: Vec<V> = d
                .iter()
                .map(|&x| match &m.types[e] {
                    Type::Int(w) => V::Int(*w, x & mask(*w)),
                    Type::Half => V::Real(16, f16_a_f32(x as u16) as f64),
                    Type::BFloat => V::Real(BF16, bf16_a_f32(x as u16) as f64),
                    Type::Float => V::Real(32, f32::from_bits(x as u32) as f64),
                    Type::Double => V::Real(64, f64::from_bits(x)),
                    _ => V::Undef,
                })
                .collect();
            if matches!(m.types[ty], Type::Vector(..)) { V::Vec(v) } else { V::Agg(v) }
        }
        Const::Str(s) => V::Agg(s.iter().map(|&b| V::Int(8, b as u64)).collect()),
        Const::Expr { code, ops } => {
            // CE_CAST [opcode, tipo, valor]: los usados en AIR (addrspacecast y bitcast de punteros)
            if *code == 11 && ops.len() >= 3 && (ops[0] == 11 || ops[0] == 12) {
                let x = values.get(ops[2] as usize)?;
                return match &x.kind {
                    ValueKind::Global(g) => Some(V::Ptr(Mem::Global(*g), 0)),
                    ValueKind::Function(f) => Some(V::Ptr(Mem::Function(*f), 0)),
                    ValueKind::Const(c) => constante(m, values, x.ty, c),
                    _ => None,
                };
            }
            // GEP constante: CE_GEP (32) [tipo base, banderas, (tipo, valor)*]; CE_INBOUNDS_GEP (20) y CE_GEP antiguo (12):
            // [(tipo base), (tipo, valor)*] (con número impar de operandos, el primero es el tipo base)
            let (src, pares) = match *code {
                32 => (ops.first()?, ops.get(2..)?),
                20 | 12 if ops.len() % 2 == 1 => (ops.first()?, ops.get(1..)?),
                _ => return None,
            };
            let mut vals = Vec::new();
            for par in pares.chunks(2) {
                let x = values.get(*par.get(1)? as usize)?;
                vals.push(match &x.kind {
                    ValueKind::Global(g) => V::Ptr(Mem::Global(*g), 0),
                    ValueKind::Function(f) => V::Ptr(Mem::Function(*f), 0),
                    ValueKind::Const(c) => constante(m, values, x.ty, c)?,
                    _ => return None,
                });
            }
            let (base, idx) = vals.split_first()?;
            return gep(m, *src as usize, base, idx);
        }
    })
}

/// El cero de un tipo.
pub fn cero(m: &Module, ty: TypeId) -> V {
    match &m.types[ty] {
        Type::Int(w) => V::Int(*w, 0),
        Type::Half => V::Real(16, 0.0),
        Type::BFloat => V::Real(BF16, 0.0),
        Type::Float => V::Real(32, 0.0),
        Type::Double => V::Real(64, 0.0),
        Type::Ptr(_) => V::Ptr(Mem::Null, 0),
        Type::Vector(n, e) => V::Vec(vec![cero(m, *e); *n as usize]),
        Type::Array(n, e) => V::Agg(vec![cero(m, *e); *n as usize]),
        Type::Struct { elems, .. } => V::Agg(elems.iter().map(|&e| cero(m, e)).collect()),
        _ => V::Undef,
    }
}

fn real_bits(m: &Module, t: TypeId) -> Option<u32> {
    match &m.types[t] {
        Type::Half => Some(16),
        Type::BFloat => Some(BF16),
        Type::Float => Some(32),
        Type::Double => Some(64),
        Type::Vector(_, e) => real_bits(m, *e),
        _ => None,
    }
}

fn int_bits(m: &Module, t: TypeId) -> Option<u32> {
    match &m.types[t] {
        Type::Int(w) => Some(*w),
        Type::Vector(_, e) => int_bits(m, *e),
        _ => None,
    }
}

/// Aplica `f` componente a componente (vectores) o al valor (escalares).
fn por_comp(a: &V, b: &V, f: &impl Fn(&V, &V) -> Option<V>) -> Option<V> {
    match (a, b) {
        (V::Vec(x), V::Vec(y)) => Some(V::Vec(x.iter().zip(y).map(|(p, q)| por_comp(p, q, f)).collect::<Option<_>>()?)),
        _ => f(a, b),
    }
}

fn bin(op: BinOp, a: &V, b: &V) -> Option<V> {
    por_comp(a, b, &|a, b| match (a, b) {
        (V::Int(w, x), V::Int(_, y)) => {
            let (w, x, y) = (*w, *x, *y);
            let (sx, sy) = (sext(w, x), sext(w, y));
            let r = match op {
                BinOp::Add => x.wrapping_add(y),
                BinOp::Sub => x.wrapping_sub(y),
                BinOp::Mul => x.wrapping_mul(y),
                // entre cero: indefinido en LLVM (y en la GPU, un valor cualquiera)
                BinOp::UDiv => match x.checked_div(y) { Some(r) => r, None => return Some(V::Undef) },
                BinOp::SDiv => match sx.checked_div(sy) { Some(r) => r as u64, None => return Some(V::Undef) },
                BinOp::URem => match x.checked_rem(y) { Some(r) => r, None => return Some(V::Undef) },
                BinOp::SRem => match sx.checked_rem(sy) { Some(r) => r as u64, None => return Some(V::Undef) },
                BinOp::Shl => if y >= w as u64 { return Some(V::Undef) } else { x << y },
                BinOp::LShr => if y >= w as u64 { return Some(V::Undef) } else { x >> y },
                BinOp::AShr => if y >= w as u64 { return Some(V::Undef) } else { (sx >> y) as u64 },
                BinOp::And => x & y,
                BinOp::Or => x | y,
                BinOp::Xor => x ^ y,
            };
            Some(V::Int(w, r & mask(w)))
        }
        (V::Real(w, x), V::Real(_, y)) => {
            let r = match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::UDiv | BinOp::SDiv => x / y, // fdiv llega con el mismo código
                BinOp::URem | BinOp::SRem => x % y,
                _ => return None,
            };
            Some(V::Real(*w, redondear(*w, r)))
        }
        (V::Undef, _) | (_, V::Undef) => Some(V::Undef),
        _ => None,
    })
}

fn cmp(pred: u32, a: &V, b: &V) -> Option<V> {
    por_comp(a, b, &|a, b| {
        let r = match (a, b) {
            (V::Real(_, x), V::Real(_, y)) => {
                let uno = x.is_nan() || y.is_nan();
                match pred {
                    0 => false,
                    1 => !uno && x == y,
                    2 => !uno && x > y,
                    3 => !uno && x >= y,
                    4 => !uno && x < y,
                    5 => !uno && x <= y,
                    6 => !uno && x != y,
                    7 => !uno,
                    8 => uno,
                    9 => uno || x == y,
                    10 => uno || x > y,
                    11 => uno || x >= y,
                    12 => uno || x < y,
                    13 => uno || x <= y,
                    14 => uno || x != y,
                    15 => true,
                    _ => return None,
                }
            }
            (V::Int(w, x), V::Int(_, y)) => {
                let (sx, sy) = (sext(*w, *x), sext(*w, *y));
                match pred {
                    32 => x == y,
                    33 => x != y,
                    34 => x > y,
                    35 => x >= y,
                    36 => x < y,
                    37 => x <= y,
                    38 => sx > sy,
                    39 => sx >= sy,
                    40 => sx < sy,
                    41 => sx <= sy,
                    _ => return None,
                }
            }
            (V::Ptr(m1, o1), V::Ptr(m2, o2)) => match pred {
                32 => m1 == m2 && o1 == o2,
                33 => m1 != m2 || o1 != o2,
                _ => return None,
            },
            _ => return None,
        };
        Some(V::Int(1, r as u64))
    })
}

fn cast(m: &Module, op: CastOp, v: &V, to: TypeId) -> Option<V> {
    let elem_to = match &m.types[to] {
        Type::Vector(_, e) => *e,
        _ => to,
    };
    if let V::Vec(x) = v {
        if !matches!(op, CastOp::BitCast) {
            return Some(V::Vec(x.iter().map(|e| cast(m, op, e, elem_to)).collect::<Option<_>>()?));
        }
    }
    let tb = int_bits(m, to);
    let tr = real_bits(m, to);
    Some(match (op, v) {
        (_, V::Undef) => V::Undef,
        (CastOp::Trunc | CastOp::ZExt, V::Int(_, x)) => V::Int(tb?, x & mask(tb?)),
        (CastOp::SExt, V::Int(w, x)) => V::Int(tb?, sext(*w, *x) as u64 & mask(tb?)),
        (CastOp::FPToUI, V::Real(_, x)) => V::Int(tb?, (*x as u64) & mask(tb?)),
        (CastOp::FPToSI, V::Real(_, x)) => V::Int(tb?, (*x as i64) as u64 & mask(tb?)),
        (CastOp::UIToFP, V::Int(_, x)) => V::Real(tr?, redondear(tr?, *x as f64)),
        (CastOp::SIToFP, V::Int(w, x)) => V::Real(tr?, redondear(tr?, sext(*w, *x) as f64)),
        (CastOp::FPTrunc | CastOp::FPExt, V::Real(_, x)) => V::Real(tr?, redondear(tr?, *x)),
        (CastOp::AddrSpaceCast | CastOp::BitCast, V::Ptr(..)) => v.clone(),
        (CastOp::PtrToInt, V::Ptr(Mem::Null, o)) => V::Int(tb?, *o as u64 & mask(tb?)),
        (CastOp::IntToPtr, V::Int(_, 0)) => V::Ptr(Mem::Null, 0),
        (CastOp::BitCast, _) => {
            let bytes = a_bytes_v(v)?;
            de_bytes(m, to, &bytes)?
        }
        _ => return None,
    })
}

/// Bytes de un valor según su propia forma (para bitcast entre tipos del mismo tamaño).
fn a_bytes_v(v: &V) -> Option<Vec<u8>> {
    Some(match v {
        V::Int(w, x) => x.to_le_bytes()[..(*w as usize).div_ceil(8)].to_vec(),
        V::Real(16, x) => f32_a_f16(*x as f32).to_le_bytes().to_vec(),
        V::Real(32, x) => (*x as f32).to_le_bytes().to_vec(),
        V::Real(_, x) => x.to_le_bytes().to_vec(),
        V::Vec(e) => e.iter().map(a_bytes_v).collect::<Option<Vec<_>>>()?.concat(),
        _ => return None,
    })
}

/// Un valor del tipo `t` desde bytes (disposición de memoria de AIR).
pub fn de_bytes(m: &Module, t: TypeId, b: &[u8]) -> Option<V> {
    let rd = |n: usize| -> Option<u64> {
        let s = b.get(..n)?;
        let mut a = [0u8; 8];
        a[..n].copy_from_slice(s);
        Some(u64::from_le_bytes(a))
    };
    Some(match &m.types[t] {
        Type::Int(w) => V::Int(*w, rd((*w as usize).div_ceil(8))? & mask(*w)),
        Type::Half => V::Real(16, f16_a_f32(rd(2)? as u16) as f64),
        Type::BFloat => V::Real(BF16, bf16_a_f32(rd(2)? as u16) as f64),
        Type::Float => V::Real(32, f32::from_bits(rd(4)? as u32) as f64),
        Type::Double => V::Real(64, f64::from_bits(rd(8)?)),
        Type::Vector(n, e) => {
            let es = layout::elem_size(m, t) as usize;
            V::Vec((0..*n as usize).map(|i| de_bytes(m, *e, b.get(i * es..)?)).collect::<Option<_>>()?)
        }
        Type::Array(n, e) => {
            let es = layout::size(m, *e) as usize;
            V::Agg((0..*n as usize).map(|i| de_bytes(m, *e, b.get(i * es..)?)).collect::<Option<_>>()?)
        }
        Type::Struct { elems, .. } => {
            let offs = layout::offsets(m, t);
            V::Agg(elems.iter().zip(offs).map(|(&e, o)| de_bytes(m, e, b.get(o as usize..)?)).collect::<Option<_>>()?)
        }
        _ => return None,
    })
}

/// Bytes de un valor del tipo `t` (lo inverso de `de_bytes`). Los punteros no se pueden guardar en memoria
/// interpretada salvo nulos.
pub fn a_bytes(m: &Module, t: TypeId, v: &V, out: &mut [u8]) -> Option<()> {
    let wr = |out: &mut [u8], n: usize, x: u64| -> Option<()> {
        out.get_mut(..n)?.copy_from_slice(&x.to_le_bytes()[..n]);
        Some(())
    };
    match (&m.types[t], v) {
        (_, V::Undef) => Some(()),
        (Type::Int(w), V::Int(_, x)) => wr(out, (*w as usize).div_ceil(8), *x),
        (Type::Half, V::Real(_, x)) => wr(out, 2, f32_a_f16(*x as f32) as u64),
        (Type::BFloat, V::Real(_, x)) => wr(out, 2, f32_a_bf16(*x as f32) as u64),
        (Type::Float, V::Real(_, x)) => wr(out, 4, (*x as f32).to_bits() as u64),
        (Type::Double, V::Real(_, x)) => wr(out, 8, x.to_bits()),
        (Type::Ptr(_), V::Ptr(Mem::Null, o)) => wr(out, 8, *o as u64),
        (Type::Vector(_, e), V::Vec(xs)) => {
            let es = layout::elem_size(m, t) as usize;
            for (i, x) in xs.iter().enumerate() {
                a_bytes(m, *e, x, out.get_mut(i * es..)?)?;
            }
            Some(())
        }
        (Type::Array(_, e), V::Agg(xs)) => {
            let es = layout::size(m, *e) as usize;
            for (i, x) in xs.iter().enumerate() {
                a_bytes(m, *e, x, out.get_mut(i * es..)?)?;
            }
            Some(())
        }
        (Type::Struct { elems, .. }, V::Agg(xs)) => {
            for ((&e, o), x) in elems.iter().zip(layout::offsets(m, t)).zip(xs) {
                a_bytes(m, e, x, out.get_mut(o as usize..)?)?;
            }
            Some(())
        }
        _ => None,
    }
}

fn agg_get(v: &V, idx: &[u32]) -> Option<V> {
    let mut v = v.clone();
    for &i in idx {
        v = match v {
            V::Undef => return Some(V::Undef),
            _ => v.elems()?.get(i as usize)?.clone(),
        };
    }
    Some(v)
}

fn agg_set(v: &V, idx: &[u32], x: V) -> Option<V> {
    if idx.is_empty() {
        return Some(x);
    }
    let mut e = match v {
        V::Vec(e) | V::Agg(e) => e.clone(),
        _ => return None,
    };
    let i = idx[0] as usize;
    let n = agg_set(e.get(i)?, &idx[1..], x)?;
    e[i] = n;
    Some(if matches!(v, V::Vec(_)) { V::Vec(e) } else { V::Agg(e) })
}

/// Pliega una instrucción sin efectos con operandos conocidos. None si no se puede (falta un operando, tiene
/// memoria o efectos, o no es una operación pura).
pub fn plegar(m: &Module, f: &Function, inst: &Inst, ty: TypeId, get: &dyn Fn(ValueId) -> Option<V>) -> Option<V> {
    Some(match inst {
        Inst::Bin { op, a, b } => bin(*op, &get(*a)?, &get(*b)?)?,
        Inst::FNeg(a) => match get(*a)? {
            V::Real(w, x) => V::Real(w, -x),
            V::Vec(xs) => V::Vec(xs.into_iter().map(|x| if let V::Real(w, x) = x { V::Real(w, -x) } else { x }).collect()),
            v => v,
        },
        Inst::Cast { op, v } => cast(m, *op, &get(*v)?, ty)?,
        Inst::Cmp { pred, a, b } => cmp(*pred, &get(*a)?, &get(*b)?)?,
        Inst::Select { c, t, f: fl } => match get(*c)? {
            V::Int(_, x) => if x != 0 { get(*t)? } else { get(*fl)? },
            V::Vec(cs) => {
                let (tv, fv) = (get(*t)?, get(*fl)?);
                let (te, fe) = (tv.elems()?, fv.elems()?);
                V::Vec(cs.iter().enumerate().map(|(i, c)| if c.int() != Some(0) { te[i].clone() } else { fe[i].clone() }).collect())
            }
            _ => return None,
        },
        Inst::ExtractElt { v, i } => {
            let i = get(*i)?.int()? as usize;
            match get(*v)? {
                V::Vec(e) => e.get(i)?.clone(),
                V::Undef => V::Undef,
                _ => return None,
            }
        }
        Inst::InsertElt { v, e, i } => {
            let i = get(*i)?.int()? as u32;
            let base = match get(*v)? {
                V::Undef => match &m.types[ty] {
                    Type::Vector(n, _) => V::Vec(vec![V::Undef; *n as usize]),
                    _ => return None,
                },
                x => x,
            };
            agg_set(&base, &[i], get(*e)?)?
        }
        Inst::Shuffle { a, b, mask: mk } => {
            let n = match &m.types[f.values[*a].ty] {
                Type::Vector(n, _) => *n as usize,
                _ => return None,
            };
            let ex = |v: V| -> Vec<V> { match v { V::Vec(e) => e, _ => vec![V::Undef; n] } };
            let (av, bv) = (ex(get(*a)?), ex(get(*b).unwrap_or(V::Undef)));
            let mv = match get(*mk)? {
                V::Vec(e) => e,
                V::Undef => vec![V::Undef; match &m.types[ty] { Type::Vector(k, _) => *k as usize, _ => return None }],
                _ => return None,
            };
            V::Vec(mv.iter().map(|x| match x.int() {
                Some(k) if (k as usize) < n => av[k as usize].clone(),
                Some(k) => bv.get(k as usize - n).cloned().unwrap_or(V::Undef),
                None => V::Undef,
            }).collect())
        }
        Inst::ExtractVal { v, idx } => agg_get(&get(*v)?, idx)?,
        Inst::InsertVal { agg, v, idx } => {
            let base = match get(*agg)? {
                V::Undef => cero_undef(m, ty),
                x => x,
            };
            agg_set(&base, idx, get(*v)?)?
        }
        Inst::Freeze(v) => match get(*v)? {
            V::Undef => cero(m, ty),
            x => x,
        },
        _ => return None,
    })
}

/// Un agregado del tipo con todos sus elementos indefinidos (base de insertvalue sobre undef).
fn cero_undef(m: &Module, ty: TypeId) -> V {
    match &m.types[ty] {
        Type::Vector(n, _) => V::Vec(vec![V::Undef; *n as usize]),
        Type::Array(n, _) => V::Agg(vec![V::Undef; *n as usize]),
        Type::Struct { elems, .. } => V::Agg(vec![V::Undef; elems.len()]),
        _ => V::Undef,
    }
}

// ---------------------------------------------------------------- ejecución

/// Memoria del intérprete: una región de bytes por global, por alloca y por región externa.
#[derive(Default)]
pub struct Memoria {
    pub globales: Vec<Vec<u8>>,
    pub allocas: Vec<Vec<u8>>,
    pub externas: Vec<Vec<u8>>,
}

impl Memoria {
    /// Memoria con las globales del módulo inicializadas.
    pub fn nueva(m: &Module) -> Memoria {
        let mut g = Vec::with_capacity(m.globals.len());
        for gl in &m.globals {
            let mut b = vec![0u8; layout::size(m, gl.ty) as usize];
            if let Some(i) = gl.init {
                if let Some(x) = m.values.get(i) {
                    if let ValueKind::Const(c) = &x.kind {
                        if let Some(v) = constante(m, &m.values, gl.ty, c) {
                            let _ = a_bytes(m, gl.ty, &v, &mut b);
                        }
                    }
                }
            }
            g.push(b);
        }
        Memoria { globales: g, allocas: Vec::new(), externas: Vec::new() }
    }
    fn region(&mut self, r: Mem) -> Option<&mut Vec<u8>> {
        match r {
            Mem::Global(i) => self.globales.get_mut(i),
            Mem::Alloca(i) => self.allocas.get_mut(i),
            Mem::Externa(i) => self.externas.get_mut(i as usize),
            _ => None,
        }
    }
    pub fn cargar(&mut self, m: &Module, p: &V, t: TypeId) -> Res<V> {
        let V::Ptr(r, o) = p else { return Err(format!("load de algo que no es un puntero: {p:?}")) };
        let b = self.region(*r).ok_or_else(|| format!("load de {r:?}"))?;
        let n = layout::size(m, t) as usize;
        let Some(s) = b.get(*o as usize..*o as usize + n) else {
            // fuera de un buffer: 0, como con robustBufferAccess en la GPU
            if matches!(r, Mem::Externa(_)) {
                return de_bytes(m, t, &vec![0u8; n]).ok_or_else(|| "load de un tipo no soportado".into());
            }
            return Err(format!("load fuera de {r:?} ({o}+{n})"));
        };
        de_bytes(m, t, s).ok_or_else(|| "load de un tipo no soportado".into())
    }
    pub fn guardar(&mut self, m: &Module, p: &V, t: TypeId, v: &V) -> Res<()> {
        let V::Ptr(r, o) = p else { return Err(format!("store en algo que no es un puntero: {p:?}")) };
        let n = layout::size(m, t) as usize;
        let b = self.region(*r).ok_or_else(|| format!("store en {r:?}"))?;
        let Some(s) = b.get_mut(*o as usize..*o as usize + n) else {
            if matches!(r, Mem::Externa(_)) {
                return Ok(()); // fuera de un buffer: se ignora (robustBufferAccess)
            }
            return Err(format!("store fuera de {r:?}"));
        };
        a_bytes(m, t, v, s).ok_or_else(|| "store de un tipo no soportado".into())
    }
}

/// Llamadas que no son funciones del módulo (air.*, llvm.*): nombre y argumentos → resultado (o None si no se sabe).
pub type Externas<'a> = dyn FnMut(&str, &[V]) -> Option<Option<V>> + 'a;

/// GEP sobre un puntero conocido (para el análisis de constantes).
pub fn gep_pub(m: &Module, src: TypeId, base: &V, idx: &[V]) -> Option<V> {
    gep(m, src, base, idx)
}

fn gep(m: &Module, src: TypeId, base: &V, idx: &[V]) -> Option<V> {
    let V::Ptr(r, mut o) = base.clone() else { return None };
    let mut t = src;
    for (k, i) in idx.iter().enumerate() {
        let i = sext(match i { V::Int(w, _) => *w, _ => 64 }, i.int()?);
        if k == 0 {
            o += i * layout::size(m, t) as i64;
            continue;
        }
        match &m.types[t] {
            Type::Struct { elems, .. } => {
                o += layout::offsets(m, t)[i as usize] as i64;
                t = elems[i as usize];
            }
            Type::Array(_, e) => {
                o += i * layout::size(m, *e) as i64;
                t = *e;
            }
            Type::Vector(_, e) => {
                o += i * layout::elem_size(m, t) as i64;
                t = *e;
            }
            _ => return None,
        }
    }
    Some(V::Ptr(r, o))
}

/// Ejecuta una función del módulo con esos argumentos. `externas` resuelve las llamadas a funciones sin cuerpo.
pub fn ejecutar(m: &Module, fi: usize, args: &[V], mem: &mut Memoria, externas: &mut Externas, pasos: &mut u64) -> Res<Option<V>> {
    let f = &m.functions[fi];
    if f.is_proto {
        return externas(&f.name, args).ok_or_else(|| format!("llamada externa sin resolver: {}", f.name));
    }
    let mut h = Hilo::nuevo(f, args);
    loop {
        if let Parada::Fin(v) = correr(m, f, &mut h, mem, externas, pasos, u64::MAX)? {
            return Ok(v);
        }
    }
}

/// Estado de un hilo del intérprete: se puede parar en una barrera y seguir después (para ejecutar un threadgroup).
pub struct Hilo {
    reg: Vec<Option<V>>,
    bb: usize,
    prev: usize,
    /// instrucción por la que sigue dentro de `bb` (0: el bloque empieza, con sus phis)
    pos: usize,
    /// valor que espera el resultado de una función colectiva
    pendiente: Option<ValueId>,
}

pub enum Parada {
    Fin(Option<V>),
    /// llegó a una barrera de threadgroup (air.wg.barrier)
    Barrera,
    /// llegó a una función de SIMD-group (air.simd_*, air.quad_*): espera a los demás hilos del grupo; el resultado
    /// se le da con `Hilo::reanudar`
    Colectiva(String, Vec<V>),
}

impl Hilo {
    pub fn nuevo(f: &Function, args: &[V]) -> Hilo {
        let mut reg: Vec<Option<V>> = vec![None; f.values.len()];
        for (k, &a) in f.args.iter().enumerate() {
            reg[a] = args.get(k).cloned();
        }
        Hilo { reg, bb: 0, prev: usize::MAX, pos: 0, pendiente: None }
    }

    /// Da el resultado de la función colectiva en la que se paró.
    pub fn reanudar(&mut self, v: Option<V>) {
        if let (Some(k), Some(v)) = (self.pendiente.take(), v) {
            self.reg[k] = Some(v);
        }
    }
}

/// Ejecuta `f` (que puede no estar en el módulo, p. ej. con todo en línea) desde donde se quedó el hilo hasta que
/// termina o llega a una barrera. Las llamadas a funciones del módulo se ejecutan enteras (sin barreras dentro).
pub fn correr(m: &Module, f: &Function, h: &mut Hilo, mem: &mut Memoria, externas: &mut Externas, pasos: &mut u64, max_pasos: u64) -> Res<Parada> {
    let valor = |reg: &Vec<Option<V>>, v: ValueId| -> Option<V> {
        if let Some(x) = reg.get(v).cloned().flatten() {
            return Some(x);
        }
        let x = f.values.get(v)?;
        match &x.kind {
            ValueKind::Const(c) => constante(m, &f.values, x.ty, c),
            ValueKind::Global(g) => Some(V::Ptr(Mem::Global(*g), 0)),
            ValueKind::Function(g) => Some(V::Ptr(Mem::Function(*g), 0)),
            _ => None,
        }
    };
    let limite = max_pasos.min(50_000_000);
    loop {
        let blk = f.blocks.get(h.bb).ok_or("salto a un bloque inexistente")?;
        if h.pos == 0 {
            // phis: todos a la vez con los valores del bloque anterior
            let mut nuevos = Vec::new();
            for ins in &blk.insts {
                if let Inst::Phi { incoming } = &ins.inst {
                    let (v, _) = incoming.iter().find(|(_, b)| *b == h.prev).ok_or("phi sin entrada para el bloque anterior")?;
                    nuevos.push((ins.val.unwrap(), valor(&h.reg, *v).unwrap_or(V::Undef)));
                }
            }
            for (k, v) in nuevos {
                h.reg[k] = Some(v);
            }
        }
        let mut siguiente = None;
        while h.pos < blk.insts.len() {
            let ins = &blk.insts[h.pos];
            h.pos += 1;
            *pasos += 1;
            if *pasos > limite {
                return Err("demasiados pasos (¿bucle infinito?)".into());
            }
            let reg = &h.reg;
            let get = |v: ValueId| valor(reg, v);
            let res: Option<V> = match &ins.inst {
                Inst::Phi { .. } => continue,
                Inst::Br(t) => {
                    siguiente = Some(*t);
                    break;
                }
                Inst::CondBr { c, t, f: fl } => {
                    let c = get(*c).and_then(|x| x.int()).ok_or("condición desconocida")?;
                    siguiente = Some(if c != 0 { *t } else { *fl });
                    break;
                }
                Inst::Switch { v, default, cases } => {
                    let x = get(*v).and_then(|x| x.int()).ok_or("switch sobre un valor desconocido")?;
                    let dest = cases.iter().find(|(c, _)| get(*c).and_then(|y| y.int()) == Some(x)).map(|(_, b)| *b).unwrap_or(*default);
                    siguiente = Some(dest);
                    break;
                }
                Inst::Ret(v) => return Ok(Parada::Fin(v.and_then(get))),
                Inst::Unreachable => return Err("unreachable".into()),
                Inst::Load { ptr, .. } => Some(mem.cargar(m, &get(*ptr).ok_or("puntero desconocido")?, ins.ty)?),
                Inst::Store { ptr, val, .. } => {
                    let vt = f.values[*val].ty;
                    mem.guardar(m, &get(*ptr).ok_or("puntero desconocido")?, vt, &get(*val).unwrap_or(V::Undef))?;
                    None
                }
                Inst::Alloca { ty, n, .. } => {
                    let k = get(*n).and_then(|x| x.int()).unwrap_or(1) as usize;
                    mem.allocas.push(vec![0u8; layout::size(m, *ty) as usize * k.max(1)]);
                    Some(V::Ptr(Mem::Alloca(mem.allocas.len() - 1), 0))
                }
                Inst::Gep { src_ty, ptr, idx, .. } => {
                    let base = get(*ptr).ok_or("gep sobre un puntero desconocido")?;
                    let ix: Vec<V> = idx.iter().map(|&i| get(i).ok_or_else(|| "índice desconocido".to_string())).collect::<Res<_>>()?;
                    Some(gep(m, *src_ty, &base, &ix).ok_or("gep no soportado")?)
                }
                Inst::Call { callee, args: ca, .. } => {
                    let av: Vec<V> = ca.iter().map(|&a| get(a).unwrap_or(V::Undef)).collect();
                    match get(*callee) {
                        Some(V::Ptr(Mem::Function(g), _)) => {
                            let gf = &m.functions[g];
                            if gf.name.starts_with("llvm.lifetime") || gf.name.starts_with("llvm.dbg") || gf.name == "llvm.assume" || gf.name.starts_with("llvm.experimental.noalias") {
                                None
                            } else if gf.name.starts_with("llvm.memset") || gf.name.starts_with("llvm.memcpy") || gf.name.starts_with("llvm.memmove") {
                                memoria_llvm(mem, &gf.name, &av)?;
                                None
                            } else if gf.name.starts_with("air.atomic.") {
                                atomica_cpu(m, mem, &gf.name, &av, ins.ty)?
                            } else if gf.name == "air.wg.barrier" || gf.name == "air.simdgroup.barrier" {
                                return Ok(Parada::Barrera);
                            } else if gf.name.starts_with("air.simd_") || gf.name.starts_with("air.quad_") {
                                h.pendiente = ins.val;
                                return Ok(Parada::Colectiva(gf.name.clone(), av));
                            } else {
                                ejecutar(m, g, &av, mem, externas, pasos)?
                            }
                        }
                        _ => return Err("llamada indirecta".into()),
                    }
                }
                Inst::Fence => None,
                other => Some(plegar(m, f, other, ins.ty, &get).ok_or_else(|| format!("no se puede ejecutar {other:?}"))?),
            };
            if let (Some(k), Some(v)) = (ins.val, res) {
                h.reg[k] = Some(v);
            }
        }
        h.prev = h.bb;
        h.bb = siguiente.ok_or("bloque sin terminador")?;
        h.pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half() {
        for x in [0.0f32, 1.0, -2.5, 65504.0, 0.000061035156, 1.0 / 3.0] {
            let h = f32_a_f16(x);
            assert!((f16_a_f32(h) - x).abs() <= x.abs() / 1024.0, "{x}");
        }
        assert_eq!(f32_a_f16(1.0), 0x3c00);
        assert_eq!(f32_a_f16(-2.0), 0xc000);
        assert_eq!(f16_a_f32(0x7bff), 65504.0);
    }

    #[test]
    fn enteros() {
        assert_eq!(bin(BinOp::Add, &V::Int(8, 250), &V::Int(8, 10)), Some(V::Int(8, 4)));
        assert_eq!(bin(BinOp::AShr, &V::Int(8, 0x80), &V::Int(8, 1)), Some(V::Int(8, 0xc0)));
        assert_eq!(cmp(40, &V::Int(8, 0xff), &V::Int(8, 1)), Some(V::Int(1, 1))); // -1 < 1 con signo
        assert_eq!(cmp(36, &V::Int(8, 0xff), &V::Int(8, 1)), Some(V::Int(1, 0)));
    }
}

// ---------------------------------------------------------------- funciones air.* en la CPU (referencia)

fn mapa1(v: &V, f: &impl Fn(f64) -> f64) -> Option<V> {
    Some(match v {
        V::Real(w, x) => V::Real(*w, redondear(*w, f(*x))),
        V::Vec(xs) => V::Vec(xs.iter().map(|x| mapa1(x, f)).collect::<Option<_>>()?),
        _ => return None,
    })
}

fn mapa_n(args: &[V], f: &impl Fn(&[f64]) -> f64) -> Option<V> {
    match &args[0] {
        V::Real(w, _) => {
            let xs: Vec<f64> = args.iter().map(|a| a.real()).collect::<Option<_>>()?;
            Some(V::Real(*w, redondear(*w, f(&xs))))
        }
        V::Vec(e0) => {
            let n = e0.len();
            let mut out = Vec::with_capacity(n);
            for k in 0..n {
                let comp: Vec<V> = args.iter().map(|a| match a { V::Vec(e) => e.get(k).cloned(), s => Some(s.clone()) }).collect::<Option<_>>()?;
                out.push(mapa_n(&comp, f)?);
            }
            Some(V::Vec(out))
        }
        _ => None,
    }
}

fn enteros_n(args: &[V], f: &impl Fn(&[(u32, u64)]) -> u64) -> Option<V> {
    match &args[0] {
        V::Int(w, _) => {
            let xs: Vec<(u32, u64)> = args.iter().map(|a| match a { V::Int(w, x) => Some((*w, *x)), _ => None }).collect::<Option<_>>()?;
            Some(V::Int(*w, f(&xs) & mask(*w)))
        }
        V::Vec(e0) => {
            let n = e0.len();
            let mut out = Vec::with_capacity(n);
            for k in 0..n {
                let comp: Vec<V> = args.iter().map(|a| match a { V::Vec(e) => e.get(k).cloned(), s => Some(s.clone()) }).collect::<Option<_>>()?;
                out.push(enteros_n(&comp, f)?);
            }
            Some(V::Vec(out))
        }
        _ => None,
    }
}

/// Funciones matemáticas y de conversión de AIR. `ty` es el tipo del resultado (para convert).
pub fn air_math(m: &Module, nombre: &str, args: &[V], ty: TypeId) -> Option<V> {
    let resto = nombre.strip_prefix("air.")?;
    let p: Vec<&str> = resto.split('.').collect();
    let base = p[0].strip_prefix("fast_").unwrap_or(p[0]);
    let signo = p.get(1) == Some(&"s");
    Some(match base {
        "convert" => {
            let (a, de) = (p.get(1).copied()?, p.get(3).copied()?);
            let op = match (a, de) {
                ("f", "f") => CastOp::FPExt,
                ("f", "u") => CastOp::UIToFP,
                ("f", "s") => CastOp::SIToFP,
                ("u", "f") => CastOp::FPToUI,
                ("s", "f") => CastOp::FPToSI,
                (_, "u") => CastOp::ZExt,
                (_, "s") => CastOp::SExt,
                _ => return None,
            };
            // del mismo ancho o a menos bits: recortar
            let op = match (op, &args[0]) {
                (CastOp::ZExt | CastOp::SExt, V::Int(w, _)) if int_bits(m, ty)? <= *w => CastOp::Trunc,
                (CastOp::ZExt | CastOp::SExt, V::Vec(e)) if matches!(e.first(), Some(V::Int(w, _)) if int_bits(m, ty)? <= *w) => CastOp::Trunc,
                (o, _) => o,
            };
            return cast(m, op, &args[0], ty);
        }
        "dot" => {
            let (V::Vec(a), V::Vec(b)) = (&args[0], &args[1]) else { return None };
            let w = match m.types[ty] { Type::Half => 16, Type::Double => 64, _ => 32 };
            let s: f64 = a.iter().zip(b).map(|(x, y)| x.real().unwrap_or(0.0) * y.real().unwrap_or(0.0)).sum();
            V::Real(w, redondear(w, s))
        }
        "saturate" => mapa1(&args[0], &|x| x.clamp(0.0, 1.0))?,
        "fabs" => mapa1(&args[0], &f64::abs)?,
        "floor" => mapa1(&args[0], &f64::floor)?,
        "ceil" => mapa1(&args[0], &f64::ceil)?,
        "trunc" => mapa1(&args[0], &f64::trunc)?,
        "fract" => mapa1(&args[0], &|x| x - x.floor())?,
        "sqrt" => mapa1(&args[0], &f64::sqrt)?,
        "rsqrt" => mapa1(&args[0], &|x| 1.0 / x.sqrt())?,
        "exp" => mapa1(&args[0], &f64::exp)?,
        "exp2" => mapa1(&args[0], &f64::exp2)?,
        "log" => mapa1(&args[0], &f64::ln)?,
        "log2" => mapa1(&args[0], &f64::log2)?,
        "sin" => mapa1(&args[0], &f64::sin)?,
        "cos" => mapa1(&args[0], &f64::cos)?,
        "sign" => mapa1(&args[0], &|x| if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 })?,
        "tan" => mapa1(&args[0], &f64::tan)?,
        "asin" => mapa1(&args[0], &f64::asin)?,
        "acos" => mapa1(&args[0], &f64::acos)?,
        "atan" => mapa1(&args[0], &f64::atan)?,
        "sinh" => mapa1(&args[0], &f64::sinh)?,
        "cosh" => mapa1(&args[0], &f64::cosh)?,
        "tanh" => mapa1(&args[0], &f64::tanh)?,
        "round" => mapa1(&args[0], &f64::round)?,
        "rint" => mapa1(&args[0], &|x| { let r = x.round(); if (x - x.trunc()).abs() == 0.5 { 2.0 * (x / 2.0).round() } else { r } })?,
        "step" => mapa_n(args, &|a| if a[1] < a[0] { 0.0 } else { 1.0 })?,
        "smoothstep" => mapa_n(args, &|a| { let t = ((a[2] - a[0]) / (a[1] - a[0])).clamp(0.0, 1.0); t * t * (3.0 - 2.0 * t) })?,
        "is_uniform" => args[0].clone(),
        "any" | "all" => {
            let xs: Vec<u64> = match &args[0] { V::Vec(e) => e.iter().map(|x| x.int().unwrap_or(0)).collect(), x => vec![x.int().unwrap_or(0)] };
            let r = if base == "any" { xs.iter().any(|&x| x != 0) } else { xs.iter().all(|&x| x != 0) };
            match m.types[ty] { Type::Int(w) => V::Int(w, r as u64), _ => V::Int(1, r as u64) }
        }
        "length" => {
            let xs: Vec<f64> = match &args[0] { V::Vec(e) => e.iter().map(|x| x.real().unwrap_or(0.0)).collect(), x => vec![x.real()?] };
            let w = match m.types[ty] { Type::Half => 16, _ => 32 };
            V::Real(w, redondear(w, xs.iter().map(|x| x * x).sum::<f64>().sqrt()))
        }
        "fmax" => mapa_n(args, &|a| a[0].max(a[1]))?,
        "fmin" => mapa_n(args, &|a| a[0].min(a[1]))?,
        "fmax3" => mapa_n(args, &|a| a[0].max(a[1]).max(a[2]))?,
        "fmin3" => mapa_n(args, &|a| a[0].min(a[1]).min(a[2]))?,
        "pow" | "powr" => mapa_n(args, &|a| a[0].powf(a[1]))?,
        "atan2" => mapa_n(args, &|a| a[0].atan2(a[1]))?,
        "fma" => mapa_n(args, &|a| a[0] * a[1] + a[2])?,
        "mix" => mapa_n(args, &|a| a[0] + (a[1] - a[0]) * a[2])?,
        "clamp" if matches!(args[0], V::Real(..)) || matches!(&args[0], V::Vec(e) if matches!(e.first(), Some(V::Real(..)))) => {
            mapa_n(args, &|a| a[0].max(a[1]).min(a[2]))?
        }
        "clamp" => enteros_n(args, &|a| {
            let (w, x) = a[0];
            if signo {
                sext(w, x).max(sext(w, a[1].1)).min(sext(w, a[2].1)) as u64
            } else {
                x.max(a[1].1).min(a[2].1)
            }
        })?,
        "min" => enteros_n(args, &|a| if signo { sext(a[0].0, a[0].1).min(sext(a[0].0, a[1].1)) as u64 } else { a[0].1.min(a[1].1) })?,
        "max" => enteros_n(args, &|a| if signo { sext(a[0].0, a[0].1).max(sext(a[0].0, a[1].1)) as u64 } else { a[0].1.max(a[1].1) })?,
        _ => return None,
    })
}

// ---------------------------------------------------------------- funciones de SIMD-group en la CPU

fn elem2(a: &V, b: &V, f: &dyn Fn(&V, &V) -> Option<V>) -> Option<V> {
    match (a, b) {
        (V::Vec(x), V::Vec(y)) => Some(V::Vec(x.iter().zip(y).map(|(p, q)| elem2(p, q, f)).collect::<Option<_>>()?)),
        _ => f(a, b),
    }
}

fn red(op: &str, signo: bool, a: &V, b: &V) -> Option<V> {
    elem2(a, b, &|x, y| match (x, y) {
        (V::Real(w, p), V::Real(_, q)) => Some(V::Real(*w, redondear(*w, match op {
            "sum" => p + q,
            "product" => p * q,
            "max" => p.max(*q),
            "min" => p.min(*q),
            _ => return None,
        }))),
        (V::Int(w, p), V::Int(_, q)) => Some(V::Int(*w, mask(*w) & match op {
            "sum" => p.wrapping_add(*q),
            "product" => p.wrapping_mul(*q),
            "max" if signo => sext(*w, *p).max(sext(*w, *q)) as u64,
            "min" if signo => sext(*w, *p).min(sext(*w, *q)) as u64,
            "max" => *p.max(q),
            "min" => *p.min(q),
            "and" | "all" => p & q,
            "or" | "any" => p | q,
            "xor" => p ^ q,
            _ => return None,
        })),
        _ => None,
    })
}

/// Calcula una función colectiva para los hilos de un SIMD-group (32 carriles). `args[c]`: los argumentos del carril
/// `c` si está en la llamada (activo). Devuelve el resultado de cada carril activo.
pub fn simd_cpu(nombre: &str, args: &[Option<Vec<V>>]) -> Result<Vec<Option<V>>, String> {
    let resto = nombre.strip_prefix("air.").unwrap_or(nombre);
    let partes: Vec<&str> = resto.split('.').collect();
    let base = partes[0];
    let quad = base.starts_with("quad_");
    let op = base.split_once('_').map(|x| x.1).unwrap_or("");
    let signo = partes.get(1) == Some(&"s");
    let ancho = if quad { 4 } else { 32 };
    let n = args.len();
    let val = |c: usize| args.get(c).and_then(|a| a.as_ref()).and_then(|a| a.first().cloned());
    let arg = |c: usize, k: usize| args.get(c).and_then(|a| a.as_ref()).and_then(|a| a.get(k).cloned());
    let mut out = vec![None; n];
    for c in 0..n {
        if args[c].is_none() {
            continue;
        }
        let g0 = c / ancho * ancho;
        let activos: Vec<usize> = (g0..(g0 + ancho).min(n)).filter(|&k| args[k].is_some()).collect();
        let r = match op {
            "sum" | "product" | "max" | "min" | "and" | "or" | "xor" | "all" | "any" => {
                let mut acc = val(activos[0]).ok_or("sin valor")?;
                for &k in &activos[1..] {
                    acc = red(op, signo, &acc, &val(k).ok_or("sin valor")?).ok_or_else(|| format!("{nombre}: tipos"))?;
                }
                acc
            }
            "prefix_inclusive_sum" | "prefix_exclusive_sum" | "prefix_inclusive_product" | "prefix_exclusive_product" => {
                let o = if op.ends_with("sum") { "sum" } else { "product" };
                let incl = op.contains("inclusive");
                let previos: Vec<usize> = activos.iter().copied().filter(|&k| if incl { k <= c } else { k < c }).collect();
                let v0 = val(c).ok_or("sin valor")?;
                let neutro = match &v0 {
                    V::Real(w, _) => V::Real(*w, if o == "sum" { 0.0 } else { 1.0 }),
                    V::Int(w, _) => V::Int(*w, (o != "sum") as u64),
                    V::Vec(e) => V::Vec(e.iter().map(|x| match x { V::Real(w, _) => V::Real(*w, if o == "sum" { 0.0 } else { 1.0 }), V::Int(w, _) => V::Int(*w, (o != "sum") as u64), _ => V::Undef }).collect()),
                    _ => V::Undef,
                };
                let mut acc = neutro;
                for k in previos {
                    acc = red(o, signo, &acc, &val(k).ok_or("sin valor")?).ok_or_else(|| format!("{nombre}: tipos"))?;
                }
                acc
            }
            "shuffle" | "broadcast" | "shuffle_xor" | "shuffle_down" | "shuffle_up" | "shuffle_rotate_down" | "shuffle_rotate_up" => {
                let d = arg(c, 1).and_then(|x| x.int()).ok_or("índice desconocido")? as i64;
                let local = (c - g0) as i64;
                let (orig, valido) = match op {
                    "shuffle" | "broadcast" => (d & (ancho as i64 - 1), true),
                    "shuffle_xor" => (local ^ d, (local ^ d) < ancho as i64),
                    "shuffle_down" => (local + d, local + d < ancho as i64),
                    "shuffle_up" => (local - d, local >= d),
                    "shuffle_rotate_down" => ((local + d).rem_euclid(ancho as i64), true),
                    _ => ((local - d).rem_euclid(ancho as i64), true),
                };
                if valido { val(g0 + orig as usize).unwrap_or(V::Undef) } else { val(c).unwrap_or(V::Undef) }
            }
            "is_first" => V::Int(1, (activos.first() == Some(&c)) as u64),
            "ballot" | "active_threads_mask" => {
                let mut m = 0u64;
                for &k in &activos {
                    if op == "active_threads_mask" || arg(k, 0).and_then(|x| x.int()).unwrap_or(0) != 0 {
                        m |= 1 << (k - g0);
                    }
                }
                V::Int(32, m)
            }
            _ => return Err(format!("{nombre} sin referencia en la CPU")),
        };
        out[c] = Some(r);
    }
    Ok(out)
}

/// llvm.memset / llvm.memcpy / llvm.memmove en el intérprete.
fn memoria_llvm(mem: &mut Memoria, nombre: &str, a: &[V]) -> Res<()> {
    let n = a.get(2).and_then(|x| x.int()).ok_or("memset/memcpy de tamaño desconocido")? as usize;
    let V::Ptr(rd, od) = a[0] else { return Err("memset/memcpy sin destino".into()) };
    let datos: Vec<u8> = if nombre.starts_with("llvm.memset") {
        vec![a[1].int().unwrap_or(0) as u8; n]
    } else {
        let V::Ptr(ro, oo) = a[1] else { return Err("memcpy sin origen".into()) };
        let b = mem.region(ro).ok_or("memcpy desde una región desconocida")?;
        let mut v = vec![0u8; n];
        if let Some(s) = b.get(oo as usize..oo as usize + n) {
            v.copy_from_slice(s);
        }
        v
    };
    let b = mem.region(rd).ok_or("memset/memcpy a una región desconocida")?;
    if let Some(s) = b.get_mut(od as usize..od as usize + n) {
        s.copy_from_slice(&datos);
    }
    Ok(())
}

/// Intrínsecas llvm.* en la CPU (referencia).
pub fn llvm_math(nombre: &str, args: &[V]) -> Option<V> {
    let resto = nombre.strip_prefix("llvm.")?;
    let base = resto.split('.').next()?;
    let ent = |f: &dyn Fn(u32, u64, u64) -> u64| enteros_n(args, &|a| f(a[0].0, a[0].1, a.get(1).map(|x| x.1).unwrap_or(0)));
    Some(match base {
        "smax" => ent(&|w, x, y| sext(w, x).max(sext(w, y)) as u64)?,
        "smin" => ent(&|w, x, y| sext(w, x).min(sext(w, y)) as u64)?,
        "umax" => ent(&|_, x, y| x.max(y))?,
        "umin" => ent(&|_, x, y| x.min(y))?,
        "abs" if matches!(args[0], V::Int(..)) || matches!(&args[0], V::Vec(e) if matches!(e.first(), Some(V::Int(..)))) => {
            enteros_n(&args[..1], &|a| sext(a[0].0, a[0].1).unsigned_abs())?
        }
        "ctpop" => enteros_n(&args[..1], &|a| a[0].1.count_ones() as u64)?,
        "cttz" => enteros_n(&args[..1], &|a| if a[0].1 == 0 { a[0].0 as u64 } else { a[0].1.trailing_zeros() as u64 })?,
        "ctlz" => enteros_n(&args[..1], &|a| if a[0].1 == 0 { a[0].0 as u64 } else { (a[0].1.leading_zeros() - (64 - a[0].0)) as u64 })?,
        "bitreverse" => enteros_n(&args[..1], &|a| a[0].1.reverse_bits() >> (64 - a[0].0))?,
        "bswap" => enteros_n(&args[..1], &|a| a[0].1.swap_bytes() >> (64 - a[0].0))?,
        "fabs" => mapa1(&args[0], &f64::abs)?,
        "floor" => mapa1(&args[0], &f64::floor)?,
        "ceil" => mapa1(&args[0], &f64::ceil)?,
        "trunc" => mapa1(&args[0], &f64::trunc)?,
        "rint" | "nearbyint" | "roundeven" => mapa1(&args[0], &|x| { let r = x.round(); if (x - x.trunc()).abs() == 0.5 { 2.0 * (x / 2.0).round() } else { r } })?,
        "round" => mapa1(&args[0], &f64::round)?,
        "sqrt" => mapa1(&args[0], &f64::sqrt)?,
        "exp2" => mapa1(&args[0], &f64::exp2)?,
        "log2" => mapa1(&args[0], &f64::log2)?,
        "exp" => mapa1(&args[0], &f64::exp)?,
        "log" => mapa1(&args[0], &f64::ln)?,
        "sin" => mapa1(&args[0], &f64::sin)?,
        "cos" => mapa1(&args[0], &f64::cos)?,
        "pow" => mapa_n(args, &|a| a[0].powf(a[1]))?,
        "minnum" | "minimum" => mapa_n(args, &|a| a[0].min(a[1]))?,
        "maxnum" | "maximum" => mapa_n(args, &|a| a[0].max(a[1]))?,
        "fma" | "fmuladd" => mapa_n(args, &|a| a[0] * a[1] + a[2])?,
        "copysign" => mapa_n(args, &|a| a[0].abs().copysign(a[1]))?,
        _ => return None,
    })
}

/// Atómicas de AIR en el intérprete (los hilos van de uno en uno: basta con leer y escribir).
fn atomica_cpu(m: &Module, mem: &mut Memoria, nombre: &str, a: &[V], ty: TypeId) -> Res<Option<V>> {
    let p: Vec<&str> = nombre.split('.').collect();
    let op = p.get(3).copied().unwrap_or("");
    if p.get(2) == Some(&"fence") {
        return Ok(None);
    }
    let signo = p.get(4) == Some(&"s");
    let real = p.last().is_some_and(|t| t.starts_with('f'));
    let t32 = m.types.iter().position(|t| *t == Type::Int(32)).ok_or("sin tipo i32")?;
    let viejo = mem.cargar(m, &a[0], t32)?;
    let V::Int(_, x) = viejo else { return Err("atómica sobre un valor desconocido".into()) };
    let bits = |v: &V| -> u64 { match v { V::Real(_, f) => (*f as f32).to_bits() as u64, o => o.int().unwrap_or(0) & 0xffff_ffff } };
    let como = |x: u64| if real { V::Real(32, f32::from_bits(x as u32) as f64) } else { V::Int(32, x) };
    let (nuevo, res) = match op {
        "load" => (None, Some(como(x))),
        "store" => (Some(bits(&a[1])), None),
        "cmpxchg" => {
            let esperado = mem.cargar(m, &a[1], t32)?.int().unwrap_or(0);
            let ok = x == esperado;
            mem.guardar(m, &a[1], t32, &V::Int(32, x))?;
            let r = match m.types[ty] { Type::Int(w) => V::Int(w, ok as u64), _ => V::Int(1, ok as u64) };
            (if ok { Some(bits(&a[2])) } else { None }, Some(r))
        }
        _ => {
            let y = bits(&a[1]);
            let n = if real {
                let (fx, fy) = (f32::from_bits(x as u32), f32::from_bits(y as u32));
                (match op { "add" => fx + fy, "sub" => fx - fy, "xchg" => fy, _ => return Err(format!("{nombre} sin referencia")) }).to_bits() as u64
            } else {
                let (sx, sy) = (x as u32 as i32, y as u32 as i32);
                match op {
                    "add" => x.wrapping_add(y),
                    "sub" => x.wrapping_sub(y),
                    "max" if signo => sx.max(sy) as u32 as u64,
                    "min" if signo => sx.min(sy) as u32 as u64,
                    "max" => x.max(y),
                    "min" => x.min(y),
                    "and" => x & y,
                    "or" => x | y,
                    "xor" => x ^ y,
                    "xchg" => y,
                    _ => return Err(format!("{nombre} sin referencia")),
                }
            };
            (Some(n & 0xffff_ffff), Some(como(x)))
        }
    };
    if let Some(n) = nuevo {
        mem.guardar(m, &a[0], t32, &V::Int(32, n))?;
    }
    Ok(res)
}
