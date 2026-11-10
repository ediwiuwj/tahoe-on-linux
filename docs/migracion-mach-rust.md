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

Estado actual: **en curso, no completo**. El primer corte aborda estados de
puertos/conjuntos/guardas y construcción de respuestas MIG; colas, derechos,
memoria y demás componentes siguen pendientes hasta ser migrados y probados.
