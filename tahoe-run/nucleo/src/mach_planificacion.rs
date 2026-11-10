//! Conversión de observaciones Linux a datos de planificación Mach.
#![forbid(unsafe_code)]
#[repr(C)]
pub struct Tiempo { pub segundos: i32, pub microsegundos: i32 }
pub fn ticks(t: i64, hz: i64) -> Tiempo {
    let hz=if hz>0 {hz} else {100};
    Tiempo{segundos:(t/hz) as i32,microsegundos:((t%hz)*(1_000_000/hz)) as i32}
}
pub fn prioridad(nice:i64)->i32 {31i64.saturating_sub(nice).clamp(0,63) as i32}
pub fn estado(st:u8)->i32 {match st {b'R'=>1,b'T'|b't'=>2,b'D'=>4,b'Z'|b'X'=>5,_=>3}}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn tiempo(){let t=ticks(125,100);assert_eq!(t.segundos,1);assert_eq!(t.microsegundos,250000);assert_eq!(ticks(100,0).segundos,1);}
    #[test] fn prioridades(){assert_eq!(prioridad(0),31);assert_eq!(prioridad(-20),51);assert_eq!(prioridad(19),12);assert_eq!(prioridad(i64::MIN),63);assert_eq!(prioridad(i64::MAX),0);}
    #[test] fn estados(){for (s,v) in [(b'R',1),(b'T',2),(b't',2),(b'D',4),(b'Z',5),(b'X',5),(b'S',3),(0,3)]{assert_eq!(estado(s),v);}}
}
