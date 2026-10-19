//! Inlining en el IR: las llamadas a funciones definidas en el módulo (las internas `fastcc` que deja el compilador
//! de Metal) se sustituyen por su cuerpo. Así el traductor a SPIR-V solo ve una función, con todos sus punteros
//! locales a la vista (un puntero de Metal se traduce como «buffer + desplazamiento» y no puede pasar de una función
//! de SPIR-V a otra).

use super::ir::{BasicBlock, Function, Inst, Instr, Module, Value, ValueId, ValueKind};

pub type Res<T> = Result<T, String>;

/// Devuelve una copia de la función con todas las llamadas a funciones del módulo sustituidas (en profundidad).
pub fn inline_todo(m: &Module, fi: usize) -> Res<Function> {
    let mut f = m.functions[fi].clone();
    let mut vueltas = 0;
    loop {
        let pos = f.blocks.iter().enumerate().find_map(|(b, blk)| {
            blk.insts.iter().position(|i| es_llamada_interna(m, &f, i)).map(|k| (b, k))
        });
        let Some((b, k)) = pos else { return Ok(f) };
        vueltas += 1;
        if vueltas > 1000 {
            return Err("demasiadas llamadas internas (¿recursión?)".into());
        }
        insertar(m, &mut f, b, k)?;
    }
}

fn es_llamada_interna(m: &Module, f: &Function, i: &Instr) -> bool {
    if let Inst::Call { callee, .. } = &i.inst {
        if let Some(ValueKind::Function(c)) = f.values.get(*callee).map(|v| &v.kind) {
            return !m.functions[*c].is_proto;
        }
    }
    false
}

/// Sustituye la llamada en (bloque b, instrucción k).
fn insertar(m: &Module, f: &mut Function, b: usize, k: usize) -> Res<()> {
    let llamada = f.blocks[b].insts[k].clone();
    let Inst::Call { callee, args, .. } = &llamada.inst else { unreachable!() };
    let ValueKind::Function(ci) = f.values[*callee].kind else { unreachable!() };
    let callee_f = &m.functions[ci];
    if callee_f.blocks.is_empty() {
        return Err(format!("{}: sin cuerpo", callee_f.name));
    }
    // 1) partir el bloque: [0..k) se queda en b; (k..] pasa a un bloque nuevo `cont`
    let resto: Vec<Instr> = f.blocks[b].insts.split_off(k + 1);
    f.blocks[b].insts.pop(); // la llamada
    let cont = f.blocks.len();
    f.blocks.push(BasicBlock { insts: resto });
    // los sucesores de b ahora lo son de cont: sus phis deben nombrar a cont
    for s in sucesores(&f.blocks[cont]) {
        for ins in &mut f.blocks[s].insts {
            if let Inst::Phi { incoming } = &mut ins.inst {
                for (_, pb) in incoming.iter_mut() {
                    if *pb == b {
                        *pb = cont;
                    }
                }
            }
        }
    }
    // las instrucciones movidas a cont cambian de bloque en la tabla de valores
    for (pos, ins) in f.blocks[cont].insts.iter().enumerate() {
        if let Some(v) = ins.val {
            f.values[v].kind = ValueKind::Inst(cont, pos);
        }
    }
    // 2) copiar los valores del llamado: los globales y constantes del módulo coinciden (mismo prefijo de la tabla);
    //    los argumentos se sustituyen por los de la llamada; constantes locales e instrucciones, nuevos números
    let n_mod = m.values.len();
    let mut mapa: Vec<ValueId> = Vec::with_capacity(callee_f.values.len());
    let base_bloque = f.blocks.len();
    for (v, val) in callee_f.values.iter().enumerate() {
        if v < n_mod {
            mapa.push(v);
            continue;
        }
        match &val.kind {
            ValueKind::Arg(i) => mapa.push(*args.get(*i).ok_or("faltan argumentos en la llamada")?),
            ValueKind::Inst(bb, pos) => {
                mapa.push(f.values.len());
                f.values.push(Value { ty: val.ty, kind: ValueKind::Inst(base_bloque + bb, *pos) });
            }
            _ => {
                mapa.push(f.values.len());
                f.values.push(val.clone());
            }
        }
    }
    // las constantes copiadas que nombran otras constantes locales (agregados): a los números nuevos
    for (v, _) in callee_f.values.iter().enumerate().skip(n_mod) {
        let nv = mapa[v];
        if let ValueKind::Const(super::ir::Const::Aggregate(ids)) = &f.values[nv].kind {
            let ids2: Vec<ValueId> = ids.iter().map(|&i| mapa.get(i).copied().unwrap_or(i)).collect();
            f.values[nv].kind = ValueKind::Const(super::ir::Const::Aggregate(ids2));
        }
    }
    let mv = |v: ValueId| -> ValueId { if v >= 1_000_000_000 { v } else { mapa.get(v).copied().unwrap_or(v) } };
    let mb = |x: usize| base_bloque + x;
    // 3) copiar los bloques del llamado; cada ret salta a cont y aporta su valor al phi del resultado
    let mut rets: Vec<(Option<ValueId>, usize)> = Vec::new();
    for (bi, blk) in callee_f.blocks.iter().enumerate() {
        let mut nb = BasicBlock::default();
        for ins in &blk.insts {
            let val = ins.val.map(mv);
            let inst = match &ins.inst {
                Inst::Ret(v) => {
                    rets.push((v.map(mv), mb(bi)));
                    Inst::Br(cont)
                }
                other => remapear(other, &mv, &mb),
            };
            nb.insts.push(Instr { inst, ty: ins.ty, val });
        }
        f.blocks.push(nb);
    }
    // 4) b salta a la entrada del llamado
    f.blocks[b].insts.push(Instr { inst: Inst::Br(base_bloque), ty: llamada.ty, val: None });
    // 5) el resultado: un phi al principio de cont con los valores de los ret
    if let Some(res) = llamada.val {
        let inc: Vec<(ValueId, usize)> = rets.iter().filter_map(|(v, bb)| v.map(|v| (v, *bb))).collect();
        if inc.len() == 1 {
            // un solo ret: el resultado es ese valor; se sustituye en todos los usos
            sustituir(f, res, inc[0].0);
        } else {
            f.blocks[cont].insts.insert(0, Instr { inst: Inst::Phi { incoming: inc }, ty: llamada.ty, val: Some(res) });
            for (pos, ins) in f.blocks[cont].insts.iter().enumerate() {
                if let Some(v) = ins.val {
                    f.values[v].kind = ValueKind::Inst(cont, pos);
                }
            }
        }
    }
    Ok(())
}

