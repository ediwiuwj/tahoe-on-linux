//! Lector del IR de LLVM en formato bitcode (el AIR de Metal es un módulo LLVM con funciones `air.*` y metadatos
//! `!air.*`). El crate `llvm-bitcode` da los bloques y registros del flujo de bits; aquí se interpretan como en
//! `BitcodeReader.cpp` de LLVM: tabla de tipos, valores globales (con nombres en STRTAB), constantes, metadatos y el
//! cuerpo de cada función con la numeración relativa de operandos (versión 2 del módulo).

use std::collections::HashMap;

use llvm_bitcode::bitcode::{BitcodeElement, Payload};
use llvm_bitcode::Bitcode;

pub type Res<T> = Result<T, String>;

// ---------------------------------------------------------------- flujo de bits normalizado

/// Un registro: código y operandos (los campos y, si lo hay, el arreglo final del registro abreviado).
#[derive(Debug, Clone)]
pub struct Rec {
    pub code: u64,
    pub ops: Vec<u64>,
    pub blob: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum Elem {
    Rec(Rec),
    Blk(Blk),
}

#[derive(Debug, Clone)]
pub struct Blk {
    pub id: u32,
    pub elems: Vec<Elem>,
}

impl Blk {
    pub fn recs(&self) -> impl Iterator<Item = &Rec> {
        self.elems.iter().filter_map(|e| if let Elem::Rec(r) = e { Some(r) } else { None })
    }
    pub fn blks(&self) -> impl Iterator<Item = &Blk> {
        self.elems.iter().filter_map(|e| if let Elem::Blk(b) = e { Some(b) } else { None })
    }
}

fn convertir(e: BitcodeElement) -> Elem {
    match e {
        BitcodeElement::Block(b) => Elem::Blk(Blk { id: b.id, elems: b.elements.into_iter().map(convertir).collect() }),
        BitcodeElement::Record(mut r) => {
            let mut ops = r.fields().to_vec();
            let mut blob = None;
            match r.take_payload() {
                Some(Payload::Array(a)) => ops.extend(a),
                Some(Payload::Char6String(s)) => ops.extend(s.bytes().map(u64::from)),
                Some(Payload::Blob(b)) => blob = Some(b),
                None => {}
            }
            Elem::Rec(Rec { code: r.id, ops, blob })
        }
    }
}

// Bloques (LLVMBitCodes.h)
const MODULE: u32 = 8;
const PARAMATTR: u32 = 9;
const CONSTANTS: u32 = 11;
const FUNCTION: u32 = 12;
const VALUE_SYMTAB: u32 = 14;
const METADATA: u32 = 15;
const TYPE_NEW: u32 = 17;
const STRTAB: u32 = 23;

// ---------------------------------------------------------------- tipos

pub type TypeId = usize;

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Void,
    Half,
    BFloat,
    Float,
    Double,
    Label,
    Metadata,
    Token,
    Int(u32),
    /// Puntero opaco con su espacio de direcciones (AIR: 0 privado, 1 device, 2 constant, 3 threadgroup).
    Ptr(u32),
    Array(u64, TypeId),
    Vector(u32, TypeId),
    Struct { name: Option<String>, packed: bool, elems: Vec<TypeId> },
    Function { vararg: bool, ret: TypeId, params: Vec<TypeId> },
    Opaque(Option<String>),
    Otro(u64),
}

// ---------------------------------------------------------------- valores

pub type ValueId = usize;

#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Null,
    Undef,
    Poison,
    Int(i128),
    /// Bits del número en coma flotante (half, float o double según el tipo).
    Float(u64),
    Aggregate(Vec<ValueId>),
    /// Arreglo o vector de enteros o reales en crudo (CST_CODE_DATA).
    Data(Vec<u64>),
    Str(Vec<u8>),
    /// Expresión constante: un código de instrucción con operandos (CE_CAST, CE_GEP...).
    Expr { code: u64, ops: Vec<u64> },
}

#[derive(Debug, Clone)]
pub enum ValueKind {
    Function(usize),
    Global(usize),
    Const(Const),
    Arg(usize),
    /// Resultado de una instrucción: (bloque, posición).
    Inst(usize, usize),
    /// Valor de metadatos usado como operando (llamadas a llvm.dbg.* y similares).
    Metadata(usize),
}

