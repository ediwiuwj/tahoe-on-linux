//! Traducción de AIR (Metal) a SPIR-V (Vulkan) para la GPU del PC.
//!
//! Decisiones, pensadas para GPUs como la Haswell (sin enteros de 64 ni 16 bits, sin half, sin
//! buffer_device_address):
//! - Tipos: i1 → bool; i8/i16/i32 → u32 (con extensión de ceros: tras cada operación se recortan los bits); i64 → los
//!   32 bits bajos (índices y desplazamientos); half y float → float de 32 bits (al bajar a half se redondea con
//!   OpQuantizeToF16 para dar el mismo resultado).
//! - Memoria: un puntero de Metal es «base + desplazamiento en bytes». Las bases son los buffers (un arreglo de
//!   palabras de 32 bits por binding), la memoria privada de cada alloca, la de threadgroup y los datos constantes del
//!   módulo. Cargas y guardados se componen de palabras (con desplazamientos para 8 y 16 bits y atómicos al guardar
//!   trozos de palabra en memoria compartida).
//! - Control de flujo: una función de un solo bloque se emite tal cual; con varios, como una máquina de estados (un
//!   bucle con un switch sobre el bloque siguiente), que siempre es SPIR-V estructurado válido. Los valores que pasan
//!   de un bloque a otro y los phi van por variables.
//! - Function constants: antes de traducir se pliegan (ver `fc` y `sccp`) y solo se emiten los bloques alcanzables.
//!
//! Recursos (conjunto 0): buffer N de Metal → binding N; textura N → 32 + N (160 + N si se escribe en ella); sampler
//! N → 192 + N; los samplers constantes del shader (constexpr) → 208 en adelante (con su estado en el enlace).

use std::collections::HashMap;

use rspirv::binary::Assemble;
use rspirv::dr::{Builder, Operand};
use rspirv::spirv::{self, BuiltIn, Capability, Decoration, Dim, ExecutionMode, ExecutionModel, FunctionControl, ImageFormat, ImageOperands, MemorySemantics, Scope, SelectionControl, StorageClass, Word};

use super::interp::{self, Memoria, Mem, V};
use super::ir::{BinOp, CastOp, Function, Inst, Module, Type, TypeId, ValueId, ValueKind};
use super::{fc, inline, layout, sccp, Descripcion, Entrada, Etapa};

pub type Res<T> = Result<T, String>;

pub const BIND_TEXTURA: u32 = 32;
pub const BIND_TEXTURA_ESCRITURA: u32 = 160;
pub const BIND_SAMPLER: u32 = 192;
pub const BIND_SAMPLER_FIJO: u32 = 208;

#[derive(Debug, Clone, PartialEq)]
pub enum Recurso {
    /// Buffer de Metal (StorageBuffer de palabras); `escritura` si el shader escribe en él.
    Buffer { indice: u32, escritura: bool },
    /// Textura que se muestrea o lee (imagen muestreada).
    Textura { indice: u32, dim: Dim, arreglo: bool },
    /// Textura en la que se escribe (imagen de almacenamiento).
    TexturaEscritura { indice: u32, dim: Dim, arreglo: bool },
    Sampler { indice: u32 },
    /// Sampler constante del shader: el estado de 64 bits de Metal (filtros, direcciones...).
    SamplerFijo { estado: [u64; 2] },
}

#[derive(Debug, Clone)]
pub struct Enlace {
    pub binding: u32,
    pub recurso: Recurso,
}

#[derive(Debug, Clone)]
pub struct Traduccion {
    pub spirv: Vec<u32>,
    pub etapa: Etapa,
    pub enlaces: Vec<Enlace>,
    /// Varyings (nombre de Metal → location) que escribe un vertex shader o lee un fragment shader.
    pub varyings: Vec<(String, u32)>,
    /// Atributos de vértice: (índice de atributo de Metal, location, tipo LLVM legible).
    pub atributos: Vec<(u32, u32, String)>,
    /// Salidas de color de un fragment shader: índice de render target.
    pub colores: Vec<u32>,
    pub bloques: usize,
    pub maquina_de_estados: bool,
    /// Usa operaciones de subgrupo (SIMD-group de Metal): el pipeline pide subgrupos de 32 como en Apple.
    pub subgrupos: bool,
}

