//! Bibliotecas de shaders de Metal (`.metallib`): cabecera "MTLB", lista de funciones con etiquetas (NAME, TYPE,
//! HASH, MDSZ, OFFT, VERS...) y, al final, el bitcode LLVM (AIR) de cada función. Formato deducido de los archivos
//! de macOS (ver `tahoe-gpu metallib <archivo>`).

use crate::vk::Res;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tipo {
    Vertex,
    Fragment,
    Kernel,
    Otro(u8),
}

#[derive(Debug, Clone)]
pub struct Funcion {
    pub nombre: String,
    pub tipo: Tipo,
    /// Desplazamiento del bitcode dentro de la sección de bitcode, y su tamaño.
    pub bitcode_off: u64,
    pub bitcode_len: u64,
    /// Versiones AIR y del lenguaje (mayor, menor) si vienen.
    pub air: Option<(u16, u16)>,
    pub lenguaje: Option<(u16, u16)>,
}

#[derive(Debug)]
pub struct Biblioteca<'a> {
    pub datos: &'a [u8],
    pub version: (u16, u16),
    pub tipo_archivo: u8,
    pub funciones: Vec<Funcion>,
    pub bitcode: (u64, u64),
}

fn u16_at(b: &[u8], o: usize) -> Res<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| format!("metallib corto (u16 en {o})"))
}
fn u32_at(b: &[u8], o: usize) -> Res<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("metallib corto (u32 en {o})"))
}
fn u64_at(b: &[u8], o: usize) -> Res<u64> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("metallib corto (u64 en {o})"))
}

/// CPU_TYPE_AIR64: el trozo con bitcode AIR genérico de un metallib universal (los demás trozos de los archivos de
/// macOS son binarios ya compilados para GPUs concretas de Apple).
pub const CPU_AIR64: u32 = 0x0100_0017;

/// Los trozos (tipo de CPU, subtipo, datos) de un archivo universal (cabecera fat de Mach-O, en big endian); None si
/// no lo es.
pub fn trozos(d: &[u8]) -> Option<Vec<(u32, u32, &[u8])>> {
    let be = |o: usize| d.get(o..o + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap()));
    if be(0)? != 0xcafe_babe {
        return None;
    }
    let n = be(4)? as usize;
    (0..n)
        .map(|i| {
            let o = 8 + i * 20;
            let (ty, sub, off, len) = (be(o)?, be(o + 4)?, be(o + 8)? as usize, be(o + 12)? as usize);
            Some((ty, sub, d.get(off..off + len)?))
        })
        .collect()
}

