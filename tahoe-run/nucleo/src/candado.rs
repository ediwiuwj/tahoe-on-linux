//! Candado y esperas de la región compartida de puertos Mach (`emu_port.c`), entre procesos.
//!
//! Antes era un `pthread_mutex_t` robusto y compartido de glibc con `pthread_cond_t` compartidos. glibc identifica al
//! dueño por el TID que guarda en su descriptor de hilo, y tahoe-run hace `fork` con la llamada al sistema directa (sin
//! pasar por glibc): en el hijo, glibc sigue creyendo que su TID es el del hilo del padre. Un hijo que pedía el candado
//! mientras el padre lo tenía se creía ya su dueño, entraba sin esperar y al soltarlo liberaba el del padre. Así se
//! rompía la exclusión y se perdían despertares: un `bash` se quedó 30 minutos dormido con el candado libre.
//!
//! Aquí el dueño es el TID real (`gettid` en cada toma, que no depende de glibc), los futex son compartidos (sin
//! `FUTEX_PRIVATE_FLAG`: la palabra está en memoria compartida entre procesos) y toda espera tiene plazo, así que un
//! despertar perdido cuesta como mucho un plazo y nunca un cuelgue:
//! - candado (`u32`): 0 libre; si no, TID del dueño, con el bit 31 si alguien espera. Si la espera vence y el dueño ya
//!   no existe (murió con el candado tomado), se toma igualmente y se devuelve `EOWNERDEAD`, como el robusto de glibc;
//! - espera (`u32`): un número de secuencia. Quien espera lo lee con el candado tomado, suelta el candado y duerme
//!   mientras no cambie; quien avisa lo incrementa (con el candado tomado) y despierta a todos. Los despertares pueden
//!   ser espurios: quien espera vuelve a comprobar su condición, como con `pthread_cond_wait`.

use core::ffi::{c_int, c_long};
use core::sync::atomic::{AtomicU32, Ordering};

const ESPERAS: u32 = 1 << 31;
const TID: u32 = !ESPERAS;

pub const ETIMEDOUT: i32 = 110;
pub const EOWNERDEAD: i32 = 130;
const ESRCH: c_int = 3;

// x86_64
const SYS_FUTEX: c_long = 202;
const SYS_GETTID: c_long = 186;
const SYS_KILL: c_long = 62;
const FUTEX_WAKE: c_int = 1;
const FUTEX_WAIT_BITSET: c_int = 9;
const FUTEX_BITSET_MATCH_ANY: u32 = 0xffff_ffff;
const CLOCK_MONOTONIC: c_int = 1;

/// Cuánto se duerme como mucho de una vez: tras el plazo se mira si el dueño del candado sigue vivo (y una espera sin
/// plazo vuelve para que quien espera compruebe su condición).
const PLAZO_CANDADO_NS: i64 = 200_000_000;
const PLAZO_ESPERA_NS: i64 = 1_000_000_000;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

extern "C" {
    fn syscall(n: c_long, ...) -> c_long;
    fn __errno_location() -> *mut c_int;
    fn clock_gettime(clk: c_int, ts: *mut Timespec) -> c_int;
}

