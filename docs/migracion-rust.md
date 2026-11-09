# Migración incremental a Rust

Objetivo: reducir accesos inseguros, errores de límites y estado compartido sin
cambiar accidentalmente la semántica BSD/Mach. No es una reescritura de una sola
vez ni una garantía automática de estabilidad.

## Primera unidad: banderas de sockets

`nucleo/src/red.rs` sustituye la traducción `msg_flags` de `emu_net.c`.
La función exportada recibe y devuelve `uint32_t`, sin punteros, asignaciones
ni `unsafe`. Conserva las ocho conversiones anteriores, los bits ignorados y
la supresión de SIGPIPE en el anfitrión. La entrega de SIGPIPE al invitado sigue
pendiente: esta refactorización no la implementa.

Las constantes Linux se comprueban mediante `_Static_assert` en C contra las
cabeceras reales. Dos pruebas Rust cubren las 256 combinaciones conocidas y
cada bit restante. El workflow compila el emulador y ejecuta las pruebas de
biblioteca exclusivamente en GitHub; no se compila en la máquina local.

## Política de seguridad

Rust seguro por defecto. `unsafe` se admite únicamente en componentes concretos
que lo necesiten para FFI, memoria invitada o contexto de CPU, con precondiciones
`SAFETY` documentadas y una interfaz segura hacia el resto. No extenderlo por
comodidad ni presentar la frontera C restante como memoria segura.
La prioridad actual es shell y servicios, sin desarrollo gráfico.

## Orden de trabajo

1. Funciones puras de conversión y validación, con pruebas de equivalencia.
2. Parsers de estructuras invitadas sobre slices comprobados, dejando el acceso
   a memoria en una frontera explícita y pequeña.
3. Propiedad de recursos y limpieza de errores, migrando un subsistema cada vez.
4. Concurrencia y servicios Mach/IOKit, después de fijar sus invariantes y pruebas.

Cada cambio debe pasar CI y repetir shell/launchctl con Recovery antes de
considerarse validado funcionalmente. No trasladar `unsafe` de C a Rust sin
reducir ni documentar sus precondiciones. Mantener la ABI estable durante la
transición y no mezclar la migración con cambios funcionales no relacionados.
