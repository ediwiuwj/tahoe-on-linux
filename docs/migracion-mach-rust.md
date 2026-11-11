# Migración completa del subsistema Mach a Rust

## Objetivo solicitado

Migrar toda la implementación de compatibilidad Mach, no sólo parsers. Linux
continúa siendo el kernel; no se ejecuta XNU. La migración no se declara
terminada mientras C conserve lógica Mach propia. Durante la transición se
mantienen el ABI Darwin, el formato de la región compartida y las llamadas
existentes para poder comparar regresiones.

## Inventario de alcance

- `emu_port.c`: tabla/índice de puertos, derechos send/receive/send-once,
  conjuntos, guardas, contextos, colas, traslado de derechos/descriptores,
  notificaciones, recolector al morir procesos, kobjects, puertos especiales,
  vigilancia de exec/VFS y temporizadores Mach.
- `emu_mach.c`: mach_msg2, recepción y trailers, codecs MIG, políticas y
  consultas host/task/thread, memoria virtual/entradas de memoria, clock y
  creación de semáforos.
- `emu_sem.c`: semáforos y sus esperas/señalización.
- `emu_psynch.c`: sincronización que implementa las primitivas del userland;
  revisar la frontera BSD/Mach sin omitirla del inventario.
- `tahoe_run.c` y `emu_proc.c`: traps Mach registrados allí y soporte de
  tareas/hilos. La recepción Linux de SIGSYS y el contexto CPU se separan de la
  lógica de trap que deben terminar en Rust.
- `nucleo/src`: revisar también el Rust ya existente (`mensaje`, `candado`,
  `desborde`, `rwlock`), no asumir que sus bloques unsafe son necesarios o
  correctos sólo por existir.

IOKit/MIG que consume puertos sigue como subsistema consumidor: sus adaptadores
no justifican dejar la lógica de derechos/colas en C. No se desarrolla interfaz
gráfica en esta tarea.

## Política de unsafe

Rust seguro por defecto. Los módulos de lógica/estado/codecs prohíben unsafe.
Si una frontera requiere punteros FFI, atomics en mmap compartido, syscall Linux
o contexto CPU, se documentan tamaño, alineación, validez, propiedad, exclusión
mutua y duración. No crear referencias Rust aliasadas a memoria que otro proceso
puede modificar: primero diseñar el protocolo de exclusión y el layout.
Los atributos de exportación de símbolos se aíslan y justifican por ABI.

## Criterio de cierre

- Inventario completo de funciones migradas; cero lógica Mach propia restante
  en C, o excepciones de frontera enumeradas y justificadas.
- Pruebas Rust de estados, límites, derechos, errores, concurrencia y mensajes
  malformados; comparación de layouts C/Rust durante la transición.
- Compilación exclusivamente GitHub Actions/Codespaces.
- Regresión real shell, spawn, señales, launchctl y servicios con Recovery.
- No sustituir funciones por stubs de éxito ni presentar un refactor parcial
  como implementación completa.

Estado actual: **en curso, no completo**. Hay cortes de estados de
puertos/conjuntos/guardas, planes de colas/derechos y respuestas MIG. Su cierre
requiere validación externa; memoria y demás componentes no se consideran cerrados.

### Corte de notificaciones (sin compilación ni ejecución local)

`mach_notificacion.rs` prohíbe unsafe y decide registro, reemplazo, cancelación,
armado/rearmado de SEND_POSSIBLE, sync/qlimit, DEAD_NAME inmediato, consumo de
NO_SENDERS y devolución PORT_DESTROYED (incluida la guarda de servicio).
`mach_notificacion_ffi.rs` exporta exclusivamente valores; no recibe punteros.
Usa `deny(unsafe_code)` con excepciones sólo para `unsafe(no_mangle)`, justificadas
por símbolos únicos y ABI espejo; no hay bloques unsafe nuevos.
Se reutiliza `Sport` y se copian entradas dnreq/spreq sin cambiar el ABI shm.
Assertions C/Rust fijan los layouts del plan y de las entradas.

Se conserva explícitamente la conducta previa: NO_SENDERS no se dispara al
registrarlo (sync se conserva como argumento); tabla de peticiones llena y
variante desconocida siguen devolviendo éxito; los avisos se consumen aunque
la pila TLS esté llena; flush sigue siendo LIFO. No se pretende corregir estas
diferencias con XNU en este corte. El contador spn usa saturación defensiva en
Rust (no afecta estados válidos: su máximo es MAXSP=1024).

Tests añadidos: armado, consumo único, rearmado/reemplazo/cancelación, sync con
cola llena/disponible, no-senders con/sin holders, devolución con guarda,
nombres muertos, errores y límites de las colas. Se añade un codec seguro de
mensajes de notificación (SEND_ONCE, DEAD_NAME, NO_SENDERS, SEND_POSSIBLE y
PORT_DESTROYED), con pruebas de bytes y layout. Estas pruebas nuevas no se han
ejecutado localmente: quedan compilación y regresiones en Actions.

C pendiente en `emu_port.c`: lock, lookup y recorridos de tablas, snapshots y
commit de campos, pila TLS/flush y serialización de mensajes, copyin/copyout,
poke/syscalls, destrucción de mensajes/derechos, recolector, kobjects, puertos
especiales y temporizadores. La selección de cuándo hay bloqueo de envío o
hueco de recepción sigue en la integración con los planes de cola. No es el
cierre de todas las funciones C ni de la migración Mach.

### Recepción y validación externa

`mach_recepcion.rs` migra la transformación de cabecera, RCV_LARGE_IDENTITY y
los trailers LP64 (credenciales, audit token y contexto). El core prohíbe unsafe;
la FFI trabaja por valor. Copyout, descriptores y efectos de derechos siguen en C.
Este corte nuevo aún requiere Actions y prueba del artefacto resultante.

El artefacto del commit `f21c75d`, compilado en Actions `42862094692`, se ejecutó
con Recovery Tahoe: `MACH_PUERTOS_SEMAFOROS_OK` (incluye envío/recepción, timeout y
destrucción), `SENALES_MASCARAS_SIGINFO_OK`, `SIGLONGJMP_REPETIDO_OK` (24 saltos)
y ambas pruebas de spawn aprobadas. Shell: 23/24; `trap_cruzado` sigue fallando
con retorno 139. No se atribuye ese fallo a Bash ni se declara resuelto sin prueba
independiente. Estos resultados no validan todavía notificaciones ni recepción
recién migradas. PTY con el mismo artefacto: diez Ctrl-C y
Ctrl-Z/jobs/bg/fg aprobados.

La inicialización/reutilización de `Sport` también se decide en Rust: elimina
el estado anterior y conserva exclusivamente la secuencia `cv`, para que un
waiter antiguo vuelva a comprobar el puerto. Lookup, reserva de ranura, índice
y creación del socket aún siguen en C; no se declara migrada toda la creación.
