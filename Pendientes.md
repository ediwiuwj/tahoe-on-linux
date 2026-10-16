# Pendientes

Lista de trabajo del macOS 26 completo sobre tahoe-run, en orden de prioridad. Se actualiza al cerrar o abrir tareas.

**En pausa desde el 14 de octubre de 2026.** Cómo retomarlo: README, «Cómo retomarlo».

## Por dónde seguir
1. **Pantalla (punto 4 de abajo)**: WindowServer rellena la pantalla de negro y dibuja el cursor, pero la ventana de
   inicio de sesión no aparece. loginwindow pasa los primeros ~7 min esperando a lsd, que en cada arranque vuelve a
   registrar todos los programas (en el disco mecánico tarda; su base de datos sí se guarda). A los 11 min loginwindow
   sigue en `_LSCopyServerStore` esperando a lsd (pide la base de datos y lsd no responde). Siguiente: qué hace lsd a
   los 10–11 min (`ESPERA=60 STACKS=lsd tools/diag/hang.sh true lsd 600`, con `/dev/shm/tahoe-msg-trace` = «lsd
   loginwindow») y por qué rehace la base de datos en cada arranque.
2. **`diskutil list` en la Recovery** se cuelga a veces (también con el binario anterior): mirar con `tools/diag/discos.sh`.
3. Lo demás, en la lista de abajo.


## Hecho recientemente
- loginwindow ya no se cae al terminar de arrancar: un puerto podía estar en 8 conjuntos como mucho y loginwindow (un modo
  de runloop por cada cosa que espera) necesitaba más; abortaba con «Unable to insert wake up port into port set» y
  WindowServer se cerraba con él, en bucle. Ahora 32. Con eso WindowServer y loginwindow aguantan 15 minutos seguidos.
  Nueva herramienta: `tools/diag/pantalla.sh` (arranca, guarda la pantalla cada N s y dice qué programas gráficos viven).
- /dev/shm ya no se llena: las entradas de memoria (`mach_make_memory_entry`) se borran cuando nadie tiene ya derechos
  de envío, como en XNU. lsd crea una de 1,5 MB por cada cliente que pide su base de datos y en ~2 min llenaba /dev/shm
  (2,3 GB): launchd moría por SIGBUS y con él todo el macOS (también el «WindowServer no está vivo» de algunas pruebas).
  Ahora /dev/shm se queda en ~360 MB. Con ello coreaudiod arranca y `say` funciona: `say -v ?` da 68 voces, `say -o`
  crea el AIFF y `say hola` termina (sin sonido: falta la tarjeta de sonido, ver «Teclado, ratón y sonido»).
- Llavero del sistema: «System unlock is working». Faltaban los firmlinks (`/usr/share/firmlinks`): en un Mac
  `/Library` del sistema y `/System/Volumes/Data/Library` son la misma carpeta, no un enlace; securityd veía un enlace
  y convertía el llavero en una copia `-db` que SystemKey no abría. Con ello el Kerberos local (LKDC) se configura y
  digest-service sale bien en vez de relanzarse.
- `diskutil apfs list` da las capacidades (tamaño, usado, libre y lo que consume cada volumen): cliente de
  AppleAPFSContainer (selector 8: diccionario por nombre BSD, el contenedor con size/used/reserve y cada volumen con
  fs_used/fs_reserve/fs_quota; selector 9: rol de cada volumen).
- `system_profiler` termina: un puerto Mach puede estar en varios conjuntos a la vez (CFRunLoop lo necesita; sin ello
  NSTask no se enteraba de que csrutil había terminado).
- Memoria: el TLS estático de tahoe-run (~830 KB que glibc pone a cero en cada hilo, unos 2 GB en el macOS completo) baja
  a 40 KB, y `madvise(MADV_FREE*)` devuelve de verdad la memoria. A mitad del arranque: 2,9 GB usados en vez de 4,6 GB.
  `diskutil list` responde: la primera vez tras arrancar espera ~1 min a que lsd limpie su base de datos (registra de
  nuevo los paquetes «requeridos», como en un Mac; aquí va lento porque el disco del PC es mecánico), luego 0,2 s.
- Listado de carpetas con `NSFileManager`/`CFURLEnumerator` (todo macOS): `getattrlist` marcaba siempre `ATTR_CMN_ERROR`
  en los atributos devueltos, y CoreServices lo toma por «esta entrada falló» → `ENOTDIR`; además `getattrlistbulk` no
  admitía los comunes extendidos (`REALDEVID`, `EXT_FLAGS`). Con ello `init_featureflags` ve las 500 listas de
  `/System/Library/FeatureFlags/Domain`: 3328 funciones activas (antes 124) y locationd ya no se cae («key cannot be nil»:
  con `CoreLocation/cl_pula` apagada, el userName de cada cliente salía vacío). Efecto secundario: órdenes que antes
  terminaban sin hacer nada ahora hacen el trabajo real y se pasan del plazo del inventario (`say -v ?` encuentra las
  voces y espera a coreaudiod, que se queda cargando sus complementos de audio): el inventario corto baja de ~61 a ~58.
