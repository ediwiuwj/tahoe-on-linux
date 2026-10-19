//! psynch_rw_*: la parte del núcleo de los `pthread_rwlock` de libpthread (llamadas 306-308).
//!
//! libpthread lleva en memoria del proceso tres palabras de secuencia (L: intentos de bloqueo, U: desbloqueos, S: la
//! secuencia del núcleo) y solo entra en el núcleo cuando hay contención: `psynch_rw_rdlock`/`psynch_rw_wrlock` para
//! dormir y `psynch_rw_unlock` para repartir el cerrojo entre los que esperan. El valor que devuelve el núcleo (los bits
//! de L y cuántos lectores entran) lo aplica libpthread tal cual: si no cuadra con su protocolo, aborta con `ud1`.
//! Por eso esto es una traducción fiel, paso a paso, de `kern_synch.c` (libpthread de Apple, APSL): la cola por
//! dirección (`ksyn_wait_queue`), las colas de lectores y escritores ordenadas por secuencia, los desbloqueos que llegan
//! antes que los que esperan («prepost»), el solapamiento de lectores y la reinicialización del cerrojo. Los nombres de
//! las funciones de Apple van en el comentario de cada una para poder compararlas.
//!
//! Lo que no se traduce porque aquí no existe: los turnstiles y la QoS (prioridades del dueño), los cerrojos compartidos
//! entre procesos (`PTHREAD_PROCESS_SHARED`: tahoe-run guarda la tabla en cada proceso; el C lo anota) y los tipos de
//! cola de mutex y variables de condición (los lleva `emu_psynch.c` a su manera).
//!
//! Dormir y despertar los hilos es cosa del C (`emu_psynch.c`), que llama a todo esto con su candado tomado: aquí solo
//! se decide quién espera y con qué valor se despierta a cada uno (la función `despertar` que pasa el C).

/// Cerrojo para lectura o escritura (PTH_RW_TYPE_READ / PTH_RW_TYPE_WRITE de kern_synch.c).
pub const TIPO_LECTURA: u32 = 0x01;
pub const TIPO_ESCRITURA: u32 = 0x04;
const PTH_RW_TYPE_MASK: u32 = 0xff;
const PTH_RWSHFT_TYPE_READ: u32 = 0x0100;
const PTH_RWSHFT_TYPE_WRITE: u32 = 0x0400;
const PTH_RWSHFT_TYPE_MASK: u32 = 0xff00;

// synch_internal.h
const PTHRW_COUNT_SHIFT: u32 = 8;
pub const PTHRW_INC: u32 = 1 << PTHRW_COUNT_SHIFT;
const PTHRW_BIT_MASK: u32 = (1 << PTHRW_COUNT_SHIFT) - 1;
pub const PTHRW_COUNT_MASK: u32 = !PTHRW_BIT_MASK;
const PTHRW_MAX_READERS: u32 = PTHRW_COUNT_MASK;
// palabra L
pub const PTH_RWL_KBIT: u32 = 0x01;
pub const PTH_RWL_EBIT: u32 = 0x02;
pub const PTH_RWL_WBIT: u32 = 0x04;
pub const PTH_RWL_MBIT: u32 = 0x40;
pub const PTH_RWL_IBIT: u32 = 0x80;
const PTHRW_RWL_INIT: u32 = PTH_RWL_IBIT;
// palabra S
const PTH_RWS_SBIT: u32 = 0x01;
const PTH_RWS_WSVBIT: u32 = 0x04;
const PTHRW_RWS_SAVEMASK: u32 = PTH_RWS_WSVBIT;
const PTHRW_RWS_INIT: u32 = PTH_RWS_SBIT;
/// rw_flags: cerrojo compartido entre procesos
pub const PTHREAD_PROCESS_SHARED: u32 = 0x10;
const PTHREAD_PSHARED_FLAGS_MASK: u32 = 0x30;

// kern_synch.c
const KSYN_KWF_INITCLEARED: u16 = 0x1;
const KSYN_KWF_OVERLAP_GUARD: u16 = 0x8;
const KSYN_CLEANUP_DEADLINE: u64 = 10;
const KW_UNLOCK_PREPOST: u32 = 0x01;
const KW_UNLOCK_PREPOST_READLOCK: u32 = 0x08;
const KW_UNLOCK_PREPOST_WRLOCK: u32 = 0x20;
const KSYN_QUEUE_READ: usize = 0;
const KSYN_QUEUE_WRITE: usize = 1;
const KWQ_INTR_NONE: u32 = 0;
const KWQ_INTR_READ: u32 = 0x1;
const KWQ_INTR_WRITE: u32 = 0x2;

// errno de Darwin (los mismos números que en Linux para estos)
pub const ESRCH: i32 = 3;
pub const ENOMEM: i32 = 12;
pub const EBUSY: i32 = 16;
pub const EINVAL: i32 = 22;

/// Colas por proceso y hilos que caben en cada cola. En XNU son dinámicas; aquí, sin memoria dinámica, fijas: una cola
/// solo existe mientras hay contención (o un prepost pendiente) y cada hilo espera en una sola a la vez.
const KWQS: usize = 256;
const COLA: usize = 64;

// ---- comparación de secuencias (synch_internal.h): circulares, módulo 2^32, solo la parte de contador ----

