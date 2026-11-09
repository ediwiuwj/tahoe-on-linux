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

Pendientes independientes: estado por invocación/hilo (hoy es global), errores
al construir acciones glibc, aplicación SETEXEC y conservación de descriptores
altos con CLOEXEC_DEFAULT. Esta migración no afirma resolverlos.

Pruebas Rust: cabeceras inválidas, cantidades, tipos, descriptores, rutas y
flags. `tools/macos/spawnprobe.c`, compilado exclusivamente en Actions, comprueba
un spawn normal y el rechazo explícito de 33 acciones. No se considera aprobado
hasta ejecutar el nuevo artefacto sobre Recovery.
