# Acciones de posix_spawn: migración del parser

`nucleo/src/spawn.rs` analiza por valor el registro Darwin de 1040 bytes,
comprueba tipos, descriptores, terminación de rutas y tamaño del bloque. Es Rust
seguro con `forbid(unsafe_code)`; la exportación de símbolos queda aislada en
`spawn_ffi.rs`. C conserva sólo la lectura de memoria invitada y ejecución.

El conjunto completo se publica después de validar todas las acciones. Una
lectura parcial no ejecuta un prefijo. Tipos no implementados producen ENOTSUP;
rutas sin terminador producen ENAMETOOLONG. El límite existente de 32 acciones
ahora da E2BIG: antes se truncaba silenciosamente. Esto evita éxito falso, no
implementa todavía cantidades arbitrarias.

El plan `CLOEXEC_DEFAULT` también usa un registro fijo probado en Rust: conserva
hasta 32 destinos, incluidos descriptores altos del rango invitado; deduplica y
rechaza metadatos inválidos. C enumera `/proc/self/fd`, añade cierres sólo para
los descriptores huésped abiertos que no sobreviven y no usa `closefrom` sobre
la zona reservada `>=10240`. Los errores de inicialización/construcción de
acciones glibc se propagan y destruyen el objeto parcial. El estado de las
acciones y flags de spawn quedó en TLS.

`SETEXEC` comprueba y devuelve los errores de cada acción, pero puede haber
aplicado un prefijo antes de fallar; no hay rollback transaccional. La
enumeración de descriptores tiene una ventana frente a aperturas concurrentes
por otros hilos; no se afirma atomicidad del snapshot.

Pruebas Rust cubren cabeceras, tipos, límites, descriptores y plan CLOEXEC.
`spawnprobe.c`, compilado en Actions `42666648197` (commit `1744198`), se ejecutó
sobre Recovery Tahoe 26.6.2 y emitió `SPAWN_FD_ALTO_OK`: un descriptor fuente
mayor que 1025 se duplicó a 1025, `CLOEXEC_DEFAULT` lo conservó en el hijo y el
hijo escribió por ese descriptor a una tubería. También emitió
`SPAWN_SIN_TRUNCAR_OK` para spawn normal y rechazo de 33 acciones con E2BIG.
La suite general tuvo 23/24 pases y PTY/job-control pasó; el fallo que queda es
el trap cruzado de Bash documentado aparte.

Pendiente: repetir la prueba de 33 acciones verifica límite del parser, no la
capacidad arbitraria. El plan/ejecución usa el snapshot Linux `/proc/self/fd`
y no es atómico frente a aperturas concurrentes por hilos del mismo proceso.