#[derive(Debug, Clone)]
pub struct Value {
    pub ty: TypeId,
    pub kind: ValueKind,
}

// ---------------------------------------------------------------- instrucciones

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    UDiv,
    SDiv,
    URem,
    SRem,
    Shl,
    LShr,
    AShr,
    And,
    Or,
    Xor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastOp {
    Trunc,
    ZExt,
    SExt,
    FPToUI,
    FPToSI,
    UIToFP,
    SIToFP,
    FPTrunc,
    FPExt,
    PtrToInt,
    IntToPtr,
    BitCast,
    AddrSpaceCast,
}

#[derive(Debug, Clone)]
pub enum Inst {
    Bin { op: BinOp, a: ValueId, b: ValueId },
    FNeg(ValueId),
    Cast { op: CastOp, v: ValueId },
    /// Predicado de LLVM: 0-15 comparaciones reales, 32-41 enteras.
    Cmp { pred: u32, a: ValueId, b: ValueId },
    Select { c: ValueId, t: ValueId, f: ValueId },
    ExtractElt { v: ValueId, i: ValueId },
    InsertElt { v: ValueId, e: ValueId, i: ValueId },
    Shuffle { a: ValueId, b: ValueId, mask: ValueId },
    ExtractVal { v: ValueId, idx: Vec<u32> },
    InsertVal { agg: ValueId, v: ValueId, idx: Vec<u32> },
    Load { ptr: ValueId, align: u32 },
    Store { ptr: ValueId, val: ValueId, align: u32 },
    Gep { src_ty: TypeId, ptr: ValueId, idx: Vec<ValueId>, inbounds: bool },
    Alloca { ty: TypeId, n: ValueId, align: u32 },
    Call { fnty: TypeId, callee: ValueId, args: Vec<ValueId> },
    Phi { incoming: Vec<(ValueId, usize)> },
    Br(usize),
    CondBr { c: ValueId, t: usize, f: usize },
    Switch { v: ValueId, default: usize, cases: Vec<(ValueId, usize)> },
    Ret(Option<ValueId>),
    Unreachable,
    Freeze(ValueId),
    AtomicRmw { op: u32, ptr: ValueId, val: ValueId },
    CmpXchg { ptr: ValueId, cmp: ValueId, new: ValueId },
    Fence,
}

#[derive(Debug, Clone)]
pub struct Instr {
    pub inst: Inst,
    /// Tipo del resultado (Void si no da valor).
    pub ty: TypeId,
    /// Número de valor del resultado, si da valor.
    pub val: Option<ValueId>,
}

#[derive(Debug, Clone, Default)]
pub struct BasicBlock {
    pub insts: Vec<Instr>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub ty: TypeId,
    pub is_proto: bool,
    /// Tabla de valores de la función: globales, constantes del módulo, argumentos, constantes de la función e
    /// instrucciones, por número de valor.
    pub values: Vec<Value>,
    pub args: Vec<ValueId>,
    pub blocks: Vec<BasicBlock>,
}

#[derive(Debug, Clone)]
pub struct Global {
    pub name: String,
    /// Tipo del contenido (no el del puntero).
    pub ty: TypeId,
    pub addrspace: u32,
    pub is_const: bool,
    pub init: Option<ValueId>,
}

// ---------------------------------------------------------------- metadatos

#[derive(Debug, Clone)]
pub enum Md {
    Str(String),
    /// Un valor del módulo (número de valor global y su tipo).
    Value(TypeId, ValueId),
    Node(Vec<Option<usize>>),
    Otro,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub types: Vec<Type>,
    pub functions: Vec<Function>,
    pub globals: Vec<Global>,
    /// Valores del ámbito del módulo (globales y constantes), por número de valor.
    pub values: Vec<Value>,
    pub md: Vec<Md>,
    pub named_md: HashMap<String, Vec<usize>>,
    pub triple: String,
}

// ---------------------------------------------------------------- lectura

struct Lector {
    types: Vec<Type>,
    values: Vec<Value>,
    functions: Vec<Function>,
    globals: Vec<Global>,
    md: Vec<Md>,
    named_md: HashMap<String, Vec<usize>>,
    strtab: Vec<u8>,
    triple: String,
}

fn texto(ops: &[u64]) -> String {
    ops.iter().map(|&c| c as u8 as char).collect()
}

