//! `tahoe-gpu servir <socket>`: el servidor de GPU de Linux. Los drivers Metal de los procesos de macOS (tahoe-metal) se
//! conectan a este socket Unix y le piden lo que haga falta (ver protocolo.rs); este proceso es el único que usa Vulkan.
//!
//! Cada cliente (un proceso de macOS) tiene sus buffers (memoria compartida en /dev/shm, importada sin copias),
//! bibliotecas (metallib) y pipelines (AIR traducido a SPIR-V, uno por tamaño de grupo). Los command buffers se ejecutan
//! enteros al llegar, con espera (aún sin colas asíncronas).

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};

use ash::vk;

use crate::air::interp::V;
use crate::air::{self, spirv};
use crate::formatos;
use crate::metallib;
use crate::motor::{Buffer, Enlazado, Motor, PipelineComputo, Textura};
use crate::protocolo::{self, clase, orden, tipo, Escritor, Info, Lector};
use crate::render::{Atributo, Carga, ColorPase, DescRender, Dibujo, EnlaceVertices, Mezcla, PipelineRender};
use crate::vk::Res;

pub fn servir(a: Vec<String>) -> Res<()> {
    let ruta = a.first().ok_or("uso: tahoe-gpu servir <socket>")?.clone();
    let motor = Arc::new(Mutex::new(Motor::new()?));
    let nombre = motor.lock().unwrap().gpu.name();
    let _ = std::fs::remove_file(&ruta);
    let l = UnixListener::bind(&ruta).map_err(|e| format!("{ruta}: {e}"))?;
    eprintln!("tahoe-gpu: sirviendo {nombre} en {ruta}");
    for c in l.incoming() {
        let Ok(c) = c else { continue };
        let m = motor.clone();
        std::thread::spawn(move || {
            let mut cl = Cliente { motor: m, buffers: HashMap::new(), bibliotecas: HashMap::new(), pipelines: HashMap::new(), texturas: HashMap::new(), samplers: HashMap::new(), renders: HashMap::new() };
            if let Err(e) = cl.atender(c) {
                if e.kind() != std::io::ErrorKind::UnexpectedEof {
                    eprintln!("tahoe-gpu: cliente: {e}");
                }
            }
            cl.soltar_todo();
        });
    }
    Ok(())
}

/// Memoria compartida proyectada y el VkBuffer que la importa.
struct BufferCompartido {
    ptr: *mut u8,
    len: usize,
    buf: Option<Buffer>,
}

struct Pipeline {
    traduccion: spirv::Traduccion,
    /// uno por tamaño de grupo (constantes de especialización 0-2)
    por_grupo: HashMap<[u32; 3], PipelineComputo>,
}

/// Textura de Metal: imagen de Vulkan y, si es sobre una IOSurface, su memoria compartida (que se copia a la imagen antes
/// de usarla y de vuelta después de escribirla).
struct TexturaServidor {
    t: Textura,
    superficie: Option<(BufferCompartido, u64)>,
}

/// Pipeline de render: las dos etapas traducidas y su descripción; uno de Vulkan por topología.
struct Render {
    v: spirv::Traduccion,
    f: Option<spirv::Traduccion>,
    atributos: Vec<Atributo>,
    enlaces_vertices: Vec<EnlaceVertices>,
    colores: Vec<(vk::Format, Mezcla)>,
    por_topologia: HashMap<i32, PipelineRender>,
}

struct Cliente {
    motor: Arc<Mutex<Motor>>,
    buffers: HashMap<u32, BufferCompartido>,
    bibliotecas: HashMap<u32, Vec<u8>>,
    pipelines: HashMap<u32, Pipeline>,
    texturas: HashMap<u32, TexturaServidor>,
    samplers: HashMap<u32, vk::Sampler>,
    renders: HashMap<u32, Render>,
}

/// Valor de una function constant: bytes y MTLDataType.
fn valor_constante(tipo: u32, b: &[u8]) -> V {
    let u = |k: usize, n: usize| -> u64 {
        let mut a = [0u8; 8];
        let s = b.get(k * n..k * n + n).unwrap_or(&[]);
        a[..s.len()].copy_from_slice(s);
        u64::from_le_bytes(a)
    };
    let vec = |n: usize, f: &dyn Fn(usize) -> V| if n == 1 { f(0) } else { V::Vec((0..n).map(f).collect()) };
    match tipo {
        3..=6 => vec((tipo - 2) as usize, &|k| V::Real(32, f32::from_bits(u(k, 4) as u32) as f64)),
        16..=19 => vec((tipo - 15) as usize, &|k| V::Real(16, crate::air::interp::f16_a_f32(u(k, 2) as u16) as f64)),
        29..=36 => vec(((tipo - 29) % 4 + 1) as usize, &|k| V::Int(32, u(k, 4))),
        37..=44 => vec(((tipo - 37) % 4 + 1) as usize, &|k| V::Int(16, u(k, 2))),
        45..=52 => vec(((tipo - 45) % 4 + 1) as usize, &|k| V::Int(8, u(k, 1))),
        53..=56 => vec((tipo - 52) as usize, &|k| V::Int(1, (u(k, 1) != 0) as u64)),
        81..=88 => V::Int(64, u(0, 8)),
        _ => V::Int(32, u(0, 4)),
    }
}

