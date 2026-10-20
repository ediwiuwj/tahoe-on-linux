//! Traducción de la función de entrada: interfaz (argumentos, builtins, recursos, salidas), control de flujo e
//! instrucciones (incluidas las funciones `air.*` y `llvm.*`). Ver la descripción general en `spirv.rs`.

use super::*;

/// Traduce el punto de entrada `e` del módulo.
pub fn traducir(m: &Module, e: &Entrada, op: &Opciones) -> Res<Traduccion> {
    // estructurado si se puede; si el CFG no se deja (irreducible...), máquina de estados
    if std::env::var_os("TAHOE_MAQUINA").is_none() {
        match traducir_modo(m, e, op, true) {
            Ok(t) => return Ok(t),
            Err(err) if err.starts_with("estructura: ") => {}
            Err(err) => return Err(err),
        }
    }
    traducir_modo(m, e, op, false)
}

fn traducir_modo(m: &Module, e: &Entrada, op: &Opciones, estructurado: bool) -> Res<Traduccion> {
    let esp = fc::especializar(m, &op.constantes)?;
    let f = inline::inline_todo(m, e.funcion)?;
    let an = sccp::analizar(m, &f, &esp.memoria, &esp.definidas);
    let mut b = Builder::new();
    b.set_version(1, 5); // Vulkan 1.2 admite SPIR-V 1.5 (interfaz con todas las globales, select de vectores)
    for c in [Capability::Shader, Capability::ImageQuery, Capability::StorageImageWriteWithoutFormat, Capability::SampledImageArrayDynamicIndexing] {
        b.capability(c);
    }
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);
    let glsl = b.ext_inst_import("GLSL.std.450");
    let t_void = b.type_void();
    let t_bool = b.type_bool();
    let t_u32 = b.type_int(32, 0);
    let t_i32 = b.type_int(32, 1);
    let t_f32 = b.type_float(32);
    let t_sampler = b.type_sampler();
    let reach = an.alcanzables.iter().filter(|&&x| x).count();
    let mut t = Tr {
        m,
        f: &f,
        an: &an,
        mem: &esp.memoria,
        ent: e,
        op,
        b,
        glsl,
        t_void,
        t_bool,
        t_u32,
        t_i32,
        t_f32,
        tcache: HashMap::new(),
        vcache: HashMap::new(),
        pcache: HashMap::new(),
        acache: HashMap::new(),
        ccache: HashMap::new(),
        vals: vec![None; f.values.len()],
        buffers: HashMap::new(),
        buffers_escritos: Vec::new(),
        texturas: HashMap::new(),
        samplers: HashMap::new(),
        t_sampler,
        enlaces: Vec::new(),
        interfaz: Vec::new(),
        builtins: HashMap::new(),
        privadas: HashMap::new(),
        globales: HashMap::new(),
        multi: reach > 1,
        estructurado,
        sin_terminador: false,
        var_destino: 0,
        n_lazos: 0,
        cabeceras_lazo: Vec::new(),
        vars: HashMap::new(),
        var_estado: 0,
        etiquetas: HashMap::new(),
        sel_merge: 0,
        salidas: Vec::new(),
        varyings_out: Vec::new(),
        atributos: Vec::new(),
        colores: Vec::new(),
        grid: None,
        usa_demote: false,
        usa_subgrupos: false,
        escribe_profundidad: false,
        capacidades: Vec::new(),
        set: op.set,
        bases: HashMap::new(),
        fijos: HashMap::new(),
        wg_size: None,
        t_bloque: None,
        tipos_textura: HashMap::new(),
        wg_args: HashMap::new(),
    };
    t.emitir()?;
    // recursos usados
    let mut enlaces = std::mem::take(&mut t.enlaces);
    let mut bufs: Vec<u32> = t.buffers.keys().copied().collect();
    bufs.sort();
    for i in bufs {
        let escritura = t.buffers_escritos.contains(&i);
        if !escritura {
            let v = t.buffers[&i];
            t.b.decorate(v, Decoration::NonWritable, []);
        }
        enlaces.push(Enlace { binding: i, recurso: Recurso::Buffer { indice: i, escritura } });
    }
    if t.usa_demote {
        t.b.capability(Capability::DemoteToHelperInvocation);
        t.b.extension("SPV_EXT_demote_to_helper_invocation");
    }
    let mut caps = std::mem::take(&mut t.capacidades);
    caps.sort_by_key(|c| *c as u32);
    caps.dedup();
    for c in caps {
        t.b.capability(c);
    }
    if t.usa_subgrupos {
        // (sin GroupNonUniformArithmetic: la GPU no la tiene; las reducciones van con shuffles)
        for c in [Capability::GroupNonUniform, Capability::GroupNonUniformShuffle, Capability::GroupNonUniformShuffleRelative, Capability::GroupNonUniformQuad, Capability::GroupNonUniformBallot, Capability::GroupNonUniformVote] {
            t.b.capability(c);
        }
    }
    let colores = t.colores.clone();
    let varyings = t.varyings_out.clone();
    let atributos = t.atributos.clone();
    let spirv = t.b.module().assemble();
    Ok(Traduccion { spirv, etapa: e.etapa, enlaces, varyings, atributos, colores, bloques: reach, maquina_de_estados: reach > 1 && !estructurado, subgrupos: t.usa_subgrupos })
}

fn nombre_air<'a>(m: &'a Module, f: &Function, callee: ValueId) -> &'a str {
    match f.values.get(callee).map(|v| &v.kind) {
        Some(ValueKind::Function(g)) => &m.functions[*g].name,
        _ => "",
    }
}