fn errno() -> c_int {
    // SAFETY: __errno_location devuelve la dirección del errno del hilo.
    unsafe { *__errno_location() }
}
fn gettid() -> u32 {
    // SAFETY: gettid no tiene argumentos y no falla.
    unsafe { syscall(SYS_GETTID) as u32 }
}
fn ahora() -> Timespec {
    let mut t = Timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: t es un Timespec válido.
    unsafe { clock_gettime(CLOCK_MONOTONIC, &mut t) };
    t
}
fn sumar(t: Timespec, ns: i64) -> Timespec {
    let n = t.tv_nsec + ns;
    Timespec { tv_sec: t.tv_sec + n / 1_000_000_000, tv_nsec: n % 1_000_000_000 }
}
fn antes(a: &Timespec, b: &Timespec) -> bool {
    (a.tv_sec, a.tv_nsec) < (b.tv_sec, b.tv_nsec)
}
/// Duerme mientras `*p == val`, hasta el instante `hasta` (CLOCK_MONOTONIC). 0, ETIMEDOUT o el errno (EAGAIN si ya no
/// valía `val`, EINTR).
fn futex_esperar(p: &AtomicU32, val: u32, hasta: &Timespec) -> c_int {
    // SAFETY: p es una palabra válida; FUTEX_WAIT_BITSET toma un plazo absoluto de CLOCK_MONOTONIC.
    let r = unsafe {
        syscall(SYS_FUTEX, p.as_ptr(), FUTEX_WAIT_BITSET, val, hasta as *const Timespec, 0usize, FUTEX_BITSET_MATCH_ANY)
    };
    if r == 0 {
        0
    } else {
        errno()
    }
}
fn futex_despertar(p: &AtomicU32, n: c_int) {
    // SAFETY: p es una palabra válida.
    unsafe { syscall(SYS_FUTEX, p.as_ptr(), FUTEX_WAKE, n) };
}
/// ¿Existe aún el hilo `tid`? (kill con señal 0: ESRCH si no; cualquier otra respuesta cuenta como vivo).
fn vivo(tid: u32) -> bool {
    // SAFETY: la señal 0 no envía nada.
    let r = unsafe { syscall(SYS_KILL, tid as c_int, 0) };
    if r != 0 && errno() == ESRCH {
        return false;
    }
    // Un proceso muerto que su padre aún no ha recogido (zombi) sigue respondiendo a kill(…, 0): se mira su estado en
    // /proc/<tid>/stat («pid (nombre) Z …»). Sin esto, un dueño que murió con el candado tomado no se recuperaba nunca y
    // todo el arranque se quedaba esperando.
    !zombi(tid)
}
extern "C" {
    fn open(path: *const core::ffi::c_char, flags: c_int, ...) -> c_int;
    fn read(fd: c_int, buf: *mut u8, n: usize) -> isize;
    fn close(fd: c_int) -> c_int;
}
fn zombi(tid: u32) -> bool {
    let mut ruta = *b"/proc/0000000000/stat\0";
    // dígitos del tid en su sitio, y el resto de la ruta detrás
    let mut dig = [0u8; 10];
    let (mut v, mut k) = (tid, dig.len());
    loop {
        k -= 1;
        dig[k] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let d = &dig[k..];
    let mut n = 6;
    ruta[n..n + d.len()].copy_from_slice(d);
    n += d.len();
    ruta[n..n + 6].copy_from_slice(b"/stat\0");
    let mut buf = [0u8; 256];
    // SAFETY: ruta termina en NUL; buf es nuestro.
    let fd = unsafe { open(ruta.as_ptr().cast(), 0) };
    if fd < 0 {
        return true; // ya no existe
    }
    let got = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
    unsafe { close(fd) };
    if got <= 0 {
        return true;
    }
    let s = &buf[..got as usize];
    match s.iter().rposition(|&c| c == b')') {
        Some(i) if i + 2 < s.len() => matches!(s[i + 2], b'Z' | b'X' | b'x'),
        _ => false,
    }
}
/// Toma el candado. 0, o EOWNERDEAD si el dueño anterior había muerto con él tomado (el estado que protege puede estar a
/// medias).
pub fn tomar(c: &AtomicU32) -> i32 {
    let yo = gettid();
    if c.compare_exchange(0, yo, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        return 0;
    }
    loop {
        let v = c.load(Ordering::Relaxed);
        if v == 0 {
            // tras haber esperado se toma con el bit de esperas: puede quedar alguien dormido detrás
            if c.compare_exchange(0, yo | ESPERAS, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return 0;
            }
            continue;
        }
        if v & TID == yo {
            // imposible si todo va bien (nadie toma dos veces): mejor seguir que quedarse colgado para siempre
            if c.compare_exchange(v, yo | ESPERAS, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return EOWNERDEAD;
            }
            continue;
        }
        if v & ESPERAS == 0 && c.compare_exchange(v, v | ESPERAS, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            continue;
        }
        let hasta = sumar(ahora(), PLAZO_CANDADO_NS);
        if futex_esperar(c, v | ESPERAS, &hasta) == ETIMEDOUT {
            let cur = c.load(Ordering::Relaxed);
            let dueno = cur & TID;
            if dueno != 0
                && !vivo(dueno)
                && c.compare_exchange(cur, yo | ESPERAS, Ordering::Acquire, Ordering::Relaxed).is_ok()
            {
                return EOWNERDEAD;
            }
        }
    }
}

/// Suelta el candado y, si alguien esperaba, despierta a uno.
pub fn soltar(c: &AtomicU32) {
    if c.swap(0, Ordering::Release) & ESPERAS != 0 {
        futex_despertar(c, 1);
    }
}

/// Con el candado tomado: lo suelta, duerme hasta un aviso en `e` (o el plazo absoluto `hasta`, CLOCK_MONOTONIC) y lo
/// vuelve a tomar. 0 (aviso o despertar espurio), ETIMEDOUT (venció el plazo) o EOWNERDEAD (al volver a tomarlo).
pub fn esperar(e: &AtomicU32, c: &AtomicU32, hasta: Option<&Timespec>) -> i32 {
    let s = e.load(Ordering::Relaxed);
    soltar(c);
    let tope = sumar(ahora(), PLAZO_ESPERA_NS);
    let limite = match hasta {
        Some(h) if antes(h, &tope) => *h,
        _ => tope,
    };
    let r = futex_esperar(e, s, &limite);
    let vencido = r == ETIMEDOUT && hasta.is_some_and(|h| !antes(&ahora(), h));
    let t = tomar(c);
    if vencido {
        ETIMEDOUT
    } else if t != 0 {
        t
    } else {
        0
    }
}

/// Con el candado tomado: avisa a todos los que esperan en `e`.
pub fn avisar(e: &AtomicU32) {
    e.fetch_add(1, Ordering::Release);
    futex_despertar(e, c_int::MAX);
}

// ---- interfaz con C (emu_port.c) ----

/// # Safety
/// `c` apunta a una palabra de 4 bytes alineada (en la región compartida).
#[no_mangle]
pub unsafe extern "C" fn nucleo_candado_tomar(c: *mut u32) -> i32 {
    tomar(unsafe { AtomicU32::from_ptr(c) })
}
/// # Safety
/// Como `nucleo_candado_tomar`, y el candado lo tiene este hilo.
#[no_mangle]
pub unsafe extern "C" fn nucleo_candado_soltar(c: *mut u32) {
    soltar(unsafe { AtomicU32::from_ptr(c) })
}
/// # Safety
/// `e` y `c` palabras válidas; el candado lo tiene este hilo; `hasta` nulo (sin plazo) o un Timespec válido.
#[no_mangle]
pub unsafe extern "C" fn nucleo_esperar(e: *mut u32, c: *mut u32, hasta: *const Timespec) -> i32 {
    let h = if hasta.is_null() { None } else { Some(unsafe { &*hasta }) };
    esperar(unsafe { AtomicU32::from_ptr(e) }, unsafe { AtomicU32::from_ptr(c) }, h)
}
/// # Safety
/// `e` es una palabra válida.
#[no_mangle]
pub unsafe extern "C" fn nucleo_avisar(e: *mut u32) {
    avisar(unsafe { AtomicU32::from_ptr(e) })
}

#[cfg(test)]
mod pruebas {
    use super::*;
    extern crate std;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn exclusion_entre_hilos() {
        struct Comp {
            c: AtomicU32,
            n: core::cell::UnsafeCell<u64>,
        }
        unsafe impl Sync for Comp {}
        let s = Arc::new(Comp { c: AtomicU32::new(0), n: core::cell::UnsafeCell::new(0) });
        let hs: std::vec::Vec<_> = (0..8)
            .map(|_| {
                let s = s.clone();
                thread::spawn(move || {
                    for _ in 0..20000 {
                        assert_eq!(tomar(&s.c), 0);
                        unsafe { *s.n.get() += 1 };
                        soltar(&s.c);
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(unsafe { *s.n.get() }, 160000);
        assert_eq!(s.c.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn dueno_muerto_se_recupera() {
        let c = Arc::new(AtomicU32::new(0));
        let c2 = c.clone();
        // un hilo toma el candado y termina sin soltarlo
        thread::spawn(move || assert_eq!(tomar(&c2), 0)).join().unwrap();
        assert_ne!(c.load(Ordering::Relaxed), 0);
        let t0 = Instant::now();
        assert_eq!(tomar(&c), EOWNERDEAD);
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert_eq!(c.load(Ordering::Relaxed) & TID, gettid());
        soltar(&c);
    }

    #[test]
    fn dueno_zombi_se_recupera() {
        extern "C" {
            fn fork() -> c_int;
            fn _exit(c: c_int) -> !;
            fn waitpid(p: c_int, st: *mut c_int, o: c_int) -> c_int;
        }
        // un hijo que termina y que no se recoge todavía (zombi) es el dueño del candado
        let hijo = unsafe { fork() };
        if hijo == 0 {
            unsafe { _exit(0) };
        }
        thread::sleep(Duration::from_millis(100));
        let c = AtomicU32::new(hijo as u32);
        assert!(!vivo(hijo as u32));
        let t0 = Instant::now();
        assert_eq!(tomar(&c), EOWNERDEAD);
        assert!(t0.elapsed() < Duration::from_secs(2));
        soltar(&c);
        let mut st = 0;
        unsafe { waitpid(hijo, &mut st, 0) };
    }

    #[test]
    fn espera_con_aviso_y_con_plazo() {
        let c = Arc::new(AtomicU32::new(0));
        let e = Arc::new(AtomicU32::new(0));
        let listo = Arc::new(AtomicU32::new(0));
        let (c2, e2, l2) = (c.clone(), e.clone(), listo.clone());
        let h = thread::spawn(move || {
            tomar(&c2);
            while l2.load(Ordering::Relaxed) == 0 {
                esperar(&e2, &c2, None);
            }
            soltar(&c2);
        });
        thread::sleep(Duration::from_millis(50));
        tomar(&c);
        listo.store(1, Ordering::Relaxed);
        avisar(&e);
        soltar(&c);
        h.join().unwrap();
        // con plazo y sin aviso: ETIMEDOUT, y vuelve con el candado tomado
        tomar(&c);
        let hasta = sumar(ahora(), 100_000_000);
        let t0 = Instant::now();
        let mut r = 0;
        while r == 0 {
            r = esperar(&e, &c, Some(&hasta));
        }
        assert_eq!(r, ETIMEDOUT);
        assert!(t0.elapsed() >= Duration::from_millis(90));
        assert_eq!(c.load(Ordering::Relaxed) & TID, gettid());
        soltar(&c);
    }

    #[test]
    fn exclusion_entre_procesos() {
        extern "C" {
            fn mmap(a: *mut u8, l: usize, p: c_int, f: c_int, fd: c_int, o: i64) -> *mut u8;
            fn fork() -> c_int;
            fn waitpid(p: c_int, st: *mut c_int, o: c_int) -> c_int;
            fn _exit(c: c_int) -> !;
        }
        // memoria compartida: candado + contador
        let m = unsafe { mmap(core::ptr::null_mut(), 4096, 3, 0x01 | 0x20, -1, 0) };
        assert!(!m.is_null());
        let c = unsafe { AtomicU32::from_ptr(m as *mut u32) };
        let n = unsafe { &mut *(m.add(8) as *mut u64) };
        let hijo = unsafe { fork() };
        let vueltas = 50000;
        for _ in 0..vueltas {
            tomar(c);
            let v = unsafe { core::ptr::read_volatile(n) };
            unsafe { core::ptr::write_volatile(n, v + 1) };
            soltar(c);
        }
        if hijo == 0 {
            unsafe { _exit(0) };
        }
        let mut st = 0;
        unsafe { waitpid(hijo, &mut st, 0) };
        assert_eq!(*n, 2 * vueltas);
    }
}
