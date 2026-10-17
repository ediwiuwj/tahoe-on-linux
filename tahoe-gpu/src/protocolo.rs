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
    /// driver → servidor: textura {u32 id, u32 tipo (MTLTextureType), u32 formato (MTLPixelFormat), u32 ancho, u32 alto,
    /// u32 profundidad, u32 niveles, u32 capas, u32 uso (MTLTextureUsage), texto IOSurface ("" o la ruta en /dev/shm),
    /// u64 bytes por fila de la IOSurface}. Respuesta vacía.
    pub const TEXTURA: u32 = 7;
    /// driver → servidor: replaceRegion {u32 id, u32 nivel, u32 capa, u32 x, u32 y, u32 z, u32 ancho, u32 alto,
    /// u32 prof, u32 bytes por fila, bytes píxeles}. Respuesta vacía.
    pub const TEXTURA_SUBIR: u32 = 8;
    /// driver → servidor: getBytes {u32 id, u32 nivel, u32 capa, u32 x, u32 y, u32 ancho, u32 alto, u32 bytes por fila}.
    /// Respuesta: {bytes píxeles}.
    pub const TEXTURA_BAJAR: u32 = 9;
    /// driver → servidor: sampler {u32 id, u32 min, u32 mag, u32 mip (MTLSamplerMinMagFilter/MipFilter), 3 × u32
    /// direcciones (MTLSamplerAddressMode), u32 normalizadas, u32 anisotropía, f32 lod mín, f32 lod máx, u32 comparación,
    /// u32 color de borde}. Respuesta vacía.
    pub const SAMPLER: u32 = 10;
    /// driver → servidor: pipeline de render {u32 id, u32 biblioteca vertex, texto función vertex, constantes vertex,
    /// u32 biblioteca fragment (0: sin fragment), texto función fragment, constantes fragment, u32 n atributos, n × {u32
    /// atributo, u32 formato (MTLVertexFormat), u32 offset, u32 buffer}, u32 n layouts, n × {u32 buffer, u32 paso, u32
    /// función de paso}, u32 n colores, n × {u32 formato (MTLPixelFormat), u32 mezcla activa, 6 × u32 factores y
    /// operaciones (MTLBlendFactor/MTLBlendOperation: src rgb, dst rgb, op rgb, src alfa, dst alfa, op alfa), u32
    /// máscara (MTLColorWriteMask)}, u32 formato de profundidad, u32 clase de topología}. Constantes: {u32 n, n × {u32
    /// índice, u32 tipo (MTLDataType), bytes valor}}. Respuesta vacía.
    pub const PIPELINE_RENDER: u32 = 11;
    /// servidor → driver: error (texto).
    pub const ERROR: u32 = 0xffff_ffff;
}

/// Clases de objeto para LIBERAR.
pub mod clase {
    pub const BUFFER: u32 = 1;
    pub const BIBLIOTECA: u32 = 2;
    pub const PIPELINE: u32 = 3;
    pub const TEXTURA: u32 = 4;
    pub const SAMPLER: u32 = 5;
    pub const PIPELINE_RENDER: u32 = 6;
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
    /// {u32 índice, u32 textura} (setTexture:atIndex: de cómputo)
    pub const TEXTURA: u32 = 9;
    /// {u32 índice, u32 sampler}
    pub const SAMPLER: u32 = 10;
    /// empieza un pase de render {u32 n colores, n × {u32 textura, u32 nivel, u32 capa, u32 carga (MTLLoadAction),
    /// u32 guardar (MTLStoreAction), 4 × f32 color de limpiar}}
    pub const PASE: u32 = 11;
    /// {u32 pipeline de render}
    pub const PIPELINE_RENDER: u32 = 12;
    /// {u32 etapa (0 vertex, 1 fragment), u32 índice, u32 buffer, u64 desplazamiento}
    pub const BUFFER_ETAPA: u32 = 13;
    /// {u32 etapa, u32 índice, bytes datos}
    pub const BYTES_ETAPA: u32 = 14;
    /// {u32 etapa, u32 índice, u32 textura}
    pub const TEXTURA_ETAPA: u32 = 15;
    /// {u32 etapa, u32 índice, u32 sampler}
    pub const SAMPLER_ETAPA: u32 = 16;
    /// {6 × f32: x, y, ancho, alto, profundidad mín y máx}
    pub const VIEWPORT: u32 = 17;
    /// {4 × u32: x, y, ancho, alto}
    pub const TIJERA: u32 = 18;
    /// {u32 MTLCullMode, u32 MTLWinding}
    pub const CULL: u32 = 19;
    /// dibujo {u32 MTLPrimitiveType, u32 primero, u32 cuenta, u32 instancias, u32 primera instancia}
    pub const DIBUJAR: u32 = 20;
    /// dibujo con índices {u32 MTLPrimitiveType, u32 cuenta, u32 MTLIndexType, u32 buffer, u64 desplazamiento,
    /// u32 instancias, i32 base de vértices, u32 primera instancia}
    pub const DIBUJAR_INDICES: u32 = 21;
    /// fin del pase de render (se ejecuta)
    pub const FIN_PASE: u32 = 22;
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