impl Tr<'_> {
    fn emitir(&mut self) -> Res<()> {
        let modelo = match self.ent.etapa {
            Etapa::Kernel => ExecutionModel::GLCompute,
            Etapa::Vertex => ExecutionModel::Vertex,
            Etapa::Fragment => ExecutionModel::Fragment,
        };
        if self.ent.etapa == Etapa::Kernel {
            // tamaño del grupo: constantes de especialización 0, 1 y 2 (lo fija la app al despachar)
            let v3 = self.vec_t(self.t_u32, 3);
            let x = self.b.spec_constant_bit32(self.t_u32, 1);
            let y = self.b.spec_constant_bit32(self.t_u32, 1);
            let z = self.b.spec_constant_bit32(self.t_u32, 1);
            for (k, c) in [x, y, z].into_iter().enumerate() {
                self.b.decorate(c, Decoration::SpecId, [Operand::LiteralBit32(k as u32)]);
            }
            let wg = self.b.spec_constant_composite(v3, [x, y, z]);
            self.b.decorate(wg, Decoration::BuiltIn, [Operand::BuiltIn(BuiltIn::WorkgroupSize)]);
            self.wg_size = Some(wg);
        }
        // variables de la función (antes del cuerpo): las de los valores que cruzan bloques, los phi y las allocas
        let fty = self.b.type_function(self.t_void, []);
        let fun = self.b.begin_function(self.t_void, None, FunctionControl::NONE, fty).map_err(|e| format!("{e:?}"))?;
        self.b.begin_block(None).unwrap();
        self.preparar_variables()?;
        self.argumentos()?;
        self.analizar_bases();
        if self.ent.etapa == Etapa::Kernel {
            self.limite_de_rejilla()?;
        }
        let orden: Vec<usize> = self.an.orden.clone();
        if !self.multi {
            for &bb in &orden {
                self.bloque(bb)?;
            }
        } else if self.estructurado {
            let cero = self.cu(0);
            self.b.store(self.var_destino, cero, None, []).unwrap();
            self.emitir_estructurado().map_err(|e| format!("estructura: {e}"))?;
        } else {
            // máquina de estados: entrada → cabecera (bucle) → switch sobre el bloque → caso → fusión → continuar
            let cab = self.b.id();
            let disp = self.b.id();
            let cont = self.b.id();
            let fin = self.b.id();
            self.sel_merge = self.b.id();
            for &bb in &orden {
                let l = self.b.id();
                self.etiquetas.insert(bb, l);
            }
            let cero = self.cu(orden.first().copied().unwrap_or(0) as u32);
            self.b.store(self.var_estado, cero, None, []).unwrap();
            self.b.branch(cab).unwrap();
            self.b.begin_block(Some(cab)).unwrap();
            self.b.loop_merge(fin, cont, spirv::LoopControl::NONE, []).unwrap();
            self.b.branch(disp).unwrap();
            self.b.begin_block(Some(disp)).unwrap();
            let s = self.b.load(self.t_u32, None, self.var_estado, None, []).unwrap();
            self.b.selection_merge(self.sel_merge, SelectionControl::NONE).unwrap();
            let casos: Vec<(Operand, Word)> = orden.iter().map(|bb| (Operand::LiteralBit32(*bb as u32), self.etiquetas[bb])).collect();
            self.b.switch(s, self.sel_merge, casos).unwrap();
            for &bb in &orden {
                let l = self.etiquetas[&bb];
                self.b.begin_block(Some(l)).unwrap();
                self.bloque(bb)?;
            }
            self.b.begin_block(Some(self.sel_merge)).unwrap();
            self.b.branch(cont).unwrap();
            self.b.begin_block(Some(cont)).unwrap();
            self.b.branch(cab).unwrap();
            self.b.begin_block(Some(fin)).unwrap();
            self.b.unreachable().unwrap();
        }
        self.b.end_function().unwrap();
        let nombre = "main";
        let interfaz = self.interfaz.clone();
        self.b.entry_point(modelo, fun, nombre, interfaz);
        match self.ent.etapa {
            Etapa::Kernel => {}
            Etapa::Fragment => {
                self.b.execution_mode(fun, ExecutionMode::OriginUpperLeft, []);
                if self.escribe_profundidad {
                    // Sin este modo, el SPIR-V con OpStore %gl_FragDepth no valida (VUID-FragDepth-04216).
                    self.b.execution_mode(fun, ExecutionMode::DepthReplacing, []);
                }
            }
            Etapa::Vertex => {}
        }
        Ok(())
    }

    fn preparar_variables(&mut self) -> Res<()> {
        let f = self.f;
        // allocas: memoria privada en palabras
        for blk in &f.blocks {
            for ins in &blk.insts {
                if let Inst::Alloca { ty, n, .. } = &ins.inst {
                    let k = self.an.constantes.get(*n).cloned().flatten().and_then(|x| x.int()).unwrap_or(1).max(1);
                    let words = (layout::size(self.m, *ty) * k).div_ceil(4).max(1) as u32;
                    let at = self.arr_t(self.t_u32, words);
                    let pt = self.ptr_t(StorageClass::Function, at);
                    let v = self.b.variable(pt, None, StorageClass::Function, None);
                    self.privadas.insert(ins.val.unwrap(), v);
                }
            }
        }
        if !self.multi {
            return Ok(());
        }
        let pu = self.ptr_t(StorageClass::Function, self.t_u32);
        self.var_estado = self.b.variable(pu, None, StorageClass::Function, None);
        self.var_destino = self.b.variable(pu, None, StorageClass::Function, None);
        // bloque de definición de cada valor y usos fuera de él
        let mut def_bloque: HashMap<ValueId, usize> = HashMap::new();
        for (bi, blk) in f.blocks.iter().enumerate() {
            for ins in &blk.insts {
                if let Some(v) = ins.val {
                    def_bloque.insert(v, bi);
                }
            }
        }
        let mut cruzan: Vec<ValueId> = Vec::new();
        for (bi, blk) in f.blocks.iter().enumerate() {
            if !self.an.alcanzables[bi] {
                continue;
            }
            for ins in &blk.insts {
                let usados: Vec<ValueId> = match &ins.inst {
                    Inst::Phi { incoming } => incoming.iter().map(|(v, _)| *v).collect(),
                    other => operandos(other),
                };
                let es_phi = matches!(ins.inst, Inst::Phi { .. });
                for u in usados {
                    if let Some(&db) = def_bloque.get(&u) {
                        if (db != bi || es_phi) && !cruzan.contains(&u) && self.an.constantes.get(u).cloned().flatten().is_none() {
                            cruzan.push(u);
                        }
                    }
                }
                if es_phi {
                    if let Some(v) = ins.val {
                        if !cruzan.contains(&v) {
                            cruzan.push(v);
                        }
                    }
                }
            }
        }
        for v in cruzan {
            let t = f.values[v].ty;
            // punteros: la variable guarda el desplazamiento; texturas y samplers no cruzan por variables
            let st = if espacio(self.m, t).is_some() { self.t_u32 } else { self.ty(t)? };
            let pt = self.ptr_t(StorageClass::Function, st);
            let var = self.b.variable(pt, None, StorageClass::Function, None);
            self.vars.insert(v, var);
        }
        Ok(())
    }

    // ------------------------------------------------------------ interfaz de entrada

    fn builtin(&mut self, bi: BuiltIn, ty: Word, sc: StorageClass) -> Word {
        if let Some(&(v, _)) = self.builtins.get(&bi) {
            return v;
        }
        let pt = self.ptr_t(sc, ty);
        let v = self.var_global(pt, sc, None);
        self.b.decorate(v, Decoration::BuiltIn, [Operand::BuiltIn(bi)]);
        self.builtins.insert(bi, (v, ty));
        self.interfaz.push(v);
        v
    }

    /// Valor de un builtin de entrada convertido al tipo del argumento (uint, uint2, uint3, ushort...).
    fn leer_builtin(&mut self, bi: BuiltIn, n_bi: u32, t_arg: TypeId) -> Res<Word> {
        let tb = self.vec_t(self.t_u32, n_bi);
        let var = self.builtin(bi, tb, StorageClass::Input);
        let x = self.b.load(tb, None, var, None, []).unwrap();
        let n_arg = match self.m.types[t_arg] {
            Type::Vector(n, _) => n,
            _ => 1,
        };
        Ok(if n_arg == n_bi {
            x
        } else if n_arg == 1 {
            self.b.composite_extract(self.t_u32, None, x, [0]).unwrap()
        } else {
            let st = self.vec_t(self.t_u32, n_arg);
            let comps: Vec<u32> = (0..n_arg).collect();
            self.b.vector_shuffle(st, None, x, x, comps).unwrap()
        })
    }

    fn argumentos(&mut self) -> Res<()> {
        let args = self.f.args.clone();
        let descs = self.ent.argumentos.clone();
        for d in &descs {
            let Some(&v) = args.get(d.indice as usize) else { continue };
            let t_arg = self.f.values[v].ty;
            if let Some(pred) = self.predicado(d) {
                if !pred {
                    self.vals[v] = Some(TV::C(V::Undef));
                    continue;
                }
            }
            let tv = match d.clase.as_str() {
                "air.buffer" | "air.indirect_buffer" => {
                    let i = d.int_tras("air.location_index").unwrap_or(0) as u32;
                    if d.int_tras("air.address_space") == Some(3) { TV::Ptr(Base::WgArg(i), Off::C(0)) } else { TV::Ptr(Base::Buf(i), Off::C(0)) }
                }
                "air.texture" => {
                    let i = d.int_tras("air.location_index").unwrap_or(0) as u32;
                    let nombre = d.txt_tras("air.arg_type_name").unwrap_or("");
                    let k = if nombre.contains("<uint") || nombre.contains("<ushort") || nombre.contains("<uchar") { 1 } else if nombre.contains("<int") || nombre.contains("<short") || nombre.contains("<char") { 2 } else { 0 };
                    self.tipos_textura.insert(i, k);
                    TV::Tex(i)
                }
                "air.sampler" => TV::Smp(Smp::Arg(d.int_tras("air.location_index").unwrap_or(0) as u32)),
                "air.thread_position_in_grid" => TV::Id(self.leer_builtin(BuiltIn::GlobalInvocationId, 3, t_arg)?),
                "air.thread_position_in_threadgroup" => TV::Id(self.leer_builtin(BuiltIn::LocalInvocationId, 3, t_arg)?),
                "air.threadgroup_position_in_grid" => TV::Id(self.leer_builtin(BuiltIn::WorkgroupId, 3, t_arg)?),
                "air.threadgroups_per_grid" => TV::Id(self.leer_builtin(BuiltIn::NumWorkgroups, 3, t_arg)?),
                "air.thread_index_in_threadgroup" => TV::Id(self.leer_builtin(BuiltIn::LocalInvocationIndex, 1, t_arg)?),
                "air.threads_per_threadgroup" => {
                    let g = self.wg_size.ok_or("tamaño de grupo fuera de un kernel")?;
                    let n = match self.m.types[t_arg] { Type::Vector(n, _) => n, _ => 1 };
                    TV::Id(if n == 3 { g } else if n == 1 { self.b.composite_extract(self.t_u32, None, g, [0]).unwrap() } else {
                        let st = self.vec_t(self.t_u32, n);
                        self.b.vector_shuffle(st, None, g, g, [0, 1]).unwrap()
                    })
                }
                "air.threads_per_grid" => {
                    let g = self.rejilla()?;
                    let n = match self.m.types[t_arg] { Type::Vector(n, _) => n, _ => 1 };
                    TV::Id(if n == 3 { g } else if n == 1 { self.b.composite_extract(self.t_u32, None, g, [0]).unwrap() } else {
                        let st = self.vec_t(self.t_u32, n);
                        self.b.vector_shuffle(st, None, g, g, [0, 1]).unwrap()
                    })
                }
                "air.simdgroups_per_threadgroup" => {
                    self.usa_subgrupos = true;
                    TV::Id(self.leer_builtin(BuiltIn::NumSubgroups, 1, t_arg)?)
                }
                "air.thread_index_in_simdgroup" => {
                    self.usa_subgrupos = true;
                    TV::Id(self.leer_builtin(BuiltIn::SubgroupLocalInvocationId, 1, t_arg)?)
                }
                "air.simdgroup_index_in_threadgroup" => {
                    self.usa_subgrupos = true;
                    TV::Id(self.leer_builtin(BuiltIn::SubgroupId, 1, t_arg)?)
                }
                "air.thread_index_in_quadgroup" | "air.quadgroup_index_in_threadgroup" | "air.quadgroups_per_threadgroup" => {
                    let x = match d.clase.as_str() {
                        "air.thread_index_in_quadgroup" => {
                            let l = self.leer_builtin(BuiltIn::SubgroupLocalInvocationId, 1, t_arg)?;
                            self.usa_subgrupos = true;
                            let tres = self.cu(3);
                            self.b.bitwise_and(self.t_u32, None, l, tres).unwrap()
                        }
                        "air.quadgroup_index_in_threadgroup" => {
                            let l = self.leer_builtin(BuiltIn::LocalInvocationIndex, 1, t_arg)?;
                            let dos = self.cu(2);
                            self.b.shift_right_logical(self.t_u32, None, l, dos).unwrap()
                        }
                        _ => {
                            let g = self.wg_size.ok_or("tamaño de grupo fuera de un kernel")?;
                            let gx = self.b.composite_extract(self.t_u32, None, g, [0]).unwrap();
                            let gy = self.b.composite_extract(self.t_u32, None, g, [1]).unwrap();
                            let gz = self.b.composite_extract(self.t_u32, None, g, [2]).unwrap();
                            let xy = self.b.i_mul(self.t_u32, None, gx, gy).unwrap();
                            let n = self.b.i_mul(self.t_u32, None, xy, gz).unwrap();
                            let tres = self.cu(3);
                            let n3 = self.b.i_add(self.t_u32, None, n, tres).unwrap();
                            let dos = self.cu(2);
                            self.b.shift_right_logical(self.t_u32, None, n3, dos).unwrap()
                        }
                    };
                    TV::Id(x)
                }
                "air.threads_per_simdgroup" => {
                    self.usa_subgrupos = true;
                    TV::Id(self.leer_builtin(BuiltIn::SubgroupSize, 1, t_arg)?)
                }
                "air.vertex_id" => TV::Id(self.leer_entero(BuiltIn::VertexIndex)?),
                "air.instance_id" => TV::Id(self.leer_entero(BuiltIn::InstanceIndex)?),
                "air.base_vertex" => TV::Id(self.leer_entero(BuiltIn::BaseVertex)?),
                "air.base_instance" => TV::Id(self.leer_entero(BuiltIn::BaseInstance)?),
                "air.vertex_input" => {
                    let attr = d.int_tras("air.location_index").unwrap_or(0) as u32;
                    let loc = self.atributos.len() as u32;
                    let st = self.ty(t_arg)?;
                    let pt = self.ptr_t(StorageClass::Input, st);
                    let var = self.var_global(pt, StorageClass::Input, None);
                    self.b.decorate(var, Decoration::Location, [Operand::LiteralBit32(attr)]);
                    self.interfaz.push(var);
                    self.atributos.push((attr, loc, super::super::tipo(self.m, t_arg)));
                    TV::Id(self.b.load(st, None, var, None, []).unwrap())
                }
                "air.position" if self.ent.etapa == Etapa::Fragment => {
                    let v4 = self.vec_t(self.t_f32, 4);
                    let var = self.builtin(BuiltIn::FragCoord, v4, StorageClass::Input);
                    TV::Id(self.b.load(v4, None, var, None, []).unwrap())
                }
                "air.front_facing" => {
                    let var = self.builtin(BuiltIn::FrontFacing, self.t_bool, StorageClass::Input);
                    TV::Id(self.b.load(self.t_bool, None, var, None, []).unwrap())
                }
                "air.fragment_input" => {
                    let nombre = d.datos.iter().find_map(|x| match x { super::super::Dato::Txt(s) if s != "air.fragment_input" && !s.starts_with("air.") => Some(s.clone()), _ => None }).unwrap_or_default();
                    let loc = match self.op.varyings.get(&nombre) {
                        Some(&l) => l,
                        None => self.varyings_out.len() as u32,
                    };
                    let st = self.ty(t_arg)?;
                    let pt = self.ptr_t(StorageClass::Input, st);
                    let var = self.var_global(pt, StorageClass::Input, None);
                    self.b.decorate(var, Decoration::Location, [Operand::LiteralBit32(loc)]);
                    self.interpolacion(var, d, t_arg);
                    self.interfaz.push(var);
                    self.varyings_out.push((nombre, loc));
                    TV::Id(self.b.load(st, None, var, None, []).unwrap())
                }
                otra => return Err(format!("argumento {otra} sin soporte aún")),
            };
            self.vals[v] = Some(tv);
        }
        // salidas: variables de salida (se escriben en cada ret)
        let salidas = self.ent.salidas.clone();
        let ret_ty = match &self.m.types[self.f.ty] {
            Type::Function { ret, .. } => *ret,
            _ => 0,
        };
        let campos: Vec<TypeId> = match &self.m.types[ret_ty] {
            Type::Struct { elems, .. } => elems.clone(),
            Type::Void => Vec::new(),
            _ => vec![ret_ty],
        };
        for (k, d) in salidas.iter().enumerate() {
            let Some(&tc) = campos.get(k) else { break };
            if self.predicado(d) == Some(false) {
                self.salidas.push((0, d.clone()));
                continue;
            }
            let st = self.ty(tc)?;
            let pt = self.ptr_t(StorageClass::Output, st);
            let var = self.var_global(pt, StorageClass::Output, None);
            match d.clase.as_str() {
                "air.position" => {
                    self.b.decorate(var, Decoration::BuiltIn, [Operand::BuiltIn(BuiltIn::Position)]);
                }
                "air.point_size" => self.b.decorate(var, Decoration::BuiltIn, [Operand::BuiltIn(BuiltIn::PointSize)]),
                "air.depth" => {
                    self.b.decorate(var, Decoration::BuiltIn, [Operand::BuiltIn(BuiltIn::FragDepth)]);
                    self.escribe_profundidad = true;
                }
                "air.vertex_output" => {
                    let nombre = d.propios().first().and_then(|x| match x { super::super::Dato::Txt(s) => Some(s.clone()), _ => None }).unwrap_or_default();
                    let loc = self.varyings_out.len() as u32;
                    self.b.decorate(var, Decoration::Location, [Operand::LiteralBit32(loc)]);
                    self.interpolacion(var, d, tc);
                    self.varyings_out.push((nombre, loc));
                }
                "air.render_target" => {
                    let rt = d.propios().first().and_then(|x| match x { super::super::Dato::Int(i) => Some(*i as u32), _ => None }).unwrap_or(0);
                    self.b.decorate(var, Decoration::Location, [Operand::LiteralBit32(rt)]);
                    self.colores.push(rt);
                }
                otra => return Err(format!("salida {otra} sin soporte aún")),
            }
            self.interfaz.push(var);
            self.salidas.push((var, d.clone()));
        }
        Ok(())
    }

    fn leer_entero(&mut self, bi: BuiltIn) -> Res<Word> {
        let var = self.builtin(bi, self.t_i32, StorageClass::Input);
        let x = self.b.load(self.t_i32, None, var, None, []).unwrap();
        Ok(self.b.bitcast(self.t_u32, None, x).unwrap())
    }

    fn interpolacion(&mut self, var: Word, d: &Descripcion, t: TypeId) {
        let entero = matches!(self.m.types[t], Type::Int(_)) || matches!(self.m.types[t], Type::Vector(_, e) if matches!(self.m.types[e], Type::Int(_)));
        if entero || d.tiene("air.flat") {
            self.b.decorate(var, Decoration::Flat, []);
        } else if d.tiene("air.no_perspective") {
            self.b.decorate(var, Decoration::NoPerspective, []);
        }
        if d.tiene("air.centroid") && !entero {
            self.b.decorate(var, Decoration::Centroid, []);
        }
    }

    /// Si el argumento o la salida depende de una function constant, su valor (tras especializar).
    fn predicado(&self, d: &Descripcion) -> Option<bool> {
        if !d.tiene("air.function_constant") && d.clase != "air.function_constant" {
            return None;
        }
        let g = d.datos.iter().find_map(|x| match x { super::super::Dato::Pred(g) => Some(*g), _ => None })?;
        // tras especializar, el predicado está en la memoria de su global (un bool de Metal: un byte)
        Some(self.mem.globales.get(g)?.first().copied().unwrap_or(0) != 0)
    }

    /// Tamaño de la rejilla (constantes de empuje: x, y, z en hilos).
    /// Tamaño de la rejilla de Metal (en hilos), de la push constant (se lee en el bloque de entrada).
    fn rejilla(&mut self) -> Res<Word> {
        if let Some(g) = self.grid {
            return Ok(g);
        }
        if self.ent.etapa != Etapa::Kernel {
            return Err("tamaño de rejilla fuera de un kernel".into());
        }
        let v3 = self.vec_t(self.t_u32, 3);
        let st = self.b.type_struct([v3]);
        self.b.decorate(st, Decoration::Block, []);
        self.b.member_decorate(st, 0, Decoration::Offset, [Operand::LiteralBit32(0)]);
        let pt = self.ptr_t(StorageClass::PushConstant, st);
        let var = self.var_global(pt, StorageClass::PushConstant, None);
        self.interfaz.push(var);
        let pv = self.ptr_t(StorageClass::PushConstant, v3);
        let cero = self.cu(0);
        let p = self.b.access_chain(pv, None, var, [cero]).unwrap();
        let g = self.b.load(v3, None, p, None, []).unwrap();
        self.grid = Some(g);
        Ok(g)
    }

    /// En un kernel: los hilos fuera de la rejilla de Metal (dispatchThreads con grupos que no la dividen) salen ya.
    fn limite_de_rejilla(&mut self) -> Res<()> {
        let v3 = self.vec_t(self.t_u32, 3);
        let g = self.rejilla()?;
        let gidv = self.builtin(BuiltIn::GlobalInvocationId, v3, StorageClass::Input);
        let gid = self.b.load(v3, None, gidv, None, []).unwrap();
        let bv3 = self.vec_t(self.t_bool, 3);
        let fuera = self.b.u_greater_than_equal(bv3, None, gid, g).unwrap();
        let alguno = self.b.any(self.t_bool, None, fuera).unwrap();
        let (salir, seguir) = (self.b.id(), self.b.id());
        self.b.selection_merge(seguir, SelectionControl::NONE).unwrap();
        self.b.branch_conditional(alguno, salir, seguir, []).unwrap();
        self.b.begin_block(Some(salir)).unwrap();
        self.b.ret().unwrap();
        self.b.begin_block(Some(seguir)).unwrap();
        Ok(())
    }

    // ------------------------------------------------------------ bloques

    fn bloque(&mut self, bb: usize) -> Res<()> {
        let f = self.f;
        let insts = &f.blocks[bb].insts;
        // los valores de otros bloques se releen de sus variables
        let cruzan: Vec<ValueId> = self.vars.keys().copied().collect();
        for v in cruzan {
            self.vals[v] = None;
        }
        // phis: desde sus variables
        for ins in insts {
            if let Inst::Phi { .. } = ins.inst {
                let v = ins.val.unwrap();
                if self.an.constantes.get(v).cloned().flatten().is_some() || self.fijos.contains_key(&v) {
                    continue;
                }
                let t = f.values[v].ty;
                if espacio(self.m, t).is_some() {
                    // phi de punteros: misma base en todas las entradas (se toma la de la primera ya traducida)
                    let Inst::Phi { incoming } = &ins.inst else { unreachable!() };
                    let base = self.bases.get(&v).copied().or_else(|| incoming.iter().find_map(|(x, _)| match self.tv(*x) { Ok(TV::Ptr(b, _)) => Some(b), _ => None }));
                    let var = *self.vars.get(&v).ok_or("phi sin variable")?;
                    let off = self.b.load(self.t_u32, None, var, None, []).unwrap();
                    self.vals[v] = Some(TV::Ptr(base.ok_or("phi de punteros sin base conocida")?, Off::Id(off)));
                } else {
                    let var = *self.vars.get(&v).ok_or("phi sin variable")?;
                    let st = self.ty(t)?;
                    let x = self.b.load(st, None, var, None, []).unwrap();
                    self.vals[v] = Some(TV::Id(x));
                }
            }
        }
        if self.b.selected_block().is_none() {
            return Err(format!("el bloque {bb} empieza cerrado"));
        }
        for ins in insts {
            if matches!(ins.inst, Inst::Phi { .. }) {
                continue;
            }
            if let Some(v) = ins.val {
                if let Some(Some(_)) = self.an.constantes.get(v) {
                    continue; // constante plegada
                }
            }
            if self.b.selected_block().is_none() {
                return Err(format!("el bloque {bb} quedó cerrado antes de {:?}", ins.inst));
            }
            let es_term = matches!(ins.inst, Inst::Br(_) | Inst::CondBr { .. } | Inst::Switch { .. } | Inst::Ret(_) | Inst::Unreachable);
            if es_term {
                if self.sin_terminador {
                    return Ok(());
                }
                self.terminador(bb, &ins.inst)?;
                return Ok(());
            }
            if let Inst::Alloca { .. } = ins.inst {
                let v = ins.val.unwrap();
                self.vals[v] = Some(TV::Ptr(Base::Priv(v), Off::C(0)));
                if self.vars.contains_key(&v) {
                    self.bases.insert(v, Base::Priv(v));
                    let (var, c) = (self.vars[&v], self.cu(0));
                    self.b.store(var, c, None, []).unwrap();
                }
                continue;
            }
            let r = self.instruccion(&ins.inst, ins.ty).map_err(|e| format!("{e} (en {:?})", ins.inst))?;
            if self.b.selected_block().is_none() {
                return Err(format!("{:?} cerró el bloque {bb}", ins.inst));
            }
            if let (Some(v), Some(r)) = (ins.val, r) {
                // si cruza bloques, a su variable
                if let Some(&var) = self.vars.get(&v) {
                    match &r {
                        TV::Id(id) => self.b.store(var, *id, None, []).unwrap(),
                        TV::Ptr(base, o) => {
                            self.bases.insert(v, *base);
                            let o = self.off_id(*o);
                            self.b.store(var, o, None, []).unwrap();
                        }
                        TV::C(c) => {
                            let t = f.values[v].ty;
                            let id = self.constante(t, c)?;
                            self.b.store(var, id, None, []).unwrap();
                        }
                        _ => {}
                    }
                }
                self.vals[v] = Some(r);
            }
        }
        Err(format!("bloque {bb} sin terminador"))
    }

    /// Base de cada puntero de la función, antes de traducir (para los que cruzan bloques y los phi, sobre todo en
    /// bucles, cuya entrada puede venir de un bloque aún sin traducir): argumentos, allocas y globales, y de ahí por
    /// GEP, casts, select y phi hasta que no cambia nada. Los que mezclan bases distintas no entran.
    fn analizar_bases(&mut self) {
        let f = self.f;
        let mut base: HashMap<ValueId, Option<Base>> = HashMap::new(); // None: bases distintas
        let inicial = |tr: &Self, v: ValueId| -> Option<Option<Base>> {
            if let Some(Some(TV::Ptr(b, _))) = tr.vals.get(v) {
                return Some(Some(*b));
            }
            match tr.an.constantes.get(v).cloned().flatten() {
                Some(V::Ptr(Mem::Global(g), _)) => Some(Some(tr.base_global(g))),
                _ => match f.values.get(v).map(|x| &x.kind) {
                    Some(ValueKind::Global(g)) => Some(Some(tr.base_global(*g))),
                    _ => None,
                },
            }
        };
        for _ in 0..64 {
            let mut cambio = false;
            for (bb, blk) in f.blocks.iter().enumerate() {
                if !self.an.alcanzables.get(bb).copied().unwrap_or(false) {
                    continue;
                }
                for ins in &blk.insts {
                    let Some(v) = ins.val else { continue };
                    if espacio(self.m, f.values[v].ty).is_none() {
                        continue;
                    }
                    let de = |x: ValueId, base: &HashMap<ValueId, Option<Base>>| -> Option<Option<Base>> { base.get(&x).copied().or_else(|| inicial(self, x)) };
                    let fuentes: Vec<ValueId> = match &ins.inst {
                        Inst::Alloca { .. } => {
                            if base.insert(v, Some(Base::Priv(v))) != Some(Some(Base::Priv(v))) {
                                cambio = true;
                            }
                            continue;
                        }
                        Inst::Gep { ptr, .. } => vec![*ptr],
                        Inst::Cast { v: x, .. } => vec![*x],
                        Inst::Select { t, f: fl, .. } => vec![*t, *fl],
                        Inst::Phi { incoming } => incoming.iter().filter(|(_, pb)| self.an.alcanzables.get(*pb).copied().unwrap_or(false)).map(|(x, _)| *x).collect(),
                        _ => continue,
                    };
                    // encuentro: las desconocidas (undef, nulos, aún sin saber) no cuentan
                    let mut r: Option<Option<Base>> = None;
                    for x in fuentes {
                        match de(x, &base) {
                            None | Some(Some(Base::Nula)) => {}
                            Some(b) => {
                                r = Some(match (r, b) {
                                    (None, b) => b,
                                    (Some(Some(a)), Some(b)) if a == b => Some(a),
                                    _ => None,
                                })
                            }
                        }
                    }
                    if let Some(r) = r {
                        if base.get(&v) != Some(&r) {
                            // solo baja: de una base a «distintas»
                            let nuevo = match base.get(&v) { Some(Some(a)) if Some(*a) != r => None, _ => r };
                            if base.get(&v) != Some(&nuevo) {
                                base.insert(v, nuevo);
                                cambio = true;
                            }
                        }
                    }
                }
            }
            if !cambio {
                break;
            }
        }
        for (v, b) in base {
            if let Some(b) = b {
                self.bases.entry(v).or_insert(b);
            }
        }
        // texturas y samplers: la misma en todas las entradas (sin contar la nula)
        for _ in 0..16 {
            let mut cambio = false;
            for (bb, blk) in f.blocks.iter().enumerate() {
                if !self.an.alcanzables.get(bb).copied().unwrap_or(false) {
                    continue;
                }
                for ins in &blk.insts {
                    let Some(v) = ins.val else { continue };
                    if self.fijos.contains_key(&v) {
                        continue;
                    }
                    let fuentes: Vec<ValueId> = match &ins.inst {
                        Inst::Select { t, f: fl, .. } => vec![*t, *fl],
                        Inst::Phi { incoming } => incoming.iter().filter(|(_, pb)| self.an.alcanzables.get(*pb).copied().unwrap_or(false)).map(|(x, _)| *x).collect(),
                        Inst::Cast { v: x, .. } => vec![*x],
                        _ => continue,
                    };
                    let mut r: Option<TV> = None;
                    let mut ok = true;
                    for x in fuentes {
                        let tvx = self.fijos.get(&x).cloned().or_else(|| match self.vals.get(x) {
                            Some(Some(t @ (TV::Tex(_) | TV::Smp(_)))) => Some(t.clone()),
                            _ => None,
                        });
                        let nula = match f.values[x].kind {
                            ValueKind::Inst(b2, k2) => matches!(&f.blocks[b2].insts[k2].inst, Inst::Call { callee, .. } if nombre_air(self.m, f, *callee).starts_with("air.get_null_texture")),
                            ValueKind::Const(_) => true,
                            _ => false,
                        };
                        // argumentos que no están en esta variante (function constants): undef, como la nula
                        let ausente = matches!(self.vals.get(x), Some(Some(TV::C(V::Undef)))) || matches!(self.fijos.get(&x), Some(TV::Ptr(Base::Nula, _)));
                        match (tvx, &r) {
                            (Some(TV::Ptr(Base::Nula, _)), _) => {}
                            (Some(t), None) => r = Some(t),
                            (Some(t), Some(a)) if t == *a => {}
                            (None, _) if nula || ausente => {}
                            _ => ok = false,
                        }
                    }
                    // solo nulas: es la textura nula
                    let r = if ok && r.is_none() && espacio(self.m, f.values[v].ty) == Some(1) { Some(TV::Ptr(Base::Nula, Off::C(0))) } else { r };
                    if std::env::var_os("TAHOE_AIR_DEBUG").is_some() && matches!(ins.inst, Inst::Phi { .. }) && espacio(self.m, f.values[v].ty).is_some() {
                        eprintln!("fijos: %{v} ok={ok} r={r:?} fuentes={:?}", match &ins.inst { Inst::Phi { incoming } => incoming.iter().map(|(x, _)| (self.vals.get(*x).cloned(), f.values[*x].kind.clone())).collect::<Vec<_>>(), _ => vec![] });
                    }
                    if let (true, Some(t)) = (ok, r) {
                        self.fijos.insert(v, t);
                        cambio = true;
                    }
                }
            }
            if !cambio {
                break;
            }
        }
    }

    /// Antes de saltar de `desde` a `hacia`: los phi de `hacia` reciben su valor (todos leídos antes de escribir).
    fn copiar_phis(&mut self, desde: usize, hacia: usize) -> Res<()> {
        let f = self.f;
        let mut pares = Vec::new();
        for ins in &f.blocks[hacia].insts {
            let Inst::Phi { incoming } = &ins.inst else { continue };
            let v = ins.val.unwrap();
            let Some(&var) = self.vars.get(&v) else { continue };
            if self.fijos.contains_key(&v) {
                continue;
            }
            let Some((x, _)) = incoming.iter().find(|(_, b)| *b == desde) else { continue };
            let t = f.values[v].ty;
            let tvx = self.tv(*x)?;
            let id = match tvx {
                TV::Ptr(_, o) => self.off_id(o),
                other => self.id_de(&other, t)?,
            };
            pares.push((var, id));
        }
        for (var, id) in pares {
            self.b.store(var, id, None, []).unwrap();
        }
        Ok(())
    }

    fn saltar(&mut self, desde: usize, hacia: usize) -> Res<()> {
        self.copiar_phis(desde, hacia)?;
        let c = self.cu(hacia as u32);
        self.b.store(self.var_estado, c, None, []).unwrap();
        Ok(())
    }

    fn terminador(&mut self, bb: usize, i: &Inst) -> Res<()> {
        match i {
            Inst::Ret(v) => {
                self.escribir_salidas(*v)?;
                self.b.ret().unwrap();
                return Ok(());
            }
            Inst::Unreachable => {
                self.b.ret().unwrap();
                return Ok(());
            }
            _ => {}
        }
        if !self.multi {
            return Err("salto en una función de un solo bloque".into());
        }
        match i {
            Inst::Br(t) => self.saltar(bb, *t)?,
            Inst::CondBr { c, t, f } => {
                match self.tv(*c)? {
                    TV::C(V::Int(_, x)) => self.saltar(bb, if x != 0 { *t } else { *f })?,
                    _ => {
                        let cid = self.id(*c)?;
                        // los phi de los dos destinos se rellenan en ramas separadas
                        let (lt, lf, lm) = (self.b.id(), self.b.id(), self.b.id());
                        self.b.selection_merge(lm, SelectionControl::NONE).unwrap();
                        self.b.branch_conditional(cid, lt, lf, []).unwrap();
                        self.b.begin_block(Some(lt)).unwrap();
                        self.saltar(bb, *t)?;
                        self.b.branch(lm).unwrap();
                        self.b.begin_block(Some(lf)).unwrap();
                        self.saltar(bb, *f)?;
                        self.b.branch(lm).unwrap();
                        self.b.begin_block(Some(lm)).unwrap();
                    }
                }
            }
            Inst::Switch { v, default, cases } => {
                let x = self.id(*v)?;
                let lm = self.b.id();
                let ld = self.b.id();
                let mut dest = Vec::new();
                let mut etiquetas = Vec::new();
                for (cv, b) in cases {
                    let k = match self.tv(*cv)? {
                        TV::C(V::Int(_, k)) => k as u32,
                        _ => return Err("caso de switch no constante".into()),
                    };
                    let l = self.b.id();
                    dest.push((Operand::LiteralBit32(k), l));
                    etiquetas.push((l, *b));
                }
                self.b.selection_merge(lm, SelectionControl::NONE).unwrap();
                self.b.switch(x, ld, dest).unwrap();
                for (l, b) in etiquetas {
                    self.b.begin_block(Some(l)).unwrap();
                    self.saltar(bb, b)?;
                    self.b.branch(lm).unwrap();
                }
                self.b.begin_block(Some(ld)).unwrap();
                self.saltar(bb, *default)?;
                self.b.branch(lm).unwrap();
                self.b.begin_block(Some(lm)).unwrap();
            }
            _ => unreachable!(),
        }
        self.b.branch(self.sel_merge).unwrap();
        Ok(())
    }

    fn escribir_salidas(&mut self, v: Option<ValueId>) -> Res<()> {
        let Some(v) = v else { return Ok(()) };
        if self.salidas.is_empty() {
            return Ok(());
        }
        let t = self.f.values[v].ty;
        let x = self.tv(v)?;
        let struct_ret = matches!(self.m.types[t], Type::Struct { .. });
        let salidas = self.salidas.clone();
        for (k, (var, _)) in salidas.iter().enumerate() {
            if *var == 0 {
                continue;
            }
            let (tc, val) = if struct_ret {
                let tc = match &self.m.types[t] {
                    Type::Struct { elems, .. } if k < elems.len() => elems[k],
                    _ => return Err(format!("salida {k} sin campo en la struct de retorno")),
                };
                let id = match &x {
                    TV::C(V::Agg(es)) if k < es.len() => self.constante(tc, &es[k])?,
                    TV::C(V::Agg(_)) => return Err(format!("salida {k} sin valor en la constante")),
                    TV::C(V::Undef) => {
                        let st = self.ty(tc)?;
                        self.b.constant_null(st)
                    }
                    other => {
                        let agg = self.id_de(other, t)?;
                        let st = self.ty(tc)?;
                        self.b.composite_extract(st, None, agg, [k as u32]).unwrap()
                    }
                };
                (tc, id)
            } else {
                (t, self.id_de(&x, t)?)
            };
            let _ = tc;
            self.b.store(*var, val, None, []).unwrap();
        }
        Ok(())
    }
}