- `log show`: la segunda vez tarda ~5 s (antes 20–27 s); `kern.bootsessionuuid` distinto en cada arranque y
  `proc_info` extendido con los argumentos en su sitio. `lwp-request` no se cuelga (25 s en frío, sale 255 como en un Mac).
- `thread_policy_get`, grupo de procesos en `posix_spawn` (`POSIX_SPAWN_SETPGROUP`), `fcntl F_GETSIGSINFO`, buzón de
  EndpointSecurity válido.
- `bless --info` funciona (encuentra el Preboot del contenedor y lee sus datos de bless: sin carpeta bendecida) y
  `bless --info --getBoot` da `/dev/disk1s1`.
- Llaveros: `security create-keychain`, `add-generic-password`, `list-keychains`... funcionan (securityd daba EINVAL
  al preguntar por la sesión de auditoría del cliente, que no estaba registrada) y apsd ya guarda su llavero.
  `csops` da el estado de firma real (CS_VALID, binario de plataforma de Apple...): securityd valida a sus clientes y
  `shortcuts` ya no se cae con «Assertion failed: (platformBinary)».
- EndpointSecurity mínimo: el servicio EndpointSecurityDriver y su cliente (sin eventos); xprotectd ya no se relanza en
  bucle.
- Arranque: `IODeviceTree:/efi/platform` (libimage4 abortaba sin él: kmutil, una tarea temprana, y cryptexd se caían con
  «failed to get platform node»); ahora salen ~90 programas en los 10 primeros segundos (antes 1–3). La frecuencia del
  TSC se mide una vez (antes 30 ms de espera activa en cada proceso).
- Discos del macOS completo: `diskutil list` muestra Macintosh HD, Macintosh HD - Data y Preboot (antes «macOS Base
  System»), con sus roles; `diskutil info /System/Volumes/Data` y `.../Preboot` encuentran su volumen (getattrlist
  describe el volumen de cada ruta y reconoce sus puntos de montaje).
- `setitimer`/`getitimer`: `alarm()` no avisaba nunca (perl -e 'alarm N', plazos de muchos programas).
- Caída «BUG IN LIBDISPATCH: Invalid wlh state» (opendirectoryd, softwareupdated, PerfPowerServices, notifyd...): un hilo
  de workloop llegaba con una petición que libdispatch ya había retirado. Ahora se comprueba en el punto de no retorno
  (3 de 5 arranques con caída antes; 1 de 15 después). Con ello `id eduardo` funciona (uid 501, staff, admin).
- Preconfiguración como en las empresas (`tools/preconfigurar.sh`): usuario administrador, idioma y región, sin Asistente
  de configuración; reversible con `deshacer`.
- Identidad del equipo con `macserial` (`tools/smbios.sh`): modelo, placa, número de serie, MLB, UUID y ROM en IOKit,
  `hw.model`/`hw.target` y NVRAM. Los datos viven fuera del repositorio (`~/.config/tahoe-run/smbios.json`).
- Shell interactiva: ya no se congela (SIGTTOU/SIGTTIN/SIGTSTP ignoradas también en Linux).
- Bootstrap: la shell ya no hereda un puerto nulo si se une antes de que launchd lo registre (era la causa de que
  `launchctl`, `defaults`, `diskutil`... fallaran en la mitad de los arranques).
- Estadísticas del núcleo: `vm_stat`, `memory_pressure`, `iostat`, `uptime` y `top` funcionan.
- `sigreturn`, `getrusage`, `pselect`, `faccessat`.
- Tablas compartidas: fuga de puertos (PORT_DESTROYED sin destino), fileports de procesos muertos, NOTE_EXEC a 1024,
  recolector sin tope, puertos de hilo que soltaba `top`.

## Pendiente

