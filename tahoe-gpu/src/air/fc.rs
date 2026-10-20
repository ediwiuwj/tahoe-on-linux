//! Function constants de Metal. En AIR son globales del espacio constant (`@..MTL_FC_INIT_<índice>_<tipo>`) listadas en
//! `!air.function_constants` como `!{ptr @global, !"tipo", !"nombre", i32 índice, i1 obligatoria}`. Además, un
//! constructor global (en `@llvm.global_ctors`) calcula a partir de ellas otras globales (los predicados
//! `__metal_implicit_fc_pred_N` que deciden qué argumentos y salidas existen, y constantes derivadas). Al crear un
//! pipeline se especializa: se escriben los valores dados, se ejecuta el constructor con el intérprete y la memoria
//! resultante hace de constante en la traducción (las ramas que dependen de ella se pliegan).

use std::collections::HashMap;

use super::interp::{self, Memoria, Mem, V};
use super::ir::{Const, Md, Module, Type, ValueKind};

pub type Res<T> = Result<T, String>;

#[derive(Debug, Clone)]
pub struct FuncConst {
    pub global: usize,
    pub tipo: String,
    pub nombre: String,
    pub indice: u32,
}

fn md_global(m: &Module, i: Option<usize>) -> Option<usize> {
    match m.md.get(i?)? {
        Md::Value(_, v) => match &m.values.get(*v)?.kind {
            ValueKind::Global(g) => Some(*g),
            _ => None,
        },
        _ => None,
    }
}

fn md_int(m: &Module, i: Option<usize>) -> Option<i64> {
    match m.md.get(i?)? {
        Md::Value(_, v) => match &m.values.get(*v)?.kind {
            ValueKind::Const(Const::Int(n)) => Some(*n as i64),
            ValueKind::Const(Const::Null) => Some(0),
            _ => None,
        },
        _ => None,
    }
}

/// Las function constants declaradas en el módulo.
pub fn constantes(m: &Module) -> Vec<FuncConst> {
    let Some(nodos) = m.named_md.get("air.function_constants") else { return Vec::new() };
    nodos
        .iter()
        .filter_map(|&n| {
            let o = m.md_node(n)?;
            Some(FuncConst {
                global: md_global(m, o.first().copied().flatten())?,
                tipo: o.get(1).copied().flatten().and_then(|i| m.md_str(i)).unwrap_or("").to_string(),
                nombre: o.get(2).copied().flatten().and_then(|i| m.md_str(i)).unwrap_or("").to_string(),
                indice: md_int(m, o.get(3).copied().flatten())? as u32,
            })
        })
        .collect()
}

/// Resultado de especializar: la memoria de las globales tras el constructor y qué constantes se dieron.
pub struct Especializacion {
    pub memoria: Memoria,
    pub definidas: Vec<usize>,
}

/// Especializa con `valores` (índice de la function constant → valor). Las que no se den quedan sin definir
/// (air.is_function_constant_defined da falso).
/// Si el nombre que pone el cliente («_shader_state») es el de la constante del AIR
/// («RB::Shader::Constant::_shader_state»).
pub fn coincide(nombre_air: &str, nombre_cliente: &str) -> bool {
    if nombre_air == nombre_cliente {
        return true;
    }
    // El cliente escribe el nombre tal como aparece en el código; en el AIR viene cualificado.
    let corto = nombre_air.rsplit("::").next().unwrap_or(nombre_air);
    corto == nombre_cliente
}

/// Cuántos componentes tiene una constante de tipo vectorial («uint4» → 4, «float3» → 3).
fn componentes(tipo: &str) -> Option<usize> {
    let n = tipo.chars().last()?.to_digit(10)? as usize;
    if (1..=4).contains(&n) && tipo.len() > 1 {
        Some(n)
    } else {
        None
    }
}

/// El valor de una constante, tal y como lo da el cliente. Una constante vectorial (uint4, float4...) llega
/// como varios escalares consecutivos (el cliente los pone con setConstantValues:type:withRange:) y aquí se reunen.
fn valor_de(fc: &FuncConst, valores: &HashMap<u32, V>) -> Res<Option<V>> {
    let Some(v) = valores.get(&fc.indice) else { return Ok(None) };
    if matches!(v, V::Vec(_)) {
        return Ok(Some(v.clone()));
    }
    let Some(n) = componentes(&fc.tipo) else { return Ok(Some(v.clone())) };
    if n <= 1 {
        return Ok(Some(v.clone()));
    }
    let mut vs = Vec::with_capacity(n);
    for k in 0..n {
        match valores.get(&(fc.indice + k as u32)) {
            Some(x) => vs.push(x.clone()),
            None => return Err(format!("constante {}: faltan componentes", fc.nombre)),
        }
    }
    Ok(Some(V::Vec(vs)))
}

