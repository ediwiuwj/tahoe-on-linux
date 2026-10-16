//! `tahoe-gpu probar <metallib> <kernel> [ancho alto]`: ejecuta un kernel de Metal traducido en la GPU y el mismo AIR
//! en el intérprete de la CPU, con las mismas entradas, y compara las salidas (buffers y texturas en las que escribe).
//! Para kernels sin memoria de threadgroup ni barreras (cada hilo es independiente en la CPU).

use std::collections::HashMap;

use ash::vk;

use crate::air::interp::{self, Mem, Memoria, V};
use crate::air::ir::{Module, Type};
use crate::air::spirv::{self, Recurso};
use crate::air::{self, Entrada};
use crate::metallib;
use crate::motor::{Enlazado, Forma, Motor};
use crate::vk::Res;

const TEX_ENTRADA: u32 = 512;
const BUF: usize = 1 << 16;

/// Valor de prueba de una textura de entrada en (x, y).
fn patron(x: u32, y: u32) -> [f32; 4] {
    let h = |v: f32| interp::f16_a_f32(interp::f32_a_f16(v));
    [h(x as f32 / 512.0), h(y as f32 / 512.0), h(((x ^ y) & 255) as f32 / 255.0), 1.0]
}

fn valor_arg(m: &Module, t: usize, xyz: [u32; 3]) -> V {
    match &m.types[t] {
        Type::Int(w) => V::Int(*w, xyz[0] as u64),
        Type::Vector(n, e) => {
            let w = match m.types[*e] { Type::Int(w) => w, _ => 32 };
            V::Vec((0..*n as usize).map(|k| V::Int(w, xyz[k] as u64)).collect())
        }
        _ => V::Undef,
    }
}