pub fn is_seqlower(x: u32, y: u32) -> bool {
    let (x, y) = (x & PTHRW_COUNT_MASK, y & PTHRW_COUNT_MASK);
    if x < y {
        (y - x) < (PTHRW_MAX_READERS / 2)
    } else {
        (x - y) > (PTHRW_MAX_READERS / 2)
    }
}
pub fn is_seqlower_eq(x: u32, y: u32) -> bool {
    (x & PTHRW_COUNT_MASK) == (y & PTHRW_COUNT_MASK) || is_seqlower(x, y)
}
pub fn is_seqhigher(x: u32, y: u32) -> bool {
    let (x, y) = (x & PTHRW_COUNT_MASK, y & PTHRW_COUNT_MASK);
    if x > y {
        (x - y) < (PTHRW_MAX_READERS / 2)
    } else {
        (y - x) > (PTHRW_MAX_READERS / 2)
    }
}
pub fn is_seqhigher_eq(x: u32, y: u32) -> bool {
    (x & PTHRW_COUNT_MASK) == (y & PTHRW_COUNT_MASK) || is_seqhigher(x, y)
}
fn diff_genseq(x: u32, y: u32) -> u32 {
    let (x, y) = (x & PTHRW_COUNT_MASK, y & PTHRW_COUNT_MASK);
    if x == y {
        0
    } else if x > y {
        x - y
    } else {
        (PTHRW_MAX_READERS - y).wrapping_add(x).wrapping_add(PTHRW_INC)
    }
}
/// Cuántos incrementos hay entre las dos secuencias (L - U: cuántos esperan o tienen el cerrojo).
pub fn find_diff(upto: u32, lowest: u32) -> u32 {
    if upto == lowest {
        return 0;
    }
    let diff = if is_seqlower(upto, lowest) { diff_genseq(lowest, upto) } else { diff_genseq(upto, lowest) };
    diff >> PTHRW_COUNT_SHIFT
}
fn is_rwl_wbit_set(x: u32) -> bool {
    x & PTH_RWL_WBIT != 0
}
fn is_rws_sbit_set(x: u32) -> bool {
    x & PTH_RWS_SBIT != 0
}
fn is_rws_savemask_set(x: u32) -> bool {
    x & PTHRW_RWS_SAVEMASK != 0
}

/// Un hilo que espera (ksyn_waitq_element): su secuencia y el identificador que le da el C para despertarlo.
#[derive(Clone, Copy)]
struct Elem {
    seq: u32,
    espera: u64,
}
const ELEM0: Elem = Elem { seq: 0, espera: 0 };

/// ksyn_queue: los que esperan, ordenados por secuencia (SEQFIT), y la primera y la última.
#[derive(Clone, Copy)]
struct Cola {
    el: [Elem; COLA],
    count: u32,
    firstnum: u32,
    lastnum: u32,
}
const COLA0: Cola = Cola { el: [ELEM0; COLA], count: 0, firstnum: 0, lastnum: 0 };

impl Cola {
    fn items(&self) -> &[Elem] {
        &self.el[..self.count as usize]
    }
    fn meter(&mut self, i: usize, e: Elem) {
        let n = self.count as usize;
        self.el.copy_within(i..n, i + 1);
        self.el[i] = e;
    }
}

/// ksyn_wait_queue de un rwlock.
#[derive(Clone, Copy)]
struct Kwq {
    addr: u64,
    en_hash: bool,  // KSYN_WQ_INHASH: la ranura está en uso para addr
    en_libre: bool, // KSYN_WQ_FLIST: sin usuarios, pendiente de liberar
    sello: u64,     // kw_stamp: cuándo entró en la lista de libres (s)
    iocount: u32,
    tipo: u16, // kw_type: 0 = por inicializar
    kflags: u16,
    inqueue: u32,
    highseq: u32,
    lowseq: u32,
    lword: u32,
    uword: u32,
    sword: u32,
    lastunlockseq: u32,
    lastseqword: u32,
    nextseqword: u32,
    pp_count: u32, // kw_prepost
    pp_lseq: u32,
    pp_sseq: u32,
    in_type: u32, // kw_intr
    in_count: u32,
    in_seq: u32,
    in_returnbits: u32,
    colas: [Cola; 2],
}
const KWQ0: Kwq = Kwq {
    addr: 0,
    en_hash: false,
    en_libre: false,
    sello: 0,
    iocount: 0,
    tipo: 0,
    kflags: 0,
    inqueue: 0,
    highseq: 0,
    lowseq: 0,
    lword: 0,
    uword: 0,
    sword: 0,
    lastunlockseq: 0,
    lastseqword: 0,
    nextseqword: 0,
    pp_count: 0,
    pp_lseq: 0,
    pp_sseq: 0,
    in_type: 0,
    in_count: 0,
    in_seq: 0,
    in_returnbits: 0,
    colas: [COLA0; 2],
};
const KSYN_WQTYPE_RWLOCK: u16 = 0x04;

/// Despertar a un hilo que espera: (identificador, valor que devuelve su llamada). false = ya no esperaba
/// (KERN_NOT_WAITING: en XNU, un hilo interrumpido por una señal).
pub type Despertar<'a> = &'a mut dyn FnMut(u64, u32) -> bool;

/// Resultado de un bloqueo: concedido con este valor, o el hilo debe esperar (en la ranura dada) hasta que lo despierten.
#[derive(Debug, PartialEq, Eq)]
pub enum Bloqueo {
    Concedido(u32),
    Espera(usize),
}

