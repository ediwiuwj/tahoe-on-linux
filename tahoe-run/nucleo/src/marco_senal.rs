//! Contextos de señal Darwin x86_64 normales (FP, sin AVX ni FULL/LDT).
//!
//! ABI contrastado mediante `gh` con apple-oss-distributions/xnu, revisión
//! f6217f891ac0bb64f3d375211650a4c1ff8ca1ea:
//! bsd/sys/_types/_ucontext64.h, bsd/i386/_mcontext.h,
//! osfmk/mach/i386/_structs.h y bsd/dev/i386/unix_signal.c.
//! No entrega señales, no accede a direcciones invitadas ni restaura CPU.
//! La validación aquí es estructural; no sustituye thread_setstatus de XNU.
#![forbid(unsafe_code)]

pub const UCONTEXT64_TAM: usize = 56;
pub const EXCEPTION64_TAM: usize = 16;
pub const THREAD64_TAM: usize = 21 * 8;
/// 8 reservados + cabecera FPU de 32 + 8 ST/MM de 16 + 16 XMM de 16
/// + 96 reservados + int final de 4. No es un FXSAVE crudo de 512 bytes.
pub const FLOAT64_TAM: usize = 524;
pub const THREAD64_OFFSET: usize = EXCEPTION64_TAM;
pub const FLOAT64_OFFSET: usize = THREAD64_OFFSET + THREAD64_TAM;
/// 16 + 168 + 524 = 708, redondeado a alineación 8 del mcontext.
pub const MCONTEXT64_TAM: usize = 712;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Truncado { necesario: usize, disponible: usize },
    TamanoContexto(u64),
    DireccionContexto { esperada: u64, recibida: u64 },
    DesbordeDireccion,
    TokenInvalido,
}

fn longitud(bytes: &[u8], necesario: usize) -> Result<(), Error> {
    if bytes.len() < necesario {
        Err(Error::Truncado { necesario, disponible: bytes.len() })
    } else {
        Ok(())
    }
}

// Sólo se llaman después de comprobar la longitud total del objeto.
fn leer<const N: usize>(bytes: &[u8], offset: usize) -> [u8; N] {
    let mut valor = [0; N];
    valor.copy_from_slice(&bytes[offset..offset + N]);
    valor
}

/// Genera layouts C y codecs explícitos: nunca se copian representaciones de Rust.
macro_rules! estructura {
    ($nombre:ident, $tam:expr, { $($campo:ident: $tipo:ident => $offset:expr),* $(,)? }) => {
        #[repr(C)]
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $nombre { $(pub $campo: $tipo),* }
        impl $nombre {
            /// Lee el prefijo ABI; admite bytes posteriores sin consumirlos.
            pub fn desde_bytes(bytes: &[u8]) -> Result<Self, Error> {
                longitud(bytes, $tam)?;
                Ok(Self { $($campo: $tipo::from_le_bytes(leer(bytes, $offset))),* })
            }
            /// Escribe sólo el prefijo ABI. Si está truncado, no modifica nada.
            pub fn a_bytes(&self, bytes: &mut [u8]) -> Result<(), Error> {
                longitud(bytes, $tam)?;
                $(let valor = self.$campo.to_le_bytes();
                  bytes[$offset..$offset + valor.len()].copy_from_slice(&valor);)*
                Ok(())
            }
        }
    };
}

estructura!(Ucontext64, UCONTEXT64_TAM, {
    onstack: i32 => 0,
    sigmask: u32 => 4,
    stack_sp: u64 => 8,
    stack_size: u64 => 16,
    stack_flags: i32 => 24,
    // Relleno ABI explícito para preservar roundtrips byte a byte.
    stack_relleno: u32 => 28,
    link: u64 => 32,
    mcsize: u64 => 40,
    mcontext: u64 => 48,
});

estructura!(ExceptionState64, EXCEPTION64_TAM, {
    trapno: u16 => 0,
    cpu: u16 => 2,
    err: u32 => 4,
    faultvaddr: u64 => 8,
});

estructura!(ThreadState64, THREAD64_TAM, {
    rax: u64 => 0, rbx: u64 => 8, rcx: u64 => 16, rdx: u64 => 24,
    rdi: u64 => 32, rsi: u64 => 40, rbp: u64 => 48, rsp: u64 => 56,
    r8: u64 => 64, r9: u64 => 72, r10: u64 => 80, r11: u64 => 88,
    r12: u64 => 96, r13: u64 => 104, r14: u64 => 112, r15: u64 => 120,
    rip: u64 => 128, rflags: u64 => 136, cs: u64 => 144,
    fs: u64 => 152, gs: u64 => 160,
});

/// Estado FPU opaco, preservado íntegramente, incluidos los reservados.
/// Su alineación C es 4 (los registros MMST/XMM son arrays de char).
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatState64 {
    pub bytes: [u8; FLOAT64_TAM],
}