/// Valores que usa una instrucción (sin los phi).
pub(super) fn operandos(i: &Inst) -> Vec<ValueId> {
    match i {
        Inst::Bin { a, b, .. } | Inst::Cmp { a, b, .. } => vec![*a, *b],
        Inst::FNeg(a) | Inst::Freeze(a) | Inst::Cast { v: a, .. } => vec![*a],
        Inst::Select { c, t, f } => vec![*c, *t, *f],
        Inst::ExtractElt { v, i } => vec![*v, *i],
        Inst::InsertElt { v, e, i } => vec![*v, *e, *i],
        Inst::Shuffle { a, b, mask } => vec![*a, *b, *mask],
        Inst::ExtractVal { v, .. } => vec![*v],
        Inst::InsertVal { agg, v, .. } => vec![*agg, *v],
        Inst::Load { ptr, .. } => vec![*ptr],
        Inst::Store { ptr, val, .. } => vec![*ptr, *val],
        Inst::Gep { ptr, idx, .. } => {
            let mut v = vec![*ptr];
            v.extend(idx);
            v
        }
        Inst::Alloca { n, .. } => vec![*n],
        Inst::Call { args, .. } => args.iter().copied().filter(|&a| a < 1_000_000_000).collect(),
        Inst::CondBr { c, .. } => vec![*c],
        Inst::Switch { v, .. } => vec![*v],
        Inst::Ret(Some(v)) => vec![*v],
        Inst::AtomicRmw { ptr, val, .. } => vec![*ptr, *val],
        Inst::CmpXchg { ptr, cmp, new } => vec![*ptr, *cmp, *new],
        _ => Vec::new(),
    }
}

#[path = "spirv_ops.rs"]
mod ops;
#[path = "spirv_estructura.rs"]
mod estructura;
