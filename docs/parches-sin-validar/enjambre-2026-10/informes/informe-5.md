# Informe de la tarea 5 (Pendientes, punto 12 «Para lo último»)

**Qué:** `diskutil list` ponía el tamaño del contenedor (63,1 GB) en todos los volúmenes y `diskutil apfs list` decía
0 B consumidos en los volúmenes de datos y Preboot.

**Cómo queda:** el parche está en `../parche-5.diff` (aplica sobre `main`). **No he arrancado macOS ni `tahoe-run`**
(las reglas lo prohíben): lo que he hecho es compilar, probar la aritmética del reparto con un test propio y llamar al
código real del emulador (`emu_fs.c`) sobre el macOS completo de la PC, sin arrancar el sistema (ver §5).

> **Aviso sobre el estado del repositorio.** Al empezar, esta copia ya traía una implementación de la tarea (el intento
> anterior, interrumpido): `tahoe-run/tahoe.h`, `emu_fs.c`, `emu_iokit.inc`, `tools/volsize.sh`, `test/vol_quotas.c` y las
> notas de `README.md`/`Pendientes.md`. **La he conservado y la he revisado** (§4: encontré tres fallos reales y los he
> corregido). Durante el trabajo, además, **otro proceso estaba editando los mismos archivos a la vez** (cambios entre las
> 22:00 y las 22:25, con su propio `../informe-5.md` y `../parche-5.diff`): el estado que se entrega es el merged de las dos
>(one revisión). Si al aplicar el parche se ven partes que no reconoces, son de ahí.

---

## 1. Dónde se fabricaban las cifras (estado antes del parche)

Las tres superficies que nombra la tarea, y una cuarta que también fabricaba números de disco:

| Superficie | Dónde (en el código original) | Qué ponía |
|---|---|---|
| Propiedades IOKit de tamaño | `tahoe-run/emu_iokit.inc:330-336` (`disk_media`) y `io_disks()` (338-400) | la propiedad `Size` de **todos** los medios (`disk0`, `disk0s1`, `disk1`, `disk1s1`, `disk1s2`, `disk1s3`) era el tamaño del disco de Linux: de ahí el «63,1 GB» en cada volumen |
| Espacio APFS del contenedor | `tahoe-run/emu_iokit.inc:1365-1402` (`uc_apfs_container`, selector 8 = `APFSExtendedSpaceInfo`) | `fs_used` = el usado de todo el disco, solo para `disk1s1`, y **0** para `disk1s2` y `disk1s3` (de ahí el «0 B consumidos»); `fs_quota` = 0 en todos |
| `statfs` | `tahoe-run/emu_fs.c`, `put_statfs()` (antes 706-725), usada por `bsd_statfs64`, `bsd_fstatfs64` y `getfsstat64` | los bloques del disco de Linux para todos los montajes (el contenedor entero en cada volumen) |
| `getattrlist` (atributos de volumen) | `tahoe-run/emu_fs.c`, bloque de atributos de volumen (antes ~1003-1040) | `ATTR_VOL_SIZE`, `ATTR_VOL_SPACEFREE`, `ATTR_VOL_SPACEAVAIL` y `ATTR_VOL_SPACEUSED` del disco de Linux; `ATTR_VOL_QUOTA_SIZE` = 0 |

`emu_sysctl.c` **no** fabrica tamaños de volumen: solo da el UUID del Preboot (`kern.apfsprebootuuid`,
`kern.bootuuid`) y los números de tipo de vfs (`apfs` = 0x18, que sí usa `getattrlist` para `ATTR_VOL_FSTYPE`). No se toca.

## 2. El modelo

* El **contenedor** APFS que ve macOS es el sistema de archivos del anfitrión donde vive `TAHOE_ROOT` (el disco virtual
  ext4 con el macOS instalado). Su capacidad y su usado salen de `statfs`: `f_blocks × f_bsize` y
  `(f_blocks − f_bfree) × f_bsize`. Igual que antes.
* Los **volúmenes** del contenedor son carpetas de ese mismo sistema de archivos: el del sistema (`TAHOE_ROOT`), el de datos
  (`TAHOE_ROOT/System/Volumes/Data`) y el Preboot (`TAHOE_ROOT/System/Volumes/Preboot`).
