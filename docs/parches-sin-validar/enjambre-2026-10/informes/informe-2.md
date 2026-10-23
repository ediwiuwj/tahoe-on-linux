# Informe 2 — `fcntl F_PREALLOCATE` (y `F_SETSIZE`, `F_PUNCHHOLE`, `F_RDADVISE`)

**Punto 7 de `Pendientes.md`** · repositorio `tahoe-on-linux` (macOS 26 sobre Linux, sin XNU)
· parche: `parche-2.diff` · sin commits, sin push, sin sudo.

---

## 1. Qué cambié

### `tahoe-run/emu_fs.c` — la capa de ficheros (`bsd_fcntl`, syscall `fcntl` de Darwin, 92)

| Comando | Antes | Ahora |
|---|---|---|
| `F_PREALLOCATE` (42) | **no existía** → `EINVAL` ("sin implementar") | implementado con la `fstore_t` de 32 bytes |
| `F_SETSIZE` (43) | **no existía** → `EINVAL` | implementado (`ftruncate` con las reglas de XNU) |
| `F_RDADVISE` (44) | **no existía** → `EINVAL` | implementado (petición asesoria, sin efecto observable) |
| `F_PUNCHHOLE` (99) | existía, pero sin ninguna comprobación de XNU | completado con las comprobaciones de XNU |

Auxiliares nuevos en el mismo fichero: `fd_writable()` (¿FWRITE?), `fd_is_vnode()`
(¿`DTYPE_VNODE`?), `fd_vnodeclass()` (fichero normal / directorio / otro) y `fs_blocksize()`
(bloque del sistema de ficheros, con `fstatfs`).

Traducción a Linux:

- `F_PREALLOCATE` → `fallocate(fd, FALLOC_FL_KEEP_SIZE, base, len)`. `KEEP_SIZE` porque XNU
  tampoco cambia el tamaño al preasignar.
- `F_PUNCHHOLE` → `fallocate(fd, FALLOC_FL_PUNCH_HOLE|FALLOC_FL_KEEP_SIZE, lo, hi-lo)`.
- `F_SETSIZE` → `ftruncate(fd, size)`, con la condición de root de XNU.
- `fst_bytesalloc` → los bytes que cubren los bloques del rango `[base, base+len)`.

### `tahoe-run/tahoe.h`

Añadidos al enum de códigos de error de Darwin: `D_EACCES = 13`, `D_EISDIR = 21`,
`D_ENOTSUP = 102` (faltaban).

### `Pendientes.md`

El punto 7 ya no dice que `F_PREALLOCATE` no está implementado; dice que **está implementado pero
sin verificar en un arranque real**.

---

## 2. Por qué: la referencia de XNU

Fuentes (solo lectura) en `~/macos26/src/xnu`, XNU 12377.

### `F_PREALLOCATE` — `bsd/kern/kern_descrip.c:3371-3461`

```
case F_PREALLOCATE: {
        if (fp->f_type != DTYPE_VNODE)              error = EBADF;
        if ((fp->f_flag & FWRITE) == 0)              error = EBADF;
        error = copyin(argp, &alloc_struct, sizeof(alloc_struct));   // EFAULT
        alloc_struct.fst_bytesalloc = 0;             // el kernel lo pone a cero antes de reservar
        ... F_ALLOCATECONTIG / F_ALLOCATEALL / F_ALLOCATEPERSIST -> ALLOCATECONTIG / ALLOCATEALL / ALLOCATEPERSIST
        switch (alloc_struct.fst_posmode) {
        case F_PEOFPOSMODE: if (fst_offset != 0)  error = EINVAL;   // y NO se copia la estructura de vuelta
        case F_VOLPOSMODE:  if (fst_offset <= 0)  error = EINVAL;   // ídem
        default:            error = EINVAL;                          // ídem
        }
        error = VNOP_ALLOCATE(vp, fst_length, alloc_flags, &fst_bytesalloc, fst_offset, &context);
        error2 = copyout(&alloc_struct, argp, sizeof(alloc_struct));  // la estructura se copia SIEMPRE aquí
        if (error == 0) error = error2;                              // el error que se ve es el de la reserva
}
```

Tres detalles que la emulación respeta y que son fáciles de pasar por alto:

1. **La `fstore_t` se copia de vuelta incluso si la reserva falla** (pero **no** si el `posmode` es
   inválido, ni si faltan los permisos). El emulador escribe la estructura con `fst_bytesalloc = 0`
   en el camino del error, como XNU.