fn signo(v: u64) -> i64 {
    // VBR con signo en el bit bajo (emitSignedInt64)
    if v & 1 == 0 { (v >> 1) as i64 } else if v != 1 { -((v >> 1) as i64) } else { i64::MIN }
}

impl Lector {
    fn nombre(&self, off: u64, len: u64) -> String {
        let (o, l) = (off as usize, len as usize);
        self.strtab.get(o..o + l).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()
    }

    fn tipos(&mut self, b: &Blk) -> Res<()> {
        let mut nombre: Option<String> = None;
        for r in b.recs() {
            let o = &r.ops;
            let t = match r.code {
                1 => continue, // NUMENTRY
                2 => Type::Void,
                3 => Type::Float,
                4 => Type::Double,
                5 => Type::Label,
                6 => Type::Opaque(nombre.take()),
                7 => Type::Int(o[0] as u32),
                8 => Type::Ptr(o.get(1).copied().unwrap_or(0) as u32),
                10 => Type::Half,
                11 => Type::Array(o[0], o[1] as usize),
                12 => Type::Vector(o[0] as u32, o[1] as usize),
                16 => Type::Metadata,
                18 => Type::Struct { name: None, packed: o[0] != 0, elems: o[1..].iter().map(|&x| x as usize).collect() },
                19 => {
                    nombre = Some(texto(o));
                    continue;
                }
                20 => Type::Struct { name: nombre.take(), packed: o[0] != 0, elems: o[1..].iter().map(|&x| x as usize).collect() },
                21 => Type::Function { vararg: o[0] != 0, ret: o[1] as usize, params: o[2..].iter().map(|&x| x as usize).collect() },
                22 => Type::Token,
                23 => Type::BFloat,
                25 => Type::Ptr(o.first().copied().unwrap_or(0) as u32),
                c => Type::Otro(c),
            };
            self.types.push(t);
        }
        Ok(())
    }

    fn ty(&self, t: TypeId) -> Res<&Type> {
        self.types.get(t).ok_or_else(|| format!("tipo {t} fuera de la tabla"))
    }

    /// Bloque de constantes: añade valores a `values`.
    fn constantes(&self, b: &Blk, values: &mut Vec<Value>) -> Res<()> {
        let mut ty: TypeId = 0;
        for r in b.recs() {
            let o = &r.ops;
            let c = match r.code {
                1 => {
                    ty = o[0] as usize;
                    continue;
                }
                2 => Const::Null,
                3 => Const::Undef,
                26 => Const::Poison,
                4 => Const::Int(signo(o[0]) as i128),
                5 => {
                    // entero ancho: palabras de 64 bits con signo
                    let lo = signo(o[0]) as i128 as u128 & 0xffff_ffff_ffff_ffff;
                    let hi = o.get(1).map(|&x| signo(x) as i128).unwrap_or(0);
                    Const::Int(((hi << 64) as u128 | lo) as i128)
                }
                6 => Const::Float(o[0]),
                7 => Const::Aggregate(o.iter().map(|&x| x as usize).collect()),
                8 | 9 => {
                    let mut s: Vec<u8> = o.iter().map(|&x| x as u8).collect();
                    if r.code == 9 {
                        s.push(0);
                    }
                    Const::Str(s)
                }
                22 => Const::Data(o.clone()),
                c => Const::Expr { code: c, ops: o.clone() },
            };
            values.push(Value { ty, kind: ValueKind::Const(c) });
        }
        Ok(())
    }

    fn metadatos(&mut self, b: &Blk) -> Res<()> {
        let mut nombre: Option<String> = None;
        for r in b.recs() {
            let o = &r.ops;
            match r.code {
                35 => {
                    // METADATA_STRINGS: [cuenta, desplazamiento], blob = longitudes en VBR6 y luego los caracteres
                    let (n, off) = (o[0] as usize, o[1] as usize);
                    let blob = r.blob.as_deref().ok_or("METADATA_STRINGS sin blob")?;
                    let mut lens = Vec::with_capacity(n);
                    let mut bits = BitLector { d: blob, pos: 0 };
                    for _ in 0..n {
                        lens.push(bits.vbr(6)? as usize);
                    }
                    let mut p = off;
                    for l in lens {
                        let s = blob.get(p..p + l).ok_or("cadena de metadatos fuera del blob")?;
                        self.md.push(Md::Str(String::from_utf8_lossy(s).into_owned()));
                        p += l;
                    }
                }
                1 => self.md.push(Md::Str(texto(o))),
                2 => self.md.push(Md::Value(o[0] as usize, o[1] as usize)),
                3 | 5 => self.md.push(Md::Node(o.iter().map(|&x| if x == 0 { None } else { Some(x as usize - 1) }).collect())),
                4 => nombre = Some(texto(o)),
                10 => {
                    if let Some(n) = nombre.take() {
                        self.named_md.insert(n, o.iter().map(|&x| x as usize).collect());
                    }
                }
                6 | 11 | 36 | 38 | 39 => {} // KIND, ATTACHMENT, GLOBAL_DECL_ATTACHMENT, INDEX_OFFSET, INDEX: no ocupan número
                _ => self.md.push(Md::Otro),
            }
        }
        Ok(())
    }

