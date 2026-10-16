//! AIR (el formato intermedio de los shaders de Metal): un módulo LLVM (ver `ir`) cuyos metadatos con nombre
//! `!air.kernel`, `!air.vertex` y `!air.fragment` dicen qué funciones son puntos de entrada y qué es cada argumento:
//! `!{i32 índice, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", ...}`.

pub mod ir;

use ir::{Const, Md, Module, Type, ValueKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Etapa {
    Kernel,
    Vertex,
    Fragment,
}

/// Un elemento de la descripción de un argumento o de una salida: texto o entero.
#[derive(Debug, Clone, PartialEq)]
pub enum Dato {
    Txt(String),
    Int(i64),
    Otro,
}

#[derive(Debug, Clone)]
pub struct Descripcion {
    /// Índice del argumento (o de la salida).
    pub indice: i64,
    /// Lo que es: "air.buffer", "air.texture", "air.thread_position_in_grid", "air.position"...
    pub clase: String,
    pub datos: Vec<Dato>,
}

impl Descripcion {
    /// El valor que sigue a la etiqueta `clave` (p. ej. "air.location_index" → índice de binding).
    pub fn tras(&self, clave: &str) -> Option<&Dato> {
        let i = self.datos.iter().position(|d| *d == Dato::Txt(clave.to_string()))?;
        self.datos.get(i + 1)
    }
    pub fn int_tras(&self, clave: &str) -> Option<i64> {
        match self.tras(clave)? {
            Dato::Int(v) => Some(*v),
            _ => None,
        }
    }
    pub fn txt_tras(&self, clave: &str) -> Option<&str> {
        match self.tras(clave)? {
            Dato::Txt(s) => Some(s),
            _ => None,
        }
    }
    pub fn tiene(&self, clave: &str) -> bool {
        self.datos.iter().any(|d| *d == Dato::Txt(clave.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct Entrada {
    pub etapa: Etapa,
    /// Índice de la función en `Module::functions`.
    pub funcion: usize,
    pub argumentos: Vec<Descripcion>,
    pub salidas: Vec<Descripcion>,
}

fn dato(m: &Module, i: Option<usize>) -> Dato {
    let Some(i) = i else { return Dato::Otro };
    match m.md.get(i) {
        Some(Md::Str(s)) => Dato::Txt(s.clone()),
        Some(Md::Value(_, v)) => match m.values.get(*v).map(|x| &x.kind) {
            Some(ValueKind::Const(Const::Int(n))) => Dato::Int(*n as i64),
            Some(ValueKind::Const(Const::Null)) => Dato::Int(0),
            _ => Dato::Otro,
        },
        _ => Dato::Otro,
    }
}

fn descripciones(m: &Module, nodo: Option<usize>) -> Vec<Descripcion> {
    let Some(lista) = nodo.and_then(|n| m.md_node(n)) else { return Vec::new() };
    lista
        .iter()
        .filter_map(|&d| {
            let ops = m.md_node(d?)?;
            let datos: Vec<Dato> = ops.iter().map(|&o| dato(m, o)).collect();
            let indice = match datos.first() {
                Some(Dato::Int(v)) => *v,
                _ => -1,
            };
            let (clase, resto) = match (datos.first(), datos.get(1)) {
                (Some(Dato::Int(_)), Some(Dato::Txt(c))) => (c.clone(), datos[2..].to_vec()),
                (Some(Dato::Txt(c)), _) => (c.clone(), datos[1..].to_vec()),
                _ => (String::new(), datos.clone()),
            };
            Some(Descripcion { indice, clase, datos: resto })
        })
        .collect()
}

/// Los puntos de entrada del módulo, según `!air.kernel`, `!air.vertex` y `!air.fragment`.
pub fn entradas(m: &Module) -> Vec<Entrada> {
    let mut v = Vec::new();
    for (nombre, etapa) in [("air.kernel", Etapa::Kernel), ("air.vertex", Etapa::Vertex), ("air.fragment", Etapa::Fragment)] {
        let Some(nodos) = m.named_md.get(nombre) else { continue };
        for &n in nodos {
            let Some(ops) = m.md_node(n) else { continue };
            let funcion = match ops.first().copied().flatten().and_then(|i| m.md.get(i)) {
                Some(Md::Value(_, val)) => match m.values.get(*val).map(|x| &x.kind) {
                    Some(ValueKind::Function(f)) => *f,
                    _ => continue,
                },
                _ => continue,
            };
            // kernel: !{fn, !{}, !{args}}; vertex/fragment: !{fn, !{salidas}, !{args}}
            let salidas = descripciones(m, ops.get(1).copied().flatten());
            let argumentos = descripciones(m, ops.get(2).copied().flatten());
            v.push(Entrada { etapa, funcion, argumentos, salidas });
        }
    }
    v
}

/// Resumen de una función para `tahoe-gpu air`: bloques, instrucciones y llamadas a air.* y llvm.*.
pub fn resumen(m: &Module, f: usize) -> String {
    let fun = &m.functions[f];
    let n: usize = fun.blocks.iter().map(|b| b.insts.len()).sum();
    let mut llamadas: Vec<String> = Vec::new();
    for b in &fun.blocks {
        for i in &b.insts {
            if let ir::Inst::Call { callee, .. } = &i.inst {
                if let Some(ValueKind::Function(c)) = fun.values.get(*callee).map(|v| &v.kind) {
                    let nom = &m.functions[*c].name;
                    if !llamadas.contains(nom) {
                        llamadas.push(nom.clone());
                    }
                }
            }
        }
    }
    llamadas.sort();
    format!("{} bloques, {} instrucciones; llama a: {}", fun.blocks.len(), n, llamadas.join(" "))
}

/// Nombre legible de un tipo (para mensajes).
pub fn tipo(m: &Module, t: ir::TypeId) -> String {
    match m.types.get(t) {
        Some(Type::Int(w)) => format!("i{w}"),
        Some(Type::Float) => "float".into(),
        Some(Type::Half) => "half".into(),
        Some(Type::Double) => "double".into(),
        Some(Type::Void) => "void".into(),
        Some(Type::Ptr(a)) => format!("ptr({a})"),
        Some(Type::Vector(n, e)) => format!("<{n} x {}>", tipo(m, *e)),
        Some(Type::Array(n, e)) => format!("[{n} x {}]", tipo(m, *e)),
        Some(Type::Struct { elems, .. }) => format!("{{{}}}", elems.iter().map(|&e| tipo(m, e)).collect::<Vec<_>>().join(", ")),
        Some(t) => format!("{t:?}"),
        None => "?".into(),
    }
}