pub fn especializar(m: &Module, valores: &HashMap<u32, V>) -> Res<Especializacion> {
    let mut mem = Memoria::nueva(m);
    let mut definidas = Vec::new();
    for fc in constantes(m) {
        if let Some(v) = valor_de(&fc, valores)? {
            let ty = m.globals[fc.global].ty;
            // un bool de Metal es un i8 en memoria
            let v = match (&m.types[ty], v) {
                (Type::Int(8), V::Int(1, x)) => V::Int(8, x),
                (_, v) => v,
            };
            mem.guardar(m, &V::Ptr(Mem::Global(fc.global), 0), ty, &v).map_err(|e| format!("constante {}: {e}", fc.nombre))?;
            definidas.push(fc.global);
        }
    }
    // constructores globales: @llvm.global_ctors = [{ i32 prioridad, ptr función, ptr datos }]
    let ctores: Vec<usize> = m
        .globals
        .iter()
        .find(|g| g.name == "llvm.global_ctors")
        .and_then(|g| g.init)
        .and_then(|i| match &m.values.get(i)?.kind {
            ValueKind::Const(Const::Aggregate(es)) => Some(es.clone()),
            _ => None,
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| match &m.values.get(e)?.kind {
            ValueKind::Const(Const::Aggregate(campos)) => match &m.values.get(*campos.get(1)?)?.kind {
                ValueKind::Function(f) => Some(*f),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let def = definidas.clone();
    let mut externas = move |nombre: &str, args: &[V]| -> Option<Option<V>> {
        match nombre {
            "air.is_function_constant_defined" => match args.first()? {
                V::Ptr(Mem::Global(g), _) => Some(Some(V::Int(1, def.contains(g) as u64))),
                _ => None,
            },
            // un bool de function constant normalizado a 0/1
            n if n.starts_with("air.normalize_function_constant_predicate") => match args.first()? {
                V::Int(w, x) => Some(Some(V::Int(*w, (*x != 0) as u64))),
                V::Undef => Some(Some(V::Int(8, 0))),
                _ => None,
            },
            n if n.starts_with("llvm.lifetime") || n.starts_with("llvm.dbg") => Some(None),
            _ => None,
        }
    };
    for f in ctores {
        let mut pasos = 0;
        interp::ejecutar(m, f, &[], &mut mem, &mut externas, &mut pasos).map_err(|e| format!("constructor {}: {e}", m.functions[f].name))?;
    }
    Ok(Especializacion { memoria: mem, definidas })
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn el_nombre_del_cliente_coincide_con_el_del_air() {
        assert!(coincide("RB::Shader::Constant::_shader_state", "_shader_state"));
        assert!(coincide("_shader_state", "_shader_state"));
        assert!(!coincide("RB::Shader::Constant::_shader_state", "otra_cosa"));
    }

    #[test]
    fn los_componentes_de_un_tipo_vectorial() {
        assert_eq!(componentes("uint4"), Some(4));
        assert_eq!(componentes("float3"), Some(3));
        assert_eq!(componentes("half2"), Some(2));
        assert_eq!(componentes("uint"), None);
    }

    #[test]
    fn una_constante_vectorial_se_reune_de_esc() {
        // El cliente pone cuatro escalares en los índices 0..3; la constante del AIR es un uint4 en el índice 0.
        let fc = FuncConst { global: 0, tipo: "uint4".into(), nombre: "_shader_state".into(), indice: 0 };
        let mut v = HashMap::new();
        v.insert(0u32, V::Int(32, 0));
        v.insert(1u32, V::Int(32, 0));
        v.insert(2u32, V::Int(32, 0x80000));
        v.insert(3u32, V::Int(32, 0));
        let valor = valor_de(&fc, &v).unwrap().unwrap();
        match valor {
            V::Vec(es) => {
                assert_eq!(es.len(), 4);
                assert!(matches!(es[2], V::Int(32, 0x80000)));
            }
            o => panic!("se esperaba un vector, se obtuvo {o:?}"),
        }
    }
}