    fn modulo(&mut self, m: &Blk) -> Res<()> {
        // 1) tipos, atributos y constantes/valores globales en el orden del bloque
        let mut cuerpos: Vec<&Blk> = Vec::new();
        for e in &m.elems {
            match e {
                Elem::Blk(b) if b.id == TYPE_NEW => self.tipos(b)?,
                Elem::Blk(b) if b.id == CONSTANTS => {
                    let mut v = std::mem::take(&mut self.values);
                    self.constantes(b, &mut v)?;
                    self.values = v;
                }
                Elem::Blk(b) if b.id == METADATA => self.metadatos(b)?,
                Elem::Blk(b) if b.id == FUNCTION => cuerpos.push(b),
                Elem::Blk(b) if b.id == PARAMATTR || b.id == VALUE_SYMTAB => {}
                Elem::Blk(_) => {}
                Elem::Rec(r) => {
                    let o = &r.ops;
                    match r.code {
                        2 => self.triple = texto(o),
                        7 => {
                            // GLOBALVAR v2: [strtab_off, strtab_size, tipo, isconst|explícito<<1|as<<2, initid, linkage, ...]
                            let name = self.nombre(o[0], o[1]);
                            let flags = o[3];
                            let ty = o[2] as usize;
                            let addrspace = if flags & 2 != 0 { (flags >> 2) as u32 } else if let Ok(Type::Ptr(a)) = self.ty(ty) { *a } else { 0 };
                            let init = if o[4] != 0 { Some(o[4] as usize - 1) } else { None };
                            let gi = self.globals.len();
                            self.globals.push(Global { name, ty, addrspace, is_const: flags & 1 != 0, init });
                            let pty = self.ptr_type(addrspace);
                            self.values.push(Value { ty: pty, kind: ValueKind::Global(gi) });
                        }
                        8 => {
                            // FUNCTION v2: [strtab_off, strtab_size, tipo, cc, isproto, linkage, ..., addrspace(16)]
                            let name = self.nombre(o[0], o[1]);
                            let fi = self.functions.len();
                            let addrspace = o.get(18).copied().unwrap_or(0) as u32;
                            self.functions.push(Function { name, ty: o[2] as usize, is_proto: o[4] != 0, values: Vec::new(), args: Vec::new(), blocks: Vec::new() });
                            let pty = self.ptr_type(addrspace);
                            self.values.push(Value { ty: pty, kind: ValueKind::Function(fi) });
                        }
                        14 => {
                            // ALIAS: ocupa un número de valor
                            self.values.push(Value { ty: o[2] as usize, kind: ValueKind::Const(Const::Undef) });
                        }
                        _ => {}
                    }
                }
            }
        }
        // 2) cuerpos: en el orden de las funciones definidas
        let definidas: Vec<usize> = (0..self.functions.len()).filter(|&i| !self.functions[i].is_proto).collect();
        if definidas.len() != cuerpos.len() {
            return Err(format!("{} funciones definidas y {} cuerpos", definidas.len(), cuerpos.len()));
        }
        for (fi, b) in definidas.into_iter().zip(cuerpos) {
            if std::env::var_os("TAHOE_AIR_DEBUG").is_some() {
                eprintln!("cuerpo {} ({} elementos)", self.functions[fi].name, b.elems.len());
            }
            self.cuerpo(fi, b).map_err(|e| format!("{}: {e}", self.functions[fi].name))?;
        }
        Ok(())
    }

