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

use crate::air::{self, spirv};
use crate::metallib;
use crate::motor::{Buffer, Enlazado, Motor, PipelineComputo};
use crate::protocolo::{self, clase, orden, tipo, Escritor, Info, Lector};
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
            let mut cl = Cliente { motor: m, buffers: HashMap::new(), bibliotecas: HashMap::new(), pipelines: HashMap::new() };
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

struct Cliente {
    motor: Arc<Mutex<Motor>>,
    buffers: HashMap<u32, BufferCompartido>,
    bibliotecas: HashMap<u32, Vec<u8>>,
    pipelines: HashMap<u32, Pipeline>,
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
                        e.u32(0).texto("");
                    }
                    Err(er) => {
                        eprintln!("tahoe-gpu: command buffer: {er}");
                        e.u32(1).texto(&er);
                    }
                }
            }
            otro => return Err(io(format!("mensaje {otro} desconocido"))),
        }
        Ok(())
    }

    fn buffer(&mut self, id: u32, len: usize, ruta: &str) -> Res<()> {
        if !ruta.starts_with("/dev/shm/") || ruta.contains("..") {
            return Err(format!("ruta de buffer no válida: {ruta}"));
        }
        let len = len.next_multiple_of(4096).max(4096);
        let c = std::ffi::CString::new(ruta).map_err(|_| "ruta")?;
        // SAFETY: llamadas al sistema con argumentos válidos; la proyección vive en BufferCompartido.
        let ptr = unsafe {
            let fd = libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
            if fd < 0 {
                return Err(format!("{ruta}: {}", std::io::Error::last_os_error()));
            }
            let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            libc::close(fd);
            if p == libc::MAP_FAILED {
                return Err(format!("mmap de {ruta}: {}", std::io::Error::last_os_error()));
            }
            p.cast::<u8>()
        };
        // SAFETY: la proyección de len bytes vive lo que BufferCompartido.
        let buf = unsafe { self.motor.lock().unwrap().buffer_en(ptr, len)? };
        self.buffers.insert(id, BufferCompartido { ptr, len, buf: Some(buf) });
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
    }

    /// Ejecuta un command buffer: órdenes de cómputo y de copia.
    fn ejecutar(&mut self, ordenes: &[u8]) -> Res<()> {
        let mut l = Lector::new(ordenes);
        let mut pipeline: Option<u32> = None;
        let mut buffers: HashMap<u32, (u32, u64)> = HashMap::new();
        let mut temporales: HashMap<u32, Buffer> = HashMap::new();
        let motor = self.motor.clone();
        let m = motor.lock().unwrap();
        let alin = m.alineacion_buffer();
        let r = (|| -> Res<()> {
            while let Ok(op) = l.u32() {
                let e = |x: std::io::Error| x.to_string();
                match op {
                    orden::PIPELINE => pipeline = Some(l.u32().map_err(e)?),
                    orden::BUFFER => {
                        let (i, b, off) = (l.u32().map_err(e)?, l.u32().map_err(e)?, l.u64().map_err(e)?);
                        if off % alin != 0 {
                            return Err(format!("desplazamiento {off} del buffer {i} sin alinear a {alin} (aún sin soporte)"));
                        }
                        temporales.remove(&i);
                        buffers.insert(i, (b, off));
                    }
                    orden::BYTES => {
                        let (i, datos) = (l.u32().map_err(e)?, l.bytes().map_err(e)?);
                        let t = m.buffer(datos.len().max(16))?;
                        t.bytes()[..datos.len()].copy_from_slice(datos);
                        buffers.remove(&i);
                        temporales.insert(i, t);
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
                        let pc = &p.por_grupo[&grupo];
                        let mut rec: HashMap<u32, Enlazado> = HashMap::new();
                        for en in &p.traduccion.enlaces {
                            if let spirv::Recurso::Buffer { indice, .. } = en.recurso {
                                if let Some(t) = temporales.get(&indice) {
                                    rec.insert(en.binding, Enlazado::Buffer(t));
                                } else if let Some(&(b, off)) = buffers.get(&indice) {
                                    let bc = self.buffers.get(&b).ok_or_else(|| format!("buffer {b} desconocido"))?;
                                    rec.insert(en.binding, Enlazado::BufferDesde(bc.buf.as_ref().unwrap(), off));
                                } else {
                                    return Err(format!("falta el buffer {indice}"));
                                }
                            } else {
                                return Err(format!("recurso {:?} aún sin soporte en el servidor", en.recurso));
                            }
                        }
                        m.despachar(pc, &rec, rejilla, grupo)?;
                    }
                    orden::COPIA_BUFFER => {
                        let (a, ao, b, bo, n) = (l.u32().map_err(e)?, l.u64().map_err(e)?, l.u32().map_err(e)?, l.u64().map_err(e)?, l.u64().map_err(e)?);
                        let ba = self.buffers.get(&a).and_then(|x| x.buf.as_ref()).ok_or("buffer de origen desconocido")?;
                        let bb = self.buffers.get(&b).and_then(|x| x.buf.as_ref()).ok_or("buffer de destino desconocido")?;
                        m.copiar_buffer(ba, ao, bb, bo, n)?;
                    }
                    orden::FIN => {
                        buffers.clear();
                        pipeline = None;
                    }
                    o => return Err(format!("orden {o} desconocida")),
                }
            }
            Ok(())
        })();
        for (_, t) in temporales {
            t.destruir(&m);
        }
        r
    }
}