/// Constantes por índice y por nombre (índice u32::MAX y el nombre).
type ConstantesLeidas = (HashMap<u32, V>, Vec<(String, V)>);

fn leer_constantes(l: &mut Lector) -> std::io::Result<ConstantesLeidas> {
    let n = l.u32()?;
    let mut c = HashMap::new();
    let mut por_nombre = Vec::new();
    for _ in 0..n {
        let i = l.u32()?;
        let nombre = if i == u32::MAX { l.texto()? } else { String::new() };
        let (t, b) = (l.u32()?, l.bytes()?);
        if i == u32::MAX {
            por_nombre.push((nombre, valor_constante(t, b)));
        } else {
            c.insert(i, valor_constante(t, b));
        }
    }
    Ok((c, por_nombre))
}

/// Proyecta un archivo de /dev/shm (buffers del driver e IOSurfaces).
fn proyectar(ruta: &str, len: Option<usize>) -> Res<(*mut u8, usize)> {
    if !ruta.starts_with("/dev/shm/") || ruta.contains("..") {
        return Err(format!("ruta no válida: {ruta}"));
    }
    let c = std::ffi::CString::new(ruta).map_err(|_| "ruta")?;
    // SAFETY: llamadas al sistema con argumentos válidos.
    unsafe {
        let fd = libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(format!("{ruta}: {}", std::io::Error::last_os_error()));
        }
        let len = match len {
            Some(n) => n,
            None => {
                let mut st: libc::stat = std::mem::zeroed();
                libc::fstat(fd, &mut st);
                st.st_size as usize
            }
        }
        .next_multiple_of(4096)
        .max(4096);
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        libc::close(fd);
        if p == libc::MAP_FAILED {
            return Err(format!("mmap de {ruta}: {}", std::io::Error::last_os_error()));
        }
        Ok((p.cast::<u8>(), len))
    }
}

fn info(m: &Motor) -> Info {
    let g = &m.gpu;
    let l = &g.props.limits;
    Info {
        version: protocolo::VERSION,
        nombre: format!("Tahoe GPU ({})", g.name()),
        registry_id: ((g.props.vendor_id as u64) << 32) | g.props.device_id as u64,
        max_buffer: (l.max_storage_buffer_range as u64).max(256 << 20),
        max_hilos_grupo: l.max_compute_work_group_invocations,
        memoria_grupo: l.max_compute_shared_memory_size,
        ancho_simd: 32,
    }
}

fn io(e: String) -> std::io::Error {
    std::io::Error::other(e)
}

impl Cliente {
    fn atender(&mut self, c: UnixStream) -> std::io::Result<()> {
        let mut r = BufReader::new(c.try_clone()?);
        let mut w = c;
        loop {
            let (t, cuerpo) = protocolo::recibir(&mut r)?;
            let mut e = Escritor::default();
            match self.mensaje(t, &cuerpo, &mut e) {
                Ok(()) => protocolo::enviar(&mut w, t, &e.0)?,
                Err(err) => {
                    let mut e = Escritor::default();
                    e.texto(&err.to_string());
                    protocolo::enviar(&mut w, tipo::ERROR, &e.0)?;
                }
            }
        }
    }

