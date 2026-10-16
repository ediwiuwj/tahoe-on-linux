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
        Type::Float => 32,
        Type::Double => 64,
        _ => 0,
    }
}

fn es_real(m: &Module, t: TypeId) -> bool {
    matches!(m.types[elem(m, t)], Type::Half | Type::Float | Type::Double)
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
                    let (TV::Ptr(bt, ot), TV::Ptr(bf, of)) = (pt, pf) else { return Err("select de punteros sin forma conocida".into()) };
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
                let TV::Ptr(base, off) = self.tv(*ptr)? else { return Err("load de algo que no es memoria".into()) };
                TV::Id(self.cargar(base, off, ty)?)
            }
            Inst::Store { ptr, val, .. } => {
                let TV::Ptr(base, off) = self.tv(*ptr)? else { return Err("store en algo que no es memoria".into()) };
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
                if wt == 16 { self.b.quantize_to_f16(st, None, x).unwrap() } else { x }
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
                let ids: Vec<Word> = a.iter().map(|&v| self.id(v)).collect::<Res<_>>()?;
                let op = if real { 43 } else if signo { 45 } else { 44 };
                self.ext(st, op, &ids)
            }
            "min" | "max" if !real => {
                let ids: Vec<Word> = a.iter().map(|&v| self.id(v)).collect::<Res<_>>()?;
                let op = match (base, signo) { ("min", true) => 39, ("min", false) => 38, ("max", true) => 42, _ => 41 };
                self.ext(st, op, &ids)
            }
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
            b if b.starts_with("sample_texture") || b.starts_with("read_texture") || b.starts_with("write_texture") || b.starts_with("get_width") || b.starts_with("get_height") || b.starts_with("get_num_mip") => {
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
    fn var_textura(&mut self, idx: u32, escritura: bool, dim: Dim, arreglo: bool, muestra: Word) -> Word {
        if let Some(&(v, _)) = self.texturas.get(&(idx, escritura)) {
            return v;
        }
        let it = self.b.type_image(muestra, dim, 0, arreglo as u32, 0, if escritura { 2 } else { 1 }, ImageFormat::Unknown, None);
        let pt = self.ptr_t(StorageClass::UniformConstant, it);
        let v = self.var_global(pt, StorageClass::UniformConstant, None);
        self.b.decorate(v, Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
        let binding = BIND_TEXTURA + idx + if escritura { 16 } else { 0 };
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
        self.b.decorate(v, Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
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

    fn textura(&mut self, base: &str, p: &[&str], a: &[ValueId], ty: TypeId) -> Res<Option<TV>> {
        let dim_txt = base.split('_').skip(1).collect::<Vec<_>>().join("_"); // texture_2d, texture_2d_array...
        let (dim, arreglo) = if dim_txt.contains("3d") {
            (Dim::Dim3D, false)
        } else if dim_txt.contains("cube") {
            (Dim::DimCube, dim_txt.contains("array"))
        } else {
            (Dim::Dim2D, dim_txt.contains("array"))
        };
        // tipo de muestra: del valor de vuelta ({<4 x T>, i8} o void)
        let (muestra, vec4) = {
            let tipo_v = match &self.m.types[ty] {
                Type::Struct { elems, .. } => elems[0],
                _ => ty,
            };
            // del tipo del argumento (texture2d<uint>...) y, si no se sabe, del nombre de la función (.u., .s.)
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
            let _ = tipo_v;
            (w, self.vec_t(w, 4))
        };
        let tex_idx = match self.tv(a[0])? {
            TV::Tex(i) => i,
            TV::Ptr(Base::Nula, _) => return Err("textura nula".into()),
            o => return Err(format!("textura desconocida: {o:?}")),
        };
        let res_ty = |tr: &mut Self, texel: Word| -> Res<TV> {
            // {<4 x T>, i8}: el texel y la residencia (siempre presente)
            if let Type::Struct { .. } = tr.m.types[ty] {
                let st = tr.ty(ty)?;
                let cero = tr.cu(0);
                Ok(TV::Id(tr.b.composite_construct(st, None, [texel, cero]).unwrap()))
            } else {
                Ok(TV::Id(texel))
            }
        };
        if base.starts_with("write_texture") {
            let v = self.var_textura(tex_idx, true, dim, arreglo, muestra);
            let (_, it) = self.texturas[&(tex_idx, true)];
            let img = self.b.load(it, None, v, None, []).unwrap();
            let coord = self.id(a[1])?;
            let texel = self.id(a[2])?;
            self.b.image_write(img, coord, texel, None, []).unwrap();
            return Ok(None);
        }
        let v = self.var_textura(tex_idx, false, dim, arreglo, muestra);
        let (_, it) = self.texturas[&(tex_idx, false)];
        let img = self.b.load(it, None, v, None, []).unwrap();
        if base.starts_with("get_width") || base.starts_with("get_height") {
            let n = match dim { Dim::Dim3D => 3, _ => 2 + arreglo as u32 };
            let vt = self.vec_t(self.t_u32, n);
            let lod = if a.len() > 1 { self.id(a[1])? } else { self.cu(0) };
            let sz = self.b.image_query_size_lod(vt, None, img, lod).unwrap();
            let k = if base.starts_with("get_width") { 0 } else { 1 };
            return Ok(Some(TV::Id(self.b.composite_extract(self.t_u32, None, sz, [k]).unwrap())));
        }
        if base.starts_with("read_texture") {
            // (textura, sampler, coord, desplazamiento, lod, acceso)
            let coord = self.id(a[2])?;
            let lod = self.id(a[4])?;
            let texel = self.b.image_fetch(vec4, None, img, coord, Some(ImageOperands::LOD), [Operand::IdRef(lod)]).unwrap();
            return res_ty(self, texel).map(Some);
        }
        // sample: (textura, sampler, coord, ¿desplazamiento?, desplazamiento, nivel explícito, nivel o sesgo, lod mínimo, ...)
        let smp = match self.tv(a[1])? {
            TV::Smp(s) => s,
            TV::Ptr(Base::Glob(g), _) => Smp::Fijo(g),
            o => return Err(format!("sampler desconocido: {o:?}")),
        };
        let sv = self.var_sampler(smp)?;
        let s = self.b.load(self.t_sampler, None, sv, None, []).unwrap();
        let sit = self.b.type_sampled_image(it);
        let si = self.b.sampled_image(sit, None, img, s).unwrap();
        let coord = self.id(a[2])?;
        let explicito = matches!(self.tv(a[5])?, TV::C(V::Int(_, 1)));
        let nivel = self.tv(a[6])?;
        let desplazamiento = match self.tv(a[4])? {
            TV::C(V::Vec(es)) if es.iter().any(|e| e.int().unwrap_or(0) != 0) => Some(self.id(a[4])?),
            _ => None,
        };
        let fragment = self.ent.etapa == Etapa::Fragment;
        let mut ops = Vec::new();
        let mut flags = ImageOperands::NONE;
        let texel = if explicito || !fragment {
            flags |= ImageOperands::LOD;
            let l = if explicito { self.id_de(&nivel, self.f.values[a[6]].ty)? } else { self.cf(0.0) };
            ops.push(Operand::IdRef(l));
            if let Some(d) = desplazamiento {
                flags |= ImageOperands::CONST_OFFSET;
                ops.push(Operand::IdRef(d));
            }
            self.b.image_sample_explicit_lod(vec4, None, si, coord, flags, ops).unwrap()
        } else {
            if !matches!(nivel, TV::C(V::Real(_, x)) if x == 0.0) {
                flags |= ImageOperands::BIAS;
                let l = self.id_de(&nivel, self.f.values[a[6]].ty)?;
                ops.push(Operand::IdRef(l));
            }
            if let Some(d) = desplazamiento {
                flags |= ImageOperands::CONST_OFFSET;
                ops.push(Operand::IdRef(d));
            }
            self.b.image_sample_implicit_lod(vec4, None, si, coord, if flags.is_empty() { None } else { Some(flags) }, ops).unwrap()
        };
        res_ty(self, texel).map(Some)
    }
}