2. **`fst_bytesalloc` se pone a cero antes de reservar**: si la reserva falla, el valor que ve el
   programa es 0.
3. El `copyout` ocurre después de `VNOP_ALLOCATE`, así que el error que recibe el programa es el de
   la reserva (ENOSPC, EFBIG…), no un `EFAULT` del `copyout`.

### Errores exactos: `bsd/man/man2/fcntl.2` (sección *ERRORS*)

El manual de `fcntl(2)` de XNU enumera el error de cada caso, y es la referencia que usé:

- **EACCES** — «the argument `cmd` is `F_SETSIZE` and the calling process does not have root privileges».
- **EBADF** — «the argument `cmd` is `F_PREALLOCATE` and the calling process does not have file write permission».
- **EFBIG** — «`F_PREALLOCATE`, `F_PEOFPOSMODE` is set and preallocating `fst_length` bytes would exceed the maximum file size».
- **EINVAL** — «`F_PREALLOCATE` and the `fst_posmode` is not a valid mode, or when `F_PEOFPOSMODE` is set and `fst_offset` is a non-zero value, or when `F_VOLPOSMODE` is set and `fst_offset` is a negative or zero value».
- **EINVAL** — «`F_PUNCHHOLE` and either `fp_offset` or `fp_length` are negative, or when **both** `fp_offset` and `fp_length` are not multiples of the file system block size, or when either `fp_flags` or `reserved` is non-zero value».
- **ENOSPC** — no hay sitio en el volumen (o `F_ALLOCATEALL` y no cabe todo).

El manual también dice de `F_SETSIZE`: «**Deprecated.** In previous releases, this would allow a
process with root privileges to truncate a file without zeroing space. For security reasons, this
operation is no longer supported and will instead truncate the file in the same manner as
`truncate(2)». De ahí el comentario del código: se trunca como `truncate(2)`.

### `F_SETSIZE` — `bsd/kern/kern_descrip.c:3619-3682`

`DTYPE_VNODE` → EBADF · sin `FWRITE` → EBADF · `copyin` del `off_t` → EFAULT ·
`vnode_isappendonly` → **EPERM** · **sin root → EACCES** · y con root,
`vnode_setsize(vp, offset, IO_NOZEROFILL)`.

El `EPERM` del fichero de solo-anexar sale de `vnode_isappendonly()`
(`bsd/vfs/vfs_subr.c:13503`), que se activa con las banderas `UF_APPEND` (0x4) o `SF_APPEND`
(0x40000) del `st_flags` (`bsd/sys/stat.h:474` y `:501`). El emulador guarda ese `st_flags` en el
xattr `user.tahoe.flags`, así que la comprobación es directa.

Ojo con una confusión fácil: **`F_SETSIZE` y `ftruncate(2)` no son lo mismo** en XNU.
`ftruncate(2)` (`bsd/vfs/vfs_syscalls.c:8840`) da **EINVAL** si no hay `FWRITE` y **no** exige root;
`F_SETSIZE` da **EBADF** y **exige root** (`EACCES`). El emulador los mantiene distintos.

### `F_PUNCHHOLE` — `bsd/kern/kern_descrip.c:3463-3500`

`DTYPE_VNODE` → EBADF · sin `FWRITE` → **EPERM** (aquí es EPERM, no EBADF como en `F_PREALLOCATE`) ·
`copyin` de la `fpunchhole_t` → EFAULT · y el resto lo hace el fs (`VNOP_IOCTL`).

### `F_RDADVISE` — `bsd/kern/kern_descrip.c:3786-3810`

`DTYPE_VNODE` → EBADF · `copyin` → EFAULT · `ra_offset < 0` o `ra_count < 0` → EINVAL · y luego se
pasa al fs, que solo hace una lectura preventiva (no copia nada al usuario): se acepta con 0.

### Estructuras — `bsd/sys/fcntl.h`

- `fstore_t`: `u32 fst_flags; i32 fst_posmode; off_t fst_offset; off_t fst_length; off_t fst_bytesalloc` → **32 bytes**.
- `fpunchhole_t`: `u32 fp_flags; u32 reserved; off_t fp_offset; off_t fp_length` → **24 bytes**.
- `struct radvisory`: `off_t ra_offset; int ra_count` → **12 bytes en el núcleo**, porque en
  `bsd/sys/fcntl.h:493-505` está dentro de un `#pragma pack(4)` (sólo en compilación de núcleo).
  El `copyin` de XNU copia `sizeof(radvisory)` = 12, que es lo que hace el emulador.

### `ENOTSUP` para lo que el fs no implementa

