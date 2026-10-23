# Informe 6 — `fcntl F_PREALLOCATE` (y `F_SETSIZE`; revisión de `F_PUNCHHOLE`)

**Tarea:** Pendientes.md, punto 7 — *«Muchas preferencias de `/Library/Preferences` y de root se guardan vacías (0 bytes): loginwindow, dock, CoreGraphics... (¿cfprefsd? `fcntl F_PREALLOCATE` no está implementado»*.

## Qué cambié

Solo `tahoe-run/emu_fs.c`, en `bsd_fcntl` (syscall 92). 61 líneas añadidas, ninguna quitada.

1. **`F_PREALLOCATE` (42), nuevo.** Lee `fstore_t` del tercer argumento (32 bytes: `fst_flags`, `fst_posmode`, `fst_offset`, `fst_length`, `fst_bytesalloc`) y lo traduce a `fallocate(fd, FALLOC_FL_KEEP_SIZE, peof, len)`:
   - `EBADF` si el descriptor no vale o no está abierto para escritura (XNU exige FWRITE; comprueba antes del copyin).
   - `EINVAL` si `fst_posmode` no es 3 (`F_PEOFPOSMODE`, que exige offset 0) ni 4 (`F_VOLPOSMODE`, offset > 0); esa validación va **antes** del «VFS» y no copia la estructura de vuelta, igual que `kern_descrip.c`.
   - PEOF = `max(st_size, st_blocks·512)`: los bloques ya asignados, como `ff_blocks` de HFS+ (el bloque parcial ya cuenta). Los huecos no existen en HFS+; con huecos se toma el tamaño, que es lo que falta por garantizar.
   - `F_PEOFPOSMODE`: `fst_length` es **adicional** al PEOF (hfs: `length += filebytes`).
   - `F_VOLPOSMODE`: `fst_length` es el **total deseado**; pedir menos de lo ya asignado es `EINVAL` (hfs). El offset es solo una pista de colocación de bloques en HFS+: se ignora (Linux no permite elegirla).
   - `fst_length < 0` → `EINVAL` (`hfs_vnop_allocate`); `len == 0` → éxito sin tocar nada (HFS+ lo trata como no-op; además `fallocate` con len 0 devuelve EINVAL, comprobado).
   - Éxito: retorno 0 y `fst_bytesalloc = fst_length`. La estructura se copia de vuelta **siempre** que se llega al «VFS» (aunque la reserva falle, como el copyout incondicional de `kern_descrip.c`), y el error de la reserva prevalece sobre el del copyout.
   - `EOPNOTSUPP` de fallocate → `ENOTSUP` (45), como `err_allocate`, el VNOP por omisión de XNU. `ENOSPC` (28) y `EFBIG` (27) pasan tal cual; XNU documenta ambos para `F_PREALLOCATE`.
2. **`F_SETSIZE` (43), nuevo.** `EBADF` sin FWRITE, `EFAULT` si no se puede leer el `off_t`, `EACCES` si la uid emulada (`cred_euid`) no es 0, y con root `ftruncate(fd, size)`. (XNU: igual orden y errores; con root, `vnode_setsize`.)
3. **`F_PUNCHHOLE` (99), revisado.** Ya existía (`fallocate` PUNCH_HOLE|KEEP_SIZE, correcto). Añadido el chequeo de XNU que faltaba: sin FWRITE → `EPERM` (`kern_descrip.c` lo comprueba antes de leer la estructura; Linux habría dado `EBADF`).

## Por qué (referencia XNU)

- `bsd/kern/kern_descrip.c`, casos `F_PREALLOCATE`, `F_SETSIZE` y `F_PUNCHHOLE` de `sys_fcntl_nocancel`: orden de comprobaciones (tipo de descriptor, FWRITE, copyin, posmode), copyout incondicional tras el VNOP con prevalencia del error del VNOP, y errnos exactos (EBADF/EPERM/EACCES/EINVAL).
- `bsd/sys/fcntl.h`: `fstore_t` (disposición en memoria de 32 bytes), flags `F_ALLOCATE{CONTIG,ALL,PERSIST}`, posmodes `F_PEOFPOSMODE`=3 y `F_VOLPOSMODE`=4.
- `bsd/sys/vnode_if.h` (documentación de `VNOP_ALLOCATE`): reserva espacio «sin usarlo» — *«if it grows, then the ubc size is unchanged»* — y `bytesallocated` es lo **añadido** (0 si nada o si el archivo se contrajo). De ahí `FALLOC_FL_KEEP_SIZE`: el tamaño lógico no se toca.
- `hfs_vnop_allocate` (proyecto `hfs` de apple-oss-distributions, `core/hfs_readwrite.c`): la única implementación abierta de `VNOP_ALLOCATE` (APFS es cerrado; msdosfs no la tiene). De ella se toma: PEOF = bloques asignados; en PEOFPOSMODE `fst_length` se suma al PEOF; en VOLPOSMODE es el total y no puede encoger (EINVAL); `F_ALLOCATEALL` sin espacio → ENOSPC sin reservar nada (fallocate también es «todo o nada», coincide); `F_ALLOCATECONTIG` y las pistas de colocación; el informe de `bytesalloc` redondeado al bloque.
- Para la escritura atómica de preferencias esto es lo que importa: los bloques quedan reservados, escribir el plist no puede fallar a mitad por falta de espacio y el archivo no se llena de ceros de relleno (el tamaño lógico no se mueve).

## Desviaciones conscientes respecto de XNU/HFS (también comentadas en el código)

- `F_ALLOCATECONTIG`: Linux no puede prometer contigüidad; se reserva igual, quizá no contiguo. Los llamadores de macOS reintentan sin CONTIG cuando falla; no pueden exigir el fallo.
- `F_ALLOCATEPERSIST`: lo reservado con fallocate no se libera al cerrar (HFS+ libera lo no escrito); queda como bloques asignados de más. Despreciable para quien escribe todo lo que reserva (el caso de cfprefsd).
- Sin `F_ALLOCATEALL` y con el disco lleno, HFS+ reserva parte e informa lo parcial; fallocate es «todo o nada», así que devolvemos ENOSPC con `fst_bytesalloc = 0`.
- `fst_bytesalloc` informa exactamente lo pedido (HFS+ redondearía al bloque). La página man de macOS admite «the same size or larger»: nunca informamos de menos de lo pedido, y no arriesga a un llamador que usara el valor para truncar o escribir.
- `F_SETSIZE`: el `EPERM` de XNU con SF_APPEND no se aplica; las banderas BSD solo se guardan como atributo extendido y no se hacen cumplir en ninguna otra ruta de escritura (no vimos motivo de empezar aquí).
- Descriptores que no son archivo (tuberías, sockets): XNU da EBADF antes del VFS; aquí el error llega de fallocate (ESPIPE/ENOTSUP...). Ningún llamador real de estos comandos pasa por ahí.

## Qué compiló

- `cargo build --release -j1` de `tahoe-run/nucleo`: bien (22 s, biblioteca estática para el enlace).
- El enlazado completo de `build.sh` (clang -O1 -g -Wall -Wextra -static, `nice -n 19 ionice -c3`): **exit 0**. `emu_fs.c` compila solo sin ningún aviso con `-Wall -Wextra`. El único aviso del enlace es el preexistente de glibc por `getpwuid` en binarios estáticos, ajeno a este cambio.
- El binario `tahoe-run/tahoe-run` quedó regenerado (está en `.gitignore`; no forma parte del diff).

## Qué NO pude verificar

- **Nada se probó en ejecución**: está prohibido arrancar tahoe-run, m.sh, t.sh o la batería (RAM compartida con otros agentes). En particular **no he comprobado que las preferencias dejen de guardarse a 0 bytes**: es la hipótesis del punto 7, pero confirmarla exige un arranque del macOS completo y mirar `/Library/Preferences` con contenido. La batería A/B (`tools/diag/bateria.sh`) queda pendiente para quien pueda ejecutarla.
- **Quién llama a `F_PREALLOCATE`**: el `cfprefsd` de Tahoe es un stub de 29 KB (su lógica y la de CFPreferences viven en la `dyld_shared_cache`, cerrada; lo comprobé descifrando el binario con `tools/diag/dsmos.py`, solo lectura, y desensamblando). No pude confirmar el punto exacto de la llamada ni cómo reaccionaba al `EINVAL` anterior. La implementación se apoya en la semántica de XNU/HFS, que es el criterio del proyecto.
- **Prueba aislada de la traducción** (programa propio en `/tmp/opencode`, sin tocar el emulador): confirmó en este Linux que `fallocate(KEEP_SIZE)` no mueve `st_size`, que reservas sucesivas **acumulan** (por eso el PEOF se calcula con `st_blocks`, no con el tamaño), que escribir dentro de lo reservado no necesita bloques nuevos y que `fallocate` con len 0 da EINVAL. Eso valida la traducción, no el código del emulador en marcha.
- `F_VOLPOSMODE` con pistas de colocación solo tiene sentido en HFS+; la ruta está implementada según `hfs_vnop_allocate` pero no hay forma real de ejercitarla aquí.