    fn mensaje(&mut self, t: u32, cuerpo: &[u8], e: &mut Escritor) -> std::io::Result<()> {
        let mut l = Lector::new(cuerpo);
        match t {
            tipo::HOLA => {
                let v = l.u32()?;
                if v != protocolo::VERSION {
                    return Err(io(format!("versión del protocolo {v}; este servidor habla la {}", protocolo::VERSION)));
                }
                info(&self.motor.lock().unwrap()).escribir(e);
            }
            tipo::BUFFER => {
                let (id, len, ruta) = (l.u32()?, l.u64()? as usize, l.texto()?);
                self.buffer(id, len, &ruta).map_err(io)?;
            }
            tipo::LIBERAR => {
                let (c, id) = (l.u32()?, l.u32()?);
                match c {
                    clase::BUFFER => {
                        if let Some(b) = self.buffers.remove(&id) {
                            self.soltar_buffer(b);
                        }
                    }
                    clase::BIBLIOTECA => {
                        self.bibliotecas.remove(&id);
                    }
                    clase::PIPELINE => {
                        self.pipelines.remove(&id);
                    }
                    clase::TEXTURA => {
                        if let Some(t) = self.texturas.remove(&id) {
                            let m = self.motor.lock().unwrap();
                            t.t.destruir(&m);
                            if let Some((mut s, _)) = t.superficie {
                                if let Some(b) = s.buf.take() {
                                    b.destruir(&m);
                                }
                                // SAFETY: proyección de proyectar().
                                unsafe { libc::munmap(s.ptr.cast(), s.len) };
                            }
                        }
                    }
                    clase::SAMPLER => {
                        if let Some(sm) = self.samplers.remove(&id) {
                            // SAFETY: sampler de este dispositivo; los command buffers se ejecutan enteros con espera.
                            unsafe { self.motor.lock().unwrap().gpu.device.destroy_sampler(sm, None) };
                        }
                    }
                    clase::PIPELINE_RENDER => {
                        if let Some(r) = self.renders.remove(&id) {
                            let m = self.motor.lock().unwrap();
                            for (_, p) in r.por_topologia {
                                p.destruir(&m);
                            }
                        }
                    }
                    _ => {}
                }
            }
            tipo::BIBLIOTECA => {
                let (id, datos) = (l.u32()?, l.bytes()?.to_vec());
                let b = metallib::Biblioteca::leer(&datos).map_err(io)?;
                e.u32(b.funciones.len() as u32);
                for f in &b.funciones {
                    e.texto(&f.nombre);
                }
                self.bibliotecas.insert(id, datos);
            }
            tipo::PIPELINE_COMPUTO => {
                let (id, bib, func) = (l.u32()?, l.u32()?, l.texto()?);
                let datos = self.bibliotecas.get(&bib).ok_or_else(|| io(format!("biblioteca {bib} desconocida")))?;
                let b = metallib::Biblioteca::leer(datos).map_err(io)?;
                let f = b.funciones.iter().find(|f| f.nombre == func).ok_or_else(|| io(format!("la biblioteca no tiene «{func}»")))?;
                let m = air::ir::leer(b.bitcode(f).map_err(io)?).map_err(io)?;
                let ent = air::entradas(&m).into_iter().next().ok_or_else(|| io("sin punto de entrada".into()))?;
                let t = spirv::traducir(&m, &ent, &Default::default()).map_err(|er| io(format!("{func}: {er}")))?;
                let mm = self.motor.lock().unwrap();
                e.u32(32).u32(mm.gpu.props.limits.max_compute_work_group_invocations.min(1024)).u32(0);
                drop(mm);
                self.pipelines.insert(id, Pipeline { traduccion: t, por_grupo: HashMap::new() });
            }
            tipo::COMANDOS => {
                let ordenes = l.bytes()?.to_vec();
                match self.ejecutar(&ordenes) {
                    Ok(()) => {
                        eprintln!("tahoe-gpu: command buffer: {} bytes de órdenes", ordenes.len());
                        e.u32(0).texto("");
                    }
                    Err(er) => {
                        eprintln!("tahoe-gpu: command buffer: {er}");
                        e.u32(1).texto(&er);
                    }
                }
            }
            tipo::TEXTURA => {
                let (id, t, f, w, h, d, nv, cp, us) = (l.u32()?, l.u32()?, l.u32()?, l.u32()?, l.u32()?, l.u32()?, l.u32()?, l.u32()?, l.u32()?);
                let _ = us;
                let (sup, bpr) = (l.texto()?, l.u64()?);
                self.textura(id, t, f, w, h, d.max(1), nv.max(1), cp.max(1), &sup, bpr).map_err(io)?;
            }
            tipo::TEXTURA_SUBIR | tipo::TEXTURA_BAJAR => {
                let id = l.u32()?;
                let (nivel, capa, x, y) = (l.u32()?, l.u32()?, l.u32()?, l.u32()?);
                let (z, w, h, prof, bpr) = if t == tipo::TEXTURA_SUBIR { (l.u32()?, l.u32()?, l.u32()?, l.u32()?, l.u32()?) } else { (0, l.u32()?, l.u32()?, 1, l.u32()?) };
                let tx = self.texturas.get(&id).ok_or_else(|| io(format!("textura {id} desconocida")))?;
                let m = self.motor.lock().unwrap();
                let bpp = tx.t.bpp.max(1);
                let tam = (bpr as usize) * (h as usize) * (prof.max(1) as usize);
                let tmp = m.buffer(tam.max(16)).map_err(io)?;
                let fila = bpr / bpp;
                if t == tipo::TEXTURA_SUBIR {
                    let datos = l.bytes()?;
                    tmp.bytes()[..datos.len().min(tam)].copy_from_slice(&datos[..datos.len().min(tam)]);
                    m.copiar_region(&tx.t, &tmp, 0, fila, nivel, capa, [x as i32, y as i32, z as i32], [w, h, prof], true).map_err(io)?;
                    Self::sincronizar(&m, tx, false).map_err(io)?;
                } else {
                    Self::sincronizar(&m, tx, true).map_err(io)?;
                    m.copiar_region(&tx.t, &tmp, 0, fila, nivel, capa, [x as i32, y as i32, 0], [w, h, 1], false).map_err(io)?;
                    e.bytes(&tmp.bytes()[..tam]);
                }
                tmp.destruir(&m);
            }
            tipo::SAMPLER => {
                let id = l.u32()?;
                let (mn, mg, mp) = (l.u32()?, l.u32()?, l.u32()?);
                let dir = [l.u32()?, l.u32()?, l.u32()?];
                let (norm, aniso) = (l.u32()? != 0, l.u32()?);
                let lod = [f32::from_bits(l.u32()?), f32::from_bits(l.u32()?)];
                let (_cmp, borde) = (l.u32()?, l.u32()?);
                let s = self.motor.lock().unwrap().sampler_desc(mn, mg, mp, dir, norm, aniso, lod, borde).map_err(io)?;
                self.samplers.insert(id, s);
            }
            tipo::PIPELINE_RENDER => {
                let id = l.u32()?;
                let (bv, fv) = (l.u32()?, l.texto()?);
                let cv = leer_constantes(&mut l)?;
                let (bf, ff) = (l.u32()?, l.texto()?);
                let cf = leer_constantes(&mut l)?;
                let mut atributos = Vec::new();
                for _ in 0..l.u32()? {
                    let (a, f, off, b) = (l.u32()?, l.u32()?, l.u32()?, l.u32()?);
                    let formato = formatos::vertice(f).ok_or_else(|| io(format!("MTLVertexFormat {f} sin soporte")))?;
                    atributos.push(Atributo { location: a, buffer: b, formato, offset: off });
                }
                let mut enlaces_vertices = Vec::new();
                for _ in 0..l.u32()? {
                    let (b, paso, func) = (l.u32()?, l.u32()?, l.u32()?);
                    enlaces_vertices.push(EnlaceVertices { buffer: b, paso, por_instancia: func == 2 });
                }
                let mut colores = Vec::new();
                for _ in 0..l.u32()? {
                    let (f, activa) = (l.u32()?, l.u32()? != 0);
                    let mut fx = [0u32; 6];
                    for x in fx.iter_mut() {
                        *x = l.u32()?;
                    }
                    let mascara = l.u32()?;
                    let vf = formatos::pixel(f).map(|x| x.0).unwrap_or(vk::Format::B8G8R8A8_UNORM);
                    colores.push((
                        vf,
                        Mezcla {
                            activa,
                            src_color: formatos::factor(fx[0]),
                            dst_color: formatos::factor(fx[1]),
                            op_color: formatos::operacion(fx[2]),
                            src_alfa: formatos::factor(fx[3]),
                            dst_alfa: formatos::factor(fx[4]),
                            op_alfa: formatos::operacion(fx[5]),
                            mascara: formatos::mascara(mascara),
                        },
                    ));
                }
                let (_prof, _topo) = (l.u32()?, l.u32()?);
                let traducir = |bib: u32, func: &str, op: &spirv::Opciones, por_nombre: &[(String, V)]| -> std::io::Result<spirv::Traduccion> {
                    let datos = self.bibliotecas.get(&bib).ok_or_else(|| io(format!("biblioteca {bib} desconocida")))?;
                    let b = metallib::Biblioteca::leer(datos).map_err(io)?;
                    let f = b.funciones.iter().find(|f| f.nombre == func).ok_or_else(|| io(format!("la biblioteca no tiene «{func}»")))?;
                    let m = air::ir::leer(b.bitcode(f).map_err(io)?).map_err(io)?;
                    let ent = air::entradas(&m).into_iter().next().ok_or_else(|| io("sin punto de entrada".into()))?;
                    // las constantes por nombre, a su índice en esta función
                    let mut op2 = spirv::Opciones { constantes: op.constantes.clone(), varyings: op.varyings.clone(), set: op.set };
                    for fc in air::fc::constantes(&m) {
                        if let Some((_, v)) = por_nombre.iter().find(|(n, _)| *n == fc.nombre) {
                            op2.constantes.insert(fc.indice, v.clone());
                        }
                    }
                    spirv::traducir(&m, &ent, &op2).map_err(|er| io(format!("{func}: {er}")))
                };
                let (cv, nv) = cv;
                let (cf, nf) = cf;
                let v = traducir(bv, &fv, &spirv::Opciones { constantes: cv, set: 0, ..Default::default() }, &nv)?;
                let f = if bf != 0 {
                    let varyings: HashMap<String, u32> = v.varyings.iter().cloned().collect();
                    Some(traducir(bf, &ff, &spirv::Opciones { constantes: cf, varyings, set: 1 }, &nf)?)
                } else {
                    None
                };
                self.renders.insert(id, Render { v, f, atributos, enlaces_vertices, colores, por_topologia: HashMap::new() });
            }
            otro => return Err(io(format!("mensaje {otro} desconocido"))),
        }
        Ok(())
    }

