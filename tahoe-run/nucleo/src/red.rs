//! Traducción pura de banderas de mensajes Darwin → Linux x86_64.
//! No accede a memoria invitada, no reserva memoria y no usa `unsafe`.

const MAPA: [(u32, u32); 8] = [
    (0x01, 0x01), // OOB
    (0x02, 0x02), // PEEK
    (0x04, 0x04), // DONTROUTE
    (0x08, 0x80), // EOR
    (0x10, 0x20), // TRUNC
    (0x20, 0x08), // CTRUNC
    (0x40, 0x100), // WAITALL
    (0x80, 0x40), // DONTWAIT
];
const NOSIGNAL: u32 = 0x4000;

/// Conserva la política anterior: suprimir SIGPIPE en el anfitrión hasta que
/// el emulador pueda entregarlo al invitado. Los bits sin equivalente se ignoran;
/// pasarlos directamente activaría opciones distintas de Linux.
#[no_mangle]
pub extern "C" fn nucleo_red_msg_flags(darwin: u32) -> u32 {
    let mut linux = NOSIGNAL;
    for (origen, destino) in MAPA {
        if darwin & origen != 0 {
            linux |= destino;
        }
    }
    linux
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todas_las_combinaciones_conocidas() {
        // Oráculo independiente de la tabla usada en producción.
        for f in 0u32..=255 {
            let esperado = NOSIGNAL | (f & 7) | ((f & 8) << 4)
                | ((f & 16) << 1) | ((f & 32) >> 2)
                | ((f & 64) << 2) | ((f & 128) >> 1);
            assert_eq!(nucleo_red_msg_flags(f), esperado, "banderas {f:#x}");
        }
    }

    #[test]
    fn bits_desconocidos_no_se_filtran() {
        for bit in 8..32 {
            assert_eq!(nucleo_red_msg_flags(1u32 << bit), NOSIGNAL);
        }
        assert_eq!(nucleo_red_msg_flags(u32::MAX), nucleo_red_msg_flags(255));
    }
}