impl Kwq {
    // _kwq_clear_preposted_wakeup / _kwq_mark_preposted_wakeup
    fn clear_preposted_wakeup(&mut self) {
        self.pp_lseq = 0;
        self.pp_sseq = PTHRW_RWS_INIT;
        self.pp_count = 0;
    }
    fn mark_preposted_wakeup(&mut self, count: u32, lseq: u32, sseq: u32) {
        self.pp_count = count;
        self.pp_lseq = lseq;
        self.pp_sseq = sseq;
    }
    // _kwq_clear_interrupted_wakeup / _kwq_mark_interruped_wakeup
    fn clear_interrupted_wakeup(&mut self) {
        self.in_type = KWQ_INTR_NONE;
        self.in_count = 0;
        self.in_seq = 0;
        self.in_returnbits = 0;
    }
    fn mark_interrupted_wakeup(&mut self, tipo: u32, count: u32, lseq: u32, returnbits: u32) {
        self.in_count = count;
        self.in_seq = lseq;
        self.in_returnbits = returnbits;
        self.in_type = tipo;
    }
    // _kwq_is_used
    fn is_used(&self) -> bool {
        self.inqueue != 0 || self.pp_count != 0 || self.in_count != 0
    }
    // CLEAR_REINIT_BITS (rama de rwlock)
    fn clear_reinit_bits(&mut self) {
        if self.tipo == KSYN_WQTYPE_RWLOCK {
            self.nextseqword = PTHRW_RWS_INIT;
            self.kflags &= !KSYN_KWF_OVERLAP_GUARD;
        }
        self.clear_preposted_wakeup();
        self.lastunlockseq = PTHRW_RWL_INIT;
        self.lastseqword = PTHRW_RWS_INIT;
        self.clear_interrupted_wakeup();
        self.lword = 0;
        self.uword = 0;
        self.sword = PTHRW_RWS_INIT;
    }
    // _ksyn_check_init
    fn check_init(&mut self, lgenval: u32) -> bool {
        let res = lgenval & PTHRW_RWL_INIT != 0;
        if res && self.kflags & KSYN_KWF_INITCLEARED == 0 {
            // el primero que ve el cerrojo reiniciado borra los preposts
            self.clear_reinit_bits();
            self.kflags |= KSYN_KWF_INITCLEARED;
        }
        res
    }
    // _kwq_handle_interrupted_wakeup. Como en XNU, se compara el tipo de cerrojo (1 lectura, 4 escritura) con el tipo
    // de interrupción (1 lectura, 2 escritura): para escritores nunca coincide.
    fn handle_interrupted_wakeup(&mut self, tipo: u32, lseq: u32) -> Option<u32> {
        if self.in_count != 0 && self.in_type == tipo && (self.in_seq == 0 || is_seqlower_eq(lseq, self.in_seq)) {
            self.in_count -= 1;
            let r = self.in_returnbits;
            if self.in_returnbits == 0 {
                self.clear_interrupted_wakeup();
            }
            return Some(r);
        }
        None
    }
    // _kwq_handle_overlap: un lector que llega mientras otros lectores tienen el cerrojo y no hay escritores entra ya.
    fn handle_overlap(&mut self, tipo: u32, lgenval: u32, rw_wc: u32) -> Option<u32> {
        if tipo != TIPO_LECTURA {
            return None;
        }
        if self.kflags & KSYN_KWF_OVERLAP_GUARD != 0 && !is_rws_savemask_set(rw_wc) && !is_rwl_wbit_set(lgenval) {
            if is_seqlower_eq(rw_wc, self.nextseqword) || is_seqhigher_eq(self.lastseqword, rw_wc) {
                self.nextseqword = self.nextseqword.wrapping_add(PTHRW_INC);
                return Some(PTHRW_INC | (self.nextseqword & PTHRW_BIT_MASK) | PTH_RWL_MBIT);
            }
        }
        None
    }
    // _kwq_handle_preposted_wakeup: el desbloqueo llegó antes que este hilo; si es el último que faltaba, se reparte.
    fn handle_preposted_wakeup(&mut self, tipo_pp: u32, lseq: u32, despertar: Despertar) -> Option<u32> {
        if self.pp_count == 0 || !is_seqlower_eq(lseq, self.pp_lseq) {
            return None;
        }
        self.pp_count -= 1;
        if self.pp_count > 0 {
            return None;
        }
        let (pp_lseq, pp_sseq) = (self.pp_lseq, self.pp_sseq);
        self.clear_preposted_wakeup();
        self.kflags &= !KSYN_KWF_INITCLEARED;
        let (updatebits, block) = self.handle_unlock(pp_lseq, pp_sseq, tipo_pp | KW_UNLOCK_PREPOST, lseq, despertar);
        if block {
            return None;
        }
        Some(updatebits)
    }

    // update_low_high / find_nextlowseq / find_nexthighseq
    fn update_low_high(&mut self, lockseq: u32) {
        if self.inqueue == 1 {
            self.lowseq = lockseq;
            self.highseq = lockseq;
        } else {
            if is_seqlower(lockseq, self.lowseq) {
                self.lowseq = lockseq;
            }
            if is_seqhigher(lockseq, self.highseq) {
                self.highseq = lockseq;
            }
        }
    }
    fn find_nextlowseq(&self) -> u32 {
        let mut lowest = 0;
        let mut first = true;
        for q in &self.colas {
            if q.count > 0 && (first || is_seqlower(q.firstnum, lowest)) {
                lowest = q.firstnum;
                first = false;
            }
        }
        lowest
    }
    fn find_nexthighseq(&self) -> u32 {
        let mut highest = 0;
        let mut first = true;
        for q in &self.colas {
            if q.count > 0 && (first || is_seqhigher(q.lastnum, highest)) {
                highest = q.lastnum;
                first = false;
            }
        }
        highest
    }