* El **usado de cada volumen** lo mide `tools/volsize.sh` (`du` sobre el árbol de cada volumen) y queda en un archivo
  (`~/.config/tahoe-run/volsize`, o donde diga `TAHOE_VOLFILE`). El del sistema es lo que queda del disco.
* La **capacidad de cada volumen** es una **cuota**: su parte de la capacidad del contenedor, en proporción a lo que ocupa.
  Así cada volumen tiene su propio tamaño (que es lo que pedía la tarea) y las cifras cuadran: la suma de las cuotas es la
  capacidad del contenedor, la suma de los usados es el usado del contenedor, y ninguna cuota es menor que lo que ocupa su
  volumen (el libre nunca es negativo).
* Sin el archivo de tamaños (o sin los volúmenes de datos y Preboot, como en la Recovery), el comportamiento es **exactamente
  el de antes**: todo el usado del disco es del volumen del sistema y los volúmenes salen con la capacidad del contenedor.
* `TAHOE_VOL_QUOTA=0` quita las cuotas (todos los volúmenes con la capacidad del contenedor, como en un Mac donde no se
  pone ninguna) y deja solo el usado por volumen. Es la palanca para hacer la prueba A/B.

## 3. Los archivos

* `tahoe-run/tahoe.h:44-87` — tipos (`tahoe_vol`, `tahoe_volspace`), `tahoe_vol_space()` y el reparto de las cuotas
  (`tahoe_vol_quotas()`, función pura, con el ajuste a la baja si la medición no cuadra con el disco).
* `tahoe-run/emu_fs.c:117-205` — `tahoe_vol_space()`: lee `statfs`, lee el archivo de tamaños y reparte las cuotas.
* `tahoe-run/emu_fs.c:798-802` — `vol_of_mount()`: qué montaje es qué volumen (los tmpfs y devfs, ninguno).
* `tahoe-run/emu_fs.c:804-830` — `put_statfs()`: cada volumen da su capacidad y su libre.
* `tahoe-run/emu_fs.c:1112-1147` — atributos de volumen de `getattrlist`: `SIZE`, `SPACEFREE`, `SPACEAVAIL`, `SPACEUSED`
  y `QUOTA_SIZE` del volumen.
* `tahoe-run/emu_iokit.inc:338-345, 378, 390` — la propiedad `Size` de cada medio es la cuota de su volumen (el del
  contenedor y el del disco siguen siendo el tamaño del disco).
* `tahoe-run/emu_iokit.inc:1373-1392` — `APFSExtendedSpaceInfo`: `fs_used` y `fs_quota` de cada volumen.
* `tools/volsize.sh` (nuevo) — mide y escribe el archivo.
* `test/vol_quotas.c` (nuevo) — prueba de los invariantes del reparto.
* `Pendientes.md` y `README.md` — el punto 12 queda anotado y el cambio se documenta en la historia del README.

## 4. Lo que he encontrado y corregido en la implementación heredada

La revisión del código que ya estaba en la copia ha encontrado tres fallos reales (el primero, de ellos, visible):

1. **Sin archivo de tamaños, los volúmenes de datos y Preboot quedaban con cuota 0** (y su `Size` de IOKit, 0). Con el
   reparto proporcional, un volumen cuyo uso se conoce como 0 se lleva una cuota de 0: `diskutil list` habría pasado de
   «63,1 GB en todos» a «0 B en datos y Preboot». Ahora, si no hay medidas, **no hay cuotas**: cada volumen se queda con la
   capacidad del contenedor y `statfs`/`getattrlist` siguen con los del sistema de archivos, como antes.
   *(El campo `medido` de `tahoe_volspace` es lo que distingue los dos casos.)*
2. **Una medición vieja (mayor que el disco) dejaba al volumen del sistema con cuota ~0**: como el uso del sistema se deduce
   de lo que queda del disco, si la medición dice que datos y Preboot ocupan más que el disco entero, el sistema se quedaba
   sin nada. Ahora, si la medición **no cabe** en el disco, no se usa (se queda el comportamiento de antes); si cabe pero
   sobra, se ajusta a la baja en proporción y las cifras que se enseñan son las ajustadas (las sumas siguen cuadrando).