pub fn sucesores(b: &BasicBlock) -> Vec<usize> {
    match b.insts.last().map(|i| &i.inst) {
        Some(Inst::Br(t)) => vec![*t],
        Some(Inst::CondBr { t, f, .. }) => vec![*t, *f],
        Some(Inst::Switch { default, cases, .. }) => {
            let mut v = vec![*default];
            v.extend(cases.iter().map(|(_, b)| *b));
            v
        }
        _ => Vec::new(),
    }
}

/// Cambia todos los usos del valor `de` por `a`.
fn sustituir(f: &mut Function, de: ValueId, a: ValueId) {
    let mv = |v: ValueId| if v == de { a } else { v };
    let mb = |x: usize| x;
    for blk in &mut f.blocks {
        for ins in &mut blk.insts {
            ins.inst = remapear(&ins.inst, &mv, &mb);
        }
    }
}

/// Copia de la instrucción con los valores y bloques renombrados.
pub fn remapear(i: &Inst, v: &dyn Fn(ValueId) -> ValueId, b: &dyn Fn(usize) -> usize) -> Inst {
    match i {
        Inst::Bin { op, a, b: x } => Inst::Bin { op: *op, a: v(*a), b: v(*x) },
        Inst::FNeg(a) => Inst::FNeg(v(*a)),
        Inst::Cast { op, v: x } => Inst::Cast { op: *op, v: v(*x) },
        Inst::Cmp { pred, a, b: x } => Inst::Cmp { pred: *pred, a: v(*a), b: v(*x) },
        Inst::Select { c, t, f } => Inst::Select { c: v(*c), t: v(*t), f: v(*f) },
        Inst::ExtractElt { v: x, i } => Inst::ExtractElt { v: v(*x), i: v(*i) },
        Inst::InsertElt { v: x, e, i } => Inst::InsertElt { v: v(*x), e: v(*e), i: v(*i) },
        Inst::Shuffle { a, b: x, mask } => Inst::Shuffle { a: v(*a), b: v(*x), mask: v(*mask) },
        Inst::ExtractVal { v: x, idx } => Inst::ExtractVal { v: v(*x), idx: idx.clone() },
        Inst::InsertVal { agg, v: x, idx } => Inst::InsertVal { agg: v(*agg), v: v(*x), idx: idx.clone() },
        Inst::Load { ptr, align } => Inst::Load { ptr: v(*ptr), align: *align },
        Inst::Store { ptr, val, align } => Inst::Store { ptr: v(*ptr), val: v(*val), align: *align },
        Inst::Gep { src_ty, ptr, idx, inbounds } => Inst::Gep { src_ty: *src_ty, ptr: v(*ptr), idx: idx.iter().map(|&x| v(x)).collect(), inbounds: *inbounds },
        Inst::Alloca { ty, n, align } => Inst::Alloca { ty: *ty, n: v(*n), align: *align },
        Inst::Call { fnty, callee, args } => Inst::Call { fnty: *fnty, callee: v(*callee), args: args.iter().map(|&x| v(x)).collect() },
        Inst::Phi { incoming } => Inst::Phi { incoming: incoming.iter().map(|&(x, bb)| (v(x), b(bb))).collect() },
        Inst::Br(t) => Inst::Br(b(*t)),
        Inst::CondBr { c, t, f } => Inst::CondBr { c: v(*c), t: b(*t), f: b(*f) },
        Inst::Switch { v: x, default, cases } => Inst::Switch { v: v(*x), default: b(*default), cases: cases.iter().map(|&(c, bb)| (v(c), b(bb))).collect() },
        Inst::Ret(x) => Inst::Ret(x.map(v)),
        Inst::Unreachable => Inst::Unreachable,
        Inst::Freeze(x) => Inst::Freeze(v(*x)),
        Inst::AtomicRmw { op, ptr, val } => Inst::AtomicRmw { op: *op, ptr: v(*ptr), val: v(*val) },
        Inst::CmpXchg { ptr, cmp, new } => Inst::CmpXchg { ptr: v(*ptr), cmp: v(*cmp), new: v(*new) },
        Inst::Fence => Inst::Fence,
    }
}
