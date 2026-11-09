# Regresiones de shell sobre Recovery

## Base verificada el 9 de noviembre de 2026

- Recovery original de Apple: macOS 26.6.2, build 25G83.
- Emulador: commit `92e03169fb8c11f9e59f31a5ad1535525b88d7eb`.
- GitHub Actions: ejecución `42582988131`, aprobada.
- Regresiones ejecutadas localmente con el binario compilado en Actions:
  19/19 casos de la suite existente en ese commit y prueba de PTY aprobados.
- Incluye auto-USR1, continuación después del trap y dos entregas consecutivas.
- No se compila localmente ni se distribuyen los binarios de Apple.

Esto NO demuestra shell completa, señales externas, entrega multihilo,
SIGCHLD asíncrono, Ctrl-C, conservación SIMD bajo todas las variantes ni
soporte completo de los servicios de Recovery. Nuevos casos de la suite
pueden seguir fallando: la cuenta anterior corresponde al commit indicado.

## Ampliación verificada

El emulador `80cb7ab` (Actions `42583758088`) pasó 23/24 regresiones y PTY.
El fallo es `trap_cruzado` de Bash, documentado separadamente; no se elimina ni
se cuenta como aprobado. El mismo script pasó con Dash original.

`signalprobe`, compilado en Actions `42584735861` desde `556734a`, se ejecutó
sobre ese emulador y Recovery con salida 0 y `SENALES_MASCARAS_SIGINFO_OK`.
Comprueba bloqueo de USR1/USR2, pendientes, desbloqueo selectivo, `siginfo`
y entrega anidada de USR2 desde el handler USR1. No prueba señales externas.

## Artefacto de integración reciente

- Commit emulador `1744198342befcab792223e91bf0eae9f231a0dc`, compilado en
  Actions `42666648197`; sin compilación local.
- `spawnprobe` imprimió `SPAWN_FD_ALTO_OK` y `SPAWN_SIN_TRUNCAR_OK`: acción
  normal, 33 acciones rechazadas con E2BIG, fd fuente alto duplicado a 1025
  y heredado bajo `CLOEXEC_DEFAULT` con escritura verificada por pipe.
- La suite shell: 23/24; sigue fallando únicamente `trap_cruzado` (Bash 3.2).
- PTY pasó Ctrl-C en el prompt y en sleep foreground, 1 y 10 repeticiones;
  Ctrl-Z/jobs/bg/fg también pasó. No cubre todos los grupos/carreras.
- `probar-senales-externas.py`: USR1 y USR2 aprobadas; `signalprobe` y
  `signaljumpprobe` también aprobaron metadata/máscaras y 24 saltos combinados.
- `launchctl list` en prueba aislada salió 0 con la extracción HFS completa;
  hubo servicios sin PID/errores, por lo que no se declara Recovery íntegra.

Esto aún NO demuestra shell completa: quedan EINTR/SA_RESTART en todas las
esperas, señales externas durante loops sin syscalls, multihilo, todos los
servicios, la compatibilidad cruzada de Bash, y más escenarios de spawn.

## Extracción

7zip dejó archivos HFS vacíos; no usar esa extracción como referencia completa.
Se compiló `hfsdump`/`hfstar` en GitHub desde `0x09/hfsfuse`, revisión
`eed780cbe0445893de6f4aa8d74461da00756984`. El archivo tar producido se extrajo
en un directorio nuevo con `tools/extraer-hfs-tar.py`. Los enlaces absolutos
se confinan a la raíz y se rechazan escapes; no se extraen dispositivos.
El volumen Data vacío se preparó desde `System/Library/Templates/Data`,
copiando enlaces sin seguirlos. El `rm` original quedó recuperado y se probó
la creación/borrado real de archivos. Los datos Apple permanecen fuera del repo.

## Repetición

```sh
python3 -I tools/probar-shell.py --root "$TAHOE_ROOT" --run "$TAHOE_RUN" --salida "$RESULTADOS/shell"
python3 -I tools/probar-shell-pty.py --root "$TAHOE_ROOT" --run "$TAHOE_RUN" --salida "$RESULTADOS/pty"
```

Las suites usan límites temporales y grupos propios para la limpieza. PTY
comprueba prompt/órdenes y trabajo con espera; no equivale a validar todo el
control de trabajos y señales de terminal.