    /// Un tipo puntero del espacio de direcciones (lo crea si no existe).
    fn ptr_type(&mut self, a: u32) -> TypeId {
        if let Some(i) = self.types.iter().position(|t| *t == Type::Ptr(a)) {
            return i;
        }
        self.types.push(Type::Ptr(a));
        self.types.len() - 1
    }

    #[allow(unused_assignments)]
    fn cuerpo(&mut self, fi: usize, b: &Blk) -> Res<()> {
        let fty = self.functions[fi].ty;
        let params = match self.ty(fty)? {
            Type::Function { params, .. } => params.clone(),
            t => return Err(format!("la función no tiene tipo de función: {t:?}")),
        };
        let mut values = self.values.clone();
        let mut args = Vec::new();
        for (i, &p) in params.iter().enumerate() {
            args.push(values.len());
            values.push(Value { ty: p, kind: ValueKind::Arg(i) });
        }
        let mut blocks: Vec<BasicBlock> = Vec::new();
        let mut cur = 0usize;
        // tipos de los valores usados antes de definirse (en bucles, un bloque posterior puede dominar a uno anterior)
        let mut fwd: HashMap<ValueId, TypeId> = HashMap::new();
        let void = self.types.iter().position(|t| *t == Type::Void);
        for e in &b.elems {
            let r = match e {
                Elem::Blk(sb) if sb.id == CONSTANTS => {
                    self.constantes(sb, &mut values)?;
                    continue;
                }
                Elem::Blk(sb) if sb.id == METADATA => {
                    continue; // metadatos locales (valores de la función usados como metadatos)
                }
                Elem::Blk(_) => continue,
                Elem::Rec(r) => r,
            };
            let o = &r.ops;
            let next = values.len();
            if std::env::var_os("TAHOE_AIR_DEBUG").is_some() {
                eprintln!("  [{next}] código {} {:?}", r.code, o);
            }
            let mut p = 0usize;
            // operando relativo; si es una referencia adelantada lleva su tipo detrás
            macro_rules! vt {
                () => {{
                    let v = (next as u64).wrapping_sub(o[p]) as u32 as usize;
                    p += 1;
                    if v >= next {
                        fwd.insert(v, o[p] as usize); // referencia adelantada: su tipo va detrás
                        p += 1;
                    }
                    v
                }};
            }
            macro_rules! rel {
                () => {{
                    let v = (next as u64).wrapping_sub(o[p]) as u32 as usize;
                    p += 1;
                    v
                }};
            }
            let ty_of = |values: &Vec<Value>, fwd: &HashMap<ValueId, TypeId>, v: ValueId| values.get(v).map(|x| x.ty).or_else(|| fwd.get(&v).copied());
            let mut res_ty: Option<TypeId> = None;
            let inst = match r.code {
                1 => {
                    blocks = vec![BasicBlock::default(); o[0] as usize];
                    continue;
                }
                2 => {
                    let a = vt!();
                    let bb = rel!();
                    let op = match o[p] {
                        0 => BinOp::Add,
                        1 => BinOp::Sub,
                        2 => BinOp::Mul,
                        3 => BinOp::UDiv,
                        4 => BinOp::SDiv,
                        5 => BinOp::URem,
                        6 => BinOp::SRem,
                        7 => BinOp::Shl,
                        8 => BinOp::LShr,
                        9 => BinOp::AShr,
                        10 => BinOp::And,
                        11 => BinOp::Or,
                        12 => BinOp::Xor,
                        x => return Err(format!("binop {x}")),
                    };
                    res_ty = ty_of(&values, &fwd, a);
                    Inst::Bin { op, a, b: bb }
                }
                3 => {
                    let v = vt!();
                    res_ty = Some(o[p] as usize);
                    let op = match o[p + 1] {
                        0 => CastOp::Trunc,
                        1 => CastOp::ZExt,
                        2 => CastOp::SExt,
                        3 => CastOp::FPToUI,
                        4 => CastOp::FPToSI,
                        5 => CastOp::UIToFP,
                        6 => CastOp::SIToFP,
                        7 => CastOp::FPTrunc,
                        8 => CastOp::FPExt,
                        9 => CastOp::PtrToInt,
                        10 => CastOp::IntToPtr,
                        11 => CastOp::BitCast,
                        12 => CastOp::AddrSpaceCast,
                        x => return Err(format!("cast {x}")),
                    };
                    Inst::Cast { op, v }
                }
                56 => {
                    let v = vt!();
                    res_ty = ty_of(&values, &fwd, v);
                    Inst::FNeg(v)
                }
                6 => {
                    let v = vt!();
                    let i = vt!();
                    res_ty = match ty_of(&values, &fwd, v).and_then(|t| self.types.get(t)) {
                        Some(Type::Vector(_, e)) => Some(*e),
                        _ => None,
                    };
                    Inst::ExtractElt { v, i }
                }
                7 => {
                    let v = vt!();
                    let e2 = rel!();
                    let i = vt!();
                    res_ty = ty_of(&values, &fwd, v);
                    Inst::InsertElt { v, e: e2, i }
                }
                8 => {
                    let a = vt!();
                    let bb = rel!();
                    let mask = vt!();
                    let (n, elt) = match (ty_of(&values, &fwd, mask).and_then(|t| self.types.get(t)), ty_of(&values, &fwd, a).and_then(|t| self.types.get(t))) {
                        (Some(Type::Vector(n, _)), Some(Type::Vector(_, e))) => (*n, *e),
                        (x, y) => return Err(format!("shufflevector sin vectores: máscara {x:?}, operando {y:?} (valor {a} de {next})")),
                    };
                    res_ty = Some(self.vec_type(n, elt));
                    Inst::Shuffle { a, b: bb, mask }
                }
                9 | 28 => {
                    let a = vt!();
                    let bb = rel!();
                    let pred = o[p] as u32;
                    let at = ty_of(&values, &fwd, a).unwrap_or(0);
                    let boolean = self.int_type(1);
                    res_ty = Some(match self.types.get(at) {
                        Some(Type::Vector(n, _)) => {
                            let n = *n;
                            self.vec_type(n, boolean)
                        }
                        _ => boolean,
                    });
                    Inst::Cmp { pred, a, b: bb }
                }
                29 => {
                    let t = vt!();
                    let f = rel!();
                    let c = vt!();
                    res_ty = ty_of(&values, &fwd, t);
                    Inst::Select { c, t, f }
                }
                26 => {
                    let v = vt!();
                    let idx: Vec<u32> = o[p..].iter().map(|&x| x as u32).collect();
                    let mut t = ty_of(&values, &fwd, v).unwrap_or(0);
                    for &i in &idx {
                        t = match self.ty(t)? {
                            Type::Struct { elems, .. } => elems[i as usize],
                            Type::Array(_, e) | Type::Vector(_, e) => *e,
                            x => return Err(format!("extractvalue en {x:?}")),
                        };
                    }
                    res_ty = Some(t);
                    Inst::ExtractVal { v, idx }
                }
                27 => {
                    let agg = vt!();
                    let v = vt!();
                    let idx: Vec<u32> = o[p..].iter().map(|&x| x as u32).collect();
                    res_ty = ty_of(&values, &fwd, agg);
                    Inst::InsertVal { agg, v, idx }
                }
                20 => {
                    let ptr = vt!();
                    res_ty = Some(o[p] as usize);
                    let align = (1u32 << (o[p + 1] & 0x1f)) >> 1;
                    Inst::Load { ptr, align }
                }
                41 => {
                    let ptr = vt!();
                    res_ty = Some(o[p] as usize);
                    Inst::Load { ptr, align: 0 }
                }
                44 | 45 => {
                    let ptr = vt!();
                    let val = vt!();
                    let align = (1u32 << (o.get(p).copied().unwrap_or(0) & 0x1f)) >> 1;
                    Inst::Store { ptr, val, align }
                }
                43 => {
                    let inbounds = o[0] & 1 != 0;
                    let src_ty = o[1] as usize;
                    p = 2;
                    let ptr = vt!();
                    let mut idx = Vec::new();
                    while p < o.len() {
                        idx.push(vt!());
                    }
                    // tipo: puntero del mismo espacio, o vector de punteros si alguno lo es
                    res_ty = ty_of(&values, &fwd, ptr);
                    for &i in &idx {
                        if let Some(Type::Vector(n, _)) = ty_of(&values, &fwd, i).and_then(|t| self.types.get(t)) {
                            let n = *n;
                            let pt = res_ty.unwrap_or(0);
                            res_ty = Some(self.vec_type(n, pt));
                        }
                    }
                    Inst::Gep { src_ty, ptr, idx, inbounds }
                }
                19 => {
                    // ALLOCA: [tipo asignado, tipo del número, número (absoluto), alineación y banderas]
                    let ty = o[0] as usize;
                    let n = o[2] as usize;
                    let a = o[3];
                    let align = ((1u64 << ((a & 0x1f) | ((a >> 8) & 0x7) << 5)) >> 1) as u32;
                    res_ty = Some(self.ptr_type(0));
                    Inst::Alloca { ty, n, align }
                }
                34 => {
                    // CALL: [atributos, cc, (fmf), (tipo de función), callee, args...]
                    let cc = o[1];
                    p = 2;
                    if cc & (1 << 17) != 0 {
                        p += 1; // banderas fast-math
                    }
                    let explicit = cc & (1 << 15) != 0;
                    let mut fnty = if explicit {
                        p += 1;
                        Some(o[p - 1] as usize)
                    } else {
                        None
                    };
                    let callee = vt!();
                    if fnty.is_none() {
                        fnty = match values.get(callee).map(|v| &v.kind) {
                            Some(ValueKind::Function(f)) => Some(self.functions[*f].ty),
                            _ => None,
                        };
                    }
                    let fnty = fnty.ok_or("llamada sin tipo de función")?;
                    let (ret, params, vararg) = match self.ty(fnty)? {
                        Type::Function { ret, params, vararg } => (*ret, params.clone(), *vararg),
                        t => return Err(format!("llamada con tipo {t:?}")),
                    };
                    let mut args = Vec::new();
                    for &pt in &params {
                        if matches!(self.ty(pt)?, Type::Metadata) {
                            // argumento de metadatos: número de metadatos (no de valor)
                            let m = o[p] as usize;
                            p += 1;
                            args.push(values.len() + 1_000_000_000 + m); // marcado: no es un valor
                        } else {
                            args.push(rel!());
                        }
                    }
                    if vararg {
                        while p < o.len() {
                            args.push(vt!());
                        }
                    }
                    res_ty = Some(ret);
                    Inst::Call { fnty, callee, args }
                }
                16 => {
                    // PHI: [tipo, (valor con signo relativo, bloque)*, (fmf)]
                    res_ty = Some(o[0] as usize);
                    let mut inc = Vec::new();
                    let mut q = 1;
                    while q + 1 < o.len() {
                        let v = (next as i64 - signo(o[q])) as usize;
                        inc.push((v, o[q + 1] as usize));
                        q += 2;
                    }
                    Inst::Phi { incoming: inc }
                }
                11 => {
                    if o.len() == 1 {
                        Inst::Br(o[0] as usize)
                    } else {
                        p = 2;
                        let c = rel!();
                        Inst::CondBr { c, t: o[0] as usize, f: o[1] as usize }
                    }
                }
                12 => {
                    // SWITCH: [tipo, condición (relativa), defecto, (valor absoluto, bloque)*]
                    p = 1;
                    let v = rel!();
                    let default = o[2] as usize;
                    let cases = o[3..].chunks(2).filter(|c| c.len() == 2).map(|c| (c[0] as usize, c[1] as usize)).collect();
                    Inst::Switch { v, default, cases }
                }
                10 => {
                    if o.is_empty() {
                        Inst::Ret(None)
                    } else {
                        Inst::Ret(Some(vt!()))
                    }
                }
                15 => Inst::Unreachable,
                58 => {
                    let v = vt!();
                    res_ty = ty_of(&values, &fwd, v);
                    Inst::Freeze(v)
                }
                59 => {
                    let ptr = vt!();
                    let val = vt!();
                    res_ty = ty_of(&values, &fwd, val);
                    Inst::AtomicRmw { op: o[p] as u32, ptr, val }
                }
                46 => {
                    let ptr = vt!();
                    let cmp = vt!();
                    let new = rel!();
                    let it = ty_of(&values, &fwd, cmp).unwrap_or(0);
                    let b1 = self.int_type(1);
                    res_ty = Some(self.struct_type(vec![it, b1]));
                    Inst::CmpXchg { ptr, cmp, new }
                }
                36 => Inst::Fence,
                33 | 35 | 60 | 55 => continue, // DEBUG_LOC_AGAIN, DEBUG_LOC, BLOCKADDR_USERS, OPERAND_BUNDLE
                c => return Err(format!("instrucción con código {c} no soportada")),
            };
            let termina = matches!(inst, Inst::Br(_) | Inst::CondBr { .. } | Inst::Switch { .. } | Inst::Ret(_) | Inst::Unreachable);
            let ty = res_ty.unwrap_or_else(|| void.unwrap_or(0));
            let da_valor = res_ty.is_some() && Some(ty) != void;
            let val = if da_valor {
                values.push(Value { ty, kind: ValueKind::Inst(cur, blocks.get(cur).map(|b| b.insts.len()).unwrap_or(0)) });
                Some(next)
            } else {
                None
            };
            blocks.get_mut(cur).ok_or("instrucción fuera de los bloques declarados")?.insts.push(Instr { inst, ty, val });
            if termina {
                cur += 1;
            }
        }
        if std::env::var_os("TAHOE_AIR_DEBUG").is_some() {
            let consts = values.iter().skip(self.values.len()).filter(|v| matches!(v.kind, ValueKind::Const(_))).count();
            eprintln!("{}: módulo {} valores, {} argumentos, {} constantes locales, {} en total", self.functions[fi].name, self.values.len(), params.len(), consts, values.len());
        }
        let f = &mut self.functions[fi];
        f.values = values;
        f.args = args;
        f.blocks = blocks;
        Ok(())
    }

