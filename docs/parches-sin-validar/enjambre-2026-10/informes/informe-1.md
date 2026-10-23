# Informe 1 — Punto 9 de «Pendientes» («Detalles»): `top` (mach_vm_region, mach_port_space_basic_info) y `necp_open`

Trabajo en la copia del repo en una copia de trabajo del repo (rama `main`, HEAD `ed41eb3`). Nada fuera de ese directorio.

## 0. Punto de partida: qué había ya hecho el intento anterior

Al empezar, `diff -r` contra `~/macos26/tahoe-on-linux` no mostraba ninguna diferencia: la copia estaba
limpia. Durante mi sesión apareceron (22:04–22:10) cuatro ficheros modificados en la copia —`tahoe-run/emu_mach.c`,
`emu_net.c`, `emu_port.c`, `tahoe.h`— que son **el trabajo del intento anterior que se interrumpió** (por eso el
`diff -r` inicial no lo mostraba). Lo he conservado y lo he revisado contra la semántica de XNU; he encontrado **dos
fallos reales** en `mach_vm_region` (ver §2) y he completado lo que faltaba.

Lo que traía el intento anterior:

| Fichero | Qué |
|---|---|
| `emu_mach.c` | `mig_mach_vm_region` (MIG 4816) y `mig_mach_port_space_basic_info` (MIG 3235) + sus `case` en el despachador de `mach_msg2` |
| `emu_port.c` | `port_space_counts(pid, &inuse, &tsize)`: entradas del espacio IPC de un proceso |
| `emu_net.c` | `necp_open` (syscall 501) |
| `tahoe.h` | declaraciones y `D_EACCES` |

## 1. Cómo comprobé el formato de los mensajes (lo importante)

Los `.defs` de XNU dicen qué argumentos hay, pero **no dónde van en el mensaje**. Eso lo dice el *cliente* de macOS 26
(`libsystem_kernel`, dentro de la caché de dyld). He desensamblado los stubs con las herramientas del propio repo
(`tools/diag/dis.py` + los símbolos ya cacheados en `~/.cache/tahoe-syms`), y de ahí salen los números y el reparto exactos:

| Rutina | Símbolo (en la caché de dyld) | Id (del `msid`) | Petición | Respuesta |
|---|---|---|---|---|
| `mach_vm_region` | `_mach_vm_region` (0x7ff80073d5b6) | **4816** | 48 bytes: NDR +24, `address` +32, `flavor` +40, `count` +44 | «compleja»: 68 + 4·count bytes; descriptor de puerto +28 (disposición en +38), NDR +40, `address` +48, `size` +56, `count` +64, `info` +68 |
| `mach_vm_region_recurse` | `_mach_vm_region_recurse` (0x7ff800742759) | **4815** | 48 bytes: NDR +24, `address` +32, `nesting_depth` +40, `count` +44 | 60 + 4·count bytes: `address` +36, `size` +44, `nesting_depth` +52, `count` +56, `info` +60 |
| `mach_port_space_basic_info` | `__kernelrpc_mach_port_space_basic_info` (0x7ff800758a47) | **3235** | 24 bytes (sin cuerpo) | 60 bytes: NDR +24, `retcode` +32, `ipc_info_space_basic_t` (6 `natural_t`) +36 |
| `task_policy_get` | `_task_policy_get` (0x7ff80075da50) | **3421** | 44 bytes: NDR +24, `flavor` +32, `count` +36, `get_default` +40 | 44 + 4·count bytes: `count` +36, `info` +40, `get_default` +40+4·count |

Reglas del «mensaje complejo» que impone el cliente (comprobadas en el desensamblado): `msgh_bits` con el bit
`MACH_MSGH_BITS_COMPLEX` (0x80000000), `msgh_remote_port` = 0, `msgh_id` = id + 100, `msgh_size` exacto, y el **descriptor
de puerto** de 12 bytes con la disposición en el byte 10 (`MACH_MSG_TYPE_COPY_SEND` = 17) y el tipo en el 11
(`MACH_MSG_PORT_DESCRIPTOR` = 0). Si algo de eso no cuadra, el cliente responde MIG_TYPE_ERROR (-300) y la llamada se pierde.

## 2. Qué he cambiado

### 2.1 `emu_mach.c` — `mach_vm_region` (4816): **dos fallos corregidos**

