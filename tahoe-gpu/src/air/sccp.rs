//! Propagación de constantes con bloques alcanzables (SCCP sencillo) antes de traducir: qué valores son constantes
//! (incluidas las lecturas de la memoria constante tras especializar las function constants) y qué bloques se
//! alcanzan. Así el código de las variantes que no aplican (p. ej. el de los imageblocks de las GPU de Apple) ni se
//! traduce.

use std::collections::HashSet;

use super::interp::{self, Memoria, Mem, V};
use super::ir::{Function, Inst, Module, ValueId, ValueKind};

pub struct Analisis {
    /// Valor constante de cada número de valor (si lo es).
    pub constantes: Vec<Option<V>>,
    pub alcanzables: Vec<bool>,
    /// Orden de emisión: postorden inverso desde la entrada (cada bloque tras los que lo dominan).
    pub orden: Vec<usize>,
}

/// `memoria`: la memoria de las globales tras especializar (las del espacio constant se pliegan al leerlas);
/// `definidas`: globales de function constants con valor.
pub fn analizar(m: &Module, f: &Function, memoria: &Memoria, definidas: &[usize]) -> Analisis {
    let n = f.values.len();
    let mut c: Vec<Option<V>> = vec![None; n];
    for (i, v) in f.values.iter().enumerate() {
        c[i] = match &v.kind {
            ValueKind::Const(k) => interp::constante(m, &f.values, v.ty, k),
            ValueKind::Global(g) => Some(V::Ptr(Mem::Global(*g), 0)),
            ValueKind::Function(g) => Some(V::Ptr(Mem::Function(*g), 0)),
            _ => None,
        };
    }
    let nb = f.blocks.len();
    let mut alc = vec![false; nb];
    let mut aristas: HashSet<(usize, usize)> = HashSet::new();
    if nb > 0 {
        alc[0] = true;
    }
    for _vuelta in 0..64 {
        let mut cambio = false;
        for b in 0..nb {
            if !alc[b] {
                continue;
            }
            for ins in &f.blocks[b].insts {
                let (r, dests) = {
                let get = |v: ValueId| c.get(v).cloned().flatten();
                let r: Option<V> = match &ins.inst {
                    Inst::Phi { incoming } => {
                        let vivos: Vec<Option<V>> = incoming.iter().filter(|(_, pb)| aristas.contains(&(*pb, b))).map(|(v, _)| get(*v)).collect();
                        match vivos.first() {
                            Some(Some(x)) if vivos.iter().all(|y| y.as_ref() == Some(x)) => Some(x.clone()),
                            _ => None,
                        }
                    }
                    Inst::Load { ptr, .. } => match get(*ptr) {
                        Some(V::Ptr(Mem::Global(g), off)) if m.globals[g].addrspace == 2 => {
                            let mut mm = Memoria { globales: memoria.globales.clone(), allocas: Vec::new(), externas: Vec::new() };
                            mm.cargar(m, &V::Ptr(Mem::Global(g), off), ins.ty).ok()
                        }
                        _ => None,
                    },
                    Inst::Gep { src_ty, ptr, idx, .. } => {
                        let base = get(*ptr);
                        let ix: Option<Vec<V>> = idx.iter().map(|&i| get(i)).collect();
                        match (base, ix) {
                            (Some(b @ V::Ptr(Mem::Global(_), _)), Some(ix)) => interp::gep_pub(m, *src_ty, &b, &ix),
                            _ => None,
                        }
                    }
                    Inst::Cast { v, .. } if matches!(get(*v), Some(V::Ptr(..))) => get(*v),
                    Inst::Call { callee, args, .. } => {
                        let nombre = match f.values.get(*callee).map(|v| &v.kind) {
                            Some(ValueKind::Function(g)) => m.functions[*g].name.as_str(),
                            _ => "",
                        };
                        if nombre == "air.is_function_constant_defined" {
                            match args.first().and_then(|&a| get(a)) {
                                Some(V::Ptr(Mem::Global(g), _)) => Some(V::Int(1, definidas.contains(&g) as u64)),
                                _ => None,
                            }
                        } else if nombre.starts_with("air.normalize_function_constant_predicate") {
                            match args.first().and_then(|&a| get(a)) {
                                Some(V::Int(w, x)) => Some(V::Int(w, (x != 0) as u64)),
                                _ => None,
                            }
                        } else {
                            None
                        }
                    }
                    other => interp::plegar(m, f, other, ins.ty, &get),
                };
                // aristas vivas desde el terminador
                let dests: Vec<usize> = match &ins.inst {
                    Inst::Br(t) => vec![*t],
                    Inst::CondBr { c: cv, t, f: fl } => match get(*cv).and_then(|x| x.int()) {
                        Some(1) => vec![*t],
                        Some(0) => vec![*fl],
                        _ => vec![*t, *fl],
                    },
                    Inst::Switch { v, default, cases } => match get(*v).and_then(|x| x.int()) {
                        Some(x) => vec![cases.iter().find(|(cv, _)| get(*cv).and_then(|y| y.int()) == Some(x)).map(|(_, b)| *b).unwrap_or(*default)],
                        None => {
                            let mut d = vec![*default];
                            d.extend(cases.iter().map(|(_, b)| *b));
                            d
                        }
                    },
                    _ => Vec::new(),
                };
                (r, dests)
                };
                if let (Some(k), Some(x)) = (ins.val, r) {
                    if c[k].is_none() {
                        c[k] = Some(x);
                        cambio = true;
                    }
                }
                for d in dests {
                    if aristas.insert((b, d)) {
                        cambio = true;
                    }
                    if !alc[d] {
                        alc[d] = true;
                        cambio = true;
                    }
                }
            }
        }
        if !cambio {
            break;
        }
    }
    // orden: postorden inverso por las aristas vivas
    let mut visto = vec![false; nb];
    let mut post = Vec::new();
    let mut pila: Vec<(usize, usize)> = vec![(0, 0)];
    if nb > 0 {
        visto[0] = true;
    }
    while let Some((b, k)) = pila.pop() {
        let suc: Vec<usize> = aristas.iter().filter(|(x, _)| *x == b).map(|(_, y)| *y).collect::<Vec<_>>();
        let mut suc = suc;
        suc.sort();
        if k < suc.len() {
            pila.push((b, k + 1));
            let s = suc[k];
            if !visto[s] {
                visto[s] = true;
                pila.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    Analisis { constantes: c, alcanzables: alc, orden: post }
}