impl Default for FloatState64 {
    fn default() -> Self { Self { bytes: [0; FLOAT64_TAM] } }
}

impl FloatState64 {
    pub fn desde_bytes(bytes: &[u8]) -> Result<Self, Error> {
        longitud(bytes, FLOAT64_TAM)?;
        Ok(Self { bytes: leer(bytes, 0) })
    }

    pub fn a_bytes(&self, bytes: &mut [u8]) -> Result<(), Error> {
        longitud(bytes, FLOAT64_TAM)?;
        bytes[..FLOAT64_TAM].copy_from_slice(&self.bytes);
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mcontext64 {
    pub excepcion: ExceptionState64,
    pub hilo: ThreadState64,
    pub flotante: FloatState64,
    pub relleno: [u8; 4],
}

impl Mcontext64 {
    pub fn desde_bytes(bytes: &[u8]) -> Result<Self, Error> {
        longitud(bytes, MCONTEXT64_TAM)?;
        Ok(Self {
            excepcion: ExceptionState64::desde_bytes(bytes)?,
            hilo: ThreadState64::desde_bytes(&bytes[THREAD64_OFFSET..])?,
            flotante: FloatState64::desde_bytes(&bytes[FLOAT64_OFFSET..])?,
            relleno: leer(bytes, FLOAT64_OFFSET + FLOAT64_TAM),
        })
    }

    pub fn a_bytes(&self, bytes: &mut [u8]) -> Result<(), Error> {
        longitud(bytes, MCONTEXT64_TAM)?;
        self.excepcion.a_bytes(bytes)?;
        self.hilo.a_bytes(&mut bytes[THREAD64_OFFSET..])?;
        self.flotante.a_bytes(&mut bytes[FLOAT64_OFFSET..])?;
        bytes[FLOAT64_OFFSET + FLOAT64_TAM..MCONTEXT64_TAM]
            .copy_from_slice(&self.relleno);
        Ok(())
    }
}

/// XOR de unix_signal.c. `uctx_convertido` debe ser la dirección ya convertida
/// por la capa equivalente a machine_thread_siguctx_pointer_convert_to_user.
/// No genera secretos, no gestiona señales anidadas ni evita replay por sí solo.
pub const fn token(uctx_convertido: u64, secreto: u64) -> u64 {
    uctx_convertido ^ secreto
}

pub fn validar_token(uctx_convertido: u64, secreto: u64, recibido: u64) -> Result<(), Error> {
    if recibido == token(uctx_convertido, secreto) { Ok(()) }
    else { Err(Error::TokenInvalido) }
}

/// Dos objetos ABI separados: XNU no define un sigframe64 contiguo como en i386.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarcoNormal64 {
    pub contexto: Ucontext64,
    pub maquina: Mcontext64,
}

impl MarcoNormal64 {
    /// La capa llamadora proporciona el slice que corresponde a `direccion_mc`.
    /// Exige mcsize normal (política de este codec, no comprobación de sigreturn
    /// de XNU, que selecciona el tamaño según current_xstate). No valida permisos,
    /// direcciones canónicas, selectores, RFLAGS ni contenido FPU.
    pub fn desde_slices(
        contexto: &[u8], maquina: &[u8], direccion_mc: u64,
    ) -> Result<Self, Error> {
        let contexto = Ucontext64::desde_bytes(contexto)?;
        Self::validar_contexto(&contexto, direccion_mc)?;
        Ok(Self { contexto, maquina: Mcontext64::desde_bytes(maquina)? })
    }

    fn validar_contexto(contexto: &Ucontext64, direccion_mc: u64) -> Result<(), Error> {
        if contexto.mcsize != MCONTEXT64_TAM as u64 {
            return Err(Error::TamanoContexto(contexto.mcsize));
        }
        if contexto.mcontext != direccion_mc {
            return Err(Error::DireccionContexto {
                esperada: direccion_mc, recibida: contexto.mcontext,
            });
        }
        direccion_mc.checked_add(MCONTEXT64_TAM as u64)
            .ok_or(Error::DesbordeDireccion)?;
        Ok(())
    }

