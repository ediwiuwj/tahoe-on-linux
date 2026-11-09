//! Conversiones puras del ABI de señales y wait de Linux x86_64 a Darwin.
//! No implementa la entrega de señales ni los marcos del manejador invitado.
const SENAL: [i32; 32] = [
    0, 1, 2, 3, 4, 5, 6, 0, 8, 9, 7, 11, 31, 13, 14, 15,
    23, 19, 20, 18, 17, 21, 22, 29, 24, 25, 26, 27, 28, 0, 10, 12,
];

#[no_mangle]
pub extern "C" fn nucleo_senal_linux(darwin: i32) -> i32 {
    if (1..32).contains(&darwin) { SENAL[darwin as usize] } else { -1 }
}

fn senal_darwin(linux: i32) -> i32 {
    // Conserva la política anterior para números sin correspondencia.
    for d in 1..32 {
        if SENAL[d] == linux { return d as i32; }
    }
    linux
}

#[no_mangle]
pub extern "C" fn nucleo_wait_estado(estado: i32) -> i32 {
    let signo = estado & 0x7f;
    if signo == 0 { return estado & 0xff00; }
    if signo < 0x7f { return senal_darwin(signo) | (estado & 0x80); }
    if estado & 0xff == 0x7f {
        return 0x7f | (senal_darwin((estado >> 8) & 0xff) << 8);
    }
    if estado == 0xffff { return 0x137f; }
    estado
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limites_y_senales() {
        for d in [-1, 0, 32, i32::MAX] { assert_eq!(nucleo_senal_linux(d), -1); }
        assert_eq!(nucleo_senal_linux(30), 10);
        assert_eq!(nucleo_senal_linux(20), 17);
        assert_eq!(nucleo_senal_linux(7), 0);
    }
    #[test]
    fn salidas_normales() {
        for codigo in 0..=255 { assert_eq!(nucleo_wait_estado(codigo << 8), codigo << 8); }
    }
    #[test]
    fn terminacion_y_core() {
        assert_eq!(nucleo_wait_estado(10), 30);
        assert_eq!(nucleo_wait_estado(11 | 0x80), 11 | 0x80);
        assert_eq!(nucleo_wait_estado(31), 12);
    }
    #[test]
    fn parada_y_continuacion() {
        assert_eq!(nucleo_wait_estado((19 << 8) | 0x7f), (17 << 8) | 0x7f);
        assert_eq!(nucleo_wait_estado((20 << 8) | 0x7f), (18 << 8) | 0x7f);
        assert_eq!(nucleo_wait_estado(0xffff), 0x137f);
    }
}