3. **La cuenta se cacheaba para siempre**: si el primer `statfs` de un proceso pasaba antes de que existieran los volúmenes
   de datos y Preboot (montados por el launchd al arrancar), ese proceso se quedaba con «un solo volumen» para siempre. Ahora
   la caché se rehace si cambia el número de volúmenes.

Además, el reparto (`tahoe_vol_quotas`) se ha dejado como una función pura y comprobable, con los invariantes escritos en
el propio código, y el «usado» que se enseña es el ya ajustado (nunca se enseña un uso mayor que su cuota).

## 5. Referencia de XNU (por qué así)

* `bsd/vfs/vfs_attrlist.c:1531-1546`: los atributos de volumen **salen del `statfs` del montaje**:
  `ATTR_VOL_SIZE = f_bsize × f_blocks`, `ATTR_VOL_SPACEFREE = f_bsize × f_bfree`,
  `ATTR_VOL_SPACEAVAIL = f_bsize × f_bavail`, `ATTR_VOL_SPACEUSED = f_bsize × f_bused` (y `f_bused`, si el sistema de
  archivos no lo da, es `f_blocks − f_bfree`: `vfs_attrlist.c:1145-1147`). `ATTR_VOL_QUOTA_SIZE` sale de `f_quota`
  (`vfs_attrlist.c:429-430`). O sea: los atributos de un volumen tienen que ser las cifras **de ese volumen**, no las del
  disco que lo contiene. Por eso `getattrlist` y `statfs` dan ahora lo mismo (y por eso se tocan los dos a la vez).
* `bsd/sys/mount.h:110-127` (`__DARWIN_STRUCT_STATFS64`): el `statfs` público no tiene `f_bused` ni `f_quota` → el usado se
  deduce de los bloques (como hace el emulador) y la cuota solo se ve por `getattrlist` (como en XNU).
* `bsd/sys/attr.h:494-523`: los bits de `ATTR_VOL_*` usados (`ATTR_VOL_VALIDMASK` = 0xF0FFFFFF).
* APFS: los volúmenes de un contenedor **comparten** el espacio del contenedor; la capacidad de cada uno es la del
  contenedor o su cuota, y el usado de cada uno es el de sus archivos. Como aquí el contenedor es un disco de Linux
  compartido por tres carpetas, se reparte la capacidad en cuotas proporcionales al contenido de cada volumen: es la manera
  de que cada volumen tenga un tamaño propio sin inventar cifras.

## 6. Qué he compilado y qué he comprobado

**Compila** (con `nice -n 19 ionice -c3`, un compilador cada vez):

* `cargo build --release -j1` del núcleo en Rust (`tahoe-run/nucleo`) ✓
* `clang -O1 -g -Wall -Wextra -Wno-unused-parameter` de **todo** `tahoe-run` (los `.c` más `nucleo/target/release/libnucleo.a`):
  sin avisos nuevos ✓ (solo el de siempre de `getpwuid` al enlazar estático)
* `test/vol_quotas.c` compila sin avisos y **pasa** ✓

```
$ clang -O1 -Wall -Wextra -o /tmp/vol_quotas test/vol_quotas.c -I tahoe-run && /tmp/vol_quotas
reparto de la capacidad del contenedor entre los volúmenes:
  medida normal                capacidad     61 GiB  cuotas: 45 / 15 / 0 GiB   ocupado: 29 / 10 / 0 GiB
  volúmenes vacíos           capacidad     61 GiB  cuotas: 61 / 0 / 0 GiB   ocupado: 40 / 0 / 0 GiB
  medida mayor que el disco    capacidad     61 GiB  cuotas: 38 / 22 / 0 GiB   ocupado: 12 / 7 / 0 GiB
  sin uso                      los tres volúmenes con el disco entero
todas las comprobaciones bien
```

**Comprobado** con un programa de prueba propio (fuera del repositorio, `/tmp/opencode/test_volspace.c`) que enlaza el
`emu_fs.c` real y llama a `tahoe_vol_space()` sobre el macOS completo de la PC, con los volúmenes montados en un
`unshare -Urm` (montajes `bind`, como hace el emulador al arrancar):

```
contenedor: 63090503680 bytes (36988329984 usados, 26102173696 libres), 3 volúmenes, medido=1
  sistema cuota  43364691968  usado  25422876672  libre  17941815296
  datos   cuota   9364832256  usado   5490642944  libre   3874189312
  preboot cuota  10360979456  usado   6074810368  libre   4286169088
  suma cuotas 63090503680 (contenedor 63090503680)  suma usados 36988329984 (contenedor 36988329984)
```