    // ksyn_queue_insert con SEQFIT (los rwlocks siempre insertan por secuencia)
    fn queue_insert(&mut self, kqi: usize, e: Elem) -> Result<(), i32> {
        let lockseq = e.seq;
        let kq = &mut self.colas[kqi];
        if kq.count as usize >= COLA {
            return Err(ENOMEM);
        }
        if kq.count == 0 {
            kq.meter(0, e);
            kq.firstnum = lockseq;
            kq.lastnum = lockseq;
        } else if lockseq == kq.firstnum || lockseq == kq.lastnum {
            // dos con la misma secuencia: en XNU solo se admite para un prepost de un hilo cancelado, que aquí no hay
            return Err(EBUSY);
        } else if is_seqlower(kq.lastnum, lockseq) {
            kq.meter(kq.count as usize, e);
            kq.lastnum = lockseq;
        } else if is_seqlower(lockseq, kq.firstnum) {
            kq.meter(0, e);
            kq.firstnum = lockseq;
        } else {
            match kq.items().iter().position(|q| is_seqhigher(q.seq, lockseq)) {
                Some(i) => kq.meter(i, e),
                None => return Err(ESRCH),
            }
        }
        kq.count += 1;
        self.inqueue += 1;
        self.update_low_high(lockseq);
        Ok(())
    }
    // ksyn_queue_remove_item (del elemento i de la cola kqi)
    fn queue_remove_item(&mut self, kqi: usize, i: usize) -> Elem {
        let kq = &mut self.colas[kqi];
        let n = kq.count as usize;
        let e = kq.el[i];
        kq.el.copy_within(i + 1..n, i);
        kq.count -= 1;
        if kq.count > 0 {
            kq.firstnum = kq.el[0].seq;
            kq.lastnum = kq.el[kq.count as usize - 1].seq;
        } else {
            kq.firstnum = 0;
            kq.lastnum = 0;
        }
        self.inqueue -= 1;
        if self.inqueue > 0 {
            if self.lowseq == e.seq {
                self.lowseq = self.find_nextlowseq();
            }
            if self.highseq == e.seq {
                self.highseq = self.find_nexthighseq();
            }
        } else {
            self.lowseq = 0;
            self.highseq = 0;
        }
        e
    }
    // ksyn_queue_count_tolowest: cuántos esperan en la cola con secuencia hasta upto (incluida)
    fn queue_count_tolowest(&self, kqi: usize, upto: u32) -> u32 {
        let kq = &self.colas[kqi];
        if kq.count == 0 || is_seqhigher(kq.firstnum, upto) {
            return 0;
        }
        if upto == kq.firstnum {
            return 1;
        }
        let mut i = 0;
        for e in kq.items() {
            if is_seqhigher(e.seq, upto) {
                break;
            }
            i += 1;
            if upto == e.seq {
                break;
            }
        }
        i
    }
    // find_seq_till: ¿están ya en las colas los nwaiters hilos hasta upto? (y cuántos hay)
    fn find_seq_till(&self, upto: u32, nwaiters: u32) -> (bool, u32) {
        let mut count = 0;
        for kqi in 0..2 {
            count += self.queue_count_tolowest(kqi, upto);
            if count >= nwaiters {
                break;
            }
        }
        (count != 0 && count >= nwaiters, count)
    }
    // ksyn_signal con kwe = NULL: despierta al primero de la cola
    fn signal_first(&mut self, kqi: usize, updateval: u32, despertar: Despertar) -> bool {
        // XNU entra en pánico si la cola está vacía; aquí no se llega con ella vacía (los llamantes miran count)
        let e = self.queue_remove_item(kqi, 0);
        despertar(e.espera, updateval)
    }

    // kwq_find_rw_lowest: de qué tipo es la secuencia más baja que espera (contando la del hilo del prepost, premgen)
    fn find_rw_lowest(&self, flags: u32, premgen: u32) -> (u32, [u32; 2]) {
        let mut tipo = 0;
        let mut lowest = [0u32; 2];
        let mut numbers = [0u32; 2];
        let mut typenum = [0u32; 2];
        let mut count = 0;
        let r = &self.colas[KSYN_QUEUE_READ];
        if r.count != 0 || flags & KW_UNLOCK_PREPOST_READLOCK != 0 {
            tipo |= PTH_RWSHFT_TYPE_READ;
            let mut kw_fr = premgen;
            if r.count != 0 {
                kw_fr = r.firstnum;
                if flags & KW_UNLOCK_PREPOST_READLOCK != 0 && is_seqlower(premgen, kw_fr) {
                    kw_fr = premgen;
                }
            }
            lowest[KSYN_QUEUE_READ] = kw_fr;
            numbers[count] = kw_fr;
            typenum[count] = TIPO_LECTURA;
            count += 1;
        }
        let w = &self.colas[KSYN_QUEUE_WRITE];
        if w.count != 0 || flags & KW_UNLOCK_PREPOST_WRLOCK != 0 {
            tipo |= PTH_RWSHFT_TYPE_WRITE;
            let mut kw_fwr = premgen;
            if w.count != 0 {
                kw_fwr = w.firstnum;
                if flags & KW_UNLOCK_PREPOST_WRLOCK != 0 && is_seqlower(premgen, kw_fwr) {
                    kw_fwr = premgen;
                }
            }
            lowest[KSYN_QUEUE_WRITE] = kw_fwr;
            numbers[count] = kw_fwr;
            typenum[count] = TIPO_ESCRITURA;
            count += 1;
        }
        let mut low = numbers[0];
        let mut lowtype = typenum[0];
        for i in 1..count {
            if is_seqlower(numbers[i], low) {
                low = numbers[i];
                lowtype = typenum[i];
            }
        }
        (tipo | lowtype, lowest)
    }

    // ksyn_wakeupreaders: despierta lectores hasta el primer escritor (o todos). Devuelve (fallidos, despertados).
    fn wakeupreaders(&mut self, limitread: u32, allreaders: bool, updatebits: u32, despertar: Despertar) -> u32 {
        let mut failed = 0;
        while self.colas[KSYN_QUEUE_READ].count != 0
            && (allreaders || is_seqlower(self.colas[KSYN_QUEUE_READ].firstnum, limitread))
        {
            if !self.signal_first(KSYN_QUEUE_READ, updatebits, despertar) {
                failed += 1;
            }
        }
        failed
    }

