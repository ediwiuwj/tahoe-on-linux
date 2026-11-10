//! Construcción de respuestas MIG LP64; enteramente sobre valores propios.
#![forbid(unsafe_code)]
const NDR: [u8;8] = [0,0,0,0,1,0,0,0];
#[repr(C)]
#[derive(Clone,Copy)]
pub struct Respuesta { pub bytes: [u8;60], pub len: u32 }
fn u32_en(b: &mut [u8;60], off: usize, v: u32) { b[off..off+4].copy_from_slice(&v.to_le_bytes()); }
fn base(id: i32, compleja: bool, len: u32) -> Respuesta {
    let mut r=Respuesta{bytes:[0;60],len};
    u32_en(&mut r.bytes,0,if compleja {0x80000000} else {0});
    u32_en(&mut r.bytes,20,id.wrapping_add(100) as u32);
    r
}
pub fn error(id: i32, codigo: i32) -> Respuesta {
    let mut r=base(id,false,36);
    r.bytes[24..32].copy_from_slice(&NDR);
    u32_en(&mut r.bytes,32,codigo as u32); r
}
pub fn puerto(id:i32,nombre:u32,disposicion:u8) -> Respuesta {
    let mut r=base(id,true,40);
    u32_en(&mut r.bytes,4,40);
    u32_en(&mut r.bytes,24,1);
    u32_en(&mut r.bytes,28,nombre);
    r.bytes[38]=disposicion; r
}
/// `direccion` es un valor ABI, no se desreferencia. Su ownership y mapping
/// se mantienen en la frontera Linux; aquí sólo se codifica el descriptor.
pub fn ool(id:i32,direccion:u64,len:u32,cantidad:u32,puertos:u32,extra:u32,tiene_extra:u32) -> Respuesta {
    let mut r=base(id,true,if tiene_extra!=0 {60} else {56});
    u32_en(&mut r.bytes,24,1);
    r.bytes[28..36].copy_from_slice(&direccion.to_le_bytes());
    r.bytes[36]=1; // deallocate
    if puertos!=0 { r.bytes[38]=17; r.bytes[39]=2; u32_en(&mut r.bytes,40,cantidad); }
    else {r.bytes[37]=1; r.bytes[39]=1; u32_en(&mut r.bytes,40,len);}
    r.bytes[44..52].copy_from_slice(&NDR);
    if tiene_extra!=0 {u32_en(&mut r.bytes,52,extra);u32_en(&mut r.bytes,56,cantidad);}
    else {u32_en(&mut r.bytes,52,cantidad);}
    r
}
pub fn disposicion_recibida(tx:u32) -> u32 {
    match tx { 16=>16,17|19|20=>17,18|21=>18,_=>tx }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn word(r:&Respuesta,o:usize)->u32 {u32::from_le_bytes(r.bytes[o..o+4].try_into().unwrap())}
    #[test] fn simple_y_error_firmado(){
        let r=error(3405,-303);assert_eq!(r.len,36);assert_eq!(word(&r,20),3505);
        assert_eq!(&r.bytes[24..32],&NDR);assert_eq!(word(&r,32),(-303i32)as u32);
        assert!(r.bytes[..20].iter().all(|&b|b==0));
    }
    #[test] fn id_no_desborda(){assert_eq!(word(&error(i32::MAX,0),20),(i32::MAX as u32).wrapping_add(100));}
    #[test] fn derecho_de_puerto(){let r=puerto(205,0x2203,17);assert_eq!(r.len,40);assert_eq!(word(&r,0),0x80000000);assert_eq!(word(&r,28),0x2203);assert_eq!(r.bytes[38],17);assert_eq!(r.bytes[39],0);}
    #[test] fn memoria_y_puertos_ool(){
        let r=ool(419,0x123456789abc,4,1,1,0,0);assert_eq!(r.len,56);
        assert_eq!(&r.bytes[28..36],&0x123456789abcu64.to_le_bytes());assert_eq!(r.bytes[38],17);assert_eq!(r.bytes[39],2);assert_eq!(word(&r,52),1);
        let r=ool(204,4096,80,20,0,2,1);assert_eq!(r.len,60);assert_eq!(r.bytes[37],1);assert_eq!(r.bytes[39],1);assert_eq!(word(&r,40),80);assert_eq!(word(&r,52),2);assert_eq!(word(&r,56),20);
    }
}
