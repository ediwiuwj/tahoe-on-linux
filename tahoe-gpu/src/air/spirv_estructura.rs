//! Control de flujo estructurado: el CFG de LLVM (reducible) pasa a bucles y selecciones de SPIR-V con el algoritmo
//! de «Beyond Relooper» (N. Ramsey, ICFP 2022), en vez de la máquina de estados (bucle + switch sobre el bloque).
//!
//! Con la máquina de estados los hilos de un grupo que llegan a una barrera por caminos de distinta longitud no
//! reconvergen y la GPU se cuelga (Intel cuenta las barreras por hilo de hardware, no por carril); los shaders grandes
//! además hacen fallar al compilador de Mesa. Aquí los hilos reconvergen en los bloques de fusión.
//!
//! Esquema: árbol de dominadores; un nodo con dos o más aristas hacia delante es de «fusión» y se emite tras un
//! bloque que lo envuelve (las ramas que llegan a él salen de ese bloque); una cabecera de bucle abre un bucle (las
//! aristas hacia atrás son «continue»). Los bloques se emiten como bucles de una vuelta. SPIR-V solo deja salir del
//! bucle más interno, así que una salida a varios niveles guarda su destino en una variable y va saliendo bucle a
//! bucle; en la fusión de cada uno se mira si ha llegado.

use super::*;

/// El CFG vivo (aristas que SCCP no descarta) con orden, dominadores y bucles.
pub(super) struct Cfg {
    suc: Vec<Vec<usize>>,
    /// posición en el postorden inverso (usize::MAX: inalcanzable)
    rpo: Vec<usize>,
    /// hijos en el árbol de dominadores, en postorden inverso
    hijos: Vec<Vec<usize>>,
    cabecera: Vec<bool>,
    fusion: Vec<bool>,
}

impl Cfg {
    /// Analiza la función; error si el CFG no es reducible (entonces se usa la máquina de estados).
    pub(super) fn nuevo(f: &Function, an: &sccp::Analisis) -> Res<Cfg> {
        let n = f.blocks.len();
        let mut suc = vec![Vec::new(); n];
        for &b in &an.orden {
            suc[b] = sucesores_vivos(f, an, b);
        }
        let mut rpo = vec![usize::MAX; n];
        for (k, &b) in an.orden.iter().enumerate() {
            rpo[b] = k;
        }
        let mut pred = vec![Vec::new(); n];
        for &b in &an.orden {
            for &s in &suc[b] {
                pred[s].push(b);
            }
        }
        // dominadores (Cooper, Harvey y Kennedy)
        let entrada = *an.orden.first().ok_or("función sin bloques")?;
        let mut idom = vec![usize::MAX; n];
        idom[entrada] = entrada;
        let mut cambio = true;
        while cambio {
            cambio = false;
            for &b in an.orden.iter().skip(1) {
                let mut nuevo = usize::MAX;
                for &p in &pred[b] {
                    if idom[p] == usize::MAX {
                        continue;
                    }
                    nuevo = if nuevo == usize::MAX { p } else { interseccion(&idom, &rpo, p, nuevo) };
                }
                if nuevo != usize::MAX && idom[b] != nuevo {
                    idom[b] = nuevo;
                    cambio = true;
                }
            }
        }
        let domina = |a: usize, mut b: usize| -> bool {
            loop {
                if a == b {
                    return true;
                }
                if b == entrada || idom[b] == usize::MAX {
                    return false;
                }
                b = idom[b];
            }
        };
        let mut hijos = vec![Vec::new(); n];
        for &b in an.orden.iter().skip(1) {
            hijos[idom[b]].push(b);
        }
        let mut cabecera = vec![false; n];
        let mut adelante = vec![0usize; n];
        for &b in &an.orden {
            for &s in &suc[b] {
                if rpo[s] <= rpo[b] {
                    if !domina(s, b) {
                        return Err("control de flujo irreducible".into());
                    }
                    cabecera[s] = true;
                } else {
                    adelante[s] += 1;
                }
            }
        }
        let fusion = adelante.iter().map(|&k| k >= 2).collect();
        Ok(Cfg { suc, rpo, hijos, cabecera, fusion })
    }
}

fn interseccion(idom: &[usize], rpo: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while rpo[a] > rpo[b] {
            a = idom[a];
        }
        while rpo[b] > rpo[a] {
            b = idom[b];
        }
    }
    a
}