    // kwq_handle_unlock: reparte el cerrojo al siguiente grupo. Devuelve (updatebits, block): block = false si el hilo
    // del prepost (premgen) se lo queda y no tiene que dormir.
    fn handle_unlock(&mut self, _mgen: u32, rw_wc: u32, flags: u32, premgen: u32, despertar: Despertar) -> (u32, bool) {
        let mut block = true;
        self.lastseqword = rw_wc;
        self.lastunlockseq = rw_wc & PTHRW_COUNT_MASK;
        self.kflags &= !KSYN_KWF_OVERLAP_GUARD;

        let (rwtype, lowest) = self.find_rw_lowest(flags, premgen);
        let low_writer = lowest[KSYN_QUEUE_WRITE];
        let mut updatebits: u32 = 0;

        match rwtype & PTH_RW_TYPE_MASK {
            TIPO_LECTURA => {
                if rwtype & PTH_RWSHFT_TYPE_MASK != 0 && rwtype & PTH_RWSHFT_TYPE_WRITE != 0 {
                    updatebits |= PTH_RWL_WBIT | PTH_RWL_KBIT;
                }
                let mut limitrdnum = 0;
                let mut allreaders = false;
                let mut numneeded: u32 = 0;
                let mut curthreturns = false;
                if rwtype & PTH_RWSHFT_TYPE_WRITE != 0 {
                    limitrdnum = low_writer;
                    numneeded = self.queue_count_tolowest(KSYN_QUEUE_READ, limitrdnum);
                    if flags & KW_UNLOCK_PREPOST_READLOCK != 0 && is_seqlower(premgen, limitrdnum) {
                        curthreturns = true;
                        numneeded += 1;
                    }
                } else {
                    // sin escritores: solo lectores, entran todos y los que lleguen después se solapan
                    allreaders = true;
                    self.kflags |= KSYN_KWF_OVERLAP_GUARD;
                    numneeded += self.colas[KSYN_QUEUE_READ].count;
                    if flags & KW_UNLOCK_PREPOST_READLOCK != 0 {
                        curthreturns = true;
                        numneeded += 1;
                    }
                }
                updatebits = updatebits.wrapping_add(numneeded << PTHRW_COUNT_SHIFT);
                self.nextseqword = (rw_wc & PTHRW_COUNT_MASK).wrapping_add(updatebits);
                if curthreturns {
                    block = false;
                }
                let nfailed = self.wakeupreaders(limitrdnum, allreaders, updatebits, despertar);
                if nfailed != 0 {
                    self.mark_interrupted_wakeup(KWQ_INTR_READ, nfailed, limitrdnum, updatebits);
                }
                // XNU: panic("writer pending but no writebit set") si quedan escritores sin W; no puede pasar
                debug_assert!(self.colas[KSYN_QUEUE_WRITE].count == 0 || updatebits & PTH_RWL_WBIT != 0);
            }
            TIPO_ESCRITURA => {
                // solo entra un hilo
                updatebits |= PTHRW_INC | PTH_RWL_KBIT | PTH_RWL_EBIT;
                if flags & KW_UNLOCK_PREPOST_WRLOCK != 0 && low_writer == premgen {
                    block = false;
                    if self.colas[KSYN_QUEUE_WRITE].count != 0 {
                        updatebits |= PTH_RWL_WBIT;
                    }
                } else {
                    // no se le da al hilo del prepost: si quedan escritores (o él mismo espera), bit W
                    if self.colas[KSYN_QUEUE_WRITE].count > 1 || flags & KW_UNLOCK_PREPOST_WRLOCK != 0 {
                        updatebits |= PTH_RWL_WBIT;
                    }
                    if !self.signal_first(KSYN_QUEUE_WRITE, updatebits, despertar) {
                        self.mark_interrupted_wakeup(KWQ_INTR_WRITE, 1, low_writer, updatebits);
                    }
                }
                self.nextseqword = (rw_wc & PTHRW_COUNT_MASK).wrapping_add(updatebits);
            }
            // XNU: panic("rwunlock: invalid type for lock grants"); find_rw_lowest siempre da un tipo cuando se llama
            _ => {}
        }
        (updatebits, block)
    }
}

/// La tabla de colas de un proceso (en XNU, la tabla hash del proceso más la lista de libres).
pub struct Tabla {
    kwqs: [Kwq; KWQS],
}

impl Tabla {
    pub const fn nueva() -> Tabla {
        Tabla { kwqs: [KWQ0; KWQS] }
    }

    // psynch_wq_cleanup: libera las colas sin usuarios que llevan KSYN_CLEANUP_DEADLINE s en la lista de libres.
    fn limpiar(&mut self, ahora: u64) {
        for k in self.kwqs.iter_mut() {
            if k.en_libre && !k.is_used() && k.iocount == 0 && ahora.abs_diff(k.sello) >= KSYN_CLEANUP_DEADLINE {
                *k = KWQ0;
            }
        }
    }

    // ksyn_wqfind: la cola de la dirección (creándola si no existe) con una referencia más (iocount).
    fn wqfind(&mut self, addr: u64, mgen: u32, ugen: u32, sgen: u32, flags: u32, ahora: u64) -> Result<usize, i32> {
        if flags & PTHREAD_PSHARED_FLAGS_MASK == PTHREAD_PROCESS_SHARED {
            // compartido entre procesos: XNU lo busca por objeto de memoria; aquí la tabla es del proceso (ver arriba)
        }
        self.limpiar(ahora);
        let i = match self.kwqs.iter().position(|k| k.en_hash && k.addr == addr) {
            Some(i) => {
                self.kwqs[i].en_libre = false;
                i
            }
            None => {
                let i = self.kwqs.iter().position(|k| !k.en_hash).ok_or(ENOMEM)?;
                self.kwqs[i] = KWQ0; // zalloc(Z_ZERO)
                self.kwqs[i].en_hash = true;
                self.kwqs[i].addr = addr;
                i
            }
        };
        let k = &mut self.kwqs[i];
        if k.tipo == 0 {
            k.tipo = KSYN_WQTYPE_RWLOCK;
            k.clear_reinit_bits();
            k.lword = mgen;
            k.uword = ugen;
            k.sword = sgen;
            k.kflags = 0;
        }
        k.iocount += 1;
        Ok(i)
    }

    /// ksyn_wqrelease (qfreenow = 0): suelta la referencia; sin usuarios, la cola pasa a la lista de libres.
    pub fn wqrelease(&mut self, i: usize, ahora: u64) {
        let k = &mut self.kwqs[i];
        k.iocount -= 1;
        if k.iocount == 0 && !k.is_used() {
            k.sello = ahora;
            k.en_libre = true;
        }
    }

