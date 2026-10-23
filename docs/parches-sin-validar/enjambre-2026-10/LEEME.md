# Trabajo sin revisar: 57 commits y parches del enjambre (octubre de 2026)

> **Aviso importante.** Todo lo de esta carpeta lo escribieron modelos de IA (56 de los 57 commits llevan el trailer
> `Co-Authored-By: MiniMax M3.1 Flash`; los parches del enjambre, agentes de OpenCode con modelos gratuitos coordinados
> por Claude Code). **Nadie lo ha revisado línea por línea y casi nada se ha probado contra macOS.** Se sube tal cual
> por si a alguien le sirve de punto de partida. No forma parte de `main` ni está validado.

## De qué punto parte (importante)

La historia local y la de `main` **divergen** desde el commit `b46a26b` (11 de octubre de 2026, fusión del PR #6):

- Estos **57 commits** (19 y 20 de octubre) se hicieron sobre `b46a26b` y nunca se subieron.
- `main` tiene **124 commits** (14 y 15 de octubre, con los PR #8 y #9) que estos 57 no incluyen.

Por eso **nada de esto se ha probado sobre el `main` actual y es de esperar que haya conflictos** al aplicarlo allí. El
orden en que se produjo es: `b46a26b` → los 57 commits → los parches del enjambre (encima de los 57, sobre `ed41eb3`).

## Qué hay

| Ruta | Contenido |
|---|---|
| `commits-sin-revisar/` | Los 57 commits como `git format-patch` (`b46a26b..ed41eb3`, sin binarios). Se aplican en orden con `git am`. |
| `combinado-c.diff` | Los parches 1, 2, 3 y 5 del enjambre unidos (C y documentación), para aplicar **encima** de los 57. |
| `parche-4-rust.diff` | Parche de la tarea 4 (Rust: `tahoe-gpu` y `tahoe-metal`), regenerado porque el original salió corrupto. |
| `individuales/parche-N.diff` | Cada parche tal como lo dejó su agente. Los parches 2 y 6 implementan lo mismo (`F_PREALLOCATE`) de dos maneras y chocan entre sí. |
| `informes/informe-N.md` | El informe de cada agente: qué cambió, la referencia de XNU y lo que dice que no pudo verificar. |

Los diffs usan rutas relativas (`a/` y `b/`): se aplican con `git apply` desde la raíz del repo.

## Las tareas del enjambre

| # | Modelo | Tarea (de `Pendientes.md`) |
|---|---|---|
| 1 | MiniMax M3.1 Flash | `mach_vm_region`, `mach_port_space_basic_info` y `necp_open`, para `top` |
| 2 | MiniMax M3.1 Flash | `fcntl F_PREALLOCATE` (y `F_PUNCHHOLE`, `F_SETSIZE`, `F_RDADVISE`) |
| 3 | MiniMax M3.1 Flash | Caídas de `login` y `screen`: ioctl de pty, `getlogin`/`setlogin`, `TIOCSCTTY` |
| 4 | Nemotron 3 Ultra | Auditoría de `tahoe-gpu` y `tahoe-metal`: validación del protocolo, ABI, `unsafe` |
| 5 | MiniMax M3.1 Flash | Tamaños por volumen en `diskutil list` y `diskutil apfs list` |
| 6 | GLM 5.3 (API de NVIDIA) | `F_PREALLOCATE`, versión alternativa a la del parche 2 |

## Qué se ha comprobado y qué no

- **Comprobado:** el árbol `b46a26b` + los 57 commits (la parte de `tahoe-run`, en C) **compila** con `tahoe-run/build.sh`.
  Sobre ese árbol, los parches 1, 2, 3 y 5 se aplican juntos (el único choque era la línea del `enum` de errores de
  `tahoe.h`, resuelto uniendo los códigos de los dos) y el resultado también compila. Los parches `combinado-c.diff`,
  `parche-4-rust.diff` y `individuales/parche-6.diff` pasan `git apply --check` sobre `ed41eb3`.
- **No comprobado:**
  - Que funcionen. No se ha completado la batería de `tools/diag/bateria.sh`, no se ha ejecutado nada de esto contra
    macOS y no se sabe si arreglan los fallos que buscaban. El informe de la tarea 3 admite que no sabe si `login` y
    `screen` dejan de caerse, y el de la tarea 2 que la hipótesis de que `F_PREALLOCATE` cause las preferencias de 0
    bytes no está demostrada.
  - Las partes en Rust (los 57 commits que las tocan y el parche 4). El agente del parche 4 dice que `cargo check`
    da 0 avisos; no se ha repetido.
  - Cualquier aplicación sobre el `main` actual.
- El parche 6 no se ha probado junto con el resto y choca con el 2 en `emu_fs.c`.

## Si te sirve

Empieza por `b46a26b`, aplica los commits en orden con `git am`, pasa la batería, y luego los parches del enjambre de
uno en uno. Lo más aprovechable suele ser la referencia a XNU de cada informe (archivos y funciones concretas), más que
el código en sí.