`bsd/vfs/vfs_init.c:225-241`: las operaciones de vnode que un sistema de ficheros no implementa se
sustituyen por la operación por defecto, que devuelve **ENOTSUP**. De ahí que `F_PREALLOCATE` y
`F_PUNCHHOLE` sobre un directorio o un dispositivo devuelvan `ENOTSUP` y no el `EISDIR`/`ESPIPE` que
daría Linux.

---

## 3. Detalles de la traducción a Linux

- **`F_ALLOCATEALL`** → `fallocate()` reserva el rango entero o falla: es «todo o nada», como pide la
  bandera.
- **`F_ALLOCATECONTIG`** → **se ignora**. Linux no puede garantizar la contigüidad (la decide el fs
  y no hay una operación que la fuerce de forma fiable). El propio manual de `fcntl(2)` de XNU lo
  permite: «Note that the file system may ignore this request».
- **`F_ALLOCATEPERSIST`** → satisfecho de serie: en Linux los bloques reservados no se sueltan al
  cerrar el fichero.
- **`F_PEOFPOSMODE`** → se reserva desde el EOF (`st_size`). XNU reserva desde el «physical EOF»:
  aquí solo se conoce el EOF lógico, que es lo que usa `vp->v_size`.
- **`F_VOLPOSMODE`** → **aproximación**: XNU reserva en una posición *del volumen*, que en Linux no
  existe; se usa ese desplazamiento dentro del fichero. Queda anotado en el código y aquí.
- **`fst_bytesalloc`** → los bytes que cubren los bloques del rango `[base, base+len)` redondeados
  al bloque del fs. El manual dice que puede ser «the size requested, larger than the size
  requested, or (if the `F_ALLOCATEALL` flag is not provided) smaller than the space requested».
- **Alineación de `F_PUNCHHOLE`** → XNU sólo da `EINVAL` si **los dos** (`offset` y longitud) no son
  múltiplos del bloque; si sólo uno lo es, el fs redondea. `fallocate()` de Linux trabaja con bloques
  enteros (en ext4 exige que los dos lo sean), así que el rango se redondea antes de la llamada.
  El rango se recorta al final del fichero, porque el agujero no puede pasar de él («This will not
  change the actual file size»).

---

## 4. Qué compiló

| Qué | Cómo | Resultado |
|---|---|---|
| `tahoe-run` (C) | `clang 23.1.3 -O1 -g -Wall -Wextra -Wno-unused-parameter -static`, con `nice -n 19 ionice -c3` | **OK, sin avisos nuevos**. El único aviso es el preexistente de `ld` sobre `getpwuid` en binarios enlazados estáticamente (`tahoe_run.c:230`), ajeno a este cambio. |
| `nucleo` (Rust) | `cargo build --release` (`-j1`), sin cambios en el código | sin cambios; la biblioteca estática ya estaba compilada. |
| Binario | `tahoe-run/tahoe-run` reconstruido | el `.text` coincide byte a byte con una compilación independiente hecha por mí desde el mismo fuente (comparado con `objcopy --only-section=.text`). |

El enlace estático tarda varios minutos con los otros cuatro agentes trabajando; se compiló en
segundo plano y con `nice`/`ionice` para no molestar.

## 4.b. Lo que comprobé del lado de Linux

Con un programa propio (en `/tmp`, fuera del repositorio) medí el comportamiento real de
`fallocate(2)` en el sistema de ficheros de la máquina (tmpfs, bloque 4096):

| Comprobación | Resultado medido |
|---|---|
| `fallocate(KEEP_SIZE, 0, 4096)` | OK; **el tamaño no cambia** y se reservan bloques |
| `fallocate(KEEP_SIZE, 4, 4096)` (origen desalineado) | OK; reserva **8192 bytes** (los bloques 0 y 1), no 4096 → el nuevo cálculo de `fst_bytesalloc` coincide con lo reservado de verdad; el cálculo anterior (redondear sólo la longitud) devolvía 4096 |
| `PUNCH_HOLE` con offset o longitud desalineados | en tmpfs se acepta; en ext4 se exige que los dos sean múltiplos del bloque → de ahí que el rango se alinee antes de la llamada |
| `PUNCH_HOLE` más allá del final | sin error; el recorte al final del fichero evita el `EINVAL` de Linux |
| `fallocate(..., 0, 0)` | `EINVAL` → por eso el caso `fst_length == 0` no llama a `fallocate` |
| `fallocate` sobre un fd de sólo lectura | `EBADF` → la comprobación de `FWRITE` de XNU da lo mismo |
| `fallocate` sobre un directorio | `EBADF` en Linux → aquí se devuelve `ENOTSUP`, que es lo que dice XNU para una operación de vnode que no existe |