    /// __psynch_rw_lock: Concedido(valor) si el hilo vuelve ya; Espera(ranura) si debe dormir hasta que `despertar` lo
    /// llame con su identificador `espera` (después, el C llama a `wqrelease(ranura)`, como psynch_rw_*continue).
    pub fn lock(
        &mut self,
        tipo: u32,
        addr: u64,
        lgenval: u32,
        ugenval: u32,
        rw_wc: u32,
        flags: u32,
        espera: u64,
        ahora: u64,
        despertar: Despertar,
    ) -> Result<Bloqueo, i32> {
        let lockseq = lgenval & PTHRW_COUNT_MASK;
        let (prepost_type, kqi) = if tipo == TIPO_LECTURA {
            (KW_UNLOCK_PREPOST_READLOCK, KSYN_QUEUE_READ)
        } else {
            (KW_UNLOCK_PREPOST_WRLOCK, KSYN_QUEUE_WRITE)
        };
        let i = self.wqfind(addr, lgenval, ugenval, rw_wc, flags, ahora)?;
        let k = &mut self.kwqs[i];
        k.check_init(lgenval);
        let pronto = k
            .handle_interrupted_wakeup(tipo, lockseq)
            // los solapes van antes porque no cuentan para los preposts; usan los bits de lgenval
            .or_else(|| k.handle_overlap(tipo, lgenval, rw_wc))
            .or_else(|| k.handle_preposted_wakeup(prepost_type, lockseq, despertar));
        if let Some(v) = pronto {
            self.wqrelease(i, ahora);
            return Ok(Bloqueo::Concedido(v));
        }
        // ksyn_wait
        match k.queue_insert(kqi, Elem { seq: lockseq, espera }) {
            Ok(()) => Ok(Bloqueo::Espera(i)),
            Err(e) => {
                self.wqrelease(i, ahora);
                Err(e)
            }
        }
    }

    /// _psynch_rw_unlock: el valor que devuelve la llamada (los hilos a los que les toca se despiertan con `despertar`).
    pub fn unlock(
        &mut self,
        addr: u64,
        lgenval: u32,
        ugenval: u32,
        rw_wc: u32,
        flags: u32,
        ahora: u64,
        despertar: Despertar,
    ) -> Result<u32, i32> {
        let curgen = lgenval & PTHRW_COUNT_MASK;
        let i = self.wqfind(addr, lgenval, ugenval, rw_wc, flags, ahora)?;
        let k = &mut self.kwqs[i];
        let mut updatebits = 0;
        let mut clearedkflags = false;
        let isinit = k.check_init(lgenval);

        // un desbloqueo con secuencia más baja que el último es espurio
        if !(k.lastunlockseq != PTHRW_RWL_INIT && is_seqlower(ugenval, k.lastunlockseq)) {
            // si L - U no es el número de los que esperan, falta gente por llegar: prepost
            let diff = find_diff(lgenval, ugenval);
            let (todos, count) = k.find_seq_till(curgen, diff);
            if !todos && (count == 0 || count < diff) {
                // prepost: se guarda si la secuencia es más alta que la del anterior, o si es el primero
                if is_rws_sbit_set(k.pp_sseq) || is_seqhigher_eq(rw_wc, k.pp_sseq) {
                    k.mark_preposted_wakeup(diff - count, curgen, rw_wc);
                    updatebits = lgenval; // que libpthread no haga nada con esto
                }
            } else {
                // están todos: se quita el bit de reinicio y se reparte ya
                if isinit && k.kflags & KSYN_KWF_INITCLEARED != 0 {
                    k.kflags &= !KSYN_KWF_INITCLEARED;
                    clearedkflags = true;
                }
                k.clear_preposted_wakeup();
                updatebits = k.handle_unlock(lgenval, rw_wc, 0, 0, despertar).0;
            }
        }
        // rdar://22244050: si algún despertar falló (el hilo ya volvió por una señal), el reinicio no se borra todavía
        if clearedkflags && k.in_count > 0 {
            k.kflags |= KSYN_KWF_INITCLEARED;
        }
        self.wqrelease(i, ahora);
        Ok(updatebits)
    }
}

// ---- interfaz con C (emu_psynch.c), que llama siempre con su candado tomado ----

static mut TABLA: Tabla = Tabla::nueva();

/// Despertar del C: (contexto, identificador del que espera, valor) -> 1 si esperaba.
pub type DespertarC = unsafe extern "C" fn(ctx: *mut core::ffi::c_void, espera: u64, valor: u32) -> i32;

/// psynch_rw_rdlock / psynch_rw_wrlock. Devuelve 0 y *valor si se concede ya; 1 y *ranura si hay que esperar a que
/// `despertar` llame con `espera` (y luego `nucleo_rw_fin_espera(ranura)`); o un errno.
///
/// # Safety
/// Llamar con el candado de emu_psynch.c tomado; `valor` y `ranura` válidos.
#[no_mangle]
pub unsafe extern "C" fn nucleo_rw_lock(
    tipo: u32,
    addr: u64,
    l: u32,
    u: u32,
    s: u32,
    flags: u32,
    espera: u64,
    ahora: u64,
    despertar: DespertarC,
    ctx: *mut core::ffi::c_void,
    valor: *mut u32,
    ranura: *mut u32,
) -> i32 {
    // SAFETY: el candado del C serializa todo acceso a TABLA.
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLA) };
    let mut d = |e: u64, v: u32| unsafe { despertar(ctx, e, v) != 0 };
    match t.lock(tipo, addr, l, u, s, flags, espera, ahora, &mut d) {
        Ok(Bloqueo::Concedido(v)) => {
            unsafe { *valor = v };
            0
        }
        Ok(Bloqueo::Espera(i)) => {
            unsafe { *ranura = i as u32 };
            1
        }
        Err(e) => e,
    }
}