pub fn probar(a: Vec<String>) -> Res<()> {
    let [ruta, nombre] = [a.first(), a.get(1)].map(|x| x.cloned().unwrap_or_default());
    let ancho: u32 = a.get(2).and_then(|x| x.parse().ok()).unwrap_or(64);
    let alto: u32 = a.get(3).and_then(|x| x.parse().ok()).unwrap_or(64);
    let d = std::fs::read(&ruta).map_err(|e| format!("{ruta}: {e}"))?;
    let b = metallib::Biblioteca::leer(&d)?;
    let f = b.funciones.iter().find(|f| f.nombre == nombre).ok_or("no está esa función")?;
    let m = air::ir::leer(b.bitcode(f)?)?;
    let e: Entrada = air::entradas(&m).into_iter().next().ok_or("sin punto de entrada")?;
    let t = spirv::traducir(&m, &e, &Default::default())?;
    let motor = Motor::new()?;
    println!("{nombre}: {} palabras de SPIR-V, enlaces: {:?}", t.spirv.len(), t.enlaces.iter().map(|x| x.binding).collect::<Vec<_>>());

    // recursos: buffers con un patrón; texturas de entrada 512×512 y de salida ancho×alto (RGBA32F)
    let fmt = vk::Format::R32G32B32A32_SFLOAT;
    let mut buffers = HashMap::new();
    let mut texturas = HashMap::new();
    let mut tex_cpu: HashMap<u32, (u32, u32, Vec<[f32; 4]>)> = HashMap::new();
    let mut samplers = HashMap::new();
    for en in &t.enlaces {
        match &en.recurso {
            Recurso::Buffer { indice, .. } => {
                let buf = motor.buffer(BUF)?;
                for (k, w) in buf.bytes().chunks_mut(4).enumerate() {
                    w.copy_from_slice(&((k % 97) as f32 * 0.25).to_le_bytes());
                }
                buffers.insert(*indice, buf);
            }
            // una textura de Metal por índice: la misma imagen para leer (32+N) y escribir (160+N) si es read_write
            Recurso::Textura { indice, dim, arreglo } | Recurso::TexturaEscritura { indice, dim, arreglo } => {
                if texturas.contains_key(indice) {
                    continue;
                }
                let leida = t.enlaces.iter().any(|x| matches!(x.recurso, Recurso::Textura { indice: i, .. } if i == *indice));
                let escrita = t.enlaces.iter().any(|x| matches!(x.recurso, Recurso::TexturaEscritura { indice: i, .. } if i == *indice));
                // 2D leída: 512×512 con el patrón; escrita: ancho×alto; otras formas: ×4 capas, ×16 de profundidad
                let forma = Forma::de_spirv(*dim, *arreglo);
                let (w, h) = if escrita { (ancho, alto) } else if forma == Forma::D2 { (TEX_ENTRADA, TEX_ENTRADA) } else { (64, 64) };
                let tx = motor.textura_forma(forma, w, h, 16, 4, fmt)?;
                let n = (tx.ancho * tx.alto * tx.prof * tx.capas) as usize;
                let mut cpu = vec![[0.0f32; 4]; n];
                if leida {
                    let up = motor.buffer(n * 16)?;
                    for i in 0..n as u32 {
                        let (x, y) = (i % tx.ancho, (i / tx.ancho) % tx.alto);
                        let p = patron(x, y);
                        let o = i as usize * 16;
                        for (c, v) in p.iter().enumerate() {
                            up.bytes()[o + c * 4..o + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
                        }
                        cpu[i as usize] = p;
                    }
                    motor.copiar(&tx, &up, true)?;
                }
                if forma == Forma::D2 {
                    tex_cpu.insert(*indice, (tx.ancho, tx.alto, cpu));
                }
                texturas.insert(*indice, tx);
            }
            Recurso::Sampler { .. } | Recurso::SamplerFijo { .. } => {
                samplers.insert(en.binding, motor.sampler(false)?);
            }
        }
    }
    // copias para la CPU (antes de que la GPU escriba)
    let mut mem_cpu: HashMap<u32, Vec<u8>> = buffers.iter().map(|(&k, b)| (k, b.bytes().to_vec())).collect();
    let mut rec: HashMap<u32, Enlazado> = HashMap::new();
    for en in &t.enlaces {
        let x = match &en.recurso {
            Recurso::Buffer { indice, .. } => Enlazado::Buffer(&buffers[indice]),
            Recurso::Textura { indice, .. } | Recurso::TexturaEscritura { indice, .. } => Enlazado::Textura(&texturas[indice]),
            _ => Enlazado::Sampler(samplers[&en.binding]),
        };
        rec.insert(en.binding, x);
    }
    // sin argumentos que distingan a los hilos, el kernel es para un solo hilo (si no, todos harían lo mismo a la vez)
    let por_hilo = e.argumentos.iter().any(|d| d.clase.starts_with("air.thread") || d.clase.starts_with("air.threadgroup_position") || d.clase.starts_with("air.simdgroup_index"));
    let (grupo, rejilla) = if por_hilo { ([8u32, 8, 1], [ancho, alto]) } else { ([1, 1, 1], [1, 1]) };

    // CPU primero (cada threadgroup con sus hilos turnándose en las barreras); si con estos datos el kernel no
    // termina en un número razonable de pasos, no se lanza en la GPU (se colgaría)
    let t0 = std::time::Instant::now();
    let mut memoria = Memoria::nueva(&m);
    let orden_buf: Vec<u32> = mem_cpu.keys().copied().collect();
    for k in &orden_buf {
        memoria.externas.push(std::mem::take(mem_cpu.get_mut(k).unwrap()));
    }
    let fun = air::inline::inline_todo(&m, e.funcion)?;
    let cpu = cpu_kernel(&m, &e, &fun, &mut memoria, &orden_buf, &mut tex_cpu, rejilla, grupo);
    println!("CPU: {:.2} ms", t0.elapsed().as_secs_f64() * 1e3);
    let comparar_cpu = match &cpu {
        Ok(()) => true,
        Err(err) if err.contains("demasiados pasos") => return Err(format!("CPU: {err}; no se lanza en la GPU")),
        Err(err) => {
            println!("CPU: {err}");
            false
        }
    };

    let p = motor.pipeline_computo(&t, grupo)?;
    if !comparar_cpu && std::env::var_os("TAHOE_GPU_FORZAR").is_none() {
        // sin referencia no se sabe si con estos datos termina: no se arriesga a colgar la GPU
        println!("pipeline creado; sin referencia de la CPU no se ejecuta (TAHOE_GPU_FORZAR=1 para ejecutarlo)");
        return Ok(());
    }
    let t0 = std::time::Instant::now();
    motor.despachar(&p, &rec, [rejilla[0], rejilla[1], 1], grupo)?;
    println!("GPU: {:.2} ms", t0.elapsed().as_secs_f64() * 1e3);
    if !comparar_cpu {
        println!("GPU sin errores (sin referencia de la CPU)");
        return Ok(());
    }

    // comparar
    let mut total_mal = 0usize;
    for en in &t.enlaces {
        match &en.recurso {
            Recurso::TexturaEscritura { indice, .. } if tex_cpu.contains_key(indice) => {
                let tx = &texturas[indice];
                let down = motor.buffer((ancho * alto * 16) as usize)?;
                motor.copiar(tx, &down, false)?;
                let (_, _, cpu) = &tex_cpu[indice];
                let mut mal = 0;
                let mut ejemplo = None;
                for i in 0..(ancho * alto) as usize {
                    for c in 0..4 {
                        let o = i * 16 + c * 4;
                        let g = f32::from_le_bytes(down.bytes()[o..o + 4].try_into().unwrap());
                        let r = cpu[i][c];
                        if (g - r).abs() > 1e-3 * r.abs().max(1.0) {
                            mal += 1;
                            ejemplo.get_or_insert((i, c, g, r));
                        }
                    }
                }
                println!("textura {indice}: {} valores, {mal} distintos{}", ancho * alto * 4, ejemplo.map(|(i, c, g, r)| format!(" (p. ej. píxel {i} canal {c}: GPU {g}, CPU {r})")).unwrap_or_default());
                total_mal += mal;
            }
            Recurso::Buffer { indice, escritura: true } => {
                let pos = orden_buf.iter().position(|b| b == indice).unwrap();
                let cpu = &memoria.externas[pos];
                let gpu = buffers[indice].bytes();
                let mut mal = 0;
                for k in (0..BUF).step_by(4) {
                    let (g, r) = (f32::from_le_bytes(gpu[k..k + 4].try_into().unwrap()), f32::from_le_bytes(cpu[k..k + 4].try_into().unwrap()));
                    if g.to_bits() != r.to_bits() && (g - r).abs() > 1e-3 * r.abs().max(1.0) {
                        mal += 1;
                    }
                }
                println!("buffer {indice}: {} palabras, {mal} distintas", BUF / 4);
                total_mal += mal;
            }
            _ => {}
        }
    }
    if total_mal > 0 {
        return Err(format!("{total_mal} resultados distintos entre la GPU y la CPU"));
    }
    println!("GPU y CPU coinciden");
    Ok(())
}

fn redondear_pub(bits: u32, x: f64) -> f64 {
    match bits {
        16 => interp::f16_a_f32(interp::f32_a_f16(x as f32)) as f64,
        32 => x as f32 as f64,
        _ => x,
    }
}

/// Pasos máximos por hilo en la CPU.
const PASOS_HILO: u64 = 2_000_000;

fn vec3(m: &Module, t: usize, v: [u32; 3]) -> V {
    valor_arg(m, t, v)
}

#[allow(clippy::too_many_arguments)]
fn cpu_kernel(
    m: &Module,
    e: &Entrada,
    fun: &air::ir::Function,
    memoria: &mut Memoria,
    orden_buf: &[u32],
    tex_cpu: &mut HashMap<u32, (u32, u32, Vec<[f32; 4]>)>,
    [ancho, alto]: [u32; 2],
    grupo: [u32; 3],
) -> Res<()> {
    let grupos = [ancho.div_ceil(grupo[0]), alto.div_ceil(grupo[1]), 1];
    // buffers de threadgroup (air.buffer en address_space 3): una región por grupo, al final de las externas
    let mut wg_args: HashMap<usize, u32> = HashMap::new();
    for d in &e.argumentos {
        if d.clase == "air.buffer" && d.int_tras("air.address_space") == Some(3) {
            wg_args.insert(d.indice as usize, memoria.externas.len() as u32);
            memoria.externas.push(vec![0u8; 4096 * 4]);
        }
    }
    for gy in 0..grupos[1] {
        for gx in 0..grupos[0] {
            for &r in wg_args.values() {
                memoria.externas[r as usize].fill(0);
            }
            let base_alloca = memoria.allocas.len();
            let mut hilos = Vec::new();
            for ly in 0..grupo[1] {
                for lx in 0..grupo[0] {
                    let (x, y) = (gx * grupo[0] + lx, gy * grupo[1] + ly);
                    let mut args = Vec::new();
                    for (k, &pv) in fun.args.iter().enumerate() {
                        let t_arg = fun.values[pv].ty;
                        let desc = e.argumentos.iter().find(|d| d.indice == k as i64);
                        let v = match desc.map(|d| d.clase.as_str()) {
                            Some("air.thread_position_in_grid") => vec3(m, t_arg, [x, y, 0]),
                            Some("air.thread_position_in_threadgroup") => vec3(m, t_arg, [lx, ly, 0]),
                            Some("air.threadgroup_position_in_grid") => vec3(m, t_arg, [gx, gy, 0]),
                            Some("air.threadgroups_per_grid") => vec3(m, t_arg, grupos),
                            Some("air.threads_per_threadgroup") => vec3(m, t_arg, grupo),
                            Some("air.threads_per_grid") => vec3(m, t_arg, [ancho, alto, 1]),
                            Some("air.thread_index_in_threadgroup") => vec3(m, t_arg, [ly * grupo[0] + lx, 0, 0]),
                            Some("air.thread_index_in_simdgroup") => vec3(m, t_arg, [(ly * grupo[0] + lx) % 32, 0, 0]),
                            Some("air.simdgroup_index_in_threadgroup") => vec3(m, t_arg, [(ly * grupo[0] + lx) / 32, 0, 0]),
                            Some("air.threads_per_simdgroup") => vec3(m, t_arg, [32, 0, 0]),
                            Some("air.simdgroups_per_threadgroup") => vec3(m, t_arg, [(grupo[0] * grupo[1]).div_ceil(32), 0, 0]),
                            Some("air.thread_index_in_quadgroup") => vec3(m, t_arg, [(ly * grupo[0] + lx) % 4, 0, 0]),
                            Some("air.quadgroups_per_threadgroup") => vec3(m, t_arg, [(grupo[0] * grupo[1]).div_ceil(4), 0, 0]),
                            Some("air.quadgroup_index_in_threadgroup") => vec3(m, t_arg, [(ly * grupo[0] + lx) / 4, 0, 0]),
                            Some("air.buffer") if wg_args.contains_key(&k) => V::Ptr(Mem::Externa(wg_args[&k]), 0),
                            Some("air.buffer" | "air.indirect_buffer") => {
                                let i = desc.unwrap().int_tras("air.location_index").unwrap_or(0) as u32;
                                match orden_buf.iter().position(|&b| b == i) {
                                    Some(pos) => V::Ptr(Mem::Externa(pos as u32), 0),
                                    None => V::Undef, // sin uso en el kernel (la traducción no lo enlaza)
                                }
                            }
                            Some("air.texture") => V::Ptr(Mem::Externa(1000 + desc.unwrap().int_tras("air.location_index").unwrap_or(0) as u32), 0),
                            Some("air.sampler") => V::Undef,
                            Some(c) => return Err(format!("argumento {c} sin valor en la CPU")),
                            None => V::Undef,
                        };
                        args.push(v);
                    }
                    hilos.push((interp::Hilo::nuevo(fun, &args), 0u64, (), [x, y]));
                }
            }
            // por turnos: cada vuelta lleva a todos a la siguiente barrera o función de SIMD-group; las de SIMD-group
            // se calculan cuando han llegado todos los hilos que siguen vivos
            #[derive(Clone)]
            enum Espera {
                Nada,
                Barrera,
                Simd(String, Vec<V>),
                Fin,
            }
            let mut espera = vec![Espera::Nada; hilos.len()];
            loop {
                for (i, (h, pasos, _, [x, y])) in hilos.iter_mut().enumerate() {
                    if !matches!(espera[i], Espera::Nada) {
                        continue;
                    }
                    let mut ext = |nombre: &str, a: &[V]| externa_cpu(m, tex_cpu, nombre, a);
                    espera[i] = match interp::correr(m, fun, h, memoria, &mut ext, pasos, PASOS_HILO).map_err(|err| format!("hilo ({x},{y}): {err}"))? {
                        interp::Parada::Fin(_) => Espera::Fin,
                        interp::Parada::Barrera => Espera::Barrera,
                        interp::Parada::Colectiva(n, a) => Espera::Simd(n, a),
                    };
                }
                if espera.iter().all(|e| matches!(e, Espera::Fin)) {
                    break;
                }
                // SIMD-groups de 32 hilos seguidos: los que esperan en una función colectiva la resuelven juntos
                let mut avance = false;
                for g in (0..hilos.len()).step_by(32) {
                    let fin = (g + 32).min(hilos.len());
                    let nombre = (g..fin).find_map(|k| match &espera[k] { Espera::Simd(n, _) => Some(n.clone()), _ => None });
                    let Some(nombre) = nombre else { continue };
                    let args: Vec<Option<Vec<V>>> = (g..fin).map(|k| match &espera[k] { Espera::Simd(n, a) if *n == nombre => Some(a.clone()), _ => None }).collect();
                    let res = interp::simd_cpu(&nombre, &args)?;
                    for (k, r) in res.into_iter().enumerate() {
                        if args[k].is_some() {
                            hilos[g + k].0.reanudar(r);
                            espera[g + k] = Espera::Nada;
                            avance = true;
                        }
                    }
                }
                if !avance {
                    // nadie en funciones colectivas: se sueltan las barreras
                    for e in espera.iter_mut() {
                        if matches!(e, Espera::Barrera) {
                            *e = Espera::Nada;
                        }
                    }
                }
            }
            memoria.allocas.truncate(base_alloca);
        }
    }
    Ok(())
}

/// Llamadas air.* en la CPU: texturas (con las copias de `tex_cpu`) y matemáticas.
fn externa_cpu(m: &Module, tex_cpu: &mut HashMap<u32, (u32, u32, Vec<[f32; 4]>)>, nombre: &str, a: &[V]) -> Option<Option<V>> {
    let tex = |v: &V| match v { V::Ptr(Mem::Externa(n), _) if *n >= 1000 => Some(n - 1000), _ => None };
    if nombre.starts_with("air.read_texture_2d.") {
        let (w, h, px) = tex_cpu.get(&tex(&a[0])?)?;
        let V::Vec(c) = &a[2] else { return None };
        let (cx, cy) = (c[0].int()? as u32, c[1].int()? as u32);
        let v = if cx < *w && cy < *h { px[(cy * w + cx) as usize] } else { [0.0; 4] };
        let bits = if nombre.contains("f16") { 16 } else { 32 };
        return Some(Some(V::Agg(vec![V::Vec(v.iter().map(|&q| V::Real(bits, redondear_pub(bits, q as f64))).collect()), V::Int(8, 0)])));
    }
    if nombre.starts_with("air.write_texture_2d.") {
        let (w, h, px) = tex_cpu.get_mut(&tex(&a[0])?)?;
        let V::Vec(c) = &a[1] else { return None };
        let (cx, cy) = (c[0].int()? as u32, c[1].int()? as u32);
        let V::Vec(val) = &a[2] else { return None };
        if cx < *w && cy < *h {
            let i = (cy * *w + cx) as usize;
            for k in 0..4 {
                px[i][k] = val.get(k).and_then(|x| x.real()).unwrap_or(0.0) as f32;
            }
        }
        return Some(None);
    }
    if let Some(r) = nombre.strip_prefix("air.get_width_texture_2d").or_else(|| nombre.strip_prefix("air.get_height_texture_2d")) {
        let _ = r;
        let (w, h, _) = tex_cpu.get(&tex(&a[0])?)?;
        return Some(Some(V::Int(32, if nombre.contains("width") { *w } else { *h } as u64)));
    }
    if nombre.starts_with("air.sample_texture_2d.") {
        // sampler de las pruebas: nearest, coordenadas normalizadas, sujetas al borde
        let (w, h, px) = tex_cpu.get(&tex(&a[0])?)?;
        let V::Vec(c) = &a[2] else { return None };
        let (u, v) = (c[0].real()?, c[1].real()?);
        let cx = ((u * *w as f64).floor().max(0.0) as u32).min(w - 1);
        let cy = ((v * *h as f64).floor().max(0.0) as u32).min(h - 1);
        let t = px[(cy * w + cx) as usize];
        let bits = if nombre.contains("f16") { 16 } else { 32 };
        return Some(Some(V::Agg(vec![V::Vec(t.iter().map(|&q| V::Real(bits, redondear_pub(bits, q as f64))).collect()), V::Int(8, 0)])));
    }
    if nombre.starts_with("air.get_num_mip_levels") || nombre.starts_with("air.get_num_samples") {
        return Some(Some(V::Int(32, 1)));
    }
    if nombre == "air.get_read_sampler" {
        return Some(Some(V::Undef));
    }
    if nombre.starts_with("llvm.lifetime") || nombre.starts_with("air.mem.barrier") {
        return Some(None);
    }
    if nombre.starts_with("llvm.") {
        return interp::llvm_math(nombre, a).map(Some);
    }
    let fi = m.functions.iter().position(|f| f.name == nombre)?;
    let ret = match &m.types[m.functions[fi].ty] { Type::Function { ret, .. } => *ret, _ => return None };
    interp::air_math(m, nombre, a, ret).map(Some)
}