    fn int_type(&mut self, w: u32) -> TypeId {
        self.find_or_add(Type::Int(w))
    }
    fn vec_type(&mut self, n: u32, e: TypeId) -> TypeId {
        self.find_or_add(Type::Vector(n, e))
    }
    fn struct_type(&mut self, elems: Vec<TypeId>) -> TypeId {
        self.find_or_add(Type::Struct { name: None, packed: false, elems })
    }
    fn find_or_add(&mut self, t: Type) -> TypeId {
        if let Some(i) = self.types.iter().position(|x| *x == t) {
            return i;
        }
        self.types.push(t);
        self.types.len() - 1
    }
}

/// Lector de bits para el blob de METADATA_STRINGS (las longitudes van en VBR6).
struct BitLector<'a> {
    d: &'a [u8],
    pos: usize,
}

impl BitLector<'_> {
    fn bits(&mut self, n: usize) -> Res<u64> {
        let mut v = 0u64;
        for i in 0..n {
            let byte = *self.d.get(self.pos / 8).ok_or("fin del blob")?;
            v |= (((byte >> (self.pos % 8)) & 1) as u64) << i;
            self.pos += 1;
        }
        Ok(v)
    }
    fn vbr(&mut self, n: usize) -> Res<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let piece = self.bits(n)?;
            v |= (piece & ((1 << (n - 1)) - 1)) << shift;
            if piece & (1 << (n - 1)) == 0 {
                return Ok(v);
            }
            shift += n - 1;
        }
    }
}