/// El hilo que esperaba ya fue despertado: suelta su referencia (psynch_rw_rdcontinue / wrcontinue).
///
/// # Safety
/// Llamar con el candado de emu_psynch.c tomado y una ranura devuelta por nucleo_rw_lock.
#[no_mangle]
pub unsafe extern "C" fn nucleo_rw_fin_espera(ranura: u32, ahora: u64) {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLA) };
    if (ranura as usize) < KWQS {
        t.wqrelease(ranura as usize, ahora);
    }
}

/// psynch_rw_unlock: 0 y *valor, o un errno.
///
/// # Safety
/// Llamar con el candado de emu_psynch.c tomado; `valor` válido.
#[no_mangle]
pub unsafe extern "C" fn nucleo_rw_unlock(
    addr: u64,
    l: u32,
    u: u32,
    s: u32,
    flags: u32,
    ahora: u64,
    despertar: DespertarC,
    ctx: *mut core::ffi::c_void,
    valor: *mut u32,
) -> i32 {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLA) };
    let mut d = |e: u64, v: u32| unsafe { despertar(ctx, e, v) != 0 };
    match t.unlock(addr, l, u, s, flags, ahora, &mut d) {
        Ok(v) => {
            unsafe { *valor = v };
            0
        }
        Err(e) => e,
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;
    extern crate std;
    use std::boxed::Box;
    use std::vec::Vec;

    const A: u64 = 0x7000_1000;
    const KE: u32 = PTH_RWL_KBIT | PTH_RWL_EBIT;
    // Como en libpthread, el desbloqueo entra en el núcleo cuando U == S, y S lleva el bit I (0x2) en ese momento.

    fn tabla() -> Box<Tabla> {
        Box::new(Tabla::nueva())
    }
    /// Ejecuta con un despertar que apunta a quién se despierta y con qué valor.
    fn con<R>(f: impl FnOnce(Despertar) -> R) -> (R, Vec<(u64, u32)>) {
        let mut v = Vec::new();
        let mut d = |e: u64, x: u32| {
            v.push((e, x));
            true
        };
        let r = f(&mut d);
        (r, v)
    }

    #[test]
    fn secuencias_circulares() {
        assert!(is_seqlower(0x100, 0x200));
        assert!(!is_seqlower(0x200, 0x100));
        assert!(is_seqlower(0xffff_ff00, 0x100)); // da la vuelta
        assert!(is_seqhigher(0x100, 0xffff_ff00));
        assert!(is_seqlower_eq(0x1ff, 0x100)); // los bits bajos no cuentan
        assert_eq!(find_diff(0x300, 0x100), 2);
        assert_eq!(find_diff(0x100, 0xffff_ff00), 2);
        assert_eq!(find_diff(0x100, 0x100), 0);
    }

    #[test]
    fn escritor_espera_y_el_desbloqueo_se_lo_da() {
        let mut t = tabla();
        // un escritor tiene el cerrojo (L=0x100); otro escritor llega con L=0x200
        let (r, w) = con(|d| t.lock(TIPO_ESCRITURA, A, 0x200 | KE | PTH_RWL_WBIT, 0, 0x100, 0, 7, 0, d));
        assert_eq!(r, Ok(Bloqueo::Espera(0)));
        assert!(w.is_empty());
        // el dueño suelta: L=0x200, U=0x100 -> falta uno, que ya está en la cola
        let (r, w) = con(|d| t.unlock(A, 0x200 | KE | PTH_RWL_WBIT, 0x100, 0x102, 0, 0, d));
        assert_eq!(r, Ok(PTHRW_INC | KE));
        assert_eq!(w, [(7, PTHRW_INC | KE)]);
        t.wqrelease(0, 0);
        assert_eq!(t.kwqs[0].iocount, 0);
        assert!(t.kwqs[0].en_libre);
    }

    #[test]
    fn desbloqueo_antes_que_el_escritor_prepost() {
        let mut t = tabla();
        // el desbloqueo llega primero: nadie en la cola -> prepost de 1, devuelve L
        let (r, w) = con(|d| t.unlock(A, 0x200 | KE, 0x100, 0x102, 0, 0, d));
        assert_eq!(r, Ok(0x200 | KE));
        assert!(w.is_empty());
        assert_eq!(t.kwqs[0].pp_count, 1);
        // llega el escritor: se lo queda sin dormir
        let (r, _) = con(|d| t.lock(TIPO_ESCRITURA, A, 0x200 | KE, 0, 0x100, 0, 7, 0, d));
        assert_eq!(r, Ok(Bloqueo::Concedido(PTHRW_INC | KE)));
        assert!(!t.kwqs[0].is_used());
    }

    #[test]
    fn lectores_sin_escritores_entran_todos_y_se_solapan() {
        let mut t = tabla();
        let w0 = PTH_RWL_WBIT | KE;
        assert_eq!(con(|d| t.lock(TIPO_LECTURA, A, 0x200 | w0, 0, 0x100, 0, 1, 0, d)).0, Ok(Bloqueo::Espera(0)));
        assert_eq!(con(|d| t.lock(TIPO_LECTURA, A, 0x300 | w0, 0, 0x100, 0, 2, 0, d)).0, Ok(Bloqueo::Espera(0)));
        let (r, w) = con(|d| t.unlock(A, 0x300 | w0, 0x100, 0x106, 0, 0, d));
        assert_eq!(r, Ok(0x200));
        assert_eq!(w, [(1, 0x200), (2, 0x200)]);
        t.wqrelease(0, 0);
        t.wqrelease(0, 0);
        // otro lector que entró en el núcleo antes de ver el cambio: se solapa (entra ya, con el bit M)
        let (r, _) = con(|d| t.lock(TIPO_LECTURA, A, 0x400 | KE, 0, 0x100, 0, 3, 0, d));
        assert_eq!(r, Ok(Bloqueo::Concedido(PTHRW_INC | PTH_RWL_MBIT)));
    }

    #[test]
    fn lector_antes_que_escritor_solo_entra_el_lector() {
        let mut t = tabla();
        let w0 = PTH_RWL_WBIT | KE;
        assert_eq!(con(|d| t.lock(TIPO_LECTURA, A, 0x200 | w0, 0, 0x100, 0, 1, 0, d)).0, Ok(Bloqueo::Espera(0)));
        assert_eq!(con(|d| t.lock(TIPO_ESCRITURA, A, 0x300 | w0, 0, 0x100, 0, 2, 0, d)).0, Ok(Bloqueo::Espera(0)));
        let (r, w) = con(|d| t.unlock(A, 0x300 | w0, 0x100, 0x106, 0, 0, d));
        // un lector entra; el escritor sigue esperando (W y K puestos)
        let v = PTHRW_INC | PTH_RWL_WBIT | PTH_RWL_KBIT;
        assert_eq!(r, Ok(v));
        assert_eq!(w, [(1, v)]);
        assert_eq!(t.kwqs[0].colas[KSYN_QUEUE_WRITE].count, 1);
        // el lector suelta (L=0x300, U=0x200): ahora le toca al escritor
        let (r, w) = con(|d| t.unlock(A, 0x300 | w0, 0x200, 0x202, 0, 0, d));
        assert_eq!(r, Ok(PTHRW_INC | KE));
        assert_eq!(w, [(2, PTHRW_INC | KE)]);
    }

    #[test]
    fn la_cola_se_ordena_por_secuencia() {
        let mut t = tabla();
        for (seq, id) in [(0x500, 5), (0x300, 3), (0x400, 4), (0x200, 2)] {
            let r = con(|d| t.lock(TIPO_ESCRITURA, A, seq | KE, 0, 0x100, 0, id, 0, d)).0;
            assert_eq!(r, Ok(Bloqueo::Espera(0)));
        }
        let q = &t.kwqs[0].colas[KSYN_QUEUE_WRITE];
        assert_eq!(q.items().iter().map(|e| e.espera).collect::<Vec<_>>(), [2, 3, 4, 5]);
        assert_eq!((q.firstnum, q.lastnum), (0x200, 0x500));
        assert_eq!((t.kwqs[0].lowseq, t.kwqs[0].highseq), (0x200, 0x500));
        // misma secuencia dos veces: EBUSY, como XNU
        let r = con(|d| t.lock(TIPO_ESCRITURA, A, 0x200 | KE, 0, 0x100, 0, 9, 0, d)).0;
        assert_eq!(r, Err(EBUSY));
        // el primero sale con W (quedan más escritores)
        let (_, w) = con(|d| t.unlock(A, 0x500 | KE, 0x100, 0x102, 0, 0, d));
        assert_eq!(w, [(2, PTHRW_INC | KE | PTH_RWL_WBIT)]);
        assert_eq!(t.kwqs[0].lowseq, 0x300);
    }

    #[test]
    fn desbloqueo_espurio_se_ignora() {
        let mut t = tabla();
        assert!(con(|d| t.lock(TIPO_ESCRITURA, A, 0x300 | KE, 0, 0x100, 0, 1, 0, d)).0.is_ok());
        assert!(con(|d| t.unlock(A, 0x300 | KE, 0x200, 0x202, 0, 0, d)).0.is_ok()); // lastunlockseq = 0x200
        let (r, w) = con(|d| t.unlock(A, 0x300 | KE, 0x100, 0x102, 0, 0, d));
        assert_eq!(r, Ok(0));
        assert!(w.is_empty());
    }

    #[test]
    fn colas_sin_uso_se_liberan_a_los_10_s() {
        let mut t = tabla();
        assert!(con(|d| t.unlock(A, 0x100, 0x100, 0x100, 0, 100, d)).0.is_ok()); // diff 0: nadie
        assert!(t.kwqs[0].en_libre && t.kwqs[0].en_hash);
        t.limpiar(105);
        assert!(t.kwqs[0].en_hash); // todavía no
        t.limpiar(110);
        assert!(!t.kwqs[0].en_hash);
        // un prepost pendiente la mantiene viva
        assert!(con(|d| t.unlock(A, 0x200, 0x100, 0x100, 0, 200, d)).0.is_ok());
        t.limpiar(1000);
        assert!(t.kwqs[0].en_hash && t.kwqs[0].pp_count == 1);
    }

    #[test]
    fn reinicio_del_cerrojo_borra_preposts() {
        let mut t = tabla();
        assert!(con(|d| t.unlock(A, 0x300, 0x100, 0x100, 0, 0, d)).0.is_ok()); // prepost de 2
        assert_eq!(t.kwqs[0].pp_count, 2);
        // pthread_rwlock_init otra vez: L lleva el bit I
        let r = con(|d| t.lock(TIPO_ESCRITURA, A, 0x100 | PTH_RWL_IBIT | KE, 0, 0, 0, 1, 0, d)).0;
        assert_eq!(r, Ok(Bloqueo::Espera(0)));
        assert_eq!(t.kwqs[0].pp_count, 0);
        assert_eq!(t.kwqs[0].kflags & KSYN_KWF_INITCLEARED, KSYN_KWF_INITCLEARED);
    }

    #[test]
    fn varias_direcciones_y_tabla_llena() {
        let mut t = tabla();
        for k in 0..KWQS as u64 {
            let r = con(|d| t.lock(TIPO_ESCRITURA, 0x1000 + k * 64, 0x200 | KE, 0, 0x100, 0, k, 0, d)).0;
            assert_eq!(r, Ok(Bloqueo::Espera(k as usize)));
        }
        let r = con(|d| t.lock(TIPO_ESCRITURA, 0x9999_0000, 0x200 | KE, 0, 0x100, 0, 1, 0, d)).0;
        assert_eq!(r, Err(ENOMEM));
    }
}