/// Sucesores por las aristas vivas (las condiciones constantes solo llevan a un lado).
fn sucesores_vivos(f: &Function, an: &sccp::Analisis, b: usize) -> Vec<usize> {
    let cte = |v: ValueId| an.constantes.get(v).cloned().flatten().and_then(|x| x.int());
    let mut r = match f.blocks[b].insts.last().map(|i| &i.inst) {
        Some(Inst::Br(t)) => vec![*t],
        Some(Inst::CondBr { c, t, f: fl }) => match cte(*c) {
            Some(1) => vec![*t],
            Some(0) => vec![*fl],
            _ => vec![*t, *fl],
        },
        Some(Inst::Switch { v, default, cases }) => match cte(*v) {
            Some(x) => vec![cases.iter().find(|(c, _)| cte(*c) == Some(x)).map(|(_, b)| *b).unwrap_or(*default)],
            None => {
                let mut d = vec![*default];
                d.extend(cases.iter().map(|(_, b)| *b));
                d
            }
        },
        _ => Vec::new(),
    };
    r.retain(|&s| an.alcanzables.get(s).copied().unwrap_or(false));
    r
}

/// Un bucle de SPIR-V abierto: el de una cabecera (continue = volver a ella) o un bloque de una vuelta seguido del
/// nodo de fusión `seguido_de` (salir = ir a ese nodo).
struct Lazo {
    cabecera: Option<usize>,
    seguido_de: Option<usize>,
    merge: Word,
    cont: Word,
    /// código de este lazo en la variable de destino: 2n salir, 2n+1 continuar
    n: u32,
    /// alguna salida de varios niveles pasa por aquí (en su fusión hay que mirar el destino)
    pasan: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Destino {
    Salir,
    Continuar,
}

impl Tr<'_> {
    /// Emite el cuerpo de la función con control de flujo estructurado.
    pub(super) fn emitir_estructurado(&mut self) -> Res<()> {
        let cfg = Cfg::nuevo(self.f, self.an)?;
        let entrada = self.an.orden[0];
        let mut lazos = Vec::new();
        self.arbol(&cfg, &mut lazos, entrada)?;
        if self.b.selected_block().is_some() {
            self.b.ret().unwrap();
        }
        Ok(())
    }

    /// doTree: el nodo `x` con lo que domina.
    fn arbol(&mut self, cfg: &Cfg, lazos: &mut Vec<Lazo>, x: usize) -> Res<()> {
        let mut ys: Vec<usize> = cfg.hijos[x].iter().copied().filter(|&y| cfg.fusion[y]).collect();
        ys.sort_by_key(|&y| std::cmp::Reverse(cfg.rpo[y]));
        if cfg.cabecera[x] {
            self.abrir_lazo(lazos, Some(x), None);
            self.dentro(cfg, lazos, x, &ys)?;
            self.cerrar_lazo(lazos)?;
            Ok(())
        } else {
            self.dentro(cfg, lazos, x, &ys)
        }
    }

    /// nodeWithin: el código de `x` envuelto en un bloque por cada hijo de fusión (el de mayor orden, el de fuera).
    fn dentro(&mut self, cfg: &Cfg, lazos: &mut Vec<Lazo>, x: usize, ys: &[usize]) -> Res<()> {
        if let Some((&y, resto)) = ys.split_first() {
            self.abrir_lazo(lazos, None, Some(y));
            self.dentro(cfg, lazos, x, resto)?;
            self.cerrar_lazo(lazos)?;
            if self.b.selected_block().is_some() {
                self.arbol(cfg, lazos, y)?;
            }
            return Ok(());
        }
        self.sin_terminador = true;
        let r = self.bloque(x);
        self.sin_terminador = false;
        r?;
        self.salida(cfg, lazos, x)
    }

