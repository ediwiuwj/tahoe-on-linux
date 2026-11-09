//! Codecs y validación puros para entrega diferida de INT/USR1/USR2.
//! La frontera C copia el xstate completo a almacenamiento privado por hilo;
//! este módulo no accede a memoria invitada ni restaura CPU directamente.
#![forbid(unsafe_code)]

const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;
pub const FXSAVE_TAM: usize = 512;
/// Límite de almacenamiento explícito, no una afirmación sobre el máximo Linux.
/// Un estado mayor debe rechazarse, nunca truncarse (p. ej., AMX futuro).
pub const XSTATE_LIMITE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error { Truncado, Metadata, Limite, Desborde }

fn u32_en(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

/// Tamaño que copiar desde fpregs de un marco Linux x86_64.
/// `bytes` debe ser una copia leída con límites por la frontera de memoria.
/// Valida _fpx_sw_bytes y magic2; no valida los componentes internos de XSAVE
/// ni autoriza por sí sola restauración mediante rt_sigreturn.
pub fn longitud_xstate(bytes: &[u8]) -> Result<usize, Error> {
    if bytes.len() < FXSAVE_TAM { return Err(Error::Truncado); }
    let magic = u32_en(bytes, 464);
    if magic == 0 { return Ok(FXSAVE_TAM); }
    if magic != FP_XSTATE_MAGIC1 { return Err(Error::Metadata); }
    let extended_size = u32_en(bytes, 468) as usize;
    let xstate_size = u32_en(bytes, 480) as usize;
    if extended_size > XSTATE_LIMITE { return Err(Error::Limite); }
    // XSAVE contiene la cabecera de 64 bytes después de FXSAVE.
    if xstate_size < 576 || xstate_size.checked_add(4) != Some(extended_size) {
        return Err(Error::Metadata);
    }
    if bytes.len() < extended_size { return Err(Error::Truncado); }
    if u32_en(bytes, extended_size - 4) != FP_XSTATE_MAGIC2 { return Err(Error::Metadata); }
    Ok(extended_size)
}

/// Reserva por debajo del redzone, con RSP de entrada a función == 8 mod 16.
/// No comprueba permisos ni límites de una pila alternativa: eso corresponde
/// a la frontera de memoria antes de publicar el marco.
pub fn reservar_pila(rsp: u64, tam_marco: u64) -> Result<u64, Error> {
    let base = rsp.checked_sub(128).and_then(|v| v.checked_sub(tam_marco))
        .ok_or(Error::Desborde)?;
    (base & !15).checked_sub(8).ok_or(Error::Desborde)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn poner(bytes: &mut [u8], offset: usize, valor: u32) {
        bytes[offset..offset + 4].copy_from_slice(&valor.to_le_bytes());
    }
    #[test]
    fn fp_legacy_y_truncados() {
        let bytes = [0; 512];
        for n in 0..512 { assert_eq!(longitud_xstate(&bytes[..n]), Err(Error::Truncado)); }
        assert_eq!(longitud_xstate(&bytes), Ok(512));
    }
    #[test]
    fn xstate_no_se_trunca() {
        let mut bytes = [0; 836];
        poner(&mut bytes, 464, FP_XSTATE_MAGIC1);
        poner(&mut bytes, 468, 836);
        poner(&mut bytes, 480, 832);
        poner(&mut bytes, 832, FP_XSTATE_MAGIC2);
        assert_eq!(longitud_xstate(&bytes), Ok(836));
        assert_eq!(longitud_xstate(&bytes[..835]), Err(Error::Truncado));
        poner(&mut bytes, 832, 0);
        assert_eq!(longitud_xstate(&bytes), Err(Error::Metadata));
        poner(&mut bytes, 468, XSTATE_LIMITE as u32 + 1);
        assert_eq!(longitud_xstate(&bytes), Err(Error::Limite));
        poner(&mut bytes, 468, 512);
        poner(&mut bytes, 480, 508);
        assert_eq!(longitud_xstate(&bytes), Err(Error::Metadata));
    }
    #[test]
    fn redzone_y_alineacion() {
        for rsp in 4096..4112 {
            let nuevo = reservar_pila(rsp, 1024).unwrap();
            assert_eq!(nuevo % 16, 8);
            assert!(nuevo + 1024 <= rsp - 128);
        }
        assert_eq!(reservar_pila(128, 0), Err(Error::Desborde));
        assert_eq!(reservar_pila(0, u64::MAX), Err(Error::Desborde));
    }
}

// ABI normal verificado en XNU f6217f... osfmk/mach/i386/_structs.h.
// Linux usa FXSAVE64: RIP/RDP de 64 bits, no los pares offset/selector Darwin.
use crate::marco_senal::{FloatState64, MarcoNormal64, Mcontext64, ThreadState64, Ucontext64};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Maquina { pub regs: [u64; 21], pub fp: [u8; 512] }
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MarcoBytes { pub uc: [u8; 56], pub mc: [u8; 712] }
#[repr(C)]
pub struct Vuelta { pub maquina: Maquina, pub mascara: u32, pub onstack: i32, pub error: i32 }

fn fp_darwin(fp: &[u8; 512]) -> FloatState64 {
    let mut d = FloatState64::default();
    // Campos escalares; no copiar FXSAVE como si fuera float_state64.
    for (linux, darwin, n) in [(0,8,2), (2,10,2), (4,12,1), (6,14,2),
        (8,16,4), (16,24,4), (24,32,4), (28,36,4)] {
        d.bytes[darwin..darwin+n].copy_from_slice(&fp[linux..linux+n]);
    }
    for i in 0..8 { // sólo los 80 bits x87, no los 6 bytes reservados
        d.bytes[40+i*16..50+i*16].copy_from_slice(&fp[32+i*16..42+i*16]);
    }
    for i in 0..16 {
        d.bytes[168+i*16..184+i*16].copy_from_slice(&fp[160+i*16..176+i*16]);
    }
    d
}

fn fp_linux(d: &FloatState64, saved: &[u8; 512]) -> Result<[u8; 512], Error> {
    let baseline = fp_darwin(saved);
    // El formato normal no representa las mitades altas RIP/RDP. Preservarlas
    // privadamente; no inventar cómo aplicar un cambio de offset/selector.
    if d.bytes[16..32] != baseline.bytes[16..32] { return Err(Error::Metadata); }
    let mask = u32_en(saved, 28);
    let mask = if mask == 0 { 0x0000_ffbf } else { mask };
    if u32_en(&d.bytes, 32) & !mask != 0 { return Err(Error::Metadata); }
    let mut fp = *saved;
    for (linux, darwin, n) in [(0,8,2), (2,10,2), (4,12,1), (6,14,2), (24,32,4)] {
        fp[linux..linux+n].copy_from_slice(&d.bytes[darwin..darwin+n]);
    }
    for i in 0..8 {
        fp[32+i*16..42+i*16].copy_from_slice(&d.bytes[40+i*16..50+i*16]);
    }
    for i in 0..16 {
        fp[160+i*16..176+i*16].copy_from_slice(&d.bytes[168+i*16..184+i*16]);
    }
    Ok(fp)
}

fn hilo(regs: &[u64; 21]) -> ThreadState64 {
    let mut bytes = [0; 168];
    for (i, r) in regs.iter().enumerate() { bytes[i*8..i*8+8].copy_from_slice(&r.to_le_bytes()); }
    // Longitud fija comprobada por construcción.
    ThreadState64::desde_bytes(&bytes).unwrap()
}

/// FFI exclusivamente por valor: sin direcciones ni referencias C en Rust.
pub fn nucleo_senal_codificar(contexto: Ucontext64, maquina: Maquina) -> MarcoBytes {
    let marco = MarcoNormal64 { contexto, maquina: Mcontext64 {
        hilo: hilo(&maquina.regs), flotante: fp_darwin(&maquina.fp), ..Mcontext64::default()
    }};
    let mut bytes = MarcoBytes { uc: [0; 56], mc: [0; 712] };
    // Ambos destinos tienen el tamaño exacto del codec. Los metadatos los
    // valida el camino inverso antes de restaurar.
    marco.contexto.a_bytes(&mut bytes.uc).unwrap();
    marco.maquina.a_bytes(&mut bytes.mc).unwrap();
    bytes
}

fn direccion_usuario(v: u64) -> bool { v != 0 && v < 0x0000_8000_0000_0000 }

pub fn nucleo_senal_decodificar(bytes: MarcoBytes, guardada: Maquina, mc: u64) -> Vuelta {
    let mut vuelta = Vuelta { maquina: guardada, mascara: 0, onstack: 0, error: 22 };
    let Ok(marco) = MarcoNormal64::desde_slices(&bytes.uc, &bytes.mc, mc) else { return vuelta; };
    let t = marco.maquina.hilo;
    let old = hilo(&guardada.regs);
    // No restaurar selectores del invitado sobre FS/GS.base ni privilegios.
    // Por ahora sólo se admite cambiar flags aritméticos y DF en el ucontext.
    if t.cs != old.cs || t.fs != old.fs || t.gs != old.gs ||
        (t.rflags ^ old.rflags) & !0xcd5 != 0 ||
        !direccion_usuario(t.rip) || !direccion_usuario(t.rsp) ||
        !(0..=1).contains(&marco.contexto.onstack) { return vuelta; }
    let Ok(fp) = fp_linux(&marco.maquina.flotante, &guardada.fp) else { return vuelta; };
    let mut thread_bytes = [0; 168];
    t.a_bytes(&mut thread_bytes).unwrap();
    for i in 0..21 {
        let mut r = [0; 8]; r.copy_from_slice(&thread_bytes[i*8..i*8+8]);
        vuelta.maquina.regs[i] = u64::from_le_bytes(r);
    }
    vuelta.maquina.fp = fp;
    // SIGKILL y SIGSTOP nunca se bloquean (numeración Darwin).
    vuelta.mascara = marco.contexto.sigmask & !((1 << 8) | (1 << 16));
    vuelta.onstack = marco.contexto.onstack;
    vuelta.error = 0;
    vuelta
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PrefijoFp { pub bytes: [u8; 512] }

/// XSAVE puede omitir componentes en estado inicial. Materializarlos antes
/// de exponerlos a Darwin; después se restauran con XSTATE_BV.x87/SSE activos.
pub fn nucleo_senal_fp_inicial(mut fp: PrefijoFp, bv: u64) -> PrefijoFp {
    if bv & 1 == 0 {
        fp.bytes[..24].fill(0);
        fp.bytes[..2].copy_from_slice(&0x037fu16.to_le_bytes());
        fp.bytes[32..160].fill(0);
    }
    if bv & 2 == 0 {
        fp.bytes[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
        fp.bytes[160..416].fill(0);
    }
    fp
}
/// Consulta por valor del largo anunciado; C lee luego esa cantidad y valida
/// el magic2 antes de copiar. No es una validación completa del XSAVE.
pub fn nucleo_senal_xstate_largo(fp: PrefijoFp) -> u32 {
    let magic = u32_en(&fp.bytes, 464);
    if magic == 0 { return 512; }
    let n = u32_en(&fp.bytes, 468) as usize;
    let x = u32_en(&fp.bytes, 480) as usize;
    if magic != FP_XSTATE_MAGIC1 || n > XSTATE_LIMITE || x < 576 || x.checked_add(4) != Some(n) { return 0; }
    n as u32
}

pub fn nucleo_senal_pila(rsp: u64, tam: u64) -> u64 {
    reservar_pila(rsp, tam).unwrap_or(0)
}

/// Selección determinista de auto-señales soportadas; cero si ninguna es elegible.
/// No consume pendientes: C sólo las retira después de publicar el marco.
pub fn nucleo_senal_elegible(pendientes: u32, mascara: u32, permitidas: u32) -> u32 {
    let listas = pendientes & !mascara & permitidas & ((1u32 << 1) | (1u32 << 29) | (1u32 << 30));
    listas & listas.wrapping_neg()
}

/// Resultado empaquetado: máscara en 32 bits bajos, errno en altos.
pub fn nucleo_senal_mascara(old: u32, set: u32, how: u64, has_set: u32) -> u64 {
    let next = if has_set == 0 { old } else {
        match how { 1 => old | set, 2 => old & !set, 3 => set, _ => return 22u64 << 32 }
    };
    (next & !((1 << 8) | (1 << 16))) as u64
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    #[test]
    fn pendientes_y_prioridad() {
        let int = 1u32 << 1;
        let uno = 1u32 << 29;
        let dos = 1u32 << 30;
        // Todas las combinaciones INT/USR, además de bits no soportados.
        let casos = [0, int, uno, dos, int | uno, int | dos, uno | dos,
            int | uno | dos, 1, 1u32 << 31, u32::MAX];
        for p in casos {
            for m in casos {
                for a in casos {
                    let esperado = if p & int != 0 && m & int == 0 && a & int != 0 { int }
                        else if p & uno != 0 && m & uno == 0 && a & uno != 0 { uno }
                        else if p & dos != 0 && m & dos == 0 && a & dos != 0 { dos }
                        else { 0 };
                    assert_eq!(nucleo_senal_elegible(p, m, a), esperado);
                }
            }
        }
    }
    #[test]
    fn mascaras_y_senales_no_bloqueables() {
        assert_eq!(nucleo_senal_mascara(1, 2, 1, 1), 3);
        assert_eq!(nucleo_senal_mascara(3, 2, 2, 1), 1);
        assert_eq!(nucleo_senal_mascara(3, 2, 3, 1), 2);
        assert_eq!(nucleo_senal_mascara(3, 2, 99, 1), 22u64 << 32);
        assert_eq!(nucleo_senal_mascara(3, 2, 99, 0), 3);
        assert_eq!(nucleo_senal_mascara(0, u32::MAX, 3, 1) as u32 & ((1<<8) | (1<<16)), 0);
    }
    #[test]
    fn frontera_por_valor_y_xstate_inicial() {
        use core::mem::size_of;
        assert_eq!(size_of::<Maquina>(), 680);
        assert_eq!(size_of::<MarcoBytes>(), 768);
        assert_eq!(size_of::<Vuelta>(), 696);
        let fp = PrefijoFp { bytes: [0xa5; 512] };
        let init = nucleo_senal_fp_inicial(fp, 0);
        assert_eq!(&init.bytes[..2], &0x037fu16.to_le_bytes());
        assert_eq!(&init.bytes[24..28], &0x1f80u32.to_le_bytes());
        assert!(init.bytes[32..416].iter().all(|&b| b == 0));
        assert_eq!(&init.bytes[464..], &fp.bytes[464..]);
        assert_eq!(nucleo_senal_fp_inicial(fp, 3).bytes, fp.bytes);
    }
    #[test]
    fn fp_offsets_y_reservados() {
        let mut fp = [0; 512];
        fp[8..16].copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
        fp[16..24].copy_from_slice(&0x7654_3210_abcdef01u64.to_le_bytes());
        fp[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
        fp[32..42].fill(0x42); fp[160..416].fill(0xa5);
        fp[464..512].fill(0xcc);
        let mut d = fp_darwin(&fp);
        assert_eq!(&d.bytes[16..20], &fp[8..12]);
        assert_eq!(&d.bytes[20..24], &[0; 4]);
        assert_eq!(&d.bytes[40..50], &[0x42; 10]);
        assert_eq!(&d.bytes[168..424], &[0xa5; 256]);
        assert_eq!(fp_linux(&d, &fp), Ok(fp));
        d.bytes[168] = 0x11;
        let changed = fp_linux(&d, &fp).unwrap();
        assert_eq!(changed[160], 0x11);
        assert_eq!(&changed[464..], &fp[464..]);
        d.bytes[20] = 1;
        assert_eq!(fp_linux(&d, &fp), Err(Error::Metadata));
    }
    #[test]
    fn contexto_normal_y_rechazos() {
        let mut m = Maquina { regs: [0; 21], fp: [0; 512] };
        m.regs[7] = 0x8000; m.regs[16] = 0x1000; m.regs[17] = 0x202; m.regs[18] = 0x2b;
        let uc = Ucontext64 { mcsize: 712, mcontext: 0x2000, sigmask: u32::MAX, ..Ucontext64::default() };
        let bytes = nucleo_senal_codificar(uc, m);
        let restored = nucleo_senal_decodificar(bytes, m, 0x2000);
        assert_eq!(restored.error, 0);
        assert_eq!(restored.maquina.regs, m.regs);
        assert_eq!(restored.mascara & ((1<<8) | (1<<16)), 0);
        assert_eq!(nucleo_senal_decodificar(bytes, m, 0x2001).error, 22);
        let mut bad = bytes;
        bad.mc[16+18*8] = 0x33;
        assert_eq!(nucleo_senal_decodificar(bad, m, 0x2000).error, 22);
        bad = bytes; bad.mc[16+17*8+1] ^= 2; // IF privilegiado
        assert_eq!(nucleo_senal_decodificar(bad, m, 0x2000).error, 22);
        bad = bytes; bad.mc[184+35] = 0x80; // MXCSR reservado
        assert_eq!(nucleo_senal_decodificar(bad, m, 0x2000).error, 22);
    }
}