impl<'a> Biblioteca<'a> {
    /// Lee un metallib; si es universal, su trozo AIR64.
    pub fn leer(d: &'a [u8]) -> Res<Biblioteca<'a>> {
        if let Some(t) = trozos(d) {
            let air = t.iter().find(|(ty, _, _)| *ty == CPU_AIR64).ok_or("metallib universal sin trozo AIR64")?;
            return Self::leer(air.2);
        }
        if d.get(0..4) != Some(b"MTLB") {
            return Err("no es un metallib (falta MTLB)".into());
        }
        let version = (u16_at(d, 6)?, u16_at(d, 8)?);
        let tipo_archivo = d[10];
        let tam = u64_at(d, 16)?;
        if tam as usize != d.len() {
            return Err(format!("tamaño de la cabecera ({tam}) distinto del archivo ({})", d.len()));
        }
        let (fl_off, _fl_len) = (u64_at(d, 24)? as usize, u64_at(d, 32)?);
        let bitcode = (u64_at(d, 72)?, u64_at(d, 80)?);
        let n = u32_at(d, fl_off)? as usize;
        let mut p = fl_off + 4;
        let mut funciones = Vec::with_capacity(n);
        for _ in 0..n {
            let largo = u32_at(d, p)? as usize; // tamaño de la entrada, incluido este campo
            let fin = p + largo;
            let mut q = p + 4;
            let mut f = Funcion { nombre: String::new(), tipo: Tipo::Otro(255), bitcode_off: 0, bitcode_len: 0, air: None, lenguaje: None };
            while q + 4 <= fin {
                let etiqueta = &d[q..q + 4];
                if etiqueta == b"ENDT" {
                    break;
                }
                let tl = u16_at(d, q + 4)? as usize;
                let v = d.get(q + 6..q + 6 + tl).ok_or("etiqueta fuera del archivo")?;
                match etiqueta {
                    b"NAME" => f.nombre = String::from_utf8_lossy(v.split(|&c| c == 0).next().unwrap_or(&[])).into_owned(),
                    b"TYPE" => {
                        f.tipo = match v.first() {
                            Some(0) => Tipo::Vertex,
                            Some(1) => Tipo::Fragment,
                            Some(2) => Tipo::Kernel,
                            Some(&x) => Tipo::Otro(x),
                            None => Tipo::Otro(255),
                        }
                    }
                    b"MDSZ" if tl >= 8 => f.bitcode_len = u64_at(v, 0)?,
                    b"OFFT" if tl >= 24 => f.bitcode_off = u64_at(v, 16)?,
                    b"VERS" if tl >= 8 => {
                        f.air = Some((u16_at(v, 0)?, u16_at(v, 2)?));
                        f.lenguaje = Some((u16_at(v, 4)?, u16_at(v, 6)?));
                    }
                    _ => {}
                }
                q += 6 + tl;
            }
            funciones.push(f);
            p = fin;
        }
        Ok(Biblioteca { datos: d, version, tipo_archivo, funciones, bitcode })
    }

    /// El bitcode LLVM (AIR) de una función.
    pub fn bitcode(&self, f: &Funcion) -> Res<&'a [u8]> {
        let ini = (self.bitcode.0 + f.bitcode_off) as usize;
        self.datos.get(ini..ini + f.bitcode_len as usize).ok_or_else(|| format!("bitcode de {} fuera del archivo", f.nombre))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un metallib mínimo con una función, construido con el formato de arriba.
    fn ejemplo() -> Vec<u8> {
        let mut entrada = Vec::new();
        let tag = |e: &mut Vec<u8>, t: &[u8], v: &[u8]| {
            e.extend_from_slice(t);
            e.extend_from_slice(&(v.len() as u16).to_le_bytes());
            e.extend_from_slice(v);
        };
        tag(&mut entrada, b"NAME", b"k\0");
        tag(&mut entrada, b"TYPE", &[2]);
        tag(&mut entrada, b"MDSZ", &4u64.to_le_bytes());
        let mut offt = vec![0u8; 16];
        offt.extend_from_slice(&0u64.to_le_bytes());
        tag(&mut entrada, b"OFFT", &offt);
        entrada.extend_from_slice(b"ENDT");
        let mut lista = 1u32.to_le_bytes().to_vec();
        lista.extend_from_slice(&((entrada.len() + 4) as u32).to_le_bytes());
        lista.extend_from_slice(&entrada);
        let mut d = vec![0u8; 88];
        d[0..4].copy_from_slice(b"MTLB");
        let fl = 88u64;
        let bc = fl + lista.len() as u64;
        d.extend_from_slice(&lista);
        d.extend_from_slice(&[0xde, 0xc0, 0x17, 0x0b]);
        let tam = d.len() as u64;
        d[16..24].copy_from_slice(&tam.to_le_bytes());
        d[24..32].copy_from_slice(&fl.to_le_bytes());
        d[32..40].copy_from_slice(&(lista.len() as u64).to_le_bytes());
        d[72..80].copy_from_slice(&bc.to_le_bytes());
        d[80..88].copy_from_slice(&4u64.to_le_bytes());
        d
    }

    #[test]
    fn lee_funciones() {
        let d = ejemplo();
        let b = Biblioteca::leer(&d).unwrap();
        assert_eq!(b.funciones.len(), 1);
        assert_eq!(b.funciones[0].nombre, "k");
        assert_eq!(b.funciones[0].tipo, Tipo::Kernel);
        assert_eq!(b.bitcode(&b.funciones[0]).unwrap(), &[0xde, 0xc0, 0x17, 0x0b]);
    }

    #[test]
    fn universal_elige_air64() {
        let m = ejemplo();
        let mut d = Vec::new();
        d.extend_from_slice(&0xcafe_babeu32.to_be_bytes());
        d.extend_from_slice(&2u32.to_be_bytes());
        let o1 = 8 + 2 * 20;
        let o2 = o1 + 16;
        for (ty, sub, off, len) in [(0x0100_0013u32, 0x53u32, o1, 16usize), (CPU_AIR64, 0xc, o2, m.len())] {
            for v in [ty, sub, off as u32, len as u32, 4] {
                d.extend_from_slice(&v.to_be_bytes());
            }
        }
        d.extend_from_slice(&[0u8; 16]);
        d.extend_from_slice(&m);
        let b = Biblioteca::leer(&d).unwrap();
        assert_eq!(b.funciones[0].nombre, "k");
    }

    #[test]
    fn rechaza_otros_archivos() {
        assert!(Biblioteca::leer(b"hola").is_err());
    }
}