Las sumas cuadran **exactamente** con las del contenedor ✓. Y los tres casos de contorno:

* **sin el archivo de tamaños**: `medido=0`, el sistema con todo el disco y los otros con la capacidad del contenedor y 0 B
  → igual que antes ✓
* **con una medición vieja** (mayor que el disco): `medido=0` → igual que antes ✓ (antes: el sistema con cuota ~0)
* **con `TAHOE_VOL_QUOTA=0`**: todos los volúmenes con la capacidad del contenedor y su usado propio ✓

Las cifras de «usado» que he usado salen de medir a mano con `du` sobre el macOS completo de la PC: volumen de datos
**5.490.642.944 B**, Preboot **6.074.810.368 B** (el disco: 63.090.503.680 B de los que 36.988.329.984 B usados).

## 7. Lo que **no** he podido verificar

* **No he arrancado macOS**: no sé qué texto exacto imprimen `diskutil list` y `diskutil apfs list` con estos cambios. Las
  cifras llegan por el mismo camino que antes (la propiedad `Size` del IOMedia y el diccionario del selector 8), pero no lo he
  visto en pantalla. En particular, **no he comprobado que el framework APFS de Apple interprete `fs_quota` como la cuota
  del volumen** (si lo interpretara como «0 = sin cuota», el valor 0 del código original habría bastado y el cambio sería
  innecesario): es lo que hace que `diskutil apfs list` enseñe «Capacity Ceiling» y no el tamaño del contenedor.
* **`tools/volsize.sh` no lo he ejecutado**: escribe en `~/.config/tahoe-run/`, fuera del repositorio, y las reglas piden no
  tocar nada fuera del directorio de trabajo. Las cifras de §6 las medí a mano con `du` (con `du` salen algunos «Permiso
  denegado» en el volumen de datos: archivos que el usuario no puede leer → el tamaño medido es un pelín menor que el real).
* **El reparto en cuotas es una decisión de la emulación, no la de APFS**: en un Mac de verdad los volúmenes no tienen cuota
  y salen todos con el tamaño del contenedor (justo lo que se quejaba la tarea). Aquí se usa la cuota para que cada volumen
  tenga su tamaño propio; con `TAHOE_VOL_QUOTA=0` se vuelve al comportamiento fiel.
* **Las cuotas son proporcionales al uso, así que el volumen de datos no crece como en un Mac** (su cuota es su parte
  proporcional del contenedor: con 5,5 GB medidos se queda con 9,4 GB). Es la contrapartida de que todas las sumas cuadren
  exactamente. La alternativa «como en un Mac» (el sistema y el Preboot con cuota fija y el de datos con el resto) exige
  medir también el árbol del sistema (`du` sobre la raíz menos los volúmenes) y entonces la suma de los usados ya no cuadra
  con el usado del contenedor (el disco de Linux tiene además los archivos del usuario, que no están en ningún volumen):
  se puede hacer, pero cambia una coherencia por otra. Con `TAHOE_VOL_QUOTA=0` se quita el problema.
* **Con cuotas, `df /` pasa a mostrar 43,36 GB en vez de 63,09 GB** (la parte del contenedor que le toca al volumen del
  sistema). Es coherente con el modelo, pero es un cambio visible: también afecta a las comprobaciones de espacio de macOS y
  al instalador, y eso no lo he probado con el sistema encima.
* **La medición es un archivo estático**: envejece conforme crece el disco (habría que volver a medir con `volsize.sh`; el
  reparto se recalcula con el `statfs` de cada momento, pero el usado por volumen es el de la última medición).
* **No he probado** que `diskarbitrationd` siga anunciando bien los discos con la `Size` nueva, ni que `bless`, `apfs.util`
  o `fskitd` no se descuadren con capacidades por volumen.

## 8. Cómo se usa

```sh
tools/volsize.sh                                   # mide (du: del orden de diez minutos) y escribe ~/.config/tahoe-run/volsize
tools/macos-shell.sh --launchd -c 'diskutil list; diskutil apfs list'
TAHOE_VOL_QUOTA=0                                  # para comparar con el comportamiento de antes
```