    /// El terminador del bloque `x`.
    fn salida(&mut self, cfg: &Cfg, lazos: &mut Vec<Lazo>, x: usize) -> Res<()> {
        let f = self.f;
        let term = &f.blocks[x].insts.last().ok_or("bloque vacío")?.inst;
        let suc = &cfg.suc[x];
        match term {
            Inst::Ret(_) | Inst::Unreachable => return self.terminador(x, term),
            _ if suc.len() == 1 || suc.iter().all(|&s| s == suc[0]) => {
                let t = *suc.first().ok_or("salto sin destino vivo")?;
                return self.ir_a(cfg, lazos, x, t);
            }
            Inst::CondBr { c, t, f: fl } => {
                let cid = self.id(*c)?;
                self.si(cfg, lazos, x, cid, *t, *fl)
            }
            Inst::Switch { v, default, cases } => {
                // cadena de comparaciones (siempre estructurada y válida)
                let xv = self.id(*v)?;
                let mut casos: Vec<(u32, usize)> = Vec::new();
                for (cv, b) in cases {
                    let k = match self.tv(*cv)? {
                        TV::C(V::Int(_, k)) => k as u32,
                        _ => return Err("caso de switch no constante".into()),
                    };
                    casos.push((k, *b));
                }
                self.cadena_switch(cfg, lazos, x, xv, &casos, *default)
            }
            _ => Err("terminador desconocido".into()),
        }
    }

    fn cadena_switch(&mut self, cfg: &Cfg, lazos: &mut Vec<Lazo>, x: usize, xv: Word, casos: &[(u32, usize)], defecto: usize) -> Res<()> {
        let Some((&(k, t), resto)) = casos.split_first() else { return self.ir_a(cfg, lazos, x, defecto) };
        let kc = self.cu(k);
        let c = self.b.i_equal(self.t_bool, None, xv, kc).unwrap();
        let (lt, lf, lm) = (self.b.id(), self.b.id(), self.b.id());
        self.b.selection_merge(lm, SelectionControl::NONE).unwrap();
        self.b.branch_conditional(c, lt, lf, []).unwrap();
        self.b.begin_block(Some(lt)).unwrap();
        self.ir_a(cfg, lazos, x, t)?;
        let a = self.cerrar_rama(lm);
        self.b.begin_block(Some(lf)).unwrap();
        self.cadena_switch(cfg, lazos, x, xv, resto, defecto)?;
        let b = self.cerrar_rama(lm);
        self.fusion_de_seleccion(lm, a || b);
        Ok(())
    }

    fn si(&mut self, cfg: &Cfg, lazos: &mut Vec<Lazo>, x: usize, c: Word, t: usize, fl: usize) -> Res<()> {
        let (lt, lf, lm) = (self.b.id(), self.b.id(), self.b.id());
        self.b.selection_merge(lm, SelectionControl::NONE).unwrap();
        self.b.branch_conditional(c, lt, lf, []).unwrap();
        self.b.begin_block(Some(lt)).unwrap();
        self.ir_a(cfg, lazos, x, t)?;
        let a = self.cerrar_rama(lm);
        self.b.begin_block(Some(lf)).unwrap();
        self.ir_a(cfg, lazos, x, fl)?;
        let b = self.cerrar_rama(lm);
        self.fusion_de_seleccion(lm, a || b);
        Ok(())
    }

    /// Si la rama sigue abierta, salta a la fusión; dice si lo hizo.
    fn cerrar_rama(&mut self, lm: Word) -> bool {
        if self.b.selected_block().is_some() {
            self.b.branch(lm).unwrap();
            true
        } else {
            false
        }
    }

    /// El bloque de fusión de una selección: abierto si alguna rama llega, si no inalcanzable.
    fn fusion_de_seleccion(&mut self, lm: Word, alcanzable: bool) {
        self.b.begin_block(Some(lm)).unwrap();
        if !alcanzable {
            self.b.unreachable().unwrap();
        }
    }

    /// doBranch: de `x` a `t` (los phi de `t` reciben su valor antes).
    fn ir_a(&mut self, cfg: &Cfg, lazos: &mut Vec<Lazo>, x: usize, t: usize) -> Res<()> {
        self.copiar_phis(x, t)?;
        if cfg.rpo[t] <= cfg.rpo[x] {
            let i = lazos.iter().rposition(|l| l.cabecera == Some(t)).ok_or("continue a un bucle que no está abierto")?;
            self.escapar(lazos, i, Destino::Continuar);
            Ok(())
        } else if cfg.fusion[t] {
            let i = lazos.iter().rposition(|l| l.seguido_de == Some(t)).ok_or("salto a una fusión sin bloque abierto")?;
            self.escapar(lazos, i, Destino::Salir);
            Ok(())
        } else {
            self.arbol(cfg, lazos, t)
        }
    }