#[derive(Default, Clone)]
pub struct Opciones {
    /// Valores de las function constants (índice → valor).
    pub constantes: HashMap<u32, V>,
    /// Locations de los varyings por nombre (las del vertex shader, para enlazar el fragment).
    pub varyings: HashMap<String, u32>,
    /// Descriptor set de los recursos (en un pipeline de render, uno por etapa: en Metal el buffer 0 del vertex shader
    /// y el del fragment shader son distintos).
    pub set: u32,
}

// ---------------------------------------------------------------- valores traducidos

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Base {
    Buf(u32),
    /// Memoria de threadgroup que llega como argumento (índice de Metal).
    WgArg(u32),
    Priv(ValueId),
    Glob(usize),
    Wg(usize),
    Nula,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Off {
    C(i64),
    Id(Word),
}

#[derive(Debug, Clone, PartialEq)]
enum TV {
    Id(Word),
    C(V),
    Ptr(Base, Off),
    Tex(u32),
    Smp(Smp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Smp {
    Arg(u32),
    Fijo(usize),
    Lectura,
}

struct Tr<'a> {
    m: &'a Module,
    f: &'a Function,
    an: &'a sccp::Analisis,
    mem: &'a Memoria,
    ent: &'a Entrada,
    op: &'a Opciones,
    b: Builder,
    glsl: Word,
    t_void: Word,
    t_bool: Word,
    t_u32: Word,
    t_i32: Word,
    t_f32: Word,
    tcache: HashMap<TypeId, Word>,
    vcache: HashMap<(Word, u32), Word>,
    pcache: HashMap<(StorageClass, Word), Word>,
    acache: HashMap<(Word, u32), Word>,
    ccache: HashMap<(Word, u32), Word>,
    vals: Vec<Option<TV>>,
    // recursos
    buffers: HashMap<u32, Word>,
    buffers_escritos: Vec<u32>,
    texturas: HashMap<(u32, bool), (Word, Word)>,
    samplers: HashMap<Smp, Word>,
    t_sampler: Word,
    enlaces: Vec<Enlace>,
    interfaz: Vec<Word>,
    builtins: HashMap<BuiltIn, (Word, Word)>,
    // memoria privada, de threadgroup y constante
    privadas: HashMap<ValueId, Word>,
    globales: HashMap<usize, Word>,
    // control de flujo
    multi: bool,
    /// control de flujo estructurado (si no, máquina de estados)
    estructurado: bool,
    /// `bloque` se para antes del terminador (lo emite el estructurador)
    sin_terminador: bool,
    /// destino de las salidas de varios niveles del estructurador
    var_destino: Word,
    n_lazos: u32,
    cabeceras_lazo: Vec<Word>,
    vars: HashMap<ValueId, Word>,
    var_estado: Word,
    etiquetas: HashMap<usize, Word>,
    sel_merge: Word,
    // salidas
    salidas: Vec<(Word, Descripcion)>,
    varyings_out: Vec<(String, u32)>,
    atributos: Vec<(u32, u32, String)>,
    colores: Vec<u32>,
    grid: Option<Word>,
    usa_demote: bool,
    usa_subgrupos: bool,
    /// El shader escribe la profundidad (air.depth): necesita el modo de ejecución DepthReplacing.
    escribe_profundidad: bool,
    /// Capacidades que piden los recursos usados (Sampled1D, SampledCubeArray...).
    capacidades: Vec<Capability>,
    /// descriptor set de los recursos (Opciones::set)
    set: u32,
    /// Base de los punteros que cruzan bloques (la variable guarda solo el desplazamiento).
    bases: HashMap<ValueId, Base>,
    /// Valores que son siempre la misma textura o sampler (phi/select con la textura nula): se resuelven sin variable.
    fijos: HashMap<ValueId, TV>,
    /// Tamaño del grupo de un kernel (constantes de especialización 0, 1 y 2).
    wg_size: Option<Word>,
    t_bloque: Option<Word>,
    /// Tipo de muestra de cada textura (0 real, 1 sin signo, 2 con signo), del tipo de su argumento.
    tipos_textura: HashMap<u32, u8>,
    wg_args: HashMap<u32, Word>,
}

fn espacio(m: &Module, t: TypeId) -> Option<u32> {
    match m.types.get(t)? {
        Type::Ptr(a) => Some(*a),
        _ => None,
    }
}

impl<'a> Tr<'a> {
    // ------------------------------------------------------------ tipos y constantes

    fn vec_t(&mut self, e: Word, n: u32) -> Word {
        if n == 1 {
            return e;
        }
        if let Some(&t) = self.vcache.get(&(e, n)) {
            return t;
        }
        let t = self.b.type_vector(e, n);
        self.vcache.insert((e, n), t);
        t
    }

    fn ptr_t(&mut self, sc: StorageClass, t: Word) -> Word {
        if let Some(&p) = self.pcache.get(&(sc, t)) {
            return p;
        }
        let p = self.b.type_pointer(None, sc, t);
        self.pcache.insert((sc, t), p);
        p
    }

    fn arr_t(&mut self, e: Word, n: u32) -> Word {
        if let Some(&t) = self.acache.get(&(e, n)) {
            return t;
        }
        let len = self.cu(n);
        let t = self.b.type_array(e, len);
        self.acache.insert((e, n), t);
        t
    }

    /// Tipo SPIR-V de un tipo LLVM (los punteros no tienen: se tratan aparte).
    fn ty(&mut self, t: TypeId) -> Res<Word> {
        if let Some(&w) = self.tcache.get(&t) {
            return Ok(w);
        }
        let w = match self.m.types[t].clone() {
            Type::Void => self.t_void,
            Type::Int(1) => self.t_bool,
            Type::Int(_) => self.t_u32,
            Type::Half | Type::BFloat | Type::Float => self.t_f32,
            Type::Vector(n, e) => {
                let e = self.ty(e)?;
                self.vec_t(e, n)
            }
            Type::Array(n, e) => {
                let e = self.ty(e)?;
                self.arr_t(e, n as u32)
            }
            Type::Struct { elems, .. } => {
                let es: Vec<Word> = elems.iter().map(|&e| self.ty(e)).collect::<Res<_>>()?;
                self.b.type_struct(es)
            }
            other => return Err(format!("tipo sin traducción: {other:?}")),
        };
        self.tcache.insert(t, w);
        Ok(w)
    }

    fn cu(&mut self, v: u32) -> Word {
        let t = self.t_u32;
        self.c32(t, v)
    }
    fn cf(&mut self, v: f32) -> Word {
        let t = self.t_f32;
        self.c32(t, v.to_bits())
    }
    fn c32(&mut self, t: Word, v: u32) -> Word {
        if let Some(&c) = self.ccache.get(&(t, v)) {
            return c;
        }
        let c = self.b.constant_bit32(t, v);
        self.ccache.insert((t, v), c);
        c
    }
    fn cbool(&mut self, v: bool) -> Word {
        let t = self.t_bool;
        if v { self.b.constant_true(t) } else { self.b.constant_false(t) }
    }

    /// Una constante como SPIR-V del tipo LLVM `t`.
    fn constante(&mut self, t: TypeId, v: &V) -> Res<Word> {
        let st = self.ty(t)?;
        Ok(match (&self.m.types[t].clone(), v) {
            (_, V::Undef) => self.b.constant_null(st),
            (Type::Int(1), V::Int(_, x)) => self.cbool(*x != 0),
            (Type::Int(_), V::Int(_, x)) => self.cu(*x as u32),
            (Type::Half | Type::BFloat | Type::Float, V::Real(_, x)) => self.cf(*x as f32),
            (Type::Vector(_, e), V::Vec(xs)) | (Type::Array(_, e), V::Agg(xs)) => {
                let e = *e;
                let cs: Vec<Word> = xs.iter().map(|x| self.constante(e, x)).collect::<Res<_>>()?;
                self.b.constant_composite(st, cs)
            }
            (Type::Struct { elems, .. }, V::Agg(xs)) => {
                let elems = elems.clone();
                let cs: Vec<Word> = elems.iter().zip(xs).map(|(&e, x)| self.constante(e, x)).collect::<Res<_>>()?;
                self.b.constant_composite(st, cs)
            }
            (ty, v) => return Err(format!("constante {v:?} de tipo {ty:?}")),
        })
    }

    // ------------------------------------------------------------ valores

    fn tv(&mut self, v: ValueId) -> Res<TV> {
        if v >= 1_000_000_000 {
            return Ok(TV::C(V::Undef)); // argumento de metadatos
        }
        if let Some(x) = self.fijos.get(&v) {
            return Ok(x.clone());
        }
        if let Some(Some(c)) = self.an.constantes.get(v) {
            return Ok(match c {
                V::Ptr(Mem::Global(g), off) => TV::Ptr(self.base_global(*g), Off::C(*off)),
                V::Ptr(Mem::Null, _) => TV::Ptr(Base::Nula, Off::C(0)),
                other => TV::C(other.clone()),
            });
        }
        if let Some(Some(x)) = self.vals.get(v) {
            return Ok(x.clone());
        }
        if let Some(&var) = self.vars.get(&v) {
            // valor de otro bloque: por su variable
            let t = self.f.values[v].ty;
            if espacio(self.m, t).is_some() {
                let base = *self.bases.get(&v).ok_or_else(|| {
                    let d = |v: ValueId| match self.f.values[v].kind {
                        ValueKind::Inst(b, k) => format!("{:?}", self.f.blocks[b].insts[k].inst),
                        ref o => format!("{o:?}"),
                    };
                    let mut def = d(v);
                    if let ValueKind::Inst(b, k) = self.f.values[v].kind {
                        if let Inst::Phi { incoming } = &self.f.blocks[b].insts[k].inst {
                            for (x, _) in incoming {
                                def += &format!("; %{x} = {}", d(*x));
                            }
                        }
                    }
                    format!("puntero que cruza bloques sin base conocida: %{v} = {def}")
                })?;
                let o = self.b.load(self.t_u32, None, var, None, []).map_err(|e| format!("{e:?}"))?;
                return Ok(TV::Ptr(base, Off::Id(o)));
            }
            let st = self.ty(t)?;
            let x = self.b.load(st, None, var, None, []).map_err(|e| format!("{e:?}"))?;
            return Ok(TV::Id(x));
        }
        Err(format!("valor {v} sin definir (tipo {})", super::tipo(self.m, self.f.values[v].ty)))
    }

    /// El valor como id de SPIR-V (materializa constantes).
    fn id(&mut self, v: ValueId) -> Res<Word> {
        let x = self.tv(v)?;
        let t = self.f.values.get(v).map(|x| x.ty).unwrap_or(0);
        self.id_de(&x, t)
    }

    fn id_de(&mut self, x: &TV, t: TypeId) -> Res<Word> {
        match x {
            TV::Id(w) => Ok(*w),
            TV::C(c) => self.constante(t, c),
            other => Err(format!("se esperaba un valor y es {other:?}")),
        }
    }

    fn base_global(&self, g: usize) -> Base {
        if self.m.globals[g].addrspace == 3 { Base::Wg(g) } else { Base::Glob(g) }
    }

    /// Variable global (de módulo), aunque haya una función seleccionada: rspirv la pondría en el bloque actual.
    fn var_global(&mut self, pt: Word, sc: StorageClass, init: Option<Word>) -> Word {
        let id = self.b.id();
        let mut ops = vec![Operand::StorageClass(sc)];
        if let Some(i) = init {
            ops.push(Operand::IdRef(i));
        }
        let inst = rspirv::dr::Instruction::new(spirv::Op::Variable, Some(pt), Some(id), ops);
        self.b.module_mut().types_global_values.push(inst);
        id
    }

    fn ext(&mut self, t: Word, op: u32, args: &[Word]) -> Word {
        let ops: Vec<Operand> = args.iter().map(|&a| Operand::IdRef(a)).collect();
        self.b.ext_inst(t, None, self.glsl, op, ops).unwrap()
    }

    // ------------------------------------------------------------ memoria

    /// Variable de palabras de una base (la crea la primera vez).
    fn var_base(&mut self, b: Base) -> Res<(Word, StorageClass)> {
        match b {
            Base::Buf(i) => {
                if let Some(&v) = self.buffers.get(&i) {
                    return Ok((v, StorageClass::StorageBuffer));
                }
                // un solo tipo de bloque para todos los buffers (rspirv reutiliza el arreglo: decorarlo una vez)
                let st = match self.t_bloque {
                    Some(st) => st,
                    None => {
                        let rt = self.b.type_runtime_array(self.t_u32);
                        self.b.decorate(rt, Decoration::ArrayStride, [Operand::LiteralBit32(4)]);
                        let st = self.b.type_struct([rt]);
                        self.b.decorate(st, Decoration::Block, []);
                        self.b.member_decorate(st, 0, Decoration::Offset, [Operand::LiteralBit32(0)]);
                        self.t_bloque = Some(st);
                        st
                    }
                };
                let pt = self.ptr_t(StorageClass::StorageBuffer, st);
                let v = self.var_global(pt, StorageClass::StorageBuffer, None);
                self.b.decorate(v, Decoration::DescriptorSet, [Operand::LiteralBit32(self.set)]);
                self.b.decorate(v, Decoration::Binding, [Operand::LiteralBit32(i)]);
                self.buffers.insert(i, v);
                self.interfaz.push(v);
                Ok((v, StorageClass::StorageBuffer))
            }
            Base::WgArg(i) => {
                if let Some(&v) = self.wg_args.get(&i) {
                    return Ok((v, StorageClass::Workgroup));
                }
                // tamaño en palabras: constante de especialización 16 + índice (la fija la app con
                // setThreadgroupMemoryLength; por omisión 4096 palabras = 16 KB)
                let n = self.b.spec_constant_bit32(self.t_u32, 4096);
                self.b.decorate(n, Decoration::SpecId, [Operand::LiteralBit32(16 + i)]);
                let at = self.b.type_array(self.t_u32, n);
                let pt = self.ptr_t(StorageClass::Workgroup, at);
                let v = self.var_global(pt, StorageClass::Workgroup, None);
                self.wg_args.insert(i, v);
                self.interfaz.push(v);
                Ok((v, StorageClass::Workgroup))
            }
            Base::Priv(a) => self.privadas.get(&a).map(|&v| (v, StorageClass::Function)).ok_or_else(|| "alloca sin variable".to_string()),
            Base::Wg(g) | Base::Glob(g) => {
                let sc = if matches!(b, Base::Wg(_)) { StorageClass::Workgroup } else { StorageClass::Private };
                if let Some(&v) = self.globales.get(&g) {
                    return Ok((v, sc));
                }
                let gl = &self.m.globals[g];
                let n = layout::size(self.m, gl.ty).div_ceil(4).max(1) as u32;
                let at = self.arr_t(self.t_u32, n);
                let pt = self.ptr_t(sc, at);
                let init = if sc == StorageClass::Private {
                    // datos constantes: los bytes de la memoria (ya especializada)
                    let bytes = self.mem.globales.get(g).cloned().unwrap_or_default();
                    let ws: Vec<Word> = (0..n as usize)
                        .map(|k| {
                            let mut w = [0u8; 4];
                            for (j, x) in w.iter_mut().enumerate() {
                                *x = bytes.get(k * 4 + j).copied().unwrap_or(0);
                            }
                            u32::from_le_bytes(w)
                        })
                        .collect::<Vec<_>>()
                        .into_iter()
                        .map(|w| self.cu(w))
                        .collect();
                    Some(self.b.constant_composite(at, ws))
                } else {
                    None
                };
                let v = self.var_global(pt, sc, init);
                self.globales.insert(g, v);
                self.interfaz.push(v);
                Ok((v, sc))
            }
            Base::Nula => Err("acceso a un puntero nulo".into()),
        }
    }

    fn off_mas(&mut self, o: Off, n: i64) -> Off {
        match o {
            Off::C(c) => Off::C(c + n),
            Off::Id(id) => {
                if n == 0 {
                    return Off::Id(id);
                }
                let c = self.cu(n as u32);
                Off::Id(self.b.i_add(self.t_u32, None, id, c).unwrap())
            }
        }
    }

    fn off_id(&mut self, o: Off) -> Word {
        match o {
            Off::C(c) => self.cu(c as u32),
            Off::Id(id) => id,
        }
    }

    /// Puntero a la palabra que contiene el byte `o`.
    fn palabra(&mut self, b: Base, o: Off) -> Res<(Word, StorageClass)> {
        let (var, sc) = self.var_base(b)?;
        let idx = match o {
            Off::C(c) => self.cu((c >> 2) as u32),
            Off::Id(id) => {
                let dos = self.cu(2);
                self.b.shift_right_logical(self.t_u32, None, id, dos).unwrap()
            }
        };
        let pt = self.ptr_t(sc, self.t_u32);
        let p = if sc == StorageClass::StorageBuffer {
            let cero = self.cu(0);
            self.b.access_chain(pt, None, var, [cero, idx]).unwrap()
        } else {
            self.b.access_chain(pt, None, var, [idx]).unwrap()
        };
        Ok((p, sc))
    }

    /// Desplazamiento en bits dentro de la palabra (para trozos de 8 o 16 bits).
    fn bits_en_palabra(&mut self, o: Off) -> Word {
        match o {
            Off::C(c) => self.cu(((c & 3) * 8) as u32),
            Off::Id(id) => {
                let tres = self.cu(3);
                let a = self.b.bitwise_and(self.t_u32, None, id, tres).unwrap();
                self.b.shift_left_logical(self.t_u32, None, a, tres).unwrap() // ×8
            }
        }
    }

    fn cargar(&mut self, b: Base, o: Off, t: TypeId) -> Res<Word> {
        if b == Base::Nula {
            // puntero nulo (buffer ausente en esta variante): 0; solo en caminos que el shader no toma
            let st = self.ty(t)?;
            return Ok(self.b.constant_null(st));
        }
        let tipo = self.m.types[t].clone();
        match tipo {
            Type::Int(32) | Type::Int(64) | Type::Float => {
                let (p, _) = self.palabra(b, o)?;
                let w = self.b.load(self.t_u32, None, p, None, []).unwrap();
                Ok(if matches!(tipo, Type::Float) { self.b.bitcast(self.t_f32, None, w).unwrap() } else { w })
            }
            Type::Int(n) if n < 32 => {
                let (p, _) = self.palabra(b, o)?;
                let w = self.b.load(self.t_u32, None, p, None, []).unwrap();
                let sh = self.bits_en_palabra(o);
                let x = self.b.shift_right_logical(self.t_u32, None, w, sh).unwrap();
                let mk = self.cu(if n <= 8 { 0xff } else { 0xffff });
                let x = self.b.bitwise_and(self.t_u32, None, x, mk).unwrap();
                if n == 1 {
                    let cero = self.cu(0);
                    return Ok(self.b.i_not_equal(self.t_bool, None, x, cero).unwrap());
                }
                Ok(x)
            }
            Type::BFloat => {
                let (p, _) = self.palabra(b, o)?;
                let w = self.b.load(self.t_u32, None, p, None, []).unwrap();
                let sh = self.bits_en_palabra(o);
                let x = self.b.shift_right_logical(self.t_u32, None, w, sh).unwrap();
                let c16 = self.cu(16);
                let x = self.b.shift_left_logical(self.t_u32, None, x, c16).unwrap();
                Ok(self.b.bitcast(self.t_f32, None, x).unwrap())
            }
            Type::Half => {
                let (p, _) = self.palabra(b, o)?;
                let w = self.b.load(self.t_u32, None, p, None, []).unwrap();
                let sh = self.bits_en_palabra(o);
                let x = self.b.shift_right_logical(self.t_u32, None, w, sh).unwrap();
                let v2 = self.vec_t(self.t_f32, 2);
                let h = self.ext(v2, 62, &[x]); // UnpackHalf2x16
                Ok(self.b.composite_extract(self.t_f32, None, h, [0]).unwrap())
            }
            Type::Vector(n, e) => {
                let es = layout::elem_size(self.m, t) as i64;
                let mut cs = Vec::with_capacity(n as usize);
                for k in 0..n as i64 {
                    let ok = self.off_mas(o, k * es);
                    cs.push(self.cargar(b, ok, e)?);
                }
                let st = self.ty(t)?;
                Ok(self.b.composite_construct(st, None, cs).unwrap())
            }
            Type::Array(n, e) => {
                let es = layout::size(self.m, e) as i64;
                let mut cs = Vec::with_capacity(n as usize);
                for k in 0..n as i64 {
                    let ok = self.off_mas(o, k * es);
                    cs.push(self.cargar(b, ok, e)?);
                }
                let st = self.ty(t)?;
                Ok(self.b.composite_construct(st, None, cs).unwrap())
            }
            Type::Struct { elems, .. } => {
                let offs = layout::offsets(self.m, t);
                let mut cs = Vec::with_capacity(elems.len());
                for (&e, off) in elems.iter().zip(offs) {
                    let ok = self.off_mas(o, off as i64);
                    cs.push(self.cargar(b, ok, e)?);
                }
                let st = self.ty(t)?;
                Ok(self.b.composite_construct(st, None, cs).unwrap())
            }
            other => Err(format!("load de {other:?}")),
        }
    }

    fn guardar(&mut self, b: Base, o: Off, t: TypeId, v: Word) -> Res<()> {
        if b == Base::Nula {
            return Ok(());
        }
        if let Base::Buf(i) = b {
            if !self.buffers_escritos.contains(&i) {
                self.buffers_escritos.push(i);
            }
        }
        let tipo = self.m.types[t].clone();
        match tipo {
            Type::Int(32) | Type::Int(64) | Type::Float => {
                let (p, _) = self.palabra(b, o)?;
                let w = if matches!(tipo, Type::Float) { self.b.bitcast(self.t_u32, None, v).unwrap() } else { v };
                self.b.store(p, w, None, []).unwrap();
                Ok(())
            }
            Type::Int(n) if n < 32 => {
                let x = if n == 1 {
                    let (uno, cero) = (self.cu(1), self.cu(0));
                    self.b.select(self.t_u32, None, v, uno, cero).unwrap()
                } else {
                    v
                };
                self.guardar_trozo(b, o, x, if n <= 8 { 0xff } else { 0xffff })
            }
            Type::BFloat => {
                let r = self.a_bf16(v, 1, self.t_f32);
                let u = self.b.bitcast(self.t_u32, None, r).unwrap();
                let c16 = self.cu(16);
                let bits = self.b.shift_right_logical(self.t_u32, None, u, c16).unwrap();
                self.guardar_trozo(b, o, bits, 0xffff)
            }
            Type::Half => {
                let cero = self.cf(0.0);
                let v2 = self.vec_t(self.t_f32, 2);
                let par = self.b.composite_construct(v2, None, [v, cero]).unwrap();
                let bits = self.ext(self.t_u32, 58, &[par]); // PackHalf2x16
                self.guardar_trozo(b, o, bits, 0xffff)
            }
            Type::Vector(..) | Type::Array(..) => {
                let (n, e) = match tipo {
                    Type::Vector(n, e) => (n as u64, e),
                    Type::Array(n, e) => (n, e),
                    _ => unreachable!(),
                };
                let es = if matches!(tipo, Type::Vector(..)) { layout::elem_size(self.m, t) } else { layout::size(self.m, e) } as i64;
                let st_e = self.ty(e)?;
                for k in 0..n as i64 {
                    let x = self.b.composite_extract(st_e, None, v, [k as u32]).unwrap();
                    let ok = self.off_mas(o, k * es);
                    self.guardar(b, ok, e, x)?;
                }
                Ok(())
            }
            Type::Struct { elems, .. } => {
                let offs = layout::offsets(self.m, t);
                for (k, (&e, off)) in elems.iter().zip(offs).enumerate() {
                    let st_e = self.ty(e)?;
                    let x = self.b.composite_extract(st_e, None, v, [k as u32]).unwrap();
                    let ok = self.off_mas(o, off as i64);
                    self.guardar(b, ok, e, x)?;
                }
                Ok(())
            }
            other => Err(format!("store de {other:?}")),
        }
    }

    /// Guarda los bits bajos de `x` (máscara `mk`) en su sitio dentro de la palabra: en memoria compartida con dos
    /// atómicos (borrar y poner), para no pisar los bytes vecinos que escriban otros hilos.
    fn guardar_trozo(&mut self, b: Base, o: Off, x: Word, mk: u32) -> Res<()> {
        let (p, sc) = self.palabra(b, o)?;
        let sh = self.bits_en_palabra(o);
        let mkc = self.cu(mk);
        let xm = self.b.bitwise_and(self.t_u32, None, x, mkc).unwrap();
        let val = self.b.shift_left_logical(self.t_u32, None, xm, sh).unwrap();
        let mk_sh = self.b.shift_left_logical(self.t_u32, None, mkc, sh).unwrap();
        let inv = self.b.not(self.t_u32, None, mk_sh).unwrap();
        if sc == StorageClass::Function || sc == StorageClass::Private {
            let w = self.b.load(self.t_u32, None, p, None, []).unwrap();
            let w = self.b.bitwise_and(self.t_u32, None, w, inv).unwrap();
            let w = self.b.bitwise_or(self.t_u32, None, w, val).unwrap();
            self.b.store(p, w, None, []).unwrap();
        } else {
            let scope = self.cu(if sc == StorageClass::Workgroup { Scope::Workgroup as u32 } else { Scope::Device as u32 });
            let sem = self.cu(0);
            self.b.atomic_and(self.t_u32, None, p, scope, sem, inv).unwrap();
            self.b.atomic_or(self.t_u32, None, p, scope, sem, val).unwrap();
        }
        Ok(())
    }
}

pub use self::traducir_impl::traducir;
#[path = "spirv_inst.rs"]
mod traducir_impl;