1. **Los offsets de la petición estaban mal.** El intento anterior leía el sabor en +48 y la cuenta en +52, y exigía una
   petición de 56 bytes. El cliente de macOS 26 manda un mensaje de **48** bytes, con el sabor en **+40** y la cuenta en
   **+44** (`size` es un argumento de salida y no viaja en la petición). Con lo anterior se leía de más (la cuenta salía
   de los bytes de la NDR) y la respuesta era basura.
2. **El reparto de `vm_region_basic_info_64` estaba desplazado 4 bytes y la cuenta era incorrecta.** La estructura de
   XNU (`osfmk/mach/vm_region.h:88`) tiene `offset` (8 bytes) en el byte **24**, con 4 bytes de relleno en el 20; y
   `VM_REGION_BASIC_INFO_COUNT_64` = **10** (40 bytes), no 9. El intento anterior ponía el desplazamiento en el 20 y la
   cuenta en 9: el cliente comprueba `tamaño == 68 + 4·count` y además lee el `offset` en su sitio, así que la columna de
   memoria salía corrupta.
3. El `shared` ahora sale de la cuarta letra de `/proc/<pid>/maps` ('s' = compartido), como el `entry->is_shared` de XNU.
4. La comprobación de la cuenta es la de XNU (solo el mínimo: `vm_map.c`, «if (*count < VM_REGION_BASIC_INFO_COUNT_64)»).
   Se quita el tope invented (`VM_REGION_INFO_MAX` es el tamaño del búfer del cliente, no una comprobación del núcleo).
5. Comentario corregido: la disposición 17 es `MACH_MSG_TYPE_COPY_SEND` (no MOVE_SEND).

### 2.2 `emu_mach.c` — `mach_vm_region_recurse` (4815): **nuevo**

`top` también la importa (ver §4). Aquí no hay submaps, así que la profundidad de anidamiento no cambia y la región es la
misma que en `mach_vm_region`. La cuenta que se devuelve es la de XNU (`vm_map_region_recurse_64`): 13 (SHORT), 17 (V0) o
18 (V1) según lo que pidiera el cliente, nunca más de 19 (`type vm_region_recurse_info_t = array[*:19]`,
`osfmk/mach/mach_types.defs:573`). El reparto de `vm_region_submap_info_64` respeta sus huecos (`offset` en el byte 16,
`share_mode` en el 51, etc.).

### 2.3 `emu_mach.c` — `task_policy_get` (3421): **nuevo**

También la importa `top`. Sabores de XNU (`osfmk/kern/task_policy.c:575`): `TASK_CATEGORY_POLICY` (1),
`TASK_SUPPRESSION_POLICY` (3), `TASK_POLICY_STATE` (4, solo root), `TASK_BASE_QOS_POLICY` (8) y `TASK_OVERRIDE_QOS_POLICY`
(9); los demás, `KERN_INVALID_ARGUMENT`. Aquí las tareas no tienen papel, QoS ni supresión, así que se devuelven los
valores «sin especificar» (los mismos que da XNU con `get_default`). Como en XNU, la cuenta no se modifica: se devuelve
la que pidió el cliente (él la limita a 16) y el `get_default` vuelve sin cambios.

### 2.4 `emu_mach.c` — `mach_port_space_basic_info` (3235): **revisado, sin cambios**

Está bien tal como estaba: la petición no tiene cuerpo (24 bytes), la respuesta son 60 bytes con
`ipc_info_space_basic_t` (6 `natural_t`: `iisb_genno_mask`, `iisb_table_size`, `iisb_table_next`, `iisb_table_inuse` y
dos reservados — `osfmk/mach_debug/ipc_info.h:163`) y `iisb_genno_mask` = `MACH_PORT_NGEN(MACH_PORT_DEAD)` = 0xff
(`osfmk/ipc/port.h:72`, con `MACH_PORT_DEAD` = ~0).

### 2.5 `emu_port.c` — `port_space_counts`: **revisado, sin cambios**

Cuenta los nombres del espacio de un proceso (los puertos que posee + los derechos de envío que tiene sobre los de
otros), que es lo que informa `iisb_table_inuse`. En XNU `iisb_table_size` es el tamaño de la tabla **de cada tarea**;
aquí la tabla de puertos es única para todo el sistema, así que se da la marca más alta usada (queda explicado en el
comentario del código).