    /// Comprueba ambos destinos antes de escribir; preserva bytes posteriores.
    pub fn a_slices(
        &self, contexto: &mut [u8], maquina: &mut [u8], direccion_mc: u64,
    ) -> Result<(), Error> {
        Self::validar_contexto(&self.contexto, direccion_mc)?;
        longitud(contexto, UCONTEXT64_TAM)?;
        longitud(maquina, MCONTEXT64_TAM)?;
        self.contexto.a_bytes(contexto)?;
        self.maquina.a_bytes(maquina)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn layouts() {
        assert_eq!(size_of::<Ucontext64>(), 56);
        assert_eq!(size_of::<ExceptionState64>(), 16);
        assert_eq!(size_of::<ThreadState64>(), 168);
        assert_eq!(size_of::<FloatState64>(), 524);
        assert_eq!(align_of::<FloatState64>(), 4);
        assert_eq!(size_of::<Mcontext64>(), 712);
        assert_eq!(align_of::<Mcontext64>(), 8);
    }

    #[test]
    fn roundtrip_y_offsets() {
        let mut uc = [0u8; UCONTEXT64_TAM];
        let mut mc = [0u8; MCONTEXT64_TAM];
        for (i, b) in uc.iter_mut().enumerate() { *b = i as u8; }
        for (i, b) in mc.iter_mut().enumerate() { *b = (i % 251) as u8; }
        let u = Ucontext64::desde_bytes(&uc).unwrap();
        assert_eq!(u.onstack, i32::from_le_bytes([0, 1, 2, 3]));
        assert_eq!(u.mcontext, u64::from_le_bytes([48,49,50,51,52,53,54,55]));
        let m = Mcontext64::desde_bytes(&mc).unwrap();
        assert_eq!(m.hilo.rax, u64::from_le_bytes(leer(&mc, 16)));
        assert_eq!(m.hilo.rip, u64::from_le_bytes(leer(&mc, 144)));
        assert_eq!(m.hilo.gs, u64::from_le_bytes(leer(&mc, 176)));
        assert_eq!(m.flotante.bytes[0], mc[184]);
        let mut salida_uc = [0; UCONTEXT64_TAM];
        let mut salida_mc = [0; MCONTEXT64_TAM];
        u.a_bytes(&mut salida_uc).unwrap();
        m.a_bytes(&mut salida_mc).unwrap();
        assert_eq!(uc, salida_uc);
        assert_eq!(mc, salida_mc);
    }

    #[test]
    fn todos_los_truncados_sin_escritura_parcial() {
        macro_rules! comprobar {
            ($tipo:ty, $tam:expr) => {
                for n in 0..$tam {
                    let mut bytes = [0xa5; $tam];
                    assert!(matches!(<$tipo>::desde_bytes(&bytes[..n]), Err(Error::Truncado { .. })));
                    assert!(matches!(<$tipo>::default().a_bytes(&mut bytes[..n]), Err(Error::Truncado { .. })));
                    assert_eq!(bytes, [0xa5; $tam]);
                }
            };
        }
        comprobar!(Ucontext64, UCONTEXT64_TAM);
        comprobar!(ExceptionState64, EXCEPTION64_TAM);
        comprobar!(ThreadState64, THREAD64_TAM);
        comprobar!(FloatState64, FLOAT64_TAM);
        comprobar!(Mcontext64, MCONTEXT64_TAM);
    }

    #[test]
    fn marco_limites_y_token() {
        let mut marco = MarcoNormal64::default();
        marco.contexto.mcsize = MCONTEXT64_TAM as u64;
        marco.contexto.mcontext = 0x1000;
        let mut uc = [0xa5; UCONTEXT64_TAM + 1];
        let mut mc = [0xa5; MCONTEXT64_TAM + 1];
        assert!(marco.a_slices(&mut uc, &mut mc[..711], 0x1000).is_err());
        assert_eq!(uc, [0xa5; UCONTEXT64_TAM + 1]);
        assert_eq!(mc, [0xa5; MCONTEXT64_TAM + 1]);
        marco.a_slices(&mut uc, &mut mc, 0x1000).unwrap();
        assert_eq!(uc[56], 0xa5);
        assert_eq!(mc[712], 0xa5);
        assert_eq!(MarcoNormal64::desde_slices(&uc, &mc, 0x1000), Ok(marco));
        assert!(matches!(MarcoNormal64::desde_slices(&uc, &mc, 0x1001), Err(Error::DireccionContexto { .. })));
        marco.contexto.mcsize = 708;
        assert_eq!(marco.a_slices(&mut uc, &mut mc, 0x1000), Err(Error::TamanoContexto(708)));
        marco.contexto.mcsize = 712;
        marco.contexto.mcontext = u64::MAX - 711;
        assert_eq!(marco.a_slices(&mut uc, &mut mc, u64::MAX - 711), Err(Error::DesbordeDireccion));
        marco.contexto.mcontext = u64::MAX - 712;
        assert!(marco.a_slices(&mut uc, &mut mc, u64::MAX - 712).is_ok());
        assert_eq!(token(0x1234, 0xff), 0x12cb);
        assert_eq!(validar_token(0x1234, 0xff, 0x12cb), Ok(()));
        assert_eq!(validar_token(0x1234, 0xff, 0x12ca), Err(Error::TokenInvalido));
        assert_eq!(token(u64::MAX, u64::MAX), 0);
    }
}
