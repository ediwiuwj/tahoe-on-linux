//! Lectura segura de acciones de archivo Darwin; no accede a memoria invitada.
#![forbid(unsafe_code)]
pub const TAM_ACCION: usize = 1040;
#[repr(C)]
pub struct BytesAccion { pub bytes: [u8; TAM_ACCION] }
#[repr(C)]
pub struct Accion {
    pub tipo: i32, pub fd: i32, pub argumento: i32, pub oflag: i32,
    pub modo: u16, pub ruta: [u8; 1024],
}
#[repr(C)]
pub struct Resultado { pub accion: Accion, pub error: i32 }

/// Cuenta baja, errno Darwin alto. Capacidad local explícita: nunca truncar.
pub fn cabecera(capacidad: i32, cantidad: i32, tam: u64, limite: u32) -> u64 {
    let error = if tam < 8 || capacidad < 0 || cantidad < 0 || cantidad > capacidad { 22 }
        else if cantidad as u32 > limite { 7 } // E2BIG
        else if 8 + cantidad as u64 * TAM_ACCION as u64 > tam { 22 }
        else { 0 };
    if error != 0 { (error as u64) << 32 } else { cantidad as u64 }
}
fn entero(b: &[u8], i: usize) -> i32 {
    i32::from_le_bytes([b[i], b[i+1], b[i+2], b[i+3]])
}
pub fn leer(bytes: BytesAccion) -> Resultado {
    let b = bytes.bytes;
    let mut r = Resultado { accion: Accion { tipo: entero(&b,0), fd: entero(&b,4),
        argumento: entero(&b,8), oflag: 0, modo: 0, ruta: [0;1024] }, error: 0 };
    let tipo = r.accion.tipo;
    if !matches!(tipo, 0|1|2|3|5|6) { r.error = 45; return r; }
    if tipo != 5 && (r.accion.fd < 0 || (tipo == 2 && r.accion.argumento < 0)) {
        r.error = 9; return r;
    }
    let inicio = match tipo { 0 => 14, 5 => 8, _ => return r };
    let ruta = &b[inicio..inicio+1024];
    let Some(fin) = ruta.iter().position(|&c| c == 0) else { r.error = 63; return r; };
    r.accion.ruta[..fin].copy_from_slice(&ruta[..fin]);
    if tipo == 0 {
        r.accion.oflag = entero(&b,8);
        r.accion.modo = u16::from_le_bytes([b[12],b[13]]);
    }
    r
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn layout_ffi() {
        use core::mem::{size_of, offset_of};
        assert_eq!(size_of::<BytesAccion>(),1040);
        assert_eq!(size_of::<Accion>(),1044);
        assert_eq!(offset_of!(Accion,ruta),18);
        assert_eq!(size_of::<Resultado>(),1048);
    }
    #[test] fn cuenta_no_se_trunca() {
        assert_eq!(cabecera(32,32,8+32*1040,32),32);
        assert_eq!(cabecera(33,33,8+33*1040,32)>>32,7);
        for (cap,n,tam) in [(-1,0,8),(2,3,4000),(3,-1,4000),(1,1,1047),(0,0,7)] {
            assert_eq!(cabecera(cap,n,tam,32)>>32,22);
        }
    }
    fn bytes(tipo:i32, fd:i32) -> BytesAccion {
        let mut b=[0;1040]; b[..4].copy_from_slice(&tipo.to_le_bytes());
        b[4..8].copy_from_slice(&fd.to_le_bytes()); BytesAccion{bytes:b}
    }
    #[test] fn tipos_y_descriptores() {
        for tipo in [0,1,2,3,5,6] { assert_eq!(leer(bytes(tipo,0)).error,0); }
        assert_eq!(leer(bytes(4,0)).error,45);
        assert_eq!(leer(bytes(99,0)).error,45);
        assert_eq!(leer(bytes(1,-1)).error,9);
    }
    #[test] fn ruta_y_flags() {
        let mut b=bytes(0,7);
        b.bytes[8..12].copy_from_slice(&123i32.to_le_bytes());
        b.bytes[12..14].copy_from_slice(&0o600u16.to_le_bytes());
        b.bytes[14..18].copy_from_slice(b"/tmp");
        let r=leer(b); assert_eq!(r.error,0); assert_eq!(r.accion.fd,7);
        assert_eq!(r.accion.oflag,123); assert_eq!(r.accion.modo,0o600);
        assert_eq!(&r.accion.ruta[..5],b"/tmp\0");
        let mut b=bytes(5,0); b.bytes[8..1032].fill(b'x');
        assert_eq!(leer(b).error,63);
    }
}
