//! Cabecera y trailer de recepción Mach LP64, sin memoria invitada ni efectos.
#![forbid(unsafe_code)]

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cabecera {
    pub bits: u32,
    pub size: u32,
    pub remote: u32,
    pub local: u32,
    pub voucher: u32,
    pub id: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Emisor { pub datos: [u32; 5] }

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Trailer { pub bytes: [u8; 68], pub len: u32 }

pub fn tam_trailer(options: u64) -> u32 {
    match (options >> 24) & 0xf { 0 => 8, 1 => 12, 2 => 20, 3 => 52, _ => 68 }
}

/// Conserva incluso los bits no normalizados del ABI anterior.
pub fn cabecera(mut h: Cabecera) -> Cabecera {
    let txr = h.bits & 0xff;
    let txl = (h.bits >> 8) & 0xff;
    h.bits = (h.bits & !0x1f1f & !0xff)
        | if txl != 0 { crate::mach_mig::disposicion_recibida(txl) } else { 0 }
        | ((if txr != 0 { 16 } else { 0 }) << 8);
    core::mem::swap(&mut h.remote, &mut h.local);
    h
}

/// RCV_LARGE no convierte derechos; IDENTITY sólo copia destino a local.
pub fn cabecera_grande(mut h: Cabecera, options: u64) -> Cabecera {
    if options & 8 != 0 { h.local = h.remote; }
    h
}

pub fn trailer(options: u64, emisor: Emisor, contexto: u64) -> Trailer {
    let len = tam_trailer(options);
    let mut t = Trailer { bytes: [0; 68], len };
    t.bytes[4..8].copy_from_slice(&len.to_le_bytes());
    // Tipo y secuencia siguen a cero, como en la implementación C.
    let s = emisor.datos;
    if len >= 20 {
        t.bytes[12..16].copy_from_slice(&s[1].to_le_bytes());
        t.bytes[16..20].copy_from_slice(&s[2].to_le_bytes());
    }
    if len >= 52 {
        let audit = [s[3], s[1], s[2], s[1], s[2], s[0], s[4], 1];
        for (i, v) in audit.iter().enumerate() {
            t.bytes[20 + i * 4..24 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    if len >= 60 { t.bytes[52..60].copy_from_slice(&contexto.to_le_bytes()); }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(bits: u32) -> Cabecera {
        Cabecera { bits, size: 123, remote: 0x123, local: 0x456, voucher: 7, id: -303 }
    }
    #[test]
    fn cabeceras_y_disposiciones() {
        for txr in 0..=255u32 {
            for txl in 0..=255u32 {
                let original = h(0xabcd0000 | txl << 8 | txr);
                let r = cabecera(original);
                let disp = match txl { 16 => 16, 17 | 19 | 20 => 17, 18 | 21 => 18, _ => txl };
                assert_eq!(r.bits, (original.bits & !0x1f1fu32 & !0xffu32) | disp | ((if txr != 0 {16} else {0}) << 8));
                assert_eq!(r, Cabecera { bits: r.bits, remote: original.local, local: original.remote, ..original });
            }
        }
    }
    #[test]
    fn grande_solo_modifica_identidad() {
        let original = h(u32::MAX);
        assert_eq!(cabecera_grande(original, 4), original);
        assert_eq!(cabecera_grande(original, 12), Cabecera { local: original.remote, ..original });
    }
    #[test]
    fn trailers_bytes_y_opciones() {
        let s = Emisor { datos: [0x12345678, 501, 20, u32::MAX, 100000] };
        let ctx = 0x123456789abcdef0u64;
        for elems in 0..16 {
            let options = (elems << 24) | 0xfedcba980000000c;
            let t = trailer(options, s, ctx);
            let len = match elems {0 => 8, 1 => 12, 2 => 20, 3 => 52, _ => 68};
            assert_eq!(t.len, len);
            let mut expected = [0u8; 68];
            expected[4..8].copy_from_slice(&len.to_le_bytes());
            if len >= 20 {
                expected[12..20].copy_from_slice(&[0xf5, 1, 0, 0, 20, 0, 0, 0]);
            }
            if len >= 52 {
                let words = [u32::MAX, 501, 20, 501, 20, 0x12345678, 100000, 1];
                for (i, w) in words.iter().enumerate() {
                    expected[20+i*4..24+i*4].copy_from_slice(&w.to_le_bytes());
                }
            }
            if len >= 60 { expected[52..60].copy_from_slice(&ctx.to_le_bytes()); }
            assert_eq!(t.bytes, expected);
        }
    }
    #[test]
    fn abi() {
        use core::mem::{align_of, offset_of, size_of};
        assert_eq!((size_of::<Cabecera>(), align_of::<Cabecera>()), (24, 4));
        assert_eq!(offset_of!(Cabecera, id), 20);
        assert_eq!(size_of::<Emisor>(), 20);
        assert_eq!((size_of::<Trailer>(), align_of::<Trailer>()), (72, 4));
        assert_eq!(offset_of!(Trailer, len), 68);
    }
}