### 2.6 `emu_net.c` + `tahoe_run.c` + `tahoe.h` — `necp_open` (syscall 501): **ajustado**

El intento anterior ya lo implementaba (número de syscall correcto: 501 en macOS 26, según `sysnames.h`) y comprobaba
bien el privilegio de observador (EACCES sin él, como en XNU). Lo que he cambiado es el comportamiento del descriptor,
para que sea el de XNU y no el de `/dev/null`:

* En XNU, el descriptor de cliente NECP tiene `necp_fd_ops` con `fo_read = fo_no_read`, `fo_write = fo_no_write` y
  `fo_ioctl = fo_no_ioctl` (`bsd/net/necp_client.c:413`), o sea **ENXIO** al leer y al escribir y **ENOTTY** en los
  ioctl (`bsd/kern/kern_descrip.c:6268`, `:6296`, `:6337`).
* Aquí se abre `/dev/null` (para que el descriptor exista y se pueda cerrar) y se recuerda en una tabla pequeña: la
  lectura y la escritura responden ENXIO y el ioctl ya respondía ENOTTY. Al cerrar el descriptor se olvida (si no, un
  número de descriptor reutilizado seguiría dando ENXIO).

## 3. Referencia de XNU usada

* `osfmk/vm/vm_user.c:1437` (`mach_vm_region` → `vm_map_region`, y la conversión de `VM_REGION_BASIC_INFO` a
  `VM_REGION_BASIC_INFO_64` en `:1452`).
* `osfmk/vm/vm_map.c:13664` (`vm_map_region`: los tres sabores, `*count` y el reparto de la estructura) y `:13308`
  (`vm_map_region_recurse_64`: SHORT/V0/V1/V2).
* `osfmk/mach/vm_region.h:88` (`vm_region_basic_info_64`), `:271` (`vm_region_submap_info_64`), `:341` (versión corta).
* `osfmk/ipc/mach_debug.c:358` (`mach_port_space_basic_info`), `osfmk/mach_debug/ipc_info.h:163` (la estructura),
  `osfmk/ipc/port.h:72` (`MACH_PORT_NGEN`).
* `osfmk/kern/task_policy.c:575` (`task_policy_get`), `osfmk/mach/task_policy.h:104-169` (sabores y cuentas).
* `bsd/net/necp_client.c:8098` (`necp_open`), `:413` (`necp_fd_ops`), `bsd/net/necp.h:614` (indicadores),
  `bsd/kern/kern_descrip.c:6268/6296/6337` (`fo_no_read`, `fo_no_write`, `fo_no_ioctl`).
* `osfmk/mach/message.h:152` (`MACH_MSGH_BITS_COMPLEX`) y `osfmk/mach/message.h:408` (`mach_msg_port_descriptor_t`).

## 4. Qué necesita `top` (mirado en su binario de macOS 26.7.1)

Símbolos que importa `/usr/bin/top` (`llvm-nm --undefined-only`): `_mach_port_space_basic_info`, `_mach_vm_region`,
`_mach_vm_region_recurse`, `_task_policy_get`, `_task_info`, `_task_threads`, `_thread_info`, `_pid_for_task`,
`_processor_set_tasks_with_flavor`, `_host_processor_sets`, `_host_processor_set_priv`, `_proc_pidinfo`,
`_proc_pid_rusage`, `_mach_port_deallocate`, `_mach_vm_deallocate`, `_mach_timebase_info`.

De esos, este trabajo cubre `mach_port_space_basic_info`, `mach_vm_region`, `mach_vm_region_recurse` y `task_policy_get`;
los demás ya estaban (`task_info` 3405, `task_threads` 3402, `thread_info` 3612, `pid_for_task` trampa 46,
`processor_set_tasks` 4005/4010, `host_processor_sets` 419, `host_processor_set_priv` 420).

## 5. Qué compila (comprobado)

