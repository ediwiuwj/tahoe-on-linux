//! Instrucciones del IR y funciones `air.*`/`llvm.*` en SPIR-V.

use super::*;

/// Componentes de un tipo (1 para escalares).
fn comps(m: &Module, t: TypeId) -> u32 {
    match m.types[t] {
        Type::Vector(n, _) => n,
        _ => 1,
    }
}

fn elem(m: &Module, t: TypeId) -> TypeId {
    match m.types[t] {
        Type::Vector(_, e) => e,
        _ => t,
    }
}

fn bits(m: &Module, t: TypeId) -> u32 {
    match m.types[elem(m, t)] {
        Type::Int(w) => w,
        Type::Half => 16,
        Type::BFloat => super::interp::BF16,
        Type::Float => 32,
        Type::Double => 64,
        _ => 0,
    }
}

fn es_real(m: &Module, t: TypeId) -> bool {
    matches!(m.types[elem(m, t)], Type::Half | Type::BFloat | Type::Float | Type::Double)
}

impl Tr<'_> {
    /// Constante escalar u32 repetida en un vector de `n` componentes.
    fn cu_n(&mut self, v: u32, n: u32) -> Word {
        let c = self.cu(v);
        if n == 1 {
            return c;
        }
        let t = self.vec_t(self.t_u32, n);
        self.b.constant_composite(t, vec![c; n as usize])
    }
    fn cf_n(&mut self, v: f32, n: u32) -> Word {
        let c = self.cf(v);
        if n == 1 {
            return c;
        }
        let t = self.vec_t(self.t_f32, n);
        self.b.constant_composite(t, vec![c; n as usize])
    }

    /// Recorta un entero de `w` bits (guardado en u32) tras una operación.
    fn recortar(&mut self, x: Word, w: u32, t: TypeId) -> Word {
        if w >= 32 {
            return x;
        }
        let n = comps(self.m, t);
        let mk = self.cu_n(((1u64 << w) - 1) as u32, n);
        let st = self.vec_t(self.t_u32, n);
        self.b.bitwise_and(st, None, x, mk).unwrap()
    }

    /// Extiende el signo de un entero de `w` bits a los 32 del u32.
    fn ext_signo(&mut self, x: Word, w: u32, t: TypeId) -> Word {
        if w >= 32 {
            return x;
        }
        let n = comps(self.m, t);
        let s = self.cu_n(32 - w, n);
        let st = self.vec_t(self.t_u32, n);
        let a = self.b.shift_left_logical(st, None, x, s).unwrap();
        self.b.shift_right_arithmetic(st, None, a, s).unwrap()
    }

    pub(super) fn instruccion(&mut self, i: &Inst, ty: TypeId) -> Res<Option<TV>> {
        let m = self.m;
        Ok(Some(match i {
            Inst::Bin { op, a, b } => {
                let t = self.f.values[*a].ty;
                let st = self.ty(t)?;
                let (x, y) = (self.id(*a)?, self.id(*b)?);
                if es_real(m, t) {
                    let r = match op {
                        BinOp::Add => self.b.f_add(st, None, x, y),
                        BinOp::Sub => self.b.f_sub(st, None, x, y),
                        BinOp::Mul => self.b.f_mul(st, None, x, y),
                        BinOp::SDiv | BinOp::UDiv => self.b.f_div(st, None, x, y),
                        BinOp::SRem | BinOp::URem => self.b.f_rem(st, None, x, y),
                        o => return Err(format!("{o:?} real")),
                    }
                    .unwrap();
                    let r = if bits(m, t) == 16 { self.b.quantize_to_f16(st, None, r).unwrap() } else { r };
                    TV::Id(r)
                } else if bits(m, t) == 1 {
                    TV::Id(match op {
                        BinOp::And | BinOp::Mul => self.b.logical_and(st, None, x, y),
                        BinOp::Or => self.b.logical_or(st, None, x, y),
                        BinOp::Xor | BinOp::Add | BinOp::Sub => self.b.logical_not_equal(st, None, x, y),
                        o => return Err(format!("{o:?} de bool")),
                    }
                    .unwrap())
                } else {
                    let w = bits(m, t);
                    let r = match op {
                        BinOp::Add => self.b.i_add(st, None, x, y),
                        BinOp::Sub => self.b.i_sub(st, None, x, y),
                        BinOp::Mul => self.b.i_mul(st, None, x, y),
                        BinOp::UDiv => self.b.u_div(st, None, x, y),
                        BinOp::URem => self.b.u_mod(st, None, x, y),
                        BinOp::SDiv | BinOp::SRem => {
                            let (sx, sy) = (self.ext_signo(x, w, t), self.ext_signo(y, w, t));
                            if matches!(op, BinOp::SDiv) { self.b.s_div(st, None, sx, sy) } else { self.b.s_rem(st, None, sx, sy) }
                        }
                        BinOp::Shl => self.b.shift_left_logical(st, None, x, y),
                        BinOp::LShr => self.b.shift_right_logical(st, None, x, y),
                        BinOp::AShr => {
                            let sx = self.ext_signo(x, w, t);
                            self.b.shift_right_arithmetic(st, None, sx, y)
                        }
                        BinOp::And => self.b.bitwise_and(st, None, x, y),
                        BinOp::Or => self.b.bitwise_or(st, None, x, y),
                        BinOp::Xor => self.b.bitwise_xor(st, None, x, y),
                    }
                    .unwrap();
                    let necesita = !matches!(op, BinOp::And | BinOp::Or | BinOp::Xor | BinOp::UDiv | BinOp::URem | BinOp::LShr);
                    TV::Id(if necesita { self.recortar(r, w, t) } else { r })
                }
            }
            Inst::FNeg(a) => {
                let st = self.ty(ty)?;
                let x = self.id(*a)?;
                TV::Id(self.b.f_negate(st, None, x).unwrap())
            }
            Inst::Freeze(a) => self.tv(*a)?,
            Inst::Cmp { pred, a, b } => {
                let t = self.f.values[*a].ty;
                let n = comps(m, t);
                let bt = self.vec_t(self.t_bool, n);
                if espacio(m, t).is_some() {
                    // punteros: misma base y desplazamiento
                    let (pa, pb) = (self.tv(*a)?, self.tv(*b)?);
                    let igual = match (&pa, &pb) {
                        (TV::Ptr(ba, oa), TV::Ptr(bb, ob)) if ba == bb => {
                            let (x, y) = (self.off_id(*oa), self.off_id(*ob));
                            TV::Id(self.b.i_equal(self.t_bool, None, x, y).unwrap())
                        }
                        (TV::Tex(_) | TV::Ptr(..), TV::Ptr(Base::Nula, _)) | (TV::Ptr(Base::Nula, _), TV::Tex(_) | TV::Ptr(..)) => TV::C(V::Int(1, 0)),
                        _ => TV::C(V::Int(1, 0)),
                    };
                    return Ok(Some(match (pred, igual) {
                        (32, x) => x,
                        (33, TV::C(V::Int(_, k))) => TV::C(V::Int(1, 1 - k)),
                        (33, TV::Id(x)) => TV::Id(self.b.logical_not(self.t_bool, None, x).unwrap()),
                        _ => return Err("comparación de punteros".into()),
                    }));
                }
                let (x, y) = (self.id(*a)?, self.id(*b)?);
                let r = if es_real(m, t) {
                    match pred {
                        0 => return Ok(Some(TV::C(V::Int(1, 0)))),
                        15 => return Ok(Some(TV::C(V::Int(1, 1)))),
                        1 => self.b.f_ord_equal(bt, None, x, y),
                        2 => self.b.f_ord_greater_than(bt, None, x, y),
                        3 => self.b.f_ord_greater_than_equal(bt, None, x, y),
                        4 => self.b.f_ord_less_than(bt, None, x, y),
                        5 => self.b.f_ord_less_than_equal(bt, None, x, y),
                        6 => self.b.f_ord_not_equal(bt, None, x, y),
                        7 | 8 => {
                            let na = self.b.is_nan(bt, None, x).unwrap();
                            let nb = self.b.is_nan(bt, None, y).unwrap();
                            let alguno = self.b.logical_or(bt, None, na, nb).unwrap();
                            if *pred == 8 { Ok(alguno) } else { self.b.logical_not(bt, None, alguno) }
                        }
                        9 => self.b.f_unord_equal(bt, None, x, y),
                        10 => self.b.f_unord_greater_than(bt, None, x, y),
                        11 => self.b.f_unord_greater_than_equal(bt, None, x, y),
                        12 => self.b.f_unord_less_than(bt, None, x, y),
                        13 => self.b.f_unord_less_than_equal(bt, None, x, y),
                        14 => self.b.f_unord_not_equal(bt, None, x, y),
                        p => return Err(format!("fcmp {p}")),
                    }
                    .unwrap()
                } else if bits(m, t) == 1 {
                    match pred {
                        32 => self.b.logical_equal(bt, None, x, y),
                        33 => self.b.logical_not_equal(bt, None, x, y),
                        p => return Err(format!("icmp {p} de bool")),
                    }
                    .unwrap()
                } else {
                    let w = bits(m, t);
                    match pred {
                        32 => self.b.i_equal(bt, None, x, y),
                        33 => self.b.i_not_equal(bt, None, x, y),
                        34 => self.b.u_greater_than(bt, None, x, y),
                        35 => self.b.u_greater_than_equal(bt, None, x, y),
                        36 => self.b.u_less_than(bt, None, x, y),
                        37 => self.b.u_less_than_equal(bt, None, x, y),
                        38..=41 => {
                            let (sx, sy) = (self.ext_signo(x, w, t), self.ext_signo(y, w, t));
                            match pred {
                                38 => self.b.s_greater_than(bt, None, sx, sy),
                                39 => self.b.s_greater_than_equal(bt, None, sx, sy),
                                40 => self.b.s_less_than(bt, None, sx, sy),
                                _ => self.b.s_less_than_equal(bt, None, sx, sy),
                            }
                        }
                        p => return Err(format!("icmp {p}")),
                    }
                    .unwrap()
                };
                TV::Id(r)
            }
            Inst::Select { c, t, f } => {
                if espacio(m, ty).is_some() {
                    let (pt, pf) = (self.tv(*t)?, self.tv(*f)?);
                    // con undef en un lado vale el otro
                    match (&pt, &pf) {
                        (TV::C(V::Undef), x) | (x, TV::C(V::Undef)) => return Ok(Some(x.clone())),
                        _ => {}
                    }
                    let (TV::Ptr(bt, ot), TV::Ptr(bf, of)) = (pt.clone(), pf.clone()) else { return Err(format!("select de punteros sin forma conocida: {pt:?} / {pf:?}")) };
                    if bt != bf {
                        return Err("select entre punteros de bases distintas".into());
                    }
                    let cid = self.id(*c)?;
                    let (x, y) = (self.off_id(ot), self.off_id(of));
                    return Ok(Some(TV::Ptr(bt, Off::Id(self.b.select(self.t_u32, None, cid, x, y).unwrap()))));
                }
                let st = self.ty(ty)?;
                let mut cid = self.id(*c)?;
                let (nc, nv) = (comps(m, self.f.values[*c].ty), comps(m, ty));
                if nc == 1 && nv > 1 {
                    // SPIR-V 1.3: la condición de un select de vectores es otro vector
                    let bt = self.vec_t(self.t_bool, nv);
                    cid = self.b.composite_construct(bt, None, vec![cid; nv as usize]).unwrap();
                }
                let (x, y) = (self.id(*t)?, self.id(*f)?);
                TV::Id(self.b.select(st, None, cid, x, y).unwrap())
            }
            Inst::Cast { op, v } => return self.conversion(*op, *v, ty).map(Some),
            Inst::ExtractElt { v, i } => {
                let st = self.ty(ty)?;
                let x = self.id(*v)?;
                match self.tv(*i)? {
                    TV::C(V::Int(_, k)) => TV::Id(self.b.composite_extract(st, None, x, [k as u32]).unwrap()),
                    _ => {
                        let k = self.id(*i)?;
                        TV::Id(self.b.vector_extract_dynamic(st, None, x, k).unwrap())
                    }
                }
            }
            Inst::InsertElt { v, e, i } => {
                let st = self.ty(ty)?;
                let (x, y) = (self.id(*v)?, self.id(*e)?);
                match self.tv(*i)? {
                    TV::C(V::Int(_, k)) => TV::Id(self.b.composite_insert(st, None, y, x, [k as u32]).unwrap()),
                    _ => {
                        let k = self.id(*i)?;
                        TV::Id(self.b.vector_insert_dynamic(st, None, x, y, k).unwrap())
                    }
                }
            }
            Inst::Shuffle { a, b, mask } => {
                let st = self.ty(ty)?;
                let (x, y) = (self.id(*a)?, self.id(*b)?);
                let mk = match self.tv(*mask)? {
                    TV::C(V::Vec(es)) => es.iter().map(|e| e.int().map(|k| k as u32).unwrap_or(0xffff_ffff)).collect::<Vec<u32>>(),
                    TV::C(V::Undef) => vec![0xffff_ffff; comps(m, ty) as usize],
                    TV::C(V::Agg(_)) | TV::C(_) => vec![0; comps(m, ty) as usize],
                    _ => return Err("máscara de shufflevector no constante".into()),
                };
                TV::Id(self.b.vector_shuffle(st, None, x, y, mk).unwrap())
            }
            Inst::ExtractVal { v, idx } => {
                let st = self.ty(ty)?;
                let x = self.id(*v)?;
                TV::Id(self.b.composite_extract(st, None, x, idx.clone()).unwrap())
            }
            Inst::InsertVal { agg, v, idx } => {
                let st = self.ty(ty)?;
                let base = match self.tv(*agg)? {
                    TV::C(V::Undef) => self.b.constant_null(st),
                    other => self.id_de(&other, ty)?,
                };
                let x = self.id(*v)?;
                TV::Id(self.b.composite_insert(st, None, x, base, idx.clone()).unwrap())
            }
            Inst::Load { ptr, .. } => {
                let (base, off) = match self.tv(*ptr)? {
                    TV::Ptr(b, o) => (b, o),
                    TV::C(V::Undef) => (Base::Nula, Off::C(0)), // buffer ausente en esta variante
                    _ => return Err("load de algo que no es memoria".into()),
                };
                TV::Id(self.cargar(base, off, ty)?)
            }
            Inst::Store { ptr, val, .. } => {
                let (base, off) = match self.tv(*ptr)? {
                    TV::Ptr(b, o) => (b, o),
                    TV::C(V::Undef) => return Ok(None),
                    _ => return Err("store en algo que no es memoria".into()),
                };
                let t = self.f.values[*val].ty;
                let x = self.id(*val)?;
                self.guardar(base, off, t, x)?;
                return Ok(None);
            }
            Inst::Alloca { .. } => return Err("alloca fuera de sitio".into()),
            Inst::Gep { src_ty, ptr, idx, .. } => {
                let p = self.tv(*ptr)?;
                match p {
                    TV::Ptr(base, off) => TV::Ptr(base, self.gep(*src_ty, off, idx)?),
                    TV::Tex(_) | TV::Smp(_) => p,
                    TV::C(V::Undef) => TV::Ptr(Base::Nula, Off::C(0)), // buffer ausente en esta variante
                    _ => return Err("gep sobre algo que no es memoria".into()),
                }
            }
            Inst::Call { callee, args, .. } => return self.llamada(*callee, args, ty),
            Inst::Fence => return Ok(None),
            Inst::AtomicRmw { op, ptr, val } => {
                let TV::Ptr(base, off) = self.tv(*ptr)? else { return Err("atómico fuera de memoria".into()) };
                let (p, sc) = self.palabra(base, off)?;
                let x = self.id(*val)?;
                let scope = self.cu(if sc == StorageClass::Workgroup { Scope::Workgroup as u32 } else { Scope::Device as u32 });
                let sem = self.cu(0);
                let u = self.t_u32;
                TV::Id(match op {
                    0 => self.b.atomic_exchange(u, None, p, scope, sem, x),
                    1 => self.b.atomic_i_add(u, None, p, scope, sem, x),
                    2 => self.b.atomic_i_sub(u, None, p, scope, sem, x),
                    3 => self.b.atomic_and(u, None, p, scope, sem, x),
                    5 => self.b.atomic_or(u, None, p, scope, sem, x),
                    6 => self.b.atomic_xor(u, None, p, scope, sem, x),
                    7 => self.b.atomic_s_max(u, None, p, scope, sem, x),
                    8 => self.b.atomic_s_min(u, None, p, scope, sem, x),
                    9 => self.b.atomic_u_max(u, None, p, scope, sem, x),
                    10 => self.b.atomic_u_min(u, None, p, scope, sem, x),
                    o => return Err(format!("atomicrmw {o}")),
                }
                .unwrap())
            }
            other => return Err(format!("instrucción sin traducción: {other:?}")),
        }))
    }

    /// Desplazamiento de un getelementptr.
    fn gep(&mut self, src: TypeId, off: Off, idx: &[ValueId]) -> Res<Off> {
        let m = self.m;
        let mut o = off;
        let mut t = src;
        for (k, &i) in idx.iter().enumerate() {
            let (stride, siguiente, campo) = if k == 0 {
                (layout::size(m, t), t, None)
            } else {
                match &m.types[t] {
                    Type::Struct { elems, .. } => {
                        let n = match self.tv(i)? {
                            TV::C(V::Int(_, n)) => n as usize,
                            _ => return Err("índice de campo no constante".into()),
                        };
                        (0, elems[n], Some(layout::offsets(m, t)[n]))
                    }
                    Type::Array(_, e) => (layout::size(m, *e), *e, None),
                    Type::Vector(_, e) => (layout::elem_size(m, t), *e, None),
                    other => return Err(format!("gep en {other:?}")),
                }
            };
            if let Some(c) = campo {
                o = self.off_mas(o, c as i64);
            } else {
                match self.tv(i)? {
                    TV::C(V::Int(w, n)) => {
                        let n = if w < 64 { ((n << (64 - w)) as i64) >> (64 - w) } else { n as i64 };
                        o = self.off_mas(o, n * stride as i64);
                    }
                    _ => {
                        let x = self.id(i)?;
                        let s = self.cu(stride as u32);
                        let d = self.b.i_mul(self.t_u32, None, x, s).unwrap();
                        let base = self.off_id(o);
                        o = Off::Id(self.b.i_add(self.t_u32, None, base, d).unwrap());
                    }
                }
            }
            t = siguiente;
        }
        Ok(o)
    }

    fn conversion(&mut self, op: CastOp, v: ValueId, to: TypeId) -> Res<TV> {
        let m = self.m;
        let from = self.f.values[v].ty;
        if matches!(op, CastOp::BitCast | CastOp::AddrSpaceCast) && espacio(m, from).is_some() {
            return self.tv(v);
        }
        if op == CastOp::PtrToInt || op == CastOp::IntToPtr {
            return Err("ptrtoint/inttoptr sin soporte".into());
        }
        let st = self.ty(to)?;
        let x = self.id(v)?;
        let (wf, wt) = (bits(m, from), bits(m, to));
        let n = comps(m, to);
        Ok(TV::Id(match op {
            CastOp::Trunc => {
                if wt == 1 {
                    let uno = self.cu_n(1, n);
                    let ut = self.vec_t(self.t_u32, n);
                    let a = self.b.bitwise_and(ut, None, x, uno).unwrap();
                    let cero = self.cu_n(0, n);
                    self.b.i_not_equal(st, None, a, cero).unwrap()
                } else {
                    self.recortar(x, wt, to)
                }
            }
            CastOp::ZExt | CastOp::SExt => {
                if wf == 1 {
                    let uno = self.cu_n(if op == CastOp::SExt { ((1u64 << wt.min(32)) - 1) as u32 } else { 1 }, n);
                    let cero = self.cu_n(0, n);
                    self.b.select(st, None, x, uno, cero).unwrap()
                } else if op == CastOp::ZExt {
                    x
                } else {
                    let s = self.ext_signo(x, wf, from);
                    self.recortar(s, wt, to)
                }
            }
            CastOp::FPToUI | CastOp::FPToSI if wt == 1 => {
                let ut = self.vec_t(self.t_u32, n);
                let r = self.b.convert_f_to_u(ut, None, x).unwrap();
                let cero = self.cu_n(0, n);
                self.b.i_not_equal(st, None, r, cero).unwrap()
            }
            CastOp::FPToUI => {
                let r = self.b.convert_f_to_u(st, None, x).unwrap();
                self.recortar(r, wt, to)
            }
            CastOp::FPToSI => {
                let ti = self.vec_t(self.t_i32, n);
                let r = self.b.convert_f_to_s(ti, None, x).unwrap();
                let r = self.b.bitcast(st, None, r).unwrap();
                self.recortar(r, wt, to)
            }
            CastOp::UIToFP | CastOp::SIToFP if wf == 1 => {
                let uno = self.cf_n(if op == CastOp::SIToFP { -1.0 } else { 1.0 }, n);
                let cero = self.cf_n(0.0, n);
                self.b.select(st, None, x, uno, cero).unwrap()
            }
            CastOp::UIToFP => {
                let r = self.b.convert_u_to_f(st, None, x).unwrap();
                if wt == 16 { self.b.quantize_to_f16(st, None, r).unwrap() } else { r }
            }
            CastOp::SIToFP => {
                let s = self.ext_signo(x, wf, from);
                let ti = self.vec_t(self.t_i32, n);
                let si = self.b.bitcast(ti, None, s).unwrap();
                let r = self.b.convert_s_to_f(st, None, si).unwrap();
                if wt == 16 { self.b.quantize_to_f16(st, None, r).unwrap() } else { r }
            }
            CastOp::FPTrunc => {
                if wt == 16 {
                    self.b.quantize_to_f16(st, None, x).unwrap()
                } else if wt == super::interp::BF16 {
                    self.a_bf16(x, n, st)
                } else {
                    x
                }
            }
            CastOp::FPExt => x,
            CastOp::BitCast => return self.reinterpretar(x, from, to),
            other => return Err(format!("conversión {other:?}")),
        }))
    }

    /// bitcast entre tipos del mismo tamaño: se pasa por una lista de trozos de bits.
    fn reinterpretar(&mut self, x: Word, from: TypeId, to: TypeId) -> Res<TV> {
        let m = self.m;
        let (nf, nt) = (comps(m, from), comps(m, to));
        let (wf, wt) = (bits(m, from), bits(m, to));
        let (rf, rt) = (es_real(m, from), es_real(m, to));
        // casos de 32 bits por componente: un OpBitcast
        if wf == 32 && wt == 32 && nf == nt {
            let st = self.ty(to)?;
            return Ok(TV::Id(if rf == rt { x } else { self.b.bitcast(st, None, x).unwrap() }));
        }
        // general: a palabras de 32 bits y de vuelta
        let ef = elem(m, from);
        let mut piezas: Vec<Word> = Vec::new(); // valores de wf bits (en u32)
        let ste = self.ty(ef)?;
        for k in 0..nf {
            let c = if nf == 1 { x } else { self.b.composite_extract(ste, None, x, [k]).unwrap() };
            let u = if wf == 16 && rf {
                let cero = self.cf(0.0);
                let v2 = self.vec_t(self.t_f32, 2);
                let par = self.b.composite_construct(v2, None, [c, cero]).unwrap();
                self.ext(self.t_u32, 58, &[par])
            } else if rf {
                self.b.bitcast(self.t_u32, None, c).unwrap()
            } else {
                c
            };
            piezas.push(u);
        }
        // a una secuencia de bits: palabras de 32
        let total = nf * wf;
        if total != nt * wt || total > 128 {
            return Err(format!("bitcast de {total} bits a {} bits", nt * wt));
        }
        let mut salida = Vec::new();
        let et = elem(m, to);
        for k in 0..nt {
            let bit0 = k * wt;
            // el valor de wt bits que empieza en bit0: sale de una o varias piezas
            let mut acc: Option<Word> = None;
            let mut puestos = 0;
            while puestos < wt {
                let pos = bit0 + puestos;
                let (pi, dentro) = ((pos / wf) as usize, pos % wf);
                let tomar = (wf - dentro).min(wt - puestos);
                let p = piezas[pi];
                let sh = self.cu(dentro);
                let a = self.b.shift_right_logical(self.t_u32, None, p, sh).unwrap();
                let mk = self.cu(if tomar >= 32 { u32::MAX } else { (1u32 << tomar) - 1 });
                let a = self.b.bitwise_and(self.t_u32, None, a, mk).unwrap();
                let a = if puestos > 0 {
                    let s = self.cu(puestos);
                    self.b.shift_left_logical(self.t_u32, None, a, s).unwrap()
                } else {
                    a
                };
                acc = Some(match acc {
                    None => a,
                    Some(prev) => self.b.bitwise_or(self.t_u32, None, prev, a).unwrap(),
                });
                puestos += tomar;
            }
            let u = acc.unwrap();
            let c = if wt == 16 && rt {
                let v2 = self.vec_t(self.t_f32, 2);
                let h = self.ext(v2, 62, &[u]);
                self.b.composite_extract(self.t_f32, None, h, [0]).unwrap()
            } else if rt {
                self.b.bitcast(self.t_f32, None, u).unwrap()
            } else {
                u
            };
            salida.push(c);
        }
        let _ = et;
        let st = self.ty(to)?;
        Ok(TV::Id(if nt == 1 { salida[0] } else { self.b.composite_construct(st, None, salida).unwrap() }))
    }

    // ------------------------------------------------------------ llamadas

    fn llamada(&mut self, callee: ValueId, args: &[ValueId], ty: TypeId) -> Res<Option<TV>> {
        let nombre = super::nombre_air(self.m, self.f, callee).to_string();
        if nombre.starts_with("llvm.lifetime") || nombre.starts_with("llvm.dbg") || nombre.starts_with("llvm.experimental.noalias") || nombre == "llvm.assume" {
            return Ok(None);
        }
        if nombre.starts_with("llvm.memcpy") || nombre.starts_with("llvm.memmove") {
            return self.memcpy(args).map(|_| None);
        }
        if nombre.starts_with("llvm.memset") {
            return self.memset(args).map(|_| None);
        }
        if let Some(resto) = nombre.strip_prefix("llvm.") {
            return self.intrinseca_llvm(resto, args, ty).map(|r| Some(TV::Id(r)));
        }
        let Some(resto) = nombre.strip_prefix("air.") else { return Err(format!("llamada a {nombre}")) };
        let partes: Vec<&str> = resto.split('.').collect();
        let base = partes[0].strip_prefix("fast_").unwrap_or(partes[0]);
        let signo = partes.get(1) == Some(&"s");
        let st = if matches!(self.m.types[ty], Type::Void) { self.t_void } else { self.ty(ty).unwrap_or(self.t_void) };
        let real = !matches!(self.m.types[ty], Type::Void | Type::Struct { .. }) && es_real(self.m, ty);
        let n = if matches!(self.m.types[ty], Type::Void | Type::Struct { .. }) { 1 } else { comps(self.m, ty) };
        let a: Vec<ValueId> = args.to_vec();
        // funciones de GLSL.std.450 con los mismos argumentos
        let glsl = |b: &str| -> Option<u32> {
            Some(match b {
                "fabs" => 4,
                "abs" => 5,
                "floor" => 8,
                "ceil" => 9,
                "fract" => 10,
                "trunc" => 3,
                "round" => 1,
                "rint" => 2,
                "sin" => 13,
                "cos" => 14,
                "tan" => 15,
                "asin" => 16,
                "acos" => 17,
                "atan" => 18,
                "sinh" => 19,
                "cosh" => 20,
                "tanh" => 21,
                "atan2" => 25,
                "pow" | "powr" => 26,
                "exp" => 27,
                "log" => 28,
                "exp2" => 29,
                "log2" => 30,
                "sqrt" => 31,
                "rsqrt" => 32,
                "fmin" => 37,
                "fmax" => 40,
                "mix" => 46,
                "step" => 48,
                "smoothstep" => 49,
                "fma" => 50,
                "length" => 66,
                "distance" => 67,
                "cross" => 68,
                "normalize" => 69,
                "reflect" => 71,
                _ => return None,
            })
        };
        let r = match base {
            "convert" => return self.convertir_air(&partes, a[0], ty).map(Some),
            "dot" => {
                let (x, y) = (self.id(a[0])?, self.id(a[1])?);
                self.b.dot(st, None, x, y).unwrap()
            }
            "saturate" => {
                let x = self.id(a[0])?;
                let (c0, c1) = (self.cf_n(0.0, n), self.cf_n(1.0, n));
                self.ext(st, 43, &[x, c0, c1])
            }
            "clamp" => {
                let mut ids: Vec<Word> = a.iter().map(|&v| self.id(v)).collect::<Res<_>>()?;
                let op = if real { 43 } else if signo { 45 } else { 44 };
                let w = bits(self.m, ty);
                if !real && signo && w < 32 {
                    for x in ids.iter_mut() {
                        *x = self.ext_signo(*x, w, ty);
                    }
                    let r = self.ext(st, op, &ids);
                    self.recortar(r, w, ty)
                } else {
                    self.ext(st, op, &ids)
                }
            }
            "min" | "max" if !real => {
                let mut ids: Vec<Word> = a.iter().map(|&v| self.id(v)).collect::<Res<_>>()?;
                let op = match (base, signo) { ("min", true) => 39, ("min", false) => 38, ("max", true) => 42, _ => 41 };
                let w = bits(self.m, ty);
                if signo && w < 32 {
                    for x in ids.iter_mut() {
                        *x = self.ext_signo(*x, w, ty);
                    }
                    let r = self.ext(st, op, &ids);
                    self.recortar(r, w, ty)
                } else {
                    self.ext(st, op, &ids)
                }
            }
            // pista de optimización: el mismo valor
            "is_uniform" => return self.tv(a[0]).map(Some),
            "fmax3" | "fmin3" => {
                let ids: Vec<Word> = a.iter().map(|&v| self.id(v)).collect::<Res<_>>()?;
                let op = if base == "fmax3" { 40 } else { 37 };
                let p = self.ext(st, op, &ids[..2]);
                self.ext(st, op, &[p, ids[2]])
            }
            "sign" => {
                let x = self.id(a[0])?;
                self.ext(st, if real { 6 } else { 7 }, &[x])
            }
            "abs" if real => {
                let x = self.id(a[0])?;
                self.ext(st, 4, &[x])
            }
            "any" | "all" => {
                let x = self.id(a[0])?;
                let t0 = self.f.values[a[0]].ty;
                let bx = if matches!(self.m.types[elem(self.m, t0)], Type::Int(1)) {
                    x
                } else {
                    let k = comps(self.m, t0);
                    let bt = self.vec_t(self.t_bool, k);
                    let cero = self.cu_n(0, k);
                    self.b.i_not_equal(bt, None, x, cero).unwrap()
                };
                let rb = if comps(self.m, self.f.values[a[0]].ty) == 1 { bx } else if base == "any" { self.b.any(self.t_bool, None, bx).unwrap() } else { self.b.all(self.t_bool, None, bx).unwrap() };
                if matches!(self.m.types[ty], Type::Int(1)) { rb } else {
                    let (uno, cero) = (self.cu(1), self.cu(0));
                    self.b.select(st, None, rb, uno, cero).unwrap()
                }
            }
            "dfdx" | "dfdy" | "fwidth" => {
                let x = self.id(a[0])?;
                match base {
                    "dfdx" => self.b.d_pdx(st, None, x),
                    "dfdy" => self.b.d_pdy(st, None, x),
                    _ => self.b.fwidth(st, None, x),
                }
                .unwrap()
            }
            "discard_fragment" => {
                self.usa_demote = true;
                // (rspirv lo trata como terminador de bloque: se inserta a mano)
                let inst = rspirv::dr::Instruction::new(spirv::Op::DemoteToHelperInvocation, None, None, vec![]);
                self.b.insert_into_block(rspirv::dr::InsertPoint::End, inst).map_err(|e| format!("{e:?}"))?;
                return Ok(None);
            }
            "wg" if partes.get(1) == Some(&"barrier") => {
                let (sc, sem) = (self.cu(Scope::Workgroup as u32), self.cu((MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::WORKGROUP_MEMORY | MemorySemantics::UNIFORM_MEMORY | MemorySemantics::IMAGE_MEMORY).bits()));
                self.b.control_barrier(sc, sc, sem).unwrap();
                return Ok(None);
            }
            "simdgroup" if partes.get(1) == Some(&"barrier") => {
                self.usa_subgrupos = true;
                let (sc, sem) = (self.cu(Scope::Subgroup as u32), self.cu((MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::WORKGROUP_MEMORY | MemorySemantics::UNIFORM_MEMORY | MemorySemantics::IMAGE_MEMORY).bits()));
                self.b.control_barrier(sc, sc, sem).unwrap();
                return Ok(None);
            }
            b if b.starts_with("simd_") || b.starts_with("quad_") => return self.simd(b, &partes, &a, ty).map(Some),
            "atomic" => return self.atomica(&partes, &a, ty),
            "mem" if partes.get(1) == Some(&"barrier") => {
                let (sc, sem) = (self.cu(Scope::Workgroup as u32), self.cu((MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::WORKGROUP_MEMORY | MemorySemantics::UNIFORM_MEMORY).bits()));
                self.b.memory_barrier(sc, sem).unwrap();
                return Ok(None);
            }
            "unpack" => {
                let x = self.id(a[0])?;
                let op = match partes.get(1) {
                    Some(&"unorm4x8") => 64,
                    Some(&"snorm4x8") => 63,
                    Some(&"unorm2x16") => 61,
                    Some(&"snorm2x16") => 60,
                    o => return Err(format!("unpack {o:?}")),
                };
                self.ext(st, op, &[x])
            }
            "pack" => {
                let x = self.id(a[0])?;
                let op = match partes.get(1) {
                    Some(&"unorm4x8") => 55,
                    Some(&"snorm4x8") => 54,
                    Some(&"unorm2x16") => 57,
                    Some(&"snorm2x16") => 56,
                    o => return Err(format!("pack {o:?}")),
                };
                self.ext(st, op, &[x])
            }
            "get_null_texture_2d" | "get_null_texture_3d" | "get_null_texture_cube" => return Ok(Some(TV::Ptr(Base::Nula, Off::C(0)))),
            "get_read_sampler" => return Ok(Some(TV::Smp(Smp::Lectura))),
            "get_simdgroup_size" => {
                self.usa_subgrupos = true;
                let var = self.builtin(BuiltIn::SubgroupSize, self.t_u32, StorageClass::Input);
                self.b.load(self.t_u32, None, var, None, []).unwrap()
            }
            b if b.contains("_texture_") && ["sample_", "read_", "write_texture", "gather_", "get_", "is_null_", "calculate_"].iter().any(|x| b.starts_with(x)) && !b.starts_with("get_null") => {
                return self.textura(base, &partes, &a, ty);
            }
            b => {
                let Some(op) = glsl(b) else { return Err(format!("air.{resto} sin traducción")) };
                let ids: Vec<Word> = a.iter().map(|&v| self.id(v)).collect::<Res<_>>()?;
                let r = self.ext(st, op, &ids);
                if real && bits(self.m, ty) == 16 { self.b.quantize_to_f16(st, None, r).unwrap() } else { r }
            }
        };
        Ok(Some(TV::Id(r)))
    }

    /// Intrínsecas de LLVM que deja el compilador de Metal (llvm.smax.i32, llvm.ctpop.i32, llvm.fma.f32…).
    fn intrinseca_llvm(&mut self, resto: &str, args: &[ValueId], ty: TypeId) -> Res<Word> {
        let base = resto.split('.').next().unwrap_or("");
        let st = self.ty(ty)?;
        let (w, n) = (bits(self.m, ty), comps(self.m, ty));
        let ids: Vec<Word> = args.iter().map(|&v| self.id(v)).collect::<Res<_>>()?;
        let real = es_real(self.m, ty);
        let r = match base {
            "smax" | "smin" => {
                let (x, y) = (self.ext_signo(ids[0], w, ty), self.ext_signo(ids[1], w, ty));
                let r = self.ext(st, if base == "smax" { 42 } else { 39 }, &[x, y]);
                self.recortar(r, w, ty)
            }
            "umax" => self.ext(st, 41, &ids[..2]),
            "umin" => self.ext(st, 38, &ids[..2]),
            "abs" if !real => {
                let x = self.ext_signo(ids[0], w, ty);
                let r = self.ext(st, 5, &[x]);
                self.recortar(r, w, ty)
            }
            "ctpop" => self.b.bit_count(st, None, ids[0]).unwrap(),
            "cttz" => {
                // FindILsb(0) = -1; cttz(0) = ancho
                let l = self.ext(st, 73, &[ids[0]]);
                let m = self.cu_n(w.min(32), n);
                self.ext(st, 38, &[l, m])
            }
            "ctlz" => {
                // (ancho-1) - FindUMsb; FindUMsb(0) = -1 da justo «ancho»
                let msb = self.ext(st, 75, &[ids[0]]);
                let c = self.cu_n(w.min(32) - 1, n);
                self.b.i_sub(st, None, c, msb).unwrap()
            }
            "bitreverse" => {
                let r = self.b.bit_reverse(st, None, ids[0]).unwrap();
                if w < 32 {
                    let s = self.cu_n(32 - w, n);
                    self.b.shift_right_logical(st, None, r, s).unwrap()
                } else {
                    r
                }
            }
            "bswap" => {
                // por bytes: solo 16 y 32 bits
                let ocho = self.cu_n(8, n);
                match w {
                    16 => {
                        let a = self.b.shift_left_logical(st, None, ids[0], ocho).unwrap();
                        let b = self.b.shift_right_logical(st, None, ids[0], ocho).unwrap();
                        let o = self.b.bitwise_or(st, None, a, b).unwrap();
                        self.recortar(o, 16, ty)
                    }
                    _ => {
                        let mut acc = self.cu_n(0, n);
                        for k in 0..4u32 {
                            let (off, cnt) = (self.cu_n(k * 8, n), ocho);
                            let byte = self.b.bit_field_u_extract(st, None, ids[0], off, cnt).unwrap();
                            let d = self.cu_n((3 - k) * 8, n);
                            let mv = self.b.shift_left_logical(st, None, byte, d).unwrap();
                            acc = self.b.bitwise_or(st, None, acc, mv).unwrap();
                        }
                        acc
                    }
                }
            }
            "fabs" => self.ext(st, 4, &ids[..1]),
            "floor" => self.ext(st, 8, &ids[..1]),
            "ceil" => self.ext(st, 9, &ids[..1]),
            "trunc" => self.ext(st, 3, &ids[..1]),
            "rint" | "nearbyint" | "roundeven" => self.ext(st, 2, &ids[..1]),
            "round" => self.ext(st, 1, &ids[..1]),
            "sqrt" => self.ext(st, 31, &ids[..1]),
            "exp2" => self.ext(st, 29, &ids[..1]),
            "log2" => self.ext(st, 30, &ids[..1]),
            "exp" => self.ext(st, 27, &ids[..1]),
            "log" => self.ext(st, 28, &ids[..1]),
            "sin" => self.ext(st, 13, &ids[..1]),
            "cos" => self.ext(st, 14, &ids[..1]),
            "pow" => self.ext(st, 26, &ids[..2]),
            "minnum" | "minimum" => self.ext(st, 37, &ids[..2]),
            "maxnum" | "maximum" => self.ext(st, 40, &ids[..2]),
            "fma" | "fmuladd" => self.ext(st, 50, &ids[..3]),
            "copysign" => {
                // |x| con el signo de y, por bits
                let ut = self.vec_t(self.t_u32, n);
                let (xu, yu) = (self.b.bitcast(ut, None, ids[0]).unwrap(), self.b.bitcast(ut, None, ids[1]).unwrap());
                let (m1, m2) = (self.cu_n(0x7fff_ffff, n), self.cu_n(0x8000_0000, n));
                let a = self.b.bitwise_and(ut, None, xu, m1).unwrap();
                let s = self.b.bitwise_and(ut, None, yu, m2).unwrap();
                let o = self.b.bitwise_or(ut, None, a, s).unwrap();
                self.b.bitcast(st, None, o).unwrap()
            }
            _ => return Err(format!("llamada a llvm.{resto}")),
        };
        Ok(if real && w == 16 && !matches!(base, "fabs" | "copysign") { self.b.quantize_to_f16(st, None, r).unwrap() } else { r })
    }

    /// Redondea un f32 (o vector) a bfloat16, devolviendo un f32: (bits + 0x7fff + bit16) & 0xffff0000.
    pub(crate) fn a_bf16(&mut self, x: Word, n: u32, st: Word) -> Word {
        let ut = self.vec_t(self.t_u32, n);
        let b = self.b.bitcast(ut, None, x).unwrap();
        let (c16, c1, c7, mk) = (self.cu_n(16, n), self.cu_n(1, n), self.cu_n(0x7fff, n), self.cu_n(0xffff_0000, n));
        let par = self.b.shift_right_logical(ut, None, b, c16).unwrap();
        let par = self.b.bitwise_and(ut, None, par, c1).unwrap();
        let r = self.b.i_add(ut, None, b, c7).unwrap();
        let r = self.b.i_add(ut, None, r, par).unwrap();
        let r = self.b.bitwise_and(ut, None, r, mk).unwrap();
        self.b.bitcast(st, None, r).unwrap()
    }

    // ------------------------------------------------------------ atómicas

    /// air.atomic.{global,local}.<op>[.u|.s].<tipo>(puntero, valor, orden, ámbito, volatile); load sin valor; cmpxchg
    /// (puntero, puntero a lo esperado, deseado, orden, orden, ámbito, volatile) devuelve si acertó y deja el valor
    /// que había en lo esperado; atomic.fence(flags, orden, ámbito).
    fn atomica(&mut self, p: &[&str], a: &[ValueId], ty: TypeId) -> Res<Option<TV>> {
        if p.get(1) == Some(&"fence") {
            let (sc, sem) = (self.cu(Scope::Device as u32), self.cu((MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::WORKGROUP_MEMORY | MemorySemantics::UNIFORM_MEMORY | MemorySemantics::IMAGE_MEMORY).bits()));
            self.b.memory_barrier(sc, sem).unwrap();
            return Ok(None);
        }
        let op = p.get(2).copied().unwrap_or("");
        let signo = p.get(3) == Some(&"s");
        let real = p.last().is_some_and(|t| t.starts_with('f'));
        let TV::Ptr(base, off) = self.tv(a[0])? else { return Err("atómica sin puntero conocido".into()) };
        let (ptr, sc) = self.palabra(base, off)?;
        let local = sc == StorageClass::Workgroup;
        let scope = self.cu(if local { Scope::Workgroup as u32 } else { Scope::Device as u32 });
        let sem = self.cu(0);
        let u = self.t_u32;
        // los valores de 8 y 16 bits no existen en las atómicas de Metal; todo es de 32
        let r = match op {
            "load" => self.b.atomic_load(u, None, ptr, scope, sem).unwrap(),
            "store" => {
                let v = self.id(a[1])?;
                let v = if real { self.b.bitcast(u, None, v).unwrap() } else { v };
                self.b.atomic_store(ptr, scope, sem, v).unwrap();
                return Ok(None);
            }
            "cmpxchg" => {
                // p[3] = weak/strong
                let TV::Ptr(be, oe) = self.tv(a[1])? else { return Err("cmpxchg sin puntero a lo esperado".into()) };
                let i32t = self.m.types.iter().position(|t| *t == Type::Int(32)).ok_or("sin tipo i32")?;
                let esperado = self.cargar(be, oe, i32t)?;
                let deseado = self.id(a[2])?;
                let deseado = if real { self.b.bitcast(u, None, deseado).unwrap() } else { deseado };
                let viejo = self.b.atomic_compare_exchange(u, None, ptr, scope, sem, sem, deseado, esperado).unwrap();
                self.guardar(be, oe, i32t, viejo)?;
                let ok = self.b.i_equal(self.t_bool, None, viejo, esperado).unwrap();
                let st = self.ty(ty)?;
                return Ok(Some(TV::Id(if self.es_bool(ty) { ok } else { let (uno, cero) = (self.cu(1), self.cu(0)); self.b.select(st, None, ok, uno, cero).unwrap() })));
            }
            "add" | "sub" if real => {
                // sin atómicas de punto flotante: bucle de compare-exchange
                let v = self.id(a[1])?;
                let v = if op == "sub" { self.b.f_negate(self.t_f32, None, v).unwrap() } else { v };
                return self.atomica_cas_real(ptr, scope, sem, v).map(|x| Some(TV::Id(x)));
            }
            _ => {
                let v = self.id(a[1])?;
                match (op, signo) {
                    ("add", _) => self.b.atomic_i_add(u, None, ptr, scope, sem, v).unwrap(),
                    ("sub", _) => self.b.atomic_i_sub(u, None, ptr, scope, sem, v).unwrap(),
                    ("max", true) => self.b.atomic_s_max(u, None, ptr, scope, sem, v).unwrap(),
                    ("min", true) => self.b.atomic_s_min(u, None, ptr, scope, sem, v).unwrap(),
                    ("max", false) => self.b.atomic_u_max(u, None, ptr, scope, sem, v).unwrap(),
                    ("min", false) => self.b.atomic_u_min(u, None, ptr, scope, sem, v).unwrap(),
                    ("and", _) => self.b.atomic_and(u, None, ptr, scope, sem, v).unwrap(),
                    ("or", _) => self.b.atomic_or(u, None, ptr, scope, sem, v).unwrap(),
                    ("xor", _) => self.b.atomic_xor(u, None, ptr, scope, sem, v).unwrap(),
                    ("xchg", _) => {
                        let v = if real { self.b.bitcast(u, None, v).unwrap() } else { v };
                        self.b.atomic_exchange(u, None, ptr, scope, sem, v).unwrap()
                    }
                    (o, _) => return Err(format!("atómica {o} sin traducción")),
                }
            }
        };
        Ok(Some(TV::Id(if real { self.b.bitcast(self.t_f32, None, r).unwrap() } else { r })))
    }

    /// Suma atómica de punto flotante con un bucle de compare-exchange; devuelve el valor anterior.
    fn atomica_cas_real(&mut self, ptr: Word, scope: Word, sem: Word, v: Word) -> Res<Word> {
        let u = self.t_u32;
        let (cab, cuerpo, cont, fin) = (self.b.id(), self.b.id(), self.b.id(), self.b.id());
        self.b.branch(cab).unwrap();
        self.b.begin_block(Some(cab)).unwrap();
        self.b.loop_merge(fin, cont, spirv::LoopControl::NONE, []).unwrap();
        self.b.branch(cuerpo).unwrap();
        self.b.begin_block(Some(cuerpo)).unwrap();
        let viejo = self.b.atomic_load(u, None, ptr, scope, sem).unwrap();
        let vf = self.b.bitcast(self.t_f32, None, viejo).unwrap();
        let suma = self.b.f_add(self.t_f32, None, vf, v).unwrap();
        let nuevo = self.b.bitcast(u, None, suma).unwrap();
        let visto = self.b.atomic_compare_exchange(u, None, ptr, scope, sem, sem, nuevo, viejo).unwrap();
        let ok = self.b.i_equal(self.t_bool, None, visto, viejo).unwrap();
        self.b.branch(cont).unwrap();
        self.b.begin_block(Some(cont)).unwrap();
        self.b.branch_conditional(ok, fin, cab, []).unwrap();
        self.b.begin_block(Some(fin)).unwrap();
        Ok(self.b.bitcast(self.t_f32, None, viejo).unwrap())
    }

    // ------------------------------------------------------------ funciones de SIMD-group y quad

    /// Índice del hilo en el subgrupo.
    fn carril(&mut self) -> Word {
        self.usa_subgrupos = true;
        let var = self.builtin(BuiltIn::SubgroupLocalInvocationId, self.t_u32, StorageClass::Input);
        self.b.load(self.t_u32, None, var, None, []).unwrap()
    }

    fn tam_subgrupo(&mut self) -> Word {
        self.usa_subgrupos = true;
        let var = self.builtin(BuiltIn::SubgroupSize, self.t_u32, StorageClass::Input);
        self.b.load(self.t_u32, None, var, None, []).unwrap()
    }

    /// Instrucción de shuffle de subgrupo (Shuffle, ShuffleXor, ShuffleUp, ShuffleDown) a mano: rspirv no la tiene.
    fn shuffle(&mut self, op: spirv::Op, st: Word, v: Word, id: Word) -> Word {
        let sc = self.cu(Scope::Subgroup as u32);
        let r = self.b.id();
        let inst = rspirv::dr::Instruction::new(op, Some(st), Some(r), vec![Operand::IdRef(sc), Operand::IdRef(v), Operand::IdRef(id)]);
        self.b.insert_into_block(rspirv::dr::InsertPoint::End, inst).unwrap();
        r
    }

    /// Operación de reducción entre dos valores del tipo `ty`.
    fn op_reduccion(&mut self, op: &str, st: Word, ty: TypeId, x: Word, y: Word, signo: bool) -> Res<Word> {
        let real = es_real(self.m, ty);
        let w = bits(self.m, ty);
        Ok(match (op, real) {
            ("sum", true) => self.b.f_add(st, None, x, y).unwrap(),
            ("sum", false) => self.b.i_add(st, None, x, y).unwrap(),
            ("product", true) => self.b.f_mul(st, None, x, y).unwrap(),
            ("product", false) => self.b.i_mul(st, None, x, y).unwrap(),
            ("max", true) => self.ext(st, 40, &[x, y]),
            ("min", true) => self.ext(st, 37, &[x, y]),
            ("max" | "min", false) if signo => {
                let (sx, sy) = (self.ext_signo(x, w, ty), self.ext_signo(y, w, ty));
                let r = self.ext(st, if op == "max" { 42 } else { 39 }, &[sx, sy]);
                self.recortar(r, w, ty)
            }
            ("max", false) => self.ext(st, 41, &[x, y]),
            ("min", false) => self.ext(st, 38, &[x, y]),
            ("and", _) | ("all", _) => if self.es_bool(ty) { self.b.logical_and(st, None, x, y).unwrap() } else { self.b.bitwise_and(st, None, x, y).unwrap() },
            ("or", _) | ("any", _) => if self.es_bool(ty) { self.b.logical_or(st, None, x, y).unwrap() } else { self.b.bitwise_or(st, None, x, y).unwrap() },
            ("xor", _) => if self.es_bool(ty) { self.b.logical_not_equal(st, None, x, y).unwrap() } else { self.b.bitwise_xor(st, None, x, y).unwrap() },
            _ => return Err(format!("reducción {op} sin traducción")),
        })
    }

    fn es_bool(&self, ty: TypeId) -> bool {
        matches!(self.m.types[elem(self.m, ty)], Type::Int(1))
    }

    /// simd_* y quad_*: reducciones con mariposa de ShuffleXor (la GPU no tiene la aritmética de subgrupos), prefijos
    /// con ShuffleUp, y los shuffles de Metal (que dejan el valor propio cuando el origen se sale del grupo).
    fn simd(&mut self, base: &str, partes: &[&str], a: &[ValueId], ty: TypeId) -> Res<TV> {
        self.usa_subgrupos = true;
        let quad = base.starts_with("quad_");
        let op = base.split_once('_').map(|x| x.1).unwrap_or("");
        let signo = partes.get(1) == Some(&"s");
        let ancho = if quad { 4 } else { 32 };
        let st = self.ty(ty)?;
        let half = es_real(self.m, ty) && bits(self.m, ty) == 16;
        let q = |tr: &mut Self, r: Word| if half { tr.b.quantize_to_f16(st, None, r).unwrap() } else { r };
        let carril = self.carril();
        let r = match op {
            "sum" | "product" | "max" | "min" | "and" | "or" | "xor" | "all" | "any" => {
                let mut v = self.id(a[0])?;
                let tam = self.tam_subgrupo();
                let mut paso = ancho / 2;
                while paso >= 1 {
                    let c = self.cu(paso);
                    let otro = self.shuffle(spirv::Op::GroupNonUniformShuffleXor, st, v, c);
                    let r = self.op_reduccion(op, st, ty, v, otro, signo)?;
                    // con subgrupos más pequeños que el paso, el compañero no existe
                    let dentro = self.b.u_less_than(self.t_bool, None, c, tam).unwrap();
                    v = self.select_valor(ty, dentro, r, v)?;
                    paso /= 2;
                }
                q(self, v)
            }
            "prefix_inclusive_sum" | "prefix_exclusive_sum" | "prefix_inclusive_product" | "prefix_exclusive_product" => {
                let red = if op.ends_with("sum") { "sum" } else { "product" };
                let mut v = self.id(a[0])?;
                let mut paso = 1;
                while paso < 32 {
                    let c = self.cu(paso);
                    let otro = self.shuffle(spirv::Op::GroupNonUniformShuffleUp, st, v, c);
                    let r = self.op_reduccion(red, st, ty, v, otro, signo)?;
                    let tiene = self.b.u_greater_than_equal(self.t_bool, None, carril, c).unwrap();
                    v = self.select_valor(ty, tiene, r, v)?;
                    paso *= 2;
                }
                if op.contains("exclusive") {
                    let uno = self.cu(1);
                    let antes = self.shuffle(spirv::Op::GroupNonUniformShuffleUp, st, v, uno);
                    let neutro = if red == "sum" { self.cero_de(ty)? } else { self.uno_de(ty)? };
                    let cero = self.cu(0);
                    let es0 = self.b.i_equal(self.t_bool, None, carril, cero).unwrap();
                    v = self.select_valor(ty, es0, neutro, antes)?;
                }
                q(self, v)
            }
            "shuffle" | "broadcast" | "shuffle_xor" | "shuffle_down" | "shuffle_up" | "shuffle_rotate_down" | "shuffle_rotate_up" => {
                let v = self.id(a[0])?;
                let d = self.id(a[1])?;
                let mk = self.cu(ancho - 1);
                let base_q = if quad { let nm = self.cu(!(ancho - 1)); self.b.bitwise_and(self.t_u32, None, carril, nm).unwrap() } else { self.cu(0) };
                let local = self.b.bitwise_and(self.t_u32, None, carril, mk).unwrap();
                // índice de origen dentro del grupo (de `ancho` hilos) y si es válido
                let (orig, valido) = match op {
                    "shuffle" | "broadcast" => (self.b.bitwise_and(self.t_u32, None, d, mk).unwrap(), None),
                    "shuffle_xor" => {
                        let x = self.b.bitwise_xor(self.t_u32, None, local, d).unwrap();
                        let lim = self.cu(ancho);
                        let ok = self.b.u_less_than(self.t_bool, None, x, lim).unwrap();
                        (x, Some(ok))
                    }
                    "shuffle_down" => {
                        let x = self.b.i_add(self.t_u32, None, local, d).unwrap();
                        let lim = self.cu(ancho);
                        let ok = self.b.u_less_than(self.t_bool, None, x, lim).unwrap();
                        (x, Some(ok))
                    }
                    "shuffle_up" => {
                        let ok = self.b.u_greater_than_equal(self.t_bool, None, local, d).unwrap();
                        (self.b.i_sub(self.t_u32, None, local, d).unwrap(), Some(ok))
                    }
                    "shuffle_rotate_down" => {
                        let x = self.b.i_add(self.t_u32, None, local, d).unwrap();
                        (self.b.bitwise_and(self.t_u32, None, x, mk).unwrap(), None)
                    }
                    _ => {
                        let x = self.b.i_sub(self.t_u32, None, local, d).unwrap();
                        (self.b.bitwise_and(self.t_u32, None, x, mk).unwrap(), None)
                    }
                };
                let orig = self.b.bitwise_and(self.t_u32, None, orig, mk).unwrap();
                let id = self.b.bitwise_or(self.t_u32, None, base_q, orig).unwrap();
                let r = self.shuffle(spirv::Op::GroupNonUniformShuffle, st, v, id);
                match valido {
                    Some(ok) => self.select_valor(ty, ok, r, v)?,
                    None => r,
                }
            }
            "shuffle_and_fill_down" | "shuffle_and_fill_up" => {
                // (datos, relleno, delta[, módulo]): lo que se sale se toma del relleno
                let (v, rell, d) = (self.id(a[0])?, self.id(a[1])?, self.id(a[2])?);
                let lim = self.cu(ancho);
                let mk = self.cu(ancho - 1);
                let local = self.b.bitwise_and(self.t_u32, None, carril, mk).unwrap();
                let base_q = { let nm = self.cu(!(ancho - 1)); self.b.bitwise_and(self.t_u32, None, carril, nm).unwrap() };
                let (x, dentro) = if op.ends_with("down") {
                    let x = self.b.i_add(self.t_u32, None, local, d).unwrap();
                    (x, self.b.u_less_than(self.t_bool, None, x, lim).unwrap())
                } else {
                    let x = self.b.i_sub(self.t_u32, None, local, d).unwrap();
                    (self.b.i_add(self.t_u32, None, x, lim).unwrap(), self.b.u_greater_than_equal(self.t_bool, None, local, d).unwrap())
                };
                let xm = self.b.bitwise_and(self.t_u32, None, x, mk).unwrap();
                let id = self.b.bitwise_or(self.t_u32, None, base_q, xm).unwrap();
                let rd = self.shuffle(spirv::Op::GroupNonUniformShuffle, st, v, id);
                let rr = self.shuffle(spirv::Op::GroupNonUniformShuffle, st, rell, id);
                self.select_valor(ty, dentro, rd, rr)?
            }
            "is_first" if !quad => {
                let sc = self.cu(Scope::Subgroup as u32);
                self.b.group_non_uniform_elect(self.t_bool, None, sc).unwrap()
            }
            "is_first" => {
                // el primer hilo activo de su quad: bits del ballot de su quad
                let sc = self.cu(Scope::Subgroup as u32);
                let v4 = self.vec_t(self.t_u32, 4);
                let t = self.cbool(true);
                let bal = self.b.group_non_uniform_ballot(v4, None, sc, t).unwrap();
                let x = self.b.composite_extract(self.t_u32, None, bal, [0]).unwrap();
                let nm = self.cu(!3);
                let base_q = self.b.bitwise_and(self.t_u32, None, carril, nm).unwrap();
                let bits_q = self.b.shift_right_logical(self.t_u32, None, x, base_q).unwrap();
                let f = self.cu(0xf);
                let bits_q = self.b.bitwise_and(self.t_u32, None, bits_q, f).unwrap();
                let lsb = self.ext(self.t_u32, 73, &[bits_q]);
                let tres = self.cu(3);
                let local = self.b.bitwise_and(self.t_u32, None, carril, tres).unwrap();
                self.b.i_equal(self.t_bool, None, lsb, local).unwrap()
            }
            "ballot" | "active_threads_mask" => {
                let sc = self.cu(Scope::Subgroup as u32);
                let v4 = self.vec_t(self.t_u32, 4);
                let c = if op == "ballot" { let x = self.id(a[0])?; self.a_bool(x, self.f.values[a[0]].ty) } else { self.cbool(true) };
                let bal = self.b.group_non_uniform_ballot(v4, None, sc, c).unwrap();
                self.b.composite_extract(self.t_u32, None, bal, [0]).unwrap()
            }
            _ => return Err(format!("air.{base} sin traducción")),
        };
        Ok(TV::Id(r))
    }

    /// select entre dos valores de tipo `ty` con una condición escalar (los vectores la necesitan repetida).
    fn select_valor(&mut self, ty: TypeId, c: Word, x: Word, y: Word) -> Res<Word> {
        let st = self.ty(ty)?;
        Ok(self.b.select(st, None, c, x, y).unwrap())
    }

    fn a_bool(&mut self, x: Word, t: TypeId) -> Word {
        if self.es_bool(t) {
            return x;
        }
        let cero = self.cu(0);
        self.b.i_not_equal(self.t_bool, None, x, cero).unwrap()
    }

    fn cero_de(&mut self, ty: TypeId) -> Res<Word> {
        let n = comps(self.m, ty);
        Ok(if es_real(self.m, ty) { self.cf_n(0.0, n) } else { self.cu_n(0, n) })
    }

    fn uno_de(&mut self, ty: TypeId) -> Res<Word> {
        let n = comps(self.m, ty);
        Ok(if es_real(self.m, ty) { self.cf_n(1.0, n) } else { self.cu_n(1, n) })
    }

    /// air.convert.<a>.<tipo a>.<de>.<tipo de>: f (real), u (sin signo), s (con signo).
    fn convertir_air(&mut self, p: &[&str], v: ValueId, to: TypeId) -> Res<TV> {
        let (a, de) = (p.get(1).copied().unwrap_or("f"), p.get(3).copied().unwrap_or("f"));
        let from = self.f.values[v].ty;
        let op = match (a, de) {
            ("f", "f") => if bits(self.m, to) < bits(self.m, from) { CastOp::FPTrunc } else { CastOp::FPExt },
            ("f", "u") => CastOp::UIToFP,
            ("f", "s") => CastOp::SIToFP,
            ("u", "f") => CastOp::FPToUI,
            ("s", "f") => CastOp::FPToSI,
            ("u", "u") | ("s", "u") => if bits(self.m, to) < bits(self.m, from) { CastOp::Trunc } else { CastOp::ZExt },
            ("u", "s") | ("s", "s") => if bits(self.m, to) < bits(self.m, from) { CastOp::Trunc } else { CastOp::SExt },
            o => return Err(format!("air.convert {o:?}")),
        };
        if (op == CastOp::FPExt || op == CastOp::ZExt) && bits(self.m, to) == bits(self.m, from) {
            return self.tv(v);
        }
        self.conversion(op, v, to)
    }

    fn memcpy(&mut self, a: &[ValueId]) -> Res<()> {
        let (TV::Ptr(bd, od), TV::Ptr(bo, oo)) = (self.tv(a[0])?, self.tv(a[1])?) else { return Err("memcpy sin punteros".into()) };
        let TV::C(V::Int(_, n)) = self.tv(a[2])? else { return Err("memcpy de tamaño variable".into()) };
        let mut k = 0;
        let i32t = self.m.types.iter().position(|t| *t == Type::Int(32));
        let i8t = self.m.types.iter().position(|t| *t == Type::Int(8));
        while k < n as i64 {
            let (t, paso) = if n as i64 - k >= 4 && i32t.is_some() { (i32t.unwrap(), 4) } else { (i8t.ok_or("memcpy sin tipo i8")?, 1) };
            let (oo2, od2) = (self.off_mas(oo, k), self.off_mas(od, k));
            let x = self.cargar(bo, oo2, t)?;
            self.guardar(bd, od2, t, x)?;
            k += paso;
        }
        Ok(())
    }

    fn memset(&mut self, a: &[ValueId]) -> Res<()> {
        let TV::Ptr(bd, od) = self.tv(a[0])? else { return Err("memset sin puntero".into()) };
        let TV::C(V::Int(_, n)) = self.tv(a[2])? else { return Err("memset de tamaño variable".into()) };
        let i8t = self.m.types.iter().position(|t| *t == Type::Int(8)).ok_or("memset sin tipo i8")?;
        let x = self.id(a[1])?;
        for k in 0..n as i64 {
            let o = self.off_mas(od, k);
            self.guardar(bd, o, i8t, x)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ texturas

    /// Variable de una textura: (variable, tipo de imagen). `escritura`: imagen de almacenamiento.
    fn var_textura(&mut self, idx: u32, escritura: bool, forma: Forma, muestra: Word) -> Word {
        if let Some(&(v, _)) = self.texturas.get(&(idx, escritura)) {
            return v;
        }
        let Forma { dim, arreglo, ms } = forma;
        match (dim, escritura) {
            (Dim::Dim1D, false) => self.capacidades.push(Capability::Sampled1D),
            (Dim::Dim1D, true) => self.capacidades.push(Capability::Image1D),
            (Dim::DimCube, false) if arreglo => self.capacidades.push(Capability::SampledCubeArray),
            (Dim::DimCube, true) if arreglo => self.capacidades.push(Capability::ImageCubeArray),
            _ => {}
        }
        if ms && escritura {
            self.capacidades.push(Capability::StorageImageMultisample);
        }
        let it = self.b.type_image(muestra, dim, 0, arreglo as u32, ms as u32, if escritura { 2 } else { 1 }, ImageFormat::Unknown, None);
        let pt = self.ptr_t(StorageClass::UniformConstant, it);
        let v = self.var_global(pt, StorageClass::UniformConstant, None);
        self.b.decorate(v, Decoration::DescriptorSet, [Operand::LiteralBit32(self.set)]);
        let binding = if escritura { BIND_TEXTURA_ESCRITURA } else { BIND_TEXTURA } + idx;
        self.b.decorate(v, Decoration::Binding, [Operand::LiteralBit32(binding)]);
        if escritura {
            self.b.decorate(v, Decoration::NonReadable, []);
        }
        self.interfaz.push(v);
        self.texturas.insert((idx, escritura), (v, it));
        let recurso = if escritura { Recurso::TexturaEscritura { indice: idx, dim, arreglo } } else { Recurso::Textura { indice: idx, dim, arreglo } };
        self.enlaces.push(Enlace { binding, recurso });
        v
    }

    fn var_sampler(&mut self, s: Smp) -> Res<Word> {
        if let Some(&v) = self.samplers.get(&s) {
            return Ok(v);
        }
        let pt = self.ptr_t(StorageClass::UniformConstant, self.t_sampler);
        let v = self.var_global(pt, StorageClass::UniformConstant, None);
        self.b.decorate(v, Decoration::DescriptorSet, [Operand::LiteralBit32(self.set)]);
        let (binding, recurso) = match s {
            Smp::Arg(i) => (BIND_SAMPLER + i, Recurso::Sampler { indice: i }),
            Smp::Fijo(g) => {
                let k = self.enlaces.iter().filter(|e| matches!(e.recurso, Recurso::SamplerFijo { .. })).count() as u32;
                let b = self.mem.globales.get(g).cloned().unwrap_or_default();
                let w = |i: usize| u64::from_le_bytes(b.get(i * 8..i * 8 + 8).and_then(|s| s.try_into().ok()).unwrap_or([0; 8]));
                (BIND_SAMPLER_FIJO + k, Recurso::SamplerFijo { estado: [w(0), w(1)] })
            }
            Smp::Lectura => return Err("el sampler de lectura no se usa para muestrear".into()),
        };
        self.b.decorate(v, Decoration::Binding, [Operand::LiteralBit32(binding)]);
        self.interfaz.push(v);
        self.samplers.insert(s, v);
        self.enlaces.push(Enlace { binding, recurso });
        Ok(v)
    }

    /// Coordenada con la capa al final (texturas array y caras de cubo). `real`: la coordenada es de punto flotante
    /// (la capa entera se convierte).
    fn coord_con_capa(&mut self, coord: ValueId, capa: Option<Word>, real: bool) -> Res<Word> {
        let c = self.id(coord)?;
        let Some(capa) = capa else { return Ok(c) };
        let n = comps(self.m, self.f.values[coord].ty);
        let e = if real { self.t_f32 } else { self.t_u32 };
        let capa = if real { self.b.convert_u_to_f(self.t_f32, None, capa).unwrap() } else { capa };
        let vt = self.vec_t(e, n + 1);
        let mut partes = Vec::new();
        if n == 1 {
            partes.push(c);
        } else {
            for k in 0..n {
                partes.push(self.b.composite_extract(e, None, c, [k]).unwrap());
            }
        }
        partes.push(capa);
        Ok(self.b.composite_construct(vt, None, partes).unwrap())
    }

    /// Desplazamiento constante distinto de cero (o None).
    fn desplazamiento(&mut self, v: ValueId) -> Res<Option<Word>> {
        Ok(match self.tv(v)? {
            TV::C(V::Vec(es)) if es.iter().any(|e| e.int().unwrap_or(0) != 0) => Some(self.id(v)?),
            TV::C(V::Int(_, x)) if x != 0 => Some(self.id(v)?),
            _ => None,
        })
    }

    fn sampler_de(&mut self, v: ValueId) -> Res<Word> {
        let smp = match self.tv(v)? {
            TV::Smp(s) => s,
            TV::Ptr(Base::Glob(g), _) => Smp::Fijo(g),
            o => return Err(format!("sampler desconocido: {o:?}")),
        };
        let sv = self.var_sampler(smp)?;
        Ok(self.b.load(self.t_sampler, None, sv, None, []).unwrap())
    }

    /// Funciones de texturas de AIR. Las firmas (de todos los metallib de macOS):
    /// - sample_texture_{1d,2d,3d}[_array]: (tex, smp, coord, [capa], i1, desplazamiento, i1 nivel explícito, nivel o
    ///   sesgo, lod mínimo, i32); cube: (tex, smp, coord3, i1 nivel explícito, nivel, lod mínimo, i32);
    ///   2d_grad: (tex, smp, coord, [capa], dPdx, dPdy, lod mínimo, i1, desplazamiento, i32)
    /// - gather_texture_2d[_array]: (tex, smp, coord, [capa], i1, desplazamiento, componente, i32)
    /// - read_texture_*: (tex, [smp], coord, [capa o cara], [muestra (ms)], [desplazamiento], [nivel], acceso)
    /// - write_texture_*: (tex, coord, [capa o cara], valor, [nivel], acceso)
    /// - get_{width,height,depth}_texture_*(tex, nivel), get_array_size_*(tex), get_num_mip_levels_*(tex),
    ///   get_num_samples_*(tex), is_null_texture_*(tex), calculate_[un]clamped_lod_*(tex, smp, coord, i32)
    fn textura(&mut self, base: &str, p: &[&str], a: &[ValueId], ty: TypeId) -> Res<Option<TV>> {
        let dim_txt = base.split("texture_").nth(1).unwrap_or("2d");
        let ms = dim_txt.contains("_ms");
        let cubo = dim_txt.starts_with("cube");
        let dim = if dim_txt.starts_with("1d") {
            Dim::Dim1D
        } else if dim_txt.starts_with("3d") {
            Dim::Dim3D
        } else if cubo {
            Dim::DimCube
        } else if dim_txt.starts_with("buffer") {
            return Err(format!("air.{base}: texturas buffer sin soporte aún"));
        } else {
            Dim::Dim2D
        };
        let arreglo = dim_txt.contains("array");
        let forma = Forma { dim, arreglo, ms };
        if base.starts_with("is_null_texture") {
            let nula = matches!(self.tv(a[0])?, TV::Ptr(Base::Nula, _) | TV::C(V::Undef));
            return Ok(Some(TV::Id(self.cbool(nula))));
        }
        // tipo de muestra: del tipo del argumento (texture2d<uint>...) y, si no se sabe, del nombre (.u., .s.)
        let (muestra, vec4) = {
            let tex_idx0 = match self.tv(a[0])? { TV::Tex(i) => Some(i), _ => None };
            let sufijo = match tex_idx0.and_then(|i| self.tipos_textura.get(&i).copied()) {
                Some(k) => k,
                None => p.iter().any(|x| *x == "u") as u8 + 2 * p.iter().any(|x| *x == "s") as u8,
            };
            let w = match sufijo {
                1 => self.t_u32,
                2 => self.t_i32,
                _ => self.t_f32,
            };
            (w, self.vec_t(w, 4))
        };
        let tex_idx = match self.tv(a[0])? {
            TV::Tex(i) => i,
            // textura nula (o ausente en esta variante): Metal da 0 al leer, no escribe y mide 0
            TV::Ptr(Base::Nula, _) | TV::C(V::Undef) => {
                if base.starts_with("write_") {
                    return Ok(None);
                }
                if matches!(self.m.types[ty], Type::Void) {
                    return Ok(None);
                }
                if let Type::Struct { .. } = self.m.types[ty] {
                    let st = self.ty(ty)?;
                    let cero4 = { let c = self.cu(0); let z = match muestra { x if x == self.t_f32 => self.cf(0.0), _ => c }; self.b.constant_composite(vec4, vec![z; 4]) };
                    let cero = self.cu(0);
                    return Ok(Some(TV::Id(self.b.composite_construct(st, None, [cero4, cero]).unwrap())));
                }
                let st = self.ty(ty)?;
                return Ok(Some(TV::Id(self.b.constant_null(st))));
            }
            o => return Err(format!("textura desconocida: {o:?}")),
        };
        // variantes half (v4f16): el texel se redondea a half, como en Metal
        let es_half = p.iter().any(|x| x.ends_with("f16"));
        let res_ty = |tr: &mut Self, texel: Word| -> Res<TV> {
            let texel = if es_half { tr.b.quantize_to_f16(vec4, None, texel).unwrap() } else { texel };
            // {<4 x T>, i8}: el texel y la residencia (siempre presente)
            if let Type::Struct { .. } = tr.m.types[ty] {
                let st = tr.ty(ty)?;
                let cero = tr.cu(0);
                Ok(TV::Id(tr.b.composite_construct(st, None, [texel, cero]).unwrap()))
            } else {
                Ok(TV::Id(texel))
            }
        };
        // capa de las coordenadas enteras: la capa, o la cara del cubo (cube_array: capa·6 + cara)
        let capa_entera = |tr: &mut Self, k: &mut usize| -> Res<Option<Word>> {
            if !(arreglo || cubo) {
                return Ok(None);
            }
            let c = tr.id(a[*k])?;
            *k += 1;
            if cubo && arreglo {
                let l = tr.id(a[*k])?;
                *k += 1;
                let seis = tr.cu(6);
                let x = tr.b.i_mul(tr.t_u32, None, l, seis).unwrap();
                return Ok(Some(tr.b.i_add(tr.t_u32, None, x, c).unwrap()));
            }
            Ok(Some(c))
        };
        if base.starts_with("write_texture") {
            let v = self.var_textura(tex_idx, true, forma, muestra);
            let (_, it) = self.texturas[&(tex_idx, true)];
            let img = self.b.load(it, None, v, None, []).unwrap();
            let mut k = 2;
            let capa = capa_entera(self, &mut k)?;
            let coord = self.coord_con_capa(a[1], capa, false)?;
            let texel = self.id(a[k])?;
            self.b.image_write(img, coord, texel, None, []).unwrap();
            return Ok(None);
        }
        let v = self.var_textura(tex_idx, false, forma, muestra);
        let (_, it) = self.texturas[&(tex_idx, false)];
        let img = self.b.load(it, None, v, None, []).unwrap();
        if let Some(q) = ["get_width", "get_height", "get_depth", "get_array_size"].iter().find(|q| base.starts_with(**q)) {
            let n = match dim { Dim::Dim1D => 1, Dim::Dim3D => 3, _ => 2 } + arreglo as u32;
            let vt = self.vec_t(self.t_u32, n);
            let sz = if ms {
                self.b.image_query_size(vt, None, img).unwrap()
            } else {
                let lod = if a.len() > 1 { self.id(a[1])? } else { self.cu(0) };
                self.b.image_query_size_lod(vt, None, img, lod).unwrap()
            };
            let k = match *q { "get_width" => 0, "get_height" => 1, "get_depth" => 2, _ => n - 1 };
            let r = if n == 1 { sz } else { self.b.composite_extract(self.t_u32, None, sz, [k]).unwrap() };
            let r = if *q == "get_array_size" && cubo { let seis = self.cu(6); self.b.u_div(self.t_u32, None, r, seis).unwrap() } else { r };
            return Ok(Some(TV::Id(r)));
        }
        if base.starts_with("get_num_mip_levels") {
            return Ok(Some(TV::Id(self.b.image_query_levels(self.t_u32, None, img).unwrap())));
        }
        if base.starts_with("get_num_samples") {
            return Ok(Some(TV::Id(self.b.image_query_samples(self.t_u32, None, img).unwrap())));
        }
        if base.starts_with("read_texture") {
            if cubo {
                return Err(format!("air.{base}: lectura de cubos sin soporte aún"));
            }
            // ¿segundo argumento sampler? (punteros del espacio 2)
            let con_smp = espacio(self.m, self.f.values[a[1]].ty) == Some(2);
            let mut k = if con_smp { 2 } else { 1 };
            let c0 = a[k];
            k += 1;
            let capa = capa_entera(self, &mut k)?;
            let coord = self.coord_con_capa(c0, capa, false)?;
            let resto = a.len() - k;
            let texel = if ms {
                let s = self.id(a[k])?;
                self.b.image_fetch(vec4, None, img, coord, Some(ImageOperands::SAMPLE), [Operand::IdRef(s)]).unwrap()
            } else {
                // (desplazamiento, nivel, acceso) o (nivel, acceso)
                let (desp, lod) = if resto >= 3 { (self.desplazamiento(a[k])?, a[k + 1]) } else { (None, a[k]) };
                let lod = self.id(lod)?;
                let mut flags = ImageOperands::LOD;
                let mut ops = vec![Operand::IdRef(lod)];
                if let Some(d) = desp {
                    flags |= ImageOperands::CONST_OFFSET;
                    ops.push(Operand::IdRef(d));
                }
                self.b.image_fetch(vec4, None, img, coord, Some(flags), ops).unwrap()
            };
            return res_ty(self, texel).map(Some);
        }
        let s = self.sampler_de(a[1])?;
        let sit = self.b.type_sampled_image(it);
        let si = self.b.sampled_image(sit, None, img, s).unwrap();
        let mut k = 3;
        let capa = if arreglo {
            k += 1;
            Some(self.id(a[3])?)
        } else {
            None
        };
        let coord = self.coord_con_capa(a[2], capa, true)?;
        if base.starts_with("calculate_") {
            let v2 = self.vec_t(self.t_f32, 2);
            let q = self.b.image_query_lod(v2, None, si, coord).unwrap();
            let i = if base.starts_with("calculate_clamped") { 0 } else { 1 };
            return Ok(Some(TV::Id(self.b.composite_extract(self.t_f32, None, q, [i]).unwrap())));
        }
        if base.starts_with("gather_texture") {
            // (i1, desplazamiento, componente, i32)
            let desp = self.desplazamiento(a[k + 1])?;
            let comp = self.id(a[k + 2])?;
            let (flags, ops) = match desp {
                Some(d) => (Some(ImageOperands::CONST_OFFSET), vec![Operand::IdRef(d)]),
                None => (None, vec![]),
            };
            let texel = self.b.image_gather(vec4, None, si, coord, comp, flags, ops).unwrap();
            return res_ty(self, texel).map(Some);
        }
        let fragment = self.ent.etapa == Etapa::Fragment;
        let mut ops = Vec::new();
        let mut flags = ImageOperands::NONE;
        if base.contains("_grad") {
            // (dPdx, dPdy, lod mínimo, i1, desplazamiento, i32)
            let (dx, dy) = (self.id(a[k])?, self.id(a[k + 1])?);
            flags |= ImageOperands::GRAD;
            ops.push(Operand::IdRef(dx));
            ops.push(Operand::IdRef(dy));
            if let Some(d) = self.desplazamiento(a[k + 4])? {
                flags |= ImageOperands::CONST_OFFSET;
                ops.push(Operand::IdRef(d));
            }
            let texel = self.b.image_sample_explicit_lod(vec4, None, si, coord, flags, ops).unwrap();
            return res_ty(self, texel).map(Some);
        }
        // (i1, desplazamiento, i1 explícito, nivel, …) o, en cubos, (i1 explícito, nivel, …)
        let (desp, i_expl) = if cubo { (None, k) } else { (self.desplazamiento(a[k + 1])?, k + 2) };
        let explicito = matches!(self.tv(a[i_expl])?, TV::C(V::Int(_, 1)));
        let nivel = self.tv(a[i_expl + 1])?;
        let t_nivel = self.f.values[a[i_expl + 1]].ty;
        let texel = if explicito || !fragment {
            flags |= ImageOperands::LOD;
            let l = if explicito { self.id_de(&nivel, t_nivel)? } else { self.cf(0.0) };
            ops.push(Operand::IdRef(l));
            if let Some(d) = desp {
                flags |= ImageOperands::CONST_OFFSET;
                ops.push(Operand::IdRef(d));
            }
            self.b.image_sample_explicit_lod(vec4, None, si, coord, flags, ops).unwrap()
        } else {
            if !matches!(nivel, TV::C(V::Real(_, x)) if x == 0.0) {
                flags |= ImageOperands::BIAS;
                let l = self.id_de(&nivel, t_nivel)?;
                ops.push(Operand::IdRef(l));
            }
            if let Some(d) = desp {
                flags |= ImageOperands::CONST_OFFSET;
                ops.push(Operand::IdRef(d));
            }
            self.b.image_sample_implicit_lod(vec4, None, si, coord, if flags.is_empty() { None } else { Some(flags) }, ops).unwrap()
        };
        res_ty(self, texel).map(Some)
    }
}

/// Forma de una textura en SPIR-V.
#[derive(Clone, Copy)]
pub(super) struct Forma {
    dim: Dim,
    arreglo: bool,
    ms: bool,
}
