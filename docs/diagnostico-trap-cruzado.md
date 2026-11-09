# Trap cruzado: posible bug de Bash 3.2

## Estado

**Posible bug de Bash 3.2, pendiente de reproducción independiente.** No se ha
cambiado el emulador para ocultarlo. No se considera aprobada esta regresión.
No hubo compilación local ni commits durante esta investigación.

El coordinador observó FAIL 139 en `trap_cruzado`, revisión `80cb7ab`,
Actions run `42583758088` (23/24 regresiones y PTY aprobado). Se reprodujo
localmente con el ELF ya construido en ese run y el Bash de Recovery existente:

```bash
n=0
trap 'n=$((n+1)); kill -USR2 $$' USR1
trap 'n=$((n+10))' USR2
kill -USR1 $$
test "$n" = 11 && printf OK
```

## Evidencia verificada

Con `TAHOE_TRACE=1`, la reproducción local terminó con código 139. Conteos
sobre `/tmp/tahoe-cross-trace.log` (log local, no incorporado al repositorio):

- 1 syscall `kill` Darwin USR1 (30).
- 20 118 syscalls `kill` Darwin USR2 (31).
- 20 119 invocaciones `sigreturn`, estilo 30.
- 60 362 invocaciones `sigprocmask`.
- Ningún `sigreturn` con UC_RESET_ALT_STACK ni UC_SET_ALT_STACK.

La secuencia inicial y repetida es `kill USR2 → sigreturn30 → sigprocmask
SIG_BLOCK → consultas de máscara → kill USR2`. No muestra acumulación de marcos
Linux de entrega sin retornar; la recurrencia visible está en llamadas Bash.
La ausencia de esas formas de sigreturn **no prueba por sí sola** ausencia de
cualquier salto no local.

Una segunda reproducción usó `bash -x` y añadió una guardia antes del kill del
trap USR1: `if test "$n" -gt 3; then exit 77; fi`. Terminó con código 77 y mostró:

```text
+ kill -USR1 347525
++ n=1
++ test 1 -gt 3
++ kill -USR2 347525
+++ n=2
+++ test 2 -gt 3
+++ kill -USR2 347525
++++ n=3
++++ test 3 -gt 3
++++ kill -USR2 347525
+++++ n=4
+++++ test 4 -gt 3
+++++ exit 77
```

No se observa ejecutar el cuerpo `n=$((n+10))` antes de la salida acotada.

## Fuente Apple fijada y contraste con el binario

Fuente consultada y verificada en revisión inmutable:
[Apple Bash trap.c, 51bf3fc6f26e9517c3a2e4bc3d208f9b39b87178](https://github.com/apple-oss-distributions/bash/blob/51bf3fc6f26e9517c3a2e4bc3d208f9b39b87178/bash-3.2/trap.c).

- `run_pending_traps`, línea 268: borra `catch_flag` al entrar.
- Línea 326: ejecuta el cuerpo con `parse_and_execute`.
- Línea 331: borra `pending_traps[sig]` **después** de ejecutar el cuerpo.
- `trap_handler`, líneas 363–364: establece `catch_flag=1` e incrementa la
  pendiente de la señal recibida; línea 373 permite ejecutar pendientes.

Se desensamblaron bytes del Bash exacto usado, sin compilar ni modificarlo.
El frame recurrente del diagnóstico, `0x10003e84b`, es el retorno de una llamada
al ejecutor del cuerpo en `0x10003e846`. El borrado de la entrada pendiente
aparece después, en `0x10003e85b`. Esto concuerda con la secuencia de la fuente;
no se afirma identidad de revisión del binario por disponer de esa fuente.

El mismo script ejecutado con `/bin/dash` original de esa Recovery, sobre el
mismo emulador, terminó con salida 0 y marcador `DASH_CRUZADO_OK`. Es un
contraste útil, no una reproducción independiente de Bash sobre macOS nativo.

## Interpretación y siguiente validación

La hipótesis respaldada por esas observaciones es que USR2 vuelve a activar
`catch_flag` mientras Bash todavía ejecuta el cuerpo USR1 y conserva su entrada
pendiente. Una nueva exploración de pendientes vuelve a ejecutar USR1 antes de
que se borre su entrada, produciendo recursión de comandos y agotamiento de pila.

Falta una reproducción de referencia independiente para clasificarlo como bug
confirmado de Bash, y falta la validación cruzada de entrega/contexto en
`signalprobe` que ejecuta el coordinador. No se ha ejecutado ese probe en esta
investigación. Bloquear artificialmente todas las señales mientras una esté
bloqueada no sería una corrección legítima de las máscaras Darwin/POSIX.
