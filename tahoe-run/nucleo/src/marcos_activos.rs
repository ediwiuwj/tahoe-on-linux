//! Registro acotado de zonas de marcos. No intenta adivinar destinos longjmp.
#![forbid(unsafe_code)]
#[repr(C)]
pub struct Rangos { pub extremos: [u64; 16] }
/// Prefijo que puede conservarse si se publica un marco en [inicio, fin).
/// Una zona sobrescrita ya no puede servir para sigreturn; se invalidan también
/// sus descendientes. No elimina marcos sólo por observar un RSP distinto.
/// u32::MAX indica metadatos inválidos; no autoriza ninguna escritura.
pub fn conservar(rangos: Rangos, n: u32, inicio: u64, fin: u64) -> u32 {
    if n > 8 || inicio >= fin { return u32::MAX; }
    let mut conservar = n;
    for i in 0..n as usize {
        let a = rangos.extremos[i*2];
        let b = rangos.extremos[i*2+1];
        if a >= b { return u32::MAX; }
        if inicio < b && a < fin { conservar = conservar.min(i as u32); }
    }
    conservar
}
#[cfg(test)]
mod tests {
    use super::*;
    fn rangos() -> Rangos {
        let mut r=Rangos{extremos:[0;16]};
        r.extremos[..6].copy_from_slice(&[1000,1900,100,900,10,90]); r
    }
    #[test] fn anidados_no_se_retiran() {
        assert_eq!(conservar(rangos(),3,1,9),3);
        assert_eq!(conservar(rangos(),3,90,100),3); // adyacentes, no solapados
    }
    #[test] fn reuso_invalida_solo_el_prefijo_necesario() {
        assert_eq!(conservar(rangos(),3,1000,1900),0);
        assert_eq!(conservar(rangos(),3,100,900),1);
        assert_eq!(conservar(rangos(),3,30,80),2);
    }
    #[test] fn metadatos_invalidos_no_autorizan_retirada() {
        assert_eq!(conservar(rangos(),9,1,2),u32::MAX);
        assert_eq!(conservar(rangos(),3,2,1),u32::MAX);
        let mut r=rangos(); r.extremos[3]=0;
        assert_eq!(conservar(r,3,1,2),u32::MAX);
    }
}