---

## 5. Qué **NO** pude verificar (con honestidad)

1. **No ejecuté `tahoe-run` ni la batería** (prohibido por las reglas de la tarea, y la máquina tiene
   5,8 GB de RAM con otros cuatro agentes). Por tanto:
   - **No comprobé que las preferencias se guarden ya con bytes.** El síntoma de «0 bytes» del punto 7
     sigue **sin verificar**.
   - **No verifiqué la hipótesis causal** del punto 7 («¿`cfprefsd`? `fcntl F_PREALLOCATE` no está
     implementado»). Implementar el comando quita el `EINVAL` de `fcntl`, pero **no demuestra** que ésa
     fuera la causa del 0 bytes: puede haber otro motivo (por ejemplo, que el programa que escribe no
     llegue a la escritura si la reserva falla, u otro `fcntl` que falte). Es la hipótesis más
     plausible que tenía el proyecto, pero sigue siendo una hipótesis.
   - **No probé** el camino de `F_PUNCHHOLE`, `F_SETSIZE` ni `F_RDADVISE` con un programa real de macOS.
2. **No pude comprobar el valor exacto de `fst_bytesalloc` que devuelve APFS**: APFS es de código
   cerrado y no está en el árbol de XNU (el `VNOP_ALLOCATE` sólo se ve en `bsd/vfs/kpi_vfs.c:5880`).
   El cálculo que hace el emulador (los bloques que cubren el rango) es una aproximación razonable y
   está medida contra `fallocate()`, pero no es el número que daría APFS.
3. **`F_ALLOCATECONTIG` no se garantiza** (ver arriba) y **`F_VOLPOSMODE` es una aproximación** (ver
   arriba).
4. **`F_SETSIZE` y el caso `EACCES`**: en el emulador los procesos corren como root por defecto
   (`cred_euid() == 0`) hasta que `launchd` les asigna el uid de su plist, así que el `EACCES` de XNU
   sólo se ve en los servicios con uid propio. El camino está implementado pero no se ha podido
   exercitar.
5. **Nada se ha comprobado en un Mac real** ni contra APFS/HFS+.
6. **Fuera del alcance, pero lo dejo apuntado**: el syscall `truncate(2)`/`ftruncate(2)` del emulador
   (`tahoe-run/emu_fs2.c:56` y `:75`) **no** comprueba `FWRITE` (XNU da `EINVAL`) ni el flag de
   sólo-anexar (XNU da `EPERM`), a diferencia de `F_SETSIZE`, que sí los comprueba. No lo he tocado
   porque queda fuera de la tarea; lo dejo para quien siga.

---

## 6. Nota sobre el estado del repositorio

Al empezar, la copia de trabajo ya traía una versión **incompleta** del trabajo anterior (el intento
interrumpido por el fallo de conexión): `emu_fs.c` con `F_PREALLOCATE`, `F_SETSIZE` y `F_RDADVISE`
en borrador, sin `F_PUNCHHOLE` comprobado y con la definición de `fd_is_vnode` que trataba un FIFO
de POSIX como si fuera una tubería. **Se conservó ese trabajo** y se avanzó sobre él:

- `fd_is_vnode` ahora distingue la tubería de Linux (que en XNU es `DTYPE_PIPE`, no un vnode) del
  FIFO de POSIX (que en XNU **sí** es un vnode, `vfifo`) leyendo el enlace de `/proc/self/fd/N`.
  Antes los dos devolvían `EBADF`.
- `fst_bytesalloc` devuelve los bytes que cubren el rango completo, no sólo la longitud redondeada
  (medido: con origen desalineado, la diferencia es el doble).
- `F_PREALLOCATE` y `F_PUNCHHOLE` sobre un directorio o un dispositivo devuelven `ENOTSUP` (la
  operación por defecto de un vnode en XNU) en lugar de `EINVAL`/`EISDIR` de Linux.
- `F_PUNCHHOLE` alinea el rango a los bloques del fs (XNU sólo falla si **los dos** están
  desalineados) y lo recorta al final del fichero.
- `F_PUNCHHOLE` gana las comprobaciones que le faltan: `DTYPE_VNODE` (EBADF), `FWRITE` (**EPERM**),
  `fp_flags`/`reserved` no nulos, offset o longitud negativos y alineación.

No se tocó nada más del repositorio: ni `README.md`, ni el núcleo en Rust, ni los scripts de `tools/`.
