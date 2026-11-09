# Ctrl-C en el prompt: incremento bloqueado por saltos no locales

Estado: **no implementado ni validado**. No se habilita SIGINT parcialmente para
ocultar la desaparición de Bash. No se ha compilado localmente, lanzado Actions,
creado commits ni cambiado `tools/probar-shell-pty.py`.

## Bloqueo concreto

La entrega existente en `tahoe-run/emu_proc.c` conserva hasta ocho marcos
privados por hilo. Sólo `bsd_sigreturn` con estilo 30, dirección y token válidos
retira el último marco. Las variantes `UC_RESET_ALT_STACK` y
`UC_SET_ALT_STACK` únicamente actualizan `alt_flags`.

Si un handler abandona `_sigtramp` mediante un salto no local, no ejecuta ese
retorno normal: el marco privado puede quedar activo. Habilitar SIGINT sin
resolverlo permitiría acumular marcos y alcanzar EAGAIN; el epílogo SIGSYS
convierte un fallo diferido en terminación. Por tanto, diez interrupciones no
quedan cubiertas por añadir un tercer slot al colector.

La fuente Apple consultada es
[libplatform/src/setjmp/x86_64/setjmp.s](https://github.com/apple-oss-distributions/libplatform/blob/main/src/setjmp/x86_64/setjmp.s):
`siglongjmp` con máscara guardada pasa por `longjmp`, que restaura la máscara
mediante `sigprocmask`, llama a `__sigunaltstack` con el estado guardado y sólo
entonces transfiere a `__longjmp` para restaurar registros/pila. Sin máscara
guardada pasa directamente a `__longjmp`. Es una referencia de código fuente,
no una comprobación de la versión concreta de la caché Tahoe usada.

El aviso de pila alternativa no identifica el destino del salto ni qué marcos
privados atraviesa. Vaciar todos los marcos al recibir RESET rompería un salto
dentro del propio handler o uno que deba conservar un handler exterior. Inferir
abandono sólo comparando RSP tampoco cubre cambios de pila. No se ha establecido
un protocolo seguro que distinga esos casos y preserve máscaras, tokens y
contextos anidados; no se introduce esa heurística como solución.

## Requisitos del siguiente incremento

- Verificar en la libplatform efectiva de Tahoe la secuencia de salto y definir
  una retirada precisa de los marcos abandonados, sin tocar FS/GS, selector SUD,
  `t_cur` ni infraestructura SIGSYS. No llamar al handler invitado desde C.
- Ampliar juntos selección Rust (bit Darwin 1), colector lock-free, sigpending,
  limpieza de hilo, captura/IGN/DFL y selección/consumo de metadata externos.
- Distinguir SIGINT de terminal (`SI_KERNEL`, sin remitente de usuario válido)
  de `kill` (`SI_USER`, pid/uid); no inventar pid/uid para el primero. Los USR
  conservan su filtro SI_USER y sus pruebas actuales.
- Mantener entrega sólo desde el contexto del epílogo SIGSYS. No bloquear SIGINT
  artificialmente para evitar la prueba. El colector no debe reiniciar `read`
  automáticamente: el EINTR de Linux debe convertirse al retorno Darwin antes
  de construir el contexto del handler. Revisar también cada espera que reintente
  EINTR internamente: podría impedir alcanzar el epílogo.
- Bucles sin syscalls, señales entre epílogo y una espera bloqueante, entregas
  multihilo y trabajo foreground requieren análisis adicional. Una entrega
  diferida no garantiza latencia ni elimina por sí sola esas ventanas.

## Validación pendiente

1. Compilar exclusivamente en Actions, sobre la rama autorizada; registrar SHA
   y run/artifact exactos. No usar el binario baseline como prueba del cambio.
2. Ejecutar la prueba PTY `--ctrl-c` editada por el responsable: prompt inicial,
   Ctrl-C, prompt nuevo y comando marcador posterior, con mismo Bash vivo.
3. Tras el primer pase, diez Ctrl-C consecutivos sincronizados con cada prompt;
   comprobar comando marcador tras cada uno, sin EAGAIN, salida ni bloqueo.
4. Repetir las pruebas externas USR1/USR2, metadata, máscaras, contextos,
   anidamiento y retorno; añadir saltos dentro/fuera de handler y con handler
   exterior activo, incluyendo variantes de máscara guardada.
5. Mantener visible el fallo cruzado de trap de Bash 3.2 ya documentado; no
   reclasificarlo como pase. Foreground queda para la siguiente fase.

No se han ejecutado estas validaciones en esta intervención. La desaparición
baseline comunicada sigue abierta; esta nota no afirma que se haya corregido.