```
nice -n 19 ionice -c3 cargo build --release -j1 --manifest-path nucleo/Cargo.toml     # OK
nice -n 19 ionice -c3 clang -O1 -g -Wall -Wextra -Wno-unused-parameter -static -o tahoe-run \
    tahoe_run.c emu_sysctl.c emu_fs.c emu_mach.c emu_proc.c emu_kqueue.c emu_net.c emu_fs2.c \
    emu_port.c emu_sem.c emu_psynch.c emu_cs.c emu_cred.c emu_fileport.c emu_dsmos.c \
    nucleo/target/release/libnucleo.a                                               # OK, sin avisos
```
(Lo único que sale es el aviso habitual de glibc sobre `getpwuid` en binarios estáticos.)

## 6. Qué **no** he podido verificar (con honestidad)

* **No he ejecutado nada**: por las reglas no se puede lanzar `tahoe-run` ni la batería. **Ninguna de estas rutas se ha
  probado en ejecución**: ni `top`, ni la shell, ni un arranque. Lo único que he hecho es comparar el formato de las
  respuestas con las condiciones que impone el stub del cliente y comprobar, con una simulación en Python que construye
  los mismos bytes, que se cumplen todas (id, bits, `remote_port`, tamaño exacto, descriptor, cuenta y los campos de
  salida en su sitio). Es una comprobación *estática*, no una prueba de funcionamiento.
* **La batería de regresiones (`tools/diag/bateria.sh`) no se ha ejecutado**, así que no puedo afirmar que no haya
  regresiones. El riesgo mayor es el toque en `tahoe_run.c` (lectura/escritura/cierre): solo afecta a los descriptores que
  `necp_open` acaba de crear, y la condición se comprueba con una tabla de 64 entradas.
* **Las regiones que devuelve `mach_vm_region` son las de Linux** (`/proc/<pid>/maps`), no regiones de la VM de macOS:
  `max_protection` se pone igual que `protection` (Linux no distingue), las páginas (`pages_resident`, etc.) valen 0
  (XNU también las deja a cero sin objeto de memoria), y el `offset` es el desplazamiento del archivo mapeado. Para una
  tarea de otro proceso se lee su `/proc/<pid>/maps` (el invitado comparte el espacio de direcciones con el emulador).
* **`iisb_table_size` es la marca más alta usada de la tabla común de puertos**, no el tamaño de la tabla de cada tarea
  (en XNU cada tarea tiene la suya). Es una aproximación, y está dicha en el código.
* **El privilegio de `necp_open` se aproxima con «uid 0»**: en XNU es `PRIV_NET_PRIVILEGED_NETWORK_STATISTICS` (o el
  de skywalk), que es un derecho, no el identificador de usuario. El emulador no tiene una comprobación de privilegios.
* **No he comprobado qué hace `top` con los datos**: al no poder ejecutarlo, no sé si las columnas de memoria y de
  puertos quedarán con valores útiles o con ceros (por ejemplo, si `top` espera `VM_REGION_EXTENDED_INFO` con el
  número de páginas resident, que aquí siempre es 0).
* **No he implementado** la versión MIG de `mach_port_type` (3201) —la trampa 76 sí está— ni `necp_client_action` (502)
  ni los `__nexus_*` (503-505). Tampoco he mirado cómo implementa `libsystem_kernel` en macOS 26 `proc_pidinfo` y
  `proc_pid_rusage` (en la tabla de syscalls de macOS 26 ya no existen; quedan como funciones de la biblioteca), que
  `top` también importa y que son las que dan la columna de memoria.

## 7. Ficheros y parche

* Cambiados: `tahoe-run/emu_mach.c`, `tahoe-run/emu_net.c`, `tahoe-run/emu_port.c`, `tahoe-run/tahoe.h`,
  `tahoe-run/tahoe_run.c`.
* El parche está en `../parche-1.diff` (`git diff` de esos cinco ficheros). **Nota**: el `git diff` a secas falla en esta
  copia («unable to read …») porque el clon está incompleto y faltan objetos del historial; el diff de los ficheros
  tocados sale bien y se ha comprobado que aplica y desaplica (`git apply --check --reverse`).
* No hay commits, ni push, ni sudo. No se ha tocado nada fuera del directorio de trabajo.
* **`Pendientes.md` y `README.md` se han dejado sin tocar**: el punto 9 («Detalles») no queda cerrado (siguen `iostat` y
  `zprint`, y `necp_open` es solo un descriptor sin políticas NECP) y, como no se ha podido ejecutar nada, no sería
  honesto dar por hecho que las columnas de `top` ya salen bien.