    /// Sale hacia el lazo `i` (salir de él o continuarlo) desde el más interno.
    fn escapar(&mut self, lazos: &mut [Lazo], i: usize, d: Destino) {
        let k = lazos.len() - 1;
        if i == k {
            let destino = if d == Destino::Salir { lazos[k].merge } else { lazos[k].cont };
            self.b.branch(destino).unwrap();
            return;
        }
        let codigo = lazos[i].n * 2 + (d == Destino::Continuar) as u32;
        let c = self.cu(codigo);
        self.b.store(self.var_destino, c, None, []).unwrap();
        for l in lazos[i..].iter_mut() {
            l.pasan = true;
        }
        self.b.branch(lazos[k].merge).unwrap();
    }

    fn abrir_lazo(&mut self, lazos: &mut Vec<Lazo>, cabecera: Option<usize>, seguido_de: Option<usize>) {
        let (h, cuerpo, merge, cont) = (self.b.id(), self.b.id(), self.b.id(), self.b.id());
        self.b.branch(h).unwrap();
        self.b.begin_block(Some(h)).unwrap();
        self.b.loop_merge(merge, cont, spirv::LoopControl::NONE, []).unwrap();
        self.b.branch(cuerpo).unwrap();
        self.b.begin_block(Some(cuerpo)).unwrap();
        self.n_lazos += 1;
        lazos.push(Lazo { cabecera, seguido_de, merge, cont, n: self.n_lazos, pasan: false });
        self.cabeceras_lazo.push(h);
    }

    fn cerrar_lazo(&mut self, lazos: &mut Vec<Lazo>) -> Res<()> {
        let l = lazos.pop().ok_or("cerrar un lazo que no está abierto")?;
        let h = self.cabeceras_lazo.pop().ok_or("lazo sin cabecera")?;
        if self.b.selected_block().is_some() {
            self.b.branch(l.merge).unwrap();
        }
        // continue: deja el destino a cero (llegó) y vuelve a la cabecera; un bloque de una vuelta no repite
        self.b.begin_block(Some(l.cont)).unwrap();
        if l.cabecera.is_some() {
            let cero = self.cu(0);
            self.b.store(self.var_destino, cero, None, []).unwrap();
            self.b.branch(h).unwrap();
        } else {
            let falso = self.cbool(false);
            self.b.branch_conditional(falso, h, l.merge, []).unwrap();
        }
        self.b.begin_block(Some(l.merge)).unwrap();
        if l.pasan {
            // si la salida era a este lazo, ha llegado; si es a otro de fuera, se sigue saliendo
            let d = self.b.load(self.t_u32, None, self.var_destino, None, []).unwrap();
            let mio = self.cu(l.n * 2);
            let cero = self.cu(0);
            let es_mio = self.b.i_equal(self.t_bool, None, d, mio).unwrap();
            let nd = self.b.select(self.t_u32, None, es_mio, cero, d).unwrap();
            self.b.store(self.var_destino, nd, None, []).unwrap();
            let sigue = self.b.i_not_equal(self.t_bool, None, nd, cero).unwrap();
            let (la, lm) = (self.b.id(), self.b.id());
            self.b.selection_merge(lm, SelectionControl::NONE).unwrap();
            self.b.branch_conditional(sigue, la, lm, []).unwrap();
            self.b.begin_block(Some(la)).unwrap();
            match lazos.last_mut() {
                Some(fuera) => {
                    fuera.pasan = true;
                    let (fm, fc, fn_) = (fuera.merge, fuera.cont, fuera.n);
                    let es_cont = fuera.cabecera.is_some();
                    if es_cont {
                        let cc = self.cu(fn_ * 2 + 1);
                        let c = self.b.i_equal(self.t_bool, None, nd, cc).unwrap();
                        let (a1, a2, m2) = (self.b.id(), self.b.id(), self.b.id());
                        self.b.selection_merge(m2, SelectionControl::NONE).unwrap();
                        self.b.branch_conditional(c, a1, a2, []).unwrap();
                        self.b.begin_block(Some(a1)).unwrap();
                        self.b.branch(fc).unwrap();
                        self.b.begin_block(Some(a2)).unwrap();
                        self.b.branch(fm).unwrap();
                        self.b.begin_block(Some(m2)).unwrap();
                        self.b.unreachable().unwrap();
                    } else {
                        self.b.branch(fm).unwrap();
                    }
                }
                None => {
                    self.b.unreachable().unwrap();
                }
            }
            self.b.begin_block(Some(lm)).unwrap();
        }
        Ok(())
    }
}
