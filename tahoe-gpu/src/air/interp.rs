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

fn redondear(bits: u32, x: f64) -> f64 {
    match bits {
        16 => f16_a_f32(f32_a_f16(x as f32)) as f64,
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
                BinOp::UDiv => x.checked_div(y)?,
                BinOp::SDiv => sx.checked_div(sy)? as u64,
                BinOp::URem => x.checked_rem(y)?,
                BinOp::SRem => sx.checked_rem(sy)? as u64,
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
        let s = b.get(*o as usize..*o as usize + n).ok_or_else(|| format!("load fuera de {r:?} ({o}+{n})"))?;
        de_bytes(m, t, s).ok_or_else(|| "load de un tipo no soportado".into())
    }
    pub fn guardar(&mut self, m: &Module, p: &V, t: TypeId, v: &V) -> Res<()> {
        let V::Ptr(r, o) = p else { return Err(format!("store en algo que no es un puntero: {p:?}")) };
        let n = layout::size(m, t) as usize;
        let b = self.region(*r).ok_or_else(|| format!("store en {r:?}"))?;
        let s = b.get_mut(*o as usize..*o as usize + n).ok_or_else(|| format!("store fuera de {r:?}"))?;
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
    let mut reg: Vec<Option<V>> = vec![None; f.values.len()];
    for (k, &a) in f.args.iter().enumerate() {
        reg[a] = args.get(k).cloned();
    }
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
    let (mut bb, mut prev) = (0usize, usize::MAX);
    loop {
        let blk = f.blocks.get(bb).ok_or("salto a un bloque inexistente")?;
        // phis: todos a la vez con los valores del bloque anterior
        let mut nuevos = Vec::new();
        for ins in &blk.insts {
            if let Inst::Phi { incoming } = &ins.inst {
                let (v, _) = incoming.iter().find(|(_, b)| *b == prev).ok_or("phi sin entrada para el bloque anterior")?;
                nuevos.push((ins.val.unwrap(), valor(&reg, *v).unwrap_or(V::Undef)));
            }
        }
        for (k, v) in nuevos {
            reg[k] = Some(v);
        }
        let mut siguiente = None;
        for ins in &blk.insts {
            *pasos += 1;
            if *pasos > 50_000_000 {
                return Err("demasiados pasos (¿bucle infinito?)".into());
            }
            let get = |v: ValueId| valor(&reg, v);
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
                Inst::Ret(v) => return Ok(v.and_then(get)),
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
                            if gf.name.starts_with("llvm.lifetime") || gf.name.starts_with("llvm.dbg") {
                                None
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
                reg[k] = Some(v);
            }
        }
        prev = bb;
        bb = siguiente.ok_or("bloque sin terminador")?;
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