1. **Velocidad y esperas** (`diskutil` como primer caso):
   - `diskutil` en sí va bien: con el sistema ya asentado, `diskutil list` tarda 3,4 s la primera vez y ~1 s después.
     Era lento porque corría en pleno arranque, que tarda 2–3 minutos en calmarse. Lo que alarga el arranque, medido:
     - 0–5 s: launchd se prepara (3,5 s en silencio, probablemente leyendo los plists de servicios).
     - 5–16 s: tareas tempranas en serie (mount, apfs_boot_util, mount_apfs, cc_fips_test...); cada programa cuesta
       0,3–0,5 s en arrancar en tahoe-run.
     - Después, xpcproxy pregunta a **opendirectoryd** por el usuario de cada servicio antes de lanzarlo, y
       opendirectoryd contesta tarde. Su caída («Invalid wlh state») ya casi no ocurre; queda una ventana menor
       (la retirada llega justo después del punto de no retorno): 1 caída en 15 arranques.
     - El registro inicial de cada proceso en logd (`firehose_send_register`) se queda segundos esperando.
   - Descartado (medido): los servicios que se relanzan en bucle gastan poca CPU (~4 s en 2 min) y el límite de 48
     hilos de cola por proceso no se llena.
   - Servicios en bucle: locationd ya no se cae (su «key cannot be nil» venía de la función `CoreLocation/cl_pula`
     apagada, porque init_featureflags no podía listar sus carpetas). apfs.util (lo lanza diskarbitrationd) falla al sondear disk1s2/disk1s3 (no hay datos detrás).
     KernelEventAgent y xprotectd arreglados. watchdogd no es un bucle: con el vigilante inactivo su diseño es «dormir
     2 s y salir», y solo vuelve cuando otro servicio le escribe. apfs.util/fsck_apfs tampoco: es el sondeo único de cada
     volumen al arrancar. digest-service ya no: el Kerberos local se configura desde que se desbloquea el llavero del sistema. Otros que se relanzan: locationd, cryptexd, diskmanagementstartup, apsd, cloudd.
   - `diskutil` con el sistema ya asentado: `list` 0,5–3 s, `info /` 2–6 s, siempre con salida.
   - Esperas parecidas: `log show` tarda ~55 s la primera vez de cada arranque y ~9 s después (con `log erase` el
     registro bajó de 574 MB a 31 MB; antes 90–300 s: el disco del PC es mecánico). `system_profiler` ya
     termina (la primera vez tras arrancar espera a lsd). `say -v ?` tarda ~135 s la primera vez tras
     arrancar (espera a que lsd rehaga su base de datos); después `say` tarda 4–5 s.
   - General: apagar los registros de diagnóstico fuera de las pruebas; índices en las tablas en vez de recorrerlas;
     mensajes Mach en Rust; arranque de procesos (servicios bajo demanda) más ligero.
2. **Caídas**:
   - `shortcuts`: ya no se cae; ahora dice «Couldn’t communicate with a helper application».
   - `screen` (sin terminal debería salir con «Must be connected to a terminal»).
   - `login` (debería pedir el nombre o salir con error, no caerse).
   - El resto de los SIGILL: `kextutil`, `vpnd`, `snmptrapd`, `kgetcred`, `json_xs5.34`, `kill.d`,
     `net-snmp-create-v3-user`, `package-stash-conflicts`.
3. **Repetir el inventario completo** (`tools/inventario_total.sh`, 1167 programas) con todo lo arreglado
   (antes: 615 responden, 190 fallos del emulador).
4. **Pantalla**: que WindowServer dibuje y se vea el escritorio (o el Asistente, si se deshace la preconfiguración);
   fallo intermitente del nib de Language Chooser.
5. **Metal 2 sobre Vulkan (Rust)**: driver Metal propio publicado en IOKit (IOAccelerator, `MetalPluginName`), órdenes de
   Metal a Vulkan con `ash`, shaders AIR→SPIR-V (`metal2vulkan`); primero la composición de WindowServer y CoreAnimation
   (Liquid Glass), después Metal 2 completo y lo que pida macOS de Metal 3 (ampliando `hasvk` de Mesa si hace falta).
6. **Sesión gráfica**: con ella, `screencapture`, `osascript` y `pbcopy` (hoy se caen por no haber sesión).
7. **Servicios que fallan al arrancar**: mobileassetd, softwareupdate; apsd se cae en bucle («ProductType» nil);
   relanzados en bucle: cryptexd, watchdogd, diskmanagementstartup, triald_system, cupsd. Muchas preferencias de
   `/Library/Preferences` y de root se guardan vacías (0 bytes): loginwindow, dock, CoreGraphics... (¿cfprefsd?
   `fcntl F_PREALLOCATE` no está implementado).
8. **Teclado, ratón y sonido** dentro de macOS (después de ver el escritorio). Sonido: coreaudiod ya arranca y `say`
   termina, pero no hay tarjeta de sonido: hace falta un dispositivo de audio que macOS vea como propio y que mande el
   sonido a PipeWire/PulseAudio del PC (para oír un vídeo, `say`...).
9. **Detalles**:
    - `top`: columnas de memoria y puertos por proceso (`mach_vm_region` 4816, `mach_port_space_basic_info` 3235).
    - `iostat`: la segunda muestra de CPU sale a 0.
    - `necp_open` (red).
    - `zprint`: pide las zonas del núcleo de XNU, que no existen en Linux (probablemente se queda así).
10. **Baja prioridad**: Java (`java`, `javac`... se caen en vez de decir que no hay Java instalado), `kmutil` y lo
    relacionado con kexts.
11. **PR #9** (orden de la pausa y lo que venga después): fusionar en `main` cuando se quiera.
12. **Para lo último** (funciona, son detalles):
    - `diskutil apfs list`: los volúmenes Data y Preboot dicen 0 B consumidos (comparten el disco de Linux).
    - `diskutil list`: cada volumen sale con el tamaño del contenedor (63,1 GB).
    - `diskutil list` y `system_profiler` tardan la primera vez tras arrancar (~1 min) mientras lsd limpia su base de datos.