    fn buffer(&mut self, id: u32, len: usize, ruta: &str) -> Res<()> {
        let (ptr, len) = proyectar(ruta, Some(len))?;
        // SAFETY: la proyección de len bytes vive lo que BufferCompartido.
        let buf = unsafe { self.motor.lock().unwrap().buffer_en(ptr, len)? };
        self.buffers.insert(id, BufferCompartido { ptr, len, buf: Some(buf) });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn textura(&mut self, id: u32, tipo: u32, fmt: u32, w: u32, h: u32, prof: u32, niveles: u32, capas: u32, sup: &str, bpr: u64) -> Res<()> {
        let forma = formatos::forma(tipo).ok_or_else(|| format!("MTLTextureType {tipo} sin soporte"))?;
        let (vf, _) = formatos::pixel(fmt).ok_or_else(|| format!("MTLPixelFormat {fmt} sin soporte"))?;
        let m = self.motor.lock().unwrap();
        let t = m.textura_completa(forma, w, h, prof, capas, niveles, vf)?;
        let superficie = if sup.is_empty() {
            None
        } else {
            let (ptr, len) = proyectar(sup, None)?;
            // SAFETY: proyección de len bytes que vive lo que la textura.
            let buf = unsafe { m.buffer_en(ptr, len)? };
            Some((BufferCompartido { ptr, len, buf: Some(buf) }, bpr))
        };
        drop(m);
        self.texturas.insert(id, TexturaServidor { t, superficie });
        Ok(())
    }

    /// IOSurface → imagen (subir) o imagen → IOSurface.
    fn sincronizar(m: &Motor, t: &TexturaServidor, subir: bool) -> Res<()> {
        if let Some((s, bpr)) = &t.superficie {
            let fila = (*bpr / t.t.bpp.max(1) as u64) as u32;
            m.copiar_region(&t.t, s.buf.as_ref().unwrap(), 0, fila, 0, 0, [0, 0, 0], [t.t.ancho, t.t.alto, 1], subir)?;
        }
        Ok(())
    }

    fn soltar_buffer(&self, mut b: BufferCompartido) {
        if let Some(buf) = b.buf.take() {
            buf.destruir(&self.motor.lock().unwrap());
        }
        // SAFETY: proyección hecha en buffer().
        unsafe { libc::munmap(b.ptr.cast(), b.len) };
    }

    fn soltar_todo(&mut self) {
        let bs: Vec<BufferCompartido> = self.buffers.drain().map(|(_, b)| b).collect();
        for b in bs {
            self.soltar_buffer(b);
        }
        self.pipelines.clear();
        self.bibliotecas.clear();
        let m = self.motor.clone();
        let m = m.lock().unwrap();
        for (_, t) in self.texturas.drain() {
            t.t.destruir(&m);
            if let Some((mut s, _)) = t.superficie {
                if let Some(b) = s.buf.take() {
                    b.destruir(&m);
                }
                // SAFETY: proyección de proyectar().
                unsafe { libc::munmap(s.ptr.cast(), s.len) };
            }
        }
        for (_, s) in self.samplers.drain() {
            // SAFETY: sampler de este dispositivo, sin uso.
            unsafe { m.gpu.device.destroy_sampler(s, None) };
        }
        for (_, r) in self.renders.drain() {
            for (_, p) in r.por_topologia {
                p.destruir(&m);
            }
        }
    }

    /// Ejecuta un command buffer: cómputo, copias y pases de render, en orden.
    fn ejecutar(&mut self, ordenes: &[u8]) -> Res<()> {
        let mut l = Lector::new(ordenes);
        let motor = self.motor.clone();
        let m = motor.lock().unwrap();
        let alin = m.alineacion_buffer();
        let mut temporales: Vec<Buffer> = Vec::new();
        // enlaces por etapa: 0 vertex, 1 fragment, 2 cómputo
        let mut enl: [Enlaces; 3] = Default::default();
        let mut pipeline: Option<u32> = None;
        let mut pase: Option<Vec<(u32, u32, u32, Carga, bool)>> = None;
        let mut dibujos: Vec<DibujoDesc> = Vec::new();
        let mut est = EstadoRender::default();
        let r = (|| -> Res<()> {
            while let Ok(op) = l.u32() {
                let e = |x: std::io::Error| x.to_string();
                let etapa_de = |l: &mut Lector| -> Res<usize> { Ok((l.u32().map_err(e)? as usize).min(1)) };
                match op {
                    orden::PIPELINE => pipeline = Some(l.u32().map_err(e)?),
                    orden::BUFFER | orden::BUFFER_ETAPA => {
                        let k = if op == orden::BUFFER { 2 } else { etapa_de(&mut l)? };
                        let (i, b, off) = (l.u32().map_err(e)?, l.u32().map_err(e)?, l.u64().map_err(e)?);
                        if off % alin != 0 && k == 2 {
                            return Err(format!("desplazamiento {off} del buffer {i} sin alinear a {alin} (aún sin soporte)"));
                        }
                        enl[k].buffers.insert(i, Origen::Buffer(b, off));
                    }
                    orden::BYTES | orden::BYTES_ETAPA => {
                        let k = if op == orden::BYTES { 2 } else { etapa_de(&mut l)? };
                        let (i, datos) = (l.u32().map_err(e)?, l.bytes().map_err(e)?);
                        let t = m.buffer(datos.len().max(16))?;
                        t.bytes()[..datos.len()].copy_from_slice(datos);
                        temporales.push(t);
                        enl[k].buffers.insert(i, Origen::Temporal(temporales.len() - 1));
                    }
                    orden::TEXTURA | orden::TEXTURA_ETAPA => {
                        let k = if op == orden::TEXTURA { 2 } else { etapa_de(&mut l)? };
                        let (i, t) = (l.u32().map_err(e)?, l.u32().map_err(e)?);
                        enl[k].texturas.insert(i, t);
                    }
                    orden::SAMPLER | orden::SAMPLER_ETAPA => {
                        let k = if op == orden::SAMPLER { 2 } else { etapa_de(&mut l)? };
                        let (i, sm) = (l.u32().map_err(e)?, l.u32().map_err(e)?);
                        enl[k].samplers.insert(i, sm);
                    }
                    orden::MEMORIA_GRUPO => {
                        let _ = (l.u32().map_err(e)?, l.u32().map_err(e)?);
                    }
                    orden::GRUPOS | orden::HILOS => {
                        let mut v = [0u32; 6];
                        for x in v.iter_mut() {
                            *x = l.u32().map_err(e)?;
                        }
                        let grupo = [v[3].max(1), v[4].max(1), v[5].max(1)];
                        let rejilla = if op == orden::GRUPOS { [v[0] * grupo[0], v[1] * grupo[1], v[2] * grupo[2]] } else { [v[0], v[1], v[2]] };
                        let id = pipeline.ok_or("despacho sin pipeline")?;
                        let p = self.pipelines.get_mut(&id).ok_or_else(|| format!("pipeline {id} desconocido"))?;
                        if !p.por_grupo.contains_key(&grupo) {
                            let pc = m.pipeline_computo(&p.traduccion, grupo)?;
                            p.por_grupo.insert(grupo, pc);
                        }
                        let p = &self.pipelines[&id];
                        for t in enl[2].texturas.values() {
                            if let Some(tx) = self.texturas.get(t) {
                                Self::sincronizar(&m, tx, true)?;
                            }
                        }
                        let rec = self.recursos(&p.traduccion, &enl[2], &temporales, &m)?;
                        m.despachar(&p.por_grupo[&grupo], &rec, rejilla, grupo)?;
                        for en in &p.traduccion.enlaces {
                            if let spirv::Recurso::TexturaEscritura { indice, .. } = en.recurso {
                                if let Some(tx) = enl[2].texturas.get(&indice).and_then(|t| self.texturas.get(t)) {
                                    Self::sincronizar(&m, tx, false)?;
                                }
                            }
                        }
                    }
                    orden::COPIA_BUFFER => {
                        let (a, ao, b, bo, n) = (l.u32().map_err(e)?, l.u64().map_err(e)?, l.u32().map_err(e)?, l.u64().map_err(e)?, l.u64().map_err(e)?);
                        let ba = self.buffers.get(&a).and_then(|x| x.buf.as_ref()).ok_or("buffer de origen desconocido")?;
                        let bb = self.buffers.get(&b).and_then(|x| x.buf.as_ref()).ok_or("buffer de destino desconocido")?;
                        m.copiar_buffer(ba, ao, bb, bo, n)?;
                    }
                    orden::FIN => {
                        enl = Default::default();
                        pipeline = None;
                    }
                    orden::PASE => {
                        let n = l.u32().map_err(e)?;
                        let mut c = Vec::new();
                        for _ in 0..n {
                            let (t, nivel, capa, carga, guardar) = (l.u32().map_err(e)?, l.u32().map_err(e)?, l.u32().map_err(e)?, l.u32().map_err(e)?, l.u32().map_err(e)?);
                            let mut col = [0f32; 4];
                            for x in col.iter_mut() {
                                *x = f32::from_bits(l.u32().map_err(e)?);
                            }
                            let carga = match carga {
                                2 => Carga::Limpiar(col),
                                1 => Carga::Cargar,
                                _ => Carga::DaIgual,
                            };
                            c.push((t, nivel, capa, carga, guardar != 0));
                        }
                        pase = Some(c);
                        dibujos.clear();
                        enl = Default::default();
                        est = EstadoRender::default();
                    }
                    orden::PIPELINE_RENDER => est.pipeline = Some(l.u32().map_err(e)?),
                    orden::VIEWPORT => {
                        for x in est.viewport.iter_mut() {
                            *x = f32::from_bits(l.u32().map_err(e)?);
                        }
                        est.hay_viewport = true;
                    }
                    orden::TIJERA => {
                        let t = [l.u32().map_err(e)? as i32, l.u32().map_err(e)? as i32, l.u32().map_err(e)? as i32, l.u32().map_err(e)? as i32];
                        est.tijera = Some(t);
                    }
                    orden::CULL => {
                        let (c, w) = (l.u32().map_err(e)?, l.u32().map_err(e)?);
                        est.cull = match c {
                            1 => vk::CullModeFlags::FRONT,
                            2 => vk::CullModeFlags::BACK,
                            _ => vk::CullModeFlags::NONE,
                        };
                        est.horario = w == 0;
                    }
                    orden::DIBUJAR | orden::DIBUJAR_INDICES => {
                        let prim = l.u32().map_err(e)?;
                        let d = if op == orden::DIBUJAR {
                            let (primero, cuenta, inst, pinst) = (l.u32().map_err(e)?, l.u32().map_err(e)?, l.u32().map_err(e)?, l.u32().map_err(e)?);
                            DibujoDesc { prim, primero, cuenta, instancias: inst, primera_instancia: pinst, base_vertice: 0, indices: None, est: est.clone(), enl: [enl[0].clone(), enl[1].clone()] }
                        } else {
                            let (cuenta, tipo_i, b, off) = (l.u32().map_err(e)?, l.u32().map_err(e)?, l.u32().map_err(e)?, l.u64().map_err(e)?);
                            let (inst, base, pinst) = (l.u32().map_err(e)?, l.u32().map_err(e)? as i32, l.u32().map_err(e)?);
                            let de32 = tipo_i == 1;
                            let primero = (off / if de32 { 4 } else { 2 }) as u32;
                            DibujoDesc { prim, primero, cuenta, instancias: inst, primera_instancia: pinst, base_vertice: base, indices: Some((b, de32)), est: est.clone(), enl: [enl[0].clone(), enl[1].clone()] }
                        };
                        dibujos.push(d);
                    }
                    orden::FIN_PASE => {
                        let colores = pase.take().ok_or("fin de pase sin pase")?;
                        self.pase(&m, &colores, &dibujos, &temporales)?;
                        dibujos.clear();
                        enl = Default::default();
                    }
                    o => return Err(format!("orden {o} desconocida")),
                }
            }
            Ok(())
        })();
        for t in temporales {
            t.destruir(&m);
        }
        r
    }

    /// Enlaces de un shader traducido con lo que haya puesto el encoder.
    fn recursos<'a>(&'a self, t: &spirv::Traduccion, en: &Enlaces, temporales: &'a [Buffer], m: &Motor) -> Res<HashMap<u32, Enlazado<'a>>> {
        let mut rec: HashMap<u32, Enlazado> = HashMap::new();
        for e in &t.enlaces {
            let x = match &e.recurso {
                spirv::Recurso::Buffer { indice, .. } => match en.buffers.get(indice) {
                    Some(Origen::Temporal(k)) => Enlazado::Buffer(&temporales[*k]),
                    Some(Origen::Buffer(b, off)) => {
                        let bc = self.buffers.get(b).and_then(|x| x.buf.as_ref()).ok_or_else(|| format!("buffer {b} desconocido"))?;
                        if off % m.alineacion_buffer() == 0 {
                            Enlazado::BufferDesde(bc, *off)
                        } else {
                            return Err(format!("desplazamiento {off} del buffer {indice} sin alinear (aún sin soporte)"));
                        }
                    }
                    None => return Err(format!("falta el buffer {indice}")),
                },
                spirv::Recurso::Textura { indice, .. } | spirv::Recurso::TexturaEscritura { indice, .. } => {
                    let t = en.texturas.get(indice).ok_or_else(|| format!("falta la textura {indice}"))?;
                    Enlazado::Textura(&self.texturas.get(t).ok_or_else(|| format!("textura {t} desconocida"))?.t)
                }
                spirv::Recurso::Sampler { indice } => {
                    let s = en.samplers.get(indice).ok_or_else(|| format!("falta el sampler {indice}"))?;
                    Enlazado::Sampler(*self.samplers.get(s).ok_or_else(|| format!("sampler {s} desconocido"))?)
                }
                spirv::Recurso::SamplerFijo { .. } => {
                    // TODO: decodificar el estado de 64 bits de los samplers constantes de Metal
                    Enlazado::Sampler(m.sampler_fijo()?)
                }
            };
            rec.insert(e.binding, x);
        }
        Ok(rec)
    }

    /// Un pase de render: pipelines por topología, IOSurfaces al día, dibujos y vuelta de los attachments a sus IOSurfaces.
    fn pase(&mut self, m: &Motor, colores: &[(u32, u32, u32, Carga, bool)], dibujos: &[DibujoDesc], temporales: &[Buffer]) -> Res<()> {
        // pipelines (fase mutable)
        for d in dibujos {
            let id = d.est.pipeline.ok_or("dibujo sin pipeline de render")?;
            let topo = formatos::topologia(d.prim);
            let r = self.renders.get_mut(&id).ok_or_else(|| format!("pipeline de render {id} desconocido"))?;
            if !r.por_topologia.contains_key(&topo.as_raw()) {
                let p = m.pipeline_render(&DescRender { vertex: &r.v, fragment: r.f.as_ref(), atributos: r.atributos.clone(), enlaces_vertices: r.enlaces_vertices.clone(), colores: r.colores.clone(), topologia: topo })?;
                r.por_topologia.insert(topo.as_raw(), p);
            }
        }
        // IOSurfaces → imágenes (attachments que se cargan y texturas que se leen)
        for (t, _, _, carga, _) in colores {
            if matches!(carga, Carga::Cargar) {
                if let Some(tx) = self.texturas.get(t) {
                    Self::sincronizar(m, tx, true)?;
                }
            }
        }
        for d in dibujos {
            for k in 0..2 {
                for t in d.enl[k].texturas.values() {
                    if let Some(tx) = self.texturas.get(t) {
                        Self::sincronizar(m, tx, true)?;
                    }
                }
            }
        }
        let cps: Vec<ColorPase> = colores
            .iter()
            .map(|(t, nivel, capa, carga, guardar)| -> Res<ColorPase> {
                Ok(ColorPase { textura: &self.texturas.get(t).ok_or_else(|| format!("textura {t} desconocida"))?.t, nivel: *nivel, capa: *capa, carga: *carga, guardar: *guardar })
            })
            .collect::<Res<_>>()?;
        let (w, h) = cps.first().map(|c| ((c.textura.ancho >> c.nivel).max(1), (c.textura.alto >> c.nivel).max(1))).unwrap_or((1, 1));
        let mut ds = Vec::new();
        for d in dibujos {
            let r = &self.renders[&d.est.pipeline.unwrap()];
            let p = &r.por_topologia[&formatos::topologia(d.prim).as_raw()];
            let rv = self.recursos(&r.v, &d.enl[0], temporales, m)?;
            let rf = match &r.f {
                Some(f) => self.recursos(f, &d.enl[1], temporales, m)?,
                None => HashMap::new(),
            };
            let mut vertices = Vec::new();
            for ev in &r.enlaces_vertices {
                match d.enl[0].buffers.get(&ev.buffer) {
                    Some(Origen::Buffer(b, off)) => vertices.push((ev.buffer, self.buffers.get(b).and_then(|x| x.buf.as_ref()).ok_or("buffer de vértices desconocido")?, *off)),
                    Some(Origen::Temporal(k)) => vertices.push((ev.buffer, &temporales[*k], 0)),
                    None => return Err(format!("falta el buffer de vértices {}", ev.buffer)),
                }
            }
            let indices = match d.indices {
                Some((b, de32)) => Some((self.buffers.get(&b).and_then(|x| x.buf.as_ref()).ok_or("buffer de índices desconocido")?, 0u64, de32)),
                None => None,
            };
            let vp = if d.est.hay_viewport { d.est.viewport } else { [0.0, 0.0, w as f32, h as f32, 0.0, 1.0] };
            ds.push(Dibujo {
                pipeline: p,
                recursos: [rv, rf],
                vertices,
                indices,
                primero: d.primero,
                cuenta: d.cuenta,
                instancias: d.instancias,
                primera_instancia: d.primera_instancia,
                base_vertice: d.base_vertice,
                viewport: vp,
                tijera: d.est.tijera,
                cull: d.est.cull,
                horario: d.est.horario,
            });
        }
        m.pase_render(&cps, &ds)?;
        // attachments → sus IOSurfaces
        for (t, _, _, _, guardar) in colores {
            if *guardar {
                if let Some(tx) = self.texturas.get(t) {
                    Self::sincronizar(m, tx, false)?;
                }
            }
        }
        Ok(())
    }
}

/// De dónde sale un buffer enlazado: uno del driver (con desplazamiento) o un setBytes (temporal del command buffer).
#[derive(Clone, Copy)]
enum Origen {
    Buffer(u32, u64),
    Temporal(usize),
}

#[derive(Clone, Default)]
struct Enlaces {
    buffers: HashMap<u32, Origen>,
    texturas: HashMap<u32, u32>,
    samplers: HashMap<u32, u32>,
}

#[derive(Clone)]
struct EstadoRender {
    pipeline: Option<u32>,
    viewport: [f32; 6],
    hay_viewport: bool,
    tijera: Option<[i32; 4]>,
    cull: vk::CullModeFlags,
    horario: bool,
}

impl Default for EstadoRender {
    fn default() -> Self {
        EstadoRender { pipeline: None, viewport: [0.0; 6], hay_viewport: false, tijera: None, cull: vk::CullModeFlags::NONE, horario: true }
    }
}

struct DibujoDesc {
    prim: u32,
    primero: u32,
    cuenta: u32,
    instancias: u32,
    primera_instancia: u32,
    base_vertice: i32,
    /// buffer de índices y si son de 32 bits (el desplazamiento va en `primero`)
    indices: Option<(u32, bool)>,
    est: EstadoRender,
    enl: [Enlaces; 2],
}
