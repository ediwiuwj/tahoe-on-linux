# USR externas: incremento colector + epílogo

Alcance: Linux x86_64, invitado monohilo, SIGUSR1/SIGUSR2 enviadas con `kill` (SI_USER), traducidas a Darwin 30/31. Requiere instalar una captura Darwin con trampoline y banderas soportadas (RESTART, SIGINFO, token 0x400). IGN y DFL se instalan también en Linux; DFL conserva la terminación Linux, no se convierte en un marco Darwin. Un fallo al instalar no publica la nueva disposición en sigtab.

El colector no entrega ni llama al núcleo Rust: únicamente publica pid/uid reales en slots TLS atómicos lock-free, con TLS initial-exec. No usa locks, memoria dinámica, logging, libc, syscalls ni cambia errno o el selector SUD. Ambas USR quedan bloqueadas durante el colector; no usa NODEFER. El restorer propio está en la región SUD exenta. Puede interrumpir C o una sección con mutex sin adquirir ese mutex.

La entrega ocurre en un epílogo SIGSYS posterior, después de materializar el resultado de syscall. El codec y la selección elegible Rust existentes siguen siendo el núcleo lógico. siginfo Darwin usa SI_USER (0x10001) y pid/uid del remitente, no del receptor. La metadata sigue pendiente hasta publicar el marco o descartar por IGN. Se conserva la política existente de fallo cerrado ante un error diferido, sin convertirlo en errno de una lectura. El self-kill mantiene su ruta y prioridad propias; no consume el slot externo. Un kill fallido no sirve como punto de entrega.

Cada señal estándar coalesce en un slot: gana el primer remitente hasta consumirlo; no hay FIFO ni contador de envíos. Las pendientes self y externas son fuentes separadas y pueden producir dos entregas. El hijo de fork no hereda pendientes externas. Cambiar a IGN/DFL descarta el slot externo del hilo actual.

## Límites explícitos

- No entrega en bucles sin syscalls ni garantía de latencia.
- No EINTR/restart completo. RESTART aceptado no promete reiniciar toda syscall Linux interrumpida.
- No sigqueue, tgkill/pthread_kill ni otros si_code: el colector no los publica.
- No SA_ONSTACK Darwin, NODEFER, RESETHAND ni USERTRAMP.
- No selección de hilo ni soporte multihilo: instalar captura con varios hilos devuelve ENOTSUP; si se crean hilos después, las externas quedan en el TLS receptor y no se entregan mientras haya varios hilos. No hay migración de pendientes ni cobertura de hilos host ajenos. Debe mantenerse monohilo durante este incremento.
- No se capturan SIGSYS ni señales RT internas. No se generaliza a otras señales.

## Pruebas sugeridas (no ejecutadas aquí)

1. Repetir signalprobe existente: self USR1/USR2, máscaras, pending, siginfo y nesting.
2. Desde otro proceso enviar USR1/USR2 a bash invitado con trap instalado durante un loop con syscalls; comprobar trap y continuación del loop.
3. Con SA_SIGINFO verificar pid/uid del emisor externo; verificar número Darwin y SI_USER.
4. Bloquear máscara Darwin, enviar repetidamente, consultar pending y desbloquear: coalescencia y metadata del primer envío; repetir ambas señales.
5. Enviar mientras se ejecutan syscalls y se toman mutex de C; comprobar ausencia de bloqueo del colector y conservación del resultado/errno de lectura.
6. Probar IGN, DFL en proceso desechable, transición captura/IGN/DFL y fallo de instalación; comprobar consistencia de disposición.
7. Probar llegada durante una entrega y durante handler, más fork con pendiente; comprobar marcos, máscara y pendientes del hijo.
8. Tras compilar fuera de esta máquina, inspeccionar el colector: acceso TLS directo y atomics inline, ninguna llamada a __tls_get_addr/libatomic/libc ni instrucción syscall. No se ha compilado ni validado código máquina localmente.
