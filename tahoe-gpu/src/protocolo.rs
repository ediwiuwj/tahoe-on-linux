//! Protocolo entre el driver Metal de macOS (tahoe-metal, dentro de cada proceso de macOS) y el servidor de GPU de
//! Linux (`tahoe-gpu servir`, el único proceso que usa Vulkan). Lo comparten los dos crates (tahoe-metal lo incluye con
//! `#[path]`): sin dependencias.
//!
//! Por un socket Unix (el de la propiedad TahoeGPUSocket del IOAccelerator emulado): mensajes con cabecera
//! `[u32 longitud del cuerpo][u32 tipo]` y el cuerpo en little-endian. Cada petición tiene una respuesta, en orden. Los
//! datos grandes (buffers, superficies) irán por memoria compartida en /dev/shm, no por el socket.

use std::io::{self, Read, Write};

pub const VERSION: u32 = 1;

/// Tipos de mensaje.
pub mod tipo {
    /// driver → servidor: versión del protocolo. Respuesta: `Info`.
    pub const HOLA: u32 = 1;
    /// driver → servidor: buffer en memoria compartida {u32 id, u64 tamaño, texto ruta en /dev/shm}. Respuesta vacía.
    pub const BUFFER: u32 = 2;
    /// driver → servidor: soltar un objeto {u32 clase (`clase::*`), u32 id}. Respuesta vacía.
    pub const LIBERAR: u32 = 3;
    /// driver → servidor: biblioteca {u32 id, bytes del metallib}. Respuesta: {u32 n, n × texto nombre de función}.
    pub const BIBLIOTECA: u32 = 4;
    /// driver → servidor: pipeline de cómputo {u32 id, u32 biblioteca, texto función}. Respuesta: {u32 ancho SIMD,
    /// u32 máximo de hilos por grupo, u32 memoria de grupo estática}.
    pub const PIPELINE_COMPUTO: u32 = 5;
    /// driver → servidor: un command buffer {bytes órdenes (`orden::*`)}; se ejecuta entero. Respuesta: {u32 estado
    /// (0 bien), texto error}.
    pub const COMANDOS: u32 = 6;
    /// servidor → driver: error (texto).
    pub const ERROR: u32 = 0xffff_ffff;
}

/// Clases de objeto para LIBERAR.
pub mod clase {
    pub const BUFFER: u32 = 1;
    pub const BIBLIOTECA: u32 = 2;
    pub const PIPELINE: u32 = 3;
}

/// Órdenes dentro de COMANDOS: `u32 orden` y sus argumentos.
pub mod orden {
    /// {u32 pipeline}
    pub const PIPELINE: u32 = 1;
    /// {u32 índice, u32 buffer, u64 desplazamiento}
    pub const BUFFER: u32 = 2;
    /// {u32 índice, bytes datos} (setBytes:length:atIndex:)
    pub const BYTES: u32 = 3;
    /// {3 × u32 grupos, 3 × u32 hilos por grupo} (dispatchThreadgroups:threadsPerThreadgroup:)
    pub const GRUPOS: u32 = 4;
    /// {3 × u32 hilos, 3 × u32 hilos por grupo} (dispatchThreads:threadsPerThreadgroup:)
    pub const HILOS: u32 = 5;
    /// {u32 índice, u32 bytes} (setThreadgroupMemoryLength:atIndex:)
    pub const MEMORIA_GRUPO: u32 = 6;
    /// {u32 origen, u64 desplazamiento, u32 destino, u64 desplazamiento, u64 bytes}
    pub const COPIA_BUFFER: u32 = 7;
    /// fin de un encoder (los enlaces se olvidan)
    pub const FIN: u32 = 8;
}

/// Lo que el servidor cuenta de la GPU (MTLDevice: name, registryID, maxBufferLength...).
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub version: u32,
    pub nombre: String,
    pub registry_id: u64,
    pub max_buffer: u64,
    pub max_hilos_grupo: u32,
    pub memoria_grupo: u32,
    pub ancho_simd: u32,
}

#[derive(Default)]
pub struct Escritor(pub Vec<u8>);

impl Escritor {
    pub fn u32(&mut self, x: u32) -> &mut Self {
        self.0.extend_from_slice(&x.to_le_bytes());
        self
    }
    pub fn u64(&mut self, x: u64) -> &mut Self {
        self.0.extend_from_slice(&x.to_le_bytes());
        self
    }
    pub fn texto(&mut self, s: &str) -> &mut Self {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
        self
    }
}

pub struct Lector<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Lector<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Lector { b, pos: 0 }
    }
    fn tomar(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let r = self.b.get(self.pos..self.pos + n).ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "mensaje corto"))?;
        self.pos += n;
        Ok(r)
    }
    pub fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.tomar(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.tomar(8)?.try_into().unwrap()))
    }
    pub fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.tomar(n)
    }
    pub fn texto(&mut self) -> io::Result<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
}

impl Info {
    pub fn escribir(&self, e: &mut Escritor) {
        e.u32(self.version).texto(&self.nombre).u64(self.registry_id).u64(self.max_buffer).u32(self.max_hilos_grupo).u32(self.memoria_grupo).u32(self.ancho_simd);
    }
    pub fn leer(l: &mut Lector) -> io::Result<Info> {
        Ok(Info {
            version: l.u32()?,
            nombre: l.texto()?,
            registry_id: l.u64()?,
            max_buffer: l.u64()?,
            max_hilos_grupo: l.u32()?,
            memoria_grupo: l.u32()?,
            ancho_simd: l.u32()?,
        })
    }
}

/// Manda un mensaje.
pub fn enviar(w: &mut impl Write, tipo: u32, cuerpo: &[u8]) -> io::Result<()> {
    let mut m = Vec::with_capacity(8 + cuerpo.len());
    m.extend_from_slice(&(cuerpo.len() as u32).to_le_bytes());
    m.extend_from_slice(&tipo.to_le_bytes());
    m.extend_from_slice(cuerpo);
    w.write_all(&m)
}

/// Recibe un mensaje: (tipo, cuerpo).
pub fn recibir(r: &mut impl Read) -> io::Result<(u32, Vec<u8>)> {
    let mut h = [0u8; 8];
    r.read_exact(&mut h)?;
    let n = u32::from_le_bytes(h[..4].try_into().unwrap()) as usize;
    if n > 256 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "mensaje demasiado grande"));
    }
    let tipo = u32::from_le_bytes(h[4..].try_into().unwrap());
    let mut c = vec![0u8; n];
    r.read_exact(&mut c)?;
    Ok((tipo, c))
}
