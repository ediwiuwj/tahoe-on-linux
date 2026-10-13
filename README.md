# tahoe-on-linux — macOS 26 (Tahoe) sobre el kernel de Linux, sin XNU

Capa de compatibilidad propia (`tahoe-run`, en C) que ejecuta binarios x86_64 de **macOS 26** directamente sobre Linux:
traduce las syscalls BSD y las trampas Mach de XNU en espacio de usuario, y arranca el `launchd` real de macOS.
Objetivo: el **macOS 26 completo** en la PC, más rápido que un Hackintosh: llegar al Asistente de configuración y al
escritorio en una ventana de Linux, con aceleración gráfica (Metal sobre Vulkan en la Intel HD 4400: Metal 2 nativo y
Metal 3 de cara a macOS, emulando lo que falte, ampliando `hasvk` de Mesa si hace falta) y con lo que el Hackintosh no
tenía, como Bluetooth (a través de BlueZ). La Recovery fue el primer paso y sigue sirviendo de prueba de regresiones.
[Darling](https://github.com/darlinghq/darling) se usa solo como referencia.

**Estado actual (13 de octubre de 2026)**, medido en la PC de pruebas (i5-4210U Haswell, 5,8 GB de RAM, Arch/Omarchy, kernel 7.2):

| Parte | Estado |
|---|---|
| Arranque | El `launchd` real de macOS 26 corre como **PID 1** en un espacio de nombres de PID de Linux, sin root. `launchctl list` registra **169 servicios**, en un arranque se cargan unos **155-170 procesos**, y es estable (sin reinicios). Funcionan los dominios de proceso de launchd (servicios XPC de los frameworks, como `authd`), las fases de montaje del arranque (`mount -P`) y las señales de `launchctl kill`/`kickstart -k`. Entre los servicios: `syslogd`, `notifyd`, `cfprefsd`, `configd`, `powerd`, `securityd`, `opendirectoryd`, `diskarbitrationd`, `storagekitd`, `fskitd`, `authd`, `trustd`, `coreservicesd`, `WindowServer`. |
| Shell de macOS | `tools/macos-shell.sh` abre el `bash` de macOS en la terminal. Con `--launchd` se une a launchd y funcionan las órdenes que usan servicios. En la **Recovery**, `diskutil list` funciona (4 discos, en menos de 40 s). En el **macOS completo**, inventario corto (`tools/shell_survey.sh`, 79 órdenes): **54 funcionan**, 18 dan error (6 de ellos igual que en un Mac real), 3 se cuelgan y 4 se caen; fallan sobre todo las que hablan con launchd y servicios del sistema (`launchctl print`, `defaults`, `diskutil`, `log show`, `caffeinate`) y las que leen la memoria del núcleo (`top`, `vm_stat`, `memory_pressure`). Inventario de **todos** los programas (1.167): `tools/inventario_total.sh`. |
| macOS completo | El **macOS 26.7.1 completo** (del instalador oficial, extraído con las herramientas de `tools/`) arranca: launchd carga **~400 servicios**, ~110 procesos, y se lanzan `WindowServer` y `loginwindow` (sistema recién instalado: toca el Asistente de configuración). `loginwindow` ya obtiene de opendirectoryd la lista de usuarios y **lanza Language Chooser** (la pantalla de idioma, la primera del Asistente), que se conecta a WindowServer y le pide ventanas, pero a veces no carga su interfaz (`MainMenu.nib`) y se cierra. Queda una fuga de memoria de 1-3 GB en algún proceso (el guardián ya dice cuál). Ver «macOS 26 completo» al final. |
| Interfaz gráfica | **Primera imagen** (macOS 26 completo): `WindowServer` abre el framebuffer emulado (`AppleIntelFramebuffer`, 1280×800, EDID propio), crea el cursor compartido, proyecta la memoria de vídeo (en `/dev/shm`) y **dibuja el cursor**, por software (sin dispositivo Metal: CoreDisplay registra «MetalDevice ... 0x0»). Hay IOSurface emulado (superficies compartidas entre procesos). Desde el 12 de octubre WindowServer **ya no se cae** (rwlock de libpthread y workloops de libdispatch implementados) ni crece en memoria (avisos «send-possible» como en XNU): se queda estable en ~95 MB, pero aún no compone el escritorio. Visor en Linux: `tools/tahoe-view` (`macos-shell.sh --pantalla`); captura: `tools/diag/captura.py`. |
| Metal | Un kernel de Metal (AIR) traducido a SPIR-V con `metal2vulkan` se ejecuta en la GPU (Intel HD 4400) por Vulkan (`test/run_add_arrays.c`). Aún no está conectado a `tahoe-run`. |

Primera imagen de macOS 26 en la PC de pruebas (11 de octubre de 2026): la memoria de vídeo emulada con el cursor que dibuja
WindowServer, arriba a la izquierda; el escritorio aún no se compone.

![Primera imagen: el cursor de macOS 26 dibujado por WindowServer en el framebuffer emulado](docs/capturas/2026-10-11-primer-cursor.png)

Qué funciona en la shell de la Recovery (con `--launchd`): `ls`, `cp`/`mv`/`rm`, `find`, `grep`, `sed`, `awk`, `sort`, `tar`, `vim`, `less`, `perl`, `curl`,
`ssh -V`, `ps aux` (estado, CPU, memoria, hora de inicio), `log show`, `df`, `mount`, `stat`, `xattr`, `chmod`/`chown`, `sysctl`,
`sw_vers`, `uname`, `id`, `date`, `ifconfig`, `netstat`, `ping`,
`scutil`, `launchctl list`/`print`, `defaults read`/`write`, `plutil`, `pmset`, `ioreg`, `nvram`, `csrutil`, `dscl`, `codesign`, `spctl`,
`caffeinate`, `installer`, `resetpassword`, `security list-keychains`, `diskutil list`, `diskutil info disk0`/`disk1s1`/`/`, `diskutil apfs list`,
`hdiutil info`, `bless --getBoot`, `ls -l /dev/disk*`, `launchctl kill`/`kickstart -k`.

Los binarios de Apple **no** se incluyen (hay que extraerlos de una Recovery propia). Licencia MIT para el código propio.

## Probarlo

```sh
tahoe-run/build.sh <ruta al código de xnu>          # compila tahoe-run (C, estático)
tools/macos-shell.sh                                 # bash de macOS, sin launchd
tools/macos-shell.sh --launchd                       # bash de macOS unido a launchd (lo arranca y lo detiene al salir)
tools/macos-shell.sh --launchd -c 'launchctl list'   # una orden
tools/macos-shell.sh --launchd --depurar             # modo depuración (ver abajo)
tools/shell_survey.sh                                # inventario de órdenes
```
`TAHOE_ROOT` indica dónde está el sistema de la Recovery extraído (por omisión `~/macos26/base/macOS Base System`). Las pruebas
de arranque usan un guardián de memoria (`tools/mem_guard.sh`): con poca RAM, cerrar programas grandes antes.

**Modo normal y modo depuración.** Sin opciones, el arranque es el de una Recovery real. Con `--depurar` se pone en la NVRAM
emulada `ROS_SLEEP_ON_FAILURE`, el ajuste de `RecoveryOS.framework` por el que recoveryosd, si algo le obliga a reiniciar, se
queda dormido en vez de ejecutar `/sbin/reboot` y apagar todo el arranque: sirve para seguir mirando cuando algo falla. Las
herramientas de `tools/diag/` lo usan por omisión (`TAHOE_DEPURAR=0` para el arranque normal).

Diagnóstico con `tools/diag/` (registro y salidas en `$TAHOE_DIAG_DIR`, por omisión `~/.cache/tahoe-diag`):

| Herramienta | Uso |
|---|---|
| `msh.sh '<órdenes>' [espera] [límite]` | Órdenes en la shell unida a launchd; siempre limpia. Variables: `STACKS=<proceso>`, `ALL=1`, `MACH=1`, `IOK=1`, `BRK='dir…'`, `PEEK='expr…'`, `TAHOE_RUN=<binario>`, `CONSOLA=1` (lo que se escribe en `/dev/console`: el registro de launchd) |
| `hang.sh '<orden>' <proceso> [s]` | Vuelca cada hilo de un proceso colgado, simbolizado; con `STACKS=<proceso>`, además la pila de macOS de cada hilo |
| `pendientes.py [-v] [registro]` | Con `MACH=1`: peticiones Mach que no recibieron respuesta (quién espera a quién), para seguir cadenas de bloqueos |
| `ab.sh <N> [binario]` | N arranques con recuento de procesos y caídas: comparar dos versiones antes de dar por bueno un cambio |
| `clean.sh` | Termina todo `tahoe-run` y limpia `/dev/shm` |
| `sym.py <dir>…` / `dis.py <dir> [bytes]` | Símbolo (con `ipsw`) o desensamblado de direcciones de la caché de dyld |
| `discos.sh ['orden']` | `diskutil` (u otra orden) en un arranque, con el registro `-d` de diskarbitrationd resumido por `da_log.py`; `MSGS=`/`IOKS=` trazan mensajes Mach e IOKit de esos programas |
| `mensajes.py [-x] [-p prog] <registro>` | Mensajes Mach trazados: quién habla con quién, qué no llegó, qué peticiones quedaron sin respuesta; `-x` decodifica XPC |
| `iokit.py <registro>` | Peticiones IOKit de cada proceso con sus respuestas y avisos |
| `memoria.sh <programa> [t1] [t2]` | Muestreo de la memoria de un proceso de macOS y comparación de dos fotos de `smaps` (`memoria.py`): qué región crece |
| `dsmos.py <macho> <salida>` | Descifra el `__TEXT` protegido de un binario de Apple (loginwindow, Finder...) para desensamblarlo, con la clave de la PC |
| `../inventario_total.sh` + `inventario.py` | Ejecuta todos los programas de la shell del macOS completo y los clasifica (funciona, pide argumentos, error del emulador, se cae, cuelga) |

**Antes de dar por bueno un cambio**, se pasa una batería fija y se compara con el binario anterior (`TAHOE_RUN`): `diskutil list`
en la Recovery (`discos.sh`), el inventario corto en el macOS completo dos veces (el arranque completo varía de una vez a otra),
la memoria de WindowServer (`memoria.sh`) y `cargo test --lib` en `tahoe-run/nucleo`. Si algo empeora, no se sube.


## Hoja de ruta, por orden

Objetivo: macOS 26 completo, configurado y usable, con aceleración gráfica. La Recovery queda como prueba de regresiones.

Criterio de lenguaje: todo lo que toca la parte gráfica de Linux y la capa Metal↔Vulkan va en **Rust**. `tahoe-run` sigue en
C, pero la parte que falle una y otra vez se reescribe en Rust (biblioteca estática enlazada en `tahoe-run`) en vez de
seguir parcheándola; lo de muy bajo nivel (señales, cambios de pila) se queda en C.

1. **Escritorio en pantalla (en curso)**: WindowServer ya dibuja el cursor y se mantiene estable; loginwindow lanza
   Language Chooser, que a veces no carga su nib. Falta que Language Chooser y luego el Asistente se dibujen, y que la
   conexión con launchd y los servicios del sistema sea fiable en el macOS completo (`diskutil`, `defaults`, `launchctl`).
   Visor: `tools/tahoe-view`.
2. **Teclado y ratón**: eventos de la ventana de Linux (SDL) hacia la cola de IOHIDSystem / los servicios HID de macOS.
3. **Asistente de configuración** hasta crear la cuenta; plan B si algún paso se bloquea: crear el usuario con
   `sysadminctl` desde la shell de macOS y marcar `/private/var/db/.AppleSetupDone`.
4. **Aceleración gráfica (en Rust)**: volver a publicar el IOAccelerator, esta vez con un driver Metal propio
   (`MetalPluginName`) sobre Vulkan; primero la composición de WindowServer y CoreAnimation (Liquid Glass: desenfoque,
   refracción, brillos), después Metal 2 completo y Metal 3 de cara a macOS, emulando lo que el hardware no tenga y
   añadiendo a `hasvk` (Mesa) las extensiones que falten. Todo lo de Metal y Vulkan se escribe en **Rust** (driver
   Metal, traducción de órdenes a Vulkan con `ash`, shaders AIR→SPIR-V como `metal2vulkan`): es código complejo de
   alto nivel donde la seguridad de memoria importa. `tahoe-run` (el «núcleo», muy de bajo nivel) sigue en C.
5. **Bluetooth, audio y red** sobre los servicios de Linux (BlueZ, PipeWire/ALSA, la red del anfitrión).
6. **Estabilidad del arranque**: la fuga de memoria de 1-3 GB del macOS completo, caídas sueltas de servicios
   (locationd en una excepción de C++, kmutil «failed to get platform node»), señales reales (`sigreturn`) y perfil de
   servicios prescindibles para la PC de 5,8 GB. Ya resueltas: AirPlayXPCHelper, mediaremoted y PerfPowerServices
   (workloops con atributos) y opendirectoryd (workloops que no se liberaban).

## Qué hay en este repositorio

| Ruta | Contenido |
|---|---|
| `tahoe-run/` | La capa de compatibilidad (C): syscalls BSD, trampas Mach, puertos entre procesos, kqueue/workloops, IOKit, XPC... |
| `tahoe-run/nucleo/` | Las partes del núcleo reescritas en Rust (biblioteca estática): descriptores de mensajes Mach, desbordamiento de mensajes grandes, `pthread_rwlock` (traducción de `kern_synch.c`) y el candado de la región de puertos entre procesos; con pruebas (`cargo test --lib`) |
| `tools/inventario_total.sh` | Ejecuta todos los programas de la shell del macOS completo y los clasifica con `tools/diag/inventario.py` |
| `patches/` | Parche para [`darlinghq/indium`](https://github.com/darlinghq/indium) (`git am`) |
| `tools/extract_air.py` | Saca los módulos AIR de un `.metallib` |
| `tools/compare_syscalls.py` | Compara las syscalls BSD y trampas Mach de un XNU con las de Darling |
| `tools/map_dyld_cache.py` | Mapea la `dyld_shared_cache` en sus direcciones fijas y lo verifica |
| `test/sud_syscall_dispatch.c` | Prueba de interceptación de syscalls de macOS con syscall user dispatch |
| `tools/build_darling_codespace.sh` | Compila `mldr` y `darlingserver` de Darling en un Codespace |
| `tools/used_syscalls.py` | Lista las syscalls BSD y trampas Mach que invoca un Mach-O x86_64 |
| `tools/dyld_iterate.py` | Ejecuta un binario de macOS 26 en Darling extrayendo de la caché lo que falte |
| `tools/narrow_int64.py` | Quita `Int64` de un SPIR-V cuando solo se usa para indexar (conservador) |
| `test/add_arrays.vulkan1.2.spv` | SPIR-V generado por `metal2vulkan` (aún con `Int64`) |
| `test/run_add_arrays.c` | Programa mínimo de Vulkan que ejecuta el kernel y comprueba `c = a + b` |
| `tools/macos-shell.sh` | Shell de macOS sobre `tahoe-run`; con `--launchd`, unida a launchd |
| `tools/shell_survey.sh` | Inventario: 79 órdenes típicas de macOS clasificadas en OK, ERR, COLGADA o SEÑAL |
| `tools/mem_guard.sh` | Guardián de memoria de las pruebas (RAM disponible y crecimiento del swap) |
| `tools/dmg2raw.py` | Convierte la imagen UDIF de la Recovery en imagen cruda (para extraerla con `hfsfuse`) |
| `tools/iokit_trace.py` | Resume la traza de IOKit |
| `tools/diag/` | Diagnóstico: `msh.sh`, `hang.sh`, `ab.sh`, `clean.sh`, `sym.py`, `dis.py`, `discos.sh`, `mensajes.py`, `iokit.py`, `memoria.sh`, `dsmos.py`, `inventario.py` (ver «Probarlo») |

### Estado de los parches

- Compilan con `clang` y LLVM 22. Aceptan dispositivos Vulkan 1.2 con `VK_KHR_synchronization2` y
  `VK_EXT_extended_dynamic_state{,2}`, activan esas extensiones y cargan las funciones de 1.3 por su
  nombre `KHR`/`EXT` (solo las promovidas en 1.3).
- Se probaron con `basic-compute` en la GPU descrita: el dispositivo se acepta, se crea y los
  semáforos funcionan. **No se probó ningún shader**, por el punto 4 de «Lo que se aprendió» (en el historial).
- Añaden `#include` que faltaban en Iridium (`<cstdint>`, `<stdexcept>`, `<string>`); esos errores ya
  existían sin los parches.

## Qué no se incluye, a propósito

Se descargó el sistema base de macOS 26.6.2 (recovery de Apple, vía `macrecovery.py` de OpenCorePkg)
para estudiar dónde viven los frameworks (dentro de `dyld_shared_cache`). Es software de Apple con
licencia restringida: **no se redistribuye aquí** y cualquier implementación debe ser propia.
Los fuentes abiertos de Apple (`xnu-12377.121.6`, `dyld-1378`, `objc4-951.7`,
`libdispatch-1542.100.32`) están en `github.com/apple-oss-distributions`.

## Licencias

- **`tahoe-run/`, notas, `tools/` y `test/`**: [MIT](LICENSE). Quien los use o redistribuya debe conservar el aviso de
  copyright y la licencia.
- **`patches/`**: trabajo derivado de [Indium](https://github.com/darlinghq/indium) y por tanto bajo su
  licencia (MPL-2.0), no bajo la MIT de arriba.
- [`metal2vulkan`](https://github.com/steelbrain/metal2vulkan) es LGPL-3.0-or-later; no se incluye
  ningún código suyo, solo un SPIR-V generado con él.

---

# Historial y detalle técnico

Registro en orden cronológico. Lo que ya no es cierto está marcado.

## Parte 1: Metal sobre Vulkan en una GPU antigua

## Entorno de pruebas

- Arch Linux (Omarchy), kernel 7.2.5, Mesa 26.2.2, LLVM 22.1.8
- GPU: Intel HD 4400 (Haswell GT2). `vulkaninfo`: API **1.2.354**, `timelineSemaphore`,
  `dynamicRendering`, `synchronization2`, `VK_EXT_extended_dynamic_state` 1 y 2 presentes;
  `shaderInt64 = false`, `shaderFloat64 = false`

## Lo que se aprendió

1. **Darling ya no usa módulo de kernel.** Desde 2022 emula Mach IPC y las syscalls de XNU en
   espacio de usuario con `darlingserver`. `darling-bin` de AUR funciona tal cual: `darling shell`
   arranca y reporta macOS 11.7 (Darwin 20.6). No es macOS 26.
2. **Darling ya incluye Metal sobre Vulkan** (`src/external/metal`): *Indium* (API de Metal sobre
   Vulkan) e *Iridium* (shaders AIR → SPIR-V). Se compila con `ENABLE_METAL`.
3. **Indium rechazaba esta GPU** porque exigía `apiVersion >= 1.3` aunque el dispositivo tiene todo
   lo necesario mediante extensiones. Ver `patches/`.
4. **Iridium se cae con LLVM moderno.** `air.cpp:428` usa `LLVMGetElementType(LLVMTypeOf(fn))`, que
   supone punteros tipados (eliminados en LLVM 17). Con LLVM 22 el programa de prueba
   `basic-compute` falla con SIGSEGV en `newLibrary`.
5. **[`steelbrain/metal2vulkan`](https://github.com/steelbrain/metal2vulkan)** (Rust, LGPL-3.0,
   alpha) traduce AIR a SPIR-V para Vulkan 1.2 y maneja punteros opacos. Con él, el kernel
   `add_arrays` del test de Indium produce SPIR-V que pasa `spirv-val --target-env vulkan1.2`
   (`test/add_arrays.vulkan1.2.spv`).
6. **Ese SPIR-V pide `OpCapability Int64`**, aunque esta GPU tiene `shaderInt64 = false`. El origen es
   solo el índice del array (`zext i32 → i64` en el `getelementptr`). `tools/narrow_int64.py` lo
   estrecha a 32 bits y retira la capacidad: el resultado pasa `spirv-val --target-env vulkan1.2`.
7. **Resultado en la GPU** (Intel HD 4400, Mesa 26.2.2, `test/run_add_arrays.c`, sin activar ninguna feature):

   | SPIR-V | Pipeline | Resultado |
   |---|---|---|
   | original, con `Int64` | se crea | 64/64 correctos |
   | índice de 32 bits, sin `Int64` | se crea | 64/64 correctos |

   Es decir, `hasvk` **acepta y ejecuta bien** el shader con `Int64` aunque la feature esté desactivada.
   Eso no lo hace válido: la especificación exige activar `shaderInt64`, la capa de validación lo
   marcaría y otro driver podría rechazarlo o comportarse distinto. Por eso se mantiene la versión
   de 32 bits como la correcta. Con este kernel (solo `i32` y `float`) no se puede saber si `hasvk`
   emula de verdad aritmética de 64 bits cuando existe; no se probó.

## Cómo repetir la prueba de traducción

```sh
# 1. compilar metal2vulkan (Rust >= 1.87) y tener llvm-dis y spirv-val
git clone https://github.com/steelbrain/metal2vulkan
(cd metal2vulkan && cargo build --release)   # binario en target/release/metal2vulkan

# 2. extraer AIR de un .metallib (p. ej. indium/test/basic-compute/add.metallib)
python3 tools/extract_air.py add.metallib out/

# 3. traducir y validar
metal2vulkan out/module0.air add.spv     # imprime: spirv-val vulkan1.2: PASS

# 4. (opcional) estrechar índices a 32 bits y ejecutar en la GPU
python3 tools/narrow_int64.py add.spv add32.spv && spirv-val --target-env vulkan1.2 add32.spv
clang -O1 -o run test/run_add_arrays.c -lvulkan && ./run add32.spv   # OK: 64/64 correctos
```
El programa de prueba asume la interfaz de este kernel (3 buffers en `set 0` y 48 bytes de push constants).

## Pasos siguientes de la parte de Metal (4 de octubre de 2026)

1. ~~Quitar el `Int64` y ejecutar el kernel en la GPU.~~ Hecho (puntos 6 y 7). Falta integrar el paso de
   estrechado en `metal2vulkan` como opción (`--no-int64`) en lugar de usarlo aparte.
2. Adaptar `Library::newLibrary` de Indium para usar `metal2vulkan` en lugar de Iridium (hay que
   traducir su reflexión de descriptores y push constants al `OutputInfo` que espera Indium).
3. Compilar Darling con `ENABLE_METAL` y probar el ejemplo `triangle`.
4. ~~Subir la base de macOS 11 a macOS 26.~~ Se descartó Darling como base: `tahoe-run` ejecuta el userland real de
   macOS 26 (ver «Estado actual»). Metal se conectará a `tahoe-run` cuando haya WindowServer.

## Ejecutar el userland de macOS 26 en Darling (medido el 4 de octubre de 2026)

Objetivo: que la interfaz del recovery de macOS 26 corra sobre el kernel de Linux, sin XNU.

| Prueba | Resultado |
|---|---|
| `bin/ls` de macOS 26 (SDK 26.6.1, `minos 10.14`) | **Funciona** en Darling: usa la parte estable de la ABI |
| `Language Chooser.app` (primera pantalla del recovery, `minos 26.6`, ~45 librerías) | Cae en la 8.ª: `libIASUnifiedProgress.dylib` (`No shared cache present`) |
| Las 7 anteriores (`AVFoundation`, `SkyLight`, `CoreWLAN`...) | Son **stubs de Darling**, no las de Apple |
| Extraer esa librería de la `dyld_shared_cache` con `ipsw` y ponerla en `DYLD_LIBRARY_PATH` | dyld la encuentra y la **rechaza** (`overlapping segments`) |

**Por qué el extractor no sirve para cargar**: las imágenes de la caché están enlazadas de antemano
a direcciones fijas de la caché. El dylib extraído conserva `LC_DYLD_INFO_ONLY`, pero sus tablas de
*rebase*, *bind* y *lazy bind* salen **vacías**, y los punteros de `__got` siguen codificados con la
información de *slide*. Sin esas tablas, ningún dyld sabe qué símbolo es cada puntero. Estas
herramientas (`ipsw`, `DyldExtractor`...) sirven para desensamblar, no para cargar. Probado con
`ipsw dyld extract` con y sin `--slide`.

**Consecuencia**: la ruta viable no es extraer librerías sueltas, sino que un dyld cargue la caché
completa, lo que exige implementar las syscalls de la región compartida (`shared_region_check_np`,
`shared_region_map_and_slide_np`) en `darlingserver`, usar el `dyld` y la `libSystem` de Tahoe y
reimplementar las syscalls y trampas Mach que XNU 12377 añadió respecto a Darwin 20.

### Distancia medida entre Darling (Darwin 20) y XNU 12377 (macOS 26)

`tools/compare_syscalls.py` compara las tablas (Darling emula las syscalls en espacio de usuario):

| Interfaz | XNU 26 | Darling | Falta |
|---|---|---|---|
| Syscalls BSD | 455 reales | 268 | **189** (156 con número antiguo, 33 posteriores al 524) |
| Trampas Mach | 60 reales | 52 | **9** |

- **Lo Mach está cerca.** Faltan 9, entre ellas `iokit_user_client_trap`,
  `host_create_mach_voucher_trap`, `mk_timer_arm_leeway_trap` y `task_dyld_process_info_notify_get_trap`;
  `_exclaves_ctl_trap` y `pfz_exit` dependen de hardware o de la zona de funciones privadas de Apple.
- **Lo BSD no.** Entre las 33 nuevas hay las que sí usan `libSystem` y `libdispatch` de Tahoe:
  `ulock_wait2` (544), `kqueue_workloop_ctl` (530), `preadv`/`pwritev` (540-543),
  `shared_region_map_and_slide_2_np` (536), `map_with_linking_np` (550), `mkfifoat`/`mknodat`,
  `os_fault_with_payload` (529) y `memorystatus_available_memory` (534). Muchas de las 156 antiguas
  (`mount`, `acct`, `quotactl`, `semsys`...) no las llaman las apps normales.
- **La región compartida es hoy un stub**: `sys_shared_region_check_np` devuelve `-EINVAL` ("no hay
  región compartida"). Las variantes `map_and_slide` no existen.
- **`mldr` tiene el `dyld` fijado en el código** (`INSTALL_PREFIX/libexec/usr/lib/dyld`). Para probar el
  `dyld` de Tahoe (Mach-O `MH_DYLINKER`, `minos 26.6`) hace falta compilar un `mldr` propio o sustituir
  un archivo del sistema. El `dyld` de Darling es de 2023 (`dyld2`/`dyld3`).
- Es una medida aproximada: una syscall puede resolverse por otra vía, y solo importan las que las
  librerías de Tahoe llaman de verdad (eso aún no está medido).

### Qué necesita de verdad el `dyld` de Tahoe

`tools/used_syscalls.py` busca las instrucciones `syscall` de un Mach-O x86_64 (`mov eax, imm32` + `syscall`;
la clase va en el byte alto: 1 Mach, 2 BSD, 3 machdep). Se valida contra `libsystem_kernel` de la caché,
que da 456 syscalls BSD frente a las 455 reales de XNU 26.

- El `dyld` de Tahoe enlaza 104 syscalls BSD y 61 trampas Mach. A Darling le faltan 18 BSD y 10 Mach.
- La mayoría son ruido de enlazado (`mount`, `fmount`, `graftdmg`, `kdebug_*`). Las que están en el camino
  de arranque son: `shared_region_map_and_slide_2_np` (536), `ulock_wait2` (544),
  `terminate_with_payload` (520), `kqueue_workloop_ctl` (530), `openbyid_np` (479),
  `os_fault_with_payload` (529), `proc_info_extended_id` (545) y `map_with_linking_np` (550, solo arm64e).
- Que un stub esté enlazado no significa que se ejecute: es una cota superior, no una medida dinámica.

### Dos mecanismos que un `mldr` para Tahoe necesita, validados en este kernel (7.2.5)

1. **Interceptar `syscall` en crudo.** El `dyld` y la `libSystem` de Tahoe llaman al kernel con
   `mov eax, 0x2000000|N; syscall`, y Darling no tiene `seccomp` ni `SIGSYS` en su cargador (funciona porque
   las bibliotecas que carga son las suyas). `PR_SET_SYSCALL_USER_DISPATCH` redirige esas llamadas a un
   manejador de `SIGSYS`, como hace Wine con las de Windows. `test/sud_syscall_dispatch.c` lo comprueba: una
   syscall BSD `0x2000014` y un número inexistente llegan al manejador con `si_syscall` completo y se
   emulan (`rax = 4242` y `-ENOSYS`). Detalle que cuesta encontrar: `rt_sigreturn` debe ejecutarse desde la
   región exenta; con el restaurador de `libc` el proceso muere por recursión (violación de segmento).
2. **Mapear la caché de librerías.** `tools/map_dyld_cache.py` lee los mapeos de la cabecera de cada archivo
   de la caché (8 en total, ~1 GB, desde `0x7ff800000000`) y los mapea con `MAP_FIXED_NOREPLACE`:
   `__TEXT` ejecutable, `__DATA` privado y escribible (copy-on-write, el archivo no cambia). Sin slide.

**Lo que NO está resuelto y es lo grande**: dónde vive la emulación. Las syscalls de Darling están
implementadas como código Mach-O dentro de su `libsystem_kernel.dylib`, y con el `dyld` y la `libSystem` de
Tahoe esa biblioteca no se carga (la de la caché la sustituye). Hay que decidir entre (A) recompilar esa
emulación como ELF dentro de `mldr`, (B) cargar la emulación como una imagen Mach-O adicional desde `mldr`, o
(C) escribir una emulación nueva solo para lo que Tahoe llama. Y además `commpage` con el diseño de macOS 26,
las 18 syscalls y 10 trampas Mach que faltan y `SkyLight`.

### `tahoe-run`: programas reales de macOS 26 ejecutándose sobre Linux

Código propio en C (`tahoe-run/`), sin código de Darling. Hace de "kernel" para un programa de Tahoe:
mapea la caché de librerías (con el *slide* v2 aplicado y la región dinámica), fabrica el `commpage`,
carga el `dyld` y el programa, monta la pila `[mh][argc][argv][envp][apple]` e intercepta las syscalls con
`PR_SET_SYSCALL_USER_DISPATCH`. Las syscalls BSD, las trampas Mach y los mensajes `mach_msg2` al kernel se
emulan en módulos (`emu_fs.c`, `emu_sysctl.c`, `emu_mach.c`, `emu_proc.c`); todo lo que falta se registra
con su nombre.

**Resultado (4 de octubre de 2026): el `ls` de macOS 26 lista la raíz del recovery sobre Linux y sale con
código 0.** Es el binario auténtico de Apple con su `dyld`, su `libSystem`, `libpthread` y `libobjc`
sacados de la `dyld_shared_cache`, sin XNU ni Darling:

```
$ TAHOE_ROOT=<recovery extraído> ./tahoe-run <cache> <root>/usr/lib/dyld <root>/bin/ls /
Applications  Install macOS Tahoe.app  Library  System  Users  Volumes  [HFS+ Private Data]  bin  cores  dev ...
```

Qué hizo falta, por orden (cada paso lo señaló el registro de syscalls o un fallo):
`sysctl` (incluido `kern.bootargs`), `__mac_syscall`, `csops`, `openat`/`fstatat`/`dup`, `getattrlist` (volumen
y ruta completa), `fsgetpath`, `mach_vm_map`/`host_info`/`host_get_clock_service`/`semaphore_create`/
`task_info`/`task_get_special_port` por `mach_msg2`, `bsdthread_register`, `getdirentries64` y
`getattrlistbulk`, además de los parámetros `ptr_munge`, `stack_guard` y `malloc_entropy` en la pila.

Errores propios que costaron encontrar, por si ayudan a otros:
- `jmp *%r14` justo después de poner `r14` a cero al saltar a `dyld`.
- No crear la región dinámica de la caché (`dyld_data    v3`, que en macOS construye `launchd`) ni aplicar
  el *slide* de los punteros de datos (aparecían como `0x400003a93d0`).
- `process_vm_readv/writev` sobre el propio proceso fallaba con `EINVAL` dentro del manejador de `SIGSYS`:
  se sustituyó por `/proc/self/mem`.
- Constantes de `getattrlist` mal copiadas (`0x08000000` es `FULLPATH`, no los flags de protección).
- `libpthread` aborta con `BUG IN LIBPTHREAD: Token from the kernel is 0` si falta `ptr_munge=`.

Programas probados con éxito (`exit=0`): `ls`, `ls -l`, `echo`, `cat`, `uname -a`, `pwd` y `bash` 3.2.57 con órdenes
internas (`echo`, aritmética `$((6*7))`, variables). También funciona el `execve` (se relanza `tahoe-run` con el programa
nuevo e interpreta `#!`) cuando `bash` lanza directamente una única orden externa.

> **Resuelto (10–11 de octubre):** `fork` funciona; la shell de macOS ejecuta tuberías y listas de órdenes. El hijo reabre
> `/proc/self/mem` y no hereda la marca de creador de la región de puertos (ver «Shell de macOS»). Lo que sigue es el registro de
> cuando estaba abierto.

**Problema abierto (4 de octubre): `fork`.** `bash -c 'a; b'` o una tubería hacen `fork` y el hijo aborta con `SIGABRT` justo al
terminar la reinicialización de `libSystem` en el hijo. Lo averiguado:
- El kernel de Linux **no hereda** `PR_SET_SYSCALL_USER_DISPATCH` en el hijo de un `fork`; sin reactivarlo con
  `prctl`, las syscalls del hijo se ejecutan como syscalls reales de Linux y devuelven `ENOSYS` en silencio (así es
  como `libmalloc` acababa escribiendo en una página de solo lectura). Esto está corregido (`reenable_dispatch`).
- Con eso el hijo ya ejecuta todo el reinicio (`task_self_trap`, `bsdthread_register`, `mprotect`, `host_self_trap`,
  `mach_port_construct`, `host_get_clock_service`), pero después aborta. No es `bsdthread_ctl` (ya aceptado: lo llama la
  propia `pthread_kill`), ni el tamaño de recepción, ni el valor de retorno del `fork`.
- El mensaje de aborto no está en la anotación de crash de `libpthread`, y la pila en ese instante no conserva
  direcciones de retorno útiles. Falta saber qué función decide abortar; la siguiente prueba razonable es trazar la
  ejecución del hijo instrucción a instrucción desde el retorno del `fork`.

Otros descubrimientos de esta etapa:
- `mach_msg2` con `MACH64_SEND_KOBJECT_CALL` no lleva vector: el mensaje completo está en `data` y la respuesta se
  escribe en el mismo búfer. En mensajes a puertos normales la cabecera viaja solo en registros.
- Los argumentos 7 y 8 de una syscall van en la pila en `[rsp+8]` y `[rsp+16]`, pero en la ruta de reinicio del hijo
  ese tamaño de recepción no es fiable; para llamadas a objetos del kernel se usa el tamaño de la respuesta.
- `process_vm_readv/writev` sobre el propio proceso falla con `EINVAL`: se usa `/proc/self/mem`.

Limitación conocida: 63 de los 347 programas de `/bin`, `/usr/bin`, `/sbin` y `/usr/sbin` (y 10.626 archivos en
total) están **vacíos** en el sistema extraído, porque `7z` no entiende la compresión transparente de HFS+
(`decmpfs`, cuyo contenido va en la *resource fork*). No es un fallo del lanzador: hay que re-extraer con una
herramienta que la soporte (`hfsfuse`, en AUR).

Siguiente: señales, hilos reales y `launchd`/Mach IPC con puertos de verdad; después, la parte gráfica
(`SkyLight`, `QuartzCore`), que es la que llevaría al recovery.

### Compilar Darling en un Codespace

`tools/build_darling_codespace.sh` compila `mldr` (el cargador Mach-O) y `darlingserver` en un
Codespace de 4 núcleos y 16 GB (Ubuntu 24.04): 170 pasos, sin errores, en pocos minutos. Con el árbol
de Darling en el commit `60ba801d`. Tres cosas que costaron tiempo y quedan anotadas en el script:
compilar en `/tmp` (disco aparte, ~118 GB; `/workspaces` solo deja ~12 GB), saltarse `git-lfs`
(`GIT_LFS_SKIP_SMUDGE=1`; el submódulo `swift` pide credenciales) y que un clon abortado deja los
submódulos vacíos aunque `git submodule status` diga que están bien.

Esto compila solo las piezas de Linux. Las bibliotecas del lado macOS (`libsystem_kernel` y demás) usan
el compilador cruzado de Darling y no se han construido todavía.

`tools/dyld_iterate.py` automatiza esta prueba: ejecuta un binario en Darling, extrae la librería que
falta de la caché y reintenta. Sirve para mapear la cadena de dependencias; se detiene en el fallo
anterior.

### `launchd` como PID 1 (4 de octubre de 2026)

`unshare --pid --fork --user --map-root-user --mount-proc ./tahoe-run <caché> <root>/usr/lib/dyld <root>/sbin/launchd`
ejecuta el `launchd` real de macOS 26 como **PID 1** de un espacio de nombres (el registro muestra `<1>`). Sin root.

Qué se vio, por orden:
1. `launchd` cierra la entrada, salida y error estándar y los reabre a `/dev/null`. Eso tapaba el registro de
   `tahoe-run` (escribía en el descriptor 2): ahora usa un descriptor privado (el 900).
2. Abre `/dev/console` (sin permiso en el espacio de nombres): se redirige a `/dev/null`.
3. `fsctl 0x40084a6a` (consulta privada de APFS del estado del dispositivo raíz, 8 bytes): se responde con ceros.
   Sin eso aborta con `failed to query root device status: 78` y llama a `reboot`.
4. Lo siguiente que pidió, **ya resuelto** (el `launchd` ya no muere):
   - `bsdthread_create`/`bsdthread_terminate`: hilos de Linux con selector de despacho propio por hilo.
   - `fsctl 0xc1044a50` y `map_with_linking_np`: respuesta "no soportado"; `launchd` y `dyld` continúan.
   - `mach_msg2` id 225 (host), `csops` 16 y la zona horaria: aceptados o con valores neutros.
   - Después hicieron falta kqueue/kevent sobre epoll, workloops de libdispatch, semáforos y puertos Mach
     compartidos entre procesos. Con eso `launchd` completa el arranque y lanza servicios (ver "Dónde retomar").

> Nota: la versión anterior de esta sección decía que el proceso moría con `SIGILL`. Era el estado de un primer intento
> y ya no es cierto: hoy `launchd` se queda en ejecución como PID 1.

## Dónde estaba el proyecto el 4 de octubre de 2026 (histórico)

> El estado actual está al principio de este documento. Esta sección se conserva como registro.

**Funciona**: `tahoe-run` ejecuta programas reales de macOS 26 (`ls`, `echo`, `cat`, `uname`, `pwd`, `bash` 3.2.57 con
órdenes internas) sobre Linux, sin XNU ni Darling. Compilar con `tahoe-run/build.sh <xnu>` y ejecutar con
`TAHOE_ROOT=<recovery extraído> ./tahoe-run <caché> <root>/usr/lib/dyld <root>/bin/ls /`.

**Pendiente, por orden de utilidad**
1. **`fork` con continuación en el hijo**: el hijo reinicializaba `libSystem` y abortaba (ver "Problema abierto"); no se ha vuelto a comprobar tras los cambios posteriores (los servicios usan `posix_spawn`, no `fork`).
   Siguiente prueba: trazar el hijo instrucción a instrucción desde el retorno del `fork`.
2. **Re-extracción con `hfsfuse`: hecha** (`tools/dmg2raw.py` convierte el UDIF a imagen cruda; se corta la partición HFS+ con `dd` y se monta con `hfsfuse --force -o ro`). Con 7z salían vacíos 10.624 archivos (9.564 con datos comprimidos y 1.005 enlaces simbólicos); ya están restaurados. `com.apple.cmio.registerassistantservice` falla con `exit(78)` porque su ejecutable no existe en la imagen (igual que en un Mac real).
3. **`launchd` como PID 1 — arranca y sus demonios corren**: el `launchd` de macOS 26 (BaseSystem) completa su arranque,
   carga ~170 LaunchDaemons y lanza servicios con `posix_spawn` → `xpcproxy` → `execve`: **unos 30 demonios reales de
   macOS 26 quedan en ejecución** como procesos Linux (`syslogd`, `securityd`, `opendirectoryd`, `bluetoothd`,
   `sandboxd`, `diskarbitrationd`, `recoveryosd`, `amfid`...). Su registro: `<datos>/private/var/log/com.apple.xpc.launchd/
   launchd.log` (y `TAHOE_FD2LOG`/`TAHOE_LOGFILE` + el socket `/var/run/syslog` falso para ver sus mensajes de error).
   Lo que lo hizo posible: IPC Mach **entre procesos** (región compartida en `/dev/shm`, nombres globales, OOL,
   RCV_LARGE, conversión de disposiciones, trailer con el pid del emisor, recepción directa en `EVFILT_MACHPORT`),
   workloops de libdispatch (petición de hilo validada por `ADDR/MASK/VALUE`, vigilantes por kqueue), `EVFILT_PROC`
   con `pidfd`, `mk_timer`, `ulock`, nombres de puerto únicos por hilo, sockets, `getattrlist`, `sysctl kern.proc`,
   vouchers, `mach_ports_lookup3`, etc. Pendiente: 6 servicios acaban con `exit(78)`, IOKit (ids MIG 2800-2899 →
   `kIOReturnNotFound`), 3 plists vacíos por la extracción con 7z (hace falta `hfsfuse`), señales reales y
   `EVFILT_SIGNAL`, y la parte gráfica (`WindowServer`, `SkyLight`, Metal).
4. Señales reales, hilos y colas de trabajo; después la parte gráfica (`SkyLight`, `QuartzCore`, Indium/`metal2vulkan`).

**Ideas sueltas ya validadas**: `PR_SET_SYSCALL_USER_DISPATCH` no se hereda en `fork` (reactivar en el hijo);
`/proc/self/mem` en lugar de `process_vm_*`; la caché necesita *slide* v2 y región dinámica; `llvm-objdump` no
desensambla bytes sueltos, usar `objdump -D -b binary -m i386:x86-64`; `ipsw dyld a2s` simboliza direcciones de
la caché x86_64.


### Estado de `launchctl` y `notifyd`
`launchctl managername` (→ `System`) y `launchctl list` funcionan, y **`notifyd` real atiende a sus clientes** (ya no hay avisos
`notify_register_check failed`). Arreglos que lo permitieron: mensajes Mach de más de 1 KB; el derecho de recepción que `launchd`
mueve a un servicio necesita un aviso de despertar propio del nuevo receptor (`port_eventfd` lo recrea y avisa si ya hay mensajes);
los avisos son un nivel (se vacían y se renuevan mientras quede cola: la cola de datagramas de Unix es corta y perdía avisos);
variables de condición por puerto (antes una global despertaba a todos); y `SIDT`: libdispatch lee de ahí el "número de CPU" (en macOS
va en el límite de la IDT; en Linux siempre vale 0xfff), así que se reserva el segmento 0xfff de su montón de 32 MiB.
`launchctl print system` también funciona: `mach_make_memory_entry` se respalda con un archivo en `/dev/shm` que sustituye el rango del creador por una proyección compartida (`mach_vm_map` con objeto la mapea en el receptor).

### IOKit
`tahoe-run/emu_iokit.inc` implementa el servidor MIG de `device.defs` (ids 2800-2890) con un registro de E/S completo: árbol de
entradas (plataforma, ACPI, PCI con la GPU real leída de `/sys`, HID, batería, SMC, medios...), planos `IOService` e `IODeviceTree`,
rutas, iteradores (recursivos, hacia los padres, entrar/salir), propiedades en binario de OSSerialize y en XML, diccionarios de
coincidencia (XML/binario/OOL: `IOProviderClass` con herencia, `IONameMatch`, `IOPropertyMatch`, `IOResourceMatch`, `IOPathMatch`),
notificaciones (mensaje id 53), conexiones con ganchos por clase (AppleSMC responde "clave no encontrada"; el resto de métodos
devuelve `kIOReturnUnsupported` hasta que el puente DRM/Metal registre ganchos). El `ioreg` real de macOS 26 lo recorre entero.

### Abortos de libdispatch (arreglados)
Los demonios que abortaban con "BUG IN LIBDISPATCH: mach_msg_receive" (≈12: powerd, remoted, timed, findmydeviced, configd...) enviaban sus
peticiones XPC con `mach_msg2` en **modo vectorial** (`MACH64_MSG_VECTOR`): `data` apunta a un `mach_msg_vector_t[mensaje, auxiliar]` y el
tamaño real va en el vector, no en el registro. El envío fallaba en silencio, libxpc destruía su puerto de respuesta y la recepción siguiente
abortaba. Ahora se soporta (más cabecera auxiliar vacía al recibir). Además: `fchflags/chflags` guardan `st_flags` como xattr (libsystem
comprueba `rootless_mkdir_protected`), un kevent de puerto sin recepción directa devuelve `fflags=0`, y varias syscalls menores
(`lchown`, `getxattr`, `gethostuuid`, `auditon`). Quedan `amfid` (SIGSEGV en el arranque de dyld) y WindowServer, que sale con código 1 al
no poder abrir el cliente de usuario de `IOHIDSystem`.

### Bases más sólidas (10 de octubre de 2026)
Estado actual: `launchd` arranca unos 40 procesos y ~20–24 servicios con nombre siguen vivos; la memoria se mantiene estable
(~0,9 GB anónimos). Cambios de fondo, buscando la semántica exacta de XNU y libdispatch:
- **kqueue**: un registro de epoll por (kqueue, descriptor) con la unión del interés de los knotes que lo comparten (READ y WRITE del
  mismo socket, varios knotes `EV_UDATA_SPECIFIC` del mismo puerto); el dato del registro es el descriptor, nunca un puntero.
  Rearmar un knote existente actualiza sus parámetros (`f_touch`); `EV_ONESHOT` con `EV_DISPATCH2` queda en borrado diferido
  (`KN_DEFERDELETE`, `EINPROGRESS`, evento falso `EV_DELETE|EV_ONESHOT`). La recepción directa rellena `ext[]` como XNU.
- **Workloops y colas de trabajo**: un vigilante fijo por workloop recoge los eventos antes de lanzar hilo (sin despertares en vacío);
  los hilos se reutilizan con `WQ_FLAG_THREAD_REUSE` y mueren por la vía de libpthread (`nkevents = -1`), sin fugas de pila y con
  tope; se vigila también el kqueue de la cola (`kevent_qos` con kq = -1). Espera síncrona de `dispatch_sync`
  (`NOTE_WL_SYNC_WAIT/WAKE/END`) con comprobación ESTALE.
- **Sincronización**: `ulock_wait` devuelve el número exacto de esperadores restantes (si no, `os_unfair_lock` pierde despertares y
  libdispatch aborta); `psynch` (mutex fairshare y condiciones); `thread_switch`.
- **Mach**: códigos de `mach_msg` corregidos (`MACH_RCV_INVALID_NAME` = 0x10004002), contexto del puerto en el trailer
  (`construct`, `get/set_context`, `guard`), `mach_port_names`, `thread_info`, envíos no encolados devuelven error.
- **Sistema**: derechos (entitlements) leídos de la firma de código real del ejecutable (`csops` 7/11/14/16, `csops_audittoken`);
  `getattrlist` con atributos de volumen y comunes extendidos; `fcntl` con equivalentes nativos; semáforos POSIX con nombre sobre los
  de Linux; información de procesos desde `/proc`; al arrancar se copia lo que falte de `System/Library/Templates/Data` (base de
  usuarios dslocal...); plano `IOPower` en IOKit.
- **Diagnóstico**: los mensajes de aborto se leen de la sección `__crash_info` de todas las imágenes y del ejecutable; trazas
  dirigidas activables con archivos en `/dev/shm` (`tahoe-mach-trace`, `tahoe-stacks`, `tahoe-waittab`, `tahoe-watch-func`).

### Más bases (10 de octubre de 2026, tarde)
Ya arrancan sin caerse `remoted`, `configd`, `powerd`, `locationd`, `kernelmanagerd`, `airportd` y `containermanagerd`; quedan vivos
~22–23 servicios con nombre. Cada arreglo sale de la semántica de XNU, no de parches:
- **Guardas de puerto**: `mach_port_guard/unguard` (trampas y MIG) guardan la guarda en el contexto del puerto, que el receptor ve en el
  trailer; libxpc guarda así sus puertos de respuesta con el objeto de la petición. `MPO_CONTEXT_AS_GUARD`/`MPO_STRICT`, `swap_guard`,
  `assert_attributes`; mover un derecho de recepción borra contexto y guarda (`ipc_port_clear_receive`).
- **Notificaciones Mach**: `MACH_NOTIFY_SEND_ONCE`, `DEAD_NAME` y `PORT_DESTROYED`, con seguimiento de derechos send-once; destrucción
  de puertos como `ipc_port_destroy`; un recolector en el proceso raíz detecta los procesos muertos y destruye sus derechos (launchd
  recupera los puertos de sus servicios con su guarda y los relanza).
- **Números MIG verificados** con el preprocesador sobre los `.defs` (los de `mach_port` estaban desplazados): `get_context`,
  `get/set_attributes` completos, `get_refs`, `semaphore_destroy`, `mach_task_is_self`. Las respuestas de error son `mig_reply_error_t`.
- **Temporizadores**: `EVFILT_TIMER` con `NOTE_ABSOLUTE`, `NOTE_MACHTIME` y reloj de pared (libdispatch arma así todos los suyos).
- **Colas de trabajo**: cada hilo arranca con la QoS de su petición (y OVERCOMMIT/COOPERATIVE), como `workq_setup_and_run`; las
  peticiones de hilo de workloop siguen activas hasta que se entregan o libdispatch las retira.
- **Número de CPU**: libdispatch y libmalloc lo leen con `sidt`; en una CPU sin UMIP no se puede atrapar, así que se sustituye en la
  copia privada de la caché.
- **Sistema**: socket de eventos del núcleo (`PF_SYSTEM`/`SYSPROTO_EVENT`), `ioctl` FIO*, más `sysctl` (`hw.target`,
  `kern.bootsessionuuid`...), `OSKernelCPUType` en la raíz de IOKit, `boot-args` como cadena.
- **Diagnóstico**: al caer el proceso indicado, cada hilo vuelca su llamada en curso y su pila; modo `all` (todas sus llamadas).

### Shell de macOS (11 de octubre de 2026)
`tools/macos-shell.sh` abre el `/bin/bash` del sistema base de macOS 26 en la terminal de Linux, con un entorno de macOS
(PATH de macOS, `HOME=/var/root`, usuario root). Funcionan, entre otras, `ls`, `cd`, `cat`, `grep`, `find`, `sed`, `awk`,
`cp`/`mv`/`rm -r`, `tar`, `ps`, `uname`, `sw_vers`, `id`, `date`, `df`, `plutil`, `csrutil`, `perl`, `vim` y `less`, con
tuberías, redirecciones y edición de línea. (Al principio no funcionaban lo que necesita servicios de launchd ni la red; hoy
`ifconfig`, `netstat` y `ping` funcionan, y `launchctl` y `defaults` con `--launchd`: ver la sección siguiente.)

```
tools/macos-shell.sh                  # sesión interactiva
tools/macos-shell.sh -c 'sw_vers'     # una orden
```
Bases que hicieron falta: `fork` reabre `/proc/self/mem` en el hijo y no hereda la marca de creador de la región de puertos;
rutas confinadas a la raíz de macOS (solo `/dev` viene de Linux, con las pts como `/dev/ttysNNN`); `exec` conserva el
directorio actual; termios de Darwin; `poll`; `sysctl KERN_PROC`/`KERN_PROCARGS2` con los procesos de macOS; atributos
extendidos; uid/gid emulados.

Lo que quedaba pendiente entonces está ahora en «Hoja de ruta», al principio.

### Shell unida a launchd y herramientas de diagnóstico (11 de octubre de 2026)
`tools/macos-shell.sh --launchd` arranca launchd en su propio espacio de PID (o se une a uno que ya corre) y mete la shell con
`nsenter`, compartiendo la región de puertos Mach: sin puerto bootstrap propio usa el que launchd registró, como un proceso de
macOS. Así funcionan `launchctl list`/`print`, `defaults read/write` (cfprefsd), `scutil`, `pmset`, `ioreg`, `nvram`, `dscl`.
Además, desde esta fecha: `sysctl` (identificadores especiales `{0,1}`, `{0,2}`, `{0,4}`), `mount`/`umount` y `df` con la raíz
y `devfs` (llamadas de montaje como XNU), y `caffeinate`: el hilo gestor de eventos de libdispatch se lanza con
`WQ_FLAG_THREAD_EVENT_MANAGER`, sin lo cual ningún temporizador de la cola principal o de las globales (`dispatch_after`) se
disparaba en ningún proceso.

Herramientas usadas para encontrar estos fallos: `tools/diag/` (ver «Probarlo»).

### Fileports, llavero de root y arranque normal estable (11 de octubre de 2026, noche)
- **Fileports** (`fileport_makeport`/`fileport_makefd`, `emu_fileport.c`): los descriptores y sockets pasan entre procesos por
  `SCM_RIGHTS`; launchd entrega a cada servicio los sockets de su plist y logd su almacén a `log`.
- **`kern.osvariant_status`** lo fija solo launchd (PID 1), una vez, y lo leen todos; **almacén de logd** (`/private/var/db/diagnostics`
  como enlace a una carpeta escribible): launchd decide `HasFullLogging = YES` y carga logd.
- **`/var/run` vacío en cada arranque**, como en un Mac (quedaban marcas como `systemkeychaincheck.done` de corridas anteriores).
- **Llavero de root**: con los fileports, recoveryosd llegaba a `SecKeychainLogin`, que fallaba y reiniciaba todo. Faltaba
  `security.mac.sandbox.sentinel` (Security nombra con él los temporales de sus escrituras atómicas y, sin él, deshacía la creación).
- **`MACH_NOTIFY_NO_SENDERS`**: los servidores (logd, launchd...) cierran la conexión de un cliente cuando termina; antes acumulaban
  un puerto por cliente hasta llenar la tabla y cualquier proceso abortaba con «Unable to allocate reply port».
- **Puertos especiales y registrados por proceso y heredados** (`task_create` de XNU): antes eran globales y el último en registrar
  cambiaba el bootstrap de todos.

### Dominios de launchd, discos y señales (11 de octubre de 2026, segunda parte)
- **Dominios de proceso de launchd** (`pid/N`): `mach_port_type` no conocía los puertos de tarea de otros procesos (objetos
  del núcleo) y devolvía `KERN_INVALID_NAME`; launchd lo tomaba por un proceso muerto y destruía cada dominio nada más crearlo
  («slaying domain»). Las búsquedas de servicios desde un proceso daban 124 («Domain is tearing down») y no se cargaban los
  servicios XPC de los frameworks (`authd`) ni los de aplicación. Ahora es un derecho de envío, o un nombre muerto si la tarea
  terminó, como en XNU.
- **`NOTE_EXEC`** en `EVFILT_PROC`: el proceso vigilado avisa por un puerto justo antes de hacer `exec`. launchd ve pasar los
  servicios de `xpcproxy` a `running`.
- **Identidad y firma de quien llama**: `proc_pidpath` (`PROC_PIDPATHINFO`), `PROC_PIDCOALITIONINFO`, `CS_OPS_CDHASH` (SHA-1 o
  SHA-256 del CodeDirectory, sin bibliotecas) y `CS_OPS_PIDOFFSET`. authd los usa para validar a sus clientes.
- **Señales de launchd**: `proc_signal_delegate` y compañía (`proc_info` 0x11-0x14), y `task_info(TASK_AUDIT_TOKEN)` de la
  tarea de otro proceso devuelve su token (antes el del que pregunta: launchd se mandaba la señal a sí mismo).
- **Puertos**: tabla de 8192 con índice por nombre (con los dominios funcionando, la de 2048 se llenaba); aviso perdido entre
  vaciar el socket de aviso de un puerto y un mensaje que llega a la vez (configd se colgaba y con él powerd y storagekitd);
  `KERN_RESOURCE_SHORTAGE` con la tabla llena en vez de éxito con nombre 0.
- **Discos**: jerarquía IOKit de un Mac con APFS (`IOBlockStorageDevice` → disk0 → disk0s1 → disk1 → disk1s1 con el UUID de
  arranque); nodos `/dev/disk*` y `/dev/rdisk*`. Para que diskarbitrationd anunciara los discos hizo falta: leer bien el tipo
  de las notificaciones IOKit (el lector de cadenas devolvía siempre el mismo búfer y la notificación de terminación retiraba
  todos los discos nada más crearlos) y que arranque `fskitd`, que busca el servicio `com_apple_filesystems_lifs` (LiveFS).
  Con la jerarquía, las fases `mount -P` del arranque se ejecutan de verdad.


### macOS 26 completo (11 de octubre de 2026, tarde)
Además de la Recovery, `tahoe-run` arranca ya el **macOS 26.7.1 completo** (25G241, del `InstallAssistant.pkg` oficial).
Instalación sin Mac, con herramientas de este repositorio:

```sh
tools/xar.py saca InstallAssistant.pkg SharedSupport.dmg SharedSupport.dmg   # el paquete del instalador (xar)
# de SharedSupport.dmg (HFS+) se saca el zip de actualización (com_apple_MobileAsset_MacSoftwareUpdate/*.zip)
tools/yaa.py <actualización.zip> <raíz>       # payloads del sistema (pbzx + Apple Archive) y enlaces duros de links.txt
# el cryptex del sistema (caché de dyld x86_64h) va en <raíz>/System/Volumes/Preboot/Cryptexes/OS
tools/datos_macos.py <raíz>                   # volumen de datos desde la plantilla y firmlinks, como el instalador
TAHOE_ROOT=<raíz> tools/macos-shell.sh --launchd
```

Con un disco virtual ext4 con *casefold* (insensible a mayúsculas, como APFS) de 60 GB, el sistema extraído ocupa 30 GB.
En el primer arranque (sistema recién instalado, sin `/var/db/.AppleSetupDone`) launchd carga **~400 servicios**, corren
**~90 procesos** y se lanzan `WindowServer` y `loginwindow`, que es quien abre el Asistente de configuración. Lo que hizo falta:

- **Caché de dyld del cryptex** (`dyld_shared_cache_x86_64h`, la variante Haswell, 7 archivos): llena casi toda la región
  compartida, así que dyld se carga en 0x7ff000000000; la región dinámica anota la raíz del cryptex (`_osCryptexPathOffset`).
- **EBADARCH**: un Mach-O sin parte x86_64 (p. ej. `seputil`, la tarea de arranque `data-protection`, que en un Mac Intel no
  existe) falla en `exec`/`posix_spawn` sin crear el proceso, como en XNU; launchd lo salta.
- **Volúmenes Data y Preboot**: en un Mac son volúmenes APFS aparte. Aquí son carpetas montadas sobre sí mismas (bind) en el
  espacio de montajes del arranque; `stat` distingue el volumen por el id de montaje (`statx`) y `mount`/`df` muestran
  `/dev/disk1s2 on /System/Volumes/Data` y `/dev/disk1s3 on /System/Volumes/Preboot`. CarbonCore abortaba al ver el
  Preboot (donde está la caché) con el dispositivo de la raíz.
- **Trampas y llamadas nuevas**: `macx_triggers`, `setpriority`/`getpriority`, `mach_vm_remap` (trampolines de bloques de
  libobjc), `identitysvc` (opendirectoryd giraba al 100% de CPU reintentando registrarse como resolutor de identidades).
- **IOKit**: servicio `IOWatchdog` (watchdogd abortaba sin él).
- **Número de CPU**: esta caché lee la CPU con `sidt [rbp-0x10]` además de `sidt [rsp-0x10]`; sin sustituirla, una veintena
  de servicios caían en libdispatch.
- **Programas protegidos** («Apple Protect»: loginwindow, Finder, Dock...): su `__TEXT` está cifrado (Blowfish, página a
  página) y en un Mac lo descifra el núcleo con la clave OSK del SMC. `tahoe-run` lo descifra al cargar el ejecutable
  (`emu_dsmos.c`). **La clave no está en el repositorio**: se lee de `~/.config/tahoe-run/osk` (64 bytes) o de
  `TAHOE_OSK_FILE`; es la misma que dan VirtualSMC o FakeSMC a un Hackintosh.

Siguiente: que `loginwindow` abra el Asistente de configuración y, para verlo, una pantalla: un `IOFramebuffer` emulado
(como un Mac o una máquina virtual sin aceleración, que macOS 26 dibuja por software) mostrado en una ventana de Linux, y
después la aceleración real (Metal sobre Vulkan).

### macOS 26 completo: hacia el Asistente de configuración (12 y 13 de octubre de 2026)

Cada arreglo deja llegar un poco más lejos y destapa el siguiente bloqueo, que ya estaba ahí detrás del anterior:

- **WindowServer se caía por SIGILL**: libpthread aborta si el núcleo no implementa `psynch_rw_rdlock`/`unlock`. Ahora
  `nucleo/src/rwlock.rs` traduce `kern_synch.c` (colas de lectores y escritores, preposts, solapamiento), con pruebas.
- **WindowServer crecía 6,5 MB/s**: unos 3000 mensajes por segundo con powerd, porque tahoe-run disparaba en cada mensaje
  las peticiones `MACH_NOTIFY_SEND_POSSIBLE` sin armar. Como en XNU, solo avisan las armadas (síncronas con la cola llena,
  o armadas por un envío con `MACH_SEND_NOTIFY`). De paso, `diskutil list` de la Recovery ya cabe en 40 s.
- **Caídas en `_dispatch_source_invoke`** (WindowServer, mediaremoted, AirPlayXPCHelper, PerfPowerServices): faltaba
  `kqueue_workloop_ctl`; sin él, libdispatch usaba un camino de reserva.
- **loginwindow esperando para siempre a opendirectoryd**: los workloops no se liberaban nunca; opendirectoryd llenaba la
  tabla, `kevent_id` daba ENOMEM y se quedaba con ~500 hilos atascados. Ahora se liberan cuando quedan sin uso.
- **Language Chooser** (lo lanza loginwindow; con `tools/diag/dsmos.py` se vio que espera `lcaCheckComplete`) no podía
  abrir su nib: faltaban `fsgetpath` para identificadores que vienen de otro proceso (tabla común del arranque) y
  atributos comunes en `getattrlist` de volumen (942 ENOTSUP por arranque). Ahora a veces carga y a veces no: sigue
  pendiente.
- **Despertares perdidos y cuelgues sueltos**: el candado global de los puertos era un mutex robusto de glibc, que
  identifica al dueño por un TID que nuestro `fork` directo no actualiza en el hijo. Ahora es `nucleo/src/candado.rs`:
  TID real, futex compartidos, esperas con plazo y recuperación de dueños muertos (también zombis).

Pendiente: la fuga de memoria de 1-3 GB en algún proceso; las órdenes que fallan al hablar con launchd y servicios del
sistema en el macOS completo (`diskutil`, `defaults`, `launchctl print`, `log show`); que Language Chooser y el Asistente se
dibujen; y después, teclado y ratón.
