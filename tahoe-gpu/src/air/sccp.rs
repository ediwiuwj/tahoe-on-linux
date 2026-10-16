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

/// Retículo de cada valor: aún sin saber (todavía no se ha evaluado con entradas vivas), constante, o variable.
#[derive(Clone, PartialEq)]
enum Lat {
    Arriba,
    C(V),
    Abajo,
}

fn encuentro(a: &Lat, b: &Lat) -> Lat {
    match (a, b) {
        (Lat::Arriba, x) | (x, Lat::Arriba) => x.clone(),
        (Lat::C(x), Lat::C(y)) if x == y => Lat::C(x.clone()),
        _ => Lat::Abajo,
    }
}

/// `memoria`: la memoria de las globales tras especializar (las del espacio constant se pliegan al leerlas);
/// `definidas`: globales de function constants con valor.
pub fn analizar(m: &Module, f: &Function, memoria: &Memoria, definidas: &[usize]) -> Analisis {
    let n = f.values.len();
    let mut c: Vec<Lat> = vec![Lat::Arriba; n];
    for (i, v) in f.values.iter().enumerate() {
        c[i] = match &v.kind {
            ValueKind::Const(k) => interp::constante(m, &f.values, v.ty, k).map(Lat::C).unwrap_or(Lat::Abajo),
            ValueKind::Global(g) => Lat::C(V::Ptr(Mem::Global(*g), 0)),
            ValueKind::Function(g) => Lat::C(V::Ptr(Mem::Function(*g), 0)),
            ValueKind::Inst(..) => Lat::Arriba,
            _ => Lat::Abajo,
        };
    }
    let nb = f.blocks.len();
    let mut alc = vec![false; nb];
    let mut aristas: HashSet<(usize, usize)> = HashSet::new();
    if nb > 0 {
        alc[0] = true;
    }
    // cada valor solo baja (arriba → constante → abajo), así que el bucle termina
    for _vuelta in 0..100_000 {
        let mut cambio = false;
        for b in 0..nb {
            if !alc[b] {
                continue;
            }
            for ins in &f.blocks[b].insts {
                let (r, dests) = {
                    let lat = |v: ValueId| c.get(v).cloned().unwrap_or(Lat::Abajo);
                    let get = |v: ValueId| match c.get(v) { Some(Lat::C(x)) => Some(x.clone()), _ => None };
                    // estado de los operandos: alguno abajo → abajo; alguno arriba → arriba; si no, todos constantes
                    let ops = |vs: &[ValueId]| -> Option<Lat> {
                        let ls: Vec<Lat> = vs.iter().map(|&v| lat(v)).collect();
                        if ls.iter().any(|l| *l == Lat::Abajo) {
                            Some(Lat::Abajo)
                        } else if ls.iter().any(|l| *l == Lat::Arriba) {
                            Some(Lat::Arriba)
                        } else {
                            None
                        }
                    };
                    let plegado = |x: Option<V>| x.map(Lat::C).unwrap_or(Lat::Abajo);
                    let r: Lat = match &ins.inst {
                        Inst::Phi { incoming } => incoming.iter().filter(|(_, pb)| aristas.contains(&(*pb, b))).fold(Lat::Arriba, |acc, (v, _)| encuentro(&acc, &lat(*v))),
                        Inst::Load { ptr, .. } => match lat(*ptr) {
                            Lat::C(V::Ptr(Mem::Global(g), off)) if m.globals[g].addrspace == 2 => {
                                let mut mm = Memoria { globales: memoria.globales.clone(), allocas: Vec::new(), externas: Vec::new() };
                                plegado(mm.cargar(m, &V::Ptr(Mem::Global(g), off), ins.ty).ok())
                            }
                            Lat::Arriba => Lat::Arriba,
                            _ => Lat::Abajo,
                        },
                        Inst::Gep { src_ty, ptr, idx, .. } => {
                            let mut todos = vec![*ptr];
                            todos.extend(idx.iter().copied());
                            match ops(&todos) {
                                Some(l) => l,
                                None => match get(*ptr) {
                                    Some(b @ V::Ptr(Mem::Global(_), _)) => {
                                        let ix: Vec<V> = idx.iter().map(|&i| get(i).unwrap()).collect();
                                        plegado(interp::gep_pub(m, *src_ty, &b, &ix))
                                    }
                                    _ => Lat::Abajo,
                                },
                            }
                        }
                        Inst::Cast { v, .. } if matches!(lat(*v), Lat::C(V::Ptr(..))) => lat(*v),
                        Inst::Call { callee, args, .. } => {
                            let nombre = match f.values.get(*callee).map(|v| &v.kind) {
                                Some(ValueKind::Function(g)) => m.functions[*g].name.as_str(),
                                _ => "",
                            };
                            if nombre == "air.is_function_constant_defined" {
                                match args.first().map(|&a| lat(a)) {
                                    Some(Lat::C(V::Ptr(Mem::Global(g), _))) => Lat::C(V::Int(1, definidas.contains(&g) as u64)),
                                    Some(Lat::Arriba) => Lat::Arriba,
                                    _ => Lat::Abajo,
                                }
                            } else if nombre.starts_with("air.normalize_function_constant_predicate") {
                                match args.first().map(|&a| lat(a)) {
                                    Some(Lat::C(V::Int(w, x))) => Lat::C(V::Int(w, (x != 0) as u64)),
                                    Some(Lat::Arriba) => Lat::Arriba,
                                    _ => Lat::Abajo,
                                }
                            } else {
                                Lat::Abajo
                            }
                        }
                        Inst::Alloca { .. } | Inst::AtomicRmw { .. } | Inst::CmpXchg { .. } => Lat::Abajo,
                        other => {
                            let usados = operandos(other);
                            match ops(&usados) {
                                Some(l) => l,
                                None => plegado(interp::plegar(m, f, other, ins.ty, &get)),
                            }
                        }
                    };
                    // aristas vivas desde el terminador (con la condición aún sin saber, ninguna todavía)
                    let dests: Vec<usize> = match &ins.inst {
                        Inst::Br(t) => vec![*t],
                        Inst::CondBr { c: cv, t, f: fl } => match lat(*cv) {
                            Lat::C(x) => match x.int() {
                                Some(1) => vec![*t],
                                Some(0) => vec![*fl],
                                _ => vec![*t, *fl],
                            },
                            Lat::Arriba => Vec::new(),
                            Lat::Abajo => vec![*t, *fl],
                        },
                        Inst::Switch { v, default, cases } => match lat(*v) {
                            Lat::C(x) if x.int().is_some() => {
                                let x = x.int();
                                vec![cases.iter().find(|(cv, _)| get(*cv).and_then(|y| y.int()) == x).map(|(_, b)| *b).unwrap_or(*default)]
                            }
                            Lat::Arriba => Vec::new(),
                            _ => {
                                let mut d = vec![*default];
                                d.extend(cases.iter().map(|(_, b)| *b));
                                d
                            }
                        },
                        _ => Vec::new(),
                    };
                    (r, dests)
                };
                if let Some(k) = ins.val {
                    let nuevo = encuentro_bajada(&c[k], &r);
                    if nuevo != c[k] {
                        c[k] = nuevo;
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
    let c: Vec<Option<V>> = c.into_iter().map(|l| match l { Lat::C(x) => Some(x), _ => None }).collect();
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

/// El valor solo puede bajar: si el nuevo cálculo está más arriba que lo que ya se sabía, se queda lo anterior.
fn encuentro_bajada(viejo: &Lat, nuevo: &Lat) -> Lat {
    match (viejo, nuevo) {
        (Lat::Abajo, _) | (_, Lat::Abajo) => Lat::Abajo,
        (Lat::Arriba, x) => x.clone(),
        (x, Lat::Arriba) => x.clone(),
        (Lat::C(a), Lat::C(b)) => if a == b { Lat::C(a.clone()) } else { Lat::Abajo },
    }
}

/// Valores que usa una instrucción (para saber si se puede plegar).
fn operandos(i: &Inst) -> Vec<ValueId> {
    match i {
        Inst::Bin { a, b, .. } | Inst::Cmp { a, b, .. } => vec![*a, *b],
        Inst::FNeg(a) | Inst::Freeze(a) => vec![*a],
        Inst::Cast { v, .. } => vec![*v],
        Inst::Select { c, t, f } => vec![*c, *t, *f],
        Inst::ExtractElt { v, i } => vec![*v, *i],
        Inst::InsertElt { v, e, i } => vec![*v, *e, *i],
        Inst::Shuffle { a, b, mask } => vec![*a, *b, *mask],
        Inst::ExtractVal { v, .. } => vec![*v],
        Inst::InsertVal { agg, v, .. } => vec![*agg, *v],
        _ => Vec::new(),
    }
}