/// Lee un módulo de bitcode (con o sin la envoltura 0x0B17C0DE).
pub fn leer(datos: &[u8]) -> Res<Module> {
    let bc = Bitcode::new(datos).map_err(|e| format!("bitcode: {e:?}"))?;
    let elems: Vec<Elem> = bc.elements.into_iter().map(convertir).collect();
    let mut l = Lector { types: Vec::new(), values: Vec::new(), functions: Vec::new(), globals: Vec::new(), md: Vec::new(), named_md: HashMap::new(), strtab: Vec::new(), triple: String::new() };
    for e in &elems {
        if let Elem::Blk(b) = e {
            if b.id == STRTAB {
                if let Some(r) = b.recs().find(|r| r.code == 1) {
                    l.strtab = r.blob.clone().unwrap_or_default();
                }
            }
        }
    }
    let m = elems.iter().find_map(|e| if let Elem::Blk(b) = e { (b.id == MODULE).then_some(b) } else { None }).ok_or("no hay bloque de módulo")?;
    l.modulo(m)?;
    Ok(Module { types: l.types, functions: l.functions, globals: l.globals, values: l.values, md: l.md, named_md: l.named_md, triple: l.triple })
}

impl Module {
    pub fn md_str(&self, i: usize) -> Option<&str> {
        match self.md.get(i)? {
            Md::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn md_node(&self, i: usize) -> Option<&[Option<usize>]> {
        match self.md.get(i)? {
            Md::Node(n) => Some(n),
            _ => None,
        }
    }
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|f| f.name == name)
    }
}
